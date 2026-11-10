//! Write tracking for `NavRecordState.Changes`.
//!
//! The real service tier tells the web client which records a call inserted, modified or
//! deleted (`Changes: [{TableId, IsTemporary, FormHandles, ManyChanges, Changes: [{RecordId,
//! Operation}]}]`, data/capture/nst-real sample 14). The client uses it to refresh the other
//! forms that show those tables, e.g. the list behind a card after Delete or Modify.
//! [`TrackingConn`] wraps the session's database connection and logs every write;
//! savepoint rollbacks drop the writes they undo.

use bc_store::{Agg, AggOut, Conn, Row, ScanReq, SeqOp, StoreError, TableSchema};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// One logged write: table, schema (for the key), primary key row, NavRecordOperationTypes.
#[derive(Clone)]
pub struct Write {
    pub schema: Arc<TableSchema>,
    pub key: Row,
    pub op: i64,
}

pub const OP_INSERT: i64 = 1;
pub const OP_MODIFY: i64 = 2;
pub const OP_DELETE: i64 = 4;

#[derive(Default)]
pub struct WriteLog {
    pub writes: Vec<Write>,
    /// log length at each open savepoint (store savepoint id, length)
    marks: Vec<(usize, usize)>,
}

pub type SharedLog = Rc<RefCell<WriteLog>>;

pub struct TrackingConn {
    inner: Box<dyn Conn>,
    log: SharedLog,
}

impl TrackingConn {
    pub fn wrap(inner: Box<dyn Conn>, log: SharedLog) -> TrackingConn {
        TrackingConn { inner, log }
    }

    fn pk(schema: &Arc<TableSchema>, row: &Row) -> Row {
        schema.keys.first().map(|k| k.iter().map(|&c| row[c].clone()).collect()).unwrap_or_else(|| row.clone())
    }

    fn note(&self, schema: &Arc<TableSchema>, key: Row, op: i64) {
        self.log.borrow_mut().writes.push(Write { schema: schema.clone(), key, op });
    }
}

/// Placeholder while the real connection is moved into a [`TrackingConn`].
pub struct NoConn;

fn gone() -> StoreError {
    StoreError::Other("no database connection".into())
}

impl Conn for NoConn {
    fn get(&mut self, _: &str, _: &Arc<TableSchema>, _: &Row, _: bool) -> Result<Option<Row>, StoreError> {
        Err(gone())
    }
    fn scan(&mut self, _: &str, _: &Arc<TableSchema>, _: &ScanReq) -> Result<Vec<Row>, StoreError> {
        Err(gone())
    }
    fn count(&mut self, _: &str, _: &Arc<TableSchema>, _: &[bc_store::filter::FieldFilter]) -> Result<usize, StoreError> {
        Err(gone())
    }
    fn aggregate(&mut self, _: &str, _: &Arc<TableSchema>, _: &[bc_store::filter::FieldFilter], _: usize, _: Agg) -> Result<AggOut, StoreError> {
        Err(gone())
    }
    fn insert(&mut self, _: &str, _: &Arc<TableSchema>, _: &Row) -> Result<(), StoreError> {
        Err(gone())
    }
    fn modify(&mut self, _: &str, _: &Arc<TableSchema>, _: &Row) -> Result<(), StoreError> {
        Err(gone())
    }
    fn delete(&mut self, _: &str, _: &Arc<TableSchema>, _: &Row) -> Result<(), StoreError> {
        Err(gone())
    }
    fn lock_table(&mut self, _: &str, _: &Arc<TableSchema>) -> Result<(), StoreError> {
        Err(gone())
    }
    fn is_locked(&self, _: &str, _: i64) -> bool {
        false
    }
    fn savepoint(&mut self) -> Result<usize, StoreError> {
        Err(gone())
    }
    fn rollback_to(&mut self, _: usize) -> Result<(), StoreError> {
        Err(gone())
    }
    fn commit(&mut self) -> Result<(), StoreError> {
        Err(gone())
    }
    fn rollback(&mut self) {}
    fn in_write(&self) -> bool {
        false
    }
    fn seq(&mut self, _: SeqOp) -> Result<Option<i64>, StoreError> {
        Err(gone())
    }
}

impl Conn for TrackingConn {
    fn get(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row, lock: bool) -> Result<Option<Row>, StoreError> {
        self.inner.get(company, schema, key, lock)
    }
    fn scan(&mut self, company: &str, schema: &Arc<TableSchema>, req: &ScanReq) -> Result<Vec<Row>, StoreError> {
        self.inner.scan(company, schema, req)
    }
    fn count(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[bc_store::filter::FieldFilter]) -> Result<usize, StoreError> {
        self.inner.count(company, schema, filters)
    }
    fn aggregate(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[bc_store::filter::FieldFilter], col: usize, agg: Agg) -> Result<AggOut, StoreError> {
        self.inner.aggregate(company, schema, filters, col, agg)
    }
    fn insert(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError> {
        self.inner.insert(company, schema, row)?;
        self.note(schema, Self::pk(schema, row), OP_INSERT);
        Ok(())
    }
    fn modify(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError> {
        self.inner.modify(company, schema, row)?;
        self.note(schema, Self::pk(schema, row), OP_MODIFY);
        Ok(())
    }
    fn delete(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row) -> Result<(), StoreError> {
        self.inner.delete(company, schema, key)?;
        let k = if key.len() == schema.columns.len() { Self::pk(schema, key) } else { key.clone() };
        self.note(schema, k, OP_DELETE);
        Ok(())
    }
    fn lock_table(&mut self, company: &str, schema: &Arc<TableSchema>) -> Result<(), StoreError> {
        self.inner.lock_table(company, schema)
    }
    fn is_locked(&self, company: &str, table: i64) -> bool {
        self.inner.is_locked(company, table)
    }
    fn savepoint(&mut self) -> Result<usize, StoreError> {
        let sp = self.inner.savepoint()?;
        let mut l = self.log.borrow_mut();
        let n = l.writes.len();
        l.marks.push((sp, n));
        Ok(sp)
    }
    fn rollback_to(&mut self, sp: usize) -> Result<(), StoreError> {
        self.inner.rollback_to(sp)?;
        let mut l = self.log.borrow_mut();
        if let Some(pos) = l.marks.iter().rposition(|&(s, _)| s == sp) {
            let n = l.marks[pos].1;
            l.writes.truncate(n);
            l.marks.truncate(pos);
        }
        Ok(())
    }
    fn commit(&mut self) -> Result<(), StoreError> {
        self.inner.commit()?;
        self.log.borrow_mut().marks.clear();
        Ok(())
    }
    fn rollback(&mut self) {
        self.inner.rollback();
        let mut l = self.log.borrow_mut();
        l.writes.clear();
        l.marks.clear();
    }
    fn in_write(&self) -> bool {
        self.inner.in_write()
    }
    fn seq(&mut self, op: SeqOp) -> Result<Option<i64>, StoreError> {
        self.inner.seq(op)
    }
    fn batch_hint(&self) -> usize {
        self.inner.batch_hint()
    }
    fn flush_read_cache(&mut self, table: Option<i64>) {
        self.inner.flush_read_cache(table)
    }
    fn set_lock_timeout(&mut self, on: bool) {
        self.inner.set_lock_timeout(on)
    }
    fn lock_timeout(&self) -> bool {
        self.inner.lock_timeout()
    }
    fn set_lock_timeout_duration(&mut self, secs: i64) {
        self.inner.set_lock_timeout_duration(secs)
    }
    fn lock_timeout_duration(&self) -> i64 {
        self.inner.lock_timeout_duration()
    }
}
