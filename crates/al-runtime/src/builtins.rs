//! Global AL functions, static system classes and methods on built-in types.

use crate::interp::*;
use crate::record::*;
use crate::value::*;
use al_syntax::{Expr, ObjectKind, TypeRef};
use chrono::Datelike;
use rust_decimal::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

const STATIC: &[&str] = &[
    "codeunit", "page", "report", "xmlport", "query", "database", "session", "companyproperty", "navapp", "taskscheduler",
    "numbersequence", "isolatedstorage", "system", "debugger", "sessionsettings", "errorinfo", "dataclassification",
    "currpage", "currreport", "requestpage", "product", "uri", "telemetry", "productname", "currxmlport", "file",
];

pub fn is_static_class(n: &str) -> bool {
    STATIC.contains(&n)
}

fn texts(v: &[Val]) -> Vec<String> {
    v.iter().map(|x| x.to_text()).collect()
}

pub fn str_subst(fmt: &str, args: &[Val]) -> String {
    let mut out = String::new();
    let c: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '%' && i + 1 < c.len() && c[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < c.len() && c[j].is_ascii_digit() {
                j += 1;
            }
            let n: usize = c[i + 1..j].iter().collect::<String>().parse().unwrap_or(0);
            if n >= 1 && n <= args.len() {
                out.push_str(&format_val(&args[n - 1], 0));
            }
            i = j;
            continue;
        }
        if c[i] == '#' && i + 1 < c.len() && c[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < c.len() && c[j].is_ascii_digit() {
                j += 1;
            }
            let n: usize = c[i + 1..j].iter().collect::<String>().parse().unwrap_or(0);
            while j < c.len() && c[j] == '#' {
                j += 1;
            }
            if n >= 1 && n <= args.len() {
                out.push_str(&format_val(&args[n - 1], 0));
            }
            i = j;
            continue;
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

pub fn format_val(v: &Val, fmt: i64) -> String {
    match (v, fmt) {
        (Val::Dec(d), 9) => d.normalize().to_string(),
        (Val::Dec(d), 1) | (Val::Dec(d), 2) => format_decimal(*d, false),
        (Val::Date(d), 9) => date_to_naive(*d).map(|n| n.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "0001-01-01".into()),
        (Val::Bool(b), 9) => b.to_string(),
        // a DateFormula needs the session language: Session::format_date_formula; here (no
        // session) its stored text, through to_text below
        (Val::DateTime(t), 9) => chrono::DateTime::from_timestamp_millis(*t).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)).unwrap_or_default(),
        // standard formats 1 and 3: with seconds (1 also with the milliseconds)
        (Val::DateTime(t), 1) | (Val::DateTime(t), 3) => crate::value::format_datetime_seconds(*t, fmt == 1),
        (Val::Opt { ord, enum_id: Some(e) }, _) if *e >= 0 => CATALOG
            .get()
            .and_then(|c| c.enums.get(e))
            .and_then(|en| en.by_ordinal(*ord as i64))
            .map(|x| if fmt == 9 { x.name.clone() } else { x.caption.clone() })
            .unwrap_or_else(|| ord.to_string()),
        // Option values with their declared members (crate::optmeta)
        (Val::Opt { ord, enum_id: Some(e) }, _) if crate::optmeta::is_meta(*e) => crate::optmeta::format(*e, *ord, fmt),
        _ => v.to_text(),
    }
}

fn days(d: i32) -> Option<chrono::NaiveDate> {
    date_to_naive(d)
}
fn from_naive(n: chrono::NaiveDate) -> i32 {
    n.num_days_from_ce() * 2
}

/// CalcDate(Text, Date) for a formula text read in session language `lang` (the Date encoding is
/// `days_from_ce * 2`, plus 1 for a closing date). The CalcDate builtin goes through the
/// session's cache instead (Session::calc_date_val).
pub fn calc_date(formula: &str, lang: i32, date: i32) -> Result<i32, crate::datefml::DfError> {
    crate::datefml::calc_text(formula, lang, date)
}

/// The runtime error for an unsettled DateFormula point (spec section 4): `U1 unsettled: Pnn`,
/// recorded in assume (a test that swallows it is not trusted, one that passes is flagged).
pub fn df_unsettled<T>(p: crate::datefml::Probe) -> R<T> {
    crate::datefml::note_assumed(p);
    err(crate::datefml::unsettled_text(p))
}

/// A DateFormula error as an AL error in session language `lang`: E1..E6 with their codes, an
/// unsettled point as `U1 unsettled: Pnn`.
pub fn df_error<T>(e: crate::datefml::DfError, lang: i32) -> R<T> {
    match e.message(lang) {
        Ok(m) => err_code(e.code(), m),
        Err(p) => df_unsettled(p),
    }
}

