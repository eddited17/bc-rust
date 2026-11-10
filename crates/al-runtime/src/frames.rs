//! Cheaper frame setup (P12): the parts of `Session::invoke`'s frame construction and of
//! `default_val`/`coerce` that recomputed the same thing on every call.
//!
//! - [`Lower`]: a type name lower-cased on the stack instead of in a new `String` (type names
//!   are short; longer names still allocate).
//! - [`table_of`]: the table id of a `Record "X"` type, resolved once per type node (the
//!   catalog's types live for the whole run) instead of by name on every default value.
//! - [`initial_row`]: a new record's initial row (blank values plus the fields' InitValue) built
//!   once per (catalog, table, work date) and cloned afterwards. `init_row` depends on nothing
//!   else: the field definitions, the catalog's enums and the work date.

use crate::interp::Session;
use al_model::Catalog;
use al_syntax::ObjectKind;
use al_syntax::TypeRef;
use std::cell::RefCell;
use std::collections::HashMap;

/// An ASCII-lower-cased copy of a short name, without allocating.
pub enum Lower {
    Buf([u8; 32], u8),
    Owned(String),
}

impl Lower {
    #[inline]
    pub fn new(s: &str) -> Lower {
        let n = s.len();
        if n <= 32 {
            let mut b = [0u8; 32];
            b[..n].copy_from_slice(s.as_bytes());
            b[..n].make_ascii_lowercase();
            Lower::Buf(b, n as u8)
        } else {
            Lower::Owned(s.to_ascii_lowercase())
        }
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            // ASCII lower-casing of valid UTF-8 is valid UTF-8
            Lower::Buf(b, n) => std::str::from_utf8(&b[..*n as usize]).unwrap_or(""),
            Lower::Owned(s) => s,
        }
    }
}

#[derive(Default)]
struct AddrHasher(u64);
impl std::hash::Hasher for AddrHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    fn write_i64(&mut self, n: i64) {
        self.write_u64(n as u64)
    }
    fn write_i32(&mut self, n: i32) {
        self.write_u64(n as u64)
    }
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64)
    }
}
type Fast<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<AddrHasher>>;

thread_local! {
    static TABLES: RefCell<Fast<(usize, usize), Option<i64>>> = RefCell::new(Fast::default());
    static ROWS: RefCell<Fast<(usize, i64, i32), crate::record::CowRow>> = RefCell::new(Fast::default());
}

/// `cat.id_of(ObjectKind::Table, target)` for the Record type node `ty`, cached per node.
/// `ty` is `&'static` (catalog or leaked), so its address identifies it for the whole run.
#[inline]
pub fn table_of(cat: &'static Catalog, ty: &'static TypeRef, target: &str) -> Option<i64> {
    let key = (cat as *const Catalog as usize, ty as *const TypeRef as usize);
    if let Some(r) = TABLES.with(|m| m.borrow().get(&key).copied()) {
        return r;
    }
    let r = cat.id_of(ObjectKind::Table, target);
    TABLES.with(|m| m.borrow_mut().insert(key, r));
    r
}

/// The initial row of a new record of `table`: `schema.new_row()` followed by `init_row`.
/// Shared: a new record keeps it until its first access (crate::record::RecCell).
pub fn initial_row(s: &Session<'_>, table: i64, tdef: &'static al_model::TableDef, schema: &bc_store::TableSchema) -> crate::record::CowRow {
    let key = (s.cat as *const Catalog as usize, table, s.work_date);
    if let Some(r) = ROWS.with(|m| m.borrow().get(&key).cloned()) {
        return r;
    }
    let mut row = schema.new_row();
    s.init_row(tdef, &mut row);
    let row = crate::record::CowRow::from(row);
    ROWS.with(|m| m.borrow_mut().insert(key, row.clone()));
    row
}

// ---------------------------------------------------------------- shared label slots (round 18)

/// A process-wide session id (Session::id).
pub fn next_session_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

thread_local! {
    // (session id, declaration address) -> the label's slot
    static LABELS: RefCell<HashMap<(u64, usize), crate::value::Slot>> = RefCell::new(HashMap::new());
}

/// The slot of a Label / TextConst declaration (`ty`, `&'static`, so its address identifies it) in
/// session `session`: labels are constants, so the session's instances share it instead of each
/// copying the text.
pub fn label_slot(session: u64, ty: &'static TypeRef, text: &str) -> crate::value::Slot {
    let key = (session, ty as *const TypeRef as usize);
    if let Some(s) = LABELS.with(|m| m.borrow().get(&key).cloned()) {
        return s;
    }
    let s = crate::value::slot(crate::value::Val::Text(text.to_string()));
    LABELS.with(|m| m.borrow_mut().insert(key, s.clone()));
    s
}

