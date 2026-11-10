//! JsonObject / JsonArray / JsonValue / JsonToken. Objects keep insertion order and numbers keep
//! decimal precision, as in BC. Tokens are references: Get returns the token inside its parent.

use crate::interp::*;
use crate::record::{parse_date_text, parse_datetime_text, parse_num, parse_time_text};
use crate::value::*;
use al_syntax::Expr;
use rust_decimal::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum J {
    Null,
    Bool(bool),
    Num(Decimal),
    Str(String),
    Arr(Vec<JRef>),
    Obj(Vec<(String, JRef)>),
}

pub type JRef = Rc<RefCell<J>>;

pub fn jref(j: J) -> JRef {
    Rc::new(RefCell::new(j))
}

/// Deep copy (JsonToken.Clone()).
pub fn deep(j: &JRef) -> JRef {
    jref(match &*j.borrow() {
        J::Arr(a) => J::Arr(a.iter().map(deep).collect()),
        J::Obj(o) => J::Obj(o.iter().map(|(k, v)| (k.clone(), deep(v))).collect()),
        other => other.clone(),
    })
}

// ---------------------------------------------------------------- text

pub fn write(j: &J, out: &mut String) {
    match j {
        J::Null => out.push_str("null"),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        J::Num(d) => out.push_str(&d.normalize().to_string()),
        J::Str(s) => write_str(s, out),
        J::Arr(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(&v.borrow(), out);
            }
            out.push(']');
        }
        J::Obj(o) => {
            out.push('{');
            for (i, (k, v)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_str(k, out);
                out.push(':');
                write(&v.borrow(), out);
            }
            out.push('}');
        }
    }
}

fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn to_string(j: &JRef) -> String {
    let mut s = String::new();
    write(&j.borrow(), &mut s);
    s
}

struct P<'a> {
    c: &'a [char],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.c.len() && self.c[self.i].is_whitespace() {
            self.i += 1;
        }
    }
    fn value(&mut self) -> Option<J> {
        self.ws();
        match *self.c.get(self.i)? {
            '{' => {
                self.i += 1;
                let mut o = vec![];
                self.ws();
                if self.c.get(self.i) == Some(&'}') {
                    self.i += 1;
                    return Some(J::Obj(o));
                }
                loop {
                    self.ws();
                    let k = match self.value()? {
                        J::Str(s) => s,
                        _ => return None,
                    };
                    self.ws();
                    if self.c.get(self.i) != Some(&':') {
                        return None;
                    }
                    self.i += 1;
                    let v = self.value()?;
                    match o.iter_mut().find(|(ek, _): &&mut (String, JRef)| *ek == k) {
                        Some(e) => e.1 = jref(v),
                        None => o.push((k, jref(v))),
                    }
                    self.ws();
                    match self.c.get(self.i)? {
                        ',' => self.i += 1,
                        '}' => {
                            self.i += 1;
                            return Some(J::Obj(o));
                        }
                        _ => return None,
                    }
                }
            }
            '[' => {
                self.i += 1;
                let mut a = vec![];
                self.ws();
                if self.c.get(self.i) == Some(&']') {
                    self.i += 1;
                    return Some(J::Arr(a));
                }
                loop {
                    a.push(jref(self.value()?));
                    self.ws();
                    match self.c.get(self.i)? {
                        ',' => self.i += 1,
                        ']' => {
                            self.i += 1;
                            return Some(J::Arr(a));
                        }
                        _ => return None,
                    }
                }
            }
            '"' => {
                self.i += 1;
                let mut s = String::new();
                loop {
                    let c = *self.c.get(self.i)?;
                    self.i += 1;
                    match c {
                        '"' => return Some(J::Str(s)),
                        '\\' => {
                            let e = *self.c.get(self.i)?;
                            self.i += 1;
                            match e {
                                'n' => s.push('\n'),
                                'r' => s.push('\r'),
                                't' => s.push('\t'),
                                'b' => s.push('\u{8}'),
                                'f' => s.push('\u{c}'),
                                'u' => {
                                    let h: String = self.c.get(self.i..self.i + 4)?.iter().collect();
                                    self.i += 4;
                                    let mut cp = u32::from_str_radix(&h, 16).ok()?;
                                    if (0xD800..0xDC00).contains(&cp) && self.c.get(self.i) == Some(&'\\') && self.c.get(self.i + 1) == Some(&'u') {
                                        let h2: String = self.c.get(self.i + 2..self.i + 6)?.iter().collect();
                                        let lo = u32::from_str_radix(&h2, 16).ok()?;
                                        self.i += 6;
                                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    }
                                    s.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                                }
                                o => s.push(o),
                            }
                        }
                        c => s.push(c),
                    }
                }
            }
            't' if self.word("true") => Some(J::Bool(true)),
            'f' if self.word("false") => Some(J::Bool(false)),
            'n' if self.word("null") => Some(J::Null),
            _ => {
                let st = self.i;
                while self.i < self.c.len() && (self.c[self.i].is_ascii_digit() || "+-.eE".contains(self.c[self.i])) {
                    self.i += 1;
                }
                let t: String = self.c[st..self.i].iter().collect();
                Decimal::from_str(&t).ok().or_else(|| Decimal::from_scientific(&t).ok()).map(J::Num)
            }
        }
    }
    fn word(&mut self, w: &str) -> bool {
        let n = w.len();
        if self.c.get(self.i..self.i + n).is_some_and(|s| s.iter().collect::<String>() == w) {
            self.i += n;
            true
        } else {
            false
        }
    }
}

