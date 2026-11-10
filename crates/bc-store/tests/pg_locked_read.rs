//! A locked read holds its row lock until the transaction ends, also when it is the first
//! statement (pg.rs `get`/`scan` begin the transaction, as a locked read does in BC). BC's
//! stale-record check reads the stored row with a lock right before Modify; a lock that ended with
//! the statement let a concurrent change through, and the Modify then wrote the whole stale row
//! back (the I14 soak's silent overwrites). Needs BC_PG_TEST (a scratch database); own company schema.

use bc_store::{Backend, ColType, PgBackend, TableSchema, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn locked_read_outside_a_transaction_holds_the_lock() {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
        return;
    };
    let be = Arc::new(PgBackend::connect_url(&url).expect("connect"));
    let schema = Arc::new(TableSchema { id: 990_015, name: "S3b Locked Read".into(), columns: vec![ColType::Code, ColType::Int], field_ids: vec![1, 2], keys: vec![vec![0]], per_company: true });
    let co = "s3b locked read";
    let row = |v: i32| vec![Value::Text("K".into()), Value::Int(v)];
    let mut x = be.connect().unwrap();
    let _ = x.delete(co, &schema, &row(0));
    x.insert(co, &schema, &row(1)).unwrap();
    x.commit().unwrap();

    // session X: the stale check's locked read, before X has written anything
    assert_eq!(x.get(co, &schema, &row(0), true).unwrap().unwrap()[1], Value::Int(1));
    // session Y changes the row meanwhile: it has to wait for X's lock
    let (be2, schema2) = (be.clone(), schema.clone());
    let y = std::thread::spawn(move || -> Result<Duration, String> {
        let mut y = be2.connect().map_err(|e| e.to_string())?;
        let t0 = Instant::now();
        y.modify(co, &schema2, &vec![Value::Text("K".into()), Value::Int(2)]).map_err(|e| e.to_string())?;
        y.commit().map_err(|e| e.to_string())?;
        Ok(t0.elapsed())
    });
    std::thread::sleep(Duration::from_millis(600));
    x.modify(co, &schema, &row(3)).unwrap();
    x.commit().unwrap();
    let waited = y.join().unwrap().unwrap();
    assert!(waited >= Duration::from_millis(400), "session Y changed the row while X held its locked read (waited {waited:?})");
    let mut z = be.connect().unwrap();
    assert_eq!(z.get(co, &schema, &row(0), false).unwrap().unwrap()[1], Value::Int(2), "Y's change comes after X's commit");
}
