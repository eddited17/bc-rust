//! System.Uri, UriBuilder, UriKind/UriPartial, System.Web.HttpUtility and System.Net.WebUtility.

use super::*;

#[derive(Clone, Debug, Default)]
pub struct NetUri {
    pub original: String,
    pub absolute: bool,
    pub scheme: String,
    pub user_info: String,
    pub host: String,
    /// Explicit port, or None for the scheme default.
    pub port: Option<i32>,
    /// Escaped path ("/" for hierarchical URIs without one).
    pub path: String,
    /// Query including '?', or empty.
    pub query: String,
    /// Fragment including '#', or empty.
    pub fragment: String,
    /// Relative URIs keep their text.
    pub relative: String,
}

pub fn default_port(scheme: &str) -> i32 {
    match scheme {
        "http" | "ws" => 80,
        "https" | "wss" => 443,
        "ftp" => 21,
        "sftp" | "ssh" => 22,
        "ldap" => 389,
        "mailto" => 25,
        "news" | "nntp" => 119,
        "gopher" => 70,
        "telnet" => 23,
        _ => -1,
    }
}

const UNRESERVED: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.~";

/// Uri.EscapeDataString: everything but RFC 3986 unreserved characters, upper-case hex.
pub fn escape_data(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if UNRESERVED.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Escape characters that are not allowed in a URI component (keeps reserved chars and %XX).
fn escape_unsafe(s: &str, keep: &[u8]) -> String {
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let x = b[i];
        let is_hex = |k: usize| b.get(k).is_some_and(|c| c.is_ascii_hexdigit());
        if x == b'%' && is_hex(i + 1) && is_hex(i + 2) {
            out.push('%');
            out.push((b[i + 1] as char).to_ascii_uppercase());
            out.push((b[i + 2] as char).to_ascii_uppercase());
            i += 3;
            continue;
        }
        if UNRESERVED.contains(&x) || keep.contains(&x) {
            out.push(x as char);
        } else {
            out.push_str(&format!("%{x:02X}"));
        }
        i += 1;
    }
    out
}

const SUBDELIMS: &[u8] = b"!$&'()*+,;=:@/";

/// Uri.UnescapeDataString.
pub fn unescape_data(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out.clone()).unwrap_or_else(|_| String::from_utf8_lossy(&out).into_owned())
}

