//! System.String / Char / StringBuilder / Convert / Encoding / Math / BitConverter / Environment /
//! Guid / Version / numeric primitives, composite formatting (String.Format, ToString(format)) and
//! System.IO streams (MemoryStream, StreamReader/Writer, StringReader/Writer).

use super::culture::{self, Culture};
use super::*;
use rust_decimal::prelude::*;

// ---------------------------------------------------------------- conversions

/// .NET ToString() of a boxed AL value (current culture en-US).
pub fn net_to_string(v: &Val) -> String {
    match v {
        Val::Bool(b) => if *b { "True" } else { "False" }.into(),
        Val::Dec(d) => d.normalize().to_string(),
        Val::Guid(g) => crate::value::format_guid(g).trim_matches(|c| c == '{' || c == '}').to_lowercase(),
        Val::Void => String::new(),
        Val::DateTime(ms) => datetime::NetDateTime::from_al(*ms).format("G", culture::current()),
        Val::Duration(ms) => datetime::format_timespan(ms * datetime::TICKS_PER_MS, ""),
        other => other.to_text(),
    }
}

/// ToString of a boxed primitive with its .NET type (enum values print their names).
pub fn prim_to_string(t: &str, v: &Val) -> String {
    if let Some(names) = enum_names(t) {
        let i = v.as_i64();
        if let Some((n, _)) = names.iter().find(|(_, x)| *x == i) {
            return n.to_string();
        }
        // flags
        let parts: Vec<&str> = names.iter().filter(|(_, x)| *x != 0 && i & x == *x).map(|(n, _)| *n).collect();
        if !parts.is_empty() {
            return parts.join(", ");
        }
        return i.to_string();
    }
    net_to_string(v)
}

pub fn enum_names(t: &str) -> Option<&'static [(&'static str, i64)]> {
    Some(match t {
        "System.DayOfWeek" => &[("Sunday", 0), ("Monday", 1), ("Tuesday", 2), ("Wednesday", 3), ("Thursday", 4), ("Friday", 5), ("Saturday", 6)],
        "System.DateTimeKind" => &[("Unspecified", 0), ("Utc", 1), ("Local", 2)],
        "System.UriKind" => &[("RelativeOrAbsolute", 0), ("Absolute", 1), ("Relative", 2)],
        "System.StringComparison" => &[("CurrentCulture", 0), ("CurrentCultureIgnoreCase", 1), ("InvariantCulture", 2), ("InvariantCultureIgnoreCase", 3), ("Ordinal", 4), ("OrdinalIgnoreCase", 5)],
        "System.Text.RegularExpressions.RegexOptions" => &[
            ("None", 0),
            ("IgnoreCase", 1),
            ("Multiline", 2),
            ("ExplicitCapture", 4),
            ("Compiled", 8),
            ("Singleline", 16),
            ("IgnorePatternWhitespace", 32),
            ("RightToLeft", 64),
            ("ECMAScript", 256),
            ("CultureInvariant", 512),
        ],
        "System.Xml.XmlNodeType" => &[
            ("None", 0),
            ("Element", 1),
            ("Attribute", 2),
            ("Text", 3),
            ("CDATA", 4),
            ("EntityReference", 5),
            ("Entity", 6),
            ("ProcessingInstruction", 7),
            ("Comment", 8),
            ("Document", 9),
            ("DocumentType", 10),
            ("DocumentFragment", 11),
            ("Notation", 12),
            ("Whitespace", 13),
            ("SignificantWhitespace", 14),
            ("EndElement", 15),
            ("EndEntity", 16),
            ("XmlDeclaration", 17),
        ],
        "System.Text.NormalizationForm" => &[("FormC", 1), ("FormD", 2), ("FormKC", 5), ("FormKD", 6)],
        _ => return super::cipher::enum_names(t),
    })
}

fn enum_member(t: &'static str, m: &str) -> Option<Val> {
    let names = enum_names(t)?;
    names.iter().find(|(n, _)| n.eq_ignore_ascii_case(m)).map(|(_, v)| enum_val(t, *v))
}

/// Parse a number as .NET does with NumberStyles.Number/Float and a culture.
pub fn parse_decimal(s: &str, c: &Culture) -> Option<Decimal> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let (neg, t) = if t.starts_with('(') && t.ends_with(')') { (true, &t[1..t.len() - 1]) } else { (false, t) };
    let mut out = String::new();
    for ch in t.chars() {
        let cs = ch.to_string();
        if cs == c.group || (c.group == "\u{a0}" && ch == ' ') || (c.group == " " && ch == '\u{a0}') || (c.group == "’" && ch == '\'') {
            continue;
        }
        if cs == c.decimal {
            out.push('.');
        } else {
            out.push(ch);
        }
    }
    let d = Decimal::from_str(&out).ok().or_else(|| Decimal::from_scientific(&out).ok())?;
    Some(if neg { -d } else { d })
}

// ---------------------------------------------------------------- number formatting

/// ToString(format, provider) of a number: standard (N, F, D, X, C, P, E, G, R) and custom formats.
pub fn format_number(v: &Val, fmt: &str, c: &Culture) -> Option<String> {
    let d = match v {
        Val::Int(_) | Val::BigInt(_) | Val::Dec(_) | Val::Char(_) => v.as_dec(),
        Val::Bool(b) => return Some(if *b { "True" } else { "False" }.into()),
        _ => return Some(net_to_string(v)),
    };
    let is_int = matches!(v, Val::Int(_) | Val::BigInt(_));
    if fmt.is_empty() {
        return Some(if is_int { d.to_string() } else { d.normalize().to_string().replace('.', c.decimal) });
    }
    let mut chars = fmt.chars();
    let first = chars.next()?;
    let rest = chars.as_str();
    let prec: Option<u32> = if rest.is_empty() { None } else { rest.parse().ok() };
    if first.is_ascii_alphabetic() && (rest.is_empty() || prec.is_some()) {
        let s = match first.to_ascii_uppercase() {
            'N' => group(&fixed(d, prec.unwrap_or(2)), c),
            'F' => fixed(d, prec.unwrap_or(2)).replace('.', c.decimal),
            'D' => {
                let i = d.trunc().to_i64()?;
                let s = format!("{:0w$}", i.abs(), w = prec.unwrap_or(1) as usize);
                if i < 0 {
                    format!("-{s}")
                } else {
                    s
                }
            }
            'X' => {
                let i = d.trunc().to_i64()?;
                let s = if matches!(v, Val::Int(_)) { format!("{:X}", i as i32) } else { format!("{i:X}") };
                let s = format!("{:0>w$}", s, w = prec.unwrap_or(1) as usize);
                if first == 'x' {
                    s.to_lowercase()
                } else {
                    s
                }
            }
            'C' => {
                let body = group(&fixed(d.abs(), prec.unwrap_or(2)), c);
                if d.is_sign_negative() && !d.is_zero() {
                    format!("-{}{body}", c.currency)
                } else {
                    format!("{}{body}", c.currency)
                }
            }
            'P' => format!("{} %", group(&fixed(d * Decimal::from(100), prec.unwrap_or(2)), c)),
            'E' => {
                let x = d.to_f64()?;
                let p = prec.unwrap_or(6) as usize;
                let s = format!("{:.*e}", p, x);
                let (m, e) = s.split_once('e')?;
                let ev: i32 = e.parse().ok()?;
                let m = m.replace('.', c.decimal);
                let s = format!("{m}E{}{:03}", if ev < 0 { '-' } else { '+' }, ev.abs());
                if first == 'e' {
                    s.to_lowercase()
                } else {
                    s
                }
            }
            'G' | 'R' => match prec {
                Some(p) if p > 0 => {
                    let r = d.round_sf(p).unwrap_or(d).normalize();
                    r.to_string().replace('.', c.decimal)
                }
                _ => if is_int { d.to_string() } else { d.normalize().to_string().replace('.', c.decimal) },
            },
            _ => return None,
        };
        return Some(s);
    }
    Some(custom_number(d, fmt, c))
}

fn fixed(d: Decimal, p: u32) -> String {
    let r = d.round_dp_with_strategy(p, RoundingStrategy::MidpointAwayFromZero);
    let mut s = r.abs().to_string();
    let cur = s.find('.').map(|i| s.len() - i - 1).unwrap_or(0) as u32;
    if p > 0 && cur == 0 {
        s.push('.');
    }
    for _ in cur..p {
        s.push('0');
    }
    if r.is_sign_negative() && !r.is_zero() {
        format!("-{s}")
    } else {
        s
    }
}

