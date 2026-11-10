//! GetPage: read one page of rows for a page (form) and encode it as a NavDataSet.
//!
//! What the web server does with the reply (Microsoft.Dynamics.Nav.Client.DataBinder:
//! `NavPageReader`, `NavInitialPageReader.CreateInitialRecords`, `VirtualRows`):
//!
//! - `Tables[0]` holds the rows in `ReadDirection`, nearest to the start first. A
//!   backward read therefore comes back in descending view order; the client
//!   inserts each row at the top.
//! - When `PageSizeInOppositeDirection > 0` there is a second table with the rows on
//!   the other side of the start, again nearest first.
//! - The last row's `bookmark` becomes the next `StartingBookmark` (with
//!   `ExcludeStartingRecord`), and `MoreDataInReadDirection` /
//!   `MoreDataInOppositeDirection` in the returned state drive scrolling.
//!
//! Start position (inferred, see the module docs in `dataset.rs`):
//! - `First` / `Last`: the first / last row of the view. `ExcludeStartingRecord` does
//!   not apply to these (the client sets it for every backward read, including the
//!   one from `Last`).
//! - `Specific` with a bookmark: that record's position. The record itself is
//!   returned unless `ExcludeStartingRecord` is set or it is outside the filters.
//!   A record that no longer exists still gives a position when sorting by the
//!   primary key; otherwise the read falls back to `First` / `Last`.
//! - `Specific` with `LookupFieldIds` / `LookupFieldValues` (foreign-key bookmark):
//!   the first row whose lookup field has that value.
//! - `Specific` with neither: `First` for Next, `Last` for Previous.
//!
//! The view is the page's SourceTableView (filter group 2), then the state's sorting
//! and filter groups. Filter group -1 (the search box) ORs its filters. Filters on
//! FlowFields and the search group are applied while reading, everything else is
//! pushed to the store. A sorting that matches no key is done in memory.

use super::bookmark::{decode_bookmark, encode_bookmark, pk_row};
use super::convert::{field_cell, field_type_code};
use super::dto::*;
use super::navdataset::*;
use al_model::{CondKind, FieldClass, FieldType, PageDef, TableDef};
use al_runtime::filter::{FNode, FieldFilter};
use al_runtime::record::{copy_record, text_to_store};
use al_runtime::{AlError, Flow, RecHandle, Session};
use bc_store::{Row, Value};
use std::cmp::Ordering;

/// Name of the read-direction table (`Tables[0]`): the table id, as the NST seems
/// to do for server-built tables.
pub fn read_table_name(table_id: i64) -> String {
    table_id.to_string()
}

/// Name of the opposite-direction table (`Tables[1]`). Any name works as long as it
/// differs from the first: the client reads both by index, and the .NET table
/// collection rejects duplicate names.
pub fn opposite_table_name(table_id: i64) -> String {
    format!("{table_id}_Opposite")
}

pub const COL_VERSION: &str = "0";
pub const COL_BOOKMARK: &str = "bookmark";
pub const COL_SUGGESTION: &str = "suggestionAttributes";

/// Extension point for the page owner: extra columns (page expression columns,
/// `MP_10` / `DFD_1`) and per-row work such as OnAfterGetRecord.
pub trait PageRowHook {
    /// Columns appended after the field columns.
    fn columns(&mut self, _tdef: &'static TableDef) -> Vec<Column> {
        vec![]
    }
    /// Called for each returned row once `rec` holds it (FlowFields calculated).
    /// Returns one cell per `columns()` entry.
    fn row(&mut self, _s: &mut Session, _rec: &RecHandle) -> Result<Vec<Cell>, AlError> {
        Ok(vec![])
    }
}

/// No extra columns.
pub struct NoHook;
impl PageRowHook for NoHook {}

#[derive(Debug, Clone)]
pub struct GetPageResult {
    pub dataset: NavDataSet,
    /// The request state with `MoreDataIn*Direction` and the effective sorting set.
    pub state: NavRecordState,
    pub more_data_in_read_direction: bool,
    pub more_data_in_opposite_direction: bool,
    /// The starting bookmark could not be used (undecodable, other table, record gone
    /// with a non-primary-key sorting) and the read started from an edge instead.
    pub bookmark_fallback: bool,
}

