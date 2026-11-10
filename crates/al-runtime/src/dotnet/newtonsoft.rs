//! Newtonsoft.Json.Linq (JObject, JArray, JToken, JValue, JProperty) and JsonConvert, over the
//! JSON tree of crate::json (the one AL JsonObject/JsonArray use). Tokens are references into
//! their parent, as in Json.NET; ToString() is Json.NET's indented format.

use super::*;
use crate::json::{self, jref, JRef, J};

pub fn jtok(j: JRef) -> Val {
    net(NetObj::JTok(j))
}

/// Json.NET's indented serialization (2 spaces, CR LF).
pub fn write_indented(j: &J, out: &mut String, depth: usize) {
    let pad = |d: usize| "  ".repeat(d);
    match j {
        J::Obj(o) if o.is_empty() => out.push_str("{}"),
        J::Arr(a) if a.is_empty() => out.push_str("[]"),
        J::Obj(o) => {
            out.push_str("{\r\n");
            for (i, (k, v)) in o.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                json::write(&J::Str(k.clone()), out);
                out.push_str(": ");
                write_indented(&v.borrow(), out, depth + 1);
                if i + 1 < o.len() {
                    out.push(',');
                }
                out.push_str("\r\n");
            }
            out.push_str(&pad(depth));
            out.push('}');
        }
        J::Arr(a) => {
            out.push_str("[\r\n");
            for (i, v) in a.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                write_indented(&v.borrow(), out, depth + 1);
                if i + 1 < a.len() {
                    out.push(',');
                }
                out.push_str("\r\n");
            }
            out.push_str(&pad(depth));
            out.push(']');
        }
        other => json::write(other, out),
    }
}

/// JToken.ToString(): indented for containers, the plain value for strings (no quotes).
pub fn token_to_string(j: &JRef, indented: bool) -> String {
    let b = j.borrow();
    match &*b {
        J::Str(s) => s.clone(),
        J::Null => String::new(),
        J::Bool(v) => if *v { "True" } else { "False" }.into(),
        J::Num(d) => d.normalize().to_string(),
        other => {
            let mut out = String::new();
            if indented {
                write_indented(other, &mut out, 0);
            } else {
                json::write(other, &mut out);
            }
            out
        }
    }
}

/// A value to store in a JObject/JArray: tokens are referenced (cloned when they already have
/// a parent in Json.NET; here they are deep-copied to keep trees independent), AL values converted.
fn to_token(v: Option<&Val>) -> JRef {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::JTok(j) => json::deep(j),
            NetObj::JProp(o, k) => obj_get(o, k).map(|x| json::deep(&x)).unwrap_or_else(|| jref(J::Null)),
            NetObj::Prim(_, p) => json::from_val(p),
            NetObj::Null(_) => jref(J::Null),
            _ => jref(J::Str(super::to_text(n))),
        },
        Some(v) => json::from_val(v),
        None => jref(J::Null),
    }
}

fn obj_get(o: &JRef, k: &str) -> Option<JRef> {
    match &*o.borrow() {
        J::Obj(items) => items.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()),
        _ => None,
    }
}

fn parse_err<T>(t: &str, text: &str) -> R<T> {
    let c = text.trim_start().chars().next().map(|c| c.to_string()).unwrap_or_default();
    net_err(t, "Parse", format!("Unexpected character encountered while parsing value: {c}. Path '', line 1, position 1."))
}

/// SelectToken with a simple JSONPath: `$.a.b[0].c`, `a.b`, `['a']`.
fn select(root: &JRef, path: &str) -> Option<JRef> {
    let p = path.trim().trim_start_matches('$');
    let mut cur = root.clone();
    let mut rest = p;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            rest = r;
            continue;
        }
        if let Some(r) = rest.strip_prefix('[') {
            let end = r.find(']')?;
            let key = r[..end].trim().trim_matches('\'').trim_matches('"');
            rest = &r[end + 1..];
            let next = match key.parse::<usize>() {
                Ok(i) => match &*cur.borrow() {
                    J::Arr(a) => a.get(i).cloned(),
                    _ => None,
                },
                Err(_) => obj_get(&cur, key),
            }?;
            cur = next;
            continue;
        }
        let end = rest.find(['.', '[']).unwrap_or(rest.len());
        let key = &rest[..end];
        rest = &rest[end..];
        cur = obj_get(&cur, key)?;
    }
    Some(cur)
}

