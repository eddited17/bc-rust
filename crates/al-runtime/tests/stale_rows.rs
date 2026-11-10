//! BC's optimistic concurrency (recops::rec_stored_for_write): a record read by one session and
//! changed by another before the first modifies it. Without the check the second session's change is
//! silently lost; with it, Modify fails "Another user has modified the record ..." (DB:RecordChanged).
//! Runs on the embedded engine always and on PostgreSQL with BC_PG_TEST (a scratch database).

use al_model::Catalog;
use al_runtime::{record::to_store, Runtime, Session, Val};
use bc_store::{Backend, MemBackend, PgBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 50850 "OC Item"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Amount; Integer) { }
        field(3; Note; Text[30]) { }
    }
    keys { key(PK; "No.") { } }
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("jq13-oc-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    Box::leak(Box::new(cat))
}

/// The result of a record operation that must succeed.
fn ok<T>(r: Result<Result<T, String>, al_runtime::Flow>) -> T {
    match r {
        Ok(Ok(v)) => v,
        Ok(Err(m)) => panic!("{m}"),
        Err(al_runtime::Flow::Err(e)) => panic!("{}", e.msg),
        Err(_) => panic!("unexpected flow"),
    }
}

fn set(h: &al_runtime::RecHandle, name: &str, v: Val) {
    let mut b = h.borrow_mut();
    let fi = b.tdef.field(name).unwrap();
    let sv = to_store(b.field(fi), &v).unwrap();
    b.row[fi] = sv;
}

/// Get('K') as AL does it: the row and the version it was read with.
fn get(s: &mut Session, key: &str) -> al_runtime::RecHandle {
    let t = s.cat.table_by_name("OC Item").unwrap().id;
    let h = s.new_record(t, false);
    set(&h, "No.", Val::text(key));
    let row = s.rec_get_pk(&h).expect("row");
    let mut b = h.borrow_mut();
    b.row = row.clone().into();
    b.xrow = Some(row.into());
    drop(b);
    h
}

fn amount(s: &mut Session, key: &str) -> i64 {
    let h = get(s, key);
    let b = h.borrow();
    let fi = b.tdef.field("Amount").unwrap();
    b.get_val(fi).as_i64()
}

/// A reads K, B changes and commits K, A modifies K. Returns A's error (None: A's Modify went through).
fn race(rt: &'static Runtime, key: &str) -> Option<String> {
    let mut s0 = Session::new(rt, "OC");
    let t = s0.cat.table_by_name("OC Item").unwrap().id;
    let h = s0.new_record(t, false);
    set(&h, "No.", Val::text(key));
    set(&h, "Amount", Val::Int(1));
    ok(s0.rec_insert(&h, false));
    s0.commit().unwrap();
    let mut a = Session::new(rt, "OC");
    let mut b = Session::new(rt, "OC");
    let ha = get(&mut a, key);
    let hb = get(&mut b, key);
    set(&hb, "Amount", Val::Int(2));
    ok(b.rec_modify(&hb, false));
    b.commit().unwrap();
    set(&ha, "Note", Val::text("A was here"));
    match a.rec_modify(&ha, false) {
        Ok(Ok(())) => {
            a.commit().unwrap();
            None
        }
        Ok(Err(m)) => Some(m),
        Err(al_runtime::Flow::Err(e)) => {
            a.rollback();
            Some(format!("{} [{}]", e.msg, e.code))
        }
        Err(_) => Some("unexpected flow".into()),
    }
}

fn check(rt: &'static Runtime, tag: &str) {
    // without the check: B's Amount 2 is overwritten with A's stale 1 (the lost update)
    al_runtime::recops::set_stale_check(false);
    assert_eq!(race(rt, &format!("{tag}-OFF")), None);
    let mut s = Session::new(rt, "OC");
    assert_eq!(amount(&mut s, &format!("{tag}-OFF")), 1, "lost update without the check");
    // with the check: A's Modify fails with BC's error and B's change stays
    al_runtime::recops::set_stale_check(true);
    let e = race(rt, &format!("{tag}-ON")).expect("A's stale Modify must fail");
    assert!(e.starts_with("Another user has modified the record for this OC Item after you retrieved it from the database."), "{e}");
    assert!(e.ends_with("[DB:RecordChanged]"), "{e}");
    assert_eq!(amount(&mut s, &format!("{tag}-ON")), 2);
    // a record modifies twice in a row (its version follows its own writes), and a fresh Get is current
    let mut c = Session::new(rt, "OC");
    let h = get(&mut c, &format!("{tag}-ON"));
    set(&h, "Amount", Val::Int(5));
    ok(c.rec_modify(&h, false));
    set(&h, "Amount", Val::Int(6));
    ok(c.rec_modify(&h, false));
    c.commit().unwrap();
    eprintln!("{tag}: stale Modify -> {e}");
}

#[test]
fn stale_modify_fails_like_bc() {
    let cat = catalog();
    let mem: &'static Runtime = Box::leak(Box::new(Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())))));
    check(mem, "MEM");
    if let Ok(url) = std::env::var("BC_PG_TEST") {
        let pg: &'static Runtime = Box::leak(Box::new(Runtime::new(cat, Arc::new(PgBackend::connect_url(&url).expect("connect")))));
        let mut s = Session::new(pg, "OC");
        let t = s.cat.table_by_name("OC Item").unwrap().id;
        let _ = pg.db.sync_schema("OC", &[pg.schemas[&t].clone()]);
        // a fresh run of the scratch table: other keys per run
        let tag = format!("PG{}", std::process::id() % 100_000);
        drop(s);
        check(pg, &tag);
    } else {
        eprintln!("PostgreSQL part skipped: set BC_PG_TEST to a scratch database connection string");
    }
}