impl GetPageResult {
    pub fn into_response(self) -> DataSetResponse {
        DataSetResponse {
            requests: vec![],
            version_number: 0,
            client_context: None,
            data_set: self.dataset,
            state: self.state,
        }
    }
}

fn al(msg: impl Into<String>) -> AlError {
    AlError::new(msg.into())
}

fn flow(e: Flow) -> AlError {
    match e {
        Flow::Err(e) => *e,
        _ => al("unexpected control flow while reading page data"),
    }
}

/// GetPage for a page id: a fresh record on the page's source table with the page's
/// SourceTableView, then `read_page`. Page id 0 reads the table in
/// `State.TableView.TableId` instead (`get_table_page`): the web server does that
/// without a form for system tables, e.g. All Profile while starting a session.
pub fn get_page(s: &mut Session, page_id: i64, req: &GetPageRequest) -> Result<GetPageResult, AlError> {
    if page_id == 0 {
        let tid = req.state.table_view.as_ref().map_or(0, |t| t.table_id) as i64;
        return get_table_page(s, tid, &req.page_request_definition, &req.state, &mut NoHook);
    }
    get_page_with(s, page_id, &req.page_request_definition, &req.state, &mut NoHook)
}

/// GetPage without a page: rows of table `table_id` in the view the state describes
/// (TableView sorting and filters; primary key order when there is no sorting).
pub fn get_table_page(
    s: &mut Session,
    table_id: i64,
    def: &PageRequestDefinition,
    state: &NavRecordState,
    hook: &mut dyn PageRowHook,
) -> Result<GetPageResult, AlError> {
    if table_id == 0 {
        return Err(al("GetPage needs a page (FormId) or a table (TableView.TableId)."));
    }
    if !s.cat.tables.contains_key(&table_id) {
        return Err(al(format!("Table {table_id} does not exist.")));
    }
    let h = s.new_record(table_id, false);
    read_page(s, &h, def, state, hook)
}

pub fn get_page_with(
    s: &mut Session,
    page_id: i64,
    def: &PageRequestDefinition,
    state: &NavRecordState,
    hook: &mut dyn PageRowHook,
) -> Result<GetPageResult, AlError> {
    let page = s.cat.pages.get(&page_id).ok_or_else(|| al(format!("Page {page_id} does not exist.")))?;
    let Some(tid) = page.source_table else {
        // no source table: an empty row set with the system columns
        let mut ds = NavDataSet::new();
        ds.add_table(DataTable::new(read_table_name(0), system_columns()).map_err(|e| al(e.0))?)
            .map_err(|e| al(e.0))?;
        return Ok(GetPageResult {
            dataset: ds,
            state: state.clone(),
            more_data_in_read_direction: false,
            more_data_in_opposite_direction: false,
            bookmark_fallback: false,
        });
    };
    if !s.cat.tables.contains_key(&tid) {
        // own wording [src:X26]
        return Err(al(format!("Page {page_id} has source table {tid}, which does not exist.")));
    }
    let temporary = page.props.get("sourcetabletemporary").is_some_and(|v| v.eq_ignore_ascii_case("true"));
    let h = s.new_record(tid, temporary);
    apply_page_view(s, page, &h)?;
    read_page(s, &h, def, state, hook)
}

/// Apply SourceTableView: sorting, order and WHERE conditions (filter group 2).
pub fn apply_page_view(s: &mut Session, page: &PageDef, h: &RecHandle) -> Result<(), AlError> {
    let t = h.borrow().tdef;
    let v = &page.view;
    if !v.sorting.is_empty() {
        let idx: Vec<usize> = v.sorting.iter().filter_map(|f| t.field(f)).collect();
        if let Some(k) = find_key(h, &idx) {
            h.borrow_mut().key = k;
        }
    }
    h.borrow_mut().ascending = !v.descending;
    let saved = h.borrow().filter_group;
    h.borrow_mut().filter_group = 2;
    for c in &v.conds {
        let Some(fi) = t.field(&c.field) else { continue };
        let fd = &t.fields[fi];
        let text = match c.kind {
            CondKind::Const if c.value.contains(['|', '&', '.', '<', '>', '*', '(', ')', '=', '@']) => format!("'{}'", c.value.replace('\'', "''")),
            CondKind::Const | CondKind::Filter => c.value.clone(),
            // FIELD() has no source record in a SourceTableView
            CondKind::Field => continue,
        };
        let node = s.parse_field_filter(fd, &text).map_err(flow)?;
        h.borrow_mut().set_filter(fi, Some(node), text);
    }
    h.borrow_mut().filter_group = saved;
    Ok(())
}

