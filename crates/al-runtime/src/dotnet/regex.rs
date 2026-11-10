//! System.Text.RegularExpressions on fancy-regex (backtracking: lookaround and backreferences work).
//!
//! .NET specifics handled here: RegexOptions → inline flags, `(?'name')` groups, `\Z`, literal
//! `{`, .NET group numbering (unnamed groups first, then named ones), replacement patterns
//! (`$1`, `${name}`, `$&`, `` $` ``, `$'`, `$+`, `$_`, `$$`), Split including captured groups,
//! character indices instead of byte offsets.

use super::*;

pub struct NetRegex {
    pub pattern: String,
    pub options: i64,
    re: fancy_regex::Regex,
    /// .NET group number → (rust group index, name).
    groups: Vec<(usize, String)>,
}

pub struct GroupData {
    pub name: String,
    pub success: bool,
    /// Character index and length.
    pub index: i32,
    pub length: i32,
    pub value: String,
}

pub struct MatchData {
    pub groups: Vec<GroupData>,
    pub regex: Rc<NetRegex>,
    pub input: Rc<str>,
    /// Byte offset where NextMatch continues.
    next: usize,
}

const IGNORE_CASE: i64 = 1;
const MULTILINE: i64 = 2;
const EXPLICIT_CAPTURE: i64 = 4;
const SINGLELINE: i64 = 16;
const IGNORE_WS: i64 = 32;