pub fn parse(text: &str) -> Option<J> {
    let c: Vec<char> = text.chars().collect();
    let mut p = P { c: &c, i: 0 };
    let v = p.value()?;
    p.ws();
    (p.i == c.len()).then_some(v)
}

// ---------------------------------------------------------------- AL values

/// AL value → JSON (JsonValue.SetValue / JsonObject.Add).
pub fn from_val(v: &Val) -> JRef {
    match v {
        Val::Json(j) => j.clone(),
        Val::Void => jref(J::Null),
        Val::Bool(b) => jref(J::Bool(*b)),
        Val::Int(_) | Val::BigInt(_) | Val::Dec(_) => jref(J::Num(v.as_dec())),
        Val::Opt { .. } => jref(J::Num(Decimal::from(v.as_i64()))),
        Val::Char(c) => jref(J::Str(c.to_string())),
        Val::Date(_) | Val::DateTime(_) => jref(J::Str(crate::builtins::format_val(v, 9))),
        Val::Time(t) if *t > 0 => {
            let ms = t - 1;
            jref(J::Str(format!("{:02}:{:02}:{:02}{}", ms / 3_600_000, (ms / 60_000) % 60, (ms / 1000) % 60, if ms % 1000 > 0 { format!(".{:03}", ms % 1000) } else { String::new() })))
        }
        Val::Guid(g) => jref(J::Str(format_guid(g))),
        o => jref(J::Str(o.to_text())),
    }
}

fn kind_of(j: &JRef) -> &'static str {
    match &*j.borrow() {
        J::Obj(_) => "object",
        J::Arr(_) => "array",
        _ => "value",
    }
}

fn as_text(j: &J) -> Option<String> {
    Some(match j {
        J::Str(s) => s.clone(),
        J::Num(d) => d.normalize().to_string(),
        J::Bool(b) => b.to_string(),
        _ => return None,
    })
}

impl<'r> Session<'r> {
    fn json_err<T>(&self, msg: impl Into<String>) -> R<T> {
        err(msg.into())
    }

