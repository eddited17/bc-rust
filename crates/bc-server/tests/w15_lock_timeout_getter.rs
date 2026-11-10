//! `LockTimeout()` / `LockTimeoutDuration()` answer the session's setting on every backend (I15 W, after T's
//! review of 01-locktimeout): `LockTimeout(false)` was a no-op on the memory backends, so the getter still said
//! true afterwards, and LockTimeoutDuration was a registered guess (System.LockTimeout; the Job Queue Entry calls it
//! on every run). As in BC (ALDatabase): one per-session value in seconds, LockTimeout(false) and a duration of 0 or
//! less are -1 (no limit), LockTimeout(true) is the server's default (10 s). PostgreSQL keeps the setting in PgConn (crates/bc-store/tests/
//! pg_lock_timeout.rs); here the memory backend and al-test's copy-on-write view of a demo company
//! (`demo::cow_backend`), each with a new session getting the default (true) again.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP_JSON: &str = r#"{ "id": "8d2f5c1a-7e43-4b9a-a0c6-3f1e9b7d2c58", "name": "W15 LockTimeout Test", "publisher": "bc-rust", "version": "1.0.0.0" }"#;

const APP: &str = r#"
codeunit 50761 "W15 Lock Timeout"
{
    procedure Getter(): Text
    var
        r: Text;
    begin
        r := Format(LockTimeout());
        LockTimeout(false);
        r += '|' + Format(LockTimeout()) + '|' + Format(Database.LockTimeout());
        Database.LockTimeout(true);
        r += '|' + Format(LockTimeout());
        LockTimeout(false);
        exit(r);
    end;

    procedure Current(): Text
    begin
        exit(Format(LockTimeout()));
    end;

    procedure Duration(): Text
    var
        Prev: Integer;
        r: Text;
    begin
        Prev := LockTimeoutDuration();
        r := Format(Prev);
        LockTimeoutDuration(600);
        r += '|' + Format(LockTimeoutDuration()) + '|' + Format(Database.LockTimeoutDuration()) + '|' + Format(LockTimeout());
        LockTimeout(false);
        r += '|' + Format(LockTimeoutDuration()) + '|' + Format(LockTimeout());
        LockTimeout(true);
        r += '|' + Format(LockTimeoutDuration());
        Database.LockTimeoutDuration(0);
        r += '|' + Format(LockTimeoutDuration()) + '|' + Format(LockTimeout());
        LockTimeoutDuration(Prev);
        r += '|' + Format(LockTimeoutDuration()) + '|' + Format(LockTimeout());
        exit(r);
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("w15-locktimeout-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.json"), APP_JSON).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == "W15 Lock Timeout").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

fn check(rt: &Runtime, cat: &Catalog, what: &str) {
    let mut s = Session::new(rt, "CRONUS");
    // default on, off after LockTimeout(false) (both getters), on again after Database.LockTimeout(true)
    assert_eq!(call(&mut s, cat, "Getter"), "Yes|No|No|Yes", "{what}");
    // the session keeps its last setting (Getter ends with LockTimeout(false))
    assert_eq!(call(&mut s, cat, "Current"), "No", "{what}: same session");
    // a new session has the default
    let mut s2 = Session::new(rt, "CRONUS");
    assert_eq!(call(&mut s2, cat, "Current"), "Yes", "{what}: new session");
    // LockTimeoutDuration: seconds, one value with LockTimeout; default = the server's 10 s
    assert_eq!(call(&mut s2, cat, "Duration"), "10|600|600|Yes|-1|No|10|-1|No|10|Yes", "{what}: duration");
}

#[test]
fn lock_timeout_getter_answers_the_sessions_setting() {
    let cat = catalog();
    let mem = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    check(&mem, cat, "memory backend");
    let master = MemBackend::new(Store::memory());
    let cow = Runtime::new(cat, Arc::new(bc_server::demo::cow_backend(&master, &[]).expect("cow backend")));
    check(&cow, cat, "copy-on-write demo view");
}