fn group(s: &str, c: &Culture) -> String {
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (int, frac) = match s.find('.') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    let mut out = String::new();
    let b = int.as_bytes();
    for (i, ch) in b.iter().enumerate() {
        if i > 0 && (b.len() - i) % 3 == 0 {
            out.push_str(c.group);
        }
        out.push(*ch as char);
    }
    if !frac.is_empty() {
        out.push_str(c.decimal);
        out.push_str(frac);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

/// Custom numeric format: `0`, `#`, `.`, `,`, `%`, literals, `;` sections.
fn custom_number(d: Decimal, fmt: &str, c: &Culture) -> String {
    let sections: Vec<&str> = fmt.split(';').collect();
    let (pat, d, neg_section) = match sections.len() {
        1 => (sections[0], d, false),
        _ => {
            if d.is_sign_negative() && !d.is_zero() {
                (sections[1], d.abs(), true)
            } else if d.is_zero() && sections.len() > 2 {
                (sections[2], d, false)
            } else {
                (sections[0], d, false)
            }
        }
    };
    let pct = pat.contains('%');
    let d = if pct { d * Decimal::from(100) } else { d };
    let dot = pat.find('.');
    let (ipat, fpat) = match dot {
        Some(i) => (&pat[..i], &pat[i + 1..]),
        None => (pat, ""),
    };
    let max_frac = fpat.chars().filter(|c| *c == '0' || *c == '#').count() as u32;
    let min_frac = fpat.chars().filter(|c| *c == '0').count();
    let grouping = ipat.contains(',') && ipat.trim_end_matches(',').contains(|c| c == '0' || c == '#');
    // trailing commas before the decimal point scale by 1000
    let scale = ipat.len() - ipat.trim_end_matches(',').len();
    let mut d = d;
    for _ in 0..scale {
        d /= Decimal::from(1000);
    }
    let r = d.abs().round_dp_with_strategy(max_frac, RoundingStrategy::MidpointAwayFromZero);
    let s = r.to_string();
    let (int_digits, frac_digits) = match s.find('.') {
        Some(i) => (s[..i].to_string(), s[i + 1..].to_string()),
        None => (s.clone(), String::new()),
    };
    let min_int = ipat.chars().filter(|c| *c == '0').count();
    let mut int_s = int_digits.trim_start_matches('0').to_string();
    while int_s.len() < min_int {
        int_s.insert(0, '0');
    }
    if grouping {
        int_s = group(&int_s, c).replace(c.decimal, "");
    }
    let mut frac_s = frac_digits;
    while frac_s.len() < min_frac {
        frac_s.push('0');
    }
    while frac_s.len() > min_frac && frac_s.ends_with('0') {
        frac_s.pop();
    }
    // rebuild: literal prefix/suffix around the digit placeholders
    let first_ph = pat.find(|ch| matches!(ch, '0' | '#' | '.' | ',')).unwrap_or(pat.len());
    let last_ph = pat.rfind(|ch| matches!(ch, '0' | '#' | '.' | ',')).map(|i| i + 1).unwrap_or(first_ph);
    let lit = |x: &str| x.replace(['\'', '"'], "");
    let mut out = lit(&pat[..first_ph]);
    if d.is_sign_negative() && !r.is_zero() && !neg_section {
        out.insert(0, '-');
    }
    out.push_str(&int_s);
    if !frac_s.is_empty() {
        out.push_str(c.decimal);
        out.push_str(&frac_s);
    }
    out.push_str(&lit(&pat[last_ph..]));
    out
}

/// String.Format(format, args...): `{index[,alignment][:format]}`, `{{` and `}}`.
pub fn composite(fmt: &str, args: &[Val], c: &Culture) -> R<String> {
    let ch: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < ch.len() {
        match ch[i] {
            '{' if ch.get(i + 1) == Some(&'{') => {
                out.push('{');
                i += 2;
            }
            '}' if ch.get(i + 1) == Some(&'}') => {
                out.push('}');
                i += 2;
            }
            '{' => {
                let mut j = i + 1;
                while j < ch.len() && ch[j] != '}' {
                    j += 1;
                }
                let spec: String = ch[i + 1..j.min(ch.len())].iter().collect();
                let (head, f) = match spec.split_once(':') {
                    Some((h, f)) => (h.to_string(), f.to_string()),
                    None => (spec.clone(), String::new()),
                };
                let (idx, align) = match head.split_once(',') {
                    Some((a, b)) => (a.trim().to_string(), b.trim().parse::<i32>().unwrap_or(0)),
                    None => (head.trim().to_string(), 0),
                };
                let Ok(k) = idx.parse::<usize>() else { return net_err("String", "Format", "Input string was not in a correct format.") };
                let Some(v) = args.get(k) else { return net_err("String", "Format", "Index (zero based) must be greater than or equal to zero and less than the size of the argument list.") };
                let s = format_value(v, &f, c);
                let w = align.unsigned_abs() as usize;
                let n = s.chars().count();
                if n < w {
                    let pad = " ".repeat(w - n);
                    if align < 0 {
                        out.push_str(&s);
                        out.push_str(&pad);
                    } else {
                        out.push_str(&pad);
                        out.push_str(&s);
                    }
                } else {
                    out.push_str(&s);
                }
                i = j + 1;
            }
            x => {
                out.push(x);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// One formatted argument of String.Format / ToString(format).
pub fn format_value(v: &Val, f: &str, c: &Culture) -> String {
    match v {
        Val::Net(n) => {
            let o = n.borrow();
            match &*o {
                NetObj::DateTime(d) => datetime::format_ticks(d.ticks, Some(d.kind), None, f, c),
                NetObj::DateTimeOffset(u, off) => datetime::format_offset(*u, *off, f, c),
                NetObj::TimeSpan(t) => datetime::format_timespan(*t, f),
                NetObj::Prim(_, p) => format_value(p, f, c),
                _ => {
                    drop(o);
                    to_text(n)
                }
            }
        }
        Val::DateTime(ms) => datetime::NetDateTime::from_al(*ms).format(if f.is_empty() { "G" } else { f }, c),
        Val::Int(_) | Val::BigInt(_) | Val::Dec(_) => format_number(v, f, c).unwrap_or_else(|| v.to_text()),
        Val::Bool(b) => if *b { "True" } else { "False" }.into(),
        other => net_to_string(other),
    }
}

// ---------------------------------------------------------------- byte arrays

pub fn bytes_val(b: &[u8]) -> Val {
    net(NetObj::Array("System.Byte", b.iter().map(|x| Val::Int(*x as i32)).collect()))
}

/// Bytes of a .NET byte array / stream / AL InStream argument.
pub fn arg_bytes(v: Option<&Val>) -> Vec<u8> {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Array(_, v) => v.iter().map(|x| x.as_i64() as u8).collect(),
            NetObj::Stream(b, _) => b.clone(),
            NetObj::Prim(_, Val::Text(s)) => s.as_bytes().to_vec(),
            _ => vec![],
        },
        Some(Val::InStream(s)) => {
            let st = s.borrow();
            st.data[st.pos.min(st.data.len())..].to_vec()
        }
        Some(Val::Blob(b)) => b.clone(),
        Some(Val::Text(s)) => s.as_bytes().to_vec(),
        _ => vec![],
    }
}

pub fn chars_arg(v: Option<&Val>) -> Vec<char> {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Array(_, v) => v.iter().map(|x| match x {
                Val::Char(c) => *c,
                Val::Int(i) => char::from_u32(*i as u32).unwrap_or('\0'),
                o => o.to_text().chars().next().unwrap_or('\0'),
            }).collect(),
            NetObj::Prim(_, v) => v.to_text().chars().collect(),
            _ => vec![],
        },
        Some(Val::Char(c)) => vec![*c],
        Some(Val::Int(i)) => vec![char::from_u32(*i as u32).unwrap_or('\0')],
        Some(v) => v.to_text().chars().collect(),
        None => vec![],
    }
}

fn strings_arg(v: Option<&Val>) -> Vec<String> {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Array(_, v) | NetObj::List(_, v) => v.iter().map(|x| x.to_text()).collect(),
            NetObj::Prim(_, v) => vec![v.to_text()],
            _ => vec![],
        },
        Some(Val::List(l)) => l.borrow().iter().map(|x| x.to_text()).collect(),
        Some(v) => vec![v.to_text()],
        None => vec![],
    }
}

// ---------------------------------------------------------------- encodings

pub fn encode(cp: i32, s: &str, bom: bool) -> Vec<u8> {
    let mut out = vec![];
    match cp {
        65001 => {
            if bom {
                out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            }
            out.extend_from_slice(s.as_bytes());
        }
        1200 => {
            for u in s.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
        }
        1201 => {
            for u in s.encode_utf16() {
                out.extend_from_slice(&u.to_be_bytes());
            }
        }
        12000 => {
            for ch in s.chars() {
                out.extend_from_slice(&(ch as u32).to_le_bytes());
            }
        }
        20127 => out.extend(s.chars().map(|c| if (c as u32) < 128 { c as u8 } else { b'?' })),
        437 => out.extend(s.chars().map(|c| {
            if (c as u32) < 128 {
                c as u8
            } else {
                CP437_HIGH.chars().position(|x| x == c).map(|i| 128 + i as u8).unwrap_or(b'?')
            }
        })),
        28591 => out.extend(s.chars().map(|c| if (c as u32) < 256 { c as u32 as u8 } else { b'?' })),
        _ => match codepage_encoding(cp) {
            Some(e) => {
                let (b, _, _) = e.encode(s);
                out.extend_from_slice(&b);
            }
            None => out.extend_from_slice(s.as_bytes()),
        },
    }
    out
}

pub fn decode(cp: i32, b: &[u8]) -> String {
    match cp {
        65001 => String::from_utf8_lossy(b.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(b)).into_owned(),
        1200 => String::from_utf16_lossy(&b.chunks(2).map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect::<Vec<_>>()),
        1201 => String::from_utf16_lossy(&b.chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect::<Vec<_>>()),
        12000 => b.chunks(4).filter_map(|c| char::from_u32(u32::from_le_bytes([c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0), *c.get(3).unwrap_or(&0)]))).collect(),
        20127 => b.iter().map(|x| if *x < 128 { *x as char } else { '?' }).collect(),
        437 => b.iter().map(|x| if *x < 128 { *x as char } else { CP437_HIGH.chars().nth(*x as usize - 128).unwrap_or('?') }).collect(),
        28591 => b.iter().map(|x| *x as char).collect(),
        _ => match codepage_encoding(cp) {
            Some(e) => e.decode_without_bom_handling(b).0.into_owned(),
            None => String::from_utf8_lossy(b).into_owned(),
        },
    }
}

/// Code page 437 (OEM United States), bytes 0x80..=0xFF.
const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

fn codepage_encoding(cp: i32) -> Option<&'static encoding_rs::Encoding> {
    use encoding_rs::*;
    Some(match cp {
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1252 => WINDOWS_1252,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        874 => WINDOWS_874,
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        866 => IBM866,
        20866 => KOI8_R,
        28592 => ISO_8859_2,
        28595 => ISO_8859_5,
        28597 => ISO_8859_7,
        28605 => ISO_8859_15,
        _ => return None,
    })
}

fn codepage_of_name(n: &str) -> Option<i32> {
    let l = n.to_ascii_lowercase();
    Some(match l.as_str() {
        "utf-8" | "utf8" => 65001,
        "utf-16" | "unicode" | "utf-16le" => 1200,
        "utf-16be" | "unicodefffe" => 1201,
        "utf-32" => 12000,
        "us-ascii" | "ascii" => 20127,
        "iso-8859-1" | "latin1" => 28591,
        "windows-1252" => 1252,
        _ => {
            if let Some(cp) = l.strip_prefix("windows-").and_then(|x| x.parse().ok()) {
                return Some(cp);
            }
            let e = encoding_rs::Encoding::for_label(l.as_bytes())?;
            return [1250, 1251, 1252, 1253, 1254, 1255, 1256, 1257, 1258, 874, 932, 936, 949, 950, 866, 20866, 28592, 28595, 28597, 28605].into_iter().find(|cp| codepage_encoding(*cp) == Some(e));
        }
    })
}

fn encoding_web_name(cp: i32) -> &'static str {
    match cp {
        65001 => "utf-8",
        1200 => "utf-16",
        1201 => "utf-16BE",
        12000 => "utf-32",
        20127 => "us-ascii",
        28591 => "iso-8859-1",
        1252 => "Windows-1252",
        1250 => "windows-1250",
        1251 => "windows-1251",
        _ => "utf-8",
    }
}

pub fn encoding_val(cp: i32, bom: bool) -> Val {
    net(NetObj::Encoding(cp, bom))
}

fn known_codepage(cp: i32) -> bool {
    matches!(cp, 65001 | 1200 | 1201 | 12000 | 20127 | 28591 | 437) || codepage_encoding(cp).is_some()
}

// ---------------------------------------------------------------- streams

/// Append bytes to an AL OutStream.
pub fn outstream_append(t: &crate::streams::OutTarget, bytes: &[u8]) {
    // InStreams bound to the same Blob field see the bytes (crate::streams)
    crate::streams::append(t, bytes);
}

/// Write bytes to a .NET stream target (MemoryStream, AL OutStream).
pub fn stream_write(target: &Val, bytes: &[u8]) {
    match target {
        Val::OutStream(t) => outstream_append(t, bytes),
        // .NET writing through an AL InStream reaches its Blob field (crate::streams::in_stream_write)
        Val::InStream(s) => crate::streams::in_stream_write(s, bytes),
        Val::Net(n) if matches!(&*n.borrow(), NetObj::MmfView(..)) => {
            if let NetObj::MmfView(buf, pos) = &mut *n.borrow_mut() {
                super::nav::view_write(buf, pos, bytes);
            }
        }
        Val::Net(n) if matches!(&*n.borrow(), NetObj::CryptoStream(_)) => {
            if let NetObj::CryptoStream(w) = &mut *n.borrow_mut() {
                super::cipher::buffer(w, bytes);
            }
        }
        Val::Net(n) => {
            if let NetObj::Stream(b, pos) = &mut *n.borrow_mut() {
                let end = *pos + bytes.len();
                if b.len() < end {
                    b.resize(end, 0);
                }
                b[*pos..end].copy_from_slice(bytes);
                *pos = end;
            }
        }
        _ => {}
    }
}