    /// Methods of JsonObject / JsonArray / JsonValue / JsonToken values.
    pub fn json_method(&mut self, f: &mut Frame, j: &JRef, _base: &Expr, m: &str, args: &[Expr], discard: bool) -> R<Val> {
        // by-ref result arguments: Get(Key, var Token), WriteTo(var Text), SelectToken(Path, var Token)
        let out_arg = |s: &mut Self, f: &mut Frame, i: usize| -> R<Option<LVal>> {
            match args.get(i) {
                Some(e) => Ok(Some(s.lvalue(f, e)?)),
                None => Ok(None),
            }
        };
        let arg = |s: &mut Self, f: &mut Frame, i: usize| -> R<Val> {
            match args.get(i) {
                Some(e) => s.eval(f, e),
                None => Ok(Val::Void),
            }
        };
        match m {
            // ---- any token
            "isobject" => return Ok(Val::Bool(kind_of(j) == "object")),
            "isarray" => return Ok(Val::Bool(kind_of(j) == "array")),
            "isvalue" => return Ok(Val::Bool(kind_of(j) == "value")),
            "asobject" | "asarray" | "asvalue" | "astoken" => {
                let want = &m[2..];
                if want != "token" && kind_of(j) != want {
                    return self.json_err(format!("The JSON token is not a {}", want));
                }
                return Ok(Val::Json(j.clone()));
            }
            "clone" => return Ok(Val::Json(deep(j))),
            "writeto" => {
                let s = to_string(j);
                if let Some(lv) = out_arg(self, f, 0)? {
                    match self.read_lval(&lv) {
                        Val::OutStream(t) => self.outstream_write_text(&t, &s),
                        _ => self.write_lval(&lv, Val::Text(s))?,
                    }
                }
                return Ok(Val::Bool(true));
            }
            "readfrom" => {
                let src = arg(self, f, 0)?;
                let text = match src {
                    Val::InStream(st) => {
                        let mut st = st.borrow_mut();
                        let t = String::from_utf8_lossy(&st.data[st.pos.min(st.data.len())..]).into_owned();
                        st.pos = st.data.len();
                        t
                    }
                    o => o.to_text(),
                };
                let text = text.trim_start_matches('\u{feff}');
                return match parse(text) {
                    Some(v) => {
                        // a JsonObject/JsonArray only reads its own kind of token
                        let mismatch = matches!((&*j.borrow(), &v), (J::Obj(_), J::Arr(_) | J::Null | J::Bool(_) | J::Num(_) | J::Str(_)) | (J::Arr(_), J::Obj(_) | J::Null | J::Bool(_) | J::Num(_) | J::Str(_)));
                        if mismatch {
                            if discard {
                                return self.json_err("Invalid JSON: the text does not contain a JSON token of the expected type.");
                            }
                            return Ok(Val::Bool(false));
                        }
                        *j.borrow_mut() = v;
                        Ok(Val::Bool(true))
                    }
                    None if discard => self.json_err(format!("Invalid JSON. {}", text.chars().take(80).collect::<String>())),
                    None => Ok(Val::Bool(false)),
                };
            }
            "path" => return Ok(Val::Text("$".into())),
            // WriteWithSecretsTo(Path, Secret, var Result) / WriteWithSecretsTo(Secrets: Dictionary
            // of [Text, SecretText], var Result): the JSON text with the tokens at the paths replaced
            // by the secrets (as strings); the object itself is unchanged
            "writewithsecretsto" => {
                let first = arg(self, f, 0)?;
                let (pairs, out_i): (Vec<(String, String)>, usize) = match &first {
                    Val::Dict(d) => (d.borrow().iter().map(|(k, v)| (k.to_text(), v.to_text())).collect(), 1),
                    p => (vec![(p.to_text(), arg(self, f, 1)?.to_text())], 2),
                };
                let copy = deep(j);
                for (path, secret) in &pairs {
                    match select(&copy, path) {
                        Some(t) => *t.borrow_mut() = J::Str(secret.clone()),
                        // own wording [src:X09]
                        None if discard => return self.json_err(format!("No JSON token matches the path '{path}'.")),
                        None => return Ok(Val::Bool(false)),
                    }
                }
                if let Some(lv) = out_arg(self, f, out_i)? {
                    self.write_lval(&lv, Val::Text(to_string(&copy)))?;
                }
                return Ok(Val::Bool(true));
            }
            "selecttoken" | "selecttokens" => {
                let path = arg(self, f, 0)?.to_text();
                let found = select(j, &path);
                let ok = found.is_some();
                if let (Some(t), Some(lv)) = (found, out_arg(self, f, 1)?) {
                    self.write_lval(&lv, Val::Json(t))?;
                }
                if !ok && discard {
                    // own wording [src:X09]
                    return self.json_err(format!("No JSON token matches the path '{path}'."));
                }
                return Ok(Val::Bool(ok));
            }
            _ => {}
        }
        let kind = kind_of(j);
        match kind {
            "object" => self.json_object_method(f, j, m, args, discard),
            "array" => self.json_array_method(f, j, m, args, discard),
            _ => self.json_value_method(f, j, m, args),
        }
    }

