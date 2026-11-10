//! JSON building blocks for metadata objects: the `$polyType` wrapper, EDSpec bit
//! tracking and default omission.
//!
//! Every `ElementDefinition` goes out as `{"$polyType": "<full type name>", "obj": {...}}`.
//! Inside `obj`, `EDSpec` is written first and carries the *complete* specified-properties
//! mask, i.e. the bit of every flagged property that is present plus the bits of explicit
//! defaults that are left out. The web server's private `EDSpec` setter replaces the whole
//! mask, so a full mask gives the same result whether it is applied before or after the
//! other setters (and whether Newtonsoft goes through the `[JsonConstructor]` or not).

use serde_json::{Map, Value};

/// Namespace of every page metadata type.
pub(crate) const META_NS: &str = "Microsoft.Dynamics.Nav.Types.Metadata.";

/// Private copy of `wire::poly` so this module does not depend on the rpc owner's file.
pub(crate) fn poly(type_name: &str, obj: Value) -> Value {
    let mut m = Map::new();
    m.insert("$polyType".into(), Value::String(type_name.into()));
    m.insert("obj".into(), obj);
    Value::Object(m)
}

/// Move `EDSpec` to the front of every object in `v` (captured real-tier JSON has it last).
/// The captured masks are complete, so first and last give the same result in the web
/// server; first is bc-nst's convention (see the module docs).
pub(crate) fn edspec_first(v: &mut Value) {
    match v {
        Value::Object(m) => {
            if let Some(e) = m.shift_remove("EDSpec") {
                let rest = std::mem::take(m);
                m.insert("EDSpec".into(), e);
                m.extend(rest);
            }
            m.values_mut().for_each(edspec_first);
        }
        Value::Array(a) => a.iter_mut().for_each(edspec_first),
        _ => {}
    }
}

/// One metadata object under construction. `ty` is the short class name
/// (`ControlDefinition`); [`El::wrap`] prefixes the namespace.
pub(crate) struct El {
    ty: &'static str,
    bits: u32,
    m: Map<String, Value>,
}

impl El {
    pub fn new(ty: &'static str) -> El {
        El { ty, bits: 0, m: Map::new() }
    }

    /// Write a member unconditionally.
    pub fn put(&mut self, k: &str, v: impl Into<Value>) {
        self.m.insert(k.into(), v.into());
    }

    /// Write a string member unless it is empty (every string member defaults to null or "").
    pub fn str(&mut self, k: &str, v: &str) {
        if !v.is_empty() {
            self.put(k, v);
        }
    }

    /// Mark EDSpec bit `b` as specified.
    pub fn bit(&mut self, b: u32) {
        self.bits |= 1 << b;
    }

    /// A flagged bool whose default is false: the bit is always set, the value only written when true.
    pub fn flag(&mut self, k: &str, v: bool, bit: u32) {
        self.bit(bit);
        if v {
            self.put(k, true);
        }
    }

    /// A flagged integer or enum whose default is 0: the bit is always set, the value only written when non-zero.
    pub fn num_bit(&mut self, k: &str, v: i64, bit: u32) {
        self.bit(bit);
        if v != 0 {
            self.put(k, v);
        }
    }

    /// A flagged enum of one of the two enum types in a `[NavJsonDerivedType]` hierarchy
    /// (`RunObjectType`, `ActionContainerType`): the web server's polymorphic converter
    /// claims those, so the value goes out as `{"$polyType": "<enum type>", "obj": <int>}`.
    pub fn poly_enum_bit(&mut self, k: &str, enum_type: &str, v: i64, bit: u32) {
        self.bit(bit);
        if v != 0 {
            self.put(k, poly(&format!("{META_NS}{enum_type}"), Value::from(v)));
        }
    }

    /// An unflagged integer whose default is 0.
    pub fn num(&mut self, k: &str, v: i64) {
        if v != 0 {
            self.put(k, v);
        }
    }

    /// A list member, written even when empty (the builders iterate some lists without null checks).
    pub fn list(&mut self, k: &str, v: Vec<Value>) {
        self.put(k, Value::Array(v));
    }

    /// A list member that is left out when empty.
    pub fn list_nonempty(&mut self, k: &str, v: Vec<Value>) {
        if !v.is_empty() {
            self.list(k, v);
        }
    }

    /// The `obj` part: `EDSpec` first, then the members in insertion order.
    pub fn obj(self) -> Value {
        let mut out = Map::new();
        if self.bits != 0 {
            out.insert("EDSpec".into(), Value::from(self.bits));
        }
        out.extend(self.m);
        Value::Object(out)
    }

    /// The `$polyType` wrapped object.
    pub fn wrap(self) -> Value {
        let ty = format!("{META_NS}{}", self.ty);
        poly(&ty, self.obj())
    }
}