/// Translate .NET pattern syntax to fancy-regex syntax.
fn translate(p: &str) -> String {
    let c: Vec<char> = p.chars().collect();
    let mut out = String::with_capacity(p.len() + 8);
    let mut i = 0;
    let mut in_class = false;
    while i < c.len() {
        let ch = c[i];
        if ch == '\\' && i + 1 < c.len() {
            let n = c[i + 1];
            if !in_class && n == 'Z' {
                out.push_str("(?=\\n?\\z)");
            } else {
                out.push(ch);
                out.push(n);
            }
            i += 2;
            continue;
        }
        if in_class {
            if ch == ']' {
                in_class = false;
            }
            if ch == '[' {
                out.push('\\');
            }
            out.push(ch);
            i += 1;
            continue;
        }
        match ch {
            '[' => {
                in_class = true;
                out.push(ch);
                // a leading ']' or '^]' is literal
                if c.get(i + 1) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                if c.get(i + 1) == Some(&']') {
                    out.push_str("\\]");
                    i += 1;
                }
            }
            '(' if c.get(i + 1) == Some(&'?') && c.get(i + 2) == Some(&'\'') => {
                // (?'name'...) → (?<name>...)
                let mut j = i + 3;
                let mut name = String::new();
                while j < c.len() && c[j] != '\'' {
                    name.push(c[j]);
                    j += 1;
                }
                out.push_str(&format!("(?<{name}>"));
                i = j + 1;
                continue;
            }
            '{' => {
                // a quantifier needs digits: {n}, {n,}, {n,m}; anything else is a literal brace in .NET
                let mut j = i + 1;
                let mut ok = false;
                if j < c.len() && c[j].is_ascii_digit() {
                    while j < c.len() && c[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j < c.len() && c[j] == ',' {
                        j += 1;
                        while j < c.len() && c[j].is_ascii_digit() {
                            j += 1;
                        }
                    }
                    ok = j < c.len() && c[j] == '}';
                }
                if ok && !out.is_empty() {
                    out.push('{');
                } else {
                    out.push_str("\\{");
                }
            }
            '}' => {
                // literal unless closing a quantifier we kept
                let opened = out.rfind('{').map(|k| !out[..k].ends_with('\\') && !out[k..].contains('}')).unwrap_or(false);
                if opened {
                    out.push('}');
                } else {
                    out.push_str("\\}");
                }
            }
            _ => out.push(ch),
        }
        i += 1;
    }
    out
}

pub fn compile(pattern: &str, options: i64) -> Result<NetRegex, String> {
    let mut flags = String::new();
    if options & IGNORE_CASE != 0 {
        flags.push('i');
    }
    if options & MULTILINE != 0 {
        flags.push('m');
    }
    if options & SINGLELINE != 0 {
        flags.push('s');
    }
    if options & IGNORE_WS != 0 {
        flags.push('x');
    }
    let mut body = translate(pattern);
    if options & 256 != 0 {
        // ECMAScript: \w, \d and \s are ASCII-only
        body = body.replace("\\w", "[A-Za-z0-9_]").replace("\\W", "[^A-Za-z0-9_]").replace("\\d", "[0-9]").replace("\\D", "[^0-9]");
    }
    let full = if flags.is_empty() { body } else { format!("(?{flags}){body}") };
    let re = fancy_regex::RegexBuilder::new(&full).backtrack_limit(10_000_000).build().map_err(|e| format!("Invalid pattern '{pattern}'. {e}"))?;
    // .NET numbering: 0, unnamed groups left to right, then named groups left to right
    let names: Vec<Option<String>> = re.capture_names().map(|n| n.map(String::from)).collect();
    let mut groups = vec![(0usize, "0".to_string())];
    let mut num = 1;
    if options & EXPLICIT_CAPTURE == 0 {
        for (i, n) in names.iter().enumerate().skip(1) {
            if n.is_none() {
                groups.push((i, num.to_string()));
                num += 1;
            }
        }
    }
    for (i, n) in names.iter().enumerate().skip(1) {
        if let Some(n) = n {
            if n.chars().all(|c| c.is_ascii_digit()) {
                groups.push((i, n.clone()));
            } else {
                groups.push((i, n.clone()));
            }
        }
    }
    Ok(NetRegex { pattern: pattern.to_string(), options, re, groups })
}

fn char_idx(s: &str, byte: usize) -> i32 {
    s[..byte.min(s.len())].chars().count() as i32
}

fn byte_idx(s: &str, ch: usize) -> usize {
    s.char_indices().nth(ch).map(|x| x.0).unwrap_or(s.len())
}

impl NetRegex {
    /// The match at or after byte `pos` (None = no match).
    fn find_at(self: &Rc<Self>, input: &Rc<str>, pos: usize) -> Result<Option<MatchData>, String> {
        if pos > input.len() {
            return Ok(None);
        }
        let caps = self.re.captures_from_pos(input, pos).map_err(|e| format!("The RegEx engine has timed out while trying to match a pattern to an input string. {e}"))?;
        let Some(caps) = caps else { return Ok(None) };
        let m0 = caps.get(0).unwrap();
        let groups = self
            .groups
            .iter()
            .map(|(ri, name)| match caps.get(*ri) {
                Some(g) => GroupData { name: name.clone(), success: true, index: char_idx(input, g.start()), length: g.as_str().chars().count() as i32, value: g.as_str().to_string() },
                None => GroupData { name: name.clone(), success: false, index: 0, length: 0, value: String::new() },
            })
            .collect();
        let next = if m0.end() == m0.start() { m0.end() + input[m0.end()..].chars().next().map(|c| c.len_utf8()).unwrap_or(1) } else { m0.end() };
        Ok(Some(MatchData { groups, regex: self.clone(), input: input.clone(), next }))
    }

    fn empty_match(self: &Rc<Self>, input: &Rc<str>) -> MatchData {
        MatchData {
            groups: self.groups.iter().map(|(_, n)| GroupData { name: n.clone(), success: false, index: 0, length: 0, value: String::new() }).collect(),
            regex: self.clone(),
            input: input.clone(),
            next: input.len() + 1,
        }
    }

    fn all(self: &Rc<Self>, input: &Rc<str>, start: usize, limit: Option<usize>) -> Result<Vec<Rc<MatchData>>, String> {
        let mut out = vec![];
        let mut pos = start;
        while limit.is_none_or(|l| out.len() < l) {
            match self.find_at(input, pos)? {
                Some(m) => {
                    pos = m.next;
                    out.push(Rc::new(m));
                }
                None => break,
            }
        }
        Ok(out)
    }

    fn group_number(&self, name: &str) -> i32 {
        self.groups.iter().position(|(_, n)| n == name).map(|i| if name.chars().all(|c| c.is_ascii_digit()) { name.parse().unwrap_or(i as i32) } else { i as i32 }).unwrap_or(-1)
    }
}

/// Expand a .NET replacement pattern for one match.
pub fn expand(m: &MatchData, rep: &str) -> String {
    let c: Vec<char> = rep.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let group = |k: &str| -> Option<&GroupData> { m.groups.iter().find(|g| g.name == k) };
    let m0 = &m.groups[0];
    while i < c.len() {
        if c[i] != '$' || i + 1 >= c.len() {
            out.push(c[i]);
            i += 1;
            continue;
        }
        let n = c[i + 1];
        match n {
            '$' => {
                out.push('$');
                i += 2;
            }
            '&' => {
                out.push_str(&m0.value);
                i += 2;
            }
            '`' => {
                let b = byte_idx(&m.input, m0.index as usize);
                out.push_str(&m.input[..b]);
                i += 2;
            }
            '\'' => {
                let b = byte_idx(&m.input, (m0.index + m0.length) as usize);
                out.push_str(&m.input[b..]);
                i += 2;
            }
            '+' => {
                if let Some(g) = m.groups.iter().skip(1).last() {
                    out.push_str(&g.value);
                }
                i += 2;
            }
            '_' => {
                out.push_str(&m.input);
                i += 2;
            }
            '{' => {
                let mut j = i + 2;
                let mut name = String::new();
                while j < c.len() && c[j] != '}' {
                    name.push(c[j]);
                    j += 1;
                }
                match (j < c.len(), group(&name)) {
                    (true, Some(g)) => {
                        out.push_str(&g.value);
                        i = j + 1;
                    }
                    _ => {
                        out.push('$');
                        i += 1;
                    }
                }
            }
            d if d.is_ascii_digit() => {
                // the longest digit run that names an existing group
                let mut j = i + 1;
                while j < c.len() && c[j].is_ascii_digit() {
                    j += 1;
                }
                let mut found = false;
                while j > i + 1 {
                    let num: String = c[i + 1..j].iter().collect();
                    let key = num.parse::<usize>().map(|x| x.to_string()).unwrap_or_default();
                    if let Some(g) = group(&key) {
                        out.push_str(&g.value);
                        i = j;
                        found = true;
                        break;
                    }
                    j -= 1;
                }
                if !found {
                    out.push('$');
                    i += 1;
                }
            }
            _ => {
                out.push('$');
                i += 1;
            }
        }
    }
    out
}

fn options_of(v: Option<&Val>) -> i64 {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Prim(_, x) => x.as_i64(),
            _ => 0,
        },
        Some(Val::Int(i)) => *i as i64,
        Some(Val::Opt { ord, .. }) => *ord as i64,
        _ => 0,
    }
}

