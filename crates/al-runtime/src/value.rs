//! Runtime values of the AL interpreter.

use crate::record::RecHandle;
use bc_store::Value;
use rust_decimal::prelude::*;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

pub type Slot = Rc<RefCell<Val>>;

pub fn slot(v: Val) -> Slot {
    Rc::new(RefCell::new(v))
}

#[derive(Clone)]
pub enum Val {
    Void,
    Bool(bool),
    Int(i32),
    BigInt(i64),
    Dec(Decimal),
    Text(String),
    Char(char),
    Date(i32),
    Time(i32),
    DateTime(i64),
    Duration(i64),
    Guid([u8; 16]),
    /// Option or enum value. `enum_id` is set for enums.
    Opt { ord: i32, enum_id: Option<i64> },
    DateFormula(String),
    Record(RecHandle),
    Object(Rc<crate::interp::Instance>),
    /// Interface variable (None = not assigned)
    Iface(Option<Rc<crate::interp::Instance>>),
    List(Rc<RefCell<Vec<Val>>>),
    Dict(Rc<RefCell<Vec<(Val, Val)>>>),
    Array(Rc<RefCell<Vec<Val>>>, Vec<usize>),
    RecordId(i64, Vec<Value>),
    FieldRef(RecHandle, usize),
    /// KeyRef: a record and the index of one of its table's keys (catalog order).
    KeyRef(RecHandle, usize),
    /// TestPage variable and a control on it (TestField / TestAction / TestFilter).
    TestPage(crate::testpage::TpRef),
    TestCtl(crate::testpage::TpRef, Rc<str>),
    /// Content of a Blob field.
    Blob(Vec<u8>),
    /// JsonObject / JsonArray / JsonValue / JsonToken (reference to a JSON node).
    Json(crate::json::JRef),
    InStream(Rc<RefCell<crate::streams::InStreamState>>),
    OutStream(Rc<crate::streams::OutTarget>),
    /// Value of an unsupported platform type (HttpClient, JsonObject, ...).
    Opaque(&'static str),
    /// DotNet interop object, AL Xml* or Http* value (reference semantics, see crate::dotnet).
    Net(crate::dotnet::NetRef),
    /// The ObjectType::X / Database::X style literals evaluate to ids; this keeps
    /// the name for error messages.
    ErrorInfo(crate::errinfo::ErrRef),
    /// Notification / SessionSettings: platform value types with state (crate::platobj).
    Plat(crate::platobj::PlatRef),
}

impl std::fmt::Debug for Val {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_text())
    }
}

pub const DATE_UNDEFINED: i32 = 0;

// ---------------------------------------------------------------- dates

pub fn date_from_ymd(y: i32, m: u32, d: u32) -> Option<i32> {
    let nd = chrono::NaiveDate::from_ymd_opt(y, m, d)?;
    Some(chrono::Datelike::num_days_from_ce(&nd) * 2)
}

pub fn date_to_naive(d: i32) -> Option<chrono::NaiveDate> {
    if d <= 0 {
        return None;
    }
    chrono::NaiveDate::from_num_days_from_ce_opt(d / 2)
}

pub fn date_is_closing(d: i32) -> bool {
    d > 0 && d % 2 == 1
}

pub fn today() -> i32 {
    let n = crate::clock::local(crate::clock::peek_ms()).date();
    chrono::Datelike::num_days_from_ce(&n) * 2
}

