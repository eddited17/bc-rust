//! Record variables: current row, filters, key, temporary storage.

use crate::filter::*;
use crate::value::*;
use al_model::{FieldDef, FieldType, TableDef};
use bc_store::{ColType, Row, TableData, TableSchema, Value};
use rust_decimal::prelude::*;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

pub type RecHandle = Rc<RecCell>;

/// A record's row, shared until one holder writes it (copy on write, P12). Copies of a record
/// (by-value parameters, `A := B`, xRec), the prefetch buffer, a new record's initial row and rows
/// read from a temporary table share one allocation. Reads go through `Deref`; any `&mut` access
/// (`row[i] = v`, `iter_mut`, ...) first makes the row unique (`Arc::make_mut`), so a holder never
/// sees another holder's writes.
#[derive(Clone, Default, PartialEq)]
pub struct CowRow(Arc<Row>);

impl std::ops::Deref for CowRow {
    type Target = Row;
    #[inline]
    fn deref(&self) -> &Row {
        &self.0
    }
}

impl std::ops::DerefMut for CowRow {
    #[inline]
    fn deref_mut(&mut self) -> &mut Row {
        Arc::make_mut(&mut self.0)
    }
}

impl From<Row> for CowRow {
    #[inline]
    fn from(r: Row) -> CowRow {
        CowRow(Arc::new(r))
    }
}

impl From<Arc<Row>> for CowRow {
    #[inline]
    fn from(r: Arc<Row>) -> CowRow {
        CowRow(r)
    }
}

impl CowRow {
    /// An owned copy, for code that needs a `Row`.
    #[inline]
    pub fn to_vec(&self) -> Row {
        (*self.0).clone()
    }

    /// The row itself: no copy when this is its only holder.
    #[inline]
    pub fn into_row(self) -> Row {
        Arc::try_unwrap(self.0).unwrap_or_else(|a| (*a).clone())
    }

    /// The shared allocation (e.g. to store the row in a table without copying it).
    #[inline]
    pub fn shared(&self) -> Arc<Row> {
        self.0.clone()
    }
}

/// A record variable's state (P12). A new record's row is filled in on its first access: until then
/// `init` holds its table's initial row (crate::frames::initial_row), shared by all new records of
/// that table. Every access to the state goes through `borrow` / `borrow_mut`, which fill the row
/// first, so a record nobody touches never copies it (most table globals and many locals).
pub struct RecCell {
    cell: RefCell<RecState>,
    init: std::cell::Cell<Option<CowRow>>,
}

impl RecCell {
    pub fn new(s: RecState) -> RecCell {
        RecCell { cell: RefCell::new(s), init: std::cell::Cell::new(None) }
    }

    /// A state whose row is still `init` (its `row` is empty until the first access).
    pub fn with_initial_row(s: RecState, init: CowRow) -> RecCell {
        RecCell { cell: RefCell::new(s), init: std::cell::Cell::new(Some(init)) }
    }

    #[inline]
    fn fill(&self) {
        // only the first access finds `init`; any Ref/RefMut alive now came from an access that
        // already took it, so the borrow_mut here never conflicts
        if let Some(r) = self.init.take() {
            // shared, not copied (CowRow)
            self.cell.borrow_mut().row = r;
        }
    }

    #[inline]
    pub fn borrow(&self) -> std::cell::Ref<'_, RecState> {
        self.fill();
        self.cell.borrow()
    }

    #[inline]
    pub fn borrow_mut(&self) -> std::cell::RefMut<'_, RecState> {
        self.fill();
        self.cell.borrow_mut()
    }

    /// An independent copy of the state (a new record variable); a copy of a record that was
    /// never accessed stays unfilled.
    pub fn duplicate(&self) -> RecCell {
        let init = self.init.take();
        let s = self.cell.borrow().clone();
        self.init.set(init.clone());
        RecCell { cell: RefCell::new(s), init: std::cell::Cell::new(init) }
    }
}