/// All text readable from a stream-like argument (consumes an InStream).
pub fn read_all_text(v: Option<&Val>) -> String {
    match v {
        Some(Val::InStream(s)) => {
            let mut st = s.borrow_mut();
            let r = decode(65001, &st.data[st.pos.min(st.data.len())..]);
            st.pos = st.data.len();
            r
        }
        Some(Val::Net(n)) => match &mut *n.borrow_mut() {
            NetObj::Stream(b, pos) => {
                let r = decode(65001, &b[(*pos).min(b.len())..]);
                *pos = b.len();
                r
            }
            NetObj::Reader(_, s, p) => {
                let start = s.char_indices().nth(*p).map(|x| x.0).unwrap_or(s.len());
                let r = s[start..].to_string();
                *p = s.chars().count();
                r
            }
            NetObj::XReader(r) => r.text.to_string(),
            NetObj::Prim(_, v) => v.to_text(),
            _ => String::new(),
        },
        Some(v) => v.to_text(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------- constructors

pub fn construct(s: &mut Session, ty: &'static str, a: &Args) -> R<Val> {
    let _ = s;
    Ok(match ty {
        "System.Text.StringBuilder" => net(NetObj::StringBuilder(match a.raw(0) {
            Some(Val::Int(_)) | None => String::new(),
            Some(v) => v.to_text(),
        })),
        "System.String" => {
            // new String(char, count) / new String(char[])
            match (a.raw(0), a.raw(1)) {
                (Some(c), Some(n)) => Val::Text(chars_arg(Some(c)).first().map(|ch| ch.to_string().repeat(n.as_i64().max(0) as usize)).unwrap_or_default()),
                (Some(c), None) => Val::Text(chars_arg(Some(c)).into_iter().collect()),
                _ => Val::Text(String::new()),
            }
        }
        "System.Text.UTF8Encoding" => encoding_val(65001, a.bool(0)),
        "System.Text.UnicodeEncoding" => encoding_val(if a.bool(0) { 1201 } else { 1200 }, a.bool(1)),
        "System.Text.ASCIIEncoding" => encoding_val(20127, false),
        "System.Version" => {
            if a.len() == 1 {
                match parse_version(&a.text(0)) {
                    Some(v) => net(NetObj::Version(v)),
                    None => return net_err("Version", "Version", "Version string portion was too short or too long."),
                }
            } else {
                net(NetObj::Version((0..a.len()).map(|i| a.int(i) as i32).collect()))
            }
        }
        "System.Exception" => net(NetObj::Exception(a.text(0))),
        "System.IO.MemoryStream" => net(NetObj::Stream(if a.is_empty() || matches!(a.raw(0), Some(Val::Int(_))) { vec![] } else { arg_bytes(a.raw(0)) }, 0)),
        "System.IO.StreamReader" | "System.IO.StringReader" => {
            let text = if ty == "System.IO.StringReader" {
                a.text(0)
            } else {
                let cp = match a.raw(1) {
                    Some(Val::Net(e)) => match &*e.borrow() {
                        NetObj::Encoding(cp, _) => *cp,
                        _ => 65001,
                    },
                    _ => 65001,
                };
                match a.raw(0) {
                    Some(Val::InStream(st)) => {
                        let mut st = st.borrow_mut();
                        let r = decode(cp, &st.data[st.pos.min(st.data.len())..]);
                        st.pos = st.data.len();
                        r
                    }
                    // StreamReader(Path): the server file (the BOM is skipped, as detectEncoding does)
                    Some(Val::Text(path)) => match s.files.read(path) {
                        Some(b) => decode(cp, b.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(&b)),
                        None => return net_err("StreamReader", "StreamReader", format!("Could not find file '{}'.", crate::files::full_path(path))),
                    },
                    other => decode(cp, &arg_bytes(other)),
                }
            };
            net(NetObj::Reader(ty, text.into(), 0))
        }
        "System.IO.StreamWriter" => match a.raw(1) {
            Some(Val::Net(e)) if matches!(&*e.borrow(), NetObj::Encoding(cp, _) if *cp != 65001) => {
                let cp = match &*e.borrow() {
                    NetObj::Encoding(cp, _) => *cp,
                    _ => 65001,
                };
                net(NetObj::EncodedWriter(a.raw(0).cloned().unwrap_or(Val::Void), cp))
            }
            _ => net(NetObj::StreamWriter(a.raw(0).cloned().unwrap_or(Val::Void))),
        },
        // StringWriter(StringBuilder) writes into that StringBuilder
        "System.IO.StringWriter" => match a.raw(0) {
            Some(v @ Val::Net(n)) if matches!(&*n.borrow(), NetObj::StringBuilder(_)) => net(NetObj::StreamWriter(v.clone())),
            _ => net(NetObj::Writer(ty, String::new())),
        },
        // new ArrayList(ICollection) / List`1(IEnumerable) copy the items; a capacity starts empty
        "System.Collections.Generic.List`1" | "System.Collections.ArrayList" | "System.Collections.Queue" | "System.Collections.Stack" => net(NetObj::List(
            ty,
            match a.raw(0) {
                Some(v @ (Val::Net(_) | Val::List(_) | Val::Array(..))) => super::enumerate_val(Some(v)).unwrap_or_default(),
                _ => vec![],
            },
        )),
        "System.Collections.Generic.Dictionary`2" | "System.Collections.Hashtable" | "System.Collections.Generic.SortedDictionary`2" | "System.Collections.Generic.HashSet`1" => {
            net(NetObj::Dict(ty, vec![]))
        }
        "System.Collections.Specialized.NameValueCollection" => net(NetObj::NameValueCollection(vec![])),
        "System.Collections.Generic.KeyValuePair`2" => net(NetObj::KeyValue(a.get(0), a.get(1))),
        "System.Guid" => Val::Guid(crate::value::parse_guid(&a.text(0)).unwrap_or([0; 16])),
        "System.Object" => net(NetObj::Prim(ty, Val::Void)),
        _ => net(NetObj::Null(ty)),
    })
}

fn parse_version(s: &str) -> Option<Vec<i32>> {
    let parts: Vec<i32> = s.trim().split('.').map(|p| p.trim().parse::<i32>()).collect::<Result<_, _>>().ok()?;
    if parts.len() < 2 || parts.len() > 4 || parts.iter().any(|x| *x < 0) {
        return None;
    }
    Some(parts)
}

fn cmp_mode(v: Option<&Val>) -> (bool, bool) {
    // (ignore case, given)
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Prim("System.StringComparison", x) => (matches!(x.as_i64(), 1 | 3 | 5), true),
            NetObj::Prim(_, Val::Bool(b)) => (*b, true),
            _ => (false, false),
        },
        Some(Val::Bool(b)) => (*b, true),
        _ => (false, false),
    }
}

fn char_index(s: &str, byte: usize) -> i32 {
    s[..byte].chars().count() as i32
}

fn byte_at(s: &str, ch: usize) -> usize {
    s.char_indices().nth(ch).map(|x| x.0).unwrap_or(s.len())
}

/// .NET String.IndexOf (ordinal, optionally case-insensitive) from char position `start`.
fn index_of(s: &str, sub: &str, start: usize, ignore: bool) -> i32 {
    let (h, n) = if ignore { (s.to_lowercase(), sub.to_lowercase()) } else { (s.to_string(), sub.to_string()) };
    let b = byte_at(&h, start);
    match h[b..].find(&n) {
        Some(i) => char_index(&h, b + i),
        None => -1,
    }
}

fn split(s: &str, seps: &[String], count: Option<usize>, remove_empty: bool, trim: bool) -> Vec<String> {
    let seps: Vec<&String> = seps.iter().filter(|x| !x.is_empty()).collect();
    let mut parts: Vec<String> = vec![];
    if seps.is_empty() {
        // whitespace split
        parts = s.split(char::is_whitespace).map(String::from).collect();
    } else {
        let mut cur = String::new();
        let mut rest = s;
        'outer: while !rest.is_empty() {
            if count.is_some_and(|c| parts.len() + 1 >= c) {
                break;
            }
            for sep in &seps {
                if rest.starts_with(sep.as_str()) {
                    parts.push(std::mem::take(&mut cur));
                    rest = &rest[sep.len()..];
                    continue 'outer;
                }
            }
            let ch = rest.chars().next().unwrap();
            cur.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
        cur.push_str(rest);
        parts.push(cur);
    }
    if trim {
        parts = parts.into_iter().map(|p| p.trim().to_string()).collect();
    }
    if remove_empty {
        parts.retain(|p| !p.is_empty());
    }
    parts
}

fn str_array(v: Vec<String>) -> Val {
    net(NetObj::Array("System.String", v.into_iter().map(Val::Text).collect()))
}

fn split_args(a: &Args) -> (Vec<String>, Option<usize>, bool, bool) {
    let mut seps = vec![];
    let mut count = None;
    let mut opts = 0i64;
    for i in 0..a.len() {
        match a.raw(i) {
            Some(Val::Net(n)) => {
                let o = n.borrow();
                match &*o {
                    NetObj::Prim("System.StringSplitOptions", v) => opts = v.as_i64(),
                    NetObj::Array(t, items) => {
                        if *t == "System.Char" {
                            seps.extend(items.iter().map(|x| chars_arg(Some(x)).into_iter().collect::<String>()));
                        } else {
                            seps.extend(items.iter().map(|x| x.to_text()));
                        }
                    }
                    NetObj::Prim(_, Val::Int(x)) => count = Some(*x as usize),
                    NetObj::Prim(_, v) => seps.push(v.to_text()),
                    _ => {}
                }
            }
            Some(Val::Int(x)) if i > 0 => count = Some(*x as usize),
            Some(Val::Char(c)) => seps.push(c.to_string()),
            Some(Val::Text(t)) => seps.push(t.clone()),
            Some(Val::Opt { ord, .. }) => opts = *ord as i64,
            _ => {}
        }
    }
    (seps, count, opts & 1 != 0, opts & 2 != 0)
}

/// Members of System.String (instance and static).
fn string_call(s: &mut Session, f: &mut Frame, this: Option<String>, m: &str, a: &Args) -> R<Out> {
    let c = culture::current();
    // static
    match m {
        "format" => {
            let (cul, start) = match a.raw(0) {
                Some(Val::Net(n)) if matches!(&*n.borrow(), NetObj::Culture(..)) => match &*n.borrow() {
                    NetObj::Culture(x, _) => (*x, 1),
                    _ => (c, 1),
                },
                _ => (c, 0),
            };
            let fmt = a.text(start);
            let rest: Vec<Val> = (start + 1..a.len()).map(|i| a.raw(i).cloned().unwrap_or(Val::Void)).collect();
            // a single object[] argument holds the values
            let arr_items = match rest.as_slice() {
                [Val::Net(n)] => match &*n.borrow() {
                    NetObj::Array(_, v) => Some(v.clone()),
                    _ => None,
                },
                _ => None,
            };
            let rest = arr_items.unwrap_or(rest);
            return ok(Val::Text(composite(&fmt, &rest, cul)?));
        }
        "isnullorempty" => return ok(Val::Bool(matches!(a.raw(0), None | Some(Val::Void)) || a.text(0).is_empty() || super::is_null(a.raw(0).unwrap()))),
        "isnullorwhitespace" => return ok(Val::Bool(a.text(0).trim().is_empty())),
        "empty" => return ok(Val::Text(String::new())),
        "join" => {
            let sep = a.text(0);
            let items: Vec<String> = if a.len() == 2 { strings_arg(a.raw(1)) } else { (1..a.len()).map(|i| a.text(i)).collect() };
            return ok(Val::Text(items.join(&sep)));
        }
        "concat" => {
            let items: Vec<String> = if a.len() == 1 { strings_arg(a.raw(0)) } else { (0..a.len()).map(|i| a.text(i)).collect() };
            return ok(Val::Text(items.concat()));
        }
        "copy" => return ok(Val::Text(a.text(0))),
        "compare" | "compareordinal" if this.is_none() || a.len() >= 2 => {
            let (ig, _) = cmp_mode(a.raw(2));
            let (x, y) = (a.text(0), a.text(1));
            let r = if ig { x.to_lowercase().cmp(&y.to_lowercase()) } else if m == "compare" { x.to_lowercase().cmp(&y.to_lowercase()).then(y.cmp(&x)) } else { x.cmp(&y) };
            return ok(Val::Int(r as i32));
        }
        "equals" if this.is_none() || a.len() >= 2 => {
            let (ig, _) = cmp_mode(a.raw(2));
            let (x, y) = (a.text(0), a.text(1));
            return ok(Val::Bool(if ig { x.to_lowercase() == y.to_lowercase() } else { x == y }));
        }
        _ => {}
    }
    let Some(st) = this else { return Ok(Out::Unknown) };
    let len = st.chars().count();
    let range_err = || net_err("String", m, "Index and length must refer to a location within the string.");
    ok(match m {
        "length" => Val::Int(len as i32),
        "chars" | "get_chars" => match st.chars().nth(a.int(0) as usize) {
            Some(ch) => Val::Char(ch),
            None => return net_err("String", "Chars", "Index was outside the bounds of the array."),
        },
        "substring" => {
            let start = a.int(0).max(0) as usize;
            let n = if a.len() > 1 { a.int(1).max(0) as usize } else { len.saturating_sub(start) };
            if start > len || start + n > len {
                return range_err();
            }
            Val::Text(st.chars().skip(start).take(n).collect())
        }
        "indexof" => {
            let sub = match a.raw(0) {
                Some(Val::Char(ch)) => ch.to_string(),
                _ => a.text(0),
            };
            let (ig, given) = cmp_mode(a.raw(a.len().saturating_sub(1)));
            let start = if a.len() >= 2 && !(given && a.len() == 2) { a.int(1).max(0) as usize } else { 0 };
            Val::Int(index_of(&st, &sub, start.min(len), ig))
        }
        "lastindexof" => {
            let sub = match a.raw(0) {
                Some(Val::Char(ch)) => ch.to_string(),
                _ => a.text(0),
            };
            let (ig, _) = cmp_mode(a.raw(a.len().saturating_sub(1)));
            let (h, n) = if ig { (st.to_lowercase(), sub.to_lowercase()) } else { (st.clone(), sub) };
            let limit = if a.len() >= 2 && matches!(a.raw(1), Some(Val::Int(_))) { byte_at(&h, (a.int(1) as usize + 1).min(len)) } else { h.len() };
            Val::Int(h[..limit].rfind(&n).map(|i| char_index(&h, i)).unwrap_or(-1))
        }
        "indexofany" => {
            let set = chars_arg(a.raw(0));
            let start = if a.len() > 1 { a.int(1) as usize } else { 0 };
            Val::Int(st.chars().enumerate().skip(start).find(|(_, ch)| set.contains(ch)).map(|(i, _)| i as i32).unwrap_or(-1))
        }
        "lastindexofany" => {
            let set = chars_arg(a.raw(0));
            Val::Int(st.chars().enumerate().filter(|(_, ch)| set.contains(ch)).map(|(i, _)| i as i32).last().unwrap_or(-1))
        }
        "contains" => {
            let (ig, _) = cmp_mode(a.raw(1));
            let sub = a.text(0);
            Val::Bool(if ig { st.to_lowercase().contains(&sub.to_lowercase()) } else { st.contains(&sub) })
        }
        "startswith" | "endswith" => {
            let (ig, _) = cmp_mode(a.raw(1));
            let (h, n) = if ig { (st.to_lowercase(), a.text(0).to_lowercase()) } else { (st.clone(), a.text(0)) };
            Val::Bool(if m == "startswith" { h.starts_with(&n) } else { h.ends_with(&n) })
        }
        "replace" => {
            let from = a.text(0);
            if from.is_empty() {
                return net_err("String", "Replace", "String cannot be of zero length.");
            }
            let (ig, _) = cmp_mode(a.raw(2));
            if ig {
                let lower = st.to_lowercase();
                let fl = from.to_lowercase();
                let mut out = String::new();
                let mut last = 0;
                for (i, _) in lower.match_indices(&fl) {
                    out.push_str(&st[last..i]);
                    out.push_str(&a.text(1));
                    last = i + fl.len();
                }
                out.push_str(&st[last..]);
                Val::Text(out)
            } else {
                Val::Text(st.replace(&from, &a.text(1)))
            }
        }
        "toupper" | "toupperinvariant" => Val::Text(st.to_uppercase()),
        "tolower" | "tolowerinvariant" => Val::Text(st.to_lowercase()),
        "trim" | "trimstart" | "trimend" => {
            let set = if a.is_empty() { vec![] } else { (0..a.len()).flat_map(|i| chars_arg(a.raw(i))).collect() };
            let pred = |ch: char| if set.is_empty() { ch.is_whitespace() } else { set.contains(&ch) };
            Val::Text(match m {
                "trim" => st.trim_matches(pred).to_string(),
                "trimstart" => st.trim_start_matches(pred).to_string(),
                _ => st.trim_end_matches(pred).to_string(),
            })
        }
        "padleft" | "padright" => {
            let w = a.int(0).max(0) as usize;
            let ch = if a.len() > 1 { chars_arg(a.raw(1)).first().copied().unwrap_or(' ') } else { ' ' };
            let pad: String = std::iter::repeat_n(ch, w.saturating_sub(len)).collect();
            Val::Text(if m == "padleft" { pad + &st } else { st + &pad })
        }
        "split" => {
            let (seps, count, rm, trim) = split_args(a);
            str_array(split(&st, &seps, count, rm, trim))
        }
        "tochararray" => net(NetObj::Array("System.Char", st.chars().map(Val::Char).collect())),
        "insert" => {
            let i = a.int(0).max(0) as usize;
            if i > len {
                return range_err();
            }
            let b = byte_at(&st, i);
            Val::Text(format!("{}{}{}", &st[..b], a.text(1), &st[b..]))
        }
        "remove" => {
            let i = a.int(0).max(0) as usize;
            let n = if a.len() > 1 { a.int(1).max(0) as usize } else { len.saturating_sub(i) };
            if i + n > len {
                return range_err();
            }
            Val::Text(st.chars().enumerate().filter(|(k, _)| *k < i || *k >= i + n).map(|(_, ch)| ch).collect())
        }
        "equals" => {
            let (ig, _) = cmp_mode(a.raw(1));
            Val::Bool(if ig { st.to_lowercase() == a.text(0).to_lowercase() } else { st == a.text(0) })
        }
        "compareto" => Val::Int(st.cmp(&a.text(0)) as i32),
        "normalize" => Val::Text(st),
        "isnormalized" => Val::Bool(true),
        "tostring" | "clone" | "trimnull" => Val::Text(st),
        "gethashcode" => {
            let mut h: i32 = 5381;
            for ch in st.chars() {
                h = h.wrapping_mul(33) ^ ch as i32;
            }
            Val::Int(h)
        }
        "getenumerator" => net(NetObj::Enumerator(st.chars().map(Val::Char).collect(), -1)),
        _ => {
            let _ = (s, f);
            return Ok(Out::Unknown);
        }
    })
}

fn char_call(this: Option<char>, m: &str, a: &Args) -> R<Out> {
    let ch = match (this, a.raw(0)) {
        (_, Some(v)) if !matches!(m, "tostring" | "equals" | "compareto") => {
            let cs = chars_arg(Some(v));
            let idx = if a.len() > 1 { a.int(1) as usize } else { 0 };
            cs.get(idx).copied().unwrap_or('\0')
        }
        (Some(c), _) => c,
        _ => '\0',
    };
    ok(match m {
        "isletter" => Val::Bool(ch.is_alphabetic()),
        "isdigit" | "isnumber" => Val::Bool(ch.is_numeric()),
        "isletterordigit" => Val::Bool(ch.is_alphanumeric()),
        "iswhitespace" => Val::Bool(ch.is_whitespace()),
        "isupper" => Val::Bool(ch.is_uppercase()),
        "islower" => Val::Bool(ch.is_lowercase()),
        "ispunctuation" => Val::Bool(ch.is_ascii_punctuation() && !matches!(ch, '$' | '+' | '<' | '=' | '>' | '^' | '`' | '|' | '~')),
        "issymbol" => Val::Bool(matches!(ch, '$' | '+' | '<' | '=' | '>' | '^' | '`' | '|' | '~')),
        "iscontrol" => Val::Bool(ch.is_control()),
        "isseparator" => Val::Bool(ch == ' ' || ch == '\u{a0}'),
        "issurrogate" => Val::Bool(false),
        "toupper" | "toupperinvariant" => Val::Char(ch.to_uppercase().next().unwrap_or(ch)),
        "tolower" | "tolowerinvariant" => Val::Char(ch.to_lowercase().next().unwrap_or(ch)),
        "getnumericvalue" => Val::Dec(ch.to_digit(10).map(Decimal::from).unwrap_or(Decimal::from(-1))),
        "converttoutf32" => Val::Int(ch as i32),
        "convertfromutf32" => Val::Text(char::from_u32(a.int(0) as u32).map(|c| c.to_string()).unwrap_or_default()),
        "tostring" => Val::Text(this.map(|c| c.to_string()).unwrap_or_else(|| a.text(0))),
        "maxvalue" => Val::Char('\u{ffff}'),
        "minvalue" => Val::Char('\0'),
        "parse" => Val::Char(a.text(0).chars().next().unwrap_or('\0')),
        _ => return Ok(Out::Unknown),
    })
}

fn math_call(m: &str, a: &Args) -> R<Out> {
    let x = a.get(0).as_dec();
    let y = a.get(1).as_dec();
    let fx = x.to_f64().unwrap_or(0.0);
    let fy = y.to_f64().unwrap_or(0.0);
    let fd = |v: f64| Val::Dec(Decimal::from_f64_retain(v).map(|d| d.round_dp(15).normalize()).unwrap_or_default());
    let keep_int = |d: Decimal| match a.get(0) {
        Val::Int(_) => Val::Int(d.to_i32().unwrap_or(0)),
        Val::BigInt(_) => Val::BigInt(d.to_i64().unwrap_or(0)),
        _ => Val::Dec(d),
    };
    ok(match m {
        "abs" => keep_int(x.abs()),
        "max" => keep_int(x.max(y)),
        "min" => keep_int(x.min(y)),
        "sign" => Val::Int(if x.is_zero() { 0 } else if x.is_sign_negative() { -1 } else { 1 }),
        "floor" => Val::Dec(x.floor()),
        "ceiling" => Val::Dec(x.ceil()),
        "truncate" => Val::Dec(x.trunc()),
        "round" => {
            let dp = if a.len() > 1 && matches!(a.get(1), Val::Int(_)) { a.int(1) as u32 } else { 0 };
            let away = (0..a.len()).any(|i| matches!(a.raw(i), Some(Val::Net(n)) if matches!(&*n.borrow(), NetObj::Prim(t, v) if t.ends_with("MidpointRounding") && v.as_i64() == 1)));
            Val::Dec(x.round_dp_with_strategy(dp, if away { RoundingStrategy::MidpointAwayFromZero } else { RoundingStrategy::MidpointNearestEven }))
        }
        "pow" => fd(fx.powf(fy)),
        "sqrt" => fd(fx.sqrt()),
        "exp" => fd(fx.exp()),
        "log" => fd(if a.len() > 1 { fx.log(fy) } else { fx.ln() }),
        "log10" => fd(fx.log10()),
        "log2" => fd(fx.log2()),
        "sin" => fd(fx.sin()),
        "cos" => fd(fx.cos()),
        "tan" => fd(fx.tan()),
        "asin" => fd(fx.asin()),
        "acos" => fd(fx.acos()),
        "atan" => fd(fx.atan()),
        "atan2" => fd(fx.atan2(fy)),
        "sinh" => fd(fx.sinh()),
        "cosh" => fd(fx.cosh()),
        "tanh" => fd(fx.tanh()),
        "ieeeremainder" => fd(fx - fy * (fx / fy).round()),
        "bigmul" => Val::BigInt(a.int(0) * a.int(1)),
        "pi" => fd(std::f64::consts::PI),
        "e" => fd(std::f64::consts::E),
        _ => return Ok(Out::Unknown),
    })
}

/// Int32/Int64/Decimal/Double/Boolean/Byte statics and instance members.
fn number_call(s: &mut Session, f: &mut Frame, ty: &'static str, this: Option<Val>, m: &str, a: &Args) -> R<Out> {
    let c = culture::current();
    let parse = |t: &str, prov: &'static Culture| -> Option<Val> {
        let t = t.trim();
        match ty {
            "System.Boolean" => match t.to_ascii_lowercase().as_str() {
                "true" => Some(Val::Bool(true)),
                "false" => Some(Val::Bool(false)),
                _ => None,
            },
            "System.Int32" | "System.Int16" | "System.Byte" | "System.UInt16" => {
                let v: i64 = t.replace(prov.group, "").parse().ok()?;
                let ok_range = match ty {
                    "System.Byte" => (0..=255).contains(&v),
                    "System.Int16" => (-32768..=32767).contains(&v),
                    "System.UInt16" => (0..=65535).contains(&v),
                    _ => i32::try_from(v).is_ok(),
                };
                ok_range.then_some(Val::Int(v as i32))
            }
            "System.Int64" | "System.UInt32" | "System.UInt64" => t.replace(prov.group, "").parse::<i64>().ok().map(Val::BigInt),
            _ => parse_decimal(t, prov).map(Val::Dec),
        }
    };
    match m {
        "parse" | "tryparse" => {
            let prov = (1..a.len()).find_map(|i| match a.raw(i) {
                Some(Val::Net(n)) => match &*n.borrow() {
                    NetObj::Culture(x, _) => Some(*x),
                    _ => None,
                },
                _ => None,
            });
            let v = parse(&a.text(0), prov.unwrap_or(c));
            if m == "tryparse" {
                let r = v.is_some();
                let def = match ty {
                    "System.Boolean" => Val::Bool(false),
                    "System.Int32" | "System.Int16" | "System.Byte" | "System.UInt16" => Val::Int(0),
                    "System.Int64" | "System.UInt32" | "System.UInt64" => Val::BigInt(0),
                    _ => Val::Dec(Decimal::ZERO),
                };
                s.net_out(f, a, a.len() - 1, v.unwrap_or(def))?;
                return ok(Val::Bool(r));
            }
            return match v {
                Some(v) => ok(v),
                None => net_err(super::short_name(ty), "Parse", "Input string was not in a correct format."),
            };
        }
        "maxvalue" | "minvalue" => {
            let max = m == "maxvalue";
            return ok(match ty {
                "System.Int32" => Val::Int(if max { i32::MAX } else { i32::MIN }),
                "System.Int64" => Val::BigInt(if max { i64::MAX } else { i64::MIN }),
                "System.Int16" => Val::Int(if max { 32767 } else { -32768 }),
                "System.Byte" => Val::Int(if max { 255 } else { 0 }),
                _ => Val::Dec(if max { Decimal::MAX } else { Decimal::MIN }),
            });
        }
        "true" if ty == "System.Boolean" => return ok(Val::Bool(true)),
        "false" if ty == "System.Boolean" => return ok(Val::Bool(false)),
        _ => {}
    }
    let Some(v) = this else { return Ok(Out::Unknown) };
    ok(match m {
        "tostring" => {
            let (fmt, prov) = match (a.raw(0), a.raw(1)) {
                (Some(Val::Net(n)), None) if matches!(&*n.borrow(), NetObj::Culture(..)) => (String::new(), provider_of(a.raw(0))),
                (Some(_), p) => (a.text(0), provider_of(p)),
                (None, _) => (String::new(), c),
            };
            match format_number(&v, &fmt, prov) {
                Some(x) => Val::Text(x),
                None => return net_err(super::short_name(ty), "ToString", "Format specifier was invalid."),
            }
        }
        "compareto" => Val::Int(compare(&v, &a.get(0)) as i32),
        "equals" => Val::Bool(equals(&v, &a.get(0))),
        "gethashcode" => Val::Int(v.as_i64() as i32),
        _ => return Ok(Out::Unknown),
    })
}

