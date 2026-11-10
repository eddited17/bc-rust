//! RDLC expressions (stage 2 of the report layout engine; see docs/reports.md): the VB.NET subset
//! BC's layouts use. A value that does not start with `=` is a literal text.
//!
//! - references: `Fields!X.Value` (and `.IsMissing`), `Parameters!X.Value`, `ReportItems!X.Value`,
//!   `Globals!PageNumber` and friends, `User!UserID`, `User!Language`;
//! - custom code: `Code.Name(args)` (the layout's `<Code>` block), resolved by the context;
//! - aggregates: `Sum`, `First`, `Last`, `Count`, `CountDistinct`, `Max`, `Min`, `Avg`,
//!   `RunningValue`, `RowNumber`, `CountRows`, with an optional scope, evaluated by the context
//!   over the rows of that scope;
//! - functions: `IIf`, `Format`, `FormatNumber`, `CStr`, `CInt`, `CDbl`, `CDec`, `CBool`, `Len`,
//!   `Trim`/`LTrim`/`RTrim`, `Left`, `Right`, `Mid`, `UCase`, `LCase`, `InStr`, `Replace`,
//!   `IsNothing`, `Choose`, `Chr`, `Asc`, `Year`, `Month`, `Day`, `Today`, `Now`, `Convert.ToX`;
//! - operators with VB precedence: `^`, unary `-`, `* /`, `\`, `Mod`, `+ -`, `&`, comparisons
//!   (`= <> < <= > >=`), `Not`, `And`/`AndAlso`, `Or`/`OrElse`, `Xor`.
//!
//! Anything else is an error, which the engine reports instead of guessing (the caller falls back).

use std::fmt;

/// A value: VB's Variant as the layouts use it.
#[derive(Debug, Clone, PartialEq)]
pub enum V {
    Null,
    Bool(bool),
    Num(f64),
    Text(String),
}

impl Default for V {
    fn default() -> V {
        V::Null
    }
}

impl V {
    pub fn as_text(&self) -> String {
        match self {
            V::Null => String::new(),
            V::Bool(b) => if *b { "True" } else { "False" }.into(),
            V::Num(n) => fmt_num(*n),
            V::Text(s) => s.clone(),
        }
    }
    pub fn as_num(&self) -> f64 {
        match self {
            V::Null => 0.0,
            V::Bool(b) => {
                if *b {
                    -1.0
                } else {
                    0.0
                }
            }
            V::Num(n) => *n,
            V::Text(s) => s.trim().replace(',', "").parse().unwrap_or(0.0),
        }
    }
    pub fn as_bool(&self) -> bool {
        match self {
            V::Null => false,
            V::Bool(b) => *b,
            V::Num(n) => *n != 0.0,
            V::Text(s) => s.eq_ignore_ascii_case("true") || s.trim().parse::<f64>().is_ok_and(|n| n != 0.0),
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, V::Null)
    }
}

