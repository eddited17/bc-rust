//! Database.LockTimeout(false) on PostgreSQL (`Conn::set_lock_timeout`, docs/job-queue.md): with
//! the timeout on (the default) a session waiting for another session's row lock fails after the
//! lock timeout; with it off, it waits until the lock is released. Needs BC_PG_TEST (a scratch
//! database); uses its own company schema.

use bc_store::{Backend, ColType, PgBackend, TableSchema, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn lock_timeout_off_waits_for_the_lock() {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
        return;
    };
    // a short lock timeout for this test binary (read once, before the first connection)
    std::env::set_var("BC_PG_LOCK_TIMEOUT_MS", "300");
    let be = Arc::new(PgBackend::connect_url(&url).expect("connect"));
    let schema = Arc::new(TableSchema { id: 990_014, name: "S3b Lock Timeout".into(), columns: vec![ColType::Code, ColType::Int], field_ids: vec![1, 2], keys: vec![vec![0]], per_company: true });
    let co = "s3b lock timeout";
    let key = vec![Value::Text("K".into()), Value::Int(0)];
    let mut a = be.connect().unwrap();
    let _ = a.delete(co, &schema, &key);
    a.insert(co, &schema, &vec![Value::Text("K".into()), Value::Int(1)]).unwrap();
    a.commit().unwrap();
    // session A holds the row lock
    a.modify(co, &schema, &vec![Value::Text("K".into()), Value::Int(2)]).unwrap();

    // B with the default: fails after the lock timeout
    let mut b = be.connect().unwrap();
    assert!(b.lock_timeout());
    b.lock_table(co, &schema).unwrap();
    let t0 = Instant::now();
    let e = b.get(co, &schema, &key, true).expect_err("a lock timeout");
    assert!(t0.elapsed() < Duration::from_secs(5), "waited {:?}: {e}", t0.elapsed());
    b.rollback();
    drop(b);

    // C with LockTimeout(false): waits until A commits, then reads A's row
    let (be2, schema2) = (be.clone(), schema.clone());
    let h = std::thread::spawn(move || {
        let mut c = be2.connect().unwrap();
        c.set_lock_timeout(false);
        assert!(!c.lock_timeout());
        c.lock_table(co, &schema2).unwrap();
        let t0 = Instant::now();
        let r = c.get(co, &schema2, &vec![Value::Text("K".into()), Value::Int(0)], true).map(|r| r.map(|r| r[1].clone())).map_err(|e| e.to_string());
        let waited = t0.elapsed();
        c.rollback();
        (r, waited)
    });
    std::thread::sleep(Duration::from_millis(1200));
    a.commit().unwrap();
    let (r, waited) = h.join().unwrap();
    assert_eq!(r.unwrap(), Some(Value::Int(2)));
    assert!(waited >= Duration::from_millis(900), "waited only {waited:?}");

    // a pooled connection doesn't keep the setting: a new session has the default again
    let d = be.connect().unwrap();
    assert!(d.lock_timeout());
    // the default duration is the server's lock timeout rounded up to seconds (300 ms here: 1)
    assert_eq!(d.lock_timeout_duration(), 1);

    // LockTimeoutDuration(2) (I15 W): a lock wait now ends after about 2 s instead of 300 ms
    a.modify(co, &schema, &vec![Value::Text("K".into()), Value::Int(3)]).unwrap();
    let mut e = be.connect().unwrap();
    e.set_lock_timeout_duration(2);
    assert_eq!(e.lock_timeout_duration(), 2);
    assert!(e.lock_timeout());
    e.lock_table(co, &schema).unwrap();
    let t0 = Instant::now();
    let err = e.get(co, &schema, &key, true).expect_err("a lock timeout after the session's duration");
    let waited = t0.elapsed();
    assert!(waited >= Duration::from_millis(1500) && waited < Duration::from_secs(6), "waited {waited:?}: {err}");
    e.rollback();
    // set inside a transaction it applies from the next statement on: 0 = no limit, read back as -1
    e.lock_table(co, &schema).unwrap(); // begins the transaction
    assert!(e.in_write());
    e.set_lock_timeout_duration(0);
    assert_eq!(e.lock_timeout_duration(), -1);
    assert!(!e.lock_timeout());
    e.rollback();
    a.rollback();
}