impl<'r> Session<'r> {
    fn eval_all(&mut self, f: &mut Frame, args: &[Expr]) -> R<Vec<Val>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            out.push(self.eval(f, a)?);
        }
        Ok(out)
    }

    /// Arguments of StrSubstNo, Error, Message and Confirm: an Option record field formats with
    /// its caption, as Format(Rec.Field) does (BC option values carry the field's metadata).
    fn eval_subst_args(&mut self, f: &mut Frame, args: &[Expr]) -> R<Vec<Val>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            let v = self.eval(f, a)?;
            let v = match v {
                // R85, Q10: one module function, with the session language (T4:P16: format 0)
                Val::DateFormula(s) => match crate::datefml::text(&s, self.global_language) {
                    Ok(t) => Val::Text(t),
                    Err(e) => return df_error(e, self.global_language),
                },
                Val::Opt { enum_id: None, .. } => match self.field_of_expr(f, a)? {
                    Some((r, fi)) => {
                        let fd = r.borrow().field(fi);
                        Val::Text(self.format_field_val(fd, &v))
                    }
                    // Rec."Option Field"::Value formats like the field too
                    None => match self.option_field_of_scope(f, a)? {
                        Some(fd) => Val::Text(self.format_field_val(fd, &v)),
                        None => v,
                    },
                },
                v => v,
            };
            out.push(v);
        }
        Ok(out)
    }

    /// Format(DateFormula [, Length [, FormatNumber]]) in the session language (R50..R55).
    pub(crate) fn format_date_formula(&self, stored: &str, length: Option<&Val>, fmt: Option<&Val>) -> R<String> {
        use crate::datefml::{DfError, Probe};
        let lang = self.global_language;
        let r = match fmt {
            // a format string on a DateFormula: OPEN:P15
            Some(Val::Text(_)) => Err(DfError::Unsettled(Probe::P15)),
            _ => crate::datefml::format_with(stored, length.map(|v| v.as_i64()).unwrap_or(0), fmt.map(|v| v.as_i64()).unwrap_or(0), lang),
        };
        r.or_else(|e| df_error(e, lang))
    }

    /// Evaluate(DateFormula, Text [, Number]) into a variable or a field (R40..R44, Q3): user text
    /// in the session language. The return-value form gives false on a refused text; the
    /// statement form raises the reading error with code `Evaluate`.
    pub(crate) fn evaluate_date_formula(&mut self, lv: &LVal, text: &str, number: Option<i64>, discard: bool) -> R<Val> {
        let lang = self.global_language;
        // R43, T4:P15eval: number 9 reads the invariant letters, every other number the session's
        match crate::datefml::evaluate(text, lang, number) {
            Ok(stored) => {
                self.write_lval(lv, Val::DateFormula(stored))?;
                Ok(Val::Bool(true))
            }
            // R40: false, and the value stays
            Err(e) if !discard && e.is_refusal() => Ok(Val::Bool(false)),
            // R41: the statement form raises; the value stays (T4:P18stmt; a FieldRef: LT 67605
            // N582_Evaluate_FieldRef_Statement_BadText_MoreTypes)
            Err(e) => df_error(e, lang),
        }
    }

    /// The text of a RecordID ("<table>: <key values>", as Format / GetFilter give it; also the
    /// storage encoding) as a RecordId value; None when it names no table or the keys do not parse.
    pub(crate) fn parse_record_id_text(&self, text: &str) -> Option<Val> {
        if let Some((t, k)) = decode_record_id(text.trim()) {
            if self.cat.tables.contains_key(&t) {
                return Some(Val::RecordId(t, k));
            }
        }
        let t = text.trim();
        match t.split_once(':') {
            None if t.is_empty() => Some(Val::RecordId(0, vec![])),
            None => None,
            Some((table, keys)) => {
                let table = table.trim();
                let td = self.cat.tables.values().find(|d| d.name.eq_ignore_ascii_case(table)).or_else(|| self.cat.tables.values().find(|d| d.caption.eq_ignore_ascii_case(table)));
                td.and_then(|td| {
                    let pk = &td.pk().fields;
                    let mut parts: Vec<String> = keys.trim().split(',').map(|x| x.to_string()).collect();
                    // a key value with commas: the extra parts belong to the last field
                    while parts.len() > pk.len() && parts.len() > 1 {
                        let last = parts.pop().unwrap_or_default();
                        if let Some(p) = parts.last_mut() {
                            p.push(',');
                            p.push_str(&last);
                        }
                    }
                    let mut vals = vec![];
                    for (i, fi) in pk.iter().enumerate() {
                        let fd = &td.fields[*fi];
                        let part = parts.get(i).map(|x| x.as_str()).unwrap_or("");
                        let evf = |n: &str| self.enum_ordinal_for_field(fd, n);
                        vals.push(text_to_store(fd, part, &evf, self.work_date)?);
                    }
                    Some(Val::RecordId(td.id, vals))
                })
            }
        }
    }

    fn miss(&mut self, what: String) -> R<Val> {
        *self.missing.entry(what.clone()).or_default() += 1;
        err(format!("Not implemented in bc-rust: {what}"))
    }

    pub fn builtin(&mut self, f: &mut Frame, name: &str, args: &[Expr]) -> R<Val> {
        let discard = std::mem::take(&mut self.discard);
        let l = name.to_ascii_lowercase();
        match l.as_str() {
            "error" => {
                let a = self.eval_subst_args(f, args)?;
                // a collectible ErrorInfo inside an ErrorBehavior::Collect scope is collected
                if let Some(Val::ErrorInfo(e)) = a.first() {
                    if self.collect_error(e) {
                        return Ok(Val::Void);
                    }
                }
                let msg = match a.first() {
                    Some(Val::ErrorInfo(m)) => m.borrow().message.clone(),
                    Some(v) => str_subst(&v.to_text(), &a[1..]),
                    None => String::new(),
                };
                err_code("Dialog", msg)
            }
            "message" => {
                let a = self.eval_subst_args(f, args)?;
                let m = str_subst(&a.first().map(|v| v.to_text()).unwrap_or_default(), a.get(1..).unwrap_or(&[]));
                self.ui_message(m)?;
                Ok(Val::Void)
            }
            "confirm" => {
                let a = self.eval_subst_args(f, args)?;
                let q = str_subst(&a.first().map(|v| v.to_text()).unwrap_or_default(), a.get(2..).unwrap_or(&[]));
                let default = a.get(1).map(|v| v.as_bool()).unwrap_or(true);
                if let Some(b) = self.host_confirm(&q, default) {
                    return Ok(Val::Bool(b));
                }
                Ok(Val::Bool(self.ui_confirm(q, default)?))
            }
            "strmenu" => {
                let a = self.eval_all(f, args)?;
                let options = a.first().map(|v| v.to_text()).unwrap_or_default();
                let default = a.get(1).map(|v| v.as_i64() as i32).unwrap_or(1);
                let instruction = a.get(2).map(|v| v.to_text()).unwrap_or_default();
                if let Some(c) = self.host_strmenu(&options, default, &instruction) {
                    return Ok(Val::Int(c));
                }
                Ok(Val::Int(self.ui_strmenu(options, default, instruction)?))
            }
            "strsubstno" => {
                let a = self.eval_subst_args(f, args)?;
                Ok(Val::Text(str_subst(&a.first().map(|v| v.to_text()).unwrap_or_default(), a.get(1..).unwrap_or(&[]))))
            }
            "format" => {
                // a record field or FieldRef formats like the field: Option fields show their caption
                let field = match args.first() {
                    Some(e) => self.field_of_expr(f, e)?,
                    None => None,
                };
                let mut a = self.eval_all(f, args)?;
                if let Some((r, fi)) = &field {
                    if let Some(first) = a.first_mut() {
                        *first = r.borrow().get_val(*fi);
                    }
                }
                let fmt = a.get(2).map(|v| v.as_i64()).unwrap_or(0);
                if let Some(Val::DateFormula(df)) = a.first() {
                    return self.format_date_formula(df, a.get(1), a.get(2)).map(Val::Text);
                }
                // Rec."Option Field"::Value formats like the field too
                let scope_field = match (&field, args.first()) {
                    (None, Some(e @ Expr::Scope(..))) => self.option_field_of_scope(f, e)?,
                    _ => None,
                };
                let mut s = match (a.first(), a.get(2), &field) {
                    (Some(v), Some(Val::Text(pic)), _) => format_picture(v, pic),
                    // Option and Enum values: standard formats 2 and 9 are the ordinal [src:A17.1]
                    (Some(Val::Opt { ord, .. }), _, _) if fmt == 2 || fmt == 9 => ord.to_string(),
                    (Some(v @ Val::Opt { .. }), _, Some((r, fi))) if fmt != 9 && fmt != 2 => self.format_field_val(r.borrow().field(*fi), v),
                    (Some(v @ Val::Opt { .. }), _, None) if fmt != 9 && fmt != 2 && scope_field.is_some() => self.format_field_val(scope_field.unwrap(), v),
                    (Some(v), _, _) => format_val(v, fmt),
                    _ => String::new(),
                };
                if let Some(len) = a.get(1).map(|v| v.as_i64()).filter(|n| *n > 0) {
                    s = s.chars().take(len as usize).collect();
                }
                Ok(Val::Text(s))
            }
            "evaluate" => {
                let lv = self.lvalue(f, &args[0])?;
                // Evaluate(FieldRef, Text) evaluates into the field
                let lv = match self.read_lval(&lv) {
                    Val::FieldRef(h, fi) => LVal::Field(h, fi),
                    _ => lv,
                };
                let text = self.eval(f, &args[1])?.to_text();
                let cur = self.read_lval(&lv);
                let df_target = match (&lv, &cur) {
                    (LVal::Field(r, i), _) => matches!(r.borrow().field(*i).ty, al_model::FieldType::DateFormula),
                    (_, Val::DateFormula(_)) => true,
                    _ => false,
                };
                if df_target {
                    let number = match args.get(2) {
                        Some(e) => Some(self.eval(f, e)?.as_i64()),
                        None => None,
                    };
                    return self.evaluate_date_formula(&lv, &text, number, discard);
                }
                let parsed = match (&lv, &cur) {
                    // a RecordID field: the formatted RecordID too (GetFilter / Format), not only the
                    // storage encoding
                    (LVal::Field(r, i), _) if matches!(r.borrow().field(*i).ty, al_model::FieldType::RecordId) => self.parse_record_id_text(&text),
                    (LVal::Field(r, i), _) => {
                        let fd = r.borrow().field(*i);
                        let evf = |n: &str| self.enum_ordinal_for_field(fd, n);
                        text_to_store(fd, &text, &evf, self.work_date).map(|v| to_val(fd, &v))
                    }
                    (_, Val::Int(_)) => text.trim().parse::<i32>().ok().map(Val::Int),
                    (_, Val::BigInt(_)) => text.trim().parse::<i64>().ok().map(Val::BigInt),
                    (_, Val::Dec(_)) => parse_num(text.trim()).map(Val::Dec),
                    (_, Val::Bool(_)) => match text.trim().to_ascii_lowercase().as_str() {
                        "yes" | "true" | "1" => Some(Val::Bool(true)),
                        "no" | "false" | "0" => Some(Val::Bool(false)),
                        _ => None,
                    },
                    (_, Val::Date(_)) => parse_date_text(&text, self.work_date).map(Val::Date),
                    (_, Val::Time(_)) => parse_time_text(&text).map(Val::Time),
                    (_, Val::DateTime(_)) => parse_datetime_text(&text).map(Val::DateTime),
                    (_, Val::Guid(_)) => parse_guid(&text).map(Val::Guid),
                    // Evaluate(RecordId, Format(RecordId)): "<table>: <key values>" back into the table's key
                    (_, Val::RecordId(..)) => self.parse_record_id_text(&text),
                    // an Enum variable takes a value name or caption (Format(e, 0, 9) gives the name) or an ordinal
                    (_, Val::Opt { enum_id: Some(e), .. }) if *e >= 0 => {
                        let t = text.trim();
                        let en = self.cat.enums.get(e);
                        t.parse::<i32>()
                            .ok()
                            .or_else(|| en.and_then(|en| en.values.iter().find(|v| v.name.trim().eq_ignore_ascii_case(t) || v.caption.trim().eq_ignore_ascii_case(t))).map(|v| v.ordinal as i32))
                            .map(|o| Val::Opt { ord: o, enum_id: Some(*e) })
                    }
                    // an Option variable with its members: caption, member name or number [src:A17.2]
                    (_, Val::Opt { enum_id: Some(e), .. }) if crate::optmeta::is_meta(*e) => {
                        crate::optmeta::get(*e).and_then(|m| m.ordinal_of(&text)).map(|o| Val::Opt { ord: o, enum_id: Some(*e) })
                    }
                    (_, Val::Opt { .. }) => text.trim().parse::<i32>().ok().map(|o| Val::Opt { ord: o, enum_id: None }),
                    _ => Some(Val::Text(text.clone())),
                };
                match parsed {
                    Some(v) => {
                        self.write_lval(&lv, v)?;
                        Ok(Val::Bool(true))
                    }
                    None if discard => err(format!("The value '{text}' can't be evaluated into type {}.", cur.type_name())),
                    None => Ok(Val::Bool(false)),
                }
            }
            "copystr" => {
                let a = self.eval_all(f, args)?;
                let s: Vec<char> = a[0].to_text().chars().collect();
                let pos = (a[1].as_i64().max(1) - 1) as usize;
                let len = a.get(2).map(|v| v.as_i64().max(0) as usize).unwrap_or(usize::MAX);
                Ok(Val::Text(s.iter().skip(pos).take(len).collect()))
            }
            "strlen" => Ok(Val::Int(self.eval(f, &args[0])?.to_text().chars().count() as i32)),
            "strpos" => {
                let a = self.eval_all(f, args)?;
                let (s, sub) = (a[0].to_text(), a[1].to_text());
                Ok(Val::Int(match s.find(&sub) {
                    Some(i) if !sub.is_empty() => s[..i].chars().count() as i32 + 1,
                    _ => 0,
                }))
            }
            "uppercase" => Ok(Val::Text(self.eval(f, &args[0])?.to_text().to_uppercase())),
            "lowercase" => Ok(Val::Text(self.eval(f, &args[0])?.to_text().to_lowercase())),
            "delchr" => {
                let a = self.eval_all(f, args)?;
                let s = a[0].to_text();
                let wh = a.get(1).map(|v| v.to_text()).unwrap_or_else(|| "=".into());
                let which: Vec<char> = a.get(2).map(|v| v.to_text()).unwrap_or_else(|| " ".into()).chars().collect();
                let mut chars: Vec<char> = s.chars().collect();
                if wh.contains('=') {
                    chars.retain(|c| !which.contains(c));
                } else {
                    if wh.contains('<') {
                        while chars.first().is_some_and(|c| which.contains(c)) {
                            chars.remove(0);
                        }
                    }
                    if wh.contains('>') {
                        while chars.last().is_some_and(|c| which.contains(c)) {
                            chars.pop();
                        }
                    }
                }
                Ok(Val::Text(chars.into_iter().collect()))
            }
            "padstr" => {
                let a = self.eval_all(f, args)?;
                let mut s = a[0].to_text();
                let len = a[1].as_i64().max(0) as usize;
                let fill = a.get(2).map(|v| v.to_text()).unwrap_or_else(|| " ".into()).chars().next().unwrap_or(' ');
                let n = s.chars().count();
                if n > len {
                    s = s.chars().take(len).collect();
                } else {
                    s.extend(std::iter::repeat(fill).take(len - n));
                }
                Ok(Val::Text(s))
            }
            "incstr" => {
                let a = self.eval_all(f, args)?;
                let by = a.get(1).map(|v| v.as_i64()).unwrap_or(1);
                Ok(Val::Text(inc_str_by(&a[0].to_text(), by)))
            }
            "selectstr" => {
                let a = self.eval_all(f, args)?;
                let n = a[0].as_i64();
                let src = a[1].to_text();
                let parts: Vec<&str> = src.split(',').collect();
                // an index outside the list is an error in BC (ALSystemString.ALSelectString)
                if n < 1 || n as usize > parts.len() {
                    return err(format!("SelectStr: the string '{src}' has no value at position {n}."));
                }
                Ok(Val::Text(parts[n as usize - 1].to_string()))
            }
            "convertstr" => {
                let a = self.eval_all(f, args)?;
                let (from, to): (Vec<char>, Vec<char>) = (a[1].to_text().chars().collect(), a[2].to_text().chars().collect());
                Ok(Val::Text(a[0].to_text().chars().map(|c| from.iter().position(|x| *x == c).and_then(|i| to.get(i).copied()).unwrap_or(c)).collect()))
            }
            "insstr" => {
                let a = self.eval_all(f, args)?;
                let mut s: Vec<char> = a[0].to_text().chars().collect();
                let pos = ((a[2].as_i64().max(1) - 1) as usize).min(s.len());
                let sub: Vec<char> = a[1].to_text().chars().collect();
                s.splice(pos..pos, sub);
                Ok(Val::Text(s.into_iter().collect()))
            }
            "delstr" => {
                let a = self.eval_all(f, args)?;
                let mut s: Vec<char> = a[0].to_text().chars().collect();
                let pos = ((a[1].as_i64().max(1) - 1) as usize).min(s.len());
                let len = a.get(2).map(|v| v.as_i64().max(0) as usize).unwrap_or(s.len());
                let end = (pos + len).min(s.len());
                s.drain(pos..end);
                Ok(Val::Text(s.into_iter().collect()))
            }
            "maxstrlen" => {
                let lv = self.lvalue(f, &args[0])?;
                Ok(Val::Int(match &lv {
                    LVal::Field(r, i) => match &r.borrow().field(*i).ty {
                        al_model::FieldType::Code(n) | al_model::FieldType::Text(n) => *n as i32,
                        _ => 0,
                    },
                    LVal::Slot(_, Some(TypeRef::Simple { len: Some(n), .. })) => *n as i32,
                    _ => i32::MAX,
                }))
            }
            "round" => {
                let a = self.eval_all(f, args)?;
                let x = a[0].as_dec();
                let prec = a.get(1).map(|v| v.as_dec()).unwrap_or(Decimal::new(1, 2));
                let dir = a.get(2).map(|v| v.to_text()).unwrap_or_else(|| "=".into());
                Ok(Val::Dec(crate::interp::dec_checked(round_dec(x, prec, &dir))?))
            }
            "abs" => {
                let v = self.eval(f, &args[0])?;
                Ok(match v {
                    Val::Int(i) => Val::Int(i.abs()),
                    Val::BigInt(i) => Val::BigInt(i.abs()),
                    o => Val::Dec(o.as_dec().abs()),
                })
            }
            "power" => {
                let a = self.eval_all(f, args)?;
                Ok(Val::Dec(crate::interp::dec_checked(a[0].as_dec().checked_powd(a[1].as_dec()))?))
            }
            "random" => {
                // BC ALRandom: a negative bound counts as its absolute value, 0 as 1
                let n = (self.eval(f, &args[0])?.as_i64().unsigned_abs().clamp(1, i32::MAX as u64)) as i32;
                Ok(Val::Int(random_below(n) + 1))
            }
            // Randomize(Seed) seeds Random() where the caller enables it (al-test --bc-runner, see
            // set_randomize_seeds); otherwise, and for Randomize(), nothing changes
            "randomize" => {
                if let Some(a) = args.first() {
                    let v = self.eval(f, a)?.as_i64();
                    if RANDOMIZE_SEEDS.with(|c| c.get()) {
                        // BC: Session.Random = new Random(seed)
                        RANDOM_STATE.with(|r| *r.borrow_mut() = Some(NetRandom::new(v as i32)));
                    }
                }
                Ok(Val::Void)
            }
            "today" => Ok(Val::Date(today())),
            "workdate" => {
                if let Some(a) = args.first() {
                    let d = self.eval(f, a)?;
                    if let Val::Date(d) = d {
                        if d != 0 {
                            self.work_date = d;
                        }
                    }
                }
                Ok(Val::Date(self.work_date))
            }
            "time" => {
                let now = crate::clock::local(crate::clock::now_ms(crate::clock::at(self, self.steps))).time();
                Ok(Val::Time(chrono::Timelike::num_seconds_from_midnight(&now) as i32 * 1000 + 1))
            }
            "currentdatetime" => Ok(Val::DateTime(crate::clock::now_ms(crate::clock::at(self, self.steps)))),
            "createdatetime" => {
                let a = self.eval_all(f, args)?;
                let d = match a[0] {
                    Val::Date(d) => d,
                    _ => 0,
                };
                let t = match a[1] {
                    Val::Time(t) => t,
                    _ => 0,
                };
                Ok(Val::DateTime(date_time_to_ms(d, t)))
            }
            "dt2date" => {
                let v = self.eval(f, &args[0])?;
                Ok(Val::Date(match v {
                    Val::DateTime(t) if t != bc_store::DATETIME_UNDEFINED => chrono::DateTime::from_timestamp_millis(t).map(|d| from_naive(d.date_naive())).unwrap_or(0),
                    _ => 0,
                }))
            }
            "dt2time" => {
                let v = self.eval(f, &args[0])?;
                Ok(Val::Time(match v {
                    Val::DateTime(t) if t != bc_store::DATETIME_UNDEFINED => (t.rem_euclid(86_400_000)) as i32 + 1,
                    _ => 0,
                }))
            }
            "calcdate" => {
                let a = self.eval_all(f, args)?;
                // CalcDate(Formula) is relative to the system date (Today), not WorkDate: planning's
                // BucketSizeInDays := CalcDate(BucketSize) - Today
                let d = match a.get(1) {
                    Some(Val::Date(d)) => *d,
                    None => today(),
                    _ => self.work_date,
                };
                let lang = self.global_language;
                // Q4: a DateFormula value is its stored text (invariant letters); a Text formula
                // is user text in the session language
                let r = match &a[0] {
                    Val::DateFormula(stored) => self.df_cache.calc_stored(stored, lang, d),
                    v => self.df_cache.calc_text(&v.to_text(), lang, d),
                };
                match r {
                    Ok(v) => Ok(Val::Date(v)),
                    Err(e) => df_error(e, lang),
                }
            }
            "date2dmy" | "date2dwy" => {
                let a = self.eval_all(f, args)?;
                let d = match a[0] {
                    Val::Date(d) => d,
                    _ => 0,
                };
                let Some(n) = days(d) else { return err("Date2DMY requires a defined date") };
                let w = a[1].as_i64();
                Ok(Val::Int(if l == "date2dmy" {
                    match w {
                        1 => n.day() as i32,
                        2 => n.month() as i32,
                        _ => n.year(),
                    }
                } else {
                    match w {
                        1 => n.weekday().number_from_monday() as i32,
                        2 => n.iso_week().week() as i32,
                        _ => n.iso_week().year(),
                    }
                }))
            }
            "dmy2date" => {
                let a = self.eval_all(f, args)?;
                let y = a.get(2).map(|v| v.as_i64() as i32).unwrap_or_else(|| days(self.work_date).map(|n| n.year()).unwrap_or(2026));
                let m = a.get(1).map(|v| v.as_i64() as u32).unwrap_or(1);
                match date_from_ymd(y, m, a[0].as_i64() as u32) {
                    Some(d) => Ok(Val::Date(d)),
                    None => err("Invalid date in DMY2Date"),
                }
            }
            "dwy2date" => {
                let a = self.eval_all(f, args)?;
                let y = a.get(2).map(|v| v.as_i64() as i32).unwrap_or(2026);
                let w = a.get(1).map(|v| v.as_i64() as u32).unwrap_or(1);
                let wd = chrono::Weekday::try_from((a[0].as_i64() - 1).clamp(0, 6) as u8).unwrap_or(chrono::Weekday::Mon);
                match chrono::NaiveDate::from_isoywd_opt(y, w, wd) {
                    Some(n) => Ok(Val::Date(from_naive(n))),
                    None => err("Invalid date in DWY2Date"),
                }
            }
            "closingdate" => match self.eval(f, &args[0])? {
                Val::Date(d) => Ok(Val::Date(d | 1)),
                o => Ok(o),
            },
            "normaldate" => match self.eval(f, &args[0])? {
                Val::Date(d) => Ok(Val::Date(d & !1)),
                o => Ok(o),
            },
            "createguid" => Ok(Val::Guid(crate::clock::new_guid())),
            "isnullguid" => Ok(Val::Bool(matches!(self.eval(f, &args[0])?, Val::Guid(g) if g == [0; 16]) || false)),
            "userid" => Ok(Val::Text(self.user_id.clone())),
            "usersecurityid" => Ok(Val::Guid(self.user_sid)),
            // BC's NavUserPassword hash (crate::userauth); with a used return value a failure gives false
            "setuserpassword" | "changeuserpassword" => {
                let a = self.eval_all(f, args)?;
                let text = |i: usize| a.get(i).map(|v| v.to_text()).unwrap_or_default();
                let r = if l == "setuserpassword" {
                    let sid = match a.first() {
                        Some(Val::Guid(g)) => *g,
                        _ => [0; 16],
                    };
                    crate::userauth::al_set_user_password(self, &sid, &text(1))
                } else {
                    crate::userauth::change_user_password(self, &text(0), &text(1)).map_err(|e| e.unwrap_or_else(|| "The password that you entered does not match your current password.".into()))
                };
                match r {
                    Ok(()) => Ok(Val::Bool(true)),
                    Err(m) if discard => err(m),
                    Err(_) => Ok(Val::Bool(false)),
                }
            }
            "companyname" => Ok(Val::Text(self.company.clone())),
            "tenantid" => Ok(Val::Text(self.sys.env.tenant_id().into())),
            "sessionid" => Ok(Val::Int(crate::workers::current_session_id())),
            "serviceinstanceid" => Ok(Val::Int(crate::workers::service_instance_id())),
            "startsession" => self.start_session(f, args),
            "copystream" => self.copy_stream(f, args),
            "getlasterrorcode" => Ok(Val::Text(self.last_error_code.clone())),
            "getlasterrorcallstack" => Ok(Val::Text(self.last_error_stack.clone())),
            "bindsubscription" | "unbindsubscription" => {
                let v = self.eval(f, &args[0])?;
                let Val::Object(inst) = v else { return err(format!("{name} requires a codeunit variable")) };
                // bindings of instances that are gone have ended (crate::interp::Session::bound)
                self.bound.retain(|w| w.strong_count() > 0);
                let pos = self.bound.iter().position(|w| w.upgrade().is_some_and(|i| Rc::ptr_eq(&i, &inst)));
                if l == "bindsubscription" {
                    if pos.is_some() {
                        // a call whose result is used returns false (BC DataError.TrapError)
                        if !discard {
                            return Ok(Val::Bool(false));
                        }
                        return err("The object is already bound to the event subscriptions.");
                    }
                    // an instance of this codeunit that this frame cleared while bound (kept in a
                    // "<bound instance>" local, see Clear) is replaced by the new binding
                    for v in f.locals.iter().filter(|v| &*v.name == "<bound instance>") {
                        let old = v.slot.borrow().clone();
                        if let Val::Object(oi) = old {
                            if oi.code == inst.code {
                                self.bound.retain(|w| w.upgrade().is_some_and(|i| !Rc::ptr_eq(&i, &oi)));
                                *v.slot.borrow_mut() = Val::Void;
                            }
                        }
                    }
                    self.bound.push(Rc::downgrade(&inst));
                } else if let Some(p) = pos {
                    self.bound.remove(p);
                }
                Ok(Val::Bool(true))
            }
            "geturl" => {
                let a = self.eval_all(f, args)?;
                Ok(Val::Text(self.get_url(&a)))
            }
            // the session's server temp folder (crate::files), where BC's is <user folder>\TEMP\
            "temporarypath" => Ok(Val::Text(crate::files::TEMP_DIR.into())),
            // ImportStreamWithUrlAccess / GetDocumentUrl: Tenant Media with a security token (crate::media)
            "importstreamwithurlaccess" => {
                let a = self.eval_all(f, args)?;
                Ok(Val::Guid(self.media_create_url_access(&a)?))
            }
            "getdocumenturl" => {
                let a = self.eval_all(f, args)?;
                let id = match a.first() {
                    Some(Val::Guid(g)) => *g,
                    _ => [0; 16],
                };
                Ok(Val::Text(self.media_document_url(id)))
            }
            "serialnumber" | "applicationpath" => {
                for a in args {
                    let _ = self.eval(f, a)?;
                }
                crate::assume::hit("System.SerialNumber");
                Ok(Val::Text(String::new()))
            }
            // test sessions run in a client session, so UI is allowed and goes to the handlers
            "guiallowed" => Ok(Val::Bool(self.in_test() || self.gui_allowed)),
            "isservicetier" => Ok(Val::Bool(true)),
            "currentclienttype" => Ok(Val::Opt { ord: crate::interp::system_option_ord("clienttype", self.client_type), enum_id: None }),
            "defaultclienttype" => Ok(Val::Opt { ord: crate::interp::system_option_ord("clienttype", "web"), enum_id: None }),
            "globallanguage" => {
                if let Some(a) = args.first() {
                    let v = self.eval(f, a)?.as_i64() as i32;
                    self.global_language = v;
                }
                Ok(Val::Int(self.global_language))
            }
            "windowslanguage" => Ok(Val::Int(1033)),
            // Database.LockTimeout([Boolean]): off lets the session wait for locks without limit
            // (long batch jobs such as cost adjustment turn it off); docs/job-queue.md
            "locktimeout" => {
                if let Some(a) = args.first() {
                    let on = self.eval(f, a)?.as_bool();
                    self.db.set_lock_timeout(on);
                }
                Ok(Val::Bool(self.db.lock_timeout()))
            }
            // LockTimeoutDuration([Integer]): the session's lock timeout in seconds, one value with
            // LockTimeout (BC: ALDatabase.ALLockTimeoutDuration; 0 or less is no limit, read back as -1)
            "locktimeoutduration" => {
                if let Some(a) = args.first() {
                    let secs = self.eval(f, a)?.as_i64();
                    self.db.set_lock_timeout_duration(secs);
                }
                Ok(Val::Int(self.db.lock_timeout_duration() as i32))
            }
            "exportobjects" | "debugmode" => {
                for a in args {
                    let _ = self.eval(f, a)?;
                }
                crate::assume::hit("System.ExportObjects");
                Ok(Val::Bool(true))
            }
            // CurrentTransactionType([Type]): the running transaction's type (crate::sysfuncs TxnState)
            "currenttransactiontype" => {
                if let Some(a) = args.first() {
                    let ty = self.eval(f, a)?.as_i64() as i32;
                    self.set_transaction_type(ty, false)?;
                }
                Ok(Val::Opt { ord: self.txn().ty, enum_id: None })
            }
            "currentexecutionmode" => Ok(Val::Opt { ord: 0, enum_id: None }),
            "isnull" => {
                let v = self.eval(f, &args[0])?;
                Ok(Val::Bool(matches!(v, Val::Opaque(_) | Val::Void) || crate::dotnet::is_null(&v)))
            }
            "hascollation" => {
                crate::assume::hit("System.HasCollation");
                Ok(Val::Bool(false))
            }
            // a DotNet System.Exception in BC (GetBaseException, Message ...), not an ErrorInfo
            // after a failed DotNet call: the invoke exception with the .NET one inside (crate::dotnet)
            "getlasterrorobject" => Ok(crate::dotnet::last_error_object(&self.last_error, &self.last_error_code).unwrap_or(Val::Opaque("Object"))),
            // StrCheckSum(Digits [, WeightString] [, Modulus = 10]): (Modulus - sum(digit * weight)
            // mod Modulus) mod Modulus; weights default to 1 (GLN/EAN check digits use '1313...')
            "strchecksum" => {
                let a = self.eval_all(f, args)?;
                let s = a.first().map(|v| v.to_text()).unwrap_or_default();
                let w = a.get(1).map(|v| v.to_text()).unwrap_or_default();
                let m = a.get(2).map(|v| v.as_i64()).unwrap_or(10).max(1);
                let mut sum: i64 = 0;
                for (i, c) in s.chars().enumerate() {
                    let d = c.to_digit(10).ok_or_else(|| Flow::Err(Box::new(AlError::new(format!("The string '{s}' contains characters that are not numbers.")))))? as i64;
                    let wt = w.chars().nth(i).and_then(|x| x.to_digit(10)).map(|x| x as i64).unwrap_or(if w.is_empty() { 1 } else { 0 });
                    sum += d * wt;
                }
                Ok(Val::Int(((m - sum % m) % m) as i32))
            }
            // ApplicationArea([Text]): the session's application area (LogInManagement sets it
            // from the company's Application Area Setup; LibraryApplicationArea changes it)
            "applicationarea" => {
                if let Some(a) = args.first() {
                    let v = self.eval(f, a)?.to_text();
                    self.sys.app_area = Some(v);
                }
                Ok(Val::Text(self.sys.app_area.clone().unwrap_or_else(|| "#All,#Basic,#Suite,#Advanced".into())))
            }
            "getexecutioncontext" | "getcurrentmoduleexecutioncontext" | "getmoduleexecutioncontext" => {
                Ok(Val::Opt { ord: crate::interp::system_option_ord("executioncontext", "normal"), enum_id: None })
            }
            "commit" => {
                self.al_commit()?;
                Ok(Val::Void)
            }
            "clear" => {
                let lv = self.lvalue(f, &args[0])?;
                let cur = self.read_lval(&lv);
                match (&lv, cur) {
                    // Clear(RecordRef) rebinds the variable to a new, unopened RecordRef
                    (LVal::Slot(s, Some(TypeRef::Simple { name, .. })), Val::Record(_)) if name.eq_ignore_ascii_case("recordref") => {
                        let v = self.new_recordref();
                        *s.borrow_mut() = v;
                    }
                    // Clear(Variant) empties the Variant; a record it holds stays as it is (BC
                    // NavVariant; Workflow Record Management.RestoreRecord relies on it)
                    (LVal::Slot(s, Some(TypeRef::Simple { name, .. })), _) if name.eq_ignore_ascii_case("variant") => {
                        *s.borrow_mut() = Val::Void;
                    }
                    (LVal::Elem(a, i, Some(TypeRef::Simple { name, .. })), _) if name.eq_ignore_ascii_case("variant") => {
                        if let Some(x) = a.borrow_mut().get_mut(*i) {
                            *x = Val::Void;
                        }
                    }
                    (_, Val::Record(r)) => self.clear_record(&r),
                    // Clear(Codeunit) detaches the variable from its instance. A bound instance stays
                    // alive (and bound) until the frame that clears it ends: BC disposes an instance
                    // with the scope that owns it, not with its last reference (Test Sales Post
                    // Preview binds Sales-Post (Yes/No), clears it, and previews again)
                    (LVal::Slot(s, ty), Val::Object(inst)) if self.bound.iter().any(|w| w.upgrade().is_some_and(|i| Rc::ptr_eq(&i, &inst))) => {
                        let v = match ty {
                            Some(t) => self.default_val(t),
                            None => clear_like(&Val::Object(inst.clone())),
                        };
                        f.locals.push(Var { name: "<bound instance>".into(), slot: Rc::new(RefCell::new(Val::Object(inst))), ty: None });
                        *s.borrow_mut() = v;
                    }
                    // an array: its elements, in place, whatever length the declaration says
                    (lv, Val::Array(a, _)) => {
                        let elem_ty = match lv {
                            LVal::Slot(_, Some(TypeRef::Array { elem, .. })) => Some(&**elem),
                            _ => None,
                        };
                        let n = a.borrow().len();
                        for k in 0..n {
                            let cur = a.borrow()[k].clone();
                            let v = match (&cur, elem_ty) {
                                // an array of Variant: each element emptied, its record kept
                                (_, Some(TypeRef::Simple { name, .. })) if name.eq_ignore_ascii_case("variant") => Val::Void,
                                (Val::Record(r), _) => {
                                    self.clear_record(r);
                                    continue;
                                }
                                (_, Some(t)) => self.default_val(t),
                                _ => clear_like(&cur),
                            };
                            a.borrow_mut()[k] = v;
                        }
                    }
                    (LVal::Slot(s, ty), cur) => {
                        let v = match ty {
                            Some(t) => self.default_val(t),
                            None => clear_like(&cur),
                        };
                        *s.borrow_mut() = v;
                    }
                    (lv, cur) => {
                        let v = clear_like(&cur);
                        self.write_lval(lv, v)?;
                    }
                }
                Ok(Val::Void)
            }
            "clearall" => {
                for v in f.globals.vars.clone() {
                    if let Some(t) = v.ty {
                        // Record variables are cleared in place like Clear(Rec): a temporary
                        // table keeps its rows (BC's generated OnClear calls NavRecord.Clear)
                        let rec = match &*v.slot.borrow() {
                            Val::Record(r) if !matches!(t, TypeRef::Simple { name, .. } if name.eq_ignore_ascii_case("recordref")) => Some(r.clone()),
                            _ => None,
                        };
                        match rec {
                            Some(r) => self.clear_record(&r),
                            None => {
                                let d = self.default_val(t);
                                *v.slot.borrow_mut() = d;
                            }
                        }
                    }
                }
                Ok(Val::Void)
            }
            "clearlasterror" => {
                self.last_error.clear();
                self.last_error_code.clear();
                self.last_error_stack.clear();
                Ok(Val::Void)
            }
            "getlasterrortext" => Ok(Val::Text(self.last_error.clone())),
            "hyperlink" => {
                let url = self.eval(f, &args[0])?.to_text();
                self.ui_hyperlink(url)?;
                Ok(Val::Void)
            }
            // ApplicationIdentifier(): 'NAV' on-premises, 'FIN' online (crate::environment)
            "applicationidentifier" => Ok(Val::Text(self.sys.env.kind.application_identifier().into())),
            // SelectLatestVersion([TableNo]): there are no cached reads to refresh
            "sleep" => {
                // BC really waits (Thread.Sleep). Tests rely on it to separate timestamps: the CRM
                // synch tests Sleep(20) between two jobs and then take the last by SystemModifiedAt.
                let ms = match args.first() {
                    Some(a) => self.eval(f, a)?.as_i64(),
                    None => 0,
                };
                // the session yields: due scheduled tasks run now (crate::tasks); on the deterministic
                // clock (crate::clock) their work overlaps the sleep, which moves the clock on
                // instead of waiting
                let start = crate::clock::peek_ms();
                self.run_due_tasks()?;
                if ms > 0 && !crate::clock::sleep_from(start, ms) {
                    std::thread::sleep(std::time::Duration::from_millis(ms as u64));
                }
                Ok(Val::Void)
            }
            // SelectLatestVersion([TableNo]): BC flushes the session's data cache (FlushDataCache)
            "selectlatestversion" => {
                let t = match args.first() {
                    Some(a) => Some(self.eval(f, a)?.as_i64()),
                    None => None,
                };
                self.db.flush_read_cache(t);
                Ok(Val::Void)
            }
            "yield" => {
                for a in args {
                    let _ = self.eval(f, a)?;
                }
                Ok(Val::Void)
            }
            "captionclasstranslate" => {
                let s = self.eval(f, &args[0])?.to_text();
                Ok(Val::Text(self.caption_class_translate(&s)?))
            }
            "arraylen" => match self.eval(f, &args[0])? {
                Val::Array(a, dims) => {
                    let dim = if args.len() > 1 { self.eval(f, &args[1])?.as_i64() as usize } else { 1 };
                    Ok(Val::Int(dims.get(dim - 1).copied().unwrap_or(a.borrow().len()) as i32))
                }
                _ => Ok(Val::Int(0)),
            },
            "copyarray" => {
                let lv = self.lvalue(f, &args[0])?;
                let src = self.eval(f, &args[1])?;
                let pos = self.eval(f, &args[2])?.as_i64().max(1) as usize - 1;
                if let (Val::Array(dst, _), Val::Array(s, _)) = (self.read_lval(&lv), src) {
                    let s = s.borrow().clone();
                    let mut d = dst.borrow_mut();
                    let len = if args.len() > 3 { self.eval(f, &args[3])?.as_i64() as usize } else { s.len() - pos };
                    for (k, v) in s.into_iter().skip(pos).take(len).enumerate() {
                        if k < d.len() {
                            d[k] = v;
                        }
                    }
                }
                Ok(Val::Void)
            }
            // CompressArray: non-empty strings to the front (in order), the rest empty; returns
            // how many are non-empty [src:A30.1]
            "compressarray" => match self.eval(f, &args[0])? {
                Val::Array(a, dims) if dims.len() <= 1 => {
                    let mut a = a.borrow_mut();
                    let mut out: Vec<Val> = a.iter().filter(|x| !x.to_text().is_empty()).cloned().collect();
                    let n = out.len();
                    if let Some(e) = a.iter().find(|x| x.to_text().is_empty()).cloned() {
                        out.resize(a.len(), e);
                    }
                    *a = out;
                    Ok(Val::Int(n as i32))
                }
                Val::Array(..) => err("CompressArray does not support multidimensional arrays"),
                _ => Ok(Val::Int(0)),
            },
            "variant2date" => Ok(self.eval(f, &args[0])?),
            "char" => Ok(Val::Char(char::from_u32(self.eval(f, &args[0])?.as_i64() as u32).unwrap_or('\0'))),
            // RoundDateTime(DateTime [, Precision = 1000] [, Direction = '=']): BC
            // ALSystemDate.ALRoundDateTime rounds the client-local time in milliseconds since
            // 0000-01-01 (the CSide datetime minus 1); '=' rounds a half up
            "roundDateTime" | "rounddatetime" => {
                let a = self.eval_all(f, args)?;
                let t = match a.first() {
                    Some(Val::DateTime(t)) if *t != bc_store::DATETIME_UNDEFINED => *t,
                    Some(v) => return Ok(v.clone()),
                    None => return err("RoundDateTime needs a DateTime."),
                };
                let p = a.get(1).map(|v| v.as_i64()).unwrap_or(1000);
                if p <= 0 {
                    return err(format!("The value {p} of parameter 2 in function ROUNDDATETIME is out of range."));
                }
                let dir = a.get(2).map(|v| v.to_text()).unwrap_or_else(|| "=".into());
                let d = match dir.as_str() {
                    "<" | "=" | ">" => dir.as_bytes()[0],
                    _ => return err(format!("The value {dir} of parameter 3 in function ROUNDDATETIME is out of range.")),
                };
                Ok(Val::DateTime(round_datetime(t, p, d)))
            }
            "codecoverageinclude" | "codecoveragelog" | "codecoveragerefresh" => {
                crate::assume::hit("System.CodeCoverage");
                Ok(Val::Void)
            }
            // CreateEncryptionKey, Encrypt, Decrypt, EncryptionEnabled ... (crate::enckey)
            _ if crate::enckey::is_enc_builtin(&l) => self.enc_builtin(f, &l, args),
            // Sid, table connections, IsSessionActive (crate::sysfuncs)
            _ if crate::sysfuncs::is_sys_builtin(&l) => self.sys_builtin(f, &l, args),
            // Download / DownloadFromStream / Upload / UploadIntoStream: the client's answer (crate::filexfer)
            _ if crate::filexfer::is_transfer(&l) => self.file_transfer(f, &l, args),
            _ => match self.net_builtin(f, &l, args)? {
                Some(v) => Ok(v),
                // the Exists / Erase / Rename file functions (crate::files)
                None if matches!(l.as_str(), "exists" | "erase" | "rename") => match self.file_static(f, &l, args, false)? {
                    Some(v) => Ok(v),
                    None => self.miss(format!("function {name}")),
                },
                None => self.miss(format!("function {name}")),
            },
        }
    }

    pub fn static_call(&mut self, f: &mut Frame, class: &str, name: &str, args: &[Expr]) -> R<Val> {
        let discard = std::mem::take(&mut self.discard);
        let m = name.to_ascii_lowercase();
        match (class, m.as_str()) {
            ("codeunit", "run") => {
                let id = self.eval(f, &args[0])?.as_i64();
                let rec = match args.get(1) {
                    Some(a) => match self.eval(f, a)? {
                        Val::Record(r) => Some(r),
                        _ => None,
                    },
                    None => None,
                };
                if discard {
                    self.run_codeunit(id, rec)?;
                    return Ok(Val::Bool(true));
                }
                // with a return value: errors are caught and the codeunit's writes rolled back
                let mark = self.db.savepoint().map_err(Self::store_err)?;
                match self.trapped(|s| s.run_codeunit(id, rec)) {
                    Ok(()) => Ok(Val::Bool(true)),
                    Err(Flow::Err(e)) => {
                        self.note_error(&e);
                        self.rollback_trapped(mark)?;
                        Ok(Val::Bool(false))
                    }
                    Err(o) => Err(o),
                }
            }
            ("page", "getbackgroundparameters") | ("page", "setbackgroundtaskresult") => self.pbt_static(f, &m, args),
            ("page", "run") | ("page", "runmodal") => {
                let id = self.eval(f, &args[0])?;
                let id = match id {
                    Val::Text(t) => self.cat.id_of(ObjectKind::Page, &t).unwrap_or(0),
                    v => v.as_i64(),
                };
                let lv = match args.get(1) {
                    Some(a) => Some(self.lvalue(f, a)?),
                    None => None,
                };
                let rec = match lv.as_ref().map(|lv| self.read_lval(lv)) {
                    Some(Val::Record(r)) => Some(r),
                    _ => None,
                };
                let id = if id == 0 { rec.as_ref().and_then(|r| self.lookup_page_of(r.borrow().tdef)).unwrap_or(0) } else { id };
                let view = rec.as_ref().map(crate::record::copy_record);
                // static Page.RunModal on a list or dialog page whose result is used is a lookup: OK
                // returns LookupOK (and the selected record), which is what BaseApp compares with. BC's
                // compiler passes isLookup = the call is not a statement (CodeAnalysis
                // PageClassTypeSymbol.EmitRunModalArguments), so `Page.RunModal(0, Rec);` is no lookup
                let ui_inst = match self.cat.pages.get(&id) {
                    Some(pd) if m == "runmodal" && !discard && matches!(pd.page_type.to_ascii_lowercase().as_str(), "list" | "worksheet" | "standarddialog") => {
                        let inst = crate::interp::Instance::new(pd.code);
                        inst.lookup.set(true);
                        Some(inst)
                    }
                    _ => None,
                };
                let act = match self.host_run_page(id, None, view.clone(), m == "runmodal") {
                    Some(r) => r?,
                    None => self.ui_run_page(id, ui_inst, view.clone(), m == "runmodal")?,
                };
                // the selected record comes back on LookupOK
                if let (Some(r), Some(v)) = (&rec, &view) {
                    if equals(&act, &Val::Opt { ord: crate::interp::system_option_ord("action", "lookupok"), enum_id: None }) {
                        let row = v.borrow().row.clone();
                        r.borrow_mut().row = row;
                    }
                }
                Ok(act)
            }
            ("report", _) => self.report_static(f, &m, args),
            ("xmlport", "import") | ("xmlport", "export") => self.xmlport_static(f, &m, args),
            ("currxmlport", _) => self.curr_xmlport(f, &m, args),
            ("xmlport", "run") => {
                let a = self.eval_all(f, args)?;
                crate::assume::hit("Xmlport.Run");
                self.messages.push(format!("(UI) {class} {} requested", a.first().map(|v| v.to_text()).unwrap_or_default()));
                Ok(Val::Opt { ord: crate::interp::system_option_ord("action", "lookupcancel"), enum_id: None })
            }
            ("database", "companyname") | ("companyproperty", "displayname") | ("companyproperty", "urlname") => Ok(Val::Text(self.company.clone())),
            ("database", "tenantid") => Ok(Val::Text(self.sys.env.tenant_id().into())),
            ("database", "serviceinstanceid") => Ok(Val::Int(crate::workers::service_instance_id())),
            ("database", "sessionid") => Ok(Val::Int(crate::workers::current_session_id())),
            ("database", "userid") => Ok(Val::Text(self.user_id.clone())),
            ("database", "usersecurityid") => Ok(Val::Guid(self.user_sid)),
            ("database", "commit") => self.builtin(f, "commit", args),
            ("database", "selectlatestversion") => {
                let t = match args.first() {
                    Some(a) => Some(self.eval(f, a)?.as_i64()),
                    None => None,
                };
                self.db.flush_read_cache(t);
                Ok(Val::Bool(true))
            }
            ("database", "locktimeout") => self.builtin(f, "locktimeout", args),
            ("database", "locktimeoutduration") => self.builtin(f, "locktimeoutduration", args),
            ("database", "alterkey") => {
                crate::assume::hit("Database.AlterKey");
                Ok(Val::Bool(true))
            }
            ("database", "iswritetransaction") | ("database", "isinwritetransaction") => Ok(Val::Bool(self.db.in_write())),
            ("database", "lastusedrowversion") | ("database", "minimumactiverowversion") => {
                crate::assume::hit("Database.RowVersion");
                Ok(Val::BigInt(0))
            }
            ("session", "logmessage") | ("session", "logsecurityaudit") | ("session", "logauditmessage") | ("telemetry", "logmessage") => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.eval(f, a)?);
                }
                if let (Some(cb), true) = (self.on_log.as_mut(), m == "logmessage" && vals.len() >= 2) {
                    cb(&vals[0].to_text(), &vals[1].to_text());
                }
                Ok(Val::Void)
            }
            ("session", "currentclienttype") => Ok(Val::Opt { ord: crate::interp::system_option_ord("clienttype", self.client_type), enum_id: None }),
            ("session", "defaultclienttype") => Ok(Val::Opt { ord: crate::interp::system_option_ord("clienttype", "web"), enum_id: None }),
            // Install while Session::install_apps runs (install.rs)
            ("session", "getcurrentmoduleexecutioncontext") | ("session", "getmoduleexecutioncontext") | ("session", "getexecutioncontext") => self.execution_context(f, &m, args),
            ("session", "applicationidentifier") => Ok(Val::Text(self.sys.env.kind.application_identifier().into())),
            // AL's Session has no CurrentSessionId method [src:A35.1]: answer like SessionId()
            ("session", "currentsessionid") => Ok(Val::Int(crate::workers::current_session_id())),
            // with bc serve's background pool (crate::workers) these are real sessions
            ("session", "startsession") if crate::workers::pool().is_some() && self.test.is_none() => self.start_session(f, args),
            ("session", "issessionactive") if crate::workers::pool().is_some() && !args.is_empty() => {
                let id = self.eval(f, &args[0])?.as_i64();
                Ok(Val::Bool(id == crate::workers::current_session_id() as i64 || crate::workers::is_active(id)))
            }
            ("session", "startsession") | ("session", "isSessionActive") | ("session", "issessionactive") => {
                crate::assume::hit(if m == "startsession" { "Session.StartSession" } else { "Session.IsSessionActive" });
                Ok(Val::Bool(false))
            }
            ("session", "setdocumentserviceToken") | ("session", "enableverbosetelemetry") => Ok(Val::Void),
            // values in table 2000000107 "Isolated Storage" (crate::isolated)
            ("isolatedstorage", _) => self.isolated_storage(f, &m, args, discard),
            // the app (module) of the running / calling code: modules.rs
            ("navapp", "getcurrentmoduleinfo") | ("navapp", "getmoduleinfo") | ("navapp", "getcallermoduleinfo") => self.navapp_module_info(f, &m, args),
            ("navapp", "getcallercallstackmoduleinfos") => Ok(self.navapp_callstack_module_infos(f)),
            ("navapp", "isinstalling") | ("navapp", "isunlicensed") | ("navapp", "isentitled") => {
                if m != "isinstalling" {
                    crate::assume::hit("NavApp.IsEntitled");
                }
                Ok(Val::Bool(m == "isentitled" || (m == "isinstalling" && self.installing.is_some() && !crate::install::upgrading())))
            }
            ("navapp", "getresource") | ("navapp", "getresourceastext") | ("navapp", "getresourceasjson") | ("navapp", "listresources") => self.navapp_resource(f, &m, args),
            // TaskScheduler.CreateTask / TaskExists / CancelTask / SetTaskReady (crate::tasks)
            ("taskscheduler", t) => self.task_scheduler(f, &t.to_ascii_lowercase(), args),
            ("numbersequence", _) => self.number_sequence(f, &m, args),
            // System.X: the global function X
            ("system", _) => {
                self.discard = discard;
                self.builtin(f, name, args)
            }
            ("productname", _) => Ok(Val::Text("Business Central".into())),
            ("errorinfo", "create") => {
                let a = self.eval_all(f, args)?;
                Ok(self.errorinfo_create(&a))
            }
            ("currpage", _) => self.curr_page(f, &m, args),
            ("currreport", _) => self.curr_report(f, &m, args),
            // FILE.Exists / Erase / Copy / Rename / IsPathTemporary (crate::files)
            ("file", t) if crate::filexfer::is_transfer(t) => self.file_transfer(f, t, args),
            ("file", _) => match self.file_static(f, &m, args, discard)? {
                Some(v) => Ok(v),
                None => self.miss(format!("File.{name}")),
            },
            ("requestpage", _) => {
                for a in args {
                    let _ = self.eval(f, a);
                }
                crate::assume::hit("RequestPage.Static");
                Ok(Val::Bool(false))
            }
            _ => self.miss(format!("{class}.{name}")),
        }
    }

    fn number_sequence(&mut self, f: &mut Frame, m: &str, args: &[Expr]) -> R<Val> {
        use bc_store::SeqOp;
        let a = self.eval_all(f, args)?;
        let name = a.first().map(|v| v.to_text()).unwrap_or_default();
        // a missing sequence is an AL error raised before the database is asked: on PostgreSQL a
        // failed nextval/currval would abort the transaction that TryFunction callers recover in
        if matches!(m, "next" | "current" | "range" | "restart" | "delete") && self.db.seq(SeqOp::Exists(&name)).map_err(Self::store_err)? != Some(1) {
            // own wording [src:X07]
            return err(format!("There is no number sequence named '{name}'."));
        }
        let r = match m {
            "exists" => return Ok(Val::Bool(self.db.seq(SeqOp::Exists(&name)).map_err(Self::store_err)? == Some(1))),
            // Range(Name, Count): reserve Count numbers, return the first
            "range" => {
                let n = a.get(1).map(|v| v.as_i64()).unwrap_or(1).max(1);
                let first = self.db.seq(SeqOp::Next(&name));
                if let Ok(Some(_)) = first {
                    for _ in 1..n {
                        self.db.seq(SeqOp::Next(&name)).map_err(Self::store_err)?;
                    }
                }
                first
            }
            "insert" => {
                let start = a.get(1).map(|v| v.as_i64()).unwrap_or(0);
                let inc = a.get(2).map(|v| v.as_i64()).unwrap_or(1);
                self.db.seq(SeqOp::Create(&name, start, inc))
            }
            "delete" => self.db.seq(SeqOp::Delete(&name)),
            "restart" => {
                let start = a.get(1).map(|v| v.as_i64()).unwrap_or(0);
                self.db.seq(SeqOp::Restart(&name, start))
            }
            "next" => self.db.seq(SeqOp::Next(&name)),
            "current" => self.db.seq(SeqOp::Current(&name)),
            _ => Ok(None),
        };
        match r {
            Ok(Some(v)) => Ok(Val::BigInt(v)),
            Ok(None) => Ok(Val::Void),
            Err(e) => err(e.to_string()),
        }
    }

    pub fn value_method(&mut self, f: &mut Frame, v: Val, base: &Expr, name: &str, args: &[Expr]) -> R<Val> {
        let discard = std::mem::take(&mut self.discard);
        let m = name.to_ascii_lowercase();
        // AL File variables and DotNet File (System.IO.File) statics: the session's server files
        if let Val::Opaque(t) = v {
            if crate::files::is_file_val(&v) {
                return self.file_method(f, t, base, &m, args, discard);
            }
        }
        // Dialog variables (progress windows): crate::dialog
        if matches!(v, Val::Opaque("Dialog")) {
            return self.dialog_method(f, name, args);
        }
        // Notification / SessionSettings: crate::platobj
        if let Val::Plat(p) = &v {
            return self.plat_method(f, p, &m, args, discard);
        }
        if matches!(v, Val::Opaque("DotNet")) && self.is_dotnet_file(f, base) {
            if let Some(r) = self.dotnet_file_static(f, &m, args)? {
                return Ok(r);
            }
        }
        match &v {
            Val::Json(j) => return self.json_method(f, j, base, &m, args, discard),
            Val::FieldRef(h, fi) => return self.fieldref_method(f, h, *fi, base, &m, args),
            Val::Blob(b) => return self.blob_method(f, b.clone(), base, &m, args),
            Val::InStream(s) => return self.instream_method(f, s, &m, args),
            Val::OutStream(t) => return self.outstream_method(f, t, &m, args),
            Val::Net(n) => {
                let n = n.clone();
                return self.net_method(f, &n, &m, name, args, discard);
            }
            // Media / MediaSet fields (media.rs); other Guids fall through
            Val::Guid(_) => {
                if let Some(r) = self.media_method(f, base, &m, args)? {
                    return Ok(r);
                }
            }
            // streams that were never created: reading finds nothing, writing goes nowhere
            Val::Opaque("InStream") => {
                return Ok(match m.as_str() {
                    "eos" => Val::Bool(true),
                    "length" | "position" | "readtext" | "read" => Val::Int(0),
                    _ => Val::Void,
                })
            }
            _ => {}
        }
        // DotNet values passed to AL methods convert to AL values (DotNet String → Text, ...)
        let mut a: Vec<Val> = self.eval_all(f, args)?.into_iter().map(crate::dotnet::unboxed).collect();
        // a typed List / Dictionary variable: the arguments convert to its element (key, value)
        // types at the call, as the AL compiler converts them (List of [Guid].Add('<guid text>')
        // stores a Guid, List of [Code[20]].Add('x') stores 'X')
        if matches!(v, Val::List(_) | Val::Dict(_)) {
            let decl: Option<&'static TypeRef> = match base {
                Expr::Ident(n) => f.locals.iter().rev().chain(f.globals.vars.iter()).find(|x| x.name.eq_ignore_ascii_case(n)).and_then(|x| x.ty),
                _ => None,
            };
            match decl {
                Some(TypeRef::List(e)) if matches!(**e, TypeRef::Simple { .. }) => {
                    let e: &'static TypeRef = e;
                    let at: Vec<usize> = match m.as_str() {
                        "add" | "contains" | "indexof" | "lastindexof" | "remove" => vec![0],
                        "insert" | "set" => vec![1],
                        "addrange" => (0..a.len()).collect(),
                        _ => vec![],
                    };
                    for i in at {
                        if i < a.len() && !matches!(a[i], Val::List(_)) {
                            a[i] = self.coerce(a[i].clone(), Some(e))?;
                        }
                    }
                }
                Some(TypeRef::Dictionary(k, vt)) => {
                    let (k, vt): (&'static TypeRef, &'static TypeRef) = (k, vt);
                    if matches!(k, TypeRef::Simple { .. }) && !a.is_empty() && matches!(m.as_str(), "add" | "set" | "get" | "containskey" | "remove") {
                        a[0] = self.coerce(a[0].clone(), Some(k))?;
                    }
                    if matches!(vt, TypeRef::Simple { .. }) && a.len() > 1 && matches!(m.as_str(), "add" | "set") {
                        a[1] = self.coerce(a[1].clone(), Some(vt))?;
                    }
                }
                _ => {}
            }
        }
        match v {
            Val::Text(s) => Ok(match m.as_str() {
                "contains" => Val::Bool(s.contains(&a[0].to_text())),
                "startswith" => Val::Bool(s.starts_with(&a[0].to_text())),
                "endswith" => Val::Bool(s.ends_with(&a[0].to_text())),
                "indexof" => Val::Int(s.find(&a[0].to_text()).map(|i| s[..i].chars().count() as i32 + 1).unwrap_or(0)),
                "lastindexof" => Val::Int(s.rfind(&a[0].to_text()).map(|i| s[..i].chars().count() as i32 + 1).unwrap_or(0)),
                "substring" => {
                    let st = (a[0].as_i64().max(1) - 1) as usize;
                    let len = a.get(1).map(|x| x.as_i64() as usize).unwrap_or(usize::MAX);
                    Val::Text(s.chars().skip(st).take(len).collect())
                }
                "replace" => Val::Text(s.replace(&a[0].to_text(), &a[1].to_text())),
                "toupper" => Val::Text(s.to_uppercase()),
                "tolower" => Val::Text(s.to_lowercase()),
                "trim" => Val::Text(s.trim().to_string()),
                "trimstart" => Val::Text(match a.first() {
                    Some(c) => s.trim_start_matches(|ch| c.to_text().contains(ch)).to_string(),
                    None => s.trim_start().to_string(),
                }),
                "trimend" => Val::Text(match a.first() {
                    Some(c) => s.trim_end_matches(|ch| c.to_text().contains(ch)).to_string(),
                    None => s.trim_end().to_string(),
                }),
                "padleft" | "padright" => {
                    let n = a[0].as_i64() as usize;
                    let c = a.get(1).map(|x| x.to_text().chars().next().unwrap_or(' ')).unwrap_or(' ');
                    let k = s.chars().count();
                    let pad: String = std::iter::repeat(c).take(n.saturating_sub(k)).collect();
                    Val::Text(if m == "padleft" { pad + &s } else { s + &pad })
                }
                "split" => {
                    let seps: Vec<String> = texts(&a);
                    let parts: Vec<Val> = if seps.is_empty() {
                        vec![Val::Text(s)]
                    } else {
                        let mut out = vec![s.clone()];
                        for sep in &seps {
                            out = out.iter().flat_map(|p| p.split(sep.as_str()).map(|x| x.to_string()).collect::<Vec<_>>()).collect();
                        }
                        out.into_iter().map(Val::Text).collect()
                    };
                    Val::List(Rc::new(RefCell::new(parts)))
                }
                "remove" => {
                    let st = (a[0].as_i64().max(1) - 1) as usize;
                    let len = a.get(1).map(|x| x.as_i64() as usize).unwrap_or(usize::MAX);
                    Val::Text(s.chars().enumerate().filter(|(i, _)| *i < st || *i >= st.saturating_add(len)).map(|(_, c)| c).collect())
                }
                "length" => Val::Int(s.chars().count() as i32),
                "append" | "appendline" => {
                    // TextBuilder semantics on a Text variable
                    let lv = self.lvalue(f, base)?;
                    let mut ns = s;
                    ns.push_str(&a.first().map(|x| x.to_text()).unwrap_or_default());
                    if m == "appendline" {
                        ns.push_str("\r\n");
                    }
                    self.write_lval(&lv, Val::Text(ns))?;
                    Val::Bool(true)
                }
                "totext" => Val::Text(s),
                "addtext" => {
                    // BigText.AddText(Text [, Position])
                    let lv = self.lvalue(f, base)?;
                    let add = a.first().map(|x| x.to_text()).unwrap_or_default();
                    let mut chars: Vec<char> = s.chars().collect();
                    let pos = a.get(1).map(|p| (p.as_i64().max(1) as usize - 1).min(chars.len())).unwrap_or(chars.len());
                    chars.splice(pos..pos, add.chars());
                    self.write_lval(&lv, Val::Text(chars.into_iter().collect()))?;
                    Val::Void
                }
                "getsubtext" => {
                    // BigText.GetSubText(var Text, Position [, Length])
                    let pos = a.get(1).map(|p| p.as_i64().max(1) as usize - 1).unwrap_or(0);
                    let len = a.get(2).map(|x| x.as_i64().max(0) as usize).unwrap_or(usize::MAX);
                    let sub: String = s.chars().skip(pos).take(len).collect();
                    let lv = self.lvalue(f, &args[0])?;
                    let n = sub.chars().count() as i32;
                    self.write_lval(&lv, Val::Text(sub))?;
                    Val::Int(n)
                }
                "textpos" => Val::Int(s.find(&a[0].to_text()).map(|i| s[..i].chars().count() as i32 + 1).unwrap_or(0)),
                "write" => {
                    // BigText.Write(OutStream)
                    if let Some(Val::OutStream(t)) = a.first() {
                        let t = t.clone();
                        self.outstream_write_text(&t, &s);
                    }
                    Val::Bool(true)
                }
                "read" => {
                    // BigText.Read(InStream): the rest of the stream
                    if let Some(Val::InStream(st)) = a.first() {
                        let text = {
                            let mut st = st.borrow_mut();
                            let rest = String::from_utf8_lossy(&st.data[st.pos.min(st.data.len())..]).into_owned();
                            st.pos = st.data.len();
                            rest
                        };
                        let lv = self.lvalue(f, base)?;
                        self.write_lval(&lv, Val::Text(text))?;
                    }
                    Val::Bool(true)
                }
                _ => match crate::dotnet::text_method(&s, &m, &a) {
                    Some(v) => v,
                    None => return self.miss(format!("Text.{name}")),
                },
            }),
            Val::List(l) => {
                const BAD_LIST: &str = "An invalid argument was passed to a 'List' data type method.";
                // an invalid index: an error unless the call's Boolean is used (then false)
                let bad = |discard: bool| -> R<Val> { if discard { err(BAD_LIST) } else { Ok(Val::Bool(false)) } };
                let len = l.borrow().len() as i64;
                let range_ok = |st: i64, n: i64| st >= 1 && n >= 0 && st - 1 + n <= len;
                Ok(match m.as_str() {
                    "add" => {
                        l.borrow_mut().push(a[0].clone());
                        Val::Void
                    }
                    "addrange" => {
                        for x in a {
                            match x {
                                Val::List(o) => {
                                    let o = o.borrow().clone();
                                    l.borrow_mut().extend(o)
                                }
                                o => l.borrow_mut().push(o),
                            }
                        }
                        Val::Void
                    }
                    "count" => Val::Int(len as i32),
                    "get" => {
                        let i = a[0].as_i64();
                        let found = if (1..=len).contains(&i) { Some(l.borrow()[(i - 1) as usize].clone()) } else { None };
                        if args.len() > 1 {
                            // Get(Index, var Value): Boolean
                            match found {
                                Some(x) => {
                                    let lv = self.lvalue(f, &args[1])?;
                                    self.write_lval(&lv, x)?;
                                    Val::Bool(true)
                                }
                                None => return bad(discard),
                            }
                        } else {
                            match found {
                                Some(x) => x,
                                None => return err(BAD_LIST),
                            }
                        }
                    }
                    "set" => {
                        let i = a[0].as_i64();
                        if !(1..=len).contains(&i) {
                            return bad(discard);
                        }
                        let old = std::mem::replace(&mut l.borrow_mut()[(i - 1) as usize], a[1].clone());
                        if args.len() > 2 {
                            // Set(Index, Value, var OldValue): Boolean
                            let lv = self.lvalue(f, &args[2])?;
                            self.write_lval(&lv, old)?;
                        }
                        Val::Bool(true)
                    }
                    "contains" => Val::Bool(l.borrow().iter().any(|x| equals(x, &a[0]))),
                    "indexof" => Val::Int(l.borrow().iter().position(|x| equals(x, &a[0])).map(|i| i as i32 + 1).unwrap_or(0)),
                    "lastindexof" => Val::Int(l.borrow().iter().rposition(|x| equals(x, &a[0])).map(|i| i as i32 + 1).unwrap_or(0)),
                    "remove" => {
                        let pos = l.borrow().iter().position(|x| equals(x, &a[0]));
                        if let Some(p) = pos {
                            l.borrow_mut().remove(p);
                        }
                        Val::Bool(pos.is_some())
                    }
                    "removeat" => {
                        let i = a[0].as_i64();
                        if !(1..=len).contains(&i) {
                            return bad(discard);
                        }
                        l.borrow_mut().remove((i - 1) as usize);
                        Val::Bool(true)
                    }
                    "insert" => {
                        let i = a[0].as_i64();
                        if !(1..=len + 1).contains(&i) {
                            return bad(discard);
                        }
                        l.borrow_mut().insert((i - 1) as usize, a[1].clone());
                        Val::Bool(true)
                    }
                    "reverse" => {
                        l.borrow_mut().reverse();
                        Val::Void
                    }
                    "removerange" => {
                        let (st, n) = (a[0].as_i64(), a[1].as_i64());
                        if !range_ok(st, n) {
                            return bad(discard);
                        }
                        l.borrow_mut().drain((st - 1) as usize..(st - 1 + n) as usize);
                        Val::Bool(true)
                    }
                    "getrange" => {
                        let (st, n) = (a[0].as_i64(), a[1].as_i64());
                        let ok = range_ok(st, n);
                        let part = || Val::List(Rc::new(RefCell::new(l.borrow()[(st - 1) as usize..(st - 1 + n) as usize].to_vec())));
                        if args.len() > 2 {
                            // GetRange(Index, Count, var Result): Boolean
                            if !ok {
                                return bad(discard);
                            }
                            let r = part();
                            let lv = self.lvalue(f, &args[2])?;
                            self.write_lval(&lv, r)?;
                            Val::Bool(true)
                        } else {
                            if !ok {
                                return err(BAD_LIST);
                            }
                            part()
                        }
                    }
                    _ => return self.miss(format!("List.{name}")),
                })
            }
            Val::Dict(d) => Ok(match m.as_str() {
                "add" => {
                    if d.borrow().iter().any(|(k, _)| equals(k, &a[0])) {
                        // Add(Key, Value): Boolean is false when its result is used
                        if !discard {
                            return Ok(Val::Bool(false));
                        }
                        return err("An item with the same key has already been added.");
                    }
                    d.borrow_mut().push((a[0].clone(), a[1].clone()));
                    Val::Bool(true)
                }
                "set" => {
                    let old = {
                        let mut db = d.borrow_mut();
                        match db.iter_mut().find(|(k, _)| equals(k, &a[0])) {
                            Some(e) => Some(std::mem::replace(&mut e.1, a[1].clone())),
                            None => {
                                db.push((a[0].clone(), a[1].clone()));
                                None
                            }
                        }
                    };
                    if args.len() > 2 {
                        // Set(Key, Value, var OldValue): Boolean, true when the key existed
                        if let Some(o) = &old {
                            let lv = self.lvalue(f, &args[2])?;
                            self.write_lval(&lv, o.clone())?;
                        }
                        Val::Bool(old.is_some())
                    } else {
                        Val::Void
                    }
                }
                "get" => {
                    let found = d.borrow().iter().find(|(k, _)| equals(k, &a[0])).map(|(_, v)| v.clone());
                    if args.len() > 1 {
                        // Get(Key, var Value): Boolean raises when its result is not used
                        match found {
                            Some(v) => {
                                let lv = self.lvalue(f, &args[1])?;
                                self.write_lval(&lv, v)?;
                                Val::Bool(true)
                            }
                            None if discard => return err("The given key was not present in the dictionary."),
                            None => Val::Bool(false),
                        }
                    } else {
                        match found {
                            Some(v) => v,
                            None => return err("The given key was not present in the dictionary."),
                        }
                    }
                }
                "containskey" => Val::Bool(d.borrow().iter().any(|(k, _)| equals(k, &a[0]))),
                "remove" => {
                    let pos = d.borrow().iter().position(|(k, _)| equals(k, &a[0]));
                    if let Some(p) = pos {
                        d.borrow_mut().remove(p);
                    }
                    Val::Bool(pos.is_some())
                }
                "count" => Val::Int(d.borrow().len() as i32),
                "keys" => Val::List(Rc::new(RefCell::new(d.borrow().iter().map(|(k, _)| k.clone()).collect()))),
                "values" => Val::List(Rc::new(RefCell::new(d.borrow().iter().map(|(_, v)| v.clone()).collect()))),
                _ => return self.miss(format!("Dictionary.{name}")),
            }),
            Val::Opt { ord, enum_id } => Ok(match m.as_str() {
                "asinteger" => Val::Int(ord),
                "names" | "ordinals" => {
                    let e = enum_id.and_then(|e| self.cat.enums.get(&e));
                    let items = e
                        .map(|e| e.values.iter().map(|v| if m == "names" { Val::Text(v.name.clone()) } else { Val::Int(v.ordinal as i32) }).collect())
                        .unwrap_or_default();
                    Val::List(Rc::new(RefCell::new(items)))
                }
                "fromInteger" | "frominteger" => Val::Opt { ord: a[0].as_i64() as i32, enum_id },
                _ => return self.miss(format!("Enum.{name}")),
            }),
            // ErrorInfo methods and properties (crate::errinfo)
            Val::ErrorInfo(e) => self.errorinfo_method(&e, &m, &a),
            Val::Date(d) => Ok(match m.as_str() {
                "year" => Val::Int(days(d).map(|n| n.year()).unwrap_or(0)),
                "month" => Val::Int(days(d).map(|n| n.month() as i32).unwrap_or(0)),
                "day" => Val::Int(days(d).map(|n| n.day() as i32).unwrap_or(0)),
                "dayofweek" => Val::Int(days(d).map(|n| n.weekday().number_from_monday() as i32).unwrap_or(0)),
                "weekno" => Val::Int(days(d).map(|n| n.iso_week().week() as i32).unwrap_or(0)),
                "totext" => Val::Text(format_date(d)),
                _ => return self.miss(format!("Date.{name}")),
            }),
            // DateTime.Date() / .Time(): same results as DT2Date / DT2Time (0DT gives 0D / 0T)
            Val::DateTime(t) => Ok(match m.as_str() {
                "date" => Val::Date(if t == bc_store::DATETIME_UNDEFINED { 0 } else { chrono::DateTime::from_timestamp_millis(t).map(|x| from_naive(x.date_naive())).unwrap_or(0) }),
                "time" => Val::Time(if t == bc_store::DATETIME_UNDEFINED { 0 } else { t.rem_euclid(86_400_000) as i32 + 1 }),
                "totext" => Val::Text(format_val(&Val::DateTime(t), a.get(1).map(|v| v.as_i64()).unwrap_or(0))),
                _ => return self.miss(format!("DateTime.{name}")),
            }),
            // Time values are milliseconds after midnight + 1 (0 is 0T)
            Val::Time(t) => {
                let ms = (t - 1).max(0);
                Ok(match m.as_str() {
                    "hour" => Val::Int(if t == 0 { 0 } else { ms / 3_600_000 }),
                    "minute" => Val::Int(if t == 0 { 0 } else { ms / 60_000 % 60 }),
                    "second" => Val::Int(if t == 0 { 0 } else { ms / 1000 % 60 }),
                    "millisecond" => Val::Int(if t == 0 { 0 } else { ms % 1000 }),
                    "totext" => Val::Text(format_val(&Val::Time(t), 0)),
                    _ => return self.miss(format!("Time.{name}")),
                })
            }
            Val::Opaque("RecordRef") => match m.as_str() {
                "open" => {
                    let tid = a.first().map(|v| v.as_i64()).unwrap_or(0);
                    let temp = a.get(1).is_some_and(|v| v.as_bool());
                    if !self.cat.tables.contains_key(&tid) {
                        return err(format!("Table {tid} does not exist"));
                    }
                    let r = self.new_record(tid, temp);
                    r.borrow_mut().is_ref = true;
                    if let Some(c) = a.get(2) {
                        r.borrow_mut().company = Some(c.to_text()).filter(|s| !s.is_empty());
                    }
                    let lv = self.lvalue(f, base)?;
                    if let LVal::Slot(sl, _) = &lv {
                        *sl.borrow_mut() = Val::Record(r);
                    }
                    Ok(Val::Void)
                }
                "gettable" => {
                    if let Some(Val::Record(src)) = a.first() {
                        let copy = crate::record::copy_record(src);
                        copy.borrow_mut().is_ref = true;
                        copy.borrow_mut().unopened = false;
                        let lv = self.lvalue(f, base)?;
                        if let LVal::Slot(sl, _) = &lv {
                            *sl.borrow_mut() = Val::Record(copy);
                        }
                    }
                    Ok(Val::Void)
                }
                "close" | "isdirty" => {
                    crate::assume::hit("RecordRef.Unopened");
                    Ok(Val::Bool(false))
                }
                "number" => {
                    crate::assume::hit("RecordRef.Unopened");
                    Ok(Val::Int(0))
                }
                _ => err(format!("The RecordRef is not open (calling {name})")),
            },
            Val::KeyRef(h, k) => {
                let tdef = h.borrow().tdef;
                let Some(kd) = tdef.keys.get(k) else { return err("Invalid KeyRef") };
                Ok(match m.as_str() {
                    "fieldcount" => Val::Int(kd.fields.len() as i32),
                    "fieldindex" => {
                        let i = a.first().map(|v| v.as_i64()).unwrap_or(1);
                        match kd.fields.get((i - 1).max(0) as usize) {
                            Some(&fi) if i >= 1 => Val::FieldRef(h.clone(), fi),
                            _ => return err(format!("The field index {i} is out of range for key {}", kd.name)),
                        }
                    }
                    "active" => Val::Bool(kd.enabled),
                    "record" => Val::Record(h.clone()),
                    _ => return self.miss(format!("KeyRef.{name}")),
                })
            }
            Val::RecordId(tid, key) => Ok(match m.as_str() {
                "tableno" => Val::Int(tid as i32),
                "getrecord" => {
                    if !self.cat.tables.contains_key(&tid) {
                        return Ok(Val::Opaque("RecordRef"));
                    }
                    let r = self.new_record(tid, false);
                    let pkf = r.borrow().tdef.pk().fields.clone();
                    for (j, &fi) in pkf.iter().enumerate() {
                        if let Some(v) = key.get(j) {
                            r.borrow_mut().row[fi] = v.clone();
                        }
                    }
                    if let Some(row) = self.rec_get_pk_r(&r)? {
                        r.borrow_mut().row = (row).into();
                    }
                    r.borrow_mut().is_ref = true;
                    Val::Record(r)
                }
                _ => return self.miss(format!("RecordId.{name}")),
            }),
            Val::Opaque("DotNet") => {
                // the variable's declared .NET type and alias (an expression has none: '')
                let (ty, alias) = match base {
                    Expr::Ident(n) => f.locals.iter().rev().find(|v| v.name.eq_ignore_ascii_case(n)).or_else(|| f.globals.vars.iter().find(|v| v.name.eq_ignore_ascii_case(n))).and_then(|v| v.ty).and_then(|t| match t {
                        TypeRef::Object { kind, target, .. } if kind == "dotnet" => Some((crate::dotnet::resolve_type(self.cat, target), target.as_str())),
                        _ => None,
                    }),
                    _ => None,
                }
                .unwrap_or(("", ""));
                // NavTestExecution.IsInTestMode (Ncl): a test codeunit is executing in this
                // session (a background session inside a test is not one). An unquoted type name
                // in a dotnet declaration reaches the catalog without its dots.
                if ty.replace('.', "").eq_ignore_ascii_case("MicrosoftDynamicsNavRuntimeNavTestExecution") && m == "isintestmode" {
                    return Ok(Val::Bool(self.test.is_some() && self.bg_depth == 0));
                }
                // .NET interop is a platform boundary: shim with neutral results
                let short = if ty.contains('.') || alias.is_empty() { crate::dotnet::short_name(ty) } else { alias };
                *self.missing.entry(format!("dotnet {short}.{name}")).or_default() += 1;
                crate::assume::hit_member("DotNet.Neutral", &format!("{short}.{name}"));
                Ok(if m == "instance" || m == "create" || m.starts_with("get") && m.ends_with("instance") { Val::Opaque("DotNet") } else { Val::Void })
            }
            // ModuleInfo / Version values of an app (modules.rs)
            Val::Opaque(t) if t.starts_with("ModuleInfo") || t.starts_with("Version") => Ok(self.module_value_method(t, &m).unwrap_or(Val::Void)),
            Val::Opaque(t) if t != "JsonObject" && t != "JsonArray" && t != "JsonToken" && t != "JsonValue" && t != "HttpClient" => {
                // a native platform codeunit (no AL source): the variable's declared object
                let target = match base {
                    Expr::Ident(n) => f.locals.iter().rev().find(|v| v.name.eq_ignore_ascii_case(n)).or_else(|| f.globals.vars.iter().find(|v| v.name.eq_ignore_ascii_case(n))).and_then(|v| v.ty).and_then(|t| match t {
                        TypeRef::Object { target, .. } => Some(target.trim_matches('"').to_ascii_lowercase()),
                        _ => None,
                    }),
                    _ => None,
                };
                // "Agent Utilities" (codeunit 2000000022, Ncl Codeunit2000000022):
                // BlockPageFromBeingOpenedByAgent raises an error only in an agent session, and
                // bc-rust runs none
                if t == "Object" && target.as_deref() == Some("agent utilities") && m == "blockpagefrombeingopenedbyagent" {
                    for a in args {
                        let _ = self.eval(f, a)?;
                    }
                    return Ok(Val::Void);
                }
                // platform object without an implementation yet: log and continue with a neutral result
                *self.missing.entry(format!("{t}.{name}")).or_default() += 1;
                let owner = target.as_deref().unwrap_or(t);
                crate::assume::hit_member("PlatformObject.Neutral", &format!("{owner}.{name}"));
                Ok(Val::Void)
            }
            Val::Opaque(t) => {
                if m == "page" || m == "update" || m == "setselectionfilter" || m == "close" || m == "editable" || m == "activate" || m == "setrecord" {
                    return Ok(Val::Opaque(t));
                }
                self.miss(format!("{t}.{name}"))
            }
            // Media / MediaSet fields hold their media id (a Guid): the read-only methods
            // (bc-nst: Customer Picture factbox, Rec.Image.HasValue)
            Val::Guid(g) if matches!(m.as_str(), "hasvalue" | "mediaid" | "count") => Ok(match m.as_str() {
                "hasvalue" => Val::Bool(g != [0; 16]),
                "mediaid" => Val::Guid(g),
                _ => Val::Int((g != [0; 16]) as i32),
            }),
            other => {
                if m == "totext" {
                    return Ok(Val::Text(other.to_text()));
                }
                // a DotNet variable that is still Void: its type (or the constructor that should have
                // created it) is not emulated; name the declared type instead of "Void"
                if matches!(other, Val::Void) {
                    if let Expr::Ident(n) = base {
                        let declared = f.locals.iter().rev().find(|v| v.name.eq_ignore_ascii_case(n)).or_else(|| f.globals.vars.iter().find(|v| v.name.eq_ignore_ascii_case(n))).and_then(|v| v.ty);
                        if let Some(TypeRef::Object { kind, target, .. }) = declared {
                            if kind == "dotnet" {
                                let t = target.rsplit('.').next().unwrap_or(target).trim_matches('"').to_string();
                                return self.miss(format!("DotNet {t}.{name}"));
                            }
                        }
                    }
                }
                self.miss(format!("{}.{name}", other.type_name()))
            }
        }
    }

    /// `"Enum Name".FromInteger(n)` style calls.
    pub fn enum_static(&mut self, f: &mut Frame, enum_name: &str, name: &str, args: &[Expr]) -> R<Val> {
        let e = self.cat.enum_by_name(enum_name).unwrap();
        let a = self.eval_all(f, args)?;
        Ok(match name.to_ascii_lowercase().as_str() {
            "frominteger" => Val::Opt { ord: a[0].as_i64() as i32, enum_id: Some(e.id) },
            "names" => Val::List(Rc::new(RefCell::new(e.values.iter().map(|v| Val::Text(v.name.clone())).collect()))),
            "ordinals" => Val::List(Rc::new(RefCell::new(e.values.iter().map(|v| Val::Int(v.ordinal as i32)).collect()))),
            _ => return self.miss(format!("Enum.{name}")),
        })
    }
}