impl Val {
    pub fn text(s: impl Into<String>) -> Val {
        Val::Text(s.into())
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Val::Void => "Void",
            Val::Bool(_) => "Boolean",
            Val::Int(_) => "Integer",
            Val::BigInt(_) => "BigInteger",
            Val::Dec(_) => "Decimal",
            Val::Text(_) => "Text",
            Val::Char(_) => "Char",
            Val::Date(_) => "Date",
            Val::Time(_) => "Time",
            Val::DateTime(_) => "DateTime",
            Val::Duration(_) => "Duration",
            Val::Guid(_) => "Guid",
            Val::Opt { enum_id: Some(e), .. } if *e >= -1 => "Enum",
            Val::Opt { .. } => "Option",
            Val::DateFormula(_) => "DateFormula",
            Val::Record(_) => "Record",
            Val::Object(_) => "Object",
            Val::Iface(_) => "Interface",
            Val::List(_) => "List",
            Val::Dict(_) => "Dictionary",
            Val::Array(..) => "Array",
            Val::RecordId(..) => "RecordId",
            Val::FieldRef(..) => "FieldRef",
            Val::KeyRef(..) => "KeyRef",
            Val::TestPage(_) => "TestPage",
            Val::TestCtl(..) => "TestField",
            Val::Blob(_) => "Blob",
            Val::Json(j) => match &*j.borrow() {
                crate::json::J::Obj(_) => "JsonObject",
                crate::json::J::Arr(_) => "JsonArray",
                _ => "JsonValue",
            },
            Val::InStream(_) => "InStream",
            Val::Plat(p) => crate::platobj::type_name(&p.borrow()),
            Val::OutStream(_) => "OutStream",
            // ModuleInfo#3 / Version#3 carry the app (modules.rs)
            Val::Opaque(n) => n.split('#').next().unwrap_or(n),
            Val::Net(n) => crate::dotnet::type_name(n),
            Val::ErrorInfo(_) => "ErrorInfo",
        }
    }

    pub fn as_bool(&self) -> bool {
        match self {
            Val::Bool(b) => *b,
            Val::Int(i) => *i != 0,
            Val::Opt { ord, .. } => *ord != 0,
            Val::Net(n) => crate::dotnet::unbox(n).is_some_and(|v| v.as_bool()),
            _ => false,
        }
    }

    pub fn as_i64(&self) -> i64 {
        match self {
            Val::Int(i) => *i as i64,
            Val::BigInt(i) => *i,
            Val::Dec(d) => d.trunc().to_i64().unwrap_or(0),
            Val::Opt { ord, .. } => *ord as i64,
            Val::Bool(b) => *b as i64,
            Val::Char(c) => *c as i64,
            Val::Duration(d) => *d,
            Val::Text(s) => s.trim().parse().unwrap_or(0),
            Val::Net(n) => crate::dotnet::unbox(n).map(|v| v.as_i64()).unwrap_or(0),
            _ => 0,
        }
    }

    pub fn as_dec(&self) -> Decimal {
        match self {
            Val::Dec(d) => *d,
            Val::Int(i) => Decimal::from(*i),
            Val::BigInt(i) => Decimal::from(*i),
            Val::Opt { ord, .. } => Decimal::from(*ord),
            Val::Char(c) => Decimal::from(*c as u32),
            Val::Duration(d) => Decimal::from(*d),
            Val::Text(s) => Decimal::from_str(s.trim()).unwrap_or_default(),
            Val::Net(n) => crate::dotnet::unbox(n).map(|v| v.as_dec()).unwrap_or_default(),
            _ => Decimal::ZERO,
        }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Val::Int(_) | Val::BigInt(_) | Val::Dec(_) | Val::Char(_))
    }
    pub fn is_texty(&self) -> bool {
        matches!(self, Val::Text(_) | Val::Char(_))
    }

    /// Plain text conversion (Format without format specifiers handled by builtins).
    pub fn to_text(&self) -> String {
        match self {
            Val::Void => String::new(),
            Val::Bool(b) => if *b { "Yes" } else { "No" }.into(),
            Val::Int(i) => i.to_string(),
            Val::BigInt(i) => i.to_string(),
            Val::Dec(d) => format_decimal(*d, true),
            Val::Text(s) => s.clone(),
            Val::Char(c) => c.to_string(),
            Val::Date(d) => format_date(*d),
            Val::Time(t) => format_time(*t),
            Val::DateTime(t) => format_datetime(*t),
            Val::Duration(d) => format!("{d} ms"),
            Val::Guid(g) => format_guid(g),
            Val::Opt { ord, .. } => ord.to_string(),
            Val::DateFormula(f) => f.clone(),
            Val::Record(r) => r.borrow().table_name().to_string(),
            // Format(RecordId) = "Customer: 10000"; a blank RecordId formats as ''
            Val::RecordId(0, _) => String::new(),
            Val::RecordId(t, k) => {
                let name = crate::record::CATALOG.get().and_then(|c| c.tables.get(t)).map(|d| d.name.clone()).unwrap_or_else(|| t.to_string());
                format!("{name}: {}", k.iter().map(|v| store_to_val(v).to_text()).collect::<Vec<_>>().join(","))
            }
            Val::ErrorInfo(m) => m.borrow().message.clone(),
            Val::Json(j) => crate::json::to_string(j),
            Val::Net(n) => crate::dotnet::to_text(n),
            Val::Opaque(t) if t.starts_with("Version") => crate::modules::version_text(t),
            // a FieldRef converts to its value
            Val::FieldRef(h, fi) if crate::record::stale_fieldref(h, *fi).is_some() => String::new(),
            Val::FieldRef(h, fi) => {
                let r = h.borrow();
                let fd = r.field(*fi);
                let v = r.get_val(*fi);
                match (&fd.ty, &v) {
                    (al_model::FieldType::Option, Val::Opt { ord, .. }) => fd.option_captions.get(*ord as usize).cloned().unwrap_or_else(|| ord.to_string()),
                    _ => crate::builtins::format_val(&v, 0),
                }
            }
            other => other.type_name().to_string(),
        }
    }

    pub fn deep_copy(&self) -> Val {
        match self {
            Val::Array(a, dims) => Val::Array(Rc::new(RefCell::new(a.borrow().iter().map(|v| v.deep_copy()).collect())), dims.clone()),
            // RecordRefs are references; Records are values
            Val::Record(r) if r.borrow().is_ref => Val::Record(r.clone()),
            Val::Record(r) => Val::Record(crate::record::copy_record(r)),
            // Notification / SessionSettings are values too
            Val::Plat(p) => crate::platobj::copy(p),
            Val::ErrorInfo(e) => crate::errinfo::copy(e),
            other => other.clone(),
        }
    }
}