fn remove_dot_segments(path: &str) -> String {
    let mut out: Vec<&str> = vec![];
    let segs: Vec<&str> = path.split('/').collect();
    for (i, seg) in segs.iter().enumerate() {
        let last = i + 1 == segs.len();
        match *seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let r = out.join("/");
    if path.starts_with('/') && !r.starts_with('/') {
        format!("/{r}")
    } else {
        r
    }
}

impl NetUri {
    /// Parse an absolute URI. Errors use .NET's UriFormatException messages.
    pub fn parse_absolute(s: &str) -> Result<NetUri, String> {
        let t = s.trim();
        let colon = t.find(':').ok_or("Invalid URI: The format of the URI could not be determined.")?;
        let scheme = &t[..colon];
        if scheme.is_empty() || !scheme.chars().next().unwrap().is_ascii_alphabetic() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
            return Err("Invalid URI: The format of the URI could not be determined.".into());
        }
        if scheme.len() == 1 {
            // c:\path → file URI
            let p = t.replace('\\', "/");
            return Ok(NetUri { original: s.into(), absolute: true, scheme: "file".into(), path: format!("/{}", escape_unsafe(&p, SUBDELIMS)), ..Default::default() });
        }
        let scheme = scheme.to_ascii_lowercase();
        let mut rest = &t[colon + 1..];
        let mut u = NetUri { original: s.to_string(), absolute: true, scheme: scheme.clone(), ..Default::default() };
        // fragment, query
        if let Some(i) = rest.find('#') {
            u.fragment = format!("#{}", escape_unsafe(&rest[i + 1..], b"!$&'()*+,;=:@/?#"));
            rest = &rest[..i];
        }
        if let Some(i) = rest.find('?') {
            u.query = format!("?{}", escape_unsafe(&rest[i + 1..], b"!$&'()*+,;=:@/?"));
            rest = &rest[..i];
        }
        let hier = matches!(scheme.as_str(), "http" | "https" | "ftp" | "file" | "ws" | "wss" | "sftp" | "net.tcp" | "net.pipe") || rest.starts_with("//") || rest.starts_with("\\\\");
        let rest_norm = if hier && matches!(scheme.as_str(), "http" | "https" | "file") { rest.replace('\\', "/") } else { rest.to_string() };
        if hier && rest_norm.starts_with("//") {
            let after = &rest_norm[2..];
            let (auth, path) = match after.find('/') {
                Some(i) => (&after[..i], &after[i..]),
                None => (after, ""),
            };
            let (ui, hostport) = match auth.rfind('@') {
                Some(i) => (&auth[..i], &auth[i + 1..]),
                None => ("", auth),
            };
            u.user_info = ui.to_string();
            let (host, port) = if hostport.starts_with('[') {
                match hostport.find(']') {
                    Some(i) => (&hostport[..=i], hostport[i + 1..].strip_prefix(':')),
                    None => return Err("Invalid URI: The hostname could not be parsed.".into()),
                }
            } else {
                match hostport.rfind(':') {
                    Some(i) => (&hostport[..i], Some(&hostport[i + 1..])),
                    None => (hostport, None),
                }
            };
            if host.is_empty() && scheme != "file" {
                return Err("Invalid URI: The hostname could not be parsed.".into());
            }
            if host.chars().any(|c| c.is_whitespace() || "<>\"{}|\\^`".contains(c)) {
                return Err("Invalid URI: The hostname could not be parsed.".into());
            }
            u.host = host.to_lowercase();
            if let Some(p) = port.filter(|p| !p.is_empty()) {
                match p.parse::<i32>() {
                    Ok(v) if (0..=65535).contains(&v) => {
                        if v != default_port(&scheme) {
                            u.port = Some(v);
                        }
                    }
                    _ => return Err("Invalid URI: Invalid port specified.".into()),
                }
            }
            let path = if path.is_empty() { "/" } else { path };
            u.path = escape_unsafe(&remove_dot_segments(path), SUBDELIMS);
        } else if hier && scheme == "file" {
            let p = rest_norm.trim_start_matches('/');
            u.path = format!("/{}", escape_unsafe(p, SUBDELIMS));
        } else {
            // opaque (mailto:, urn:, ...)
            u.path = escape_unsafe(&rest_norm, SUBDELIMS);
        }
        Ok(u)
    }

    pub fn relative(s: &str) -> NetUri {
        NetUri { original: s.into(), absolute: false, relative: s.into(), ..Default::default() }
    }

    pub fn port_num(&self) -> i32 {
        self.port.unwrap_or_else(|| default_port(&self.scheme))
    }

    pub fn authority(&self) -> String {
        match self.port {
            Some(p) => format!("{}:{p}", self.host),
            None => self.host.clone(),
        }
    }

    fn has_authority(&self) -> bool {
        !self.host.is_empty() || self.scheme == "file"
    }

    pub fn absolute_uri(&self) -> String {
        if !self.absolute {
            return self.relative.clone();
        }
        let mut s = format!("{}:", self.scheme);
        if self.has_authority() {
            s.push_str("//");
            if !self.user_info.is_empty() {
                s.push_str(&self.user_info);
                s.push('@');
            }
            s.push_str(&self.authority());
        }
        s.push_str(&self.path);
        s.push_str(&self.query);
        s.push_str(&self.fragment);
        s
    }

    /// Uri.ToString(): the canonical form with unreserved characters unescaped.
    pub fn to_display(&self) -> String {
        if !self.absolute {
            return self.relative.clone();
        }
        let a = self.absolute_uri();
        // .NET unescapes %20 and other unreserved-safe escapes in ToString
        let mut out = String::new();
        let b = a.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() {
                if let Ok(v) = u8::from_str_radix(&a[i + 1..i + 3], 16) {
                    if v == b' ' || (v.is_ascii_alphanumeric()) || v >= 0x80 {
                        out.push(v as char);
                        i += 3;
                        continue;
                    }
                }
            }
            out.push(b[i] as char);
            i += 1;
        }
        // re-decode multi-byte sequences that were pushed as Latin-1 chars
        let bytes: Vec<u8> = out.chars().map(|c| c as u32 as u8).collect();
        String::from_utf8(bytes).unwrap_or(out)
    }

    pub fn segments(&self) -> Vec<String> {
        let mut out = vec![];
        let p = &self.path;
        let mut start = 0;
        for (i, c) in p.char_indices() {
            if c == '/' {
                out.push(p[start..=i].to_string());
                start = i + 1;
            }
        }
        if start < p.len() {
            out.push(p[start..].to_string());
        }
        out
    }

    pub fn left_part(&self, part: i64) -> String {
        let mut s = format!("{}:", self.scheme);
        if part == 0 {
            return if self.has_authority() { format!("{s}//") } else { s };
        }
        if self.has_authority() {
            s.push_str("//");
            if !self.user_info.is_empty() {
                s.push_str(&self.user_info);
                s.push('@');
            }
            s.push_str(&self.authority());
        }
        if part >= 2 {
            s.push_str(&self.path);
        }
        if part >= 3 {
            s.push_str(&self.query);
        }
        s
    }

    /// Resolve a relative reference against this base URI.
    pub fn join(&self, rel: &str) -> Result<NetUri, String> {
        if rel.contains("://") || rel.split_once(':').is_some_and(|(sch, _)| sch.len() > 1 && sch.chars().all(|c| c.is_ascii_alphanumeric())) && !rel.starts_with('/') {
            return NetUri::parse_absolute(rel);
        }
        let base = self.left_part(1);
        let target = if rel.starts_with("//") {
            format!("{}:{rel}", self.scheme)
        } else if rel.starts_with('/') {
            format!("{base}{rel}")
        } else if rel.starts_with('?') {
            format!("{base}{}{rel}", self.path)
        } else if rel.starts_with('#') {
            format!("{base}{}{}{rel}", self.path, self.query)
        } else if rel.is_empty() {
            format!("{base}{}{}", self.path, self.query)
        } else {
            let dir = match self.path.rfind('/') {
                Some(i) => &self.path[..=i],
                None => "/",
            };
            format!("{base}{dir}{rel}")
        };
        NetUri::parse_absolute(&target)
    }

    pub fn is_base_of(&self, other: &NetUri) -> bool {
        if !other.absolute {
            return true;
        }
        if self.scheme != other.scheme || self.host != other.host || self.port_num() != other.port_num() {
            return false;
        }
        let dir = match self.path.rfind('/') {
            Some(i) => &self.path[..=i],
            None => "/",
        };
        other.path.to_lowercase().starts_with(&dir.to_lowercase())
    }
}