/// VB's default number-to-text: no trailing zeros, up to 15 significant digits.
pub fn fmt_num(n: f64) -> String {
    if n == n.trunc() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{:.10}", n);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

impl fmt::Display for V {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_text())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(V),
    /// `Fields!X.Value` / `.IsMissing`
    Field(String, bool),
    Param(String),
    ReportItem(String),
    Global(String),
    User(String),
    /// `Code.Name(args)`
    Code(String, Vec<Expr>),
    /// `Name(args)` (functions and aggregates; names lower-cased)
    Call(String, Vec<Expr>),
    Unary(char, Box<Expr>),
    Not(Box<Expr>),
    Bin(Op, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Pow,
    Mul,
    Div,
    IntDiv,
    Mod,
    Add,
    Sub,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Xor,
}

// ------------------------------------------------------------------ tokens

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Bang,
    Dot,
    Comma,
    LParen,
    RParen,
    Op(String),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() || (ch == '_' && c.get(i + 1).is_some_and(|n| *n == '\r' || *n == '\n')) {
            i += 1;
            continue;
        }
        if ch == '"' {
            let mut t = String::new();
            i += 1;
            loop {
                match c.get(i) {
                    None => return Err("unterminated string".into()),
                    Some('"') if c.get(i + 1) == Some(&'"') => {
                        t.push('"');
                        i += 2;
                    }
                    Some('"') => {
                        i += 1;
                        break;
                    }
                    Some(x) => {
                        t.push(*x);
                        i += 1;
                    }
                }
            }
            out.push(Tok::Str(t));
            continue;
        }
        if ch.is_ascii_digit() || (ch == '.' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '.') {
                i += 1;
            }
            // VB type suffixes (1D, 2.5R, 3I ...)
            if i < c.len() && matches!(c[i].to_ascii_uppercase(), 'D' | 'R' | 'F' | 'I' | 'L' | 'S' | '@' | '!' | '#') && !c.get(i + 1).is_some_and(|n| n.is_alphanumeric()) {
                i += 1;
                let n: String = c[start..i - 1].iter().collect();
                out.push(Tok::Num(n.parse().map_err(|_| format!("bad number {n}"))?));
                continue;
            }
            let n: String = c[start..i].iter().collect();
            out.push(Tok::Num(n.parse().map_err(|_| format!("bad number {n}"))?));
            continue;
        }
        if ch.is_alphabetic() || ch == '_' || ch == '[' {
            if ch == '[' {
                // [Name] escapes an identifier
                let start = i + 1;
                while i < c.len() && c[i] != ']' {
                    i += 1;
                }
                out.push(Tok::Ident(c[start..i].iter().collect()));
                i += 1;
                continue;
            }
            let start = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(c[start..i].iter().collect()));
            continue;
        }
        let two: String = c[i..(i + 2).min(c.len())].iter().collect();
        if matches!(two.as_str(), "<>" | "<=" | ">=") {
            out.push(Tok::Op(two));
            i += 2;
            continue;
        }
        let t = match ch {
            '!' => Tok::Bang,
            '.' => Tok::Dot,
            ',' => Tok::Comma,
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '+' | '-' | '*' | '/' | '\\' | '^' | '&' | '=' | '<' | '>' => Tok::Op(ch.to_string()),
            _ => return Err(format!("unexpected character '{ch}'")),
        };
        out.push(t);
        i += 1;
    }
    Ok(out)
}