#[derive(Clone)]
pub struct RecState {
    pub tdef: &'static TableDef,
    pub schema: Arc<TableSchema>,
    pub row: CowRow,
    pub xrow: Option<CowRow>,
    pub filters: Vec<FieldFilter>,
    pub filter_group: i32,
    pub key: usize,
    /// Custom sorting: SetCurrentKey (or SetView) on fields that start no key, even with the
    /// primary key appended. BC then sorts by those fields and the primary key
    /// (TableState.AssignCustomSortingFields). None: sort by `key`.
    pub custom_key: Option<Vec<usize>>,
    pub ascending: bool,
    pub temp: Option<Rc<RefCell<TableData>>>,
    pub company: Option<String>,
    pub globals: Option<Rc<crate::interp::Globals>>,
    pub ext_globals: Vec<(u32, Rc<crate::interp::Globals>)>,
    pub marks: BTreeSet<Box<[u8]>>,
    pub mark_only: bool,
    pub auto_calc: Vec<usize>,
    /// Rows prefetched by Next() (database backends), served while the cursor
    /// position and the view (key, order, filters) are unchanged.
    pub prefetch: Prefetch,
    /// A RecordRef (reference semantics) rather than a Record variable.
    pub is_ref: bool,
    /// A RecordRef that has not been opened (Open/GetTable) yet.
    pub unopened: bool,
}

#[derive(Clone, Default)]
pub struct Prefetch {
    pub rows: std::collections::VecDeque<CowRow>,
    pub last_key: Option<Box<[u8]>>,
    pub view: String,
    pub reverse: bool,
    /// Session::write_seq when the rows were read: they are served only while no other record
    /// has written the table since (Session::table_writes).
    pub seq: u64,
}

