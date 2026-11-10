//! The layout's `<Code>` block, interpreted (a VB.NET subset; see docs/reports.md).
//!
//! BC's layouts call a few standard functions (SetData/GetData, BlankZero & co, IsNewPage,
//! GetGroupPageNumber), which rdlc_bind implements natively. The rest, about a third of the W1
//! layouts, declare their own small functions: SetRowColor, CalcPct, IsEmpty, HideOnce, GetTextPage
//! and a long tail used by one or two layouts each. This module parses the block into module
//! variables and functions and runs them:
//!
//! - declarations: `[Public|Private|Shared|Friend|Dim|Const|Static] x [As T] [= init]` (also lists);
//! - `Function F[(params)] [As T]` / `Sub S(...)` with `ByVal`/`ByRef`/`Optional p As T = v`;
//! - statements: assignment (`x = e`, `x += e`, `-=`, `&=`, the function's name as its result),
//!   `Return [e]`, `Exit Function/Sub`, `If … Then … ElseIf … Else … End If` and the one-line `If`,
//!   `Select Case … Case a, b / Case Else … End Select`, `For i = a To b [Step s] … Next`,
//!   `On Error Resume Next` (a failing statement is skipped), `Call F(…)` and other calls;
//! - comments (`'`, `REM`), line continuations (` _`) and `:` statement separators.
//!
//! Expressions are rdlc_expr's (the same VB expression syntax); identifiers resolve to locals, then
//! module variables, then module functions, then rdlc_expr's built-ins. Values convert to a
//! variable's or function's declared type on assignment and return (Boolean, the numeric types,
//! String). Calls of BC's standard functions from the code go to rdlc_bind's native ones, so their
//! state (SetData/GetData) is shared.

use crate::rdlc_expr::{self as ex, Ctx, Expr, V};
use std::collections::HashMap;

#[derive(Debug, Clone)]
struct Decl {
    name: String,
    ty: Option<String>,
    init: Option<Expr>,
}

