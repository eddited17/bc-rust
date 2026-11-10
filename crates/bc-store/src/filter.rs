//! BC filter expressions (`A|B`, `<>X`, `10000..20000`, `*abc*`, `@x*`, `''`)
//! compiled to predicates over stored values.

use crate::value::{cmp_ci, Value};
use std::cmp::Ordering;

#[derive(Debug, Clone)]
pub enum FNode {
    Or(Vec<FNode>),
    And(Vec<FNode>),
    Eq(Value),
    Ne(Value),
    Lt(Value),
    Le(Value),
    Gt(Value),
    Ge(Value),
    Range(Option<Value>, Option<Value>),
    Like(Vec<char>),
    True,
    False,
}

#[derive(Debug, Clone)]
pub struct FieldFilter {
    pub field: usize,
    pub node: FNode,
    /// Display form (GetFilter / GetFilters).
    pub text: String,
    pub group: i32,
}

impl FNode {
    pub fn matches(&self, v: &Value) -> bool {
        match self {
            FNode::Or(xs) => xs.iter().any(|x| x.matches(v)),
            FNode::And(xs) => xs.iter().all(|x| x.matches(v)),
            FNode::Eq(x) => v.cmp_sql(x) == Ordering::Equal,
            FNode::Ne(x) => v.cmp_sql(x) != Ordering::Equal,
            FNode::Lt(x) => v.cmp_sql(x) == Ordering::Less,
            FNode::Le(x) => v.cmp_sql(x) != Ordering::Greater,
            FNode::Gt(x) => v.cmp_sql(x) == Ordering::Greater,
            FNode::Ge(x) => v.cmp_sql(x) != Ordering::Less,
            FNode::Range(lo, hi) => {
                lo.as_ref().is_none_or(|l| v.cmp_sql(l) != Ordering::Less)
                    && hi.as_ref().is_none_or(|h| v.cmp_sql(h) != Ordering::Greater)
            }
            FNode::Like(p) => match v {
                Value::Text(s) => like(p, &s.chars().flat_map(|c| c.to_lowercase()).collect::<Vec<_>>()),
                _ => false,
            },
            FNode::True => true,
            FNode::False => false,
        }
    }

    /// Single equality value (for key prefix seeks).
    pub fn eq_value(&self) -> Option<&Value> {
        match self {
            FNode::Eq(v) => Some(v),
            FNode::Range(Some(a), Some(b)) if a.cmp_sql(b) == Ordering::Equal => Some(a),
            FNode::And(xs) if xs.len() == 1 => xs[0].eq_value(),
            FNode::Or(xs) if xs.len() == 1 => xs[0].eq_value(),
            _ => None,
        }
    }

    /// Inclusive range bounds (for seeks); None when not a simple range.
    pub fn range(&self) -> Option<(Option<&Value>, Option<&Value>)> {
        match self {
            FNode::Range(a, b) => Some((a.as_ref(), b.as_ref())),
            FNode::Eq(v) => Some((Some(v), Some(v))),
            FNode::Ge(v) => Some((Some(v), None)),
            FNode::Le(v) => Some((None, Some(v))),
            FNode::And(xs) | FNode::Or(xs) if xs.len() == 1 => xs[0].range(),
            _ => None,
        }
    }
}

fn like(p: &[char], s: &[char]) -> bool {
    // iterative wildcard match: * any run, ? any single char
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut mark) = (usize::MAX, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = si;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Parse filter text. `conv` converts a literal to a stored value of the field's type.
pub fn parse_filter(text: &str, conv: &dyn Fn(&str) -> Option<Value>, is_text: bool) -> Result<FNode, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut p = 0;
    let n = parse_or(&chars, &mut p, conv, is_text)?;
    Ok(n)
}

fn parse_or(c: &[char], p: &mut usize, conv: &dyn Fn(&str) -> Option<Value>, is_text: bool) -> Result<FNode, String> {
    let mut alts = vec![parse_and(c, p, conv, is_text)?];
    while *p < c.len() && c[*p] == '|' {
        *p += 1;
        alts.push(parse_and(c, p, conv, is_text)?);
    }
    Ok(if alts.len() == 1 { alts.pop().unwrap() } else { FNode::Or(alts) })
}

