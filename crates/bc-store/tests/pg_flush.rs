//! SelectLatestVersion on PostgreSQL (Conn::flush_read_cache, docs/job-queue.md): a session's
//! cached read doesn't hide another session's commit after the flush. Needs BC_PG_TEST (a scratch
//! database); uses its own company schema.

use bc_store::{Backend, ColType, PgBackend, TableSchema, Value};
use std::sync::Arc;

#[test]
fn flush_read_cache_sees_other_sessions_commits() {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
        return;
    };
    let be = PgBackend::connect_url(&url).expect("connect");
    let schema = Arc::new(TableSchema { id: 990_013, name: "JQ13 Flush".into(), columns: vec![ColType::Code, ColType::Int], field_ids: vec![1, 2], keys: vec![vec![0]], per_company: true });
    let co = "jq13 flush";
    let key = vec![Value::Text("K".into()), Value::Int(0)];
    let mut a = be.connect().unwrap();
    let mut b = be.connect().unwrap();
    let _ = a.delete(co, &schema, &key);
    a.insert(co, &schema, &vec![Value::Text("K".into()), Value::Int(1)]).unwrap();
    a.commit().unwrap();
    // an unlocked read outside a transaction: the session remembers it
    assert_eq!(a.get(co, &schema, &key, false).unwrap().unwrap()[1], Value::Int(1));
    b.modify(co, &schema, &vec![Value::Text("K".into()), Value::Int(2)]).unwrap();
    b.commit().unwrap();
    let before = a.get(co, &schema, &key, false).unwrap().unwrap()[1].clone();
    a.flush_read_cache(Some(990_013));
    assert_eq!(a.get(co, &schema, &key, false).unwrap().unwrap()[1], Value::Int(2), "after SelectLatestVersion");
    // the other tables' entries and a flush of every table
    b.modify(co, &schema, &vec![Value::Text("K".into()), Value::Int(3)]).unwrap();
    b.commit().unwrap();
    a.flush_read_cache(None);
    assert_eq!(a.get(co, &schema, &key, false).unwrap().unwrap()[1], Value::Int(3));
    eprintln!("cached read before the flush: {before:?}");
}
