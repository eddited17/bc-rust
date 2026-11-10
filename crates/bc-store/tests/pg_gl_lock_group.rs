//! The posting ledgers' end locks are one (pg.rs `lock_range_end`): BC's journal posting locks the
//! end of G/L Register before the end of G/L Entry, document posting G/L Entry first, and item and
//! G/L posting meet on Item Ledger Entry in both orders. Two sessions doing that at once must not
//! deadlock. Needs BC_PG_TEST (a scratch
//! database); uses its own company schema.

use bc_store::{Backend, ColType, PgBackend, ScanReq, TableSchema, Value};
use std::sync::{mpsc, Arc};
use std::time::Duration;

fn table(id: i64, name: &str) -> Arc<TableSchema> {
    Arc::new(TableSchema { id, name: name.into(), columns: vec![ColType::Int, ColType::Int], field_ids: vec![1, 2], keys: vec![vec![0]], per_company: true })
}

#[test]
fn posting_ledger_ends_do_not_deadlock() {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
        return;
    };
    let be = Arc::new(PgBackend::connect_url(&url).expect("connect"));
    // journal posting against document posting (G/L Register), item against G/L posting (Item Ledger Entry)
    for other in [table(45, "G/L Register"), table(32, "Item Ledger Entry")] {
        both_orders(&be, &other);
    }
}

fn both_orders(be: &Arc<PgBackend>, reg: &Arc<TableSchema>) {
    let be = be.clone();
    let co = "s3b gl lock group";
    let entry = table(17, "G/L Entry");
    let reg = reg.clone();
    {
        let mut c = be.connect().unwrap();
        for t in [&entry, &reg] {
            let _ = c.delete(co, t, &vec![Value::Int(1), Value::Int(0)]);
            c.insert(co, t, &vec![Value::Int(1), Value::Int(0)]).unwrap();
        }
        c.commit().unwrap();
    }
    // FindLast after LockTable: the locked read from the end of the table
    fn last(c: &mut Box<dyn bc_store::Conn>, co: &str, t: &Arc<TableSchema>) -> Result<(), String> {
        c.lock_table(co, t).map_err(|e| e.to_string())?;
        c.scan(co, t, &ScanReq { key: 0, reverse: true, filters: &[], after: None, limit: 1, lock: true }).map(|_| ()).map_err(|e| e.to_string())
    }
    let (tx, rx) = mpsc::channel();
    // session A, a general journal: G/L Register, then (200 ms later) G/L Entry
    let a = {
        let (be, reg, entry) = (be.clone(), reg.clone(), entry.clone());
        std::thread::spawn(move || -> Result<(), String> {
            let mut c = be.connect().map_err(|e| e.to_string())?;
            last(&mut c, co, &reg)?;
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(200));
            last(&mut c, co, &entry)?;
            std::thread::sleep(Duration::from_millis(100));
            c.commit().map_err(|e| e.to_string())
        })
    };
    // session B, a document posting: G/L Entry, then (100 ms later) G/L Register
    rx.recv().unwrap();
    let b = {
        let (be, reg, entry) = (be.clone(), reg.clone(), entry.clone());
        std::thread::spawn(move || -> Result<(), String> {
            let mut c = be.connect().map_err(|e| e.to_string())?;
            last(&mut c, co, &entry)?;
            std::thread::sleep(Duration::from_millis(100));
            last(&mut c, co, &reg)?;
            c.commit().map_err(|e| e.to_string())
        })
    };
    let (ra, rb) = (a.join().unwrap(), b.join().unwrap());
    assert!(ra.is_ok() && rb.is_ok(), "{} first: {ra:?}; G/L Entry first: {rb:?}", reg.name);
}
