//! The database: per-company tables, single-writer transactions with undo,
//! and a write-ahead log for durability (replayed at startup).

use crate::table::*;
use crate::value::*;
use parking_lot::{lock_api::ArcMutexGuard, Mutex, RawMutex, RwLock};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type Resolver = Box<dyn Fn(i64) -> Option<Arc<TableSchema>> + Send + Sync>;

type TKey = (Arc<str>, i64);

enum Undo {
    Inserted(TKey, Key),
    Replaced(TKey, Arc<Row>),
    Deleted(TKey, Arc<Row>),
}

/// A session's transaction. Reads never block; the first write acquires the
/// database writer token, which is held until commit or rollback.
#[derive(Default)]
pub struct Txn {
    guard: Option<ArcMutexGuard<RawMutex, ()>>,
    undo: Vec<Undo>,
    /// redo length before each undo entry (for savepoint rollback)
    redo_at: Vec<usize>,
    redo: Vec<u8>,
}

impl Txn {
    pub fn is_writing(&self) -> bool {
        self.guard.is_some()
    }
    pub fn pending_ops(&self) -> usize {
        self.undo.len()
    }
}

pub struct Store {
    tables: RwLock<HashMap<TKey, TableData>>,
    writer: Arc<Mutex<()>>,
    wal: Mutex<Option<BufWriter<File>>>,
    wal_path: Option<PathBuf>,
    sync: std::sync::atomic::AtomicBool,
    companies: RwLock<HashMap<String, Arc<str>>>,
}

pub use crate::backend::StoreStats;

impl Store {
    pub fn memory() -> Store {
        Store {
            tables: RwLock::new(HashMap::new()),
            writer: Arc::new(Mutex::new(())),
            wal: Mutex::new(None),
            wal_path: None,
            sync: std::sync::atomic::AtomicBool::new(false),
            companies: RwLock::new(HashMap::new()),
        }
    }

    /// Open (or create) a database whose durable state is the WAL at `path`.
    pub fn open(path: &Path, resolver: &Resolver) -> std::io::Result<(Store, usize)> {
        let mut s = Store::memory();
        let mut replayed = 0;
        if path.exists() {
            let mut buf = Vec::new();
            File::open(path)?.read_to_end(&mut buf)?;
            replayed = s.replay(&buf, resolver);
        }
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        s.wal = Mutex::new(Some(BufWriter::with_capacity(1 << 20, f)));
        s.wal_path = Some(path.to_path_buf());
        s.sync = std::sync::atomic::AtomicBool::new(true);
        Ok((s, replayed))
    }

    fn company_key(&self, company: &str, per_company: bool) -> Arc<str> {
        let c = if per_company { company } else { "" };
        if let Some(k) = self.companies.read().get(c) {
            return k.clone();
        }
        let k: Arc<str> = c.into();
        self.companies.write().insert(c.to_string(), k.clone());
        k
    }

    fn tkey(&self, company: &str, schema: &TableSchema) -> TKey {
        (self.company_key(company, schema.per_company), schema.id)
    }

    fn replay(&mut self, buf: &[u8], resolver: &Resolver) -> usize {
        let mut r = Reader { b: buf, p: 0 };
        let mut n = 0;
        let tables = self.tables.get_mut();
        let mut cache: HashMap<i64, Option<Arc<TableSchema>>> = HashMap::new();
        while let Some(len) = r.u32() {
            let Some(rec) = r.take(len as usize) else { break }; // torn tail write: ignore
            let mut rr = Reader { b: rec, p: 0 };
            while rr.p < rec.len() {
                let Some(op) = rr.u8() else { break };
                let Some(company) = rr.str() else { break };
                let Some(tid) = rr.i64() else { break };
                let schema = cache.entry(tid).or_insert_with(|| resolver(tid)).clone();
                match op {
                    1 => {
                        let Some(ncols) = rr.u32() else { break };
                        let mut pairs = Vec::with_capacity(ncols as usize);
                        for _ in 0..ncols {
                            let (Some(fid), Some(v)) = (rr.i64(), rr.value()) else { break };
                            pairs.push((fid, v));
                        }
                        let Some(schema) = schema else { continue };
                        let mut row = schema.new_row();
                        for (fid, v) in pairs {
                            if let Some(ci) = schema.field_ids.iter().position(|&x| x == fid) {
                                if std::mem::discriminant(&row[ci]) == std::mem::discriminant(&v) {
                                    row[ci] = v;
                                }
                            }
                        }
                        let key: TKey = (company.as_str().into(), tid);
                        tables.entry(key).or_insert_with(|| TableData::new(schema.clone())).put(row);
                    }
                    2 => {
                        let Some(pk) = rr.bytes() else { break };
                        let key: TKey = (company.as_str().into(), tid);
                        if let Some(t) = tables.get_mut(&key) {
                            let _ = t.delete(pk);
                        }
                    }
                    _ => break,
                }
                n += 1;
            }
        }
        // re-intern company keys
        let keys: Vec<Arc<str>> = tables.keys().map(|k| k.0.clone()).collect();
        let comp = self.companies.get_mut();
        for k in keys {
            comp.entry(k.to_string()).or_insert(k);
        }
        n
    }