#[derive(Debug, Clone)]
struct Func {
    name: String,
    params: Vec<(Decl, bool)>,
    ret: Option<String>,
    body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
enum Stmt {
    Dim(Vec<Decl>),
    Assign(String, char, Expr),
    Return(Option<Expr>),
    Exit,
    If(Vec<(Expr, Vec<Stmt>)>, Vec<Stmt>),
    Select(Expr, Vec<(Vec<Expr>, Vec<Stmt>)>, Vec<Stmt>),
    For(String, Expr, Expr, Option<Expr>, Vec<Stmt>),
    OnError(bool),
    Eval(Expr),
}

/// A parsed `<Code>` block.
#[derive(Debug, Clone, Default)]
pub struct Module {
    vars: Vec<Decl>,
    funcs: HashMap<String, Func>,
}

/// Names rdlc_bind implements natively (BC's standard layout code).
pub const NATIVE: &[&str] = &["blankzero", "blankpos", "blankzeroandpos", "blankneg", "blanknegandzero", "setdata", "getdata", "setpicture", "getpicture", "isnewpage", "getgrouppagenumber"];

// ------------------------------------------------------------------ lines

/// Logical lines: continuations joined, comments removed, `:` separators split.
fn lines(code: &str) -> Vec<String> {
    let mut joined: Vec<String> = vec![];
    let mut cur = String::new();
    for raw in code.lines() {
        let l = strip_comment(raw);
        let t = l.trim_end();
        if let Some(head) = t.strip_suffix(" _").or_else(|| if t == "_" { Some("") } else { None }) {
            cur.push_str(head);
            cur.push(' ');
            continue;
        }
        cur.push_str(t);
        joined.push(std::mem::take(&mut cur));
    }
    if !cur.is_empty() {
        joined.push(cur);
    }
    let mut out = vec![];
    for l in joined {
        for part in split_top(&l, ':') {
            let p = part.trim();
            if !p.is_empty() {
                out.push(p.to_string());
            }
        }
    }
    out
}

fn strip_comment(l: &str) -> String {
    let mut out = String::new();
    let mut in_str = false;
    for ch in l.chars() {
        if ch == '"' {
            in_str = !in_str;
        }
        if ch == '\'' && !in_str {
            break;
        }
        out.push(ch);
    }
    let t = out.trim_start();
    if t.len() >= 4 && t[..4].eq_ignore_ascii_case("rem ") || t.eq_ignore_ascii_case("rem") {
        return String::new();
    }
    out
}

/// Split at `sep` outside strings and parentheses.
fn split_top(s: &str, sep: char) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut in_str) = (vec![], String::new(), 0i32, false);
    for ch in s.chars() {
        match ch {
            '"' => in_str = !in_str,
            '(' if !in_str => depth += 1,
            ')' if !in_str => depth -= 1,
            c if c == sep && !in_str && depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    out.push(cur);
    out
}

/// The byte offset of keyword `kw` (a whole word, outside strings), from `from`.
fn find_kw(s: &str, kw: &str, from: usize) -> Option<usize> {
    let low = s.to_ascii_lowercase();
    let b = low.as_bytes();
    let mut in_str = false;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            in_str = !in_str;
        }
        if !in_str && i >= from && low[i..].starts_with(kw) {
            let before = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
            let j = i + kw.len();
            let after = j >= b.len() || !(b[j].is_ascii_alphanumeric() || b[j] == b'_');
            if before && after {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn words(l: &str) -> Vec<String> {
    l.split_whitespace().map(|w| w.to_ascii_lowercase()).collect()
}

fn expr(src: &str) -> Result<Expr, String> {
    ex::parse(&format!("={}", src.trim()))
}

const MODIFIERS: &[&str] = &["public", "private", "shared", "friend", "protected", "static", "overloads", "overridable", "readonly", "dim"];

/// `name [As T] [= init]` (also `name() As T`, arrays read as plain values).
fn decl(s: &str) -> Result<Decl, String> {
    let (lhs, init) = match split_top(s, '=').as_slice() {
        [l] => (l.clone(), None),
        [l, rest @ ..] => (l.clone(), Some(expr(&rest.join("="))?)),
        _ => return Err("empty declaration".into()),
    };
    let (name, ty) = match find_kw(&lhs, "as", 0) {
        Some(i) => (lhs[..i].trim().to_string(), Some(lhs[i + 2..].trim().trim_start_matches("New ").trim().to_ascii_lowercase())),
        None => (lhs.trim().to_string(), None),
    };
    let name = name.trim_end_matches("()").trim().to_ascii_lowercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("unsupported declaration {s}"));
    }
    Ok(Decl { name, ty, init })
}

// ------------------------------------------------------------------ parse

impl Module {
    pub fn parse(code: &str) -> Result<Module, String> {
        let ls = lines(code);
        let mut m = Module::default();
        let mut i = 0;
        while i < ls.len() {
            let l = &ls[i];
            let w = words(l);
            let mut k = 0;
            while k < w.len() && MODIFIERS.contains(&w[k].as_str()) {
                k += 1;
            }
            match w.get(k).map(String::as_str) {
                Some("function") | Some("sub") => {
                    let (f, next) = parse_func(&ls, i, &w[k])?;
                    m.funcs.insert(f.name.clone(), f);
                    i = next;
                    continue;
                }
                Some("imports") | Some("option") => {}
                Some("const") => {
                    let rest = l[find_kw(l, "const", 0).unwrap() + 5..].to_string();
                    for d in split_top(&rest, ',') {
                        m.vars.push(decl(&d)?);
                    }
                }
                Some(_) if k > 0 => {
                    // [modifiers] a [As T] [= v], b As T
                    let rest = l.split_whitespace().skip(k).collect::<Vec<_>>().join(" ");
                    for d in split_top(&rest, ',') {
                        m.vars.push(decl(&d)?);
                    }
                }
                _ => return Err(format!("unsupported line in <Code>: {l}")),
            }
            i += 1;
        }
        Ok(m)
    }

    pub fn has(&self, name: &str) -> bool {
        self.funcs.contains_key(name)
    }
}

fn parse_func(ls: &[String], at: usize, kind: &str) -> Result<(Func, usize), String> {
    let l = &ls[at];
    let s = find_kw(l, kind, 0).unwrap() + kind.len();
    let head = l[s..].trim();
    let (name, params, ret) = match head.find('(') {
        Some(p) => {
            // the parameter list's closing parenthesis (defaults may contain parentheses)
            let mut depth = 0;
            let mut close = None;
            for (j, ch) in head.char_indices().skip(p) {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let close = close.ok_or("unbalanced parameter list")?;
            let ret = head[close + 1..].trim();
            (head[..p].trim().to_string(), head[p + 1..close].to_string(), ret.to_string())
        }
        None => match find_kw(head, "as", 0) {
            Some(i) => (head[..i].trim().to_string(), String::new(), head[i..].to_string()),
            None => (head.to_string(), String::new(), String::new()),
        },
    };
    let ret = ret.trim();
    let ret = ret.strip_prefix("As ").or_else(|| ret.strip_prefix("as ")).or_else(|| ret.strip_prefix("AS ")).map(|t| t.trim().to_ascii_lowercase());
    let mut ps = vec![];
    for p in split_top(&params, ',') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        let pw: Vec<&str> = p.split_whitespace().collect();
        let mut k = 0;
        let mut optional = false;
        while k < pw.len() && ["byval", "byref", "optional", "paramarray"].contains(&pw[k].to_ascii_lowercase().as_str()) {
            optional |= pw[k].eq_ignore_ascii_case("optional");
            k += 1;
        }
        ps.push((decl(&pw[k..].join(" "))?, optional));
    }
    let end = if kind == "sub" { "end sub" } else { "end function" };
    let (body, next) = block(ls, at + 1, &[end])?;
    if next >= ls.len() {
        return Err(format!("{kind} {name}: no {end}"));
    }
    Ok((Func { name: name.to_ascii_lowercase(), params: ps, ret, body }, next + 1))
}

fn starts(l: &str, prefixes: &[&str]) -> bool {
    let low = l.to_ascii_lowercase();
    prefixes.iter().any(|p| low == *p || low.starts_with(&format!("{p} ")) || low.starts_with(&format!("{p}(")))
}

/// Statements up to (not including) a line starting with one of `ends`.
fn block(ls: &[String], mut i: usize, ends: &[&str]) -> Result<(Vec<Stmt>, usize), String> {
    let mut out = vec![];
    while i < ls.len() {
        let l = ls[i].as_str();
        if starts(l, ends) {
            return Ok((out, i));
        }
        let low = l.to_ascii_lowercase();
        let w = words(l);
        if w.first().map(String::as_str) == Some("if") {
            // `Then` is optional on VB.NET's multi-line If (BC's own IsNewPage omits it)
            let t = find_kw(l, "then", 0);
            let cond = expr(&l[2..t.unwrap_or(l.len())])?;
            let rest = t.map(|t| l[t + 4..].trim()).unwrap_or("");
            if !rest.is_empty() {
                // one-line If c Then s [Else s]
                let (a, b) = match find_kw(rest, "else", 0) {
                    Some(e) => (&rest[..e], Some(&rest[e + 4..])),
                    None => (rest, None),
                };
                let one = |s: &str| -> Result<Vec<Stmt>, String> { block(&[s.trim().to_string()], 0, &[]).map(|x| x.0) };
                out.push(Stmt::If(vec![(cond, one(a)?)], match b {
                    Some(b) => one(b)?,
                    None => vec![],
                }));
                i += 1;
                continue;
            }
            let mut arms = vec![];
            let mut els = vec![];
            let mut c = cond;
            let mut j = i + 1;
            loop {
                let (body, at) = block(ls, j, &["elseif", "else", "end if"])?;
                arms.push((c.clone(), body));
                let tl = ls.get(at).ok_or("If without End If")?.to_ascii_lowercase();
                if tl.starts_with("elseif") {
                    let t = find_kw(&ls[at], "then", 0).unwrap_or(ls[at].len());
                    c = expr(&ls[at][6..t])?;
                    j = at + 1;
                } else if tl.starts_with("else") {
                    let (body, at2) = block(ls, at + 1, &["end if"])?;
                    els = body;
                    j = at2 + 1;
                    break;
                } else {
                    j = at + 1;
                    break;
                }
            }
            out.push(Stmt::If(arms, els));
            i = j;
            continue;
        }
        if low.starts_with("select case ") {
            let e = expr(&l[12..])?;
            let mut cases = vec![];
            let mut els = vec![];
            let mut j = i + 1;
            while j < ls.len() && !starts(&ls[j], &["case", "end select"]) {
                j += 1;
            }
            while j < ls.len() && starts(&ls[j], &["case"]) {
                let head = ls[j][4..].trim().to_string();
                let (body, at) = block(ls, j + 1, &["case", "end select"])?;
                if head.eq_ignore_ascii_case("else") {
                    els = body;
                } else {
                    cases.push((split_top(&head, ',').iter().map(|v| expr(v)).collect::<Result<Vec<_>, _>>()?, body));
                }
                j = at;
            }
            out.push(Stmt::Select(e, cases, els));
            i = j + 1;
            continue;
        }
        if low.starts_with("for ") && !low.starts_with("for each") {
            let eq = l.find('=').ok_or("For without =")?;
            let to = find_kw(l, "to", eq).ok_or("For without To")?;
            let step = find_kw(l, "step", to);
            let var = l[3..eq].trim().split_whitespace().next().unwrap_or_default().to_ascii_lowercase();
            let a = expr(&l[eq + 1..to])?;
            let b = expr(&l[to + 2..step.unwrap_or(l.len())])?;
            let s = match step {
                Some(st) => Some(expr(&l[st + 4..])?),
                None => None,
            };
            let (body, at) = block(ls, i + 1, &["next"])?;
            out.push(Stmt::For(var, a, b, s, body));
            i = at + 1;
            continue;
        }
        out.push(simple(l)?);
        i += 1;
    }
    if ends.is_empty() {
        Ok((out, i))
    } else {
        Err(format!("missing {}", ends[ends.len() - 1]))
    }
}

fn simple(l: &str) -> Result<Stmt, String> {
    let low = l.to_ascii_lowercase();
    let w = words(l);
    match w.first().map(String::as_str) {
        Some("return") => return Ok(Stmt::Return(if w.len() > 1 { Some(expr(&l[6..])?) } else { None })),
        Some("exit") => return Ok(Stmt::Exit),
        Some("dim") | Some("static") | Some("const") => {
            let rest = l.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
            return Ok(Stmt::Dim(split_top(&rest, ',').iter().map(|d| decl(d)).collect::<Result<_, _>>()?));
        }
        Some("call") => return Ok(Stmt::Eval(expr(&l[4..])?)),
        _ => {}
    }
    if low.starts_with("on error resume next") {
        return Ok(Stmt::OnError(true));
    }
    if low.starts_with("on error goto 0") {
        return Ok(Stmt::OnError(false));
    }
    // assignment: a name (Me.x) then = / += / -= / &=
    let name_end = l.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.')).unwrap_or(l.len());
    let name = l[..name_end].to_ascii_lowercase();
    let rest = l[name_end..].trim_start();
    if !name.is_empty() {
        for (op, c) in [("+=", '+'), ("-=", '-'), ("&=", '&'), ("=", '=')] {
            if let Some(r) = rest.strip_prefix(op) {
                let name = name.strip_prefix("me.").unwrap_or(&name).to_string();
                return Ok(Stmt::Assign(name, c, expr(r)?));
            }
        }
    }
    Ok(Stmt::Eval(expr(l)?))
}

// ------------------------------------------------------------------ run

fn default_of(ty: &Option<String>) -> V {
    match ty.as_deref() {
        Some("boolean") => V::Bool(false),
        Some("integer" | "int32" | "long" | "int64" | "short" | "byte" | "decimal" | "double" | "single") => V::Num(0.0),
        _ => V::Null,
    }
}

fn convert(v: V, ty: &Option<String>) -> V {
    match ty.as_deref() {
        Some("boolean") => V::Bool(v.as_bool()),
        Some("integer" | "int32" | "long" | "int64" | "short" | "byte") => {
            // CInt: to even
            let x = v.as_num();
            let r = x.round();
            V::Num(if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 { r - x.signum() } else { r })
        }
        Some("decimal" | "double" | "single") => V::Num(v.as_num()),
        Some("string") if !v.is_null() => V::Text(v.as_text()),
        _ => v,
    }
}

/// The module's variables, initialised on first use.
#[derive(Debug, Default)]
pub struct VbState {
    module: Module,
    globals: HashMap<String, V>,
    inited: bool,
}

enum Flow {
    Next,
    Return,
}

struct Frame<'a> {
    module: &'a Module,
    globals: &'a mut HashMap<String, V>,
    locals: HashMap<String, V>,
    outer: &'a mut dyn Ctx,
    depth: usize,
}