pub fn construct(ty: &'static str, a: &Args) -> R<Val> {
    Ok(match ty {
        "Newtonsoft.Json.Linq.JObject" => {
            let o = match a.raw(0) {
                Some(Val::Net(n)) => match &*n.borrow() {
                    NetObj::JTok(j) if matches!(&*j.borrow(), J::Obj(_)) => json::deep(j),
                    _ => jref(J::Obj(vec![])),
                },
                _ => jref(J::Obj(vec![])),
            };
            jtok(o)
        }
        "Newtonsoft.Json.Linq.JArray" => jtok(jref(J::Arr((0..a.len()).map(|i| to_token(a.raw(i))).collect()))),
        "Newtonsoft.Json.Linq.JValue" => jtok(to_token(a.raw(0))),
        "Newtonsoft.Json.Linq.JProperty" => {
            // a detached property: an object holding just this key
            let o = jref(J::Obj(vec![(a.text(0), to_token(a.raw(1)))]));
            net(NetObj::JProp(o, a.text(0)))
        }
        JSON_TEXT_READER => {
            let text = match a.raw(0) {
                Some(Val::Net(r)) => match &*r.borrow() {
                    NetObj::Reader(_, s, p) => s[s.char_indices().nth(*p).map(|x| x.0).unwrap_or(s.len())..].to_string(),
                    _ => String::new(),
                },
                Some(v) => v.to_text(),
                None => String::new(),
            };
            let (toks, err) = reader_tokens(&text);
            net(NetObj::JReader(Box::new(JReader { toks, err, cur: None })))
        }
        _ => net(NetObj::Null(ty)),
    })
}

pub fn call(s: &mut Session, f: &mut Frame, n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    if let NetObj::JReader(r) = &mut *n.borrow_mut() {
        return reader_call(r, m);
    }
    // statics
    match (ty, m) {
        ("Newtonsoft.Json.Formatting", "none") => return ok(enum_val(ty, 0)),
        ("Newtonsoft.Json.Formatting", "indented") => return ok(enum_val(ty, 1)),
        ("Newtonsoft.Json.JsonConvert", "serializeobject") => {
            let indented = a.int(1) == 1;
            let t = match a.raw(0) {
                Some(Val::Net(x)) => match &*x.borrow() {
                    NetObj::ChartData(cd) => super::data::chart_json(cd),
                    NetObj::JTok(j) => {
                        let mut out = String::new();
                        if indented {
                            write_indented(&j.borrow(), &mut out, 0);
                        } else {
                            json::write(&j.borrow(), &mut out);
                        }
                        out
                    }
                    _ => {
                        let mut out = String::new();
                        json::write(&J::Str(super::to_text(x)), &mut out);
                        out
                    }
                },
                Some(v) => json::to_string(&json::from_val(v)),
                None => "null".into(),
            };
            return ok(Val::Text(t));
        }
        ("Newtonsoft.Json.JsonConvert", "deserializeobject") => {
            return match json::parse(&a.text(0)) {
                Some(j) => ok(jtok(jref(j))),
                None => parse_err("JsonConvert", &a.text(0)),
            }
        }
        (_, "parse") | (_, "fromobject") if matches!(&*n.borrow(), NetObj::Null(_)) || m == "parse" => {
            let text = a.text(0);
            let parsed = if m == "fromobject" { Some(to_token(a.raw(0))) } else { json::parse(&text).map(jref) };
            let Some(j) = parsed else { return parse_err(super::short_name(ty), &text) };
            let kind_ok = match (ty, &*j.borrow()) {
                ("Newtonsoft.Json.Linq.JObject", J::Obj(_)) | ("Newtonsoft.Json.Linq.JArray", J::Arr(_)) => true,
                ("Newtonsoft.Json.Linq.JObject", _) => false,
                ("Newtonsoft.Json.Linq.JArray", _) => false,
                _ => true,
            };
            if !kind_ok {
                let what = if ty.ends_with("JObject") { "JObject" } else { "JArray" };
                return net_err(what, "Parse", format!("Error reading {what} from JsonReader. Current JsonReader item is not an object: {}. Path '', line 1, position 1.", if ty.ends_with("JObject") { "StartArray" } else { "StartObject" }));
            }
            return ok(jtok(j));
        }
        _ => {}
    }
    let o = n.borrow();
    match &*o {
        NetObj::JTok(j) => {
            let j = j.clone();
            drop(o);
            token_call(s, f, &j, m, a)
        }
        NetObj::JProp(obj, key) => {
            let (obj, key) = (obj.clone(), key.clone());
            drop(o);
            let r = match m {
                "name" => Val::Text(key),
                "value" if a.len() == 1 => {
                    let v = to_token(a.raw(0));
                    if let J::Obj(items) = &mut *obj.borrow_mut() {
                        if let Some(e) = items.iter_mut().find(|(k, _)| *k == key) {
                            e.1 = v;
                        }
                    }
                    Val::Void
                }
                "value" => obj_get(&obj, &key).map(jtok).unwrap_or(Val::Void),
                "replace" => {
                    // replace this property (in its object) with another property
                    if let Some(Val::Net(x)) = a.raw(0) {
                        if let NetObj::JProp(o2, k2) = &*x.borrow() {
                            let v = obj_get(o2, k2).map(|v| json::deep(&v)).unwrap_or_else(|| jref(J::Null));
                            if let J::Obj(items) = &mut *obj.borrow_mut() {
                                if let Some(e) = items.iter_mut().find(|(k, _)| *k == key) {
                                    *e = (k2.clone(), v);
                                }
                            }
                        }
                    }
                    Val::Void
                }
                "remove" => {
                    if let J::Obj(items) = &mut *obj.borrow_mut() {
                        items.retain(|(k, _)| *k != key);
                    }
                    Val::Void
                }
                "tostring" => {
                    let mut out = String::new();
                    json::write(&J::Str(key.clone()), &mut out);
                    out.push_str(": ");
                    if let Some(v) = obj_get(&obj, &key) {
                        write_indented(&v.borrow(), &mut out, 0);
                    }
                    Val::Text(out)
                }
                _ => return Ok(Out::Unknown),
            };
            ok(r)
        }
        _ => Ok(Out::Unknown),
    }
}