fn clear_like(v: &Val) -> Val {
    match v {
        Val::Int(_) => Val::Int(0),
        Val::BigInt(_) => Val::BigInt(0),
        Val::Dec(_) => Val::Dec(Decimal::ZERO),
        Val::Bool(_) => Val::Bool(false),
        Val::Text(_) => Val::Text(String::new()),
        Val::Date(_) => Val::Date(0),
        Val::Time(_) => Val::Time(0),
        Val::Guid(_) => Val::Guid([0; 16]),
        Val::DateFormula(_) => Val::DateFormula(String::new()),
        Val::DateTime(_) => Val::DateTime(bc_store::DATETIME_UNDEFINED),
        Val::Duration(_) => Val::Duration(0),
        Val::Char(_) => Val::Char('\0'),
        Val::RecordId(..) => Val::RecordId(0, vec![]),
        Val::Opt { enum_id, .. } => Val::Opt { ord: 0, enum_id: *enum_id },
        Val::List(_) => Val::List(Rc::new(RefCell::new(vec![]))),
        Val::Dict(_) => Val::Dict(Rc::new(RefCell::new(vec![]))),
        Val::Iface(_) => Val::Iface(None),
        Val::Object(i) => Val::Object(Instance::new(i.code)),
        o => o.clone(),
    }
}