/// A session ends: its label slots are not used again.
pub fn forget_labels(session: u64) {
    LABELS.with(|m| m.borrow_mut().retain(|k, _| k.0 != session));
}

// ---------------------------------------------------------------- event publishers (round 18)

thread_local! {
    static EVENTS: RefCell<Fast<(usize, u32, u32), bool>> = RefCell::new(Fast::default());
}

/// Whether the catalog has subscribers for the event that procedure `pr` publishes (static for a
/// catalog; manual binding only filters subscribers at dispatch).
pub fn event_has_subscribers(cat: &'static Catalog, pr: al_model::ProcRef, kind: ObjectKind, id: i64, event: &str) -> bool {
    let key = (cat as *const Catalog as usize, pr.code, pr.proc);
    if let Some(b) = EVENTS.with(|m| m.borrow().get(&key).copied()) {
        return b;
    }
    let b = cat.subscribers.contains_key(&(kind, id, event.to_ascii_lowercase(), String::new()));
    EVENTS.with(|m| m.borrow_mut().insert(key, b));
    b
}

// ---------------------------------------------------------------- Simple type kinds (round 18)

/// The kind of a `TypeRef::Simple` (its lower-cased name), for the fast paths of default_val / coerce.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SimpleKind {
    Int,
    BigInt,
    Dec,
    Bool,
    Code,
    Text,
    BigText,
    SecretText,
    TextBuilder,
    Char,
    Date,
    Time,
    DateTime,
    Duration,
    Guid,
    DateFormula,
    RecordId,
    Variant,
    Other,
}

thread_local! {
    static KINDS: RefCell<Fast<usize, SimpleKind>> = RefCell::new(Fast::default());
}

/// The kind of the Simple type `ty` named `name`, cached per type node.
#[inline]
pub fn simple_kind(ty: &'static TypeRef, name: &str) -> SimpleKind {
    let key = ty as *const TypeRef as usize;
    if let Some(k) = KINDS.with(|m| m.borrow().get(&key).copied()) {
        return k;
    }
    let k = match Lower::new(name).as_str() {
        "integer" | "byte" => SimpleKind::Int,
        "biginteger" => SimpleKind::BigInt,
        "decimal" => SimpleKind::Dec,
        "boolean" => SimpleKind::Bool,
        "code" => SimpleKind::Code,
        "text" => SimpleKind::Text,
        "bigtext" => SimpleKind::BigText,
        "secrettext" => SimpleKind::SecretText,
        "textbuilder" => SimpleKind::TextBuilder,
        "char" => SimpleKind::Char,
        "date" => SimpleKind::Date,
        "time" => SimpleKind::Time,
        "datetime" => SimpleKind::DateTime,
        "duration" => SimpleKind::Duration,
        "guid" => SimpleKind::Guid,
        "dateformula" => SimpleKind::DateFormula,
        "recordid" => SimpleKind::RecordId,
        "variant" => SimpleKind::Variant,
        _ => SimpleKind::Other,
    };
    KINDS.with(|m| m.borrow_mut().insert(key, k));
    k
}

/// default_val of the plain Simple kinds (the same values as its string match); None: the others.
#[inline]
pub fn simple_default(ty: &'static TypeRef, name: &str) -> Option<crate::value::Val> {
    use crate::value::Val;
    Some(match simple_kind(ty, name) {
        SimpleKind::Int => Val::Int(0),
        SimpleKind::BigInt => Val::BigInt(0),
        SimpleKind::Dec => Val::Dec(rust_decimal::Decimal::ZERO),
        SimpleKind::Bool => Val::Bool(false),
        SimpleKind::Code | SimpleKind::Text | SimpleKind::BigText | SimpleKind::SecretText | SimpleKind::TextBuilder => Val::Text(String::new()),
        SimpleKind::Char => Val::Char('\0'),
        SimpleKind::Date => Val::Date(0),
        SimpleKind::Time => Val::Time(0),
        SimpleKind::DateTime => Val::DateTime(bc_store::DATETIME_UNDEFINED),
        SimpleKind::Duration => Val::Duration(0),
        SimpleKind::Guid => Val::Guid([0; 16]),
        SimpleKind::DateFormula => Val::DateFormula(String::new()),
        SimpleKind::RecordId => Val::RecordId(0, vec![]),
        SimpleKind::Variant => Val::Void,
        SimpleKind::Other => return None,
    })
}