fn token_call(s: &mut Session, f: &mut Frame, j: &JRef, m: &str, a: &Args) -> R<Out> {
    let is_obj = matches!(&*j.borrow(), J::Obj(_));
    let r = match m {
        "tostring" => Val::Text(token_to_string(j, a.int(0) != 0 || a.is_empty())),
        "count" => Val::Int(match &*j.borrow() {
            J::Obj(o) => o.len() as i32,
            J::Arr(x) => x.len() as i32,
            _ => 0,
        }),
        "deepclone" => jtok(json::deep(j)),
        "type" => enum_val(
            "Newtonsoft.Json.Linq.JTokenType",
            match &*j.borrow() {
                J::Obj(_) => 1,
                J::Arr(_) => 2,
                J::Num(d) if d.fract().is_zero() => 6,
                J::Num(_) => 7,
                J::Str(_) => 8,
                J::Bool(_) => 9,
                J::Null => 10,
            },
        ),
        "selecttoken" => select(j, &a.text(0)).map(jtok).unwrap_or_else(|| net(NetObj::Null("Newtonsoft.Json.Linq.JToken"))),
        "value" if !is_obj && a.is_empty() => match &*j.borrow() {
            J::Str(s) => Val::Text(s.clone()),
            J::Num(d) => Val::Dec(*d),
            J::Bool(b) => Val::Bool(*b),
            _ => Val::Void,
        },
        // ---- JObject
        "add" if is_obj => {
            // Add(JProperty) takes the property's name and value; Add(name, value) the pair
            let prop = match a.raw(0) {
                Some(Val::Net(p)) if a.len() == 1 => match &*p.borrow() {
                    NetObj::JProp(_, k) => Some(k.clone()),
                    _ => None,
                },
                _ => None,
            };
            let (k, v) = match prop {
                Some(k) => (k, to_token(a.raw(0))),
                None => (a.text(0), to_token(a.raw(1))),
            };
            let mut b = j.borrow_mut();
            let J::Obj(items) = &mut *b else { unreachable!() };
            if items.iter().any(|(x, _)| *x == k) {
                drop(b);
                return net_err("JObject", "Add", format!("Can not add property {k} to Newtonsoft.Json.Linq.JObject. Property with the same name already exists on object."));
            }
            items.push((k, v));
            Val::Void
        }
        "trygetvalue" if is_obj => {
            let found = obj_get(j, &a.text(0));
            let r = found.is_some();
            if let Some(v) = found {
                s.net_out(f, a, 1, jtok(v))?;
            }
            Val::Bool(r)
        }
        "property" if is_obj => {
            let k = a.text(0);
            if obj_get(j, &k).is_some() {
                net(NetObj::JProp(j.clone(), k))
            } else {
                net(NetObj::Null("Newtonsoft.Json.Linq.JProperty"))
            }
        }
        "properties" if is_obj => {
            let keys: Vec<String> = match &*j.borrow() {
                J::Obj(o) => o.iter().map(|(k, _)| k.clone()).collect(),
                _ => vec![],
            };
            net(NetObj::List("System.Collections.Generic.List`1", keys.into_iter().map(|k| net(NetObj::JProp(j.clone(), k))).collect()))
        }
        "remove" if is_obj => {
            let k = a.text(0);
            let mut b = j.borrow_mut();
            let J::Obj(items) = &mut *b else { unreachable!() };
            let had = items.iter().any(|(x, _)| *x == k);
            items.retain(|(x, _)| *x != k);
            Val::Bool(had)
        }
        "containskey" if is_obj => Val::Bool(obj_get(j, &a.text(0)).is_some()),
        "item" | "get_item" | "getvalue" => {
            let key = a.get(0);
            if a.len() == 2 {
                let v = to_token(a.raw(1));
                match &mut *j.borrow_mut() {
                    J::Obj(items) => {
                        let k = key.to_text();
                        match items.iter_mut().find(|(x, _)| *x == k) {
                            Some(e) => e.1 = v,
                            None => items.push((k, v)),
                        }
                    }
                    J::Arr(x) => {
                        let i = key.as_i64() as usize;
                        if i < x.len() {
                            x[i] = v;
                        }
                    }
                    _ => {}
                }
                Val::Void
            } else {
                let found = match &*j.borrow() {
                    J::Obj(_) => obj_get(j, &key.to_text()),
                    J::Arr(x) => x.get(key.as_i64() as usize).cloned(),
                    _ => None,
                };
                match found {
                    Some(v) => jtok(v),
                    None if is_obj => net(NetObj::Null("Newtonsoft.Json.Linq.JToken")),
                    None => return net_err("JArray", "Item", "Index was out of range. Must be non-negative and less than the size of the collection. (Parameter 'index')"),
                }
            }
        }
        // ---- JArray
        "add" => {
            if let J::Arr(x) = &mut *j.borrow_mut() {
                x.push(to_token(a.raw(0)));
            }
            Val::Void
        }
        "insert" => {
            if let J::Arr(x) = &mut *j.borrow_mut() {
                let i = (a.int(0).max(0) as usize).min(x.len());
                x.insert(i, to_token(a.raw(1)));
            }
            Val::Void
        }
        "removeat" => {
            let mut b = j.borrow_mut();
            if let J::Arr(x) = &mut *b {
                let i = a.int(0);
                if i < 0 || i as usize >= x.len() {
                    drop(b);
                    return net_err("JArray", "RemoveAt", "Index is equal to or greater than Count. (Parameter 'index')");
                }
                x.remove(i as usize);
            }
            Val::Void
        }
        "clear" | "removeall" => {
            match &mut *j.borrow_mut() {
                J::Arr(x) => x.clear(),
                J::Obj(o) => o.clear(),
                _ => {}
            }
            Val::Void
        }
        "getenumerator" => {
            let items: Vec<Val> = match &*j.borrow() {
                J::Arr(x) => x.iter().cloned().map(jtok).collect(),
                J::Obj(o) => o.iter().map(|(k, _)| net(NetObj::JProp(j.clone(), k.clone()))).collect(),
                _ => vec![],
            };
            net(NetObj::Enumerator(items, -1))
        }
        "first" => match &*j.borrow() {
            J::Arr(x) => x.first().cloned().map(jtok).unwrap_or(Val::Void),
            _ => Val::Void,
        },
        "haskeys" | "hasvalues" => Val::Bool(match &*j.borrow() {
            J::Obj(o) => !o.is_empty(),
            J::Arr(x) => !x.is_empty(),
            _ => false,
        }),
        _ => return Ok(Out::Unknown),
    };
    ok(r)
}

