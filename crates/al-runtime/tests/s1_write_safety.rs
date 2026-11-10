//! I14 S1: no session silently overwrites another session's committed change.
//! - The stale-record check (BC's optimistic concurrency, recops::rec_stored_for_write) is on by
//!   default: A reads, B modifies and commits, A's Modify or Delete fails with BC's error
//!   (DB:RecordChanged) and B's change stays.
//! - On PostgreSQL, A's unlocked Get in a new transaction sees B's committed row (the session Get
//!   cache ends with the transaction).
//! Runs on the embedded engine always and on PostgreSQL with BC_PG_TEST (a scratch database).

use al_model::Catalog;
use al_runtime::{record::to_store, Runtime, Session, Val};
use bc_store::{MemBackend, PgBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 50852 "S1 Item"
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
    let dir = std::env::temp_dir().join(format!("i14-s1-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    Box::leak(Box::new(cat))
}

fn ok<T>(r: Result<Result<T, String>, al_runtime::Flow>) -> T {
    match r {
        Ok(Ok(v)) => v,
        Ok(Err(m)) => panic!("{m}"),
        Err(al_runtime::Flow::Err(e)) => panic!("{}", e.msg),
        Err(_) => panic!("unexpected flow"),
    }
}

/// The error of a record operation that must fail, with its code.
fn fails(s: &mut Session, r: Result<Result<(), String>, al_runtime::Flow>) -> String {
    match r {
        Ok(Ok(())) => panic!("the stale write went through"),
        Ok(Err(m)) => m,
        Err(al_runtime::Flow::Err(e)) => {
            s.rollback();
            format!("{} [{}]", e.msg, e.code)
        }
        Err(_) => panic!("unexpected flow"),
    }
}

fn set(h: &al_runtime::RecHandle, name: &str, v: Val) {
    let mut b = h.borrow_mut();
    let fi = b.tdef.field(name).unwrap();
    let sv = to_store(b.field(fi), &v).unwrap();
    b.row[fi] = sv;
}

/// Get('K') without a lock, as AL does it: the row and the version it was read with.
fn get(s: &mut Session, key: &str) -> Option<al_runtime::RecHandle> {
    let t = s.cat.table_by_name("S1 Item").unwrap().id;
    let h = s.new_record(t, false);
    set(&h, "No.", Val::text(key));
    let row = s.rec_get_pk(&h)?;
    let mut b = h.borrow_mut();
    b.row = row.clone().into();
    b.xrow = Some(row.into());
    drop(b);
    Some(h)
}

fn amount(s: &mut Session, key: &str) -> i64 {
    let h = get(s, key).expect("row");
    let b = h.borrow();
    let fi = b.tdef.field("Amount").unwrap();
    b.get_val(fi).as_i64()
}

fn insert(rt: &'static Runtime, key: &str, amount: i32) {
    let mut s = Session::new(rt, "S1");
    let t = s.cat.table_by_name("S1 Item").unwrap().id;
    let h = s.new_record(t, false);
    set(&h, "No.", Val::text(key));
    set(&h, "Amount", Val::Int(amount));
    ok(s.rec_insert(&h, false));
    s.commit().unwrap();
}

const STALE: &str = "Another user has modified the record for this S1 Item after you retrieved it from the database. Enter your changes again in the updated window, or start the interrupted activity again. [DB:RecordChanged]";

fn check(rt: &'static Runtime, tag: &str) {
    // (b) lost update on Modify: A reads, B modifies and commits, A's Modify fails; B's change stays
    let k = format!("{tag}-M");
    insert(rt, &k, 1);
    let mut a = Session::new(rt, "S1");
    let mut b = Session::new(rt, "S1");
    let ha = get(&mut a, &k).unwrap();
    let hb = get(&mut b, &k).unwrap();
    set(&hb, "Amount", Val::Int(2));
    ok(b.rec_modify(&hb, false));
    b.commit().unwrap();
    set(&ha, "Note", Val::text("A was here"));
    let r = a.rec_modify(&ha, false);
    let e = fails(&mut a, r);
    assert_eq!(e, STALE);
    a.rollback();
    assert_eq!(amount(&mut b, &k), 2, "B's change stays");

    // Delete of a stale record fails the same way, and the row stays
    let k = format!("{tag}-D");
    insert(rt, &k, 1);
    let ha = get(&mut a, &k).unwrap();
    let hb = get(&mut b, &k).unwrap();
    set(&hb, "Amount", Val::Int(3));
    ok(b.rec_modify(&hb, false));
    b.commit().unwrap();
    let r = a.rec_delete(&ha, false);
    let e = fails(&mut a, r);
    assert_eq!(e, STALE);
    a.rollback();
    assert_eq!(amount(&mut b, &k), 3);

    // (a) A's unlocked Get in a new transaction sees B's committed row, and A's Modify then works
    let k = format!("{tag}-G");
    insert(rt, &k, 1);
    assert_eq!(amount(&mut a, &k), 1);
    let hb = get(&mut b, &k).unwrap();
    set(&hb, "Amount", Val::Int(4));
    ok(b.rec_modify(&hb, false));
    b.commit().unwrap();
    a.commit().unwrap();
    assert_eq!(amount(&mut a, &k), 4, "unlocked Get in A's next transaction");
    let ha = get(&mut a, &k).unwrap();
    set(&ha, "Note", Val::text("current"));
    ok(a.rec_modify(&ha, false));
    a.commit().unwrap();
    assert_eq!(amount(&mut b, &k), 4);

    // the session's own change through another variable is no conflict (BC, read without a lock)
    let k = format!("{tag}-O");
    insert(rt, &k, 1);
    let h1 = get(&mut a, &k).unwrap();
    let h2 = get(&mut a, &k).unwrap();
    set(&h2, "Amount", Val::Int(5));
    ok(a.rec_modify(&h2, false));
    set(&h1, "Note", Val::text("own"));
    ok(a.rec_modify(&h1, false));
    a.commit().unwrap();

    // BC raises the error only for a NEWER stored version: the session's own write rolled back
    // (asserterror, an error) leaves an older stored row, and Modify goes through
    // ("Get; Modify; asserterror ...; Modify" in Microsoft's tests)
    let k = format!("{tag}-R");
    insert(rt, &k, 1);
    let h = get(&mut a, &k).unwrap();
    set(&h, "Amount", Val::Int(7));
    ok(a.rec_modify(&h, false));
    a.rollback();
    set(&h, "Note", Val::text("after rollback"));
    ok(a.rec_modify(&h, false));
    a.commit().unwrap();
    assert_eq!(amount(&mut b, &k), 7);
    // ... but another session's change after that rollback is newer: the error stays
    let k = format!("{tag}-RB");
    insert(rt, &k, 1);
    let h = get(&mut a, &k).unwrap();
    set(&h, "Amount", Val::Int(7));
    ok(a.rec_modify(&h, false));
    a.rollback();
    let hb = get(&mut b, &k).unwrap();
    set(&hb, "Amount", Val::Int(8));
    ok(b.rec_modify(&hb, false));
    b.commit().unwrap();
    set(&h, "Note", Val::text("stale"));
    let r = a.rec_modify(&h, false);
    let e = fails(&mut a, r);
    assert_eq!(e, STALE);
    a.rollback();
    assert_eq!(amount(&mut b, &k), 8);

    // ... also after many more writes of the session: the own-writes log forgets its oldest entries
    // one by one (it used to be emptied at 4,096 entries, and then the session's own earlier write
    // looked like another user's change)
    let k = format!("{tag}-L");
    insert(rt, &k, 1);
    let h1 = get(&mut a, &k).unwrap();
    let h2 = get(&mut a, &k).unwrap();
    set(&h2, "Amount", Val::Int(6));
    ok(a.rec_modify(&h2, false));
    let t = a.cat.table_by_name("S1 Item").unwrap().id;
    for i in 0..4_100 {
        let h = a.new_record(t, false);
        set(&h, "No.", Val::text(&format!("{tag}-L{i}")));
        ok(a.rec_insert(&h, false));
    }
    set(&h1, "Note", Val::text("own, later"));
    ok(a.rec_modify(&h1, false));
    a.commit().unwrap();
}

#[test]
fn no_silent_overwrite() {
    // on by default; BC_STALE_CHECK=0 is the off switch
    if std::env::var("BC_STALE_CHECK").map_or(true, |v| v.trim() != "0") {
        assert!(al_runtime::recops::stale_check(), "the stale-record check is on by default");
    }
    al_runtime::recops::set_stale_check(true);
    let cat = catalog();
    let mem: &'static Runtime = Box::leak(Box::new(Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())))));
    check(mem, "MEM");
    if let Ok(url) = std::env::var("BC_PG_TEST") {
        let pg: &'static Runtime = Box::leak(Box::new(Runtime::new(cat, Arc::new(PgBackend::connect_url(&url).expect("connect")))));
        let t = cat.table_by_name("S1 Item").unwrap().id;
        let _ = pg.db.sync_schema("S1", &[pg.schemas[&t].clone()]);
        let tag = format!("PG{}", std::process::id() % 100_000);
        check(pg, &tag);
    } else {
        eprintln!("PostgreSQL part skipped: set BC_PG_TEST to a scratch database connection string");
    }
}