pub fn is_well_formed(s: &str, kind: i64) -> bool {
    if s.is_empty() {
        return kind != 1;
    }
    if s.chars().any(|c| c.is_whitespace() || c == '\\' || "<>\"{}|^`".contains(c) || !c.is_ascii()) {
        return false;
    }
    // stray % not followed by two hex digits
    let b = s.as_bytes();
    for (i, x) in b.iter().enumerate() {
        if *x == b'%' && !(b.get(i + 1).is_some_and(|c| c.is_ascii_hexdigit()) && b.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit())) {
            return false;
        }
    }
    match NetUri::parse_absolute(s) {
        Ok(u) => {
            if kind == 2 {
                return false;
            }
            if u.scheme == "file" && (u.host.contains(':') || !s.to_ascii_lowercase().starts_with("file:///")) {
                return false;
            }
            s.find(':').map(|i| &s[i + 1..]).is_some_and(|r| r.starts_with("//") || !matches!(u.scheme.as_str(), "http" | "https"))
        }
        Err(_) => kind != 1 && !s.contains(':'),
    }
}

// ---------------------------------------------------------------- UriBuilder

#[derive(Clone, Debug)]
pub struct Builder {
    pub scheme: String,
    pub user: String,
    pub password: String,
    pub host: String,
    pub port: i32,
    pub path: String,
    /// Without the leading '?'.
    pub query: String,
    /// Without the leading '#'.
    pub fragment: String,
}