pub fn construct(a: &Args) -> R<Val> {
    match compile(&a.text(0), options_of(a.raw(1))) {
        Ok(r) => Ok(net(NetObj::Regex(Rc::new(r)))),
        Err(e) => net_err("Regex", "Regex", e),
    }
}

fn wrap<T>(r: Result<T, String>) -> R<T> {
    r.or_else(|e| net_err("Regex", "Match", e))
}

fn replace(re: &Rc<NetRegex>, input: &str, rep: &str, count: Option<usize>, start: usize) -> R<String> {
    let inp: Rc<str> = input.into();
    let ms = wrap(re.all(&inp, byte_idx(input, start), count))?;
    let mut out = String::new();
    let mut last = 0;
    for m in ms {
        let g = &m.groups[0];
        let (b0, b1) = (byte_idx(input, g.index as usize), byte_idx(input, (g.index + g.length) as usize));
        out.push_str(&input[last..b0]);
        out.push_str(&expand(&m, rep));
        last = b1;
    }
    out.push_str(&input[last..]);
    Ok(out)
}

fn split(re: &Rc<NetRegex>, input: &str, count: Option<usize>, start: usize) -> R<Vec<String>> {
    let inp: Rc<str> = input.into();
    let limit = count.filter(|c| *c > 0).map(|c| c - 1);
    let ms = wrap(re.all(&inp, byte_idx(input, start), limit))?;
    let mut out = vec![];
    let mut last = 0;
    for m in ms {
        let g = &m.groups[0];
        let (b0, b1) = (byte_idx(input, g.index as usize), byte_idx(input, (g.index + g.length) as usize));
        out.push(input[last..b0].to_string());
        for cg in m.groups.iter().skip(1).filter(|x| x.success) {
            out.push(cg.value.clone());
        }
        last = b1;
    }
    out.push(input[last..].to_string());
    Ok(out)
}