impl VbState {
    pub fn new(code: &str) -> Result<VbState, String> {
        Ok(VbState { module: Module::parse(code)?, ..Default::default() })
    }

    pub fn has(&self, name: &str) -> bool {
        self.module.has(name)
    }

    /// Call function `name` (lower case) with `args`; `outer` is the expression scope (BC's standard
    /// functions, built-ins).
    pub fn call(&mut self, name: &str, args: Vec<V>, outer: &mut dyn Ctx) -> Result<V, String> {
        if !self.inited {
            self.inited = true;
            let vars = self.module.vars.clone();
            for d in &vars {
                let v = match &d.init {
                    Some(e) => {
                        let mut f = Frame { module: &self.module, globals: &mut self.globals, locals: HashMap::new(), outer: &mut *outer, depth: 0 };
                        convert(ex::eval(e, &mut f)?, &d.ty)
                    }
                    None => default_of(&d.ty),
                };
                self.globals.insert(d.name.clone(), v);
            }
        }
        let mut f = Frame { module: &self.module, globals: &mut self.globals, locals: HashMap::new(), outer, depth: 0 };
        f.invoke(name, args)
    }
}

impl Frame<'_> {
    fn invoke(&mut self, name: &str, args: Vec<V>) -> Result<V, String> {
        if self.depth > 64 {
            return Err(format!("{name}: call depth"));
        }
        let module = self.module;
        let func = module.funcs.get(name).ok_or_else(|| format!("Code.{name} is not defined in the layout"))?;
        let mut locals = HashMap::new();
        for (i, (p, _)) in func.params.iter().enumerate() {
            let v = match args.get(i) {
                Some(v) => convert(v.clone(), &p.ty),
                None => match &p.init {
                    Some(e) => ex::eval(e, self)?,
                    None => default_of(&p.ty),
                },
            };
            locals.insert(p.name.clone(), v);
        }
        locals.insert(func.name.clone(), default_of(&func.ret));
        let mut sub = Frame { module, globals: &mut *self.globals, locals, outer: &mut *self.outer, depth: self.depth + 1 };
        let mut resume = false;
        sub.run(&func.body, &mut resume)?;
        let r = sub.locals.remove(&func.name).unwrap_or(V::Null);
        Ok(convert(r, &func.ret))
    }

    fn run(&mut self, body: &[Stmt], resume: &mut bool) -> Result<Flow, String> {
        for s in body {
            match self.step(s, resume) {
                Ok(Flow::Next) => {}
                Ok(Flow::Return) => return Ok(Flow::Return),
                Err(_) if *resume => {}
                Err(e) => return Err(e),
            }
        }
        Ok(Flow::Next)
    }

    fn set(&mut self, name: &str, v: V) {
        if self.locals.contains_key(name) {
            self.locals.insert(name.to_string(), v);
        } else if self.globals.contains_key(name) {
            self.globals.insert(name.to_string(), v);
        } else {
            // VB without Option Explicit: an undeclared name is a local
            self.locals.insert(name.to_string(), v);
        }
    }

    fn get(&self, name: &str) -> Option<V> {
        self.locals.get(name).or_else(|| self.globals.get(name)).cloned()
    }

    fn step(&mut self, s: &Stmt, resume: &mut bool) -> Result<Flow, String> {
        match s {
            Stmt::Dim(ds) => {
                for d in ds {
                    let v = match &d.init {
                        Some(e) => convert(ex::eval(e, self)?, &d.ty),
                        None => default_of(&d.ty),
                    };
                    self.locals.insert(d.name.clone(), v);
                }
            }
            Stmt::Assign(name, op, e) => {
                let v = ex::eval(e, self)?;
                let old = self.get(name).unwrap_or(V::Null);
                let v = match op {
                    '+' => V::Num(old.as_num() + v.as_num()),
                    '-' => V::Num(old.as_num() - v.as_num()),
                    '&' => V::Text(old.as_text() + &v.as_text()),
                    _ => v,
                };
                let ty = self.module.vars.iter().find(|d| &d.name == name).map(|d| d.ty.clone()).unwrap_or(None);
                self.set(name, convert(v, &ty));
            }
            Stmt::Return(e) => {
                if let Some(e) = e {
                    let v = ex::eval(e, self)?;
                    // the function's own name holds its result
                    if let Some(fname) = self.locals.keys().find(|k| self.module.funcs.contains_key(*k)).cloned() {
                        self.locals.insert(fname, v);
                    }
                }
                return Ok(Flow::Return);
            }
            Stmt::Exit => return Ok(Flow::Return),
            Stmt::If(arms, els) => {
                for (c, body) in arms {
                    if ex::eval(c, self)?.as_bool() {
                        return self.run(body, resume);
                    }
                }
                return self.run(els, resume);
            }
            Stmt::Select(e, cases, els) => {
                let v = ex::eval(e, self)?;
                for (vals, body) in cases {
                    for c in vals {
                        let cv = ex::eval(c, self)?;
                        let eq = ex::eval(&Expr::Bin(ex::Op::Eq, Box::new(Expr::Lit(v.clone())), Box::new(Expr::Lit(cv))), self)?;
                        if eq.as_bool() {
                            return self.run(body, resume);
                        }
                    }
                }
                return self.run(els, resume);
            }
            Stmt::For(var, a, b, st, body) => {
                let (mut i, to) = (ex::eval(a, self)?.as_num(), ex::eval(b, self)?.as_num());
                let step = match st {
                    Some(s) => ex::eval(s, self)?.as_num(),
                    None => 1.0,
                };
                let mut n = 0;
                while (step >= 0.0 && i <= to) || (step < 0.0 && i >= to) {
                    self.set(var, V::Num(i));
                    if let Flow::Return = self.run(body, resume)? {
                        return Ok(Flow::Return);
                    }
                    i = self.get(var).map(|v| v.as_num()).unwrap_or(i) + step;
                    n += 1;
                    if n > 100_000 {
                        return Err("For: too many iterations".into());
                    }
                }
            }
            Stmt::OnError(on) => *resume = *on,
            Stmt::Eval(e) => {
                ex::eval(e, self)?;
            }
        }
        Ok(Flow::Next)
    }
}