impl Builder {
    pub fn from_uri(u: &NetUri) -> Builder {
        let (user, password) = match u.user_info.split_once(':') {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (u.user_info.clone(), String::new()),
        };
        Builder {
            scheme: u.scheme.clone(),
            user,
            password,
            host: u.host.clone(),
            port: u.port_num(),
            path: u.path.clone(),
            query: u.query.trim_start_matches('?').to_string(),
            fragment: u.fragment.trim_start_matches('#').to_string(),
        }
    }

    pub fn text(&self) -> String {
        let mut s = format!("{}://", self.scheme);
        if !self.user.is_empty() {
            s.push_str(&self.user);
            if !self.password.is_empty() {
                s.push(':');
                s.push_str(&self.password);
            }
            s.push('@');
        }
        s.push_str(&self.host);
        if self.port != -1 && self.port != default_port(&self.scheme) {
            s.push_str(&format!(":{}", self.port));
        }
        if !self.path.starts_with('/') && !self.host.is_empty() {
            s.push('/');
        }
        s.push_str(&self.path);
        if !self.query.is_empty() {
            s.push('?');
            s.push_str(&self.query);
        }
        if !self.fragment.is_empty() {
            s.push('#');
            s.push_str(&self.fragment);
        }
        s
    }

    pub fn to_uri(&self) -> NetUri {
        let mut u = NetUri::parse_absolute(&self.text()).unwrap_or_default();
        // an explicit port that is the default for the original scheme stays visible (UriBuilder keeps it)
        if self.port != -1 && self.port != default_port(&self.scheme) {
            u.port = Some(self.port);
        }
        u
    }
}

pub fn construct(ty: &'static str, a: &Args) -> R<Val> {
    if ty == "System.Uri" {
        let r = match (a.raw(0), a.len()) {
            (Some(Val::Net(b)), 2) if matches!(&*b.borrow(), NetObj::Uri(_)) => {
                let base = match &*b.borrow() {
                    NetObj::Uri(u) => u.clone(),
                    _ => unreachable!(),
                };
                base.join(&a.text(1))
            }
            (_, 2) => {
                let kind = a.int(1);
                let s = a.text(0);
                match (NetUri::parse_absolute(&s), kind) {
                    (Ok(u), k) if k != 2 => Ok(u),
                    (_, 2) | (Err(_), 0) => Ok(NetUri::relative(&s)),
                    (Err(e), _) => Err(e),
                    (Ok(_), _) => Err("A relative URI cannot be created because the 'uriString' parameter represents an absolute URI.".into()),
                }
            }
            _ => NetUri::parse_absolute(&a.text(0)),
        };
        return match r {
            Ok(u) => Ok(net(NetObj::Uri(Rc::new(u)))),
            Err(e) => net_err("Uri", "Uri", e),
        };
    }
    // UriBuilder
    let b = match a.len() {
        0 => Builder { scheme: "http".into(), user: String::new(), password: String::new(), host: "localhost".into(), port: -1, path: "/".into(), query: String::new(), fragment: String::new() },
        1 => {
            let u = match a.raw(0) {
                Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Uri(_)) => match &*x.borrow() {
                    NetObj::Uri(u) => (**u).clone(),
                    _ => unreachable!(),
                },
                _ => {
                    let s = a.text(0);
                    let s2 = if s.contains("://") || s.to_ascii_lowercase().starts_with("mailto:") { s.clone() } else { format!("http://{s}") };
                    match NetUri::parse_absolute(&s2) {
                        Ok(u) => u,
                        Err(e) => return net_err("UriBuilder", "UriBuilder", e),
                    }
                }
            };
            Builder::from_uri(&u)
        }
        _ => Builder {
            scheme: a.text(0).to_lowercase(),
            user: String::new(),
            password: String::new(),
            host: a.text(1),
            port: if a.len() > 2 { a.int(2) as i32 } else { -1 },
            path: if a.len() > 3 { a.text(3) } else { "/".into() },
            query: String::new(),
            fragment: String::new(),
        },
    };
    Ok(net(NetObj::UriBuilder(b)))
}

