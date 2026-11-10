//! Two sessions creating the same number sequence in their transactions (I15 W2). The pool's
//! workers insert their first Job Queue Log Entry at the same moment after a cold boot, and each
//! creates the AutoIncrement counter "$AutoIncrement:<company>:474": the second CREATE SEQUENCE
//! waited for the first transaction and then failed with `duplicate key value violates unique
//! constraint "pg_class_relname_nsp_index"` (S3b soak4, codeunit 448 failed 1 s after the start),
//! which aborted its transaction. Now the second waits for the first and finds the sequence.
//! Delete and Insert of one sequence in one transaction (No. Series RecreateNoSeries) still work.
//! Needs BC_PG_TEST (a scratch database); own company schema and sequences `shared.seq_w15*`.

use bc_store::{Backend, ColType, PgBackend, SeqOp, TableSchema, Value};
use std::sync::Arc;
use std::time::Duration;

#[test]
fn concurrent_creators_of_one_sequence_both_succeed() {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
        return;
    };
    let be = Arc::new(PgBackend::connect_url(&url).expect("connect"));
    let schema = Arc::new(TableSchema { id: 990_016, name: "W15 Sequence Race".into(), columns: vec![ColType::Code, ColType::Int], field_ids: vec![1, 2], keys: vec![vec![0]], per_company: true });
    let co = "w15 sequence race";
    let name = format!("W15Race{}", std::process::id());
    let mut a = be.connect().unwrap();
    let _ = a.seq(SeqOp::Delete(&name));
    for k in ["A", "B"] {
        let _ = a.delete(co, &schema, &vec![Value::Text(k.into()), Value::Int(0)]);
    }
    a.commit().unwrap();

    // A: a transaction that wrote something, then creates the sequence (not committed yet)
    a.insert(co, &schema, &vec![Value::Text("A".into()), Value::Int(1)]).unwrap();
    assert_eq!(a.seq(SeqOp::Exists(&name)).unwrap(), Some(0));
    a.seq(SeqOp::Create(&name, 1, 1)).unwrap();
    assert_eq!(a.seq(SeqOp::Next(&name)).unwrap(), Some(1));

    // B, the same in its own transaction, while A's is open
    let (be2, schema2, name2) = (be.clone(), schema.clone(), name.clone());
    let b = std::thread::spawn(move || -> Result<Option<i64>, String> {
        let mut b = be2.connect().map_err(|e| e.to_string())?;
        b.insert(co, &schema2, &vec![Value::Text("B".into()), Value::Int(1)]).map_err(|e| e.to_string())?;
        if b.seq(SeqOp::Exists(&name2)).map_err(|e| e.to_string())? != Some(1) {
            b.seq(SeqOp::Create(&name2, 1, 1)).map_err(|e| e.to_string())?;
        }
        let n = b.seq(SeqOp::Next(&name2)).map_err(|e| e.to_string())?;
        b.commit().map_err(|e| e.to_string())?;
        Ok(n)
    });
    std::thread::sleep(Duration::from_millis(500));
    a.commit().unwrap();
    let got = b.join().unwrap();
    assert_eq!(got, Ok(Some(2)), "the second creator");
    // both transactions' rows are there
    for k in ["A", "B"] {
        assert!(a.get(co, &schema, &vec![Value::Text(k.into()), Value::Int(0)], false).unwrap().is_some(), "row {k}");
    }

    // Delete and Insert in one transaction (RecreateNoSeries): the new sequence counts from its start
    a.insert(co, &schema, &vec![Value::Text("C".into()), Value::Int(1)]).unwrap();
    a.seq(SeqOp::Delete(&name)).unwrap();
    a.seq(SeqOp::Create(&name, 100, 10)).unwrap();
    assert_eq!(a.seq(SeqOp::Next(&name)).unwrap(), Some(100));
    a.commit().unwrap();
    assert_eq!(a.seq(SeqOp::Next(&name)).unwrap(), Some(110));

    // a rolled-back Create leaves no sequence behind
    let other = format!("{name}b");
    a.insert(co, &schema, &vec![Value::Text("D".into()), Value::Int(1)]).unwrap();
    a.seq(SeqOp::Create(&other, 1, 1)).unwrap();
    a.rollback();
    assert_eq!(a.seq(SeqOp::Exists(&other)).unwrap(), Some(0));

    a.seq(SeqOp::Delete(&name)).unwrap();
    for k in ["A", "B", "C"] {
        let _ = a.delete(co, &schema, &vec![Value::Text(k.into()), Value::Int(0)]);
    }
    a.commit().unwrap();
}