impl RecState {
    pub fn table_name(&self) -> &str {
        &self.tdef.name
    }
    pub fn is_temp(&self) -> bool {
        self.temp.is_some()
    }
    pub fn field(&self, i: usize) -> &'static FieldDef {
        &self.tdef.fields[i]
    }
    pub fn get_val(&self, i: usize) -> Val {
        to_val(self.field(i), &self.row[i])
    }
    pub fn pk(&self) -> Box<[u8]> {
        self.schema.pk(&self.row)
    }
    /// Filters on the given field in the current filter group plus all others (they AND together).
    pub fn active_filters(&self) -> impl Iterator<Item = &FieldFilter> {
        self.filters.iter()
    }
    pub fn set_filter(&mut self, field: usize, node: Option<FNode>, text: String) {
        let g = self.filter_group;
        self.filters.retain(|f| !(f.field == field && f.group == g));
        if let Some(node) = node {
            self.filters.push(FieldFilter { field, node, text, group: g });
        }
    }
    /// Filters that apply to stored rows (FlowFilter/FlowField filters only parametrize FlowFields).
    pub fn stored_filters(&self) -> Vec<FieldFilter> {
        self.filters
            .iter()
            .filter(|f| {
                let c = self.tdef.fields[f.field].class;
                f.group != -1 && c != al_model::FieldClass::FlowFilter && c != al_model::FieldClass::FlowField
            })
            .cloned()
            .collect()
    }

    /// Filters on FlowFields: they restrict the view, checked on the calculated value of each row
    /// (crate::recops fetch); FlowFilter fields only parametrize FlowFields.
    pub fn flowfield_filters(&self) -> Vec<FieldFilter> {
        self.filters.iter().filter(|f| f.group != -1 && self.tdef.fields[f.field].class == al_model::FieldClass::FlowField).cloned().collect()
    }

    /// Fingerprint of everything that defines the record's view.
    pub fn view_fingerprint(&self) -> String {
        format!("{}|{:?}|{}|{:?}|{}", self.key, self.custom_key, self.ascending, self.filters, self.mark_only)
    }

    /// The sort position of `row` in this record's current order (its key, or custom sorting).
    pub fn order_key(&self, row: &Row) -> Box<[u8]> {
        match &self.custom_key {
            Some(cs) => {
                let mut out = Vec::with_capacity(48);
                for &c in cs.iter().chain(self.schema.keys[0].iter()) {
                    bc_store::value::encode_key_part(&row[c], &mut out);
                }
                out.into_boxed_slice()
            }
            None => self.schema.index_key(self.key.min(self.schema.keys.len() - 1), row),
        }
    }

    /// SetCurrentKey(fields): the first key whose fields start with `fields` [src:A06.6] (or whose
    /// fields followed by the primary key's do: open question [src:A06.7]); otherwise custom
    /// sorting by `fields`, then the primary key. FlowFields, FlowFilters and Blobs can't sort (an
    /// error); the order then stays [src:A06.6].
    pub fn set_sort_fields(&mut self, fields: &[usize]) {
        if fields.is_empty() {
            return;
        }
        let pk = &self.schema.keys[0];
        let best = self.schema.keys.iter().position(|k| {
            let full: Vec<usize> = k.iter().copied().chain(pk.iter().copied().filter(|f| !k.contains(f))).collect();
            full.len() >= fields.len() && full[..fields.len()] == fields[..]
        });
        if let Some(k) = best {
            self.key = k;
            self.custom_key = None;
            return;
        }
        if fields.iter().any(|&f| {
            let fd = self.field(f);
            !matches!(fd.class, al_model::FieldClass::Normal) || matches!(fd.ty, al_model::FieldType::Blob)
        }) {
            return;
        }
        self.key = 0;
        self.custom_key = Some(fields.to_vec());
    }

    pub fn row_matches(&self, row: &Row) -> bool {
        let unstored = |f: &FieldFilter| {
            self.tdef.fields[f.field].class == al_model::FieldClass::FlowFilter || self.tdef.fields[f.field].class == al_model::FieldClass::FlowField
        };
        // filter group -1 (cross-column search) ORs its filters [src:A06.5]
        let cross: Vec<&FieldFilter> = self.filters.iter().filter(|f| f.group == -1 && !unstored(f)).collect();
        self.filters.iter().filter(|f| f.group != -1).all(|f| unstored(f) || f.node.matches(&row[f.field]))
            && (cross.is_empty() || cross.iter().any(|f| f.node.matches(&row[f.field])))
    }

    /// The stored-field filters of filter group -1 (cross-column search): a row is in the view
    /// when any of them matches (BC FilterGroup(-1)).
    pub fn cross_filters(&self) -> Vec<FieldFilter> {
        self.filters
            .iter()
            .filter(|f| {
                let c = self.tdef.fields[f.field].class;
                f.group == -1 && c != al_model::FieldClass::FlowFilter && c != al_model::FieldClass::FlowField
            })
            .cloned()
            .collect()
    }
    pub fn pk_text(&self) -> String {
        self.tdef
            .pk()
            .fields
            .iter()
            .map(|&i| format!("{}='{}'", self.tdef.fields[i].name, self.get_val(i).to_text()))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// The field with number `no` (RecRef.Field, FieldExist). Number 0 is BC's `timestamp` field
/// that every table has (NCLMetaTable AllFields[0], BigInteger): the SQL rowversion of the row,
/// the column SystemRowVersion reads too, so it resolves to SystemRowVersion here.
pub fn field_by_no(t: &TableDef, no: i64) -> Option<usize> {
    t.field_no(if no == 0 { 2000000005 } else { no })
}

/// The error for a FieldRef whose RecordRef no longer has field index `fi`: the RecordRef was
/// closed or opened on another table after the FieldRef was taken (BC's NavFieldRef is bound to
/// its owner RecordRef, so it is no longer valid either).
pub fn stale_fieldref(h: &RecHandle, fi: usize) -> Option<String> {
    let r = h.borrow();
    (fi >= r.tdef.fields.len()).then(|| {
        if r.unopened {
            format!("The FieldRef (field index {}) belongs to a RecordRef that is not open.", fi + 1)
        } else {
            format!("The FieldRef (field index {}) does not belong to table {}: its RecordRef was opened on another table.", fi + 1, r.tdef.name)
        }
    })
}

pub fn copy_record(r: &RecHandle) -> RecHandle {
    Rc::new(r.duplicate())
}

pub fn col_type(f: &FieldDef) -> ColType {
    match &f.ty {
        FieldType::Integer => ColType::Int,
        FieldType::BigInteger => ColType::BigInt,
        FieldType::Decimal => ColType::Dec,
        FieldType::Boolean => ColType::Bool,
        FieldType::Code(_) => ColType::Code,
        FieldType::Text(_) | FieldType::DateFormula | FieldType::TableFilter | FieldType::RecordId | FieldType::Other(_) => ColType::Text,
        FieldType::Date => ColType::Date,
        FieldType::Time => ColType::Time,
        FieldType::DateTime => ColType::DateTime,
        FieldType::Duration => ColType::Duration,
        FieldType::Guid | FieldType::Media | FieldType::MediaSet => ColType::Guid,
        FieldType::Option | FieldType::Enum(_) => ColType::Option,
        FieldType::Blob => ColType::Blob,
    }
}

pub fn schema_for(t: &TableDef) -> TableSchema {
    TableSchema {
        id: t.id,
        name: t.name.clone(),
        columns: t.fields.iter().map(col_type).collect(),
        field_ids: t.fields.iter().map(|f| f.id).collect(),
        keys: t.keys.iter().filter(|k| k.enabled || k.fields == t.keys[0].fields).map(|k| k.fields.clone()).collect(),
        per_company: t.data_per_company,
    }
}

pub static CATALOG: std::sync::OnceLock<&'static al_model::Catalog> = std::sync::OnceLock::new();

pub fn enum_id(name: &str) -> Option<i64> {
    CATALOG.get().and_then(|c| c.id_of(al_syntax::ObjectKind::Enum, name))
}

pub fn to_val(f: &FieldDef, v: &Value) -> Val {
    match (&f.ty, v) {
        (FieldType::Enum(name), Value::Option(o)) => {
            Val::Opt { ord: *o, enum_id: enum_id(name).or(Some(-1)) }
        }
        // an Option field's value carries the field's members and captions, so a Variant or
        // FieldRef.Value formats it as its caption [src:A17.3]
        (FieldType::Option, Value::Option(o)) if !f.options.is_empty() => Val::Opt { ord: *o, enum_id: Some(crate::optmeta::for_field_ref(f)) },
        (FieldType::DateFormula, Value::Text(s)) => Val::DateFormula(s.clone()),
        (FieldType::RecordId, Value::Text(s)) => match decode_record_id(s) {
            Some((t, k)) => Val::RecordId(t, k),
            None => Val::RecordId(0, vec![]),
        },
        _ => store_to_val(v),
    }
}

/// RecordID fields are stored in a text column as `<table id>:<tagged key values>`; values are
/// tagged with their type (T text, I int, B bigint, D decimal, b bool, d date, t time, M datetime,
/// U duration, G guid, O option) and separated by `,` (`\` escapes `,` and `\` in text).
/// A blank RecordID is the empty string.
pub fn encode_record_id(table: i64, key: &[Value]) -> String {
    if table == 0 {
        return String::new();
    }
    let mut out = format!("{table}:");
    for (i, v) in key.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match v {
            Value::Text(t) => {
                out.push('T');
                for c in t.chars() {
                    if c == ',' || c == '\\' {
                        out.push('\\');
                    }
                    out.push(c);
                }
            }
            Value::Int(x) => out.push_str(&format!("I{x}")),
            Value::BigInt(x) => out.push_str(&format!("B{x}")),
            Value::Dec(x) => out.push_str(&format!("D{x}")),
            Value::Bool(x) => out.push_str(if *x { "b1" } else { "b0" }),
            Value::Date(x) => out.push_str(&format!("d{x}")),
            Value::Time(x) => out.push_str(&format!("t{x}")),
            Value::DateTime(x) => out.push_str(&format!("M{x}")),
            Value::Duration(x) => out.push_str(&format!("U{x}")),
            Value::Guid(g) => out.push_str(&format!("G{}", format_guid(g))),
            Value::Option(x) => out.push_str(&format!("O{x}")),
            Value::Blob(_) => out.push('T'),
        }
    }
    out
}