pub fn inc_str(s: &str) -> String {
    inc_str_by(s, 1)
}

pub fn inc_str_by(s: &str, by: i64) -> String {
    // increment the last number in the string, preserving width
    let chars: Vec<char> = s.chars().collect();
    let end = match chars.iter().rposition(|c| c.is_ascii_digit()) {
        Some(e) => e + 1,
        None => return String::new(),
    };
    let mut start = end;
    while start > 0 && chars[start - 1].is_ascii_digit() {
        start -= 1;
    }
    let num: String = chars[start..end].iter().collect();
    let width = num.len();
    let n: i128 = num.parse::<i128>().unwrap_or(0) + by as i128;
    if n < 0 {
        return String::new();
    }
    let mut ns = n.to_string();
    if ns.len() < width {
        ns = format!("{:0>width$}", ns, width = width);
    }
    let mut out: String = chars[..start].iter().collect();
    out.push_str(&ns);
    out.extend(chars[end..].iter());
    out
}

/// Round(x, prec, dir); None when x / prec or the result leaves the decimal range (BC's
/// OverflowException, crate::interp::DEC_OVERFLOW).
pub fn round_dec(x: Decimal, prec: Decimal, dir: &str) -> Option<Decimal> {
    if prec.is_zero() {
        return Some(x);
    }
    let q = x.checked_div(prec)?;
    let r = match dir {
        ">" => q.ceil(),
        "<" => q.floor(),
        _ => q.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero),
    };
    Some(r.checked_mul(prec)?.normalize())
}