impl Ctx for Frame<'_> {
    fn field(&mut self, name: &str) -> Option<V> {
        self.outer.field(name)
    }
    fn param(&mut self, name: &str) -> V {
        self.outer.param(name)
    }
    fn report_item(&mut self, name: &str) -> V {
        self.outer.report_item(name)
    }
    fn global(&mut self, name: &str) -> V {
        self.outer.global(name)
    }
    fn aggregate(&mut self, func: &str, _arg: Option<&Expr>, _scope: Option<&str>) -> Result<V, String> {
        Err(format!("{func} in <Code>"))
    }
    fn code(&mut self, name: &str, args: Vec<V>) -> Result<V, String> {
        // Code.X / Me.X from inside the code
        let l = name.to_ascii_lowercase();
        self.ident(&l, args)
    }
    fn has_ident(&self, name: &str) -> bool {
        let n = name.strip_prefix("me.").unwrap_or(name);
        self.locals.contains_key(n) || self.globals.contains_key(n) || self.module.funcs.contains_key(n) || NATIVE.contains(&n)
    }
    fn ident(&mut self, name: &str, args: Vec<V>) -> Result<V, String> {
        let n = name.strip_prefix("me.").unwrap_or(name);
        if NATIVE.contains(&n) {
            return self.outer.code(n, args);
        }
        if self.module.funcs.contains_key(n) {
            // a function's own name inside it is its result variable, unless called with arguments
            if args.is_empty() && self.locals.contains_key(n) {
                return Ok(self.locals[n].clone());
            }
            return self.invoke(n, args);
        }
        if let Some(v) = self.get(n) {
            return Ok(v);
        }
        Err(format!("unknown identifier {n}"))
    }
}