/// Schema key index whose fields (plus the primary key appended to every index)
/// start with `fields`.
fn find_key(h: &RecHandle, fields: &[usize]) -> Option<usize> {
    if fields.is_empty() {
        return None;
    }
    let r = h.borrow();
    let pk = &r.schema.keys[0];
    r.schema.keys.iter().position(|k| {
        let mut eff = k.clone();
        eff.extend(pk.iter().copied().filter(|c| !k.contains(c)));
        eff.len() >= fields.len() && eff[..fields.len()] == *fields
    })
}

/// How a field column is filled for each row.
#[derive(Clone, Copy, PartialEq)]
enum Fill {
    /// the stored (or calculated) value
    Value,
    /// DBLazy: a FlowField not asked for in `CalcFields`, a BLOB, or a normal field
    /// outside `NormalFields` / `LoadFieldsHint`
    Lazy,
    /// the text of the record's filter on the field
    FlowFilter,
    /// Media / MediaSet: no payload yet
    Null,
}

/// The column set of a page row and how to fill it. The web server binds list rows
/// directly (`NavFormDescription.CreateRecord(row)`), and a binder whose column is
/// missing reads index -1 (IndexOutOfRangeException). So every row carries the same
/// columns as the client's own `NavFormDescription.CreateDataTable`: "0",
/// "bookmark", "suggestionAttributes", every table field, then the page's
/// expression columns (from the hook). Values that were not asked for are DBLazy,
/// which the client tolerates (it only logs a read of one).
pub(crate) struct RowLayout {
    pub columns: Vec<Column>,
    fields: Vec<(usize, Fill)>,
    n_extra: usize,
    auto_calc: Vec<usize>,
}

impl RowLayout {
    pub(crate) fn new(h: &RecHandle, def: &PageRequestDefinition, calc: &[usize], hook: &mut dyn PageRowHook) -> RowLayout {
        let tdef: &'static TableDef = h.borrow().tdef;
        let restrict: Option<Vec<usize>> = match (&def.normal_fields, &def.load_fields_hint) {
            (Some(n), _) if !n.is_empty() => Some(n.iter().filter_map(|&id| tdef.field_no(id as i64)).collect()),
            (_, Some(hint)) if !hint.is_empty() => Some(hint.iter().filter_map(|&id| tdef.field_no(id as i64)).collect()),
            _ => None,
        };
        let pk_cols = h.borrow().schema.keys[0].clone();
        let mut columns = system_columns();
        let mut fields = vec![];
        for (fi, fd) in tdef.fields.iter().enumerate() {
            if fd.id == 0 {
                continue;
            }
            let (ty, fill) = match (fd.class, field_type_code(fd)) {
                (FieldClass::FlowFilter, _) => (CellTypeCode::String, Fill::FlowFilter),
                (_, None) => (if fd.ty == FieldType::MediaSet { CellTypeCode::MediaSet } else { CellTypeCode::Media }, Fill::Null),
                (FieldClass::FlowField, Some(t)) => (t, if calc.contains(&fi) || h.borrow().auto_calc.contains(&fi) { Fill::Value } else { Fill::Lazy }),
                (FieldClass::Normal, Some(t)) if fd.ty == FieldType::Blob => (t, if calc.contains(&fi) { Fill::Value } else { Fill::Lazy }),
                (FieldClass::Normal, Some(t)) => (t, if pk_cols.contains(&fi) || restrict.as_ref().is_none_or(|r| r.contains(&fi)) { Fill::Value } else { Fill::Lazy }),
            };
            let mut col = Column::new(fd.id.to_string(), ty);
            // the real NST marks the columns it leaves unloaded
            col.lazy = fill == Fill::Lazy;
            columns.push(col);
            fields.push((fi, fill));
        }
        let extra = hook.columns(tdef);
        let n_extra = extra.len();
        columns.extend(extra);
        let mut auto_calc: Vec<usize> = calc.iter().copied().filter(|&fi| tdef.fields[fi].class == FieldClass::FlowField).collect();
        for fi in h.borrow().auto_calc.clone() {
            if !auto_calc.contains(&fi) {
                auto_calc.push(fi);
            }
        }
        RowLayout { columns, fields, n_extra, auto_calc }
    }