fn format_picture(v: &Val, pic: &str) -> String {
    if let Some(s) = format_number_picture(v, pic) {
        return s;
    }
    if let Some(s) = format_datetime_picture(v, pic) {
        return s;
    }
    // minimal support for '<Year4>-<Month,2>-<Day,2>' style pictures
    let mut out = String::new();
    let mut rest = pic;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('>') else { break };
        let tag = &rest[i + 1..i + j];
        let (name, width) = match tag.split_once(',') {
            Some((n, w)) => (n.trim(), w.trim().parse::<usize>().unwrap_or(0)),
            None => (tag.trim(), 0),
        };
        let n = match v {
            Val::Date(d) => date_to_naive(*d),
            Val::DateTime(t) => chrono::DateTime::from_timestamp_millis(*t).map(|x| x.date_naive()),
            _ => None,
        };
        let piece = match (name.to_ascii_lowercase().as_str(), n) {
            ("year4", Some(n)) => n.year().to_string(),
            ("year", Some(n)) => format!("{:02}", n.year() % 100),
            ("month", Some(n)) => n.month().to_string(),
            ("day", Some(n)) => n.day().to_string(),
            ("standard format", _) | ("standard", _) => format_val(v, 0),
            ("integer", _) => v.as_i64().to_string(),
            ("decimals", _) => {
                let s = v.as_dec().normalize().to_string();
                s.find('.').map(|p| s[p..].to_string()).unwrap_or_default()
            }
            ("sign", _) => if v.as_dec().is_sign_negative() { "-".into() } else { String::new() },
            _ => format_val(v, 0),
        };
        if width > 0 && piece.len() < width {
            out.push_str(&"0".repeat(width - piece.len()));
        }
        out.push_str(&piece);
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    if out.is_empty() {
        format_val(v, 0)
    } else {
        out
    }
}