pub fn decode_record_id(s: &str) -> Option<(i64, Vec<Value>)> {
    if s.is_empty() {
        return None;
    }
    let (t, rest) = s.split_once(':')?;
    let table: i64 = t.parse().ok()?;
    let mut parts: Vec<String> = vec![];
    if !rest.is_empty() {
        let mut cur = String::new();
        let mut esc = false;
        for c in rest.chars() {
            match c {
                _ if esc => {
                    cur.push(c);
                    esc = false;
                }
                '\\' => esc = true,
                ',' => parts.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            }
        }
        parts.push(cur);
    }
    let mut key = Vec::with_capacity(parts.len());
    for p in parts {
        let mut ch = p.chars();
        let tag = ch.next()?;
        let body = ch.as_str();
        key.push(match tag {
            'T' => Value::Text(body.to_string()),
            'I' => Value::Int(body.parse().ok()?),
            'B' => Value::BigInt(body.parse().ok()?),
            'D' => Value::Dec(Decimal::from_str(body).ok()?),
            'b' => Value::Bool(body == "1"),
            'd' => Value::Date(body.parse().ok()?),
            't' => Value::Time(body.parse().ok()?),
            'M' => Value::DateTime(body.parse().ok()?),
            'U' => Value::Duration(body.parse().ok()?),
            'G' => Value::Guid(parse_guid(body)?),
            'O' => Value::Option(body.parse().ok()?),
            _ => return None,
        });
    }
    Some((table, key))
}