    fn json_object_method(&mut self, f: &mut Frame, j: &JRef, m: &str, args: &[Expr], discard: bool) -> R<Val> {
        let key = match args.first() {
            Some(e) => self.eval(f, e)?.to_text(),
            None => String::new(),
        };
        let find = |j: &JRef, k: &str| -> Option<JRef> {
            match &*j.borrow() {
                J::Obj(o) => o.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()),
                _ => None,
            }
        };
        match m {
            "add" | "replace" => {
                let v = match args.get(1) {
                    Some(e) => from_val(&self.eval(f, e)?),
                    None => jref(J::Null),
                };
                let exists = find(j, &key);
                if m == "add" && exists.is_some() {
                    // own wording [src:X08]
                    return self.json_err(format!("The JSON object has a property '{key}' already."));
                }
                if m == "replace" && exists.is_none() {
                    if discard {
                        return self.json_err(format!("The JSON object does not contain the key '{key}'."));
                    }
                    return Ok(Val::Bool(false));
                }
                if let J::Obj(o) = &mut *j.borrow_mut() {
                    match o.iter_mut().find(|(x, _)| *x == key) {
                        Some(e) => e.1 = v,
                        None => o.push((key, v)),
                    }
                }
                Ok(Val::Bool(true))
            }
            "remove" => {
                let mut b = j.borrow_mut();
                let J::Obj(o) = &mut *b else { return Ok(Val::Bool(false)) };
                let n = o.len();
                o.retain(|(x, _)| *x != key);
                Ok(Val::Bool(o.len() != n))
            }
            "contains" => Ok(Val::Bool(find(j, &key).is_some())),
            "get" => {
                let v = find(j, &key);
                let ok = v.is_some();
                if let (Some(v), Some(e)) = (v, args.get(1)) {
                    let lv = self.lvalue(f, e)?;
                    self.write_lval(&lv, Val::Json(v))?;
                }
                if !ok && discard {
                    return self.json_err(format!("The JSON object does not contain the key '{key}'."));
                }
                Ok(Val::Bool(ok))
            }
            "keys" => Ok(Val::List(Rc::new(RefCell::new(match &*j.borrow() {
                J::Obj(o) => o.iter().map(|(k, _)| Val::Text(k.clone())).collect(),
                _ => vec![],
            })))),
            "values" => Ok(Val::List(Rc::new(RefCell::new(match &*j.borrow() {
                J::Obj(o) => o.iter().map(|(_, v)| Val::Json(v.clone())).collect(),
                _ => vec![],
            })))),
            "count" => Ok(Val::Int(match &*j.borrow() {
                J::Obj(o) => o.len() as i32,
                _ => 0,
            })),
            _ if m.starts_with("get") => {
                // GetText / GetInteger / GetBoolean / GetDecimal / GetObject / GetArray (Key [, DefaultIfNotFound])
                let default_ok = match args.get(1) {
                    Some(e) => self.eval(f, e)?.as_bool(),
                    None => false,
                };
                match find(j, &key) {
                    Some(v) => convert(&v, &m[3..]),
                    None if default_ok => Ok(default_of(&m[3..])),
                    None => self.json_err(format!("The JSON object does not contain the key '{key}'.")),
                }
            }
            _ => {
                *self.missing.entry(format!("JsonObject.{m}")).or_default() += 1;
                err(format!("Not implemented in bc-rust: JsonObject.{m}"))
            }
        }
    }

    fn json_array_method(&mut self, f: &mut Frame, j: &JRef, m: &str, args: &[Expr], discard: bool) -> R<Val> {
        let mut a = vec![];
        for (i, e) in args.iter().enumerate() {
            // Get(Index, var Token): the second argument is an output
            if m == "get" && i == 1 {
                break;
            }
            a.push(self.eval(f, e)?);
        }
        let len = match &*j.borrow() {
            J::Arr(x) => x.len(),
            _ => 0,
        };
        let idx = |v: Option<&Val>| v.map(|x| x.as_i64()).unwrap_or(0);
        match m {
            "add" => {
                let v = from_val(a.first().unwrap_or(&Val::Void));
                if let J::Arr(x) = &mut *j.borrow_mut() {
                    x.push(v);
                }
                Ok(Val::Void)
            }
            "count" => Ok(Val::Int(len as i32)),
            "get" => {
                let i = idx(a.first());
                let v = match &*j.borrow() {
                    J::Arr(x) if i >= 0 => x.get(i as usize).cloned(),
                    _ => None,
                };
                let ok = v.is_some();
                if let (Some(v), Some(e)) = (v, args.get(1)) {
                    let lv = self.lvalue(f, e)?;
                    self.write_lval(&lv, Val::Json(v))?;
                }
                if !ok && discard {
                    return self.json_err(format!("The index {i} is out of range of the JSON array."));
                }
                Ok(Val::Bool(ok))
            }
            "insert" | "set" => {
                let i = idx(a.first()).max(0) as usize;
                let v = from_val(a.get(1).unwrap_or(&Val::Void));
                let ok = if m == "insert" { i <= len } else { i < len };
                if ok {
                    if let J::Arr(x) = &mut *j.borrow_mut() {
                        if m == "insert" {
                            x.insert(i, v);
                        } else {
                            x[i] = v;
                        }
                    }
                } else if discard {
                    return self.json_err(format!("The index {i} is out of range of the JSON array."));
                }
                Ok(Val::Bool(ok))
            }
            "removeat" => {
                let i = idx(a.first());
                let ok = i >= 0 && (i as usize) < len;
                if ok {
                    if let J::Arr(x) = &mut *j.borrow_mut() {
                        x.remove(i as usize);
                    }
                }
                Ok(Val::Bool(ok))
            }
            "remove" | "indexof" => {
                let target = match a.first() {
                    Some(Val::Json(t)) => to_string(t),
                    Some(v) => to_string(&from_val(v)),
                    None => String::new(),
                };
                let pos = match &*j.borrow() {
                    J::Arr(x) => x.iter().position(|e| to_string(e) == target),
                    _ => None,
                };
                if m == "indexof" {
                    return Ok(Val::Int(pos.map(|p| p as i32).unwrap_or(-1)));
                }
                if let (Some(p), J::Arr(x)) = (pos, &mut *j.borrow_mut()) {
                    x.remove(p);
                }
                Ok(Val::Bool(pos.is_some()))
            }
            _ if m.starts_with("get") => {
                let i = idx(a.first());
                let v = match &*j.borrow() {
                    J::Arr(x) if i >= 0 => x.get(i as usize).cloned(),
                    _ => None,
                };
                match v {
                    Some(v) => convert(&v, &m[3..]),
                    None => self.json_err(format!("The index {i} is out of range of the JSON array.")),
                }
            }
            _ => {
                *self.missing.entry(format!("JsonArray.{m}")).or_default() += 1;
                err(format!("Not implemented in bc-rust: JsonArray.{m}"))
            }
        }
    }

    fn json_value_method(&mut self, f: &mut Frame, j: &JRef, m: &str, args: &[Expr]) -> R<Val> {
        match m {
            "isnull" => Ok(Val::Bool(matches!(&*j.borrow(), J::Null))),
            "isundefined" => Ok(Val::Bool(false)),
            "setvalue" => {
                let v = match args.first() {
                    Some(e) => self.eval(f, e)?,
                    None => Val::Void,
                };
                let nv = from_val(&v).borrow().clone();
                *j.borrow_mut() = nv;
                Ok(Val::Void)
            }
            "setvaluetonull" => {
                *j.borrow_mut() = J::Null;
                Ok(Val::Void)
            }
            _ if m.starts_with("as") => convert(j, &m[2..]),
            _ => {
                *self.missing.entry(format!("JsonValue.{m}")).or_default() += 1;
                err(format!("Not implemented in bc-rust: JsonValue.{m}"))
            }
        }
    }
}