/// Format(Date/Time/DateTime, 0, FormatString): fields `<Day>`, `<Month>`, `<Month Text>`, `<Quarter>`,
/// `<Year>`, `<Year4>`, `<Week>`, `<Week Year>`, `<Week Year4>`, `<WeekDay>`, `<Weekday Text>`, `<Closing>`,
/// `<Hours24>`, `<Hours12>`, `<Minutes>`, `<Seconds>`, `<Thousands>`, `<Second dec>`, `<AM/PM>`,
/// `<Standard Format,N>`; names may be abbreviated (`<H>:<M>:<S>`, first field with that prefix).
/// `,N` pads numbers with zeros and cuts or pads texts to N characters; `<Filler Character,c>`
/// after a field sets its filler [src:A08.2]. Abbreviated names are an open question [src:A08.4].
/// An undefined value formats as ''. None: not a date/time value.
fn format_datetime_picture(v: &Val, pic: &str) -> Option<String> {
    use chrono::Timelike;
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
    const DATE_FIELDS: &[&str] = &["day", "month", "month text", "quarter", "year", "year4", "week", "week year", "week year4", "weekday", "weekday text", "closing"];
    const TIME_FIELDS: &[&str] = &["hours24", "hours12", "minutes", "seconds", "thousands", "second dec", "am/pm"];
    let (date, time, closing): (Option<chrono::NaiveDate>, Option<chrono::NaiveTime>, bool) = match v {
        Val::Date(d) => (date_to_naive(*d), None, date_is_closing(*d)),
        Val::Time(t) if *t > 0 => (None, chrono::NaiveTime::from_num_seconds_from_midnight_opt(((*t - 1) / 1000) as u32, (((*t - 1) % 1000) * 1_000_000) as u32), false),
        Val::DateTime(t) if *t != bc_store::DATETIME_UNDEFINED => {
            let dt = chrono::DateTime::from_timestamp_millis(*t)?.naive_utc();
            (Some(dt.date()), Some(dt.time()), false)
        }
        Val::Time(_) | Val::DateTime(_) => return Some(String::new()),
        _ => return None,
    };
    if date.is_none() && time.is_none() {
        return Some(String::new());
    }
    let mut fields: Vec<&str> = vec![];
    if date.is_some() {
        fields.extend_from_slice(DATE_FIELDS);
    }
    if time.is_some() {
        fields.extend_from_slice(TIME_FIELDS);
    }
    // literal text and fields: (text, is_number, width, filler)
    let mut parts: Vec<(String, bool, usize, Option<char>)> = vec![];
    let mut rest = pic;
    while let Some(i) = rest.find('<') {
        if i > 0 {
            parts.push((rest[..i].to_string(), false, 0, None));
        }
        let Some(j) = rest[i..].find('>') else {
            rest = &rest[i..];
            break;
        };
        let tag = &rest[i + 1..i + j];
        rest = &rest[i + j + 1..];
        let (name, arg) = match tag.split_once(',') {
            Some((n, a)) => (n.trim().to_ascii_lowercase(), a),
            None => (tag.trim().to_ascii_lowercase(), ""),
        };
        // BC's field name is "SECOND DEC." (with the period); attributes as in number pictures
        let name = name.trim_end_matches('.').to_string();
        let attr = if fields.iter().any(|f| *f == name || f.starts_with(name.as_str())) { None } else { bc_format_name(FORMAT_ATTRIBUTES, &name) };
        if attr == Some("filler character") || name == "filler character" || name == "filler char" || name == "filler" {
            if let (Some(c), Some(last)) = (arg.chars().next(), parts.iter_mut().rev().find(|p| p.2 > 0)) {
                last.3 = Some(c);
            }
            continue;
        }
        let width = arg.trim().parse::<usize>().unwrap_or(0);
        if name.starts_with("standard") {
            parts.push((format_val(v, width as i64), false, 0, None));
            continue;
        }
        let field = fields.iter().find(|f| **f == name).or_else(|| fields.iter().find(|f| !name.is_empty() && f.starts_with(name.as_str())));
        // attributes (Comma, 1000Character, Overflow, Precision) change nothing here; an unknown
        // name is an invalid format string in BC, not the value again
        let Some(field) = field else { continue };
        let (d, t) = (date.unwrap_or_default(), time.unwrap_or_default());
        let h12 = match t.hour() % 12 {
            0 => 12,
            h => h,
        };
        let (text, num) = match *field {
            "day" => (d.day().to_string(), true),
            "month" => (d.month().to_string(), true),
            "month text" => (MONTHS[d.month0() as usize].to_string(), false),
            "quarter" => ((d.month0() / 3 + 1).to_string(), true),
            "year" => (format!("{:02}", d.year() % 100), true),
            "year4" => (d.year().to_string(), true),
            "week" => (d.iso_week().week().to_string(), true),
            "week year" => (format!("{:02}", d.iso_week().year() % 100), true),
            "week year4" => (d.iso_week().year().to_string(), true),
            "weekday" => (d.weekday().number_from_monday().to_string(), true),
            "weekday text" => (DAYS[d.weekday().num_days_from_monday() as usize].to_string(), false),
            "closing" => (if closing { "C".into() } else { String::new() }, false),
            "hours24" => (t.hour().to_string(), true),
            "hours12" => (h12.to_string(), true),
            "minutes" => (t.minute().to_string(), true),
            "seconds" => (t.second().to_string(), true),
            "thousands" => ((t.nanosecond() / 1_000_000).to_string(), true),
            "second dec" => {
                let ms = t.nanosecond() / 1_000_000;
                (if ms == 0 { String::new() } else { format!(".{ms:03}").trim_end_matches('0').to_string() }, false)
            }
            _ => ((if t.hour() < 12 { "AM" } else { "PM" }).to_string(), false),
        };
        parts.push((text, num, width, None));
    }
    let mut out = String::new();
    for (text, num, width, filler) in parts {
        let n = text.chars().count();
        if width == 0 || n == width {
            out.push_str(&text);
        } else if n > width {
            out.extend(text.chars().take(width));
        } else if num {
            out.push_str(&filler.unwrap_or('0').to_string().repeat(width - n));
            out.push_str(&text);
        } else {
            out.push_str(&text);
            out.push_str(&filler.unwrap_or(' ').to_string().repeat(width - n));
        }
    }
    out.push_str(rest);
    Some(out)
}