pub fn format_guid(g: &[u8; 16]) -> String {
    let h: String = g.iter().map(|b| format!("{b:02X}")).collect();
    format!("{{{}-{}-{}-{}-{}}}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

pub fn parse_guid(s: &str) -> Option<[u8; 16]> {
    let h: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if h.len() != 32 {
        return None;
    }
    let mut g = [0u8; 16];
    for i in 0..16 {
        g[i] = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(g)
}

pub fn format_decimal(d: Decimal, thousands: bool) -> String {
    let d = d.normalize();
    let s = d.abs().to_string();
    let (int, frac) = match s.find('.') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s.as_str(), ""),
    };
    let mut out = String::new();
    if d.is_sign_negative() && !d.is_zero() {
        out.push('-');
    }
    if thousands {
        let b = int.as_bytes();
        for (i, c) in b.iter().enumerate() {
            if i > 0 && (b.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(*c as char);
        }
    } else {
        out.push_str(int);
    }
    out.push_str(frac);
    out
}

pub fn format_date(d: i32) -> String {
    match date_to_naive(d) {
        None => String::new(),
        Some(n) => {
            let s = n.format("%m/%d/%y").to_string();
            if date_is_closing(d) {
                format!("C{s}")
            } else {
                s
            }
        }
    }
}

pub fn format_time(t: i32) -> String {
    if t <= 0 {
        return String::new();
    }
    let ms = t - 1;
    let (h, m, s) = (ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60);
    let (h12, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{h12}:{m:02}:{s:02} {ampm}")
}

pub fn format_datetime(t: i64) -> String {
    if t == bc_store::DATETIME_UNDEFINED {
        return String::new();
    }
    match chrono::DateTime::from_timestamp_millis(t) {
        Some(dt) => dt.naive_utc().format("%m/%d/%y %-I:%M %p").to_string(),
        None => String::new(),
    }
}

/// DateTime standard formats 1 (with `<Second dec.>`) and 3: the date and the time with seconds.
pub fn format_datetime_seconds(t: i64, second_dec: bool) -> String {
    if t == bc_store::DATETIME_UNDEFINED {
        return String::new();
    }
    match chrono::DateTime::from_timestamp_millis(t) {
        Some(dt) => {
            let n = dt.naive_utc();
            let ms = dt.timestamp_subsec_millis();
            // <Second dec.>: '.' and the milliseconds without trailing zeros; nothing for 0
            let dec = if second_dec && ms != 0 { format!(".{ms:03}").trim_end_matches('0').to_string() } else { String::new() };
            format!("{}{dec} {}", n.format("%m/%d/%y %-I:%M:%S"), n.format("%p"))
        }
        None => String::new(),
    }
}

/// Store value → runtime value (enum/option ids are restored by the record layer).
pub fn store_to_val(v: &Value) -> Val {
    match v {
        Value::Int(i) => Val::Int(*i),
        Value::BigInt(i) => Val::BigInt(*i),
        Value::Dec(d) => Val::Dec(*d),
        Value::Bool(b) => Val::Bool(*b),
        Value::Text(s) => Val::Text(s.clone()),
        Value::Date(d) => Val::Date(*d),
        Value::Time(t) => Val::Time(*t),
        Value::DateTime(t) => Val::DateTime(*t),
        Value::Duration(d) => Val::Duration(*d),
        Value::Guid(g) => Val::Guid(*g),
        Value::Option(o) => Val::Opt { ord: *o, enum_id: None },
        Value::Blob(b) => Val::Blob(b.clone()),
    }
}

/// AL comparison semantics (text compares case-sensitively for Text, codes are
/// already upper-cased).
pub fn compare(a: &Val, b: &Val) -> Ordering {
    use Val::*;
    match (a, b) {
        (Text(x), Text(y)) => x.cmp(y),
        (Text(x), Char(y)) => x.as_str().cmp(y.to_string().as_str()),
        (Char(x), Text(y)) => x.to_string().as_str().cmp(y.as_str()),
        (Bool(x), Bool(y)) => x.cmp(y),
        (Date(x), Date(y)) | (Time(x), Time(y)) => x.cmp(y),
        (DateTime(x), DateTime(y)) => x.cmp(y),
        (Guid(x), Guid(y)) => bc_store::guid_sort_key(x).cmp(&bc_store::guid_sort_key(y)),
        (Opt { ord: x, .. }, Opt { ord: y, .. }) => x.cmp(y),
        (Opt { ord: x, .. }, Int(y)) | (Int(x), Opt { ord: y, .. }) => {
            if matches!(a, Opt { .. }) {
                x.cmp(y)
            } else {
                y.cmp(x).reverse()
            }
        }
        (Int(x), Int(y)) => x.cmp(y),
        // the order of DateFormula values is OPEN:P14 (R82): equal stored texts are equal; others
        // are kept apart by their stored text, recorded as an assumption (crate::datefml::order).
        // Never reads the values again (Q15).
        (DateFormula(x), DateFormula(y)) => match crate::datefml::order(x, y) {
            Ok(o) => o,
            Err(_) => {
                crate::datefml::note_assumed(crate::datefml::Probe::P14);
                x.cmp(y)
            }
        },
        _ if a.is_numeric() && b.is_numeric() => a.as_dec().cmp(&b.as_dec()),
        (Duration(_), _) | (_, Duration(_)) => a.as_dec().cmp(&b.as_dec()),
        (Text(x), _) => x.as_str().cmp(b.to_text().as_str()),
        (_, Text(y)) => a.to_text().as_str().cmp(y.as_str()),
        // Version values (crate::modules): component by component
        (Opaque(x), Opaque(y)) if x.starts_with("Version") && y.starts_with("Version") => crate::modules::version_parts(x).cmp(&crate::modules::version_parts(y)),
        (RecordId(t1, k1), RecordId(t2, k2)) => t1.cmp(t2).then_with(|| {
            for (x, y) in k1.iter().zip(k2.iter()) {
                let o = x.cmp_sql(y);
                if o != Ordering::Equal {
                    return o;
                }
            }
            k1.len().cmp(&k2.len())
        }),
        _ => Ordering::Equal,
    }
}

pub fn equals(a: &Val, b: &Val) -> bool {
    match (a, b) {
        (Val::Object(x), Val::Object(y)) => Rc::ptr_eq(x, y),
        (Val::Net(x), Val::Net(y)) => crate::dotnet::net_equals(x, y),
        // R5, Q6, T4:P14eq: equality of the stored text
        (Val::DateFormula(x), Val::DateFormula(y)) => x == y,
        (Val::Iface(x), Val::Iface(y)) => match (x, y) {
            (Some(x), Some(y)) => Rc::ptr_eq(x, y),
            (None, None) => true,
            _ => false,
        },
        _ => compare(a, b) == Ordering::Equal,
    }
}