    /// Cells for the row `h` holds: FlowFields calculated, then the hook's cells.
    pub(crate) fn encode(&self, s: &mut Session, h: &RecHandle, hook: &mut dyn PageRowHook) -> Result<Vec<Cell>, AlError> {
        for &fi in &self.auto_calc {
            s.calc_field(h, fi).map_err(flow)?;
        }
        let extra_cells = hook.row(s, h)?;
        if extra_cells.len() != self.n_extra {
            return Err(al(format!("page row hook returned {} cells for {} columns", extra_cells.len(), self.n_extra)));
        }
        let r = h.borrow();
        let tdef = r.tdef;
        let mut cells = Vec::with_capacity(self.columns.len());
        cells.push(Cell::Bytes(encode_bookmark(&r.schema, &r.row)));
        cells.push(Cell::Null);
        cells.push(Cell::Int64(row_version(&r.row)));
        for &(fi, fill) in &self.fields {
            cells.push(match fill {
                Fill::Lazy => Cell::Lazy,
                Fill::Null => Cell::Null,
                Fill::FlowFilter => Cell::String(r.filters.iter().find(|f| f.field == fi).map(|f| f.text.clone()).unwrap_or_default()),
                // a stored value that does not match the field type (schema drift)
                // shows as blank instead of failing the whole page
                Fill::Value => field_cell(&tdef.fields[fi], &r.row[fi]).unwrap_or(Cell::Null),
            });
        }
        drop(r);
        cells.extend(extra_cells);
        Ok(cells)
    }
}

/// A one-row data set with the record `rec` holds now (not re-read from the store),
/// in the same layout as GetPage rows: the reply of ValidateField, ModifyRecord,
/// InsertRecord and the AfterGetCurrRecord application method. `calc` lists the
/// FlowFields to calculate (field indexes).
pub fn record_dataset(s: &mut Session, rec: &RecHandle, calc: &[usize], hook: &mut dyn PageRowHook) -> Result<NavDataSet, AlError> {
    let tid = rec.borrow().tdef.id;
    let lay = RowLayout::new(rec, &PageRequestDefinition::default(), calc, hook);
    let cells = lay.encode(s, rec, hook)?;
    let mut t = DataTable::new(read_table_name(tid), lay.columns.clone()).map_err(|e| al(e.0))?;
    t.push_row(cells).map_err(|e| al(e.0))?;
    let mut ds = NavDataSet::new();
    ds.add_table(t).map_err(|e| al(e.0))?;
    Ok(ds)
}

/// The system columns in the real NST's order (BC 29.1 capture): "bookmark",
/// "suggestionAttributes", "0". The client finds them by name.
fn system_columns() -> Vec<Column> {
    vec![
        Column::new(COL_BOOKMARK, CellTypeCode::ByteArray),
        Column::new(COL_SUGGESTION, CellTypeCode::String),
        Column::new(COL_VERSION, CellTypeCode::Int64),
    ]
}

