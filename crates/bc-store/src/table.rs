//! One table's rows plus its secondary indexes, held in ordered B-trees.

use crate::value::*;
use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::Arc;

pub type Key = Box<[u8]>;

#[derive(Debug, Clone)]
pub struct TableSchema {
    pub id: i64,
    pub name: String,
    pub columns: Vec<ColType>,
    /// AL field id per column (WAL rows are stored by field id).
    pub field_ids: Vec<i64>,
    /// keys[0] is the primary key; each key lists column indexes.
    pub keys: Vec<Vec<usize>>,
    pub per_company: bool,
}

impl TableSchema {
    /// Round the row's DateTime columns to what the database keeps (`sql_datetime_ms`). Database
    /// writes go through this; temporary tables (TableData without a Store) keep exact values,
    /// as BC's in-memory temporary records do.
    pub fn round_datetimes(&self, row: &mut Row) {
        for (i, c) in self.columns.iter().enumerate() {
            if matches!(c, ColType::DateTime) {
                if let Value::DateTime(ms) = &mut row[i] {
                    *ms = sql_datetime_ms(*ms);
                }
            }
        }
    }

    /// The row as the database stores it (see `round_datetimes`); borrowed when nothing changes.
    pub fn stored_row<'a>(&self, row: &'a Row) -> std::borrow::Cow<'a, Row> {
        let changes = self.columns.iter().enumerate().any(|(i, c)| matches!(c, ColType::DateTime) && matches!(row[i], Value::DateTime(ms) if sql_datetime_ms(ms) != ms));
        if changes {
            let mut r = row.clone();
            self.round_datetimes(&mut r);
            std::borrow::Cow::Owned(r)
        } else {
            std::borrow::Cow::Borrowed(row)
        }
    }

    pub fn new_row(&self) -> Row {
        self.columns.iter().map(|c| Value::default_for(*c)).collect()
    }

    pub fn pk(&self, row: &Row) -> Key {
        let mut out = Vec::with_capacity(32);
        for &c in &self.keys[0] {
            encode_key_part(&row[c], &mut out);
        }
        out.into_boxed_slice()
    }

    /// Index key: key fields followed by the primary key (unique, stable ordering).
    pub fn index_key(&self, index: usize, row: &Row) -> Key {
        if index == 0 {
            return self.pk(row);
        }
        let mut out = Vec::with_capacity(48);
        for &c in &self.keys[index] {
            encode_key_part(&row[c], &mut out);
        }
        for &c in &self.keys[0] {
            encode_key_part(&row[c], &mut out);
        }
        out.into_boxed_slice()
    }

    /// Encoded prefix for the first `vals.len()` fields of key `index`.
    pub fn prefix(&self, vals: &[Value]) -> Vec<u8> {
        let mut out = Vec::new();
        for v in vals {
            encode_key_part(v, &mut out);
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct TableData {
    pub schema: Arc<TableSchema>,
    pub rows: BTreeMap<Key, Arc<Row>>,
    /// Secondary indexes (keys[1..]): index key → primary key.
    pub indexes: Vec<BTreeMap<Key, Key>>,
    pub last_auto: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("The record already exists.")]
    Duplicate,
    #[error("The record does not exist.")]
    NotFound,
    #[error("{0}")]
    Other(String),
}

impl TableData {
    pub fn new(schema: Arc<TableSchema>) -> Self {
        let n = schema.keys.len().saturating_sub(1);
        TableData { schema, rows: BTreeMap::new(), indexes: (0..n).map(|_| BTreeMap::new()).collect(), last_auto: 0 }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn get(&self, pk: &[u8]) -> Option<&Arc<Row>> {
        self.rows.get(pk)
    }

    fn index_add(&mut self, pk: &Key, row: &Row) {
        for i in 1..self.schema.keys.len() {
            let k = self.schema.index_key(i, row);
            self.indexes[i - 1].insert(k, pk.clone());
        }
    }

    fn index_remove(&mut self, row: &Row) {
        for i in 1..self.schema.keys.len() {
            let k = self.schema.index_key(i, row);
            self.indexes[i - 1].remove(&k);
        }
    }

    pub fn insert(&mut self, row: Row) -> Result<Key, StoreError> {
        let pk = self.schema.pk(&row);
        if self.rows.contains_key(&pk) {
            return Err(StoreError::Duplicate);
        }
        self.index_add(&pk, &row);
        self.rows.insert(pk.clone(), Arc::new(row));
        Ok(pk)
    }

    /// Replace the row with the same primary key; returns the previous row.
    pub fn modify(&mut self, row: Row) -> Result<Arc<Row>, StoreError> {
        let pk = self.schema.pk(&row);
        let old = self.rows.get(&pk).cloned().ok_or(StoreError::NotFound)?;
        self.index_remove(&old);
        self.index_add(&pk, &row);
        self.rows.insert(pk, Arc::new(row));
        Ok(old)
    }

    /// Insert or replace (WAL replay / undo).
    pub fn put(&mut self, row: Row) {
        let pk = self.schema.pk(&row);
        if let Some(old) = self.rows.get(&pk).cloned() {
            self.index_remove(&old);
        }
        self.index_add(&pk, &row);
        self.rows.insert(pk, Arc::new(row));
    }

    pub fn delete(&mut self, pk: &[u8]) -> Result<Arc<Row>, StoreError> {
        let old = self.rows.remove(pk).ok_or(StoreError::NotFound)?;
        self.index_remove(&old);
        Ok(old)
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        for i in &mut self.indexes {
            i.clear();
        }
    }

    /// Visit rows in `index` order within bounds; the visitor returns false to stop.
    pub fn scan(
        &self,
        index: usize,
        lo: Bound<&[u8]>,
        hi: Bound<&[u8]>,
        reverse: bool,
        mut f: impl FnMut(&[u8], &Arc<Row>) -> bool,
    ) {
        // an empty range (filter `B..A` with A < B, or a resume position past the upper bound)
        // has no rows; BTreeMap::range panics on it
        if let (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) = (lo, hi) {
            if a > b || (a == b && !(matches!(lo, Bound::Included(_)) && matches!(hi, Bound::Included(_)))) {
                return;
            }
        }
        if index == 0 || index > self.indexes.len() {
            let r = self.rows.range::<[u8], _>((lo, hi));
            if reverse {
                for (k, v) in r.rev() {
                    if !f(k, v) {
                        break;
                    }
                }
            } else {
                for (k, v) in r {
                    if !f(k, v) {
                        break;
                    }
                }
            }
        } else {
            let idx = &self.indexes[index - 1];
            let r = idx.range::<[u8], _>((lo, hi));
            let mut visit = |k: &Key, pk: &Key| match self.rows.get(pk) {
                Some(row) => f(k, row),
                None => true,
            };
            if reverse {
                for (k, pk) in r.rev() {
                    if !visit(k, pk) {
                        break;
                    }
                }
            } else {
                for (k, pk) in r {
                    if !visit(k, pk) {
                        break;
                    }
                }
            }
        }
    }
}

/// Upper bound for all keys starting with `prefix`.
pub fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut v = prefix.to_vec();
    while let Some(last) = v.pop() {
        if last < 0xFF {
            v.push(last + 1);
            return Some(v);
        }
    }
    None
}