pub fn escape(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '\\' | '*' | '+' | '?' | '|' | '{' | '[' | '(' | ')' | '^' | '$' | '.' | '#' | ' ' => {
                out.push('\\');
                out.push(ch);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\x0c' => out.push_str("\\f"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn unescape(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '\\' && i + 1 < c.len() {
            out.push(match c[i + 1] {
                't' => '\t',
                'n' => '\n',
                'r' => '\r',
                'f' => '\x0c',
                'e' => '\x1b',
                'a' => '\x07',
                x => x,
            });
            i += 2;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

pub fn captures_of(m: &Rc<MatchData>, g: usize) -> Vec<Val> {
    let gd = &m.groups[g];
    if gd.success {
        vec![net(NetObj::Capture(gd.index, gd.length, gd.value.clone()))]
    } else {
        vec![]
    }
}

thread_local! {
    static CACHE_SIZE: std::cell::Cell<i32> = const { std::cell::Cell::new(15) };
}

pub fn call(s: &mut Session, f: &mut Frame, n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    let _ = (s, f);
    if ty == "System.Text.RegularExpressions.RegexOptions" {
        return ok(match super::text::enum_names(ty).and_then(|names| names.iter().find(|(x, _)| x.eq_ignore_ascii_case(m)).map(|(_, v)| *v)) {
            Some(v) => enum_val(ty, v),
            None => return Ok(Out::Unknown),
        });
    }
    let o = n.borrow();
    match &*o {
        NetObj::Null(_) if ty == "System.Text.RegularExpressions.Regex" => {
            drop(o);
            // static members: (input, pattern[, ...][, options])
            match m {
                "escape" => return ok(Val::Text(escape(&a.text(0)))),
                "unescape" => return ok(Val::Text(unescape(&a.text(0)))),
                "cachesize" => {
                    if a.len() == 1 {
                        CACHE_SIZE.with(|c| c.set(a.int(0) as i32));
                        return void();
                    }
                    return ok(Val::Int(CACHE_SIZE.with(|c| c.get())));
                }
                "ismatch" | "match" | "matches" | "replace" | "split" => {
                    let opt_i = if m == "replace" { 3 } else { 2 };
                    let re = match compile(&a.text(1), options_of(a.raw(opt_i))) {
                        Ok(r) => Rc::new(r),
                        Err(e) => return net_err("Regex", m, e),
                    };
                    let input = a.text(0);
                    return regex_op(&re, m, &input, a, true);
                }
                _ => return Ok(Out::Unknown),
            }
        }
        NetObj::Regex(re) => {
            let re = re.clone();
            drop(o);
            match m {
                "ismatch" | "match" | "matches" | "replace" | "split" => {
                    let input = a.text(0);
                    // a static overload through the instance: a pattern string where the instance
                    // overload has an Int32 (Replace: the third argument is the replacement)
                    let text_at = if m == "replace" { 2 } else { 1 };
                    if matches!(a.raw(text_at), Some(Val::Text(_))) {
                        let opt_i = if m == "replace" { 3 } else { 2 };
                        let st = match compile(&a.text(1), options_of(a.raw(opt_i))) {
                            Ok(r) => Rc::new(r),
                            Err(e) => return net_err("Regex", m, e),
                        };
                        return regex_op(&st, m, &input, a, true);
                    }
                    regex_op(&re, m, &input, a, false)
                }
                "escape" => ok(Val::Text(escape(&a.text(0)))),
                "unescape" => ok(Val::Text(unescape(&a.text(0)))),
                "cachesize" => ok(Val::Int(CACHE_SIZE.with(|c| c.get()))),
                "getgroupnames" => ok(net(NetObj::Array("System.String", re.groups.iter().map(|(_, n)| Val::Text(n.clone())).collect()))),
                "getgroupnumbers" => ok(net(NetObj::Array(
                    "System.Int32",
                    re.groups.iter().enumerate().map(|(i, (_, n))| Val::Int(if n.chars().all(|c| c.is_ascii_digit()) { n.parse().unwrap_or(i as i32) } else { i as i32 })).collect(),
                ))),
                "groupnamefromnumber" => {
                    let k = a.int(0);
                    ok(Val::Text(re.groups.get(k as usize).map(|(_, n)| n.clone()).unwrap_or_default()))
                }
                "groupnumberfromname" => ok(Val::Int(re.group_number(&a.text(0)))),
                "tostring" => ok(Val::Text(re.pattern.clone())),
                "options" => ok(enum_val("System.Text.RegularExpressions.RegexOptions", re.options)),
                "righttoleft" => ok(Val::Bool(re.options & 64 != 0)),
                "matchtimeout" => ok(datetime::timespan(-10_000)),
                _ => Ok(Out::Unknown),
            }
        }
        NetObj::MatchCollection(ms) => {
            let ms = ms.clone();
            drop(o);
            ok(match m {
                "count" => Val::Int(ms.len() as i32),
                "item" | "get_item" => match ms.get(a.int(0) as usize) {
                    Some(x) => net(NetObj::Match(x.clone())),
                    None => return net_err("MatchCollection", "Item", "Specified argument was out of the range of valid values."),
                },
                "getenumerator" => net(NetObj::Enumerator(ms.iter().map(|x| net(NetObj::Match(x.clone()))).collect(), -1)),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::Match(md) => {
            let md = md.clone();
            drop(o);
            match m {
                "groups" => ok(net(NetObj::GroupCollection(md.clone()))),
                "captures" => ok(net(NetObj::CaptureCollection(md.clone(), 0))),
                "nextmatch" => {
                    let next = wrap(md.regex.find_at(&md.input, md.next))?;
                    ok(net(NetObj::Match(Rc::new(next.unwrap_or_else(|| md.regex.empty_match(&md.input))))))
                }
                "result" => ok(Val::Text(expand(&md, &a.text(0)))),
                _ => group_member(&md, 0, m),
            }
        }
        NetObj::GroupCollection(md) => {
            let md = md.clone();
            drop(o);
            ok(match m {
                "count" => Val::Int(md.groups.len() as i32),
                "item" | "get_item" => {
                    let gi = match a.get(0) {
                        Val::Int(i) => Some(i as usize).filter(|i| *i < md.groups.len()),
                        Val::BigInt(i) => Some(i as usize).filter(|i| *i < md.groups.len()),
                        v => md.groups.iter().position(|g| g.name == v.to_text()),
                    };
                    match gi {
                        Some(g) => net(NetObj::Group(md.clone(), g)),
                        // .NET returns an unsuccessful Group for unknown names
                        None => net(NetObj::Capture(0, 0, String::new())),
                    }
                }
                "keys" => net(NetObj::Array("System.String", md.groups.iter().map(|g| Val::Text(g.name.clone())).collect())),
                "getenumerator" => net(NetObj::Enumerator((0..md.groups.len()).map(|g| net(NetObj::Group(md.clone(), g))).collect(), -1)),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::Group(md, g) => {
            let (md, g) = (md.clone(), *g);
            drop(o);
            match m {
                "captures" => ok(net(NetObj::CaptureCollection(md, g))),
                _ => group_member(&md, g, m),
            }
        }
        NetObj::CaptureCollection(md, g) => {
            let caps = captures_of(md, *g);
            drop(o);
            ok(match m {
                "count" => Val::Int(caps.len() as i32),
                "item" | "get_item" => match caps.get(a.int(0) as usize) {
                    Some(x) => x.clone(),
                    None => return net_err("CaptureCollection", "Item", "Specified argument was out of the range of valid values."),
                },
                "getenumerator" => net(NetObj::Enumerator(caps, -1)),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::Capture(i, l, v) => {
            let r = match m {
                "value" | "tostring" => Val::Text(v.clone()),
                "index" => Val::Int(*i),
                "length" => Val::Int(*l),
                "success" => Val::Bool(*l > 0 || !v.is_empty()),
                "name" => Val::Text(String::new()),
                _ => return Ok(Out::Unknown),
            };
            ok(r)
        }
        _ => Ok(Out::Unknown),
    }
}

fn group_member(md: &MatchData, g: usize, m: &str) -> R<Out> {
    let gd = &md.groups[g];
    ok(match m {
        "value" | "tostring" => Val::Text(gd.value.clone()),
        "index" => Val::Int(gd.index),
        "length" => Val::Int(gd.length),
        "success" => Val::Bool(gd.success),
        "name" => Val::Text(gd.name.clone()),
        _ => return Ok(Out::Unknown),
    })
}

/// IsMatch / Match / Matches / Replace / Split. For static calls the pattern is argument 1.
fn regex_op(re: &Rc<NetRegex>, m: &str, input: &str, a: &Args, is_static: bool) -> R<Out> {
    let inp: Rc<str> = input.into();
    let int_arg = |i: usize| -> Option<i64> {
        match a.raw(i) {
            Some(Val::Int(_) | Val::BigInt(_)) => Some(a.int(i)),
            _ => None,
        }
    };
    match m {
        "ismatch" => {
            let start = if is_static { 0 } else { int_arg(1).unwrap_or(0) as usize };
            ok(Val::Bool(wrap(re.find_at(&inp, byte_idx(input, start)))?.is_some()))
        }
        "match" => {
            let start = if is_static { 0 } else { int_arg(1).unwrap_or(0) as usize };
            // Match(input, beginning, length): search inside a substring
            let found = if !is_static && a.len() == 3 {
                let (b, l) = (a.int(1).max(0) as usize, a.int(2).max(0) as usize);
                let sub: String = input.chars().skip(b).take(l).collect();
                let subrc: Rc<str> = sub.into();
                wrap(re.find_at(&subrc, 0))?.map(|mut md| {
                    for gd in md.groups.iter_mut() {
                        gd.index += b as i32;
                    }
                    md.input = inp.clone();
                    md
                })
            } else {
                wrap(re.find_at(&inp, byte_idx(input, start)))?
            };
            ok(net(NetObj::Match(Rc::new(found.unwrap_or_else(|| re.empty_match(&inp))))))
        }
        "matches" => {
            let start = if is_static { 0 } else { int_arg(1).unwrap_or(0) as usize };
            ok(net(NetObj::MatchCollection(Rc::new(wrap(re.all(&inp, byte_idx(input, start), None))?))))
        }
        "replace" => {
            let rep = if is_static { a.text(2) } else { a.text(1) };
            let (count, start) = if is_static { (None, 0) } else { (int_arg(2).filter(|c| *c >= 0).map(|c| c as usize), int_arg(3).unwrap_or(0) as usize) };
            ok(Val::Text(replace(re, input, &rep, count, start)?))
        }
        "split" => {
            let (count, start) = if is_static { (None, 0) } else { (int_arg(1).map(|c| c as usize), int_arg(2).unwrap_or(0) as usize) };
            let parts = split(re, input, count, start)?;
            ok(net(NetObj::Array("System.String", parts.into_iter().map(Val::Text).collect())))
        }
        _ => Ok(Out::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rx(p: &str) -> Rc<NetRegex> {
        Rc::new(compile(p, 0).unwrap())
    }

    #[test]
    fn numbering_and_replace() {
        let re = rx(r"(?<y>\d{4})-(\d\d)");
        assert_eq!(re.groups.iter().map(|g| g.1.as_str()).collect::<Vec<_>>(), vec!["0", "1", "y"]);
        assert_eq!(replace(&re, "on 2024-05 and 2023-01", "$1/${y}", None, 0).ok().unwrap(), "on 05/2024 and 01/2023");
        assert_eq!(replace(&rx("a"), "banana", "$$", None, 0).ok().unwrap(), "b$n$n$");
    }

    #[test]
    fn split_keeps_groups() {
        assert_eq!(split(&rx("(-)"), "a-b", None, 0).ok().unwrap(), vec!["a", "-", "b"]);
        assert_eq!(split(&rx(","), "a,b,c", Some(2), 0).ok().unwrap(), vec!["a", "b,c"]);
    }

    #[test]
    fn literal_braces() {
        assert!(compile("{abc}", 0).is_ok());
        assert!(rx(r"^[a-zA-Z]*$").re.is_match("Abc").unwrap());
    }
}