// ------------------------------------------------------------------ parser

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        t
    }
    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn kw(&self, k: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case(k))
    }
    fn op(&self, o: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(s)) if s == o)
    }

    // precedence, lowest first: Or/OrElse/Xor, And/AndAlso, Not, comparisons, &, + -, Mod, \, * /, unary -, ^
    fn expr(&mut self) -> Result<Expr, String> {
        let mut l = self.and()?;
        loop {
            let op = if self.kw("or") || self.kw("orelse") {
                Op::Or
            } else if self.kw("xor") {
                Op::Xor
            } else {
                return Ok(l);
            };
            self.i += 1;
            let r = self.and()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn and(&mut self) -> Result<Expr, String> {
        let mut l = self.not()?;
        while self.kw("and") || self.kw("andalso") {
            self.i += 1;
            let r = self.not()?;
            l = Expr::Bin(Op::And, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn not(&mut self) -> Result<Expr, String> {
        if self.kw("not") {
            self.i += 1;
            return Ok(Expr::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<Expr, String> {
        let mut l = self.concat()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op(o)) => match o.as_str() {
                    "=" => Op::Eq,
                    "<>" => Op::Ne,
                    "<" => Op::Lt,
                    "<=" => Op::Le,
                    ">" => Op::Gt,
                    ">=" => Op::Ge,
                    _ => return Ok(l),
                },
                _ => return Ok(l),
            };
            self.i += 1;
            let r = self.concat()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn concat(&mut self) -> Result<Expr, String> {
        let mut l = self.add()?;
        while self.op("&") {
            self.i += 1;
            let r = self.add()?;
            l = Expr::Bin(Op::Concat, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn add(&mut self) -> Result<Expr, String> {
        let mut l = self.modulo()?;
        loop {
            let op = if self.op("+") {
                Op::Add
            } else if self.op("-") {
                Op::Sub
            } else {
                return Ok(l);
            };
            self.i += 1;
            let r = self.modulo()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn modulo(&mut self) -> Result<Expr, String> {
        let mut l = self.intdiv()?;
        while self.kw("mod") {
            self.i += 1;
            let r = self.intdiv()?;
            l = Expr::Bin(Op::Mod, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn intdiv(&mut self) -> Result<Expr, String> {
        let mut l = self.mul()?;
        while self.op("\\") {
            self.i += 1;
            let r = self.mul()?;
            l = Expr::Bin(Op::IntDiv, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn mul(&mut self) -> Result<Expr, String> {
        let mut l = self.unary()?;
        loop {
            let op = if self.op("*") {
                Op::Mul
            } else if self.op("/") {
                Op::Div
            } else {
                return Ok(l);
            };
            self.i += 1;
            let r = self.unary()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
    }
    fn unary(&mut self) -> Result<Expr, String> {
        if self.op("-") {
            self.i += 1;
            return Ok(Expr::Unary('-', Box::new(self.unary()?)));
        }
        if self.op("+") {
            self.i += 1;
            return self.unary();
        }
        self.pow()
    }
    fn pow(&mut self) -> Result<Expr, String> {
        let l = self.primary()?;
        if self.op("^") {
            self.i += 1;
            let r = self.unary()?;
            return Ok(Expr::Bin(Op::Pow, Box::new(l), Box::new(r)));
        }
        Ok(l)
    }
    fn args(&mut self) -> Result<Vec<Expr>, String> {
        let mut a = vec![];
        if self.eat(&Tok::RParen) {
            return Ok(a);
        }
        loop {
            a.push(self.expr()?);
            if self.eat(&Tok::Comma) {
                continue;
            }
            if self.eat(&Tok::RParen) {
                return Ok(a);
            }
            return Err("expected , or )".into());
        }
    }
    fn primary(&mut self) -> Result<Expr, String> {
        let mut e = self.primary0()?;
        // a method on the value: (...).ToString(), Code.F(x).Trim()
        while self.eat(&Tok::Dot) {
            let m = match self.next() {
                Some(Tok::Ident(m)) => m.to_ascii_lowercase(),
                _ => return Err("expected a method after .".into()),
            };
            let mut args = vec![e];
            if self.eat(&Tok::LParen) {
                args.extend(self.args()?);
            }
            e = Expr::Call(format!(".{m}"), args);
        }
        Ok(e)
    }
    fn primary0(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Expr::Lit(V::Num(n))),
            Some(Tok::Str(s)) => Ok(Expr::Lit(V::Text(s))),
            Some(Tok::LParen) => {
                let e = self.expr()?;
                if !self.eat(&Tok::RParen) {
                    return Err("expected )".into());
                }
                Ok(e)
            }
            Some(Tok::Ident(id)) => {
                let lid = id.to_ascii_lowercase();
                match lid.as_str() {
                    "true" => return Ok(Expr::Lit(V::Bool(true))),
                    "false" => return Ok(Expr::Lit(V::Bool(false))),
                    "nothing" => return Ok(Expr::Lit(V::Null)),
                    _ => {}
                }
                // Collection!Member[.Value]
                if self.eat(&Tok::Bang) {
                    let member = match self.next() {
                        Some(Tok::Ident(m)) => m,
                        _ => return Err(format!("expected a member after {id}!")),
                    };
                    let mut prop = String::from("value");
                    if self.eat(&Tok::Dot) {
                        match self.next() {
                            Some(Tok::Ident(p)) => prop = p.to_ascii_lowercase(),
                            _ => return Err("expected a property".into()),
                        }
                    }
                    return Ok(match lid.as_str() {
                        "fields" => Expr::Field(member, prop == "ismissing"),
                        "parameters" => Expr::Param(member),
                        "reportitems" => Expr::ReportItem(member),
                        "globals" => Expr::Global(member),
                        "user" => Expr::User(member),
                        _ => return Err(format!("unknown collection {id}")),
                    });
                }
                // Code.Name(args) / Convert.ToX(args) / System.Math.X ...
                if self.eat(&Tok::Dot) {
                    let mut name = match self.next() {
                        Some(Tok::Ident(m)) => m,
                        _ => return Err(format!("expected a member after {id}.")),
                    };
                    while self.eat(&Tok::Dot) {
                        match self.next() {
                            Some(Tok::Ident(m)) => name = m,
                            _ => return Err("expected a member".into()),
                        }
                    }
                    let args = if self.eat(&Tok::LParen) { self.args()? } else { vec![] };
                    return Ok(if lid == "code" { Expr::Code(name, args) } else { Expr::Call(format!("{}.{}", lid, name.to_ascii_lowercase()), args) });
                }
                if self.eat(&Tok::LParen) {
                    let args = self.args()?;
                    return Ok(Expr::Call(lid, args));
                }
                // a bare function without parentheses (Today, Now)
                Ok(Expr::Call(lid, vec![]))
            }
            t => Err(format!("unexpected {t:?}")),
        }
    }
}

/// Parse an expression. A value without a leading `=` is a literal text.
pub fn parse(src: &str) -> Result<Expr, String> {
    let s = src.trim_start();
    let Some(body) = s.strip_prefix('=') else {
        return Ok(Expr::Lit(V::Text(src.to_string())));
    };
    let mut p = P { t: lex(body)?, i: 0 };
    let e = p.expr()?;
    if p.i < p.t.len() {
        return Err(format!("unexpected {:?}", p.t[p.i]));
    }
    Ok(e)
}

// ------------------------------------------------------------------ evaluation

/// What an expression can see: the current row, parameters, report items, aggregates over a scope
/// and the layout's custom code. Implemented by the binding stage.
pub trait Ctx {
    fn field(&mut self, name: &str) -> Option<V>;
    fn param(&mut self, name: &str) -> V;
    fn report_item(&mut self, name: &str) -> V;
    fn global(&mut self, name: &str) -> V;
    /// Sum/First/Last/Count/... of `arg` over the rows of `scope` (None: the current scope).
    fn aggregate(&mut self, func: &str, arg: Option<&Expr>, scope: Option<&str>) -> Result<V, String>;
    fn code(&mut self, name: &str, args: Vec<V>) -> Result<V, String>;
    /// Whether `name` (lower case) is a variable or function of the running <Code> (rdlc_vb):
    /// those come before the built-in functions.
    fn has_ident(&self, _name: &str) -> bool {
        false
    }
    fn ident(&mut self, name: &str, _args: Vec<V>) -> Result<V, String> {
        Err(format!("unknown identifier {name}"))
    }
}

pub const AGGREGATES: &[&str] = &["sum", "first", "last", "count", "countdistinct", "max", "min", "avg", "runningvalue", "rownumber", "countrows"];

pub fn eval(e: &Expr, cx: &mut dyn Ctx) -> Result<V, String> {
    Ok(match e {
        Expr::Lit(v) => v.clone(),
        Expr::Field(n, missing) => match cx.field(n) {
            Some(v) if !*missing => v,
            Some(_) => V::Bool(false),
            None if *missing => V::Bool(true),
            None => return Err(format!("unknown field {n}")),
        },
        Expr::Param(n) => cx.param(n),
        Expr::ReportItem(n) => cx.report_item(n),
        Expr::Global(n) => cx.global(n),
        Expr::User(n) => {
            if n.eq_ignore_ascii_case("userid") {
                cx.global("UserID")
            } else if n.eq_ignore_ascii_case("language") {
                // the context answers User!Language (rdlc_bind: the report's FormatRegion)
                cx.global("Language")
            } else {
                V::Text(String::new())
            }
        }
        Expr::Code(n, args) => {
            let a = args.iter().map(|x| eval(x, cx)).collect::<Result<Vec<_>, _>>()?;
            cx.code(n, a)?
        }
        Expr::Unary(_, x) => V::Num(-eval(x, cx)?.as_num()),
        Expr::Not(x) => V::Bool(!eval(x, cx)?.as_bool()),
        Expr::Bin(op, l, r) => {
            // AndAlso/OrElse short-circuit; for these layouts And/Or behave the same on booleans
            if *op == Op::And {
                let a = eval(l, cx)?;
                if !a.as_bool() {
                    return Ok(V::Bool(false));
                }
                return Ok(V::Bool(eval(r, cx)?.as_bool()));
            }
            if *op == Op::Or {
                let a = eval(l, cx)?;
                if a.as_bool() {
                    return Ok(V::Bool(true));
                }
                return Ok(V::Bool(eval(r, cx)?.as_bool()));
            }
            let a = eval(l, cx)?;
            let b = eval(r, cx)?;
            binop(*op, &a, &b)
        }
        Expr::Call(name, args) => call(name, args, cx)?,
    })
}

fn is_numeric(v: &V) -> bool {
    match v {
        V::Num(_) | V::Bool(_) | V::Null => true,
        V::Text(s) => {
            let t = s.trim();
            !t.is_empty() && t.parse::<f64>().is_ok()
        }
    }
}

fn binop(op: Op, a: &V, b: &V) -> V {
    match op {
        Op::Concat => V::Text(a.as_text() + &b.as_text()),
        Op::Add => {
            // VB (Option Strict Off): + with a String operand concatenates, Nothing counting as "";
            // numbers (and the dataset's numeric texts, its decimal fields) add
            let text = matches!(a, V::Text(_)) || matches!(b, V::Text(_));
            let numbers = is_numeric(a) && is_numeric(b) && !a.is_null() && !b.is_null();
            if text && !numbers {
                V::Text(a.as_text() + &b.as_text())
            } else {
                V::Num(a.as_num() + b.as_num())
            }
        }
        Op::Sub => V::Num(a.as_num() - b.as_num()),
        Op::Mul => V::Num(a.as_num() * b.as_num()),
        Op::Div => V::Num(a.as_num() / b.as_num()),
        Op::IntDiv => V::Num((a.as_num() / b.as_num()).trunc()),
        Op::Mod => V::Num(a.as_num() % b.as_num()),
        Op::Pow => V::Num(a.as_num().powf(b.as_num())),
        Op::Xor => V::Bool(a.as_bool() ^ b.as_bool()),
        Op::And => V::Bool(a.as_bool() && b.as_bool()),
        Op::Or => V::Bool(a.as_bool() || b.as_bool()),
        Op::Eq | Op::Ne | Op::Lt | Op::Le | Op::Gt | Op::Ge => {
            let ord = if is_numeric(a) && is_numeric(b) && !(matches!(a, V::Null) && matches!(b, V::Text(_))) {
                a.as_num().partial_cmp(&b.as_num()).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                a.as_text().cmp(&b.as_text())
            };
            use std::cmp::Ordering::*;
            V::Bool(match op {
                Op::Eq => ord == Equal,
                Op::Ne => ord != Equal,
                Op::Lt => ord == Less,
                Op::Le => ord != Greater,
                Op::Gt => ord == Greater,
                _ => ord != Less,
            })
        }
    }
}

fn arg<'a>(args: &'a [Expr], i: usize, name: &str) -> Result<&'a Expr, String> {
    args.get(i).ok_or_else(|| format!("{name}: missing argument {}", i + 1))
}

fn call(name: &str, args: &[Expr], cx: &mut dyn Ctx) -> Result<V, String> {
    if AGGREGATES.contains(&name) {
        let scope = match args.get(1) {
            Some(Expr::Lit(V::Text(s))) => Some(s.as_str()),
            Some(Expr::Lit(V::Null)) | None => None,
            Some(other) if name == "runningvalue" => {
                let _ = other;
                match args.get(2) {
                    Some(Expr::Lit(V::Text(s))) => Some(s.as_str()),
                    _ => None,
                }
            }
            Some(_) => return Err(format!("{name}: the scope must be a literal")),
        };
        return cx.aggregate(name, args.first(), scope);
    }
    if cx.has_ident(name) {
        let vals = args.iter().map(|a| eval(a, cx)).collect::<Result<Vec<_>, _>>()?;
        return cx.ident(name, vals);
    }
    let mut ev = |i: usize| -> Result<V, String> { eval(arg(args, i, name)?, cx) };
    Ok(match name {
        // VB Math.Round / Round: to even, optional digits
        "round" | "math.round" => {
            let x = ev(0)?.as_num();
            let d = if args.len() > 1 { ev(1)?.as_num() as i32 } else { 0 };
            let f = 10f64.powi(d);
            let y = x * f;
            let r = y.round();
            let r = if (y - y.trunc()).abs() == 0.5 && r % 2.0 != 0.0 { r - y.signum() } else { r };
            V::Num(r / f)
        }
        "abs" | "math.abs" => V::Num(ev(0)?.as_num().abs()),
        "int" | "math.floor" => V::Num(ev(0)?.as_num().floor()),
        "fix" | "math.truncate" => V::Num(ev(0)?.as_num().trunc()),
        // picture values are base64 text already
        "convert.tobase64string" => V::Text(ev(0)?.as_text()),
        // methods on a value (postfix `.Name(...)`): the value is the first argument
        ".tostring" => V::Text(ev(0)?.as_text()),
        ".trim" => V::Text(ev(0)?.as_text().trim().to_string()),
        ".toupper" => V::Text(ev(0)?.as_text().to_uppercase()),
        ".tolower" => V::Text(ev(0)?.as_text().to_lowercase()),
        ".length" => V::Num(ev(0)?.as_text().chars().count() as f64),
        ".substring" => {
            let s: Vec<char> = ev(0)?.as_text().chars().collect();
            let a = (ev(1)?.as_num().max(0.0) as usize).min(s.len());
            let b = if args.len() > 2 { (a + ev(2)?.as_num().max(0.0) as usize).min(s.len()) } else { s.len() };
            V::Text(s[a..b].iter().collect())
        }
        "iif" => {
            let c = ev(0)?.as_bool();
            if c {
                ev(1)?
            } else {
                ev(2)?
            }
        }
        "cstr" | "convert.tostring" | "tostring" => V::Text(ev(0)?.as_text()),
        "cint" | "convert.toint32" | "clng" | "convert.toint64" => V::Num(ev(0)?.as_num().round()),
        "cdbl" | "cdec" | "csng" | "convert.todecimal" | "convert.todouble" | "val" => V::Num(ev(0)?.as_num()),
        "cbool" | "convert.toboolean" => V::Bool(ev(0)?.as_bool()),
        "len" => V::Num(ev(0)?.as_text().chars().count() as f64),
        "trim" => V::Text(ev(0)?.as_text().trim().to_string()),
        "ltrim" => V::Text(ev(0)?.as_text().trim_start().to_string()),
        "rtrim" => V::Text(ev(0)?.as_text().trim_end().to_string()),
        "ucase" => V::Text(ev(0)?.as_text().to_uppercase()),
        "lcase" => V::Text(ev(0)?.as_text().to_lowercase()),
        "left" => {
            let s = ev(0)?.as_text();
            let n = ev(1)?.as_num().max(0.0) as usize;
            V::Text(s.chars().take(n).collect())
        }
        "right" => {
            let s: Vec<char> = ev(0)?.as_text().chars().collect();
            let n = (ev(1)?.as_num().max(0.0) as usize).min(s.len());
            V::Text(s[s.len() - n..].iter().collect())
        }
        "mid" => {
            let s: Vec<char> = ev(0)?.as_text().chars().collect();
            let start = (ev(1)?.as_num().max(1.0) as usize - 1).min(s.len());
            let len = if args.len() > 2 { ev(2)?.as_num().max(0.0) as usize } else { s.len() };
            V::Text(s[start..(start + len).min(s.len())].iter().collect())
        }
        "instr" => {
            let (hay, needle) = if args.len() > 2 { (ev(1)?.as_text(), ev(2)?.as_text()) } else { (ev(0)?.as_text(), ev(1)?.as_text()) };
            V::Num(hay.find(&needle).map(|i| hay[..i].chars().count() as f64 + 1.0).unwrap_or(0.0))
        }
        "replace" => {
            // Microsoft.VisualBasic.Strings.Replace: a zero-length Find returns the text unchanged
            // (Book Value 01's headings use Replace(x, "", Chr(10)))
            let (s, find, with) = (ev(0)?.as_text(), ev(1)?.as_text(), ev(2)?.as_text());
            V::Text(if find.is_empty() { s } else { s.replace(&find, &with) })
        }
        "isnothing" => V::Bool(ev(0)?.is_null()),
        "choose" => {
            let i = ev(0)?.as_num() as usize;
            if i >= 1 && i < args.len() {
                ev(i)?
            } else {
                V::Null
            }
        }
        "chr" | "chrw" => V::Text(char::from_u32(ev(0)?.as_num() as u32).map(|c| c.to_string()).unwrap_or_default()),
        "asc" | "ascw" => V::Num(ev(0)?.as_text().chars().next().map(|c| c as u32 as f64).unwrap_or(0.0)),
        "format" | "formatnumber" => {
            let v = ev(0)?;
            let f = if args.len() > 1 { ev(1)?.as_text() } else { String::new() };
            V::Text(format_value(&v, &f))
        }
        _ => return Err(format!("unsupported function {name}")),
    })
}

/// .NET format strings as BC's layouts use them on numbers: `N`/`Nn`, `F`/`Fn`, `C`, `P`, `G`, `0`, `#,##0.00`.
/// Other formats (and texts) are returned as text unchanged.
pub fn format_value(v: &V, f: &str) -> String {
    let n = match v {
        V::Num(n) => *n,
        V::Text(s) if is_numeric(v) => s.trim().parse().unwrap_or(0.0),
        _ => return v.as_text(),
    };
    let f = f.trim();
    let group = |s: String| -> String {
        let (int, frac) = s.split_once('.').map(|(a, b)| (a.to_string(), Some(b.to_string()))).unwrap_or((s.clone(), None));
        let neg = int.starts_with('-');
        let digits: Vec<char> = int.trim_start_matches('-').chars().collect();
        let mut g = String::new();
        for (i, d) in digits.iter().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                g.push(',');
            }
            g.push(*d);
        }
        format!("{}{}{}", if neg { "-" } else { "" }, g, frac.map(|f| format!(".{f}")).unwrap_or_default())
    };
    let (kind, prec) = f.split_at(f.chars().next().map(|c| c.len_utf8()).unwrap_or(0));
    let p: Option<usize> = prec.parse().ok();
    match kind.to_ascii_uppercase().as_str() {
        "N" if prec.is_empty() || p.is_some() => group(format!("{:.*}", p.unwrap_or(2), n)),
        "F" if prec.is_empty() || p.is_some() => format!("{:.*}", p.unwrap_or(2), n),
        "G" | "" if prec.is_empty() => fmt_num(n),
        _ if f.contains('0') || f.contains('#') => {
            let decimals = f.split_once('.').map(|(_, d)| d.chars().filter(|c| *c == '0' || *c == '#').count()).unwrap_or(0);
            let s = format!("{:.*}", decimals, n);
            if f.contains(',') {
                group(s)
            } else {
                s
            }
        }
        _ => fmt_num(n),
    }
}