/// Convert a runtime value to the stored representation for field `f`.
pub fn to_store(f: &FieldDef, v: &Val) -> Result<Value, String> {
    let max = |s: String, n: u32| -> Result<String, String> {
        if s.chars().count() > n as usize {
            Err(format!(
                "The length of the string is {}, but it must be less than or equal to {} characters. Value: {}",
                s.chars().count(),
                n,
                s
            ))
        } else {
            Ok(s)
        }
    };
    Ok(match &f.ty {
        FieldType::Integer => Value::Int(v.as_i64() as i32),
        FieldType::BigInteger => Value::BigInt(v.as_i64()),
        FieldType::Decimal => Value::Dec(v.as_dec()),
        FieldType::Boolean => Value::Bool(v.as_bool()),
        FieldType::Code(n) => Value::Text(max(v.to_text().trim().to_uppercase(), *n)?),
        FieldType::Text(n) => Value::Text(max(v.to_text(), *n)?),
        FieldType::Date => match v {
            Val::Date(d) => Value::Date(*d),
            Val::Text(s) if s.is_empty() => Value::Date(0),
            _ => Value::Date(0),
        },
        FieldType::Time => match v {
            Val::Time(t) => Value::Time(*t),
            _ => Value::Time(0),
        },
        FieldType::DateTime => match v {
            Val::DateTime(t) => Value::DateTime(*t),
            _ => Value::DateTime(bc_store::DATETIME_UNDEFINED),
        },
        FieldType::Duration => Value::Duration(v.as_i64()),
        FieldType::Guid | FieldType::Media | FieldType::MediaSet => match v {
            Val::Guid(g) => Value::Guid(*g),
            Val::Text(s) => Value::Guid(parse_guid(s).unwrap_or([0; 16])),
            _ => Value::Guid([0; 16]),
        },
        FieldType::Option | FieldType::Enum(_) => Value::Option(v.as_i64() as i32),
        FieldType::Blob => Value::Blob(match v {
            Val::Blob(b) => b.clone(),
            _ => vec![],
        }),
        FieldType::DateFormula => Value::Text(v.to_text()),
        FieldType::RecordId => Value::Text(match v {
            Val::RecordId(t, k) => encode_record_id(*t, k),
            Val::Text(s) if decode_record_id(s).is_some() => s.clone(),
            _ => String::new(),
        }),
        _ => Value::Text(v.to_text()),
    })
}

/// Parse a filter/Evaluate literal for field `f`.
pub fn text_to_store(f: &FieldDef, s: &str, enum_values: &dyn Fn(&str) -> Option<i32>, work_date: i32) -> Option<Value> {
    let st = s.trim();
    Some(match &f.ty {
        FieldType::Code(_) => Value::Text(st.to_uppercase()),
        FieldType::Text(_) | FieldType::Other(_) | FieldType::TableFilter => Value::Text(s.to_string()),
        // Q13: a field value from code, InitValue (R8), a filter (R80), an import or an API is
        // read with the invariant letters, whatever the session language; Evaluate and TestPage
        // input read user text in the session language (Session::evaluate_date_formula)
        FieldType::DateFormula => Value::Text(crate::datefml::parse_invariant(s).ok()?.stored()),
        FieldType::RecordId => Value::Text(if decode_record_id(st).is_some() { st.to_string() } else { String::new() }),
        FieldType::Integer => Value::Int(parse_num(st)?.to_i32()?),
        FieldType::BigInteger => Value::BigInt(parse_num(st)?.to_i64()?),
        FieldType::Decimal => Value::Dec(parse_num(st)?),
        FieldType::Duration => Value::Duration(parse_num(st)?.to_i64()?),
        FieldType::Boolean => Value::Bool(match st.to_ascii_lowercase().as_str() {
            "yes" | "true" | "1" => true,
            "no" | "false" | "0" | "" => false,
            _ => return None,
        }),
        FieldType::Option => {
            if let Ok(n) = st.parse::<i32>() {
                Value::Option(n)
            } else {
                let i = f.options.iter().position(|o| text_eq(o.trim(), st)).or_else(|| f.option_captions.iter().position(|o| text_eq(o.trim(), st)))?;
                Value::Option(i as i32)
            }
        }
        FieldType::Enum(_) => {
            if let Ok(n) = st.parse::<i32>() {
                Value::Option(n)
            } else {
                // '' is the blank value, declared as value(0; " ")
                Value::Option(enum_values(st).or_else(|| if st.is_empty() { enum_values(" ") } else { None })?)
            }
        }
        FieldType::Date => Value::Date(parse_date_text(st, work_date)?),
        FieldType::Guid | FieldType::Media | FieldType::MediaSet => Value::Guid(if st.is_empty() { [0; 16] } else { parse_guid(st)? }),
        FieldType::Time => Value::Time(parse_time_text(st)?),
        FieldType::DateTime => Value::DateTime(if st.is_empty() { bc_store::DATETIME_UNDEFINED } else { parse_datetime_text(st)? }),
        FieldType::Blob => return None,
    })
}