pub fn provider_of(v: Option<&Val>) -> &'static Culture {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Culture(c, _) => c,
            _ => culture::current(),
        },
        _ => culture::current(),
    }
}

fn convert_call(m: &str, a: &Args) -> R<Out> {
    let v = a.get(0);
    let base = if a.len() > 1 && matches!(a.raw(1), Some(Val::Int(_))) { Some(a.int(1) as u32) } else { None };
    let int_of = |v: &Val| -> R<i64> {
        Ok(match (v, base) {
            (Val::Text(t), Some(b)) => match i64::from_str_radix(t.trim().trim_start_matches("0x").trim_start_matches("0X"), b) {
                Ok(x) => x,
                Err(_) => return net_err("Convert", m, "Could not find any recognizable digits."),
            },
            (Val::Text(t), None) => match t.trim().parse::<i64>() {
                Ok(x) => x,
                Err(_) => match parse_decimal(t, culture::current()) {
                    Some(d) => d.round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven).to_i64().unwrap_or(0),
                    None => return net_err("Convert", m, "Input string was not in a correct format."),
                },
            },
            (Val::Char(ch), _) => *ch as i64,
            (Val::Bool(b), _) => *b as i64,
            (Val::Dec(d), _) => d.round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven).to_i64().unwrap_or(0),
            (Val::Void, _) => 0,
            (v, _) => v.as_i64(),
        })
    };
    ok(match m {
        "tobase64string" => {
            use base64::Engine;
            let mut bytes = arg_bytes(a.raw(0));
            if a.len() >= 3 && matches!(a.raw(1), Some(Val::Int(_))) {
                let (off, n) = (a.int(1).max(0) as usize, a.int(2).max(0) as usize);
                bytes = bytes.get(off..(off + n).min(bytes.len())).unwrap_or(&[]).to_vec();
            }
            let line_breaks = (1..a.len()).any(|i| matches!(a.raw(i), Some(Val::Net(n)) if matches!(&*n.borrow(), NetObj::Prim(t, v) if t.ends_with("Base64FormattingOptions") && v.as_i64() == 1)));
            let s = base64::engine::general_purpose::STANDARD.encode(&bytes);
            Val::Text(if line_breaks { s.as_bytes().chunks(76).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join("\r\n") } else { s })
        }
        "frombase64string" => {
            use base64::Engine;
            let t: String = a.text(0).chars().filter(|c| !c.is_whitespace()).collect();
            match base64::engine::general_purpose::STANDARD.decode(t.as_bytes()) {
                Ok(b) => bytes_val(&b),
                Err(_) => return net_err("Convert", "FromBase64String", "The input is not a valid Base-64 string as it contains a non-base 64 character, more than two padding characters, or an illegal character among the padding characters."),
            }
        }
        "tohexstring" => Val::Text(arg_bytes(a.raw(0)).iter().map(|b| format!("{b:02X}")).collect()),
        "fromhexstring" => {
            let t = a.text(0);
            let b: Option<Vec<u8>> = (0..t.len() / 2).map(|i| u8::from_str_radix(&t[i * 2..i * 2 + 2], 16).ok()).collect();
            match b {
                Some(b) if t.len() % 2 == 0 => bytes_val(&b),
                _ => return net_err("Convert", "FromHexString", "The input is not a valid hex string."),
            }
        }
        "toint32" | "tobyte" | "tosbyte" => Val::Int(int_of(&v)? as i32),
        // the narrow types stay typed .NET values, so Write(Convert.ToInt16(X)) takes the Int16
        // overload as BC's compiler resolves it (AL reads them as Integer / BigInteger)
        "toint16" | "touint16" | "touint32" => {
            let n = int_of(&v)?;
            let (ty, lo, hi): (&'static str, i64, i64) = match m {
                "toint16" => ("System.Int16", i16::MIN as i64, i16::MAX as i64),
                "touint16" => ("System.UInt16", 0, u16::MAX as i64),
                _ => ("System.UInt32", 0, u32::MAX as i64),
            };
            if n < lo || n > hi {
                return net_err("Convert", m, format!("Value was either too large or too small for a{} {}.", if ty == "System.Int16" { "n" } else { "" }, &ty[7..]));
            }
            net(NetObj::Prim(ty, if ty == "System.UInt32" { Val::BigInt(n) } else { Val::Int(n as i32) }))
        }
        "toint64" | "touint64" => Val::BigInt(int_of(&v)?),
        "todecimal" | "todouble" | "tosingle" => Val::Dec(match &v {
            Val::Text(t) => match parse_decimal(t, provider_of(a.raw(1))) {
                Some(d) => d,
                None => return net_err("Convert", m, "Input string was not in a correct format."),
            },
            v => v.as_dec(),
        }),
        "toboolean" => Val::Bool(match &v {
            Val::Text(t) => match t.trim().to_ascii_lowercase().as_str() {
                "true" => true,
                "false" => false,
                _ => return net_err("Convert", "ToBoolean", "String was not recognized as a valid Boolean."),
            },
            v => v.as_bool() || v.as_i64() != 0,
        }),
        "tochar" => Val::Char(match &v {
            Val::Text(t) => t.chars().next().unwrap_or('\0'),
            v => char::from_u32(v.as_i64() as u32).unwrap_or('\0'),
        }),
        "tostring" => Val::Text(match (base, &v) {
            (Some(2), v) => format!("{:b}", v.as_i64()),
            (Some(8), v) => format!("{:o}", v.as_i64()),
            (Some(16), v) => format!("{:x}", v.as_i64()),
            _ => format_value(a.raw(0).unwrap_or(&Val::Void), "", provider_of(a.raw(1))),
        }),
        "todatetime" => match datetime::arg_dt(a.raw(0)) {
            Some(d) => net(NetObj::DateTime(d)),
            None => return net_err("Convert", "ToDateTime", "String was not recognized as a valid DateTime."),
        },
        "changetype" => v,
        "isdbnull" => Val::Bool(false),
        _ => return Ok(Out::Unknown),
    })
}

