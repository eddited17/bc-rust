//! Storage backend abstraction. The AL runtime talks to a `Conn` (one per
//! session); `Backend` hands them out. Two implementations: PostgreSQL
//! (`pg.rs`, the system of record) and the embedded in-memory engine
//! (`MemBackend`, used for tests and benchmarks).

use crate::filter::{FNode, FieldFilter};
use crate::store::{Store, Txn};
use crate::table::{prefix_end, StoreError, TableData, TableSchema};
use crate::value::*;
use parking_lot::Mutex;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::ops::Bound;
use std::sync::Arc;

/// A range read in key order. `after` is a full row: results start strictly after
/// its position in `key` order (keyset pagination, used by Next()).
pub struct ScanReq<'a> {
    pub key: usize,
    pub reverse: bool,
    pub filters: &'a [FieldFilter],
    pub after: Option<&'a Row>,
    pub limit: usize,
    /// Read with update locks (after LockTable).
    pub lock: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agg {
    Count,
    Sum,
    Min,
    Max,
    Exists,
}

#[derive(Debug, Clone)]
pub enum AggOut {
    Count(i64),
    Sum(Decimal),
    Value(Option<Value>),
    Exists(bool),
}

pub enum SeqOp<'a> {
    Exists(&'a str),
    Create(&'a str, i64, i64),
    Next(&'a str),
    Current(&'a str),
    Delete(&'a str),
    Restart(&'a str, i64),
}

#[derive(Debug, Default, Clone)]
pub struct StoreStats {
    pub tables: usize,
    pub rows: usize,
}

pub trait Backend: Send + Sync {
    fn connect(&self) -> Result<Box<dyn Conn>, StoreError>;
    fn describe(&self) -> String;
    fn stats(&self) -> StoreStats;
    /// Row count of a table (no filters); used for dashboard tiles.
    fn table_count(&self, company: &str, schema: &Arc<TableSchema>) -> usize;
    fn compact(&self) -> Result<(), StoreError> {
        Ok(())
    }
    /// Create/upgrade storage for every table up front (schema sync). Returns tables synced.
    fn sync_schema(&self, _company: &str, _schemas: &[Arc<TableSchema>]) -> Result<usize, StoreError> {
        Ok(0)
    }
    /// Statements sent to the database so far (0 for the embedded engine).
    fn queries(&self) -> u64 {
        0
    }
    /// Try to take the exclusive lock `name` without waiting (None: someone holds it). The lock
    /// is held until the returned guard is dropped, across any number of transactions, and ends
    /// with the process: background task claims (al-runtime workers, docs/job-queue.md).
    /// Default: a lock within this process (the embedded engine has no other processes).
    fn try_lock_key(&self, name: &str) -> Result<Option<Box<dyn KeyLock>>, StoreError> {
        let mut held = LOCAL_KEYS.lock().unwrap_or_else(|e| e.into_inner());
        if held.iter().any(|k| k == name) {
            return Ok(None);
        }
        held.push(name.to_string());
        Ok(Some(Box::new(LocalKeyLock(name.to_string()))))
    }
}

/// A held [`Backend::try_lock_key`] lock; released when dropped.
pub trait KeyLock: Send {
    /// Count one more start of the locked work and return the count (crash-loop guard: the count
    /// survives a crash of the process that held the lock). Backends without durable state: 1.
    fn attempt(&mut self) -> Result<i32, StoreError> {
        Ok(1)
    }
    /// The locked work is done: forget its start count.
    fn finish(&mut self) {}
}

static LOCAL_KEYS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

struct LocalKeyLock(String);

impl KeyLock for LocalKeyLock {}

impl Drop for LocalKeyLock {
    fn drop(&mut self) {
        LOCAL_KEYS.lock().unwrap_or_else(|e| e.into_inner()).retain(|k| *k != self.0);
    }
}

pub trait Conn {
    fn get(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row, lock: bool) -> Result<Option<Row>, StoreError>;
    fn scan(&mut self, company: &str, schema: &Arc<TableSchema>, req: &ScanReq) -> Result<Vec<Row>, StoreError>;
    /// `get`, sharing the stored row where the backend keeps rows in memory (the caller copies only
    /// when it writes: Arc::make_mut). Default: the owned row, wrapped without a copy.
    fn get_shared(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row, lock: bool) -> Result<Option<Arc<Row>>, StoreError> {
        Ok(self.get(company, schema, key, lock)?.map(Arc::new))
    }
    /// `scan`, sharing stored rows like `get_shared`. Default: the owned rows, wrapped.
    fn scan_shared(&mut self, company: &str, schema: &Arc<TableSchema>, req: &ScanReq) -> Result<Vec<Arc<Row>>, StoreError> {
        Ok(self.scan(company, schema, req)?.into_iter().map(Arc::new).collect())
    }
    fn count(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[FieldFilter]) -> Result<usize, StoreError>;
    fn aggregate(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[FieldFilter], col: usize, agg: Agg) -> Result<AggOut, StoreError>;
    fn insert(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError>;
    fn modify(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError>;
    fn delete(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row) -> Result<(), StoreError>;
    fn lock_table(&mut self, company: &str, schema: &Arc<TableSchema>) -> Result<(), StoreError>;
    fn is_locked(&self, company: &str, table: i64) -> bool;
    fn savepoint(&mut self) -> Result<usize, StoreError>;
    fn rollback_to(&mut self, sp: usize) -> Result<(), StoreError>;
    /// Forget savepoint `sp` and every savepoint taken after it, keeping their writes: no
    /// rollback goes back to them any more. Backends whose savepoints cost nothing: no-op.
    fn release(&mut self, _sp: usize) -> Result<(), StoreError> {
        Ok(())
    }
    fn commit(&mut self) -> Result<(), StoreError>;
    fn rollback(&mut self);
    fn in_write(&self) -> bool;
    fn seq(&mut self, op: SeqOp) -> Result<Option<i64>, StoreError>;
    /// Names of the number sequences (lower case). Backends that can't list them return none.
    fn seq_names(&mut self) -> Result<Vec<String>, StoreError> {
        Ok(vec![])
    }
    /// Rows to prefetch per Next() round trip (1 = no prefetch).
    fn batch_hint(&self) -> usize {
        1
    }
    /// SelectLatestVersion([TableNo]) (BC: NavSession.FlushDataCache): later reads of the table
    /// (`None`: of every table) don't answer from results this session cached before now. Rows
    /// the transaction wrote or locked stay. Backends without caches: nothing to do.
    fn flush_read_cache(&mut self, _table: Option<i64>) {}
    /// Database.LockTimeout(on): `false` lets this session wait for a lock as long as it takes,
    /// `true` (the default) at most the lock timeout. Backends without lock waits: nothing to do.
    fn set_lock_timeout(&mut self, _on: bool) {}
    /// Database.LockTimeout(): this session's setting.
    fn lock_timeout(&self) -> bool {
        true
    }
    /// LockTimeoutDuration(seconds) (BC: ALDatabase.ALLockTimeoutDuration): the session's lock timeout; 0 or
    /// less is no limit, as LockTimeout(false). Backends that keep no session settings: nothing to do.
    fn set_lock_timeout_duration(&mut self, _secs: i64) {}
    /// LockTimeoutDuration(): the session's lock timeout in seconds, -1 without limit.
    fn lock_timeout_duration(&self) -> i64 {
        crate::pg::default_lock_timeout_secs()
    }
}

// ---------------------------------------------------------------- shared scan logic

/// Key-range bounds implied by equality / range filters on the leading key fields.
pub fn key_bounds(schema: &TableSchema, key: usize, filters: &[FieldFilter]) -> (Vec<u8>, Option<Vec<u8>>) {
    let mut prefix = Vec::new();
    let mut lo = None;
    let mut hi = None;
    for &kf in &schema.keys[key] {
        let fl: Vec<&FieldFilter> = filters.iter().filter(|f| f.field == kf).collect();
        if fl.len() != 1 {
            break;
        }
        if let Some(v) = fl[0].node.eq_value() {
            encode_key_part(v, &mut prefix);
            continue;
        }
        if let Some((a, b)) = fl[0].node.range() {
            if let Some(a) = a {
                let mut p = prefix.clone();
                encode_key_part(a, &mut p);
                lo = Some(p);
            }
            if let Some(b) = b {
                let mut p = prefix.clone();
                encode_key_part(b, &mut p);
                hi = prefix_end(&p);
            }
        }
        break;
    }
    let lo = lo.unwrap_or_else(|| prefix.clone());
    let hi = match hi {
        Some(h) => Some(h),
        None if !prefix.is_empty() => prefix_end(&prefix),
        None => None,
    };
    (lo, hi)
}

pub fn row_matches(filters: &[FieldFilter], row: &Row) -> bool {
    filters.iter().all(|f| f.node.matches(&row[f.field]))
}

/// Run a ScanReq against one in-memory table (temporary tables and MemBackend).
pub fn scan_table(t: &TableData, req: &ScanReq) -> Vec<Row> {
    scan_table_shared(t, req).into_iter().map(|r| (*r).clone()).collect()
}

/// `scan_table`, handing out the table's rows themselves (shared: a holder copies when it writes).
pub fn scan_table_shared(t: &TableData, req: &ScanReq) -> Vec<Arc<Row>> {
    let key = req.key.min(t.schema.keys.len() - 1);
    let (lo, hi) = key_bounds(&t.schema, key, req.filters);
    let after = req.after.map(|r| t.schema.index_key(key, r));
    let mut lo_b: Bound<&[u8]> = if lo.is_empty() { Bound::Unbounded } else { Bound::Included(&lo) };
    let mut hi_b: Bound<&[u8]> = match &hi {
        Some(h) => Bound::Excluded(h),
        None => Bound::Unbounded,
    };
    if let Some(a) = &after {
        if req.reverse {
            if hi.as_ref().is_none_or(|h| a.as_ref() < h.as_slice()) {
                hi_b = Bound::Excluded(a);
            }
        } else if a.as_ref() >= lo.as_slice() {
            lo_b = Bound::Excluded(a);
        }
    }
    let mut out = Vec::new();
    t.scan(key, lo_b, hi_b, req.reverse, |_, row| {
        if row_matches(req.filters, row) {
            out.push(row.clone());
        }
        out.len() < req.limit
    });
    out
}

pub fn aggregate_rows<'a>(rows: impl Iterator<Item = &'a Row>, col: usize, agg: Agg) -> AggOut {
    match agg {
        Agg::Count => AggOut::Count(rows.count() as i64),
        Agg::Exists => AggOut::Exists(rows.take(1).count() > 0),
        Agg::Sum => {
            let mut s = Decimal::ZERO;
            for r in rows {
                s += match &r[col] {
                    Value::Dec(d) => *d,
                    Value::Int(i) => Decimal::from(*i),
                    Value::BigInt(i) => Decimal::from(*i),
                    _ => Decimal::ZERO,
                };
            }
            AggOut::Sum(s)
        }
        Agg::Min | Agg::Max => {
            let mut best: Option<&Value> = None;
            for r in rows {
                let v = &r[col];
                let better = match best {
                    None => true,
                    Some(b) => {
                        let o = v.cmp_sql(b);
                        if agg == Agg::Min {
                            o.is_lt()
                        } else {
                            o.is_gt()
                        }
                    }
                };
                if better {
                    best = Some(v);
                }
            }
            AggOut::Value(best.cloned())
        }
    }
}

// ---------------------------------------------------------------- in-memory backend

pub struct MemBackend {
    pub store: Arc<Store>,
    seqs: Arc<Mutex<HashMap<String, (i64, i64)>>>,
}

impl MemBackend {
    pub fn new(store: Store) -> MemBackend {
        MemBackend { store: Arc::new(store), seqs: Arc::new(Mutex::new(HashMap::new())) }
    }
}

impl Backend for MemBackend {
    fn connect(&self) -> Result<Box<dyn Conn>, StoreError> {
        Ok(Box::new(MemConn { store: self.store.clone(), txn: Txn::default(), seqs: self.seqs.clone(), lock_dur: None }))
    }
    fn describe(&self) -> String {
        "embedded in-memory store + WAL".into()
    }
    fn stats(&self) -> StoreStats {
        self.store.stats()
    }
    fn table_count(&self, company: &str, schema: &Arc<TableSchema>) -> usize {
        self.store.count(company, schema)
    }
    fn compact(&self) -> Result<(), StoreError> {
        self.store.compact().map_err(|e| StoreError::Other(e.to_string()))
    }
}

pub struct MemConn {
    store: Arc<Store>,
    txn: Txn,
    seqs: Arc<Mutex<HashMap<String, (i64, i64)>>>,
    /// LockTimeout / LockTimeoutDuration as AL set them (seconds, -1 no limit; None = the default), kept so
    /// the getters answer the session's setting (there are no lock waits in memory)
    lock_dur: Option<i64>,
}

impl Conn for MemConn {
    fn get(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row, _lock: bool) -> Result<Option<Row>, StoreError> {
        Ok(self.store.get(company, schema, &schema.pk(key)).map(|r| (*r).clone()))
    }
    fn scan(&mut self, company: &str, schema: &Arc<TableSchema>, req: &ScanReq) -> Result<Vec<Row>, StoreError> {
        Ok(self.store.with_table(company, schema, |t| scan_table(t, req)).unwrap_or_default())
    }
    fn get_shared(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row, _lock: bool) -> Result<Option<Arc<Row>>, StoreError> {
        Ok(self.store.get(company, schema, &schema.pk(key)))
    }
    fn scan_shared(&mut self, company: &str, schema: &Arc<TableSchema>, req: &ScanReq) -> Result<Vec<Arc<Row>>, StoreError> {
        Ok(self.store.with_table(company, schema, |t| scan_table_shared(t, req)).unwrap_or_default())
    }
    fn count(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[FieldFilter]) -> Result<usize, StoreError> {
        if filters.is_empty() {
            return Ok(self.store.count(company, schema));
        }
        let req = ScanReq { key: best_key(schema, filters), reverse: false, filters, after: None, limit: usize::MAX, lock: false };
        Ok(self.store.with_table(company, schema, |t| scan_table(t, &req).len()).unwrap_or(0))
    }
    fn aggregate(&mut self, company: &str, schema: &Arc<TableSchema>, filters: &[FieldFilter], col: usize, agg: Agg) -> Result<AggOut, StoreError> {
        let req = ScanReq {
            key: best_key(schema, filters),
            reverse: false,
            filters,
            after: None,
            limit: if agg == Agg::Exists { 1 } else { usize::MAX },
            lock: false,
        };
        let rows = self.store.with_table(company, schema, |t| scan_table(t, &req)).unwrap_or_default();
        Ok(aggregate_rows(rows.iter(), col, agg))
    }
    fn insert(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError> {
        self.store.insert(&mut self.txn, company, schema, row.clone())
    }
    fn modify(&mut self, company: &str, schema: &Arc<TableSchema>, row: &Row) -> Result<(), StoreError> {
        self.store.modify(&mut self.txn, company, schema, row.clone())
    }
    fn delete(&mut self, company: &str, schema: &Arc<TableSchema>, key: &Row) -> Result<(), StoreError> {
        self.store.delete(&mut self.txn, company, schema, &schema.pk(key)).map(|_| ())
    }
    fn lock_table(&mut self, _c: &str, _s: &Arc<TableSchema>) -> Result<(), StoreError> {
        Ok(()) // writes are serialized by the single writer token
    }
    fn is_locked(&self, _c: &str, _t: i64) -> bool {
        false
    }
    fn savepoint(&mut self) -> Result<usize, StoreError> {
        Ok(self.txn.pending_ops())
    }
    fn rollback_to(&mut self, sp: usize) -> Result<(), StoreError> {
        self.store.rollback_to(&mut self.txn, sp);
        Ok(())
    }
    fn commit(&mut self) -> Result<(), StoreError> {
        self.store.commit(&mut self.txn).map_err(|e| StoreError::Other(e.to_string()))
    }
    fn rollback(&mut self) {
        self.store.rollback(&mut self.txn)
    }
    fn set_lock_timeout(&mut self, on: bool) {
        self.lock_dur = if on { None } else { Some(-1) };
    }
    fn lock_timeout(&self) -> bool {
        self.lock_timeout_duration() != -1
    }
    fn set_lock_timeout_duration(&mut self, secs: i64) {
        self.lock_dur = Some(if secs <= 0 { -1 } else { secs });
    }
    fn lock_timeout_duration(&self) -> i64 {
        self.lock_dur.unwrap_or_else(crate::pg::default_lock_timeout_secs)
    }
    fn in_write(&self) -> bool {
        self.txn.is_writing()
    }
    fn seq(&mut self, op: SeqOp) -> Result<Option<i64>, StoreError> {
        let mut s = self.seqs.lock();
        Ok(match op {
            SeqOp::Exists(n) => Some(s.contains_key(&n.to_lowercase()) as i64),
            SeqOp::Create(n, start, inc) => {
                s.insert(n.to_lowercase(), (start - inc, inc));
                None
            }
            SeqOp::Next(n) => match s.get_mut(&n.to_lowercase()) {
                Some(e) => {
                    e.0 += e.1;
                    Some(e.0)
                }
                // own wording [src:X07]
                None => return Err(StoreError::Other(format!("There is no number sequence named '{n}'."))),
            },
            SeqOp::Current(n) => s.get(&n.to_lowercase()).map(|e| e.0),
            SeqOp::Delete(n) => {
                s.remove(&n.to_lowercase());
                None
            }
            SeqOp::Restart(n, start) => {
                if let Some(e) = s.get_mut(&n.to_lowercase()) {
                    e.0 = start - e.1;
                }
                None
            }
        })
    }
    fn seq_names(&mut self) -> Result<Vec<String>, StoreError> {
        Ok(self.seqs.lock().keys().cloned().collect())
    }
}

/// Pick the key whose leading fields have the most equality filters.
pub fn best_key(schema: &TableSchema, filters: &[FieldFilter]) -> usize {
    let mut best = (0usize, 0usize);
    for (ki, k) in schema.keys.iter().enumerate() {
        let n = k.iter().take_while(|kf| filters.iter().any(|f| f.field == **kf && f.node.eq_value().is_some())).count();
        if n > best.1 {
            best = (ki, n);
        }
    }
    best.0
}

pub fn _unused(_: FNode) {}