// ------------------------------------------------------------ JsonTextReader

pub const JSON_TEXT_READER: &str = "Newtonsoft.Json.JsonTextReader";

/// One token as Newtonsoft's JsonTextReader reports it.
pub struct JTokRead {
    /// JsonToken: StartObject 1, StartArray 2, PropertyName 4, Integer 7, Float 8, String 9,
    /// Boolean 10, Null 11, EndObject 13, EndArray 14, Date 16.
    pub kind: i32,
    pub depth: i32,
    pub path: String,
    /// Val::Void for no value (containers, null).
    pub value: Val,
    pub value_type: Option<&'static str>,
}

/// A JsonTextReader over a whole document: its tokens up to the first syntax error, which Read
/// raises when it gets there (as Newtonsoft does, so the tokens before it are still read).
pub struct JReader {
    pub toks: Vec<JTokRead>,
    pub err: Option<String>,
    /// The current token (None before the first Read).
    pub cur: Option<usize>,
}

enum Ctr {
    Obj(Option<String>),
    Arr(i64),
}

/// JsonPosition.BuildPath: `a.b[0].c`, names with special characters as `['a b']`.
fn reader_path(st: &[Ctr]) -> String {
    const SPECIAL: &[char] = &['.', ' ', '\'', '/', '"', '[', ']', '(', ')', '\t', '\n', '\r', '\u{c}', '\u{8}', '\\', '\u{85}', '\u{2028}', '\u{2029}'];
    let mut p = String::new();
    for c in st {
        match c {
            Ctr::Obj(Some(n)) if n.contains(SPECIAL) => {
                p.push_str("['");
                p.push_str(&n.replace('\\', "\\\\").replace('\'', "\\'"));
                p.push_str("']");
            }
            Ctr::Obj(Some(n)) => {
                if !p.is_empty() {
                    p.push('.');
                }
                p.push_str(n);
            }
            Ctr::Arr(i) if *i >= 0 => p.push_str(&format!("[{i}]")),
            _ => {}
        }
    }
    p
}

