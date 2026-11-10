//! Backend differences found by running Microsoft's tests on PostgreSQL (`al-test --pg`,
//! docs/pg-vs-memory.md), checked on the in-memory store and, with `BC_PG_TEST=<scratch
//! connection>`, on PostgreSQL too (a company schema `c_s3atest<pid>` with an empty table stays in
//! that scratch database).

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session, TestCtx};
use bc_store::{Backend, MemBackend, PgBackend, ScanReq, Store};
use std::sync::Arc;

const APP: &str = r#"
table 50960 "S3a Row"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}

codeunit 50961 "S3a Inner"
{
    trigger OnRun()
    var
        R: Record "S3a Row";
    begin
        R."No." := 'INNER';
        R.Insert();
        Commit();
        R."No." := 'INNER2';
        R.Insert();
        Error('inner fails');
    end;
}

codeunit 50962 "S3a Tests"
{
    Subtype = Test;

    [Test]
    procedure TrappedCommitThenError()
    var
        R: Record "S3a Row";
    begin
        R."No." := 'BEFORE';
        R.Insert();
        // the Commit() inside is a savepoint under test isolation; the trapped error rolls back
        // below it
        if Codeunit.Run(Codeunit::"S3a Inner") then;
        R."No." := 'AFTER';
        R.Insert();
        // an error rolls back to the test's last Commit(), which must still exist
        asserterror Error('later');
        R."No." := 'LAST';
        R.Insert();
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("s3a-pgmem-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

/// Run the test method as al-test runs a method under codeunit isolation; returns the rows left.
fn run(cat: &'static Catalog, db: Arc<dyn Backend>, company: &str) -> Vec<String> {
    let rt = Runtime::new(cat, db);
    let mut s = Session::new(&rt, company);
    let ci = cat.code.iter().position(|c| c.name == "S3a Tests").unwrap() as u32;
    let pi = cat.code[ci as usize].find("TrappedCommitThenError")[0];
    let inst = Instance::new(ci);
    s.test = Some(TestCtx::new(cat, inst.clone(), pi).unwrap());
    let sp = s.db.savepoint().unwrap();
    s.test.as_mut().unwrap().commit_mark = sp;
    match s.invoke(ProcRef { code: ci, proc: pi }, vec![], Some(inst), None, None) {
        Ok(_) => {}
        Err(Flow::Err(e)) => panic!("{} {:?}", e.msg, e.stack),
        Err(_) => panic!("unexpected flow"),
    }
    s.db.release(sp).expect("release the method's savepoints");
    let t = cat.table_by_name("S3a Row").unwrap();
    let req = ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: usize::MAX, lock: false };
    let rows = s.db.scan(company, &rt.schemas[&t.id], &req).expect("read after the method");
    let out = rows
        .iter()
        .map(|r| match &r[0] {
            bc_store::Value::Text(s) => s.clone(),
            v => format!("{v:?}"),
        })
        .collect();
    s.test = None;
    s.rollback();
    out
}

/// BEFORE stays (the inner Commit() kept it), INNER and INNER2 went with the trapped error, AFTER
/// went with the asserterror (back to the last Commit() still standing: where the trap began),
/// LAST was written after it. Before the fix the test's last-Commit mark pointed at the inner
/// Commit()'s savepoint, which the trapped rollback had removed: PostgreSQL failed ("savepoint
/// does not exist") and aborted the transaction, memory kept AFTER.
const EXPECTED: [&str; 2] = ["BEFORE", "LAST"];

#[test]
fn trapped_commit_then_error_memory() {
    let cat = catalog();
    let rows = run(cat, Arc::new(MemBackend::new(Store::memory())), "CRONUS");
    assert_eq!(rows, EXPECTED);
}

#[test]
fn trapped_commit_then_error_postgres() {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("BC_PG_TEST not set: skipped");
        return;
    };
    let cat = catalog();
    let company = format!("S3ATEST{}", std::process::id());
    let be = PgBackend::connect_url(&url).expect("BC_PG_TEST");
    let rows = run(cat, Arc::new(be), &company);
    assert_eq!(rows, EXPECTED);
}