fn default_of(ty: &str) -> Val {
    match ty {
        "text" | "code" => Val::Text(String::new()),
        "integer" => Val::Int(0),
        "biginteger" => Val::BigInt(0),
        "decimal" => Val::Dec(Decimal::ZERO),
        "boolean" => Val::Bool(false),
        "object" => Val::Json(jref(J::Obj(vec![]))),
        "array" => Val::Json(jref(J::Arr(vec![]))),
        _ => Val::Void,
    }
}

/// JsonValue.AsX / JsonObject.GetX conversion.
fn convert(j: &JRef, ty: &str) -> R<Val> {
    let b = j.borrow();
    let bad = |what: &str| -> R<Val> { err(format!("Unable to convert from the JSON value to {what}.")) };
    Ok(match ty {
        "text" | "code" => match as_text(&b) {
            Some(s) => Val::Text(if ty == "code" { s.to_uppercase() } else { s }),
            None if matches!(&*b, J::Null) => Val::Text(String::new()),
            None => return bad("Text"),
        },
        "integer" | "biginteger" | "decimal" | "byte" | "option" | "duration" => {
            let d = match &*b {
                J::Num(d) => *d,
                J::Str(s) => match parse_num(s.trim()) {
                    Some(d) => d,
                    None => return bad(ty),
                },
                J::Bool(x) => Decimal::from(*x as i32),
                _ => return bad(ty),
            };
            match ty {
                "decimal" => Val::Dec(d),
                "biginteger" | "duration" => Val::BigInt(d.trunc().to_i64().unwrap_or(0)),
                "option" => Val::Opt { ord: d.trunc().to_i32().unwrap_or(0), enum_id: None },
                _ => Val::Int(d.trunc().to_i32().unwrap_or(0)),
            }
        }
        "boolean" => match &*b {
            J::Bool(x) => Val::Bool(*x),
            J::Str(s) if s.eq_ignore_ascii_case("true") || s.eq_ignore_ascii_case("false") => Val::Bool(s.eq_ignore_ascii_case("true")),
            _ => return bad("Boolean"),
        },
        "date" => match &*b {
            J::Str(s) => Val::Date(parse_date_text(s.get(..10).unwrap_or(s), 0).unwrap_or(0)),
            _ => return bad("Date"),
        },
        "datetime" => match &*b {
            J::Str(s) => Val::DateTime(parse_datetime_text(s).unwrap_or(bc_store::DATETIME_UNDEFINED)),
            _ => return bad("DateTime"),
        },
        "time" => match &*b {
            J::Str(s) => Val::Time(parse_time_text(s).unwrap_or(0)),
            _ => return bad("Time"),
        },
        "char" => match &*b {
            J::Str(s) => Val::Char(s.chars().next().unwrap_or('\0')),
            J::Num(d) => Val::Char(char::from_u32(d.to_u32().unwrap_or(0)).unwrap_or('\0')),
            _ => return bad("Char"),
        },
        "token" | "value" => Val::Json(j.clone()),
        "object" => match &*b {
            J::Obj(_) => Val::Json(j.clone()),
            _ => return err("The JSON token is not an object."),
        },
        "array" => match &*b {
            J::Arr(_) => Val::Json(j.clone()),
            _ => return err("The JSON token is not an array."),
        },
        _ => return err(format!("Not implemented in bc-rust: JsonValue.As{ty}")),
    })
}