    fn begin_write(&self, txn: &mut Txn) {
        if txn.guard.is_none() {
            txn.guard = Some(self.writer.lock_arc());
        }
    }

    fn redo_put(txn: &mut Txn, company: &str, schema: &TableSchema, row: &Row) {
        let o = &mut txn.redo;
        o.push(1);
        o.extend_from_slice(&(company.len() as u32).to_le_bytes());
        o.extend_from_slice(company.as_bytes());
        o.extend_from_slice(&schema.id.to_le_bytes());
        o.extend_from_slice(&(row.len() as u32).to_le_bytes());
        for (i, v) in row.iter().enumerate() {
            o.extend_from_slice(&schema.field_ids[i].to_le_bytes());
            write_value(v, o);
        }
    }

    fn redo_del(txn: &mut Txn, company: &str, schema: &TableSchema, pk: &[u8]) {
        let o = &mut txn.redo;
        o.push(2);
        o.extend_from_slice(&(company.len() as u32).to_le_bytes());
        o.extend_from_slice(company.as_bytes());
        o.extend_from_slice(&schema.id.to_le_bytes());
        o.extend_from_slice(&(pk.len() as u32).to_le_bytes());
        o.extend_from_slice(pk);
    }

    pub fn insert(&self, txn: &mut Txn, company: &str, schema: &Arc<TableSchema>, mut row: Row) -> Result<(), StoreError> {
        schema.round_datetimes(&mut row);
        self.begin_write(txn);
        let k = self.tkey(company, schema);
        let mut g = self.tables.write();
        let t = g.entry(k.clone()).or_insert_with(|| TableData::new(schema.clone()));
        let mut buf = Txn::default();
        Self::redo_put(&mut buf, &k.0, schema, &row);
        let pk = t.insert(row)?;
        txn.redo.extend_from_slice(&buf.redo);
        txn.redo_at.push(txn.redo.len() - buf.redo.len());
        txn.undo.push(Undo::Inserted(k, pk));
        Ok(())
    }

    pub fn modify(&self, txn: &mut Txn, company: &str, schema: &Arc<TableSchema>, mut row: Row) -> Result<(), StoreError> {
        schema.round_datetimes(&mut row);
        self.begin_write(txn);
        let k = self.tkey(company, schema);
        let mut g = self.tables.write();
        let t = g.get_mut(&k).ok_or(StoreError::NotFound)?;
        let mut buf = Txn::default();
        Self::redo_put(&mut buf, &k.0, schema, &row);
        let old = t.modify(row)?;
        txn.redo.extend_from_slice(&buf.redo);
        txn.redo_at.push(txn.redo.len() - buf.redo.len());
        txn.undo.push(Undo::Replaced(k, old));
        Ok(())
    }

    pub fn delete(&self, txn: &mut Txn, company: &str, schema: &Arc<TableSchema>, pk: &[u8]) -> Result<Arc<Row>, StoreError> {
        self.begin_write(txn);
        let k = self.tkey(company, schema);
        let mut g = self.tables.write();
        let t = g.get_mut(&k).ok_or(StoreError::NotFound)?;
        let old = t.delete(pk)?;
        txn.redo_at.push(txn.redo.len());
        Self::redo_del(txn, &k.0, schema, pk);
        txn.undo.push(Undo::Deleted(k, old.clone()));
        Ok(old)
    }