fn encoding_call(s: &mut Session, f: &mut Frame, n: &NetRef, m: &str, a: &Args) -> R<Out> {
    let _ = (s, f);
    match m {
        "utf8" | "default" => return ok(encoding_val(65001, m == "utf8")),
        "ascii" => return ok(encoding_val(20127, false)),
        "unicode" => return ok(encoding_val(1200, true)),
        "bigendianunicode" => return ok(encoding_val(1201, true)),
        "utf32" => return ok(encoding_val(12000, true)),
        "latin1" => return ok(encoding_val(28591, false)),
        "getencoding" => {
            let cp = match a.raw(0) {
                // 0: the default ANSI code page of the (Windows) service tier
                Some(Val::Int(_) | Val::BigInt(_)) if a.int(0) == 0 => 1252,
                Some(Val::Int(_) | Val::BigInt(_)) => a.int(0) as i32,
                Some(_) => match codepage_of_name(&a.text(0)) {
                    Some(cp) => cp,
                    None => return net_err("Encoding", "GetEncoding", format!("'{}' is not a supported encoding name.", a.text(0))),
                },
                None => 65001,
            };
            if !(0..=65535).contains(&cp) {
                return net_err("Encoding", "GetEncoding", "Valid values are between 0 and 65535, inclusive. (Parameter 'codepage')");
            }
            if !known_codepage(cp) {
                return net_err("Encoding", "GetEncoding", format!("No data is available for encoding {cp}."));
            }
            return ok(encoding_val(cp, matches!(cp, 65001 | 1200 | 1201 | 12000)));
        }
        "convert" => {
            let cp_of = |i: usize| match a.raw(i) {
                Some(Val::Net(e)) => match &*e.borrow() {
                    NetObj::Encoding(cp, _) => *cp,
                    _ => 65001,
                },
                _ => 65001,
            };
            let text = decode(cp_of(0), &arg_bytes(a.raw(2)));
            return ok(bytes_val(&encode(cp_of(1), &text, false)));
        }
        "registerprovider" => return void(),
        _ => {}
    }
    let (cp, bom) = match &*n.borrow() {
        NetObj::Encoding(cp, bom) => (*cp, *bom),
        _ => return Ok(Out::Unknown),
    };
    ok(match m {
        "getbytes" => {
            let text: String = match a.raw(0) {
                Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Array(..)) => chars_arg(a.raw(0)).into_iter().collect(),
                _ => a.text(0),
            };
            bytes_val(&encode(cp, &text, false))
        }
        "getstring" => {
            let mut b = arg_bytes(a.raw(0));
            if a.len() >= 3 {
                let (off, n) = (a.int(1).max(0) as usize, a.int(2).max(0) as usize);
                b = b.get(off..(off + n).min(b.len())).unwrap_or(&[]).to_vec();
            }
            Val::Text(decode(cp, &b))
        }
        "getchars" => net(NetObj::Array("System.Char", decode(cp, &arg_bytes(a.raw(0))).chars().map(Val::Char).collect())),
        "getbytecount" => Val::Int(encode(cp, &a.text(0), false).len() as i32),
        "getcharcount" => Val::Int(decode(cp, &arg_bytes(a.raw(0))).chars().count() as i32),
        "getpreamble" => bytes_val(&if bom { encode(cp, "\u{feff}", false) } else { vec![] }),
        "codepage" | "windowscodepage" => Val::Int(cp),
        "webname" | "bodyname" | "headername" => Val::Text(encoding_web_name(cp).into()),
        "encodingname" => Val::Text(
            match cp {
                65001 => "Unicode (UTF-8)",
                1200 => "Unicode",
                1201 => "Unicode (Big-Endian)",
                20127 => "US-ASCII",
                1252 => "Western European (Windows)",
                _ => "Encoding",
            }
            .into(),
        ),
        _ => return Ok(Out::Unknown),
    })
}

