//! A PostgreSQL restart leaves the pooled connections dead (I15 W1). bc-nst and bc serve took
//! one from the pool for the next session, and its first statement failed ("database: error
//! communicating with the server"): 7 failed sign-ins on live after the I14 PostgreSQL crash.
//! Now a connection found dead outside a transaction is replaced and the statement sent once
//! more, the idle pooled ones are dropped, and the admin connection (table setup) reconnects.
//! Inside a transaction the error stands (the transaction is lost) and the session's next
//! statement after the rollback works again.
//!
//! The test ends the backend processes with pg_terminate_backend, which the client sees as a
//! server restart does (FATAL 57P01, then a closed socket). Its connections carry their own
//! application_name, so nothing else on the database is touched. Needs BC_PG_TEST (a scratch
//! database).

use bc_store::{Backend, ColType, PgBackend, TableSchema, Value};
use std::sync::Arc;

fn terminate(url: &str, app: &str) -> i64 {
    let mut c = postgres::Client::connect(url, postgres::NoTls).unwrap();
    let r = c
        .query_one("SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE datname = current_database() AND application_name = $1 AND pid <> pg_backend_pid()", &[&app])
        .unwrap();
    // the backends are gone once they no longer show
    for _ in 0..100 {
        let n: i64 = c.query_one("SELECT count(*) FROM pg_stat_activity WHERE application_name = $1", &[&app]).unwrap().get(0);
        if n == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    r.get(0)
}

#[test]
fn dead_pooled_connections_are_replaced() {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
        return;
    };
    let app = format!("w15_reconnect_{}", std::process::id());
    let burl = format!("{url} application_name={app}");
    let be = Arc::new(PgBackend::connect_url(&burl).expect("connect"));
    let schema = Arc::new(TableSchema { id: 990_017, name: "W15 Reconnect".into(), columns: vec![ColType::Code, ColType::Int], field_ids: vec![1, 2], keys: vec![vec![0]], per_company: true });
    let co = "w15 reconnect";
    let key = vec![Value::Text("K".into()), Value::Int(0)];
    {
        let mut c = be.connect().unwrap();
        let _ = c.delete(co, &schema, &key);
        c.insert(co, &schema, &vec![Value::Text("K".into()), Value::Int(1)]).unwrap();
        c.commit().unwrap();
        // a second session at the same time, so the pool holds two idle connections
        let mut d = be.connect().unwrap();
        assert!(d.get(co, &schema, &key, false).unwrap().is_some());
    }

    // "restart": every connection of this backend (pooled and admin) is ended by the server
    assert!(terminate(&url, &app) >= 2);
    // a read in a new session
    let mut c = be.connect().unwrap();
    assert_eq!(c.get(co, &schema, &key, false).unwrap().map(|r| r[1].clone()), Some(Value::Int(1)));
    drop(c);

    // a write in a new session (its BEGIN is the first statement)
    terminate(&url, &app);
    let mut c = be.connect().unwrap();
    c.modify(co, &schema, &vec![Value::Text("K".into()), Value::Int(2)]).unwrap();
    c.commit().unwrap();
    drop(c);

    // a new table: the admin connection sets it up
    terminate(&url, &app);
    let fresh = Arc::new(TableSchema { id: 990_018, name: "W15 Reconnect 2".into(), columns: vec![ColType::Code, ColType::Int], field_ids: vec![1, 2], keys: vec![vec![0]], per_company: true });
    assert_eq!(be.table_count(co, &fresh), 0);
    let mut c = be.connect().unwrap();
    c.insert(co, &fresh, &vec![Value::Text("N".into()), Value::Int(1)]).unwrap();
    c.commit().unwrap();

    // a named lock (the pool's task claim) after a restart
    terminate(&url, &app);
    let l = be.try_lock_key("w15 reconnect lock").unwrap();
    assert!(l.is_some());
    drop(l);

    // inside a transaction the error stands; the next statement after the rollback works
    c.modify(co, &schema, &vec![Value::Text("K".into()), Value::Int(3)]).unwrap();
    terminate(&url, &app);
    assert!(c.get(co, &schema, &key, true).is_err() || c.commit().is_err(), "a lost transaction must fail");
    c.rollback();
    assert_eq!(c.get(co, &schema, &key, false).unwrap().map(|r| r[1].clone()), Some(Value::Int(2)));
    c.delete(co, &schema, &key).unwrap();
    c.delete(co, &fresh, &vec![Value::Text("N".into()), Value::Int(0)]).unwrap();
    c.commit().unwrap();
}
