//! The Get cache of unlocked reads on PostgreSQL ends with the transaction (I14 S1, docs/job-queue.md):
//! a session that read a row, then ends its transaction, sees another session's committed change
//! in its next transaction. Before, the per-session cache outlived commit, and a long-lived session
//! read (and could write back) a row hours old. Needs BC_PG_TEST (a scratch database); uses its own
//! company schema.

use bc_store::{Backend, ColType, PgBackend, TableSchema, Value};
use std::sync::Arc;

#[test]
fn next_transaction_sees_other_sessions_commit() {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
        return;
    };
    let be = PgBackend::connect_url(&url).expect("connect");
    let schema = Arc::new(TableSchema { id: 990_015, name: "I14 Tx Cache".into(), columns: vec![ColType::Code, ColType::Int], field_ids: vec![1, 2], keys: vec![vec![0]], per_company: true });
    let co = "i14 txcache";
    let key = vec![Value::Text("K".into()), Value::Int(0)];
    let row = |n: i32| vec![Value::Text("K".into()), Value::Int(n)];
    let mut a = be.connect().unwrap();
    let mut b = be.connect().unwrap();
    let _ = a.delete(co, &schema, &key);
    a.insert(co, &schema, &row(1)).unwrap();
    a.commit().unwrap();

    // A reads without a lock, outside a write transaction (a web client request)
    assert_eq!(a.get(co, &schema, &key, false).unwrap().unwrap()[1], Value::Int(1));
    // B changes the row: not committed yet, A keeps reading the committed row
    b.modify(co, &schema, &row(2)).unwrap();
    assert_eq!(a.get(co, &schema, &key, false).unwrap().unwrap()[1], Value::Int(1));
    // B commits: its commit drops what every session of the backend cached of the table
    b.commit().unwrap();
    assert_eq!(a.get(co, &schema, &key, false).unwrap().unwrap()[1], Value::Int(2), "unlocked Get after the other session's commit");
    // A's request ends; its next transaction reads the committed row
    a.commit().unwrap();
    assert_eq!(a.get(co, &schema, &key, false).unwrap().unwrap()[1], Value::Int(2), "unlocked Get in a new transaction after commit");

    // the cache survives commits of transactions that didn't write the table (posting commits
    // mid-request): A's cached row is answered without a read after A's own commit
    a.commit().unwrap();
    let other_table = Arc::new(TableSchema { id: 990_016, name: "I14 Tx Cache 2".into(), columns: vec![ColType::Code, ColType::Int], field_ids: vec![1, 2], keys: vec![vec![0]], per_company: true });
    let _ = b.delete(co, &other_table, &key);
    b.insert(co, &other_table, &row(5)).unwrap();
    b.commit().unwrap();
    let q0 = be.queries();
    assert_eq!(a.get(co, &schema, &key, false).unwrap().unwrap()[1], Value::Int(2));
    assert_eq!(be.queries(), q0, "a cached row of a table nobody wrote since is not read again");

    // the same after a write transaction of A's own (other table rows, same table)
    let other = vec![Value::Text("L".into()), Value::Int(0)];
    let _ = a.delete(co, &schema, &other);
    a.insert(co, &schema, &vec![Value::Text("L".into()), Value::Int(7)]).unwrap();
    assert_eq!(a.get(co, &schema, &key, false).unwrap().unwrap()[1], Value::Int(2));
    a.commit().unwrap();
    b.modify(co, &schema, &row(3)).unwrap();
    b.commit().unwrap();
    assert_eq!(a.get(co, &schema, &key, false).unwrap().unwrap()[1], Value::Int(3), "after A's write transaction committed");

    // and after a rollback
    b.modify(co, &schema, &row(4)).unwrap();
    b.commit().unwrap();
    a.rollback();
    assert_eq!(a.get(co, &schema, &key, false).unwrap().unwrap()[1], Value::Int(4), "after rollback");

    // a row known missing is not missing forever either
    let gone = vec![Value::Text("M".into()), Value::Int(0)];
    let _ = b.delete(co, &schema, &gone);
    b.commit().unwrap();
    assert!(a.get(co, &schema, &gone, false).unwrap().is_none());
    a.commit().unwrap();
    b.insert(co, &schema, &vec![Value::Text("M".into()), Value::Int(9)]).unwrap();
    b.commit().unwrap();
    assert_eq!(a.get(co, &schema, &gone, false).unwrap().unwrap()[1], Value::Int(9), "a row inserted by another session");
    a.commit().unwrap();
}