fn version_call(s: &mut Session, f: &mut Frame, n: &NetRef, m: &str, a: &Args) -> R<Out> {
    match m {
        "parse" => {
            return match parse_version(&a.text(0)) {
                Some(v) => ok(net(NetObj::Version(v))),
                None => net_err("Version", "Parse", "Version string portion was too short or too long."),
            }
        }
        "tryparse" => {
            let v = parse_version(&a.text(0));
            let r = v.is_some();
            s.net_out(f, a, 1, v.map(|v| net(NetObj::Version(v))).unwrap_or_else(|| net(NetObj::Null("System.Version"))))?;
            return ok(Val::Bool(r));
        }
        _ => {}
    }
    let v = match &*n.borrow() {
        NetObj::Version(v) => v.clone(),
        _ => return Ok(Out::Unknown),
    };
    let part = |i: usize| Val::Int(v.get(i).copied().unwrap_or(-1));
    ok(match m {
        "major" => part(0),
        "minor" => part(1),
        "build" => part(2),
        "revision" => part(3),
        "compareto" => {
            let o = match a.raw(0) {
                Some(Val::Net(x)) => match &*x.borrow() {
                    NetObj::Version(o) => o.clone(),
                    _ => vec![],
                },
                Some(t) => parse_version(&t.to_text()).unwrap_or_default(),
                None => vec![],
            };
            let key = |x: &Vec<i32>| (0..4).map(|i| x.get(i).copied().unwrap_or(-1)).collect::<Vec<_>>();
            Val::Int(key(&v).cmp(&key(&o)) as i32)
        }
        "tostring" => {
            let k = if a.is_empty() { v.len() } else { (a.int(0) as usize).min(v.len()) };
            Val::Text(v[..k].iter().map(|x| x.to_string()).collect::<Vec<_>>().join("."))
        }
        _ => return Ok(Out::Unknown),
    })
}