/// Row version for the `"0"` column. bc-store keeps no timestamp per row, so this
/// is a content hash (FNV-1a over the stored values): it changes whenever the row
/// does, which is what the client compares; it is not monotonic.
pub fn row_version(row: &Row) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: &[u8]| {
        for &x in b {
            h ^= x as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for v in row {
        match v {
            Value::Int(i) | Value::Date(i) | Value::Time(i) | Value::Option(i) => eat(&i.to_le_bytes()),
            Value::BigInt(i) | Value::DateTime(i) | Value::Duration(i) => eat(&i.to_le_bytes()),
            Value::Dec(d) => eat(&d.serialize()),
            Value::Bool(b) => eat(&[*b as u8]),
            Value::Text(t) => {
                eat(&(t.len() as u32).to_le_bytes());
                eat(t.as_bytes())
            }
            Value::Guid(g) => eat(g),
            Value::Blob(b) => {
                eat(&(b.len() as u32).to_le_bytes());
                eat(b)
            }
        }
        eat(&[0xFF]);
    }
    (h & (i64::MAX as u64)) as i64
}

/// Filters evaluated while reading, after the store has applied the rest.
#[derive(Default)]
struct PostFilter {
    /// Filter group -1: a row matches if any of these does.
    any: Vec<FieldFilter>,
    /// Filters on FlowFields (need calculation).
    flow: Vec<FieldFilter>,
}

impl PostFilter {
    fn active(&self) -> bool {
        !self.any.is_empty() || !self.flow.is_empty()
    }

    fn passes(&self, s: &mut Session, h: &RecHandle, row: &Row) -> Result<bool, AlError> {
        if !self.active() {
            return Ok(true);
        }
        load(h, row);
        for f in &self.flow {
            s.calc_field(h, f.field).map_err(flow)?;
            if !f.node.matches(&h.borrow().row[f.field]) {
                return Ok(false);
            }
        }
        if self.any.is_empty() {
            return Ok(true);
        }
        for f in &self.any {
            let class = h.borrow().tdef.fields[f.field].class;
            match class {
                FieldClass::FlowFilter => continue,
                FieldClass::FlowField => s.calc_field(h, f.field).map_err(flow)?,
                FieldClass::Normal => {}
            }
            if f.node.matches(&h.borrow().row[f.field]) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn load(h: &RecHandle, row: &Row) {
    let mut r = h.borrow_mut();
    r.row = row.clone().into();
    r.xrow = Some(r.row.clone());
}

/// Where rows come from: keyset reads on a store key, or an in-memory sorted list
/// for a sorting that matches no key.
enum Source {
    Keyed,
    Sorted {
        rows: Vec<SortedRow>,
        fields: Vec<usize>,
        ascending: bool,
    },
}

struct SortedRow {
    key: Vec<Value>,
    pk: Box<[u8]>,
    row: Row,
}

fn sort_key(s: &mut Session, h: &RecHandle, fields: &[usize], row: &Row) -> Result<(Vec<Value>, Box<[u8]>), AlError> {
    let needs_calc = fields.iter().any(|&f| h.borrow().tdef.fields[f].class == FieldClass::FlowField);
    if needs_calc {
        load(h, row);
        for &f in fields {
            s.calc_field(h, f).map_err(flow)?;
        }
        let r = h.borrow();
        Ok((fields.iter().map(|&f| r.row[f].clone()).collect(), r.schema.pk(row)))
    } else {
        let r = h.borrow();
        Ok((fields.iter().map(|&f| row[f].clone()).collect(), r.schema.pk(row)))
    }
}

fn cmp_sorted(a: (&[Value], &[u8]), b: (&[Value], &[u8]), ascending: bool) -> Ordering {
    let mut o = Ordering::Equal;
    for (x, y) in a.0.iter().zip(b.0) {
        o = x.cmp_sql(y);
        if o != Ordering::Equal {
            break;
        }
    }
    if o == Ordering::Equal {
        o = a.1.cmp(b.1);
    }
    if ascending {
        o
    } else {
        o.reverse()
    }
}

impl Source {
    /// Up to `want` rows of the view after `after` (exclusive) in view order, or in
    /// reverse view order when `reverse`; from the start / end when `after` is None.
    fn read(&self, s: &mut Session, h: &RecHandle, post: &PostFilter, reverse: bool, after: Option<&Row>, want: usize) -> Result<Vec<Row>, AlError> {
        if want == 0 {
            return Ok(vec![]);
        }
        match self {
            Source::Keyed => {
                let batch = if post.active() { want.clamp(32, 1000) } else { want };
                let mut cursor = after.cloned();
                let mut out = vec![];
                loop {
                    let rows: Vec<Row> = s.fetch(h, reverse, cursor.as_ref(), batch).map_err(flow)?.into_iter().map(al_runtime::record::CowRow::into_row).collect();
                    let n = rows.len();
                    for row in rows {
                        let keep = post.passes(s, h, &row)?;
                        cursor = Some(row.clone());
                        if keep {
                            out.push(row);
                            if out.len() >= want {
                                return Ok(out);
                            }
                        }
                    }
                    if n < batch {
                        return Ok(out);
                    }
                }
            }
            Source::Sorted { rows, fields, ascending } => {
                let (lo, hi) = match after {
                    None => (0, rows.len()),
                    Some(a) => {
                        let (k, pk) = sort_key(s, h, fields, a)?;
                        let c = |x: &SortedRow| cmp_sorted((&x.key, &x.pk), (&k, &pk), *ascending);
                        if reverse {
                            (0, rows.partition_point(|x| c(x) == Ordering::Less))
                        } else {
                            (rows.partition_point(|x| c(x) != Ordering::Greater), rows.len())
                        }
                    }
                };
                let slice = &rows[lo..hi];
                Ok(if reverse {
                    slice.iter().rev().take(want).map(|r| r.row.clone()).collect()
                } else {
                    slice.iter().take(want).map(|r| r.row.clone()).collect()
                })
            }
        }
    }
}

/// Where reading starts.
struct Anchor {
    row: Row,
    /// The row exists and is inside the view.
    in_view: bool,
    /// First / Last of the view (ExcludeStartingRecord does not apply).
    edge: bool,
}

/// GetPage on an existing record variable (e.g. the page instance's `Rec` after
/// OnOpenPage, or a temporary source table). The record is copied, so its position
/// and filters stay as they are. Its key, order and filters are the base view; the
/// state's sorting and filter groups are applied on top.
pub fn read_page(
    s: &mut Session,
    rec: &RecHandle,
    def: &PageRequestDefinition,
    state: &NavRecordState,
    hook: &mut dyn PageRowHook,
) -> Result<GetPageResult, AlError> {
    let h = copy_record(rec);
    h.borrow_mut().prefetch = Default::default();
    let tdef: &'static TableDef = h.borrow().tdef;
    let tid = tdef.id;
    let schema = h.borrow().schema.clone();

    // ---- view: sorting
    let empty_tv = TableViewDto::default();
    let tv = state.table_view.as_ref().unwrap_or(&empty_tv);
    let mut adhoc: Option<Vec<usize>> = None;
    if let Some(ids) = tv.current_sorting_field_ids.as_ref().filter(|v| !v.is_empty()) {
        let idx: Option<Vec<usize>> = ids.iter().map(|&id| tdef.field_no(id as i64)).collect();
        if let Some(idx) = idx {
            match find_key(&h, &idx) {
                Some(k) => h.borrow_mut().key = k,
                None => adhoc = Some(idx),
            }
            h.borrow_mut().ascending = tv.ascending;
        }
    }

    // ---- view: filters
    let mut post = PostFilter::default();
    if state.table_view.is_some() {
        // The state's group 0 (user filters) replaces the record's, so a filter the user
        // removed is gone. Other groups are only added to, field by field: the client
        // cannot widen the page's own view (SourceTableView, links, code).
        h.borrow_mut().filters.retain(|f| f.group != 0);
        let groups = tv
            .current_filters
            .iter()
            .map(|g| (g.filter_group_no, g))
            .chain(tv.search_filter.iter().map(|g| (-1, g)));
        for (group_no, g) in groups {
            for f in &g.filters {
                let Some(fi) = tdef.field_no(f.filter_field as i64) else { continue };
                let text = f.filter_value.clone().unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                let fd = &tdef.fields[fi];
                let node = if f.is_exact_value {
                    let ev = |n: &str| s.enum_ordinal_for_field(fd, n);
                    let v = text_to_store(fd, &text, &ev, s.work_date).ok_or_else(|| al(format!("'{text}' is not a valid value for {}", fd.caption)))?;
                    FNode::Eq(v)
                } else {
                    s.parse_field_filter(fd, &text).map_err(flow)?
                };
                if group_no == -1 {
                    post.any.push(FieldFilter {
                        field: fi,
                        node,
                        text,
                        group: -1,
                    });
                } else {
                    let mut r = h.borrow_mut();
                    let saved = r.filter_group;
                    r.filter_group = group_no;
                    r.set_filter(fi, Some(node), text);
                    r.filter_group = saved;
                }
            }
        }
    }
    {
        // group -1 filters set in code also OR together; FlowField filters are evaluated here
        let mut r = h.borrow_mut();
        let (any, rest): (Vec<FieldFilter>, Vec<FieldFilter>) = std::mem::take(&mut r.filters).into_iter().partition(|f| f.group == -1);
        r.filters = rest;
        post.any.extend(any);
        post.flow = r
            .filters
            .iter()
            .filter(|f| tdef.fields[f.field].class == FieldClass::FlowField)
            .cloned()
            .collect();
    }

    // ---- columns
    let calc: Vec<usize> = def.calc_fields.iter().flatten().filter_map(|&id| tdef.field_no(id as i64)).collect();
    let lay = RowLayout::new(&h, def, &calc, hook);

    // ---- source
    let ascending = h.borrow().ascending;
    let source = match &adhoc {
        None => Source::Keyed,
        Some(fields) => {
            let all = s.fetch(&h, false, None, usize::MAX).map_err(flow)?;
            let mut rows = Vec::with_capacity(all.len());
            for row in all {
                if post.passes(s, &h, &row)? {
                    let (key, pk) = sort_key(s, &h, fields, &row)?;
                    rows.push(SortedRow { key, pk, row: row.into_row() });
                }
            }
            rows.sort_by(|a, b| cmp_sorted((&a.key, &a.pk), (&b.key, &b.pk), ascending));
            Source::Sorted {
                rows,
                fields: fields.clone(),
                ascending,
            }
        }
    };
    let effective_sort: Vec<i32> = match &adhoc {
        Some(f) => f.iter().map(|&fi| tdef.fields[fi].id as i32).collect(),
        None => {
            let k = h.borrow().key.min(schema.keys.len() - 1);
            schema.keys[k].iter().map(|&fi| tdef.fields[fi].id as i32).collect()
        }
    };

    // ---- start position
    let backward = def.read_direction == ReadDirection::Previous;
    let mut bookmark_fallback = false;
    let edge = |s: &mut Session, last: bool| -> Result<Option<Anchor>, AlError> {
        Ok(source.read(s, &h, &post, last, None, 1)?.into_iter().next().map(|row| Anchor {
            row,
            in_view: true,
            edge: true,
        }))
    };
    let anchor: Option<Anchor> = match def.start_from_page {
        StartingPage::First => edge(s, false)?,
        StartingPage::Last => edge(s, true)?,
        StartingPage::Specific => {
            if let Some(bm) = def.starting_bookmark.as_deref().filter(|b| !b.is_empty()) {
                match decode_bookmark(&schema, bm) {
                    Ok(pk) => {
                        let probe = copy_record(&h);
                        probe.borrow_mut().row = (pk_row(&schema, &pk)).into();
                        match s.rec_get_pk(&probe) {
                            Some(row) => {
                                let in_view = row_in_view(s, &h, &post, &row)?;
                                Some(Anchor { row: row.into_row(), in_view, edge: false })
                            }
                            None if matches!(source, Source::Keyed) && h.borrow().key == 0 => Some(Anchor {
                                row: pk_row(&schema, &pk),
                                in_view: false,
                                edge: false,
                            }),
                            None => {
                                bookmark_fallback = true;
                                edge(s, backward)?
                            }
                        }
                    }
                    Err(_) => {
                        bookmark_fallback = true;
                        edge(s, backward)?
                    }
                }
            } else if let Some(row) = lookup_start(s, &h, &post, &source, def)? {
                Some(Anchor {
                    row,
                    in_view: true,
                    edge: false,
                })
            } else {
                edge(s, backward)?
            }
        }
    };

    // ---- read both sides
    let more_info = def.include_more_data_information;
    let probe = more_info as usize;
    let page_size = def.page_size.max(0) as usize;
    let opp_size = def.page_size_in_opposite_direction.max(0) as usize;
    let (mut read_rows, mut opp_rows) = (vec![], vec![]);
    let (mut more_read, mut more_opp) = (false, false);
    if let Some(a) = &anchor {
        let include = a.in_view && (a.edge || !def.exclude_starting_record);
        let mut cand = vec![];
        if include {
            cand.push(a.row.clone());
        }
        let need = (page_size + probe).saturating_sub(cand.len());
        cand.extend(source.read(s, &h, &post, backward, Some(&a.row), need)?);
        more_read = cand.len() > page_size;
        cand.truncate(page_size);
        read_rows = cand;

        if opp_size > 0 || more_info {
            let mut cand = vec![];
            if a.in_view && !include {
                cand.push(a.row.clone());
            }
            let need = (opp_size + probe).saturating_sub(cand.len());
            cand.extend(source.read(s, &h, &post, !backward, Some(&a.row), need)?);
            more_opp = cand.len() > opp_size;
            cand.truncate(opp_size);
            opp_rows = cand;
        }
    }
    if !more_info {
        more_read = false;
        more_opp = false;
    }

    // ---- encode
    let mut ds = NavDataSet::new();
    let mut tables = vec![(read_table_name(tid), read_rows)];
    if opp_size > 0 {
        tables.push((opposite_table_name(tid), opp_rows));
    }
    for (name, rows) in tables {
        let mut t = DataTable::new(name, lay.columns.clone()).map_err(|e| al(e.0))?;
        for row in rows {
            load(&h, &row);
            let cells = lay.encode(s, &h, hook)?;
            t.push_row(cells).map_err(|e| al(e.0))?;
        }
        ds.add_table(t).map_err(|e| al(e.0))?;
    }

    let mut out_state = state.clone();
    out_state.more_data_in_read_direction = more_read;
    out_state.more_data_in_opposite_direction = more_opp;
    {
        let tv = out_state.table_view_mut();
        tv.table_id = tid as i32;
        tv.current_sorting_field_ids = Some(effective_sort);
        tv.ascending = ascending;
    }
    Ok(GetPageResult {
        dataset: ds,
        state: out_state,
        more_data_in_read_direction: more_read,
        more_data_in_opposite_direction: more_opp,
        bookmark_fallback,
    })
}

fn row_in_view(s: &mut Session, h: &RecHandle, post: &PostFilter, row: &Row) -> Result<bool, AlError> {
    {
        let r = h.borrow();
        if !r.row_matches(row) {
            return Ok(false);
        }
        if r.mark_only && !r.marks.contains(&r.schema.pk(row)) {
            return Ok(false);
        }
    }
    post.passes(s, h, row)
}

/// Foreign-key bookmark: first row of the view whose lookup field equals the value.
fn lookup_start(s: &mut Session, h: &RecHandle, post: &PostFilter, source: &Source, def: &PageRequestDefinition) -> Result<Option<Row>, AlError> {
    let (Some(ids), Some(vals)) = (&def.lookup_field_ids, &def.lookup_field_values) else {
        return Ok(None);
    };
    let (Some(&id), Some(v)) = (ids.first(), vals.as_array().and_then(|a| a.first())) else {
        return Ok(None);
    };
    let tdef = h.borrow().tdef;
    let Some(fi) = tdef.field_no(id as i64) else { return Ok(None) };
    let fd = &tdef.fields[fi];
    let raw = v.get("value").unwrap_or(v);
    let text = match raw {
        serde_json::Value::String(t) => t.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return Ok(None),
    };
    let ev = |n: &str| s.enum_ordinal_for_field(fd, n);
    let Some(target) = text_to_store(fd, &text, &ev, s.work_date) else {
        return Ok(None);
    };
    match source {
        Source::Keyed => {
            // extra equality filter in a private filter group, read one row
            let probe = copy_record(h);
            {
                let mut r = probe.borrow_mut();
                r.filter_group = 99;
                r.set_filter(fi, Some(FNode::Eq(target)), text);
                r.filter_group = 0;
            }
            Ok(source.read(s, &probe, post, false, None, 1)?.into_iter().next())
        }
        Source::Sorted { rows, .. } => Ok(rows.iter().find(|r| r.row[fi].cmp_sql(&target) == Ordering::Equal).map(|r| r.row.clone())),
    }
}