/// DateParseHandling.DateTime: an ISO 8601 `yyyy-MM-ddTHH:mm:ss[.fffffff][Z|±HH[:mm]]` string is a
/// Date token (Newtonsoft's DateTimeParser needs the seconds; a bare date stays a String).
fn reader_date(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| r.into_iter().all(|i| b.get(i).is_some_and(|c| c.is_ascii_digit()));
    if b.len() < 19 || !digits(0..4) || b[4] != b'-' || !digits(5..7) || b[7] != b'-' || !digits(8..10) || b[10] != b'T' || !digits(11..13) || b[13] != b':' || !digits(14..16) || b[16] != b':' || !digits(17..19) {
        return None;
    }
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        let st = i + 1;
        i = st;
        while b.get(i).is_some_and(|c| c.is_ascii_digit()) && i - st < 7 {
            i += 1;
        }
        if i == st {
            return None;
        }
    }
    match b.get(i) {
        None => {}
        Some(b'Z') if i + 1 == b.len() => {}
        Some(b'+' | b'-') if matches!(b.len() - i - 1, 2 | 4 | 5) => {}
        _ => return None,
    }
    super::datetime::parse_iso(s).map(|p| p.datetime().to_al())
}

/// Tokenize a JSON document the way JsonTextReader reads it (unquoted and single-quoted property
/// names allowed, one top-level value).
pub fn reader_tokens(s: &str) -> (Vec<JTokRead>, Option<String>) {
    #[derive(PartialEq, Clone, Copy)]
    enum Exp {
        Value,
        ValueOrEnd,
        Prop,
        PropOrEnd,
        CommaOrEnd,
        Done,
    }
    let c: Vec<char> = s.chars().collect();
    let (mut i, mut exp) = (0usize, Exp::Value);
    let mut st: Vec<Ctr> = vec![];
    let mut out: Vec<JTokRead> = vec![];
    macro_rules! fail {
        ($($t:tt)*) => {{
            let msg = format!($($t)*);
            return (out, Some(format!("{msg} Path '{}', line 1, position {}.", reader_path(&st), i)));
        }};
    }
    let push = |out: &mut Vec<JTokRead>, st: &[Ctr], kind: i32, value: Val, vt: Option<&'static str>| {
        out.push(JTokRead { kind, depth: st.len() as i32, path: reader_path(st), value, value_type: vt });
    };
    let read_str = |c: &[char], i: &mut usize| -> Option<String> {
        let q = c[*i];
        *i += 1;
        let mut s = String::new();
        while *i < c.len() {
            let ch = c[*i];
            *i += 1;
            if ch == q {
                return Some(s);
            }
            if ch != '\\' {
                s.push(ch);
                continue;
            }
            let e = *c.get(*i)?;
            *i += 1;
            match e {
                'n' => s.push('\n'),
                't' => s.push('\t'),
                'r' => s.push('\r'),
                'b' => s.push('\u{8}'),
                'f' => s.push('\u{c}'),
                'u' => {
                    let h: String = c.get(*i..*i + 4)?.iter().collect();
                    *i += 4;
                    s.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?).unwrap_or('\u{FFFD}'));
                }
                other => s.push(other),
            }
        }
        None
    };
    loop {
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        if i >= c.len() {
            if exp == Exp::Done || (st.is_empty() && out.is_empty()) {
                return (out, None);
            }
            fail!("Unexpected end when reading JSON.");
        }
        let ch = c[i];
        let after = |st: &[Ctr]| if st.is_empty() { Exp::Done } else { Exp::CommaOrEnd };
        match exp {
            Exp::Done => fail!("Additional text encountered after finished reading JSON content: {ch}."),
            Exp::CommaOrEnd => match (ch, st.last()) {
                (',', Some(Ctr::Obj(_))) => {
                    i += 1;
                    exp = Exp::Prop;
                }
                (',', _) => {
                    i += 1;
                    exp = Exp::Value;
                }
                ('}', Some(Ctr::Obj(_))) | (']', Some(Ctr::Arr(_))) => {
                    st.pop();
                    push(&mut out, &st, if ch == '}' { 13 } else { 14 }, Val::Void, None);
                    i += 1;
                    exp = after(&st);
                }
                _ => fail!("After parsing a value an unexpected character was encountered: {ch}."),
            },
            Exp::Prop | Exp::PropOrEnd => {
                if ch == '}' && exp == Exp::PropOrEnd {
                    st.pop();
                    push(&mut out, &st, 13, Val::Void, None);
                    i += 1;
                    exp = after(&st);
                    continue;
                }
                let name = if ch == '"' || ch == '\'' {
                    match read_str(&c, &mut i) {
                        Some(n) => n,
                        None => fail!("Unterminated string. Expected delimiter: {ch}."),
                    }
                } else if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                    let st0 = i;
                    while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '$') {
                        i += 1;
                    }
                    c[st0..i].iter().collect()
                } else {
                    fail!("Invalid property identifier character: {ch}.");
                };
                if let Some(Ctr::Obj(n)) = st.last_mut() {
                    *n = Some(name.clone());
                }
                push(&mut out, &st, 4, Val::Text(name), Some("System.String"));
                while i < c.len() && c[i].is_whitespace() {
                    i += 1;
                }
                match c.get(i) {
                    Some(':') => i += 1,
                    Some(x) => {
                        let x = *x;
                        fail!("Invalid character after parsing property name. Expected ':' but got: {x}.")
                    }
                    None => fail!("Unexpected end when reading JSON."),
                }
                exp = Exp::Value;
            }
            Exp::Value | Exp::ValueOrEnd => {
                if ch == ']' && exp == Exp::ValueOrEnd {
                    st.pop();
                    push(&mut out, &st, 14, Val::Void, None);
                    i += 1;
                    exp = after(&st);
                    continue;
                }
                if let Some(Ctr::Arr(n)) = st.last_mut() {
                    *n += 1;
                }
                match ch {
                    '{' => {
                        push(&mut out, &st, 1, Val::Void, None);
                        st.push(Ctr::Obj(None));
                        i += 1;
                        exp = Exp::PropOrEnd;
                    }
                    '[' => {
                        push(&mut out, &st, 2, Val::Void, None);
                        st.push(Ctr::Arr(-1));
                        i += 1;
                        exp = Exp::ValueOrEnd;
                    }
                    '"' | '\'' => {
                        let Some(v) = read_str(&c, &mut i) else { fail!("Unterminated string. Expected delimiter: {ch}.") };
                        match reader_date(&v) {
                            Some(ms) => push(&mut out, &st, 16, Val::DateTime(ms), Some("System.DateTime")),
                            None => push(&mut out, &st, 9, Val::Text(v), Some("System.String")),
                        }
                        exp = after(&st);
                    }
                    '-' | '0'..='9' => {
                        let st0 = i;
                        while i < c.len() && !c[i].is_whitespace() && !matches!(c[i], ',' | ']' | '}') {
                            i += 1;
                        }
                        let t: String = c[st0..i].iter().collect();
                        if t.contains(['.', 'e', 'E']) {
                            let d = t.parse::<rust_decimal::Decimal>().ok().or_else(|| rust_decimal::Decimal::from_scientific(&t).ok());
                            let Some(d) = d.filter(|_| t.parse::<f64>().is_ok()) else { fail!("Input string '{t}' is not a valid decimal.") };
                            push(&mut out, &st, 8, Val::Dec(d.normalize()), Some("System.Double"));
                        } else {
                            let Ok(n) = t.parse::<i64>() else { fail!("Input string '{t}' is not a valid integer.") };
                            push(&mut out, &st, 7, Val::BigInt(n), Some("System.Int64"));
                        }
                        exp = after(&st);
                    }
                    _ if c[i..].starts_with(&['t', 'r', 'u', 'e']) => {
                        i += 4;
                        push(&mut out, &st, 10, Val::Bool(true), Some("System.Boolean"));
                        exp = after(&st);
                    }
                    _ if c[i..].starts_with(&['f', 'a', 'l', 's', 'e']) => {
                        i += 5;
                        push(&mut out, &st, 10, Val::Bool(false), Some("System.Boolean"));
                        exp = after(&st);
                    }
                    _ if c[i..].starts_with(&['n', 'u', 'l', 'l']) => {
                        i += 4;
                        push(&mut out, &st, 11, Val::Void, None);
                        exp = after(&st);
                    }
                    _ => fail!("Unexpected character encountered while parsing value: {ch}."),
                }
            }
        }
    }
}