pub fn parse_num(s: &str) -> Option<Decimal> {
    if s.is_empty() {
        return Some(Decimal::ZERO);
    }
    let t: String = s.chars().filter(|c| *c != ',' && *c != ' ').collect();
    Decimal::from_str(&t).ok().or_else(|| Decimal::from_scientific(&t).ok())
}

/// Dates as typed in BC: T (today), W (work date), MMDDYY, MMDDYYYY, MM/DD/YY, YYYY-MM-DD, C-prefix closing.
pub fn parse_date_text(s: &str, work_date: i32) -> Option<i32> {
    let s = s.trim();
    if s.is_empty() || s == "0D" {
        return Some(0);
    }
    let (closing, s) = match s.strip_prefix(['C', 'c']) {
        Some(r) if !r.is_empty() && r.chars().next().unwrap().is_ascii_digit() => (true, r),
        _ => (false, s),
    };
    let base = match s.to_ascii_lowercase().as_str() {
        "t" | "today" => Some(today()),
        "w" | "workdate" => Some(work_date),
        _ => None,
    };
    let d = if let Some(b) = base {
        b
    } else if let Some((y, rest)) = s.split_once('-').filter(|(y, _)| y.len() == 4) {
        let (m, d) = rest.split_once('-')?;
        date_from_ymd(y.parse().ok()?, m.parse().ok()?, d.get(..2).unwrap_or(d).parse().ok()?)?
    } else if s.contains('/') || s.contains('.') {
        let parts: Vec<&str> = s.split(['/', '.']).collect();
        if parts.len() != 3 {
            return None;
        }
        let y: i32 = parts[2].parse().ok()?;
        let y = if parts[2].len() <= 2 { 2000 + y } else { y };
        date_from_ymd(y, parts[0].parse().ok()?, parts[1].parse().ok()?)?
    } else if s.chars().all(|c| c.is_ascii_digit()) && (s.len() == 6 || s.len() == 8) {
        let m = s[0..2].parse().ok()?;
        let d = s[2..4].parse().ok()?;
        let y: i32 = s[4..].parse().ok()?;
        date_from_ymd(if s.len() == 6 { 2000 + y } else { y }, m, d)?
    } else {
        return None;
    };
    Some(if closing { d | 1 } else { d })
}

pub fn parse_time_text(s: &str) -> Option<i32> {
    if s.is_empty() {
        return Some(0);
    }
    let up = s.to_ascii_uppercase();
    let pm = up.contains("PM");
    let am = up.contains("AM");
    let core: String = up.replace("AM", "").replace("PM", "").trim().to_string();
    let parts: Vec<&str> = core.split(':').collect();
    let mut h: i32 = parts.first()?.trim().parse().ok()?;
    let m: i32 = parts.get(1).map(|x| x.trim().parse().unwrap_or(0)).unwrap_or(0);
    let sec: f64 = parts.get(2).map(|x| x.trim().parse().unwrap_or(0.0)).unwrap_or(0.0);
    if pm && h < 12 {
        h += 12;
    }
    if am && h == 12 {
        h = 0;
    }
    Some(h * 3_600_000 + m * 60_000 + (sec * 1000.0) as i32 + 1)
}

pub fn parse_datetime_text(s: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    let (d, t) = s.split_once(' ').unwrap_or((s, ""));
    let dd = parse_date_text(d, today())?;
    let tt = if t.is_empty() { 1 } else { parse_time_text(t)? };
    Some(date_time_to_ms(dd, tt))
}

pub fn date_time_to_ms(d: i32, t: i32) -> i64 {
    match date_to_naive(d) {
        None => bc_store::DATETIME_UNDEFINED,
        Some(n) => {
            let ms = if t > 0 { (t - 1) as i64 } else { 0 };
            n.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis() + ms
        }
    }
}