fn stream_call(n: &NetRef, m: &str, a: &Args) -> R<Out> {
    let mut o = n.borrow_mut();
    match &mut *o {
        NetObj::Stream(b, pos) => ok(match m {
            "toarray" | "getbuffer" => bytes_val(b),
            "length" => Val::BigInt(b.len() as i64),
            "position" if a.len() == 1 => {
                *pos = a.int(0).max(0) as usize;
                Val::Void
            }
            "position" => Val::BigInt(*pos as i64),
            "seek" => {
                let origin = a.int(1);
                let base = match origin {
                    1 => *pos as i64,
                    2 => b.len() as i64,
                    _ => 0,
                };
                *pos = (base + a.int(0)).max(0) as usize;
                Val::BigInt(*pos as i64)
            }
            "setlength" => {
                b.resize(a.int(0).max(0) as usize, 0);
                Val::Void
            }
            "write" => {
                let src = arg_bytes(a.raw(0));
                let (off, cnt) = if a.len() >= 3 { (a.int(1).max(0) as usize, a.int(2).max(0) as usize) } else { (0, src.len()) };
                let chunk = src.get(off..(off + cnt).min(src.len())).unwrap_or(&[]).to_vec();
                let end = *pos + chunk.len();
                if b.len() < end {
                    b.resize(end, 0);
                }
                b[*pos..end].copy_from_slice(&chunk);
                *pos = end;
                Val::Void
            }
            "writebyte" => {
                let x = a.int(0) as u8;
                if *pos < b.len() {
                    b[*pos] = x;
                } else {
                    b.push(x);
                }
                *pos += 1;
                Val::Void
            }
            "readbyte" => {
                if *pos < b.len() {
                    *pos += 1;
                    Val::Int(b[*pos - 1] as i32)
                } else {
                    Val::Int(-1)
                }
            }
            "read" => {
                let (off, cnt) = (a.int(1).max(0) as usize, a.int(2).max(0) as usize);
                let avail = b.len().saturating_sub(*pos).min(cnt);
                let chunk = b[*pos..*pos + avail].to_vec();
                *pos += avail;
                if let Some(Val::Net(dst)) = a.raw(0) {
                    if let NetObj::Array(_, items) = &mut *dst.borrow_mut() {
                        for (k, x) in chunk.iter().enumerate() {
                            if off + k < items.len() {
                                items[off + k] = Val::Int(*x as i32);
                            }
                        }
                    }
                }
                Val::Int(avail as i32)
            }
            "copyto" => {
                let rest = b[(*pos).min(b.len())..].to_vec();
                *pos = b.len();
                drop(o);
                if let Some(t) = a.raw(0) {
                    stream_write(t, &rest);
                }
                Val::Void
            }
            "writeto" => {
                let all = b.clone();
                drop(o);
                if let Some(t) = a.raw(0) {
                    stream_write(t, &all);
                }
                Val::Void
            }
            "canread" | "canwrite" | "canseek" => Val::Bool(true),
            "flush" | "close" | "dispose" => Val::Void,
            _ => return Ok(Out::Unknown),
        }),
        NetObj::Reader(_, text, p) => {
            let chars: Vec<char> = text.chars().collect();
            ok(match m {
                "readtoend" => {
                    let r: String = chars[(*p).min(chars.len())..].iter().collect();
                    *p = chars.len();
                    Val::Text(r)
                }
                "readline" => {
                    if *p >= chars.len() {
                        return ok(Val::Void);
                    }
                    let mut line = String::new();
                    while *p < chars.len() && chars[*p] != '\n' && chars[*p] != '\r' {
                        line.push(chars[*p]);
                        *p += 1;
                    }
                    if *p < chars.len() && chars[*p] == '\r' {
                        *p += 1;
                    }
                    if *p < chars.len() && chars[*p] == '\n' {
                        *p += 1;
                    }
                    Val::Text(line)
                }
                "endofstream" => Val::Bool(*p >= chars.len()),
                "peek" => Val::Int(chars.get(*p).map(|c| *c as i32).unwrap_or(-1)),
                "read" => {
                    if *p < chars.len() {
                        *p += 1;
                        Val::Int(chars[*p - 1] as i32)
                    } else {
                        Val::Int(-1)
                    }
                }
                "currentencoding" => encoding_val(65001, true),
                "close" | "dispose" => Val::Void,
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::Writer(_, buf) => ok(match m {
            "write" => {
                buf.push_str(&a.text(0));
                Val::Void
            }
            "writeline" => {
                buf.push_str(&a.text(0));
                buf.push_str("\r\n");
                Val::Void
            }
            "tostring" => Val::Text(buf.clone()),
            "getstringbuilder" => net(NetObj::StringBuilder(buf.clone())),
            "flush" | "close" | "dispose" => Val::Void,
            _ => return Ok(Out::Unknown),
        }),
        NetObj::EncodedWriter(target, cp) => {
            let (target, cp) = (target.clone(), *cp);
            drop(o);
            ok(match m {
                "write" | "writeline" => {
                    let mut t = a.text(0);
                    if m == "writeline" {
                        t.push_str("\r\n");
                    }
                    stream_write(&target, &encode(cp, &t, false));
                    Val::Void
                }
                "basestream" => target,
                "flush" => Val::Void,
                // closing the writer closes its stream: a CryptoStream flushes its final block
                "close" | "dispose" => {
                    if let Val::Net(t) = &target {
                        super::cipher::finish(t)?;
                    }
                    Val::Void
                }
                "encoding" => encoding_val(cp, false),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::StreamWriter(target) => {
            let target = target.clone();
            drop(o);
            ok(match m {
                "write" | "writeline" => {
                    let mut t = a.text(0);
                    if m == "writeline" {
                        t.push_str("\r\n");
                    }
                    super::xmlwriter::write_text_to(&target, &t);
                    Val::Void
                }
                "tostring" => match &target {
                    Val::Net(x) => match &*x.borrow() {
                        NetObj::StringBuilder(s) => Val::Text(s.clone()),
                        _ => Val::Text(String::new()),
                    },
                    _ => Val::Text(String::new()),
                },
                "getstringbuilder" => target,
                "basestream" => target,
                "flush" => Val::Void,
                // closing the writer closes its stream: a CryptoStream flushes its final block
                "close" | "dispose" => {
                    if let Val::Net(t) = &target {
                        super::cipher::finish(t)?;
                    }
                    Val::Void
                }
                "autoflush" => Val::Bool(true),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::Null(_) => Ok(Out::Unknown),
        _ => Ok(Out::Unknown),
    }
}

pub fn call(s: &mut Session, f: &mut Frame, n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    let prim = match &*n.borrow() {
        NetObj::Prim(_, v) => Some(v.clone()),
        _ => None,
    };
    match ty {
        "System.String" => string_call(s, f, prim.map(|v| v.to_text()), m, a),
        "System.Char" => char_call(prim.map(|v| v.to_text().chars().next().unwrap_or('\0')), m, a),
        "System.Math" => math_call(m, a),
        "System.Convert" => convert_call(m, a),
        "System.Int16" | "System.Int32" | "System.Int64" | "System.UInt16" | "System.UInt32" | "System.UInt64" | "System.Byte" | "System.Decimal" | "System.Double"
        | "System.Single" | "System.Boolean" => number_call(s, f, ty, prim, m, a),
        "System.Text.Encoding" | "System.Text.UTF8Encoding" | "System.Text.UnicodeEncoding" | "System.Text.ASCIIEncoding" => encoding_call(s, f, n, m, a),
        "System.Text.StringBuilder" => {
            let mut o = n.borrow_mut();
            let NetObj::StringBuilder(sb) = &mut *o else { return Ok(Out::Unknown) };
            ok(match m {
                "append" => {
                    match a.raw(0) {
                        Some(Val::Char(ch)) if a.len() == 2 => sb.push_str(&ch.to_string().repeat(a.int(1).max(0) as usize)),
                        Some(v) => sb.push_str(&format_value(v, "", culture::current())),
                        None => {}
                    }
                    drop(o);
                    Val::Net(n.clone())
                }
                "appendline" => {
                    sb.push_str(&a.text(0));
                    sb.push_str("\r\n");
                    drop(o);
                    Val::Net(n.clone())
                }
                "appendformat" => {
                    let args: Vec<Val> = (1..a.len()).map(|i| a.raw(i).cloned().unwrap_or(Val::Void)).collect();
                    sb.push_str(&composite(&a.text(0), &args, culture::current())?);
                    drop(o);
                    Val::Net(n.clone())
                }
                "insert" => {
                    let b = byte_at(sb, a.int(0).max(0) as usize);
                    sb.insert_str(b, &a.text(1));
                    drop(o);
                    Val::Net(n.clone())
                }
                "replace" => {
                    *sb = sb.replace(&a.text(0), &a.text(1));
                    drop(o);
                    Val::Net(n.clone())
                }
                "remove" => {
                    let (i, k) = (a.int(0).max(0) as usize, a.int(1).max(0) as usize);
                    *sb = sb.chars().enumerate().filter(|(x, _)| *x < i || *x >= i + k).map(|(_, c)| c).collect();
                    drop(o);
                    Val::Net(n.clone())
                }
                "clear" => {
                    sb.clear();
                    drop(o);
                    Val::Net(n.clone())
                }
                "length" if a.len() == 1 => {
                    let k = a.int(0).max(0) as usize;
                    *sb = sb.chars().take(k).collect();
                    Val::Void
                }
                "length" => Val::Int(sb.chars().count() as i32),
                "capacity" => Val::Int(sb.len().max(16) as i32),
                "tostring" => {
                    if a.len() == 2 {
                        Val::Text(sb.chars().skip(a.int(0) as usize).take(a.int(1) as usize).collect())
                    } else {
                        Val::Text(sb.clone())
                    }
                }
                _ => return Ok(Out::Unknown),
            })
        }
        "System.Version" => version_call(s, f, n, m, a),
        "System.Guid" => ok(match m {
            "newguid" => Val::Guid(crate::clock::new_guid()),
            "empty" => Val::Guid([0; 16]),
            "parse" => match crate::value::parse_guid(&a.text(0)) {
                Some(g) => Val::Guid(g),
                None => return net_err("Guid", "Parse", "Guid should contain 32 digits with 4 dashes (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx)."),
            },
            "tryparse" => {
                let g = crate::value::parse_guid(&a.text(0));
                s.net_out(f, a, 1, Val::Guid(g.unwrap_or([0; 16])))?;
                Val::Bool(g.is_some())
            }
            "tostring" => {
                let g = match &prim {
                    Some(Val::Guid(g)) => *g,
                    _ => [0; 16],
                };
                let d = crate::value::format_guid(&g).trim_matches(|c| c == '{' || c == '}').to_lowercase();
                Val::Text(match a.text(0).as_str() {
                    "N" | "n" => d.replace('-', ""),
                    "B" | "b" => format!("{{{d}}}"),
                    "P" | "p" => format!("({d})"),
                    _ => d,
                })
            }
            _ => return Ok(Out::Unknown),
        }),
        "System.BitConverter" => ok(match m {
            "tostring" => {
                let b = arg_bytes(a.raw(0));
                let (off, k) = if a.len() >= 2 { (a.int(1).max(0) as usize, if a.len() > 2 { a.int(2).max(0) as usize } else { usize::MAX }) } else { (0, usize::MAX) };
                Val::Text(b.iter().skip(off).take(k).map(|x| format!("{x:02X}")).collect::<Vec<_>>().join("-"))
            }
            "getbytes" => match a.get(0) {
                Val::Int(i) => bytes_val(&i.to_le_bytes()),
                Val::BigInt(i) => bytes_val(&i.to_le_bytes()),
                Val::Bool(b) => bytes_val(&[b as u8]),
                Val::Char(c) => bytes_val(&(c as u16).to_le_bytes()),
                v => bytes_val(&v.as_i64().to_le_bytes()),
            },
            "toint32" | "touint32" | "toint16" | "touint16" | "toint64" => {
                let b = arg_bytes(a.raw(0));
                let off = a.int(1).max(0) as usize;
                let take = |n: usize| -> Vec<u8> { (0..n).map(|i| *b.get(off + i).unwrap_or(&0)).collect() };
                match m {
                    "toint32" => Val::Int(i32::from_le_bytes(take(4).try_into().unwrap())),
                    "touint32" => Val::BigInt(u32::from_le_bytes(take(4).try_into().unwrap()) as i64),
                    "toint16" => Val::Int(i16::from_le_bytes(take(2).try_into().unwrap()) as i32),
                    "touint16" => Val::Int(u16::from_le_bytes(take(2).try_into().unwrap()) as i32),
                    _ => Val::BigInt(i64::from_le_bytes(take(8).try_into().unwrap())),
                }
            }
            "islittleendian" => Val::Bool(true),
            _ => return Ok(Out::Unknown),
        }),
        "System.Threading.Thread" => ok(match m {
            "currentthread" => net(NetObj::Prim("System.Threading.Thread", Val::Void)),
            "currentculture" | "currentuiculture" => super::culture::culture_val(culture::current(), super::culture::View::Culture),
            "managedthreadid" => Val::Int(1),
            "sleep" | "yield" | "spinwait" => Val::Void,
            _ => return Ok(Out::Unknown),
        }),
        "System.Environment" => ok(match m {
            "newline" => Val::Text("\r\n".into()),
            "machinename" => Val::Text("BCRUST".into()),
            "username" => Val::Text(s.user_id.clone()),
            "processorcount" => Val::Int(std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(1)),
            "tickcount" | "tickcount64" => Val::BigInt(chrono::Utc::now().timestamp_millis() & 0x7fff_ffff),
            "getenvironmentvariable" => Val::Text(std::env::var(a.text(0)).unwrap_or_default()),
            "is64bitprocess" | "is64bitoperatingsystem" => Val::Bool(true),
            _ => return Ok(Out::Unknown),
        }),
        "System.StringComparison" | "System.Text.NormalizationForm" | "System.StringSplitOptions" | "System.IO.SeekOrigin" => {
            let v = match ty {
                "System.StringSplitOptions" => match m {
                    "none" => Some(enum_val(ty, 0)),
                    "removeemptyentries" => Some(enum_val(ty, 1)),
                    "trimentries" => Some(enum_val(ty, 2)),
                    _ => None,
                },
                "System.IO.SeekOrigin" => match m {
                    "begin" => Some(enum_val(ty, 0)),
                    "current" => Some(enum_val(ty, 1)),
                    "end" => Some(enum_val(ty, 2)),
                    _ => None,
                },
                _ => enum_member(ty, m),
            };
            match v {
                Some(v) => ok(v),
                None => Ok(Out::Unknown),
            }
        }
        "System.Exception" => {
            let msg = match &*n.borrow() {
                NetObj::Exception(x) => x.clone(),
                _ => return Ok(Out::Unknown),
            };
            ok(match m {
                "message" | "tostring" => Val::Text(msg),
                "innerexception" => net(NetObj::Null("System.Exception")),
                "stacktrace" | "source" => Val::Text(String::new()),
                "hresult" => Val::Int(-2146233088),
                _ => return Ok(Out::Unknown),
            })
        }
        "System.IO.MemoryStream" | "System.IO.Stream" | "System.IO.StreamReader" | "System.IO.StreamWriter" | "System.IO.StringReader" | "System.IO.StringWriter" | "System.IO.TextReader"
        | "System.IO.TextWriter" => stream_call(n, m, a),
        "System.Object" => match m {
            "tostring" => ok(Val::Text(prim.map(|v| net_to_string(&v)).unwrap_or_default())),
            _ => Ok(Out::Unknown),
        },
        _ => {
            // enums declared as DotNet types: X.Member
            if let Some(v) = enum_member(ty, m) {
                return ok(v);
            }
            // Enum.Parse(type, name [, ignoreCase]) called on an enum variable
            if let (Some(names), "parse" | "tryparse") = (enum_names(ty), m) {
                let (name, ic) = if a.len() >= 2 { (a.text(1), a.len() > 2 && a.bool(2)) } else { (a.text(0), false) };
                let found = name.split(',').map(|p| {
                    let p = p.trim();
                    p.parse::<i64>().ok().or_else(|| names.iter().find(|(x, _)| if ic { x.eq_ignore_ascii_case(p) } else { *x == p }).map(|(_, v)| *v))
                }).try_fold(0i64, |acc, v| v.map(|v| acc | v));
                return match found {
                    Some(v) => ok(enum_val(ty, v)),
                    None => net_err("Enum", "Parse", &format!("Requested value '{name}' was not found.")),
                };
            }
            if let (Some(v), "tostring") = (&prim, m) {
                return ok(Val::Text(prim_to_string(ty, v)));
            }
            Ok(Out::Unknown)
        }
    }
}

/// Code page for an AL TextEncoding option value (MSDos, UTF8, UTF16, Windows) and a code page.
pub fn text_encoding_cp(enc: &Val, codepage: i64) -> i32 {
    let ord = enc.as_i64() as i32;
    let is = |n: &str| ord == crate::interp::system_option_ord("textencoding", n);
    if is("utf16") {
        1200
    } else if is("windows") {
        if codepage > 0 { codepage as i32 } else { 1252 }
    } else if is("msdos") {
        if codepage > 0 { codepage as i32 } else { 437 }
    } else if codepage > 0 && !is("utf8") {
        codepage as i32
    } else {
        65001
    }
}

fn b64_encode(bytes: &[u8], line_breaks: bool) -> String {
    use base64::Engine;
    let s = base64::engine::general_purpose::STANDARD.encode(bytes);
    if line_breaks {
        s.as_bytes().chunks(76).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join("\r\n")
    } else {
        s
    }
}

fn b64_decode(s: &str) -> R<Vec<u8>> {
    use base64::Engine;
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(t.as_bytes()).or_else(|_| {
        err("The input is not a valid Base-64 string as it contains a non-base 64 character, more than two padding characters, or an illegal character among the padding characters.")
    })
}

/// The platform codeunit Base64Convert (System Application's "Base64 Convert" calls it).
pub fn base64_convert(s: &mut Session, f: &mut Frame, m: &str, a: &Args) -> R<Out> {
    let _ = (s, f);
    match m {
        "tobase64" => {
            let breaks = a.bool(1);
            match a.raw(0) {
                Some(Val::InStream(st)) => {
                    let bytes = {
                        let mut st = st.borrow_mut();
                        let b = st.data[st.pos.min(st.data.len())..].to_vec();
                        st.pos = st.data.len();
                        b
                    };
                    let text = b64_encode(&bytes, breaks);
                    if let Some(Val::OutStream(t)) = a.raw(2) {
                        outstream_append(t, text.as_bytes());
                        return void();
                    }
                    ok(Val::Text(text))
                }
                Some(Val::Opaque("InStream")) => ok(Val::Text(String::new())),
                _ => {
                    let cp = if a.len() >= 3 { text_encoding_cp(&a.get(2), a.int(3)) } else { 65001 };
                    let bytes = encode(cp, &a.text(0), false);
                    ok(Val::Text(b64_encode(&bytes, breaks)))
                }
            }
        }
        "frombase64" => {
            let bytes = b64_decode(&a.text(0))?;
            if let Some(Val::OutStream(t)) = a.raw(1) {
                outstream_append(t, &bytes);
                return void();
            }
            let cp = if a.len() >= 2 { text_encoding_cp(&a.get(1), a.int(2)) } else { 65001 };
            ok(Val::Text(decode(cp, &bytes)))
        }
        _ => Ok(Out::Unknown),
    }
}

/// CopyStream(OutStream, InStream [, Bytes]) where either side is a .NET stream.
pub fn copy_stream(out: &Val, inp: &Val, n: Option<usize>) -> R<Val> {
    let bytes: Vec<u8> = match inp {
        Val::InStream(s) => {
            let mut st = s.borrow_mut();
            let data = st.data.clone();
            let rest = &data[st.pos.min(data.len())..];
            let take = n.unwrap_or(rest.len()).min(rest.len());
            st.pos += take;
            rest[..take].to_vec()
        }
        Val::Net(x) => match &mut *x.borrow_mut() {
            NetObj::Stream(b, pos) => {
                let rest = &b[(*pos).min(b.len())..];
                let take = n.unwrap_or(rest.len()).min(rest.len());
                let r = rest[..take].to_vec();
                *pos += take;
                r
            }
            _ => return err("CopyStream requires an OutStream and an InStream that are initialized"),
        },
        _ => return err("CopyStream requires an OutStream and an InStream that are initialized"),
    };
    match out {
        Val::OutStream(_) | Val::Net(_) => {
            stream_write(out, &bytes);
            Ok(Val::Bool(true))
        }
        _ => err("CopyStream requires an OutStream and an InStream that are initialized"),
    }
}

/// Type name of an AL InStream seen as a .NET Stream (NetObj::InStreamView). Not a real .NET
/// name, so variables declared as System.IO.Stream keep their own dispatch.
pub const IN_STREAM_VIEW: &str = "bc-rust.InStreamAsStream";

/// Up to `len` bytes from a BinaryReader's source (AL InStream or MemoryStream), advancing it.
fn bin_take(src: &Val, len: usize) -> Vec<u8> {
    match src {
        Val::InStream(s) => {
            let mut st = s.borrow_mut();
            let p = st.pos.min(st.data.len());
            let e = p.saturating_add(len).min(st.data.len());
            let out = st.data[p..e].to_vec();
            st.pos = e;
            out
        }
        Val::Net(x) => match &mut *x.borrow_mut() {
            NetObj::Stream(b, pos) => {
                let p = (*pos).min(b.len());
                let e = p.saturating_add(len).min(b.len());
                *pos = e;
                b[p..e].to_vec()
            }
            _ => vec![],
        },
        _ => vec![],
    }
}

/// System.Diagnostics.Stopwatch, System.IO.BinaryWriter and BinaryReader.
pub fn misc_call(n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    use std::time::{Duration, Instant};
    if ty == "System.Diagnostics.Stopwatch" {
        match m {
            "startnew" => return ok(net(NetObj::Stopwatch(Some(Instant::now()), Duration::ZERO))),
            "frequency" => return ok(Val::BigInt(10_000_000)),
            "gettimestamp" => return ok(Val::BigInt(chrono::Utc::now().timestamp_micros() * 10)),
            "ishighresolution" => return ok(Val::Bool(true)),
            _ => {}
        }
        let mut o = n.borrow_mut();
        let NetObj::Stopwatch(start, acc) = &mut *o else { return Ok(Out::Unknown) };
        let elapsed = *acc + start.map(|s| s.elapsed()).unwrap_or_default();
        return ok(match m {
            "start" => {
                if start.is_none() {
                    *start = Some(Instant::now());
                }
                Val::Void
            }
            "stop" => {
                *acc = elapsed;
                *start = None;
                Val::Void
            }
            "reset" => {
                *acc = Duration::ZERO;
                *start = None;
                Val::Void
            }
            "restart" => {
                *acc = Duration::ZERO;
                *start = Some(Instant::now());
                Val::Void
            }
            "isrunning" => Val::Bool(start.is_some()),
            "elapsed" => datetime::timespan(elapsed.as_nanos() as i64 / 100),
            "elapsedmilliseconds" => Val::BigInt(elapsed.as_millis() as i64),
            "elapsedticks" => Val::BigInt(elapsed.as_nanos() as i64 / 100),
            _ => return Ok(Out::Unknown),
        });
    }
    // An AL InStream as a .NET Stream (BinaryReader.BaseStream): Position is 0-based here
    let view = match &*n.borrow() {
        NetObj::InStreamView(s) => Some(s.clone()),
        _ => None,
    };
    if let Some(s) = view {
        return ok(match m {
            "position" if a.len() == 1 => {
                let mut st = s.borrow_mut();
                st.pos = (a.int(0).max(0) as usize).min(st.data.len());
                Val::Void
            }
            "position" => Val::BigInt(s.borrow().pos as i64),
            "length" => Val::BigInt(s.borrow().data.len() as i64),
            "canread" | "canseek" => Val::Bool(true),
            "canwrite" => Val::Bool(false),
            "readbyte" => match bin_take(&Val::InStream(s.clone()), 1).first() {
                Some(b) => Val::Int(*b as i32),
                None => Val::Int(-1),
            },
            "seek" => {
                let mut st = s.borrow_mut();
                let base = match a.int(1) {
                    1 => st.pos as i64,
                    2 => st.data.len() as i64,
                    _ => 0,
                };
                st.pos = (base + a.get(0).as_i64()).clamp(0, st.data.len() as i64) as usize;
                Val::BigInt(st.pos as i64)
            }
            "flush" | "close" | "dispose" => Val::Void,
            _ => return Ok(Out::Unknown),
        });
    }
    // BinaryReader: reads what BinaryWriter writes (7-bit length-prefixed UTF-8 strings, little-endian numbers)
    let source = match &*n.borrow() {
        NetObj::BinaryReader(s) => Some(s.clone()),
        _ => None,
    };
    if let Some(src) = source {
        const EOS: &str = "Unable to read beyond the end of the stream.";
        let fixed = |len: usize, member: &str| -> R<Vec<u8>> {
            let b = bin_take(&src, len);
            if b.len() < len {
                return net_err("BinaryReader", member, EOS);
            }
            Ok(b)
        };
        return ok(match m {
            "basestream" => match &src {
                Val::InStream(s) => net(NetObj::InStreamView(s.clone())),
                other => other.clone(),
            },
            "readstring" => {
                let (mut len, mut shift) = (0usize, 0u32);
                loop {
                    let b = fixed(1, "ReadString")?[0];
                    len |= ((b & 0x7F) as usize) << shift;
                    if b & 0x80 == 0 {
                        break;
                    }
                    shift += 7;
                    if shift > 28 {
                        return net_err("BinaryReader", "ReadString", "Too many bytes in what should have been a 7-bit encoded integer.");
                    }
                }
                Val::Text(String::from_utf8_lossy(&fixed(len, "ReadString")?).into_owned())
            }
            "readbyte" => Val::Int(fixed(1, "ReadByte")?[0] as i32),
            "readboolean" => Val::Bool(fixed(1, "ReadBoolean")?[0] != 0),
            "readint16" => Val::Int(i16::from_le_bytes(fixed(2, "ReadInt16")?.try_into().unwrap()) as i32),
            "readuint16" => Val::Int(u16::from_le_bytes(fixed(2, "ReadUInt16")?.try_into().unwrap()) as i32),
            "readuint32" => Val::BigInt(u32::from_le_bytes(fixed(4, "ReadUInt32")?.try_into().unwrap()) as i64),
            "readsbyte" => Val::Int(fixed(1, "ReadSByte")?[0] as i8 as i32),
            "readdouble" => Val::Dec(rust_decimal::Decimal::try_from(f64::from_le_bytes(fixed(8, "ReadDouble")?.try_into().unwrap())).unwrap_or_default()),
            "readsingle" => Val::Dec(rust_decimal::Decimal::try_from(f32::from_le_bytes(fixed(4, "ReadSingle")?.try_into().unwrap()) as f64).unwrap_or_default()),
            "readint32" => Val::Int(i32::from_le_bytes(fixed(4, "ReadInt32")?.try_into().unwrap())),
            "readint64" => Val::BigInt(i64::from_le_bytes(fixed(8, "ReadInt64")?.try_into().unwrap())),
            "readbytes" => {
                let b = bin_take(&src, a.int(0).max(0) as usize);
                net(NetObj::Array("System.Byte", b.iter().map(|x| Val::Int(*x as i32)).collect()))
            }
            "readchar" => {
                let first = fixed(1, "ReadChar")?[0];
                let extra = if first >= 0xF0 { 3 } else if first >= 0xE0 { 2 } else if first >= 0xC0 { 1 } else { 0 };
                let mut b = vec![first];
                b.extend(bin_take(&src, extra));
                Val::Char(String::from_utf8_lossy(&b).chars().next().unwrap_or('\u{FFFD}'))
            }
            "peekchar" => match &src {
                Val::InStream(s) => {
                    let st = s.borrow();
                    st.data.get(st.pos).map(|b| Val::Int(*b as i32)).unwrap_or(Val::Int(-1))
                }
                _ => Val::Int(-1),
            },
            "close" | "dispose" => Val::Void,
            _ => return Ok(Out::Unknown),
        });
    }
    // BinaryWriter: .NET binary encodings (strings are length-prefixed UTF-8)
    let target = match &*n.borrow() {
        NetObj::BinaryWriter(t) => t.clone(),
        _ => return Ok(Out::Unknown),
    };
    match m {
        "write" => {
            // a typed .NET primitive picks its Write overload (2 bytes for Int16, ...)
            let typed = match a.raw(0) {
                Some(Val::Net(x)) => match &*x.borrow() {
                    NetObj::Prim(t, v) => match *t {
                        "System.Int16" | "System.UInt16" => Some((v.as_i64() as u16).to_le_bytes().to_vec()),
                        "System.Int32" | "System.UInt32" => Some((v.as_i64() as u32).to_le_bytes().to_vec()),
                        "System.Int64" | "System.UInt64" => Some(v.as_i64().to_le_bytes().to_vec()),
                        "System.Byte" | "System.SByte" => Some(vec![v.as_i64() as u8]),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            };
            if let Some(b) = typed {
                stream_write(&target, &b);
                return void();
            }
            let bytes: Vec<u8> = match a.raw(0) {
                Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Array(..)) => arg_bytes(a.raw(0)),
                _ => match a.get(0) {
                    Val::Text(s) => {
                        let mut out = vec![];
                        let mut len = s.len();
                        loop {
                            let b = (len & 0x7F) as u8;
                            len >>= 7;
                            if len == 0 {
                                out.push(b);
                                break;
                            }
                            out.push(b | 0x80);
                        }
                        out.extend_from_slice(s.as_bytes());
                        out
                    }
                    Val::Int(i) => i.to_le_bytes().to_vec(),
                    Val::BigInt(i) => i.to_le_bytes().to_vec(),
                    Val::Bool(b) => vec![b as u8],
                    Val::Char(c) => c.to_string().into_bytes(),
                    Val::Dec(d) => d.to_string().parse::<f64>().unwrap_or(0.0).to_le_bytes().to_vec(),
                    other => other.to_text().into_bytes(),
                },
            };
            stream_write(&target, &bytes);
            void()
        }
        "flush" | "close" | "dispose" => void(),
        "basestream" => ok(target),
        _ => Ok(Out::Unknown),
    }
}