    pub fn get(&self, company: &str, schema: &TableSchema, pk: &[u8]) -> Option<Arc<Row>> {
        let k = self.tkey(company, schema);
        self.tables.read().get(&k).and_then(|t| t.get(pk).cloned())
    }

    pub fn scan(
        &self,
        company: &str,
        schema: &TableSchema,
        index: usize,
        lo: Bound<&[u8]>,
        hi: Bound<&[u8]>,
        reverse: bool,
        f: impl FnMut(&[u8], &Arc<Row>) -> bool,
    ) {
        let k = self.tkey(company, schema);
        if let Some(t) = self.tables.read().get(&k) {
            t.scan(index, lo, hi, reverse, f);
        }
    }

    /// Run `f` on a table under the read lock (None if the table has no data yet).
    pub fn with_table<R>(&self, company: &str, schema: &TableSchema, f: impl FnOnce(&TableData) -> R) -> Option<R> {
        let k = self.tkey(company, schema);
        self.tables.read().get(&k).map(f)
    }

    pub fn count(&self, company: &str, schema: &TableSchema) -> usize {
        let k = self.tkey(company, schema);
        self.tables.read().get(&k).map_or(0, |t| t.len())
    }

    pub fn commit(&self, txn: &mut Txn) -> std::io::Result<()> {
        if txn.guard.is_none() {
            return Ok(());
        }
        if !txn.redo.is_empty() {
            if let Some(w) = self.wal.lock().as_mut() {
                w.write_all(&(txn.redo.len() as u32).to_le_bytes())?;
                w.write_all(&txn.redo)?;
                w.flush()?;
                if self.sync.load(std::sync::atomic::Ordering::Relaxed) {
                    w.get_ref().sync_data()?;
                }
            }
        }
        txn.redo.clear();
        txn.undo.clear();
        txn.redo_at.clear();
        txn.guard = None;
        Ok(())
    }

    pub fn rollback(&self, txn: &mut Txn) {
        if txn.guard.is_none() {
            return;
        }
        self.rollback_to(txn, 0);
        txn.guard = None;
    }

    /// Undo changes made after savepoint `mark` (= `txn.pending_ops()` at the savepoint).
    pub fn rollback_to(&self, txn: &mut Txn, mark: usize) {
        if txn.undo.len() <= mark {
            return;
        }
        let mut g = self.tables.write();
        while txn.undo.len() > mark {
            let u = txn.undo.pop().unwrap();
            let at = txn.redo_at.pop().unwrap_or(0);
            txn.redo.truncate(at);
            match u {
                Undo::Inserted(k, pk) => {
                    if let Some(t) = g.get_mut(&k) {
                        let _ = t.delete(&pk);
                    }
                }
                Undo::Replaced(k, old) | Undo::Deleted(k, old) => {
                    if let Some(t) = g.get_mut(&k) {
                        t.put((*old).clone());
                    }
                }
            }
        }
    }

    /// Rewrite the WAL as a compact snapshot of the current state.
    pub fn compact(&self) -> std::io::Result<()> {
        let Some(path) = &self.wal_path else { return Ok(()) };
        let _w = self.writer.lock();
        let tmp = path.with_extension("compact");
        {
            let mut f = BufWriter::with_capacity(1 << 22, File::create(&tmp)?);
            let g = self.tables.read();
            for ((company, _), t) in g.iter() {
                let mut txn = Txn::default();
                for row in t.rows.values() {
                    Self::redo_put(&mut txn, company, &t.schema, row);
                    if txn.redo.len() > 1 << 20 {
                        f.write_all(&(txn.redo.len() as u32).to_le_bytes())?;
                        f.write_all(&txn.redo)?;
                        txn.redo.clear();
                    }
                }
                if !txn.redo.is_empty() {
                    f.write_all(&(txn.redo.len() as u32).to_le_bytes())?;
                    f.write_all(&txn.redo)?;
                }
            }
            f.flush()?;
            f.get_ref().sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        let f = OpenOptions::new().append(true).open(path)?;
        *self.wal.lock() = Some(BufWriter::with_capacity(1 << 20, f));
        Ok(())
    }

    /// Durability switch: fdatasync on every commit (default for file-backed stores).
    pub fn set_sync(&self, on: bool) {
        self.sync.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn stats(&self) -> StoreStats {
        let g = self.tables.read();
        StoreStats { tables: g.len(), rows: g.values().map(|t| t.len()).sum() }
    }
}
