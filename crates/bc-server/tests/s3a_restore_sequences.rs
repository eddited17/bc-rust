//! `demo::restore_sequences` (al-test puts the demo company's number sequences back before every
//! codeunit) must leave Current() at the saved value and Next() at the one after it on every backend.
//! A fresh PostgreSQL sequence reports its start value as Current(), the memory store the value
//! before it: restoring with "start = saved + increment" left PostgreSQL one increment ahead (Resource
//! Employee :: EmployeeNoIncrement, SCM Costing Reports on PostgreSQL, docs/pg-vs-memory.md).
//! Memory always; PostgreSQL with `BC_PG_TEST=<scratch connection>` (sequences `shared.seq_s3arestore*`).

use bc_server::demo::{restore_sequences, SeqState};
use bc_store::{Backend, Conn, MemBackend, PgBackend, SeqOp, Store};

fn check(conn: &mut dyn Conn, name: &str) {
    // a sequence drawn up to 30 in steps of 10, and one never drawn (saved value = start - increment)
    let seqs = vec![SeqState { name: name.to_string(), current: 30, increment: 10 }, SeqState { name: format!("{name}b"), current: -9, increment: 10 }];
    restore_sequences(conn, &seqs).expect("restore");
    assert_eq!(conn.seq(SeqOp::Current(name)).unwrap(), Some(30), "Current after restore");
    assert_eq!(conn.seq(SeqOp::Next(name)).unwrap(), Some(40), "Next after restore");
    assert_eq!(conn.seq(SeqOp::Current(&format!("{name}b"))).unwrap(), Some(-9));
    assert_eq!(conn.seq(SeqOp::Next(&format!("{name}b"))).unwrap(), Some(1));
    // restoring again goes back
    restore_sequences(conn, &seqs).expect("restore again");
    assert_eq!(conn.seq(SeqOp::Next(name)).unwrap(), Some(40));
    for n in [name.to_string(), format!("{name}b")] {
        conn.seq(SeqOp::Delete(&n)).unwrap();
    }
}

#[test]
fn restored_sequences_continue_from_the_saved_value_memory() {
    let be = MemBackend::new(Store::memory());
    let mut c = be.connect().unwrap();
    check(c.as_mut(), "S3aRestore");
}

#[test]
fn restored_sequences_continue_from_the_saved_value_postgres() {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("BC_PG_TEST not set: skipped");
        return;
    };
    let be = PgBackend::connect_url(&url).expect("BC_PG_TEST");
    let mut c = be.connect().unwrap();
    check(c.as_mut(), &format!("S3aRestore{}", std::process::id()));
}