/// Format(Number, 0, FormatString) for Decimal / Integer / BigInteger values [src:A08.1]:
/// `<Standard Format,N>` expands to the type's standard format [src:A08.3],
/// `<Precision,Min:Max>`, `<1000Character,c>` and `<Comma,c>` are attributes, `<C,Code>` (currency)
/// and `<S,Symbol>` produce nothing, fields are `<Sign>`, `<Integer>`, `<Integer Thousand>`,
/// `<Decimals>`; other text is literal. None: not a number.
/// Attribute names of Format pictures, lower-case [src:A08.1].
const FORMAT_ATTRIBUTES: &[&str] = &["standard format", "1000character", "comma", "overflow", "filler character", "precision"];

/// The name equal to `token` (case-insensitive), else the first that starts with it (open
/// question [src:A08.4]).
fn bc_format_name(names: &[&'static str], token: &str) -> Option<&'static str> {
    if token.is_empty() {
        return None;
    }
    names.iter().find(|n| n.eq_ignore_ascii_case(token)).or_else(|| names.iter().find(|n| n.starts_with(token))).copied()
}

fn format_number_picture(v: &Val, pic: &str) -> Option<String> {
    let (d, is_int) = match v {
        Val::Dec(d) => (*d, false),
        Val::Int(i) => (Decimal::from(*i), true),
        Val::BigInt(i) => (Decimal::from(*i), true),
        _ => return None,
    };
    let std_format = |n: i64| -> &'static str {
        match (is_int, n) {
            (true, 0) => "<Sign><Integer Thousand>",
            (true, _) => "<Sign><Integer>",
            (false, 0) => "<Sign><Integer Thousand><Decimals>",
            (false, 3) => "<Integer Thousand><Decimals><Sign>",
            (false, 4) => "<Integer><Decimals><Sign>",
            (false, 9) => "<Sign><Integer><Decimals><Comma,.>",
            (false, _) => "<Sign><Integer><Decimals>",
        }
    };
    // split into literal text and tags (name, value), expanding <Standard Format,N>
    enum Tok {
        Lit(String),
        Tag(String, String),
    }
    let mut toks = vec![];
    let mut src = pic.to_string();
    let mut expanded = false;
    let mut i = 0;
    while i < src.len() {
        let rest = &src[i..];
        let Some(lt) = rest.find('<') else {
            toks.push(Tok::Lit(rest.to_string()));
            break;
        };
        if lt > 0 {
            toks.push(Tok::Lit(rest[..lt].to_string()));
        }
        let Some(gt) = rest[lt..].find('>') else {
            toks.push(Tok::Lit(rest[lt..].to_string()));
            break;
        };
        let inner = &rest[lt + 1..lt + gt];
        let (name, val) = match inner.split_once(',') {
            Some((n, x)) => (n.trim().to_ascii_lowercase(), x.to_string()),
            None => (inner.trim().to_ascii_lowercase(), String::new()),
        };
        // abbreviated names (`<Filler Char,0>`, `<Prec,2:2>`, `<I>`): the field names of the type
        // first, then the attributes; an exact name, else the first name it starts
        // a Decimal's <C,Code> (currency code) and <S,Symbol> come before the name lookup in BC
        let name = if !is_int && (name == "c" || name == "s") {
            name
        } else {
            let fields: &[&str] = if is_int { &["sign", "integer", "integer thousand"] } else { &["sign", "integer", "decimals", "integer thousand"] };
            bc_format_name(fields, &name).or_else(|| bc_format_name(FORMAT_ATTRIBUTES, &name)).map(str::to_string).unwrap_or(name)
        };
        let next = i + lt + gt + 1;
        if "standard format".starts_with(name.as_str()) && name.len() >= 3 && !expanded {
            let n = val.trim().parse::<i64>().unwrap_or(0);
            src = format!("{}{}", std_format(n), &src[next..]);
            expanded = true;
            i = 0;
            continue;
        }
        toks.push(Tok::Tag(name, val));
        i = next;
    }
    let (mut pmin, mut pmax): (u32, Option<u32>) = (0, None);
    let (mut thousand, mut comma) = (',', '.');
    for t in &toks {
        if let Tok::Tag(n, x) = t {
            match n.as_str() {
                "precision" => {
                    let (a, b) = x.split_once(':').unwrap_or((x.as_str(), x.as_str()));
                    pmin = a.trim().parse().unwrap_or(0);
                    pmax = b.trim().parse().ok().or(Some(pmin));
                }
                "1000character" => thousand = x.chars().next().unwrap_or(','),
                "comma" => comma = x.chars().next().unwrap_or('.'),
                _ => {}
            }
        }
    }
    let r = match pmax {
        Some(m) => d.round_dp_with_strategy(m, RoundingStrategy::MidpointAwayFromZero),
        None => d,
    };
    let r = r.normalize();
    let abs = r.abs().to_string();
    let (int, frac) = match abs.split_once('.') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (abs.clone(), String::new()),
    };
    let mut frac = frac;
    while (frac.len() as u32) < pmin {
        frac.push('0');
    }
    // fields with their width (`<Integer,4>`) and the attributes that follow them
    // (`<Filler Character,0>`): Sign, Integer and Integer Thousand are right-aligned (filler ' '
    // before the value, '*' fill on overflow except for Sign); Decimals is left-aligned with filler
    // '0' after it [src:A08.2]
    struct Piece {
        text: String,
        width: usize,
        filler: Option<char>,
        right_align: bool,
        overflow: Option<char>,
    }
    let mut pieces: Vec<Piece> = vec![];
    for t in toks {
        match t {
            Tok::Lit(l) => pieces.push(Piece { text: l, width: 0, filler: None, right_align: false, overflow: None }),
            Tok::Tag(n, x) => {
                let width = x.trim().parse::<usize>().unwrap_or(0);
                let (text, right_align, overflow) = match n.as_str() {
                    "sign" => ((if r.is_sign_negative() && !r.is_zero() { "-" } else { "" }).to_string(), true, None),
                    "integer" => (int.clone(), true, Some('*')),
                    "integer thousand" => {
                        let mut s = String::new();
                        let b = int.as_bytes();
                        for (k, c) in b.iter().enumerate() {
                            if k > 0 && (b.len() - k) % 3 == 0 {
                                s.push(thousand);
                            }
                            s.push(*c as char);
                        }
                        (s, true, Some('*'))
                    }
                    // with a width, the separator stays and the digits fill with '0' (".00")
                    "decimals" => ((if frac.is_empty() && width == 0 { String::new() } else { format!("{comma}{frac}") }), false, Some('*')),
                    "filler character" | "filler char" | "filler" => {
                        if let (Some(c), Some(p)) = (x.chars().next(), pieces.iter_mut().rev().find(|p| p.width > 0)) {
                            p.filler = Some(c);
                        }
                        continue;
                    }
                    // attributes (Precision, Comma, 1000Character), currency code and symbol
                    _ => continue,
                };
                pieces.push(Piece { text, width, filler: None, right_align, overflow });
            }
        }
    }
    let mut out = String::new();
    for p in pieces {
        let n = p.text.chars().count();
        if p.width == 0 || n == p.width {
            out.push_str(&p.text);
        } else if n > p.width {
            match p.overflow {
                Some(c) => out.push_str(&c.to_string().repeat(p.width)),
                None => out.extend(p.text.chars().take(p.width)),
            }
        } else if p.right_align {
            out.push_str(&p.filler.unwrap_or(' ').to_string().repeat(p.width - n));
            out.push_str(&p.text);
        } else {
            out.push_str(&p.text);
            out.push_str(&p.filler.unwrap_or('0').to_string().repeat(p.width - n));
        }
    }
    Some(out)
}

pub fn _unused(_: ObjectKind) {}

/// RoundDateTime on a DateTime in milliseconds since 1970 (UTC, bc-rust's client-local time):
/// BC rounds the milliseconds since 0000-01-01 (719,528 days before 1970) to `p`, down for
/// '<', up for '>', to the nearer multiple for '=' (a half goes up).
pub(crate) fn round_datetime(t: i64, p: i64, dir: u8) -> i64 {
    const EPOCH: i64 = 719_528 * 86_400_000;
    let n = t + EPOCH;
    let r = n.rem_euclid(p);
    let down = n - r;
    let up = if r > 0 { down + p } else { n };
    (match dir {
        b'<' => down,
        b'>' => up,
        _ => {
            if (up - n) <= r {
                up
            } else {
                down
            }
        }
    }) - EPOCH
}

/// Whether page property `name` is set to false (`Editable = false;`).
fn page_prop_false(p: &al_model::PageDef, name: &str) -> bool {
    p.props.get(name).is_some_and(|v| v.eq_ignore_ascii_case("false"))
}

fn find_part<'a>(tree: &'a [al_model::Control], name: &str) -> Option<&'a al_model::Control> {
    for c in tree {
        if (c.kind == "part" || c.kind == "systempart") && c.name.eq_ignore_ascii_case(name) {
            return Some(c);
        }
        if let Some(x) = find_part(&c.children, name) {
            return Some(x);
        }
    }
    None
}