fn uri_arg(v: Option<&Val>) -> Option<NetUri> {
    match v? {
        Val::Net(n) => match &*n.borrow() {
            NetObj::Uri(u) => Some((**u).clone()),
            NetObj::UriBuilder(b) => Some(b.to_uri()),
            NetObj::Prim(_, t) => NetUri::parse_absolute(&t.to_text()).ok(),
            _ => None,
        },
        v => NetUri::parse_absolute(&v.to_text()).ok(),
    }
}

// ---------------------------------------------------------------- HttpUtility / WebUtility

/// HttpUtility.UrlEncode: form encoding, space → '+', lower-case hex.
pub fn url_encode(s: &str, upper: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b' ' => out.push('+'),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'*' | b'(' | b')' => out.push(b as char),
            _ => out.push_str(&if upper { format!("%{b:02X}") } else { format!("%{b:02x}") }),
        }
    }
    out
}

pub fn url_decode(s: &str) -> String {
    unescape_data(&s.replace('+', " "))
}

pub fn html_encode(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if (c as u32) >= 160 && (c as u32) < 256 => out.push_str(&format!("&#{};", c as u32)),
            c => out.push(c),
        }
    }
    out
}

pub fn html_attribute_encode(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

pub fn html_decode(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let Some(end) = tail.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..end];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            "copy" => Some('©'),
            "reg" => Some('®'),
            "euro" => Some('€'),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn js_encode(s: &str, quotes: bool) -> String {
    let mut out = String::new();
    if quotes {
        out.push('"');
    }
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\'' | '<' | '>' | '&' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x20 || c == '\u{85}' || c == '\u{2028}' || c == '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    if quotes {
        out.push('"');
    }
    out
}

pub fn parse_query_string(s: &str) -> Vec<(Option<String>, Vec<String>)> {
    let q = s.strip_prefix('?').unwrap_or(s);
    let mut out: Vec<(Option<String>, Vec<String>)> = vec![];
    if q.is_empty() {
        return out;
    }
    for part in q.split('&') {
        let (k, v) = match part.split_once('=') {
            Some((k, v)) => (Some(url_decode(k)), url_decode(v)),
            None => (None, url_decode(part)),
        };
        match out.iter_mut().find(|(x, _)| match (x, &k) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            (None, None) => true,
            _ => false,
        }) {
            Some(e) => e.1.push(v),
            None => out.push((k, vec![v])),
        }
    }
    out
}

