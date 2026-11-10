//! Option metadata carried by Option values.
//!
//! An Option variable, parameter or return value is declared with its members
//! (`Opt: Option Normal,Item,"G/L Account"`), and BC formats the value with them:
//! `Format(Opt)` is the member caption, `Format(Opt, 0, 2)` and `Format(Opt, 0, 9)` the ordinal
//! [src:A17.1], and `Evaluate(Opt, Text)` takes a caption, a member name or a number [src:A17.2].
//!
//! bc-rust keeps `Val::Opt { ord, enum_id }` as is and encodes the metadata in `enum_id`:
//! `Some(id)` with `id >= 0` is an enum, `-1` an enum that is not in the catalog, and
//! `id <= -2` refers to a member list interned here. `None` is an Option without metadata.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

pub struct OptMeta {
    pub names: Vec<String>,
    pub captions: Vec<String>,
}

impl OptMeta {
    /// The caption (or name) of ordinal `ord`; None if it has none.
    pub fn caption(&self, ord: i32) -> Option<&str> {
        let i = usize::try_from(ord).ok()?;
        self.captions.get(i).or_else(|| self.names.get(i)).map(|s| s.as_str()).filter(|s| !s.trim().is_empty())
    }

    /// Evaluate: a caption, then a member name, then a number.
    pub fn ordinal_of(&self, text: &str) -> Option<i32> {
        let t = text.trim();
        let find = |l: &[String]| l.iter().position(|m| m.trim().eq_ignore_ascii_case(t)).map(|i| i as i32);
        find(&self.captions).or_else(|| find(&self.names)).or_else(|| t.parse::<i32>().ok())
    }
}

/// Interned metadata, shared by all threads (ids are process-wide).
static LIST: RwLock<Vec<Arc<OptMeta>>> = RwLock::new(Vec::new());
static BY_CONTENT: Mutex<Option<HashMap<String, i64>>> = Mutex::new(None);

thread_local! {
    /// (address, len) of a 'static member list → id, so the hot path takes no lock.
    static BY_PTR: RefCell<HashMap<(usize, usize, usize), i64>> = RefCell::new(HashMap::new());
}

fn id_of_index(i: usize) -> i64 {
    -2 - i as i64
}

/// True for an `enum_id` that refers to Option metadata.
pub fn is_meta(enum_id: i64) -> bool {
    enum_id <= -2
}

pub fn get(enum_id: i64) -> Option<Arc<OptMeta>> {
    if !is_meta(enum_id) {
        return None;
    }
    LIST.read().ok()?.get((-2 - enum_id) as usize).cloned()
}

fn intern(names: &[String], captions: &[String]) -> i64 {
    let key = format!("{}\u{1}{}", names.join("\u{0}"), captions.join("\u{0}"));
    let mut m = BY_CONTENT.lock().unwrap();
    let m = m.get_or_insert_with(HashMap::new);
    if let Some(&id) = m.get(&key) {
        return id;
    }
    let mut l = LIST.write().unwrap();
    l.push(Arc::new(OptMeta { names: names.to_vec(), captions: captions.to_vec() }));
    let id = id_of_index(l.len() - 1);
    m.insert(key, id);
    id
}

/// The metadata id of a declared Option type (`TypeRef::Option(members)`, 'static catalog data).
pub fn for_members(members: &'static [String]) -> i64 {
    for_lists(members, &[])
}

/// The metadata id of a platform option type whose values format as their member names
/// (`Format(FieldRef.Type)` is "Date"); None for the others.
pub fn for_system(ty: &str) -> Option<i64> {
    static FIELDTYPE: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    match ty {
        // the order of interp::system_option_ord("fieldtype", ..)
        "fieldtype" => Some(for_members(FIELDTYPE.get_or_init(|| {
            [
                "Integer", "Decimal", "Text", "Code", "Boolean", "Date", "Time", "DateTime", "Option", "GUID", "BigInteger", "BLOB", "Media", "MediaSet",
                "DateFormula", "Duration", "RecordID", "TableFilter",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        }))),
        _ => None,
    }
}

/// The metadata id of an Option field (`OptionMembers` / `OptionCaption`).
pub fn for_field(fd: &'static al_model::FieldDef) -> i64 {
    for_lists(&fd.options, &fd.option_captions)
}

/// The metadata id of an Option field reached through a plain reference (record values, API
/// input): FieldDefs only come from the catalog and live for the whole run, so the addresses of
/// their lists identify them, as in `for_lists`.
pub fn for_field_ref(fd: &al_model::FieldDef) -> i64 {
    let k = (fd.options.as_ptr() as usize, fd.options.len(), fd.option_captions.as_ptr() as usize);
    if let Some(id) = BY_PTR.with(|c| c.borrow().get(&k).copied()) {
        return id;
    }
    let id = intern(&fd.options, &fd.option_captions);
    BY_PTR.with(|c| c.borrow_mut().insert(k, id));
    id
}

fn for_lists(names: &'static [String], captions: &'static [String]) -> i64 {
    let k = (names.as_ptr() as usize, names.len(), captions.as_ptr() as usize);
    if let Some(id) = BY_PTR.with(|c| c.borrow().get(&k).copied()) {
        return id;
    }
    let id = intern(names, captions);
    BY_PTR.with(|c| c.borrow_mut().insert(k, id));
    id
}

/// Format an Option value with metadata: standard formats 2 and 9 are the ordinal, the others the
/// caption (the ordinal when the member has no caption) [src:A17.1].
pub fn format(enum_id: i64, ord: i32, fmt: i64) -> String {
    if fmt == 2 || fmt == 9 {
        return ord.to_string();
    }
    get(enum_id).and_then(|m| m.caption(ord).map(|s| s.to_string())).unwrap_or_else(|| ord.to_string())
}