fn parse_and(c: &[char], p: &mut usize, conv: &dyn Fn(&str) -> Option<Value>, is_text: bool) -> Result<FNode, String> {
    let mut parts = vec![parse_term(c, p, conv, is_text)?];
    while *p < c.len() && c[*p] == '&' {
        *p += 1;
        parts.push(parse_term(c, p, conv, is_text)?);
    }
    Ok(if parts.len() == 1 { parts.pop().unwrap() } else { FNode::And(parts) })
}

/// Read a raw literal up to an operator boundary, honoring '...' quoting.
fn literal(c: &[char], p: &mut usize, stop_at_range: bool) -> (String, bool) {
    let mut s = String::new();
    let mut quoted = false;
    while *p < c.len() {
        let ch = c[*p];
        if ch == '\'' {
            quoted = true;
            *p += 1;
            while *p < c.len() {
                if c[*p] == '\'' {
                    if *p + 1 < c.len() && c[*p + 1] == '\'' {
                        s.push('\'');
                        *p += 2;
                        continue;
                    }
                    *p += 1;
                    break;
                }
                s.push(c[*p]);
                *p += 1;
            }
            continue;
        }
        if ch == '|' || ch == '&' || ch == ')' {
            break;
        }
        if stop_at_range && ch == '.' && *p + 1 < c.len() && c[*p + 1] == '.' {
            break;
        }
        s.push(ch);
        *p += 1;
    }
    let t = if quoted { s } else { s.trim().to_string() };
    (t, quoted)
}

fn parse_term(c: &[char], p: &mut usize, conv: &dyn Fn(&str) -> Option<Value>, is_text: bool) -> Result<FNode, String> {
    while *p < c.len() && c[*p] == ' ' {
        *p += 1;
    }
    if *p < c.len() && c[*p] == '(' {
        *p += 1;
        let n = parse_or(c, p, conv, is_text)?;
        if *p < c.len() && c[*p] == ')' {
            *p += 1;
        }
        return Ok(n);
    }
    let rest: String = c[*p..].iter().take(2).collect();
    let op = if rest.starts_with("<>") {
        "<>"
    } else if rest.starts_with("<=") {
        "<="
    } else if rest.starts_with(">=") {
        ">="
    } else if rest.starts_with('<') {
        "<"
    } else if rest.starts_with('>') {
        ">"
    } else if rest.starts_with('=') {
        "="
    } else {
        ""
    };
    *p += op.chars().count();
    let cv = |s: &str| conv(s).ok_or_else(|| format!("'{s}' is not a valid value for this filter"));
    if !op.is_empty() {
        let (lit, _) = literal(c, p, false);
        let v = cv(&lit)?;
        return Ok(match op {
            "<>" => FNode::Ne(v),
            "<=" => FNode::Le(v),
            ">=" => FNode::Ge(v),
            "<" => FNode::Lt(v),
            ">" => FNode::Gt(v),
            _ => FNode::Eq(v),
        });
    }
    let (lo, lo_quoted) = literal(c, p, true);
    if *p + 1 < c.len() && c[*p] == '.' && c[*p + 1] == '.' {
        *p += 2;
        let (hi, hi_quoted) = literal(c, p, false);
        let lo_v = if lo.is_empty() && !lo_quoted { None } else { Some(cv(&lo)?) };
        let hi_v = if hi.is_empty() && !hi_quoted { None } else { Some(cv(&hi)?) };
        return Ok(FNode::Range(lo_v, hi_v));
    }
    let mut s = lo;
    // BC FilterExpressionParser.ToExpression: wildcards (and a leading '@') apply to a quoted value
    // too; quotes only protect operator characters
    if is_text && (s.contains('*') || s.contains('?') || s.starts_with('@')) {
        if let Some(x) = s.strip_prefix('@') {
            s = x.to_string();
        }
        return Ok(FNode::Like(s.chars().flat_map(|c| c.to_lowercase()).collect()));
    }
    if is_text && s.starts_with('@') && !lo_quoted {
        s.remove(0);
    }
    Ok(FNode::Eq(cv(&s)?))
}

/// Case-insensitive text equality helper used by conversions.
pub fn text_eq(a: &str, b: &str) -> bool {
    cmp_ci(a, b) == Ordering::Equal
}