pub fn call(s: &mut Session, f: &mut Frame, n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    match ty {
        "System.UriKind" => {
            return ok(match m {
                "relativeorabsolute" => enum_val(ty, 0),
                "absolute" => enum_val(ty, 1),
                "relative" => enum_val(ty, 2),
                _ => return Ok(Out::Unknown),
            })
        }
        "System.UriPartial" => {
            return ok(match m {
                "scheme" => enum_val(ty, 0),
                "authority" => enum_val(ty, 1),
                "path" => enum_val(ty, 2),
                "query" => enum_val(ty, 3),
                _ => return Ok(Out::Unknown),
            })
        }
        "System.UriComponents" | "System.UriFormat" => return Ok(Out::Unknown),
        "System.Web.HttpUtility" | "System.Net.WebUtility" => {
            let web = ty == "System.Net.WebUtility";
            return ok(match m {
                "urlencode" => Val::Text(url_encode(&a.text(0), web)),
                "urldecode" => Val::Text(url_decode(&a.text(0))),
                "urlpathencode" => Val::Text(escape_unsafe(&a.text(0), b"!$&'()*+,;=:@/?#")),
                "htmlencode" => Val::Text(html_encode(&a.text(0))),
                "htmldecode" => Val::Text(html_decode(&a.text(0))),
                "htmlattributeencode" => Val::Text(html_attribute_encode(&a.text(0))),
                "javascriptstringencode" => Val::Text(js_encode(&a.text(0), a.bool(1))),
                "parsequerystring" => net(NetObj::NameValueCollection(parse_query_string(&a.text(0)))),
                "urlencodeunicode" => Val::Text(url_encode(&a.text(0), false)),
                _ => return Ok(Out::Unknown),
            });
        }
        _ => {}
    }
    // Uri statics
    match m {
        "escapedatastring" => return ok(Val::Text(escape_data(&a.text(0)))),
        "unescapedatastring" => return ok(Val::Text(unescape_data(&a.text(0)))),
        "escapeuristring" => return ok(Val::Text(escape_unsafe(&a.text(0), b"!$&'()*+,;=:@/?#[]"))),
        "iswellformeduristring" => return ok(Val::Bool(is_well_formed(&a.text(0), a.int(1)))),
        "checkschemename" => return ok(Val::Bool(a.text(0).chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && a.text(0).chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)))),
        "urischemehttp" => return ok(Val::Text("http".into())),
        "urischemehttps" => return ok(Val::Text("https".into())),
        "schemedelimiter" => return ok(Val::Text("://".into())),
        "trycreate" if ty == "System.Uri" => {
            let r = match (a.raw(0), a.len()) {
                (Some(Val::Net(b)), 3) if matches!(&*b.borrow(), NetObj::Uri(_)) => {
                    let base = match &*b.borrow() {
                        NetObj::Uri(u) => u.clone(),
                        _ => unreachable!(),
                    };
                    base.join(&a.text(1)).ok()
                }
                _ => {
                    let s_ = a.text(0);
                    match (NetUri::parse_absolute(&s_), a.int(1)) {
                        (Ok(u), k) if k != 2 => Some(u),
                        (Ok(_), _) => None,
                        (Err(_), 1) => None,
                        (Err(_), _) => Some(NetUri::relative(&s_)),
                    }
                }
            };
            let okr = r.is_some();
            let v = match r {
                Some(u) => net(NetObj::Uri(Rc::new(u))),
                None => net(NetObj::Null("System.Uri")),
            };
            s.net_out(f, a, a.len() - 1, v)?;
            return ok(Val::Bool(okr));
        }
        "compare" if ty == "System.Uri" => {
            let (x, y) = (uri_arg(a.raw(0)), uri_arg(a.raw(1)));
            return ok(Val::Int(x.map(|u| u.absolute_uri()).cmp(&y.map(|u| u.absolute_uri())) as i32));
        }
        _ => {}
    }
    let mut o = n.borrow_mut();
    match &mut *o {
        NetObj::Uri(u) => {
            let u = u.clone();
            drop(o);
            if !u.absolute && !matches!(m, "originalstring" | "tostring" | "isabsoluteuri" | "equals") {
                return net_err("Uri", m, "This operation is not supported for a relative URI.");
            }
            ok(match m {
                "absoluteuri" => Val::Text(u.absolute_uri()),
                "originalstring" => Val::Text(u.original.clone()),
                "tostring" => Val::Text(u.to_display()),
                "scheme" => Val::Text(u.scheme.clone()),
                "host" | "dnssafehost" | "idnhost" => Val::Text(u.host.trim_matches(|c| c == '[' || c == ']').to_string()),
                "port" => Val::Int(u.port_num()),
                "isdefaultport" => Val::Bool(u.port.is_none()),
                "authority" => Val::Text(u.authority()),
                "absolutepath" => Val::Text(u.path.clone()),
                "localpath" => Val::Text(unescape_data(&u.path)),
                "pathandquery" => Val::Text(format!("{}{}", u.path, u.query)),
                "query" => Val::Text(u.query.clone()),
                "fragment" => Val::Text(u.fragment.clone()),
                "userinfo" => Val::Text(u.user_info.clone()),
                "segments" => net(NetObj::Array("System.String", u.segments().into_iter().map(Val::Text).collect())),
                "isabsoluteuri" => Val::Bool(u.absolute),
                "isfile" => Val::Bool(u.scheme == "file"),
                "isloopback" => Val::Bool(u.host == "localhost" || u.host == "127.0.0.1" || u.host == "[::1]"),
                "isunc" => Val::Bool(false),
                "getleftpart" => Val::Text(u.left_part(a.int(0))),
                "isbaseof" => match uri_arg(a.raw(0)) {
                    Some(o) => Val::Bool(u.is_base_of(&o)),
                    None => return null_ref("Uri", "IsBaseOf"),
                },
                "makerelativeuri" => match uri_arg(a.raw(0)) {
                    Some(o) => {
                        let rel = if u.is_base_of(&o) {
                            let dir = u.path.rfind('/').map(|i| &u.path[..=i]).unwrap_or("/");
                            format!("{}{}{}", &o.path[dir.len().min(o.path.len())..], o.query, o.fragment)
                        } else {
                            o.absolute_uri()
                        };
                        net(NetObj::Uri(Rc::new(NetUri::relative(&rel))))
                    }
                    None => return null_ref("Uri", "MakeRelativeUri"),
                },
                "equals" => Val::Bool(uri_arg(a.raw(0)).is_some_and(|o| o.absolute_uri() == u.absolute_uri())),
                "gethashcode" => Val::Int(u.absolute_uri().len() as i32),
                "getcomponents" => Val::Text(u.absolute_uri()),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::UriBuilder(b) => {
            let set = a.len() == 1;
            ok(match m {
                "scheme" if set => {
                    let sch = a.text(0).to_lowercase();
                    let sch = sch.split(':').next().unwrap_or("").to_string();
                    if !sch.is_empty() && !(sch.chars().next().unwrap().is_ascii_alphabetic() && sch.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))) {
                        drop(o);
                        return net_err("UriBuilder", "Scheme", "Invalid URI: The URI scheme is not valid. (Parameter 'value')");
                    }
                    b.scheme = sch;
                    Val::Void
                }
                "scheme" => Val::Text(b.scheme.clone()),
                "host" if set => {
                    b.host = a.text(0);
                    Val::Void
                }
                "host" => Val::Text(b.host.clone()),
                "port" if set => {
                    let p = a.int(0);
                    if !(-1..=65535).contains(&p) {
                        return net_err("UriBuilder", "Port", "Specified argument was out of the range of valid values. (Parameter 'value')");
                    }
                    b.port = p as i32;
                    Val::Void
                }
                "port" => Val::Int(b.port),
                "path" if set => {
                    let p = a.text(0).replace('\\', "/");
                    b.path = escape_unsafe(&p, SUBDELIMS);
                    if b.path.is_empty() {
                        b.path = "/".into();
                    }
                    Val::Void
                }
                "path" => Val::Text(b.path.clone()),
                "query" if set => {
                    let q = a.text(0);
                    b.query = q.strip_prefix('?').unwrap_or(&q).to_string();
                    Val::Void
                }
                "query" => Val::Text(if b.query.is_empty() { String::new() } else { format!("?{}", b.query) }),
                "fragment" if set => {
                    let q = a.text(0);
                    b.fragment = q.strip_prefix('#').unwrap_or(&q).to_string();
                    Val::Void
                }
                "fragment" => Val::Text(if b.fragment.is_empty() { String::new() } else { format!("#{}", b.fragment) }),
                "username" if set => {
                    b.user = a.text(0);
                    Val::Void
                }
                "username" => Val::Text(b.user.clone()),
                "password" if set => {
                    b.password = a.text(0);
                    Val::Void
                }
                "password" => Val::Text(b.password.clone()),
                "uri" => {
                    let u = b.to_uri();
                    net(NetObj::Uri(Rc::new(u)))
                }
                "tostring" => Val::Text(b.text()),
                _ => return Ok(Out::Unknown),
            })
        }
        _ => Ok(Out::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_print() {
        let u = NetUri::parse_absolute("HTTP://Microsoft.com:80/segment2/segment3/?randomquery#main").unwrap();
        assert_eq!(u.absolute_uri(), "http://microsoft.com/segment2/segment3/?randomquery#main");
        assert_eq!(u.query, "?randomquery");
        assert_eq!(u.segments(), vec!["/", "segment2/", "segment3/"]);
        assert_eq!(NetUri::parse_absolute("http://microsoft.com").unwrap().absolute_uri(), "http://microsoft.com/");
        assert!(NetUri::parse_absolute("this is not a valid URI").is_err());
        let b = Builder { scheme: "https".into(), port: 80, ..Builder::from_uri(&NetUri::parse_absolute("http://microsoft.com:80/").unwrap()) };
        assert_eq!(b.to_uri().absolute_uri(), "https://microsoft.com:80/");
    }

    #[test]
    fn escaping() {
        assert_eq!(escape_data("http://<some%url.com>"), "http%3A%2F%2F%3Csome%25url.com%3E");
        assert_eq!(unescape_data("http%3A%2F%2F%3Csome%25url.com%3E"), "http://<some%url.com>");
        assert_eq!(url_encode("a b&c=d/é", false), "a+b%26c%3dd%2f%c3%a9");
        assert_eq!(html_encode("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
        assert!(!is_well_formed("http://www.contoso.com/path???/file name", 1));
        assert!(!is_well_formed("file://c:/directory/filename", 1));
        assert!(is_well_formed("http://www.contoso.com/path", 1));
    }
}

// ---------------------------------------------------------------- System.Net.Mail.MailAddress

/// Parse `user@host` or `Display Name <user@host>` as .NET MailAddress does (None = invalid).
pub fn parse_mail(s: &str) -> Option<(String, String)> {
    let t = s.trim();
    let (display, addr) = match (t.rfind('<'), t.ends_with('>')) {
        (Some(i), true) => (t[..i].trim().trim_matches('"').trim().to_string(), t[i + 1..t.len() - 1].trim().to_string()),
        _ => (String::new(), t.to_string()),
    };
    let at = addr.rfind('@')?;
    let (local, domain) = (&addr[..at], &addr[at + 1..]);
    let quoted = local.starts_with('"') && local.ends_with('"') && local.len() >= 2;
    if local.is_empty() || domain.is_empty() {
        return None;
    }
    if !quoted && (local.starts_with('.') || local.ends_with('.') || local.contains("..") || local.chars().any(|c| c.is_whitespace() || "()<>[]:;@\\,\"".contains(c))) {
        return None;
    }
    let domain_ok = if domain.starts_with('[') && domain.ends_with(']') {
        true
    } else {
        !domain.starts_with('.') && !domain.ends_with('.') && !domain.contains("..") && domain.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '.')
    };
    if !domain_ok {
        return None;
    }
    Some((addr, display))
}

pub fn mail_construct(a: &Args) -> R<Val> {
    match parse_mail(&a.text(0)) {
        Some((addr, disp)) => {
            let disp = if a.len() > 1 { a.text(1) } else { disp };
            Ok(net(NetObj::MailAddress(addr, disp)))
        }
        None => net_err("MailAddress", "MailAddress", "The specified string is not in the form required for an e-mail address."),
    }
}

pub fn mail_call(n: &NetRef, m: &str, a: &Args) -> R<Out> {
    let _ = a;
    let o = n.borrow();
    let NetObj::MailAddress(addr, disp) = &*o else { return Ok(Out::Unknown) };
    let at = addr.rfind('@').unwrap_or(0);
    ok(match m {
        "address" => Val::Text(addr.clone()),
        "displayname" => Val::Text(disp.clone()),
        "user" => Val::Text(addr[..at].to_string()),
        "host" => Val::Text(addr[at + 1..].to_string()),
        _ => return Ok(Out::Unknown),
    })
}