impl<'r> Session<'r> {
    /// The record field an expression refers to (`Rec.Field`, an implicit-Rec field, a FieldRef).
    pub fn field_of_expr(&mut self, f: &mut Frame, e: &Expr) -> R<Option<(crate::record::RecHandle, usize)>> {
        match e {
            Expr::Member(base, name) => {
                // only plain variables: the base is evaluated again by the caller
                match &**base {
                    Expr::Ident(b) if !crate::builtins::is_static_class(&b.to_ascii_lowercase()) => {}
                    _ => return Ok(None),
                }
                match self.eval(f, base)? {
                    Val::Record(r) => {
                        let fi = r.borrow().tdef.field(name);
                        Ok(fi.map(|i| (r, i)))
                    }
                    _ => Ok(None),
                }
            }
            Expr::Ident(n) => {
                if let Some(v) = f.locals.iter().rev().chain(f.globals.vars.iter()).find(|v| v.name.eq_ignore_ascii_case(n)) {
                    return Ok(match &*v.slot.borrow() {
                        Val::FieldRef(h, fi) => Some((h.clone(), *fi)),
                        _ => None,
                    });
                }
                let rec = f.with.iter().rev().find(|r| r.borrow().tdef.field(n).is_some()).cloned().or_else(|| f.rec.clone());
                Ok(rec.and_then(|r| {
                    let fi = r.borrow().tdef.field(n);
                    fi.map(|i| (r, i))
                }))
            }
            _ => Ok(None),
        }
    }

    /// A page instance's Editable (BC NavForm.Editable): what AL set, else the page's Editable
    /// property (default true).
    pub(crate) fn page_editable(&self, inst: &Rc<Instance>, page_id: i64) -> bool {
        inst.editable.get().unwrap_or_else(|| self.cat.pages.get(&page_id).map(|p| !page_prop_false(p, "editable")).unwrap_or(true))
    }

    /// CurrPage.X inside page code.
    fn curr_page(&mut self, f: &mut Frame, m: &str, args: &[Expr]) -> R<Val> {
        let inst = f.inst.clone();
        let co = self.code(f.code);
        let page_id = match co.kind {
            ObjectKind::PageExtension => co.extends.map(|e| e.1).unwrap_or(co.id),
            _ => co.id,
        };
        let pd = self.cat.pages.get(&page_id);
        match m {
            // CurrPage.Close: the page closes after the running trigger (a TestPage: crate::testpage)
            "close" if args.is_empty() => {
                if let Some(i) = &inst {
                    i.close.set(true);
                }
                Ok(Val::Void)
            }
            // page background tasks (crate::pbt)
            "enqueuebackgroundtask" => self.pbt_enqueue(f, args),
            "cancelbackgroundtask" => self.pbt_cancel(f, args),
            "lookupmode" => {
                if let (Some(a), Some(i)) = (args.first(), &inst) {
                    let on = self.eval(f, a)?.as_bool();
                    i.lookup.set(on);
                }
                Ok(Val::Bool(inst.map(|i| i.lookup.get()).unwrap_or(false)))
            }
            "caption" => Ok(Val::Text(pd.map(|p| p.caption.clone()).unwrap_or_default())),
            "objectid" => {
                let names = match args.first() {
                    Some(a) => self.eval(f, a)?.as_bool(),
                    None => false,
                };
                Ok(Val::Text(match (names, pd) {
                    (true, Some(p)) => format!("Page {}", p.name),
                    _ => format!("Page {page_id}"),
                }))
            }
            // CurrPage.Editable([X]): BC NavForm.Editable, the page's Editable property until AL
            // sets it (the open mode does not change it: the client applies View mode itself)
            "editable" => {
                let set = match args.first() {
                    Some(a) => Some(self.eval(f, a)?.as_bool()),
                    None => None,
                };
                let Some(i) = inst else { return Ok(Val::Bool(pd.map(|p| !page_prop_false(p, "editable")).unwrap_or(true))) };
                if set.is_some() {
                    i.editable.set(set);
                }
                Ok(Val::Bool(self.page_editable(&i, page_id)))
            }
            // CurrPage.SaveRecord(): the current record is saved now; a new line is inserted (BC)
            "saverecord" if pd.and_then(|p| find_part(&p.layout, m)).is_none() => {
                if let Some(i) = &inst {
                    let tp = i.test_page.borrow().as_ref().and_then(|w| w.upgrade());
                    // NavForm.SaveRecordAsync: a pending new line is inserted (OnInsertRecord,
                    // BelowxRec); an existing record is written when it differs from its xRec
                    match tp.filter(|t| t.borrow().new_pending) {
                        Some(tp) => self.page_save_new(&tp)?,
                        None => self.page_update_save(i)?,
                    }
                }
                Ok(Val::Void)
            }
            // CurrPage.Update([SaveRecord]): the page re-reads its record afterwards (TestPage);
            // SaveRecord (default true) saves the record now (BC NavForm.UpdateAsync)
            "update" if pd.and_then(|p| find_part(&p.layout, m)).is_none() => {
                let mut save = true;
                for a in args {
                    save = self.eval(f, a)?.as_bool();
                }
                if let Some(i) = &inst {
                    if save {
                        self.page_update_save(i)?;
                    } else {
                        // re-read without saving (crate::testpage tp_save_if_changed)
                        i.update_nosave.set(true);
                    }
                    i.update.set(true);
                }
                Ok(Val::Opaque("PageControl"))
            }
            "setselectionfilter" => {
                let Some(a) = args.first() else { return Ok(Val::Void) };
                let lv = self.lvalue(f, a)?;
                if let (Val::Record(dst), Some(src)) = (self.read_lval(&lv), f.rec.clone()) {
                    if dst.borrow().tdef.id == src.borrow().tdef.id && !Rc::ptr_eq(&dst, &src) {
                        let s = src.borrow();
                        let mut d = dst.borrow_mut();
                        d.filters = s.filters.clone();
                        d.row = s.row.clone();
                        if s.temp.is_some() {
                            d.temp = s.temp.clone();
                        }
                        let pkf = d.tdef.pk().fields.clone();
                        for fi in pkf {
                            let v = d.row[fi].clone();
                            let t = to_val(d.field(fi), &v).to_text();
                            d.set_filter(fi, Some(crate::filter::FNode::Eq(v)), t);
                        }
                    }
                }
                Ok(Val::Void)
            }
            _ => {
                for a in args {
                    let _ = self.eval(f, a)?;
                }
                // CurrPage.<Part>: the part's page instance
                let part = pd.and_then(|p| find_part(&p.layout, m));
                if let (Some(c), Some(i)) = (part, &inst) {
                    if let Some(pid) = c.part_page.as_deref().and_then(|n| self.cat.id_of(ObjectKind::Page, n)) {
                        let existing = i.parts.borrow().iter().find(|(n, _)| n.eq_ignore_ascii_case(m)).map(|(_, x)| x.clone());
                        let pi = match existing {
                            Some(x) => x,
                            None => {
                                let x = Instance::new(self.cat.pages[&pid].code);
                                i.parts.borrow_mut().push((m.into(), x.clone()));
                                x
                            }
                        };
                        // the part shows the parent's current record's lines (SubPageLink)
                        let parent_rec = i.rec.borrow().clone();
                        if let Some(pr) = parent_rec {
                            let (part_rec, fresh) = match pi.rec.borrow().clone() {
                                Some(r) => (Some(r), false),
                                None => (self.cat.pages[&pid].source_table.map(|t| self.new_record(t, false)), true),
                            };
                            if let Some(r) = part_rec {
                                if fresh {
                                    *pi.rec.borrow_mut() = Some(r.clone());
                                }
                                // the link's filters (group 4), order-independent
                                let link = |r: &RecHandle| {
                                    let mut v: Vec<String> = r.borrow().filters.iter().filter(|x| x.group == 4).map(|x| format!("{}={:?}", x.field, x.node)).collect();
                                    v.sort();
                                    v
                                };
                                let before = link(&r);
                                self.apply_conds(&r, &c.sub_page_link, Some(&pr), 4)?;
                                if fresh || before != link(&r) {
                                    // the parent moved: the part reloads from its first record
                                    self.find_first(&r, false)?;
                                } else {
                                    // the part refreshes from its current row ("=><"): it stays
                                    // while the record is in its view, else the next, else the previous
                                    let in_view = matches!(self.rec_get_pk(&r), Some(row) if r.borrow().row_matches(&row));
                                    if !in_view && self.next(&r, 1)? == 0 && self.next(&r, -1)? == 0 {
                                        self.find_first(&r, false)?;
                                    }
                                }
                            }
                        }
                        return Ok(Val::Object(pi));
                    }
                }
                // Update, SaveRecord, Close, Activate, control properties ...
                Ok(Val::Opaque("PageControl"))
            }
        }
    }

    /// StartSession(var SessionId; CodeunitId [; Company [; Record [; Timeout]]]).
    /// Background sessions are emulated inline: the codeunit runs to completion right away in an
    /// isolated scope (no UI, its Commit() is a savepoint, its errors are rolled back and do not
    /// reach the caller). Its changes become durable when the caller's transaction commits.
    fn start_session(&mut self, f: &mut Frame, args: &[Expr]) -> R<Val> {
        let lv = self.lvalue(f, &args[0])?;
        let cu = self.eval(f, &args[1])?.as_i64();
        let company = match args.get(2) {
            Some(a) => self.eval(f, a)?.to_text(),
            None => String::new(),
        };
        let rec = match args.get(3) {
            Some(a) => match self.eval(f, a)? {
                Val::Record(r) => Some(crate::record::copy_record(&r)),
                _ => None,
            },
            None => None,
        };
        if !self.cat.code_by_key.contains_key(&(ObjectKind::Codeunit, cu)) {
            return err(format!("Codeunit {cu} does not exist"));
        }
        // bc serve with the background pool: a real session on a thread of its own (crate::workers)
        if let Some(p) = crate::workers::pool().filter(|_| self.test.is_none()) {
            let rec = rec.map(|r| {
                let b = r.borrow();
                (b.tdef.id, b.row.to_vec())
            });
            let company = if company.is_empty() { self.company.clone() } else { company };
            return match p.start_session(cu, company, self.user_id.clone(), rec) {
                Some(id) => {
                    self.write_lval(&lv, Val::Int(id))?;
                    Ok(Val::Bool(true))
                }
                None => Ok(Val::Bool(false)),
            };
        }
        let saved_company = (!company.is_empty() && company != self.company).then(|| std::mem::replace(&mut self.company, company));
        let saved_test = self.test.take();
        let saved_err = (self.last_error.clone(), self.last_error_code.clone(), self.last_error_stack.clone());
        self.bg_depth += 1;
        let saved_commit = self.bg_commit_mark.take();
        let mark = self.db.savepoint().map_err(Self::store_err)?;
        let res = self.run_codeunit(cu, rec);
        // an error rolls back to the session's last Commit() (what it committed stays)
        let back = self.bg_commit_mark.take().unwrap_or(mark);
        self.bg_commit_mark = saved_commit;
        self.bg_depth -= 1;
        self.test = saved_test;
        if let Some(c) = saved_company {
            self.company = c;
        }
        (self.last_error, self.last_error_code, self.last_error_stack) = saved_err;
        match res {
            Ok(()) => {}
            Err(Flow::Err(_)) => self.db.rollback_to(back).map_err(Self::store_err)?,
            Err(o) => return Err(o),
        }
        self.last_session_id += 1;
        let id = self.last_session_id;
        self.write_lval(&lv, Val::Int(id))?;
        Ok(Val::Bool(true))
    }

    /// LookupPageID of a table (Page.RunModal(0, Rec)).
    pub fn lookup_page_of(&self, t: &al_model::TableDef) -> Option<i64> {
        t.lookup_page.as_deref().and_then(|p| self.cat.id_of(ObjectKind::Page, p.trim_start_matches("Page::").trim_matches('"')))
    }
}

thread_local! {
    /// Random() on this thread: a seeded .NET System.Random, or None (every value from a fresh UUID).
    static RANDOM_STATE: std::cell::RefCell<Option<NetRandom>> = const { std::cell::RefCell::new(None) };
    /// Randomize(Seed) seeds Random() (BC's behavior); off by default on a thread.
    static RANDOMIZE_SEEDS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Let Randomize(Seed) seed Random() on this thread, as BC does.
pub fn set_randomize_seeds(on: bool) {
    RANDOMIZE_SEEDS.with(|c| c.set(on));
}

/// Seed Random() on this thread (0 unseeds it). al-test --seed-random seeds it per test, so test
/// data (LibraryRandom) repeat from run to run.
pub fn seed_random(seed: u64) {
    RANDOM_STATE.with(|r| *r.borrow_mut() = if seed == 0 { None } else { Some(NetRandom::new(seed as i32)) });
}

/// A value in 0..n (n >= 1): System.Random.Next(n) when seeded, else from a fresh UUID.
fn random_below(n: i32) -> i32 {
    RANDOM_STATE.with(|r| match r.borrow_mut().as_mut() {
        Some(rng) => rng.next_below(n),
        None => (uuid::Uuid::new_v4().as_u128() % n as u128) as i32,
    })
}

/// .NET's seeded System.Random (Knuth's subtractive generator; .NET Core keeps it for
/// `new Random(seed)` as Net5CompatSeedImpl), which BC's Randomize(Seed) creates.
pub struct NetRandom {
    seeds: [i32; 56],
    inext: usize,
    inextp: usize,
}

impl NetRandom {
    pub fn new(seed: i32) -> NetRandom {
        const MBIG: i32 = i32::MAX;
        const MSEED: i32 = 161_803_398;
        let mut seeds = [0i32; 56];
        let subtraction = if seed == i32::MIN { i32::MAX } else { seed.abs() };
        let mut mj = MSEED - subtraction;
        seeds[55] = mj;
        let mut mk = 1i32;
        let mut ii = 0usize;
        for _ in 1..55 {
            ii += 21;
            if ii >= 55 {
                ii -= 55;
            }
            seeds[ii] = mk;
            mk = mj.wrapping_sub(mk);
            if mk < 0 {
                mk = mk.wrapping_add(MBIG);
            }
            mj = seeds[ii];
        }
        for _ in 1..5 {
            for i in 1..56 {
                let mut n = i + 30;
                if n >= 55 {
                    n -= 55;
                }
                seeds[i] = seeds[i].wrapping_sub(seeds[1 + n]);
                if seeds[i] < 0 {
                    seeds[i] = seeds[i].wrapping_add(MBIG);
                }
            }
        }
        NetRandom { seeds, inext: 0, inextp: 21 }
    }

    /// InternalSample: the next value in 0..i32::MAX.
    pub fn sample_int(&mut self) -> i32 {
        let mut a = self.inext + 1;
        if a >= 56 {
            a = 1;
        }
        let mut b = self.inextp + 1;
        if b >= 56 {
            b = 1;
        }
        let mut v = self.seeds[a].wrapping_sub(self.seeds[b]);
        if v == i32::MAX {
            v -= 1;
        }
        if v < 0 {
            v = v.wrapping_add(i32::MAX);
        }
        self.seeds[a] = v;
        self.inext = a;
        self.inextp = b;
        v
    }

    /// Next(maxValue) = (int)(Sample() * maxValue), Sample() = InternalSample() / int.MaxValue.
    pub fn next_below(&mut self, max: i32) -> i32 {
        (self.sample_int() as f64 * (1.0 / i32::MAX as f64) * max as f64) as i32
    }
}