/// SelectToken with a simple JSONPath: `$.a.b[0].c`, `a.b`, `['a']`.
fn select(root: &JRef, path: &str) -> Option<JRef> {
    let p = path.trim();
    let p = p.strip_prefix('$').unwrap_or(p);
    let mut cur = root.clone();
    let chars: Vec<char> = p.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '.' => i += 1,
            '[' => {
                let end = chars[i..].iter().position(|c| *c == ']')? + i;
                let inner: String = chars[i + 1..end].iter().collect();
                let inner = inner.trim();
                let next = if let Ok(n) = inner.parse::<usize>() {
                    match &*cur.borrow() {
                        J::Arr(a) => a.get(n).cloned(),
                        _ => None,
                    }
                } else {
                    let k = inner.trim_matches(|c| c == '\'' || c == '"');
                    obj_get(&cur, k)
                };
                cur = next?;
                i = end + 1;
            }
            _ => {
                let st = i;
                while i < chars.len() && chars[i] != '.' && chars[i] != '[' {
                    i += 1;
                }
                let k: String = chars[st..i].iter().collect();
                cur = obj_get(&cur, &k)?;
            }
        }
    }
    Some(cur)
}

fn obj_get(j: &JRef, k: &str) -> Option<JRef> {
    match &*j.borrow() {
        J::Obj(o) => o.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_order_and_decimals() {
        let src = r#"{"b":1.10,"a":[true,null,"x\"y"],"c":{"d":-2.5e1}}"#;
        let j = jref(parse(src).unwrap());
        assert_eq!(to_string(&j), r#"{"b":1.1,"a":[true,null,"x\"y"],"c":{"d":-25}}"#);
        assert_eq!(to_string(&select(&j, "$.a[2]").unwrap()), r#""x\"y""#);
        assert!(select(&j, "c.d").is_some());
        assert!(parse("{").is_none());
    }
}