/// JsonTextReader members.
fn reader_call(r: &mut JReader, m: &str) -> R<Out> {
    fn tok(r: &JReader) -> Option<&JTokRead> {
        r.cur.and_then(|i| r.toks.get(i))
    }
    match m {
        "read" => {
            let next = r.cur.map(|i| i + 1).unwrap_or(0);
            if next < r.toks.len() {
                r.cur = Some(next);
                return ok(Val::Bool(true));
            }
            r.cur = Some(r.toks.len());
            match r.err.take() {
                Some(e) => net_err("JsonTextReader", "Read", e),
                None => ok(Val::Bool(false)),
            }
        }
        "tokentype" => ok(Val::Int(tok(r).map(|t| t.kind).unwrap_or(0))),
        "depth" => ok(Val::Int(tok(r).map(|t| t.depth).unwrap_or(0))),
        "path" => ok(Val::Text(tok(r).map(|t| t.path.clone()).unwrap_or_default())),
        "value" => ok(match tok(r).map(|t| t.value.clone()) {
            Some(Val::Void) | None => net(NetObj::Null("System.Object")),
            Some(v) => v,
        }),
        "valuetype" => ok(match tok(r).and_then(|t| t.value_type) {
            Some(t) => net(NetObj::Prim("System.Type", Val::Text(t.to_string()))),
            None => net(NetObj::Null("System.Type")),
        }),
        "close" | "dispose" => void(),
        _ => Ok(Out::Unknown),
    }
}
