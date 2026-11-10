//! Compiled execution (P12): the default engine; `BC_ENGINE=tree` runs the tree-walker instead.
//!
//! A procedure body is compiled once (per thread) into a tree of closures:
//! - statements: Block, If and assignment to a local or global variable; every other statement
//!   kind runs through the tree-walker (`Session::exec`) unchanged;
//! - expressions: literals (built once), local/global variable reads by index, and/or (both
//!   operands evaluated, left to right, as in AL and the tree-walker: no short-circuit), the other
//!   binary operators through `Session::binop`, the ternary. Everything else is evaluated by the
//!   tree-walker (`Session::eval`) for that node.
//!
//! Semantics stay the tree-walker's:
//! - every statement counts one step, with the same step-limit check;
//! - assignment goes through `lvalue` → `read_lval`/`binop` → `write_lval`, with the same line on errors;
//! - comparisons that the interpreter treats as Code comparisons (`expr_is_code`) are left to it.
//!
//! Variables are resolved against the first frame the procedure runs in. Later frames of the same
//! procedure have the same layout (parameters, locals, return value; the object's globals). Every
//! access still checks the name at the index and falls back to the tree-walker otherwise.
//! Only bodies of catalog procedures are compiled: the catalog lives for the whole run, so the
//! AST references (&'static) stay valid.

use crate::interp::*;
use crate::value::*;
use al_model::{Catalog, ProcRef};
use al_syntax::{AssignOp, BinOp, Expr, ObjectKind, ProcKind, Stmt, StmtKind, TypeRef, UnOp};
use rust_decimal::Decimal;
use std::cell::RefCell;
use std::collections::HashMap;
use std::str::FromStr;

type CExpr = Box<dyn for<'a> Fn(&mut Session<'a>, &mut Frame) -> R<Val>>;

#[derive(Clone, Copy)]
enum VarRef {
    Local(u32),
    Global(u32),
}

enum CStmt {
    /// Run by the tree-walker (counts its own step).
    Interp(&'static Stmt),
    Block(Vec<CStmt>),
    /// Control statements carry their line: the frame's current line while their expressions run
    /// (Session::cur_line, SessionInformation.Callstack).
    If(u32, CExpr, Box<CStmt>, Option<Box<CStmt>>),
    Assign { stmt: &'static Stmt, name: &'static str, var: VarRef, op: AssignOp, value: CExpr },
    /// An expression statement (a call), with exec's discard flag and error line.
    Expr { stmt: &'static Stmt, discard: bool, value: CExpr },
    Case { line: u32, subject: CExpr, branches: Vec<(&'static [Expr], CStmt)>, els: Option<Vec<CStmt>> },
    While(u32, CExpr, Box<CStmt>),
    Repeat(u32, Vec<CStmt>, CExpr),
    For { line: u32, var: &'static Expr, from: CExpr, to: CExpr, down: bool, body: Box<CStmt> },
    Exit(u32, Option<CExpr>),
    Break,
    Continue,
    /// `RecVar.Field := v` (var = Some) or `Field := v` on the implicit Rec (var = None).
    FieldAssign { stmt: &'static Stmt, target: &'static Expr, var: Option<(VarRef, &'static str)>, tid: i64, fi: usize, op: AssignOp, value: CExpr },
}

/// BC_ENGINE=compiled, or BC_ENGINE=lookup (diagnostic: find/compile the body, then run the
/// tree-walker, to measure the dispatch overhead alone).
fn mode() -> u8 {
    static M: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *M.get_or_init(|| match std::env::var("BC_ENGINE").unwrap_or_default().to_ascii_lowercase().as_str() {
        // the default since round 12; BC_ENGINE=tree is the escape hatch
        "tree" => 0,
        "lookup" => 2,
        _ => 1,
    })
}

pub fn enabled() -> bool {
    mode() != 0
}

#[derive(Default)]
struct KeyHasher(u64);
impl std::hash::Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(b as u64);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    fn write_u32(&mut self, n: u32) {
        self.write_u64(n as u64)
    }
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64)
    }
}

/// A compiled body and the frame layout it was compiled against.
struct Prog {
    stmts: Box<[CStmt]>,
    /// Every variable the body resolved: where, and the address of its declaration's name
    /// (`Var.name` is a &'static str from the catalog, so the address identifies the declaration).
    layout: Box<[(VarRef, usize)]>,
    /// The frame's shape: number of locals and globals, address of the first global's name.
    shape: (usize, usize, usize),
}

/// The shape of a frame's variables (see Prog::shape).
#[inline]
fn frame_shape(f: &Frame) -> (usize, usize, usize) {
    (f.locals.len(), f.globals.vars.len(), f.globals.vars.first().map_or(0, |v| v.name.as_ptr() as usize))
}

/// What eval_scope reads from the frame besides its variables: the implicit Rec's table, xRec presence.
#[inline]
fn rec_shape(f: &Frame) -> (Option<i64>, bool) {
    (f.rec.as_ref().map(|r| r.borrow().tdef.id), f.xrec.is_some())
}

thread_local! {
    // compiled bodies are kept for the whole run (leaked), like the catalog they come from
    static PROCS: RefCell<HashMap<(usize, u32, u32), &'static Prog, std::hash::BuildHasherDefault<KeyHasher>>> = RefCell::new(HashMap::default());
    // the variables resolved while one body compiles (compile_* never runs AL, so no nesting)
    static LAYOUT: RefCell<Vec<(VarRef, usize)>> = const { RefCell::new(Vec::new()) };
}

fn resolve(f: &Frame, name: &str) -> Option<VarRef> {
    let r = if let Some(i) = f.locals.iter().rposition(|v| v.name.eq_ignore_ascii_case(name)) {
        VarRef::Local(i as u32)
    } else {
        VarRef::Global(f.globals.vars.iter().position(|v| v.name.eq_ignore_ascii_case(name))? as u32)
    };
    if let Some(v) = var_slot(f, r) {
        LAYOUT.with(|l| l.borrow_mut().push((r, v.name.as_ptr() as usize)));
    }
    Some(r)
}

#[inline]
fn var_slot(f: &Frame, r: VarRef) -> Option<&Var> {
    match r {
        VarRef::Local(i) => f.locals.get(i as usize),
        VarRef::Global(i) => f.globals.vars.get(i as usize),
    }
}

/// The variable at `r`. exec_body_compiled checked the frame's layout on entry, so no name check
/// here (None only if the index is out of range: the tree-walker then resolves it).
#[inline]
fn var_at<'f>(f: &'f Frame, r: VarRef, _name: &str) -> Option<&'f Var> {
    var_slot(f, r)
}

fn interp_expr(e: &'static Expr) -> CExpr {
    Box::new(move |s, f| s.eval(f, e))
}

fn compile_expr(cat: &'static Catalog, f: &Frame, e: &'static Expr) -> CExpr {
    match e {
        Expr::Int(i) => {
            let v = if *i > i32::MAX as i64 { Val::BigInt(*i) } else { Val::Int(*i as i32) };
            Box::new(move |_, _| Ok(v.clone()))
        }
        Expr::BigInt(i) => {
            let i = *i;
            Box::new(move |_, _| Ok(Val::BigInt(i)))
        }
        Expr::Bool(b) => {
            let b = *b;
            Box::new(move |_, _| Ok(Val::Bool(b)))
        }
        Expr::Dec(s) => {
            let d = Decimal::from_str(s).unwrap_or_default();
            Box::new(move |_, _| Ok(Val::Dec(d)))
        }
        Expr::Str(s) => {
            let s: &'static str = s;
            Box::new(move |_, _| Ok(Val::Text(s.to_string())))
        }
        // literals: the tree-walker parses them on every evaluation
        Expr::Date(s) => {
            let d = parse_date_literal(s);
            Box::new(move |_, _| Ok(Val::Date(d)))
        }
        Expr::Time(s) => {
            let t = parse_time_literal(s);
            Box::new(move |_, _| Ok(Val::Time(t)))
        }
        Expr::DateTime(_) => Box::new(|_, _| Ok(Val::DateTime(bc_store::DATETIME_UNDEFINED))),
        // the tree-walker's Unary arm
        Expr::Unary(op, x) => {
            let cx = compile_expr(cat, f, x);
            match op {
                UnOp::Not => Box::new(move |s, f| Ok(Val::Bool(!cx(s, f)?.as_bool()))),
                UnOp::Plus => cx,
                UnOp::Neg => Box::new(move |s, f| {
                    Ok(match cx(s, f)? {
                        Val::Int(i) => Val::Int(-i),
                        Val::BigInt(i) => Val::BigInt(-i),
                        Val::Duration(i) => Val::Duration(-i),
                        o => Val::Dec(-o.as_dec()),
                    })
                }),
            }
        }
        // `X::Y`: evaluated once by the tree-walker, then reused while the implicit Rec's table and
        // xRec presence match (the frame layout was checked on entry)
        Expr::Scope(..) => {
            let key = rec_shape(f);
            let cache: std::cell::OnceCell<Val> = std::cell::OnceCell::new();
            Box::new(move |s, f| {
                if rec_shape(f) != key {
                    return s.eval(f, e);
                }
                if let Some(v) = cache.get() {
                    return Ok(v.clone());
                }
                let v = s.eval(f, e)?;
                if matches!(v, Val::Int(_) | Val::BigInt(_) | Val::Opt { .. } | Val::Bool(_)) {
                    let _ = cache.set(v.clone());
                }
                Ok(v)
            })
        }
        Expr::Ident(n) => match resolve(f, n) {
            Some(r) => {
                let n: &'static str = n;
                Box::new(move |s, f| match var_at(f, r, n) {
                    Some(v) => Ok(v.slot.borrow().clone()),
                    None => s.eval(f, e),
                })
            }
            // a field of the implicit Rec (eval_ident: variables, then `with` records, then Rec)
            None => match f.rec.as_ref().map(|r| (r.borrow().tdef.id, r.borrow().tdef.field(n))) {
                Some((tid, Some(fi))) => Box::new(move |s, f| {
                    if f.with.is_empty() {
                        if let Some(r) = &f.rec {
                            let b = r.borrow();
                            if b.tdef.id == tid {
                                return Ok(b.get_val(fi));
                            }
                        }
                    }
                    s.eval(f, e)
                }),
                _ => interp_expr(e),
            },
        },
        Expr::Member(base, name) => compile_member(cat, f, e, base, name, None),
        Expr::Call(callee, args) => match &**callee {
            Expr::Ident(n) => compile_call(cat, f, e, n, args),
            Expr::Member(base, name) => compile_member(cat, f, e, base, name, Some(args)),
            _ => interp_expr(e),
        },
        Expr::Binary(op, l, r) => {
            let (cl, cr) = (compile_expr(cat, f, l), compile_expr(cat, f, r));
            match op {
                // AL evaluates both operands of `and` / `or`, left then right (no short-circuit),
                // as the tree-walker does: code relies on the right side's side effects
                BinOp::And => Box::new(move |s, f| {
                    let a = cl(s, f)?.as_bool();
                    let b = cr(s, f)?.as_bool();
                    Ok(Val::Bool(a & b))
                }),
                BinOp::Or => Box::new(move |s, f| {
                    let a = cl(s, f)?.as_bool();
                    let b = cr(s, f)?.as_bool();
                    Ok(Val::Bool(a | b))
                }),
                BinOp::In => interp_expr(e),
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let (lx, rx): (&'static Expr, &'static Expr) = (l, r);
                    let op = *op;
                    Box::new(move |s, f| {
                        // Code operands compare right-aligned: the tree-walker's rule
                        if s.expr_is_code(f, lx) || s.expr_is_code(f, rx) {
                            return s.eval(f, e);
                        }
                        let a = cl(s, f)?;
                        let b = cr(s, f)?;
                        s.binop(op, a, b)
                    })
                }
                _ => {
                    let op = *op;
                    Box::new(move |s, f| {
                        let a = cl(s, f)?;
                        let b = cr(s, f)?;
                        s.binop(op, a, b)
                    })
                }
            }
        }
        Expr::Ternary(c, a, b) => {
            let (cc, ca, cb) = (compile_expr(cat, f, c), compile_expr(cat, f, a), compile_expr(cat, f, b));
            Box::new(move |s, f| if cc(s, f)?.as_bool() { ca(s, f) } else { cb(s, f) })
        }
        _ => interp_expr(e),
    }
}

fn compile_stmt(cat: &'static Catalog, f: &Frame, st: &'static Stmt) -> CStmt {
    match &st.kind {
        StmtKind::Block(b) => CStmt::Block(b.iter().map(|x| compile_stmt(cat, f, x)).collect()),
        StmtKind::If { cond, then, els } => CStmt::If(st.line, compile_expr(cat, f, cond), Box::new(compile_stmt(cat, f, then)), els.as_ref().map(|e| Box::new(compile_stmt(cat, f, e)))),
        StmtKind::Case { subject, branches, els } => CStmt::Case {
            line: st.line,
            subject: compile_expr(cat, f, subject),
            branches: branches.iter().map(|b| (b.values.as_slice(), compile_stmt(cat, f, &b.body))).collect(),
            els: els.as_ref().map(|e| e.iter().map(|x| compile_stmt(cat, f, x)).collect()),
        },
        StmtKind::While { cond, body } => CStmt::While(st.line, compile_expr(cat, f, cond), Box::new(compile_stmt(cat, f, body))),
        StmtKind::Repeat { body, until } => CStmt::Repeat(st.line, body.iter().map(|x| compile_stmt(cat, f, x)).collect(), compile_expr(cat, f, until)),
        StmtKind::For { var, from, to, down, body } => {
            CStmt::For { line: st.line, var, from: compile_expr(cat, f, from), to: compile_expr(cat, f, to), down: *down, body: Box::new(compile_stmt(cat, f, body)) }
        }
        StmtKind::Exit(v) => CStmt::Exit(st.line, v.as_ref().map(|e| compile_expr(cat, f, e))),
        StmtKind::Break => CStmt::Break,
        StmtKind::Continue => CStmt::Continue,
        StmtKind::Expr(e) => CStmt::Expr { stmt: st, discard: matches!(e, Expr::Call(..) | Expr::Member(..) | Expr::Ident(_)), value: compile_expr(cat, f, e) },
        // the tree-walker's special cases (Mark/MarkedOnly on Rec, property-call assignments)
        // only apply when the name is not a variable
        StmtKind::Assign { target: target @ Expr::Ident(n), op, value } => match resolve(f, n) {
            Some(var) => CStmt::Assign { stmt: st, name: n, var, op: *op, value: compile_expr(cat, f, value) },
            // a field of the implicit Rec (exec's Mark/MarkedOnly case only applies to non-fields)
            None if !special_assign_name(n) => match f.rec.as_ref().map(|r| (r.borrow().tdef.id, r.borrow().tdef.field(n))) {
                Some((tid, Some(fi))) => CStmt::FieldAssign { stmt: st, target, var: None, tid, fi, op: *op, value: compile_expr(cat, f, value) },
                _ => CStmt::Interp(st),
            },
            None => CStmt::Interp(st),
        },
        StmtKind::Assign { target: target @ Expr::Member(base, name), op, value } if !special_assign_name(name) => {
            let Expr::Ident(bn) = &**base else { return CStmt::Interp(st) };
            let Some(r) = resolve(f, bn) else { return CStmt::Interp(st) };
            let Some(tid) = record_var_table(cat, f, r) else { return CStmt::Interp(st) };
            match cat.tables.get(&tid).and_then(|t| t.field(name)) {
                // try_assign_call is false for a Record field, so exec goes straight to lvalue
                Some(fi) => CStmt::FieldAssign { stmt: st, target, var: Some((r, bn)), tid, fi, op: *op, value: compile_expr(cat, f, value) },
                None => CStmt::Interp(st),
            }
        }
        _ => CStmt::Interp(st),
    }
}

impl<'r> Session<'r> {
    #[inline]
    fn count_step(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > self.step_limit {
            // the step limit, or a background session's checkpoint (crate::workers)
            return self.step_limit_reached();
        }
        Ok(())
    }

    /// Run a catalog procedure's body (BC_ENGINE=compiled).
    pub fn exec_body_compiled(&mut self, f: &mut Frame, pr: ProcRef, body: &'static [Stmt]) -> R<()> {
        let key = (self.cat as *const al_model::Catalog as usize, pr.code, pr.proc);
        let prog: &'static Prog = match PROCS.with(|m| m.borrow().get(&key).copied()) {
            Some(p) => p,
            None => {
                let cat = self.cat;
                LAYOUT.with(|l| l.borrow_mut().clear());
                let stmts = body.iter().map(|s| compile_stmt(cat, f, s)).collect::<Vec<_>>().into_boxed_slice();
                let mut layout = LAYOUT.with(|l| std::mem::take(&mut *l.borrow_mut()));
                layout.sort_by_key(|(r, p)| (matches!(r, VarRef::Global(_)), match r {
                    VarRef::Local(i) | VarRef::Global(i) => *i,
                }, *p));
                layout.dedup_by(|a, b| a.1 == b.1 && std::mem::discriminant(&a.0) == std::mem::discriminant(&b.0) && var_index(a.0) == var_index(b.0));
                let p: &'static Prog = Box::leak(Box::new(Prog { stmts, layout: layout.into_boxed_slice(), shape: frame_shape(f) }));
                PROCS.with(|m| m.borrow_mut().insert(key, p));
                p
            }
        };
        if mode() == 2 {
            return self.exec_block(f, body);
        }
        // the frame must have the layout the body was compiled against; else the tree-walker runs it
        // the shape implies the layout (locals come from the declaration, globals from one object)
        debug_assert!(frame_shape(f) != prog.shape || prog.layout.iter().all(|&(r, name)| var_slot(f, r).is_some_and(|v| v.name.as_ptr() as usize == name)));
        if frame_shape(f) != prog.shape {
            return self.exec_block(f, body);
        }
        for s in prog.stmts.iter() {
            self.run_cstmt(f, s)?;
        }
        Ok(())
    }

    fn run_cstmt(&mut self, f: &mut Frame, s: &CStmt) -> R<()> {
        match s {
            CStmt::Interp(st) => self.exec(f, st),
            CStmt::Block(b) => {
                self.count_step()?;
                for x in b {
                    self.run_cstmt(f, x)?;
                }
                Ok(())
            }
            CStmt::If(line, c, then, els) => {
                self.count_step()?;
                self.cur_line = *line;
                if c(self, f)?.as_bool() {
                    self.run_cstmt(f, then)
                } else if let Some(e) = els {
                    self.run_cstmt(f, e)
                } else {
                    Ok(())
                }
            }
            CStmt::Case { line, subject, branches, els } => {
                self.count_step()?;
                self.cur_line = *line;
                let v = subject(self, f)?;
                for (values, body) in branches {
                    for cv in values.iter() {
                        if self.case_match(f, &v, cv)? {
                            return self.run_cstmt(f, body);
                        }
                    }
                }
                if let Some(e) = els {
                    for x in e {
                        self.run_cstmt(f, x)?;
                    }
                }
                Ok(())
            }
            CStmt::While(line, cond, body) => {
                self.count_step()?;
                while {
                    self.cur_line = *line;
                    cond(self, f)?.as_bool()
                } {
                    match self.run_cstmt(f, body) {
                        Err(Flow::Break) => break,
                        Err(Flow::Continue) => continue,
                        r => r?,
                    }
                }
                Ok(())
            }
            CStmt::Repeat(line, body, until) => {
                self.count_step()?;
                'outer: loop {
                    for x in body {
                        match self.run_cstmt(f, x) {
                            Err(Flow::Break) => break 'outer,
                            Err(Flow::Continue) => break,
                            r => r?,
                        }
                    }
                    self.cur_line = *line;
                    if until(self, f)?.as_bool() {
                        break;
                    }
                }
                Ok(())
            }
            CStmt::For { line, var, from, to, down, body } => {
                self.count_step()?;
                self.cur_line = *line;
                let a = from(self, f)?;
                let b = to(self, f)?;
                let lv = self.lvalue(f, var)?;
                let date = matches!(a, Val::Date(_));
                let num = |v: &Val| match v {
                    Val::Date(d) => *d as i64,
                    v => v.as_i64(),
                };
                let (lo, hi) = (num(&a), num(&b));
                let mk = |i: i64| match &a {
                    Val::Date(_) => Val::Date(i as i32),
                    Val::Opt { enum_id, .. } => Val::Opt { ord: i as i32, enum_id: *enum_id },
                    _ => Val::Int(i as i32),
                };
                let mut i = lo;
                loop {
                    if (!*down && i > hi) || (*down && i < hi) {
                        break;
                    }
                    self.write_lval(&lv, mk(i))?;
                    match self.run_cstmt(f, body) {
                        Err(Flow::Break) => break,
                        Err(Flow::Continue) => {}
                        r => r?,
                    }
                    let cur = num(&self.read_lval(&lv));
                    i = match (date, *down) {
                        (true, false) => (cur / 2 + 1) * 2,
                        (true, true) => (cur / 2 - 1) * 2,
                        (false, false) => cur + 1,
                        (false, true) => cur - 1,
                    };
                }
                Ok(())
            }
            CStmt::Exit(line, v) => {
                self.count_step()?;
                self.cur_line = *line;
                if let Some(e) = v {
                    let val = e(self, f)?;
                    let val = match f.ret_ty {
                        Some(TypeRef::Object { kind, .. }) if kind == "record" => val,
                        t => self.coerce(val, t)?,
                    };
                    match &f.ret {
                        Some(r) => *r.borrow_mut() = val,
                        None => f.ret = Some(slot(val)),
                    }
                }
                Err(Flow::Exit)
            }
            CStmt::Break => {
                self.count_step()?;
                Err(Flow::Break)
            }
            CStmt::Continue => {
                self.count_step()?;
                Err(Flow::Continue)
            }
            CStmt::FieldAssign { stmt, target, var, tid, fi, op, value } => {
                self.count_step()?;
                self.cur_line = stmt.line;
                let v = value(self, f)?;
                // the record is resolved after the value, as exec does (the value may reassign it)
                let fast = match var {
                    Some((r, n)) => match var_at(f, *r, n).map(|x| x.slot.borrow().clone()) {
                        Some(Val::Record(h)) if h.borrow().tdef.id == *tid => Some(h),
                        _ => None,
                    },
                    None => match &f.rec {
                        Some(h) if f.with.is_empty() && h.borrow().tdef.id == *tid => Some(h.clone()),
                        _ => None,
                    },
                };
                let lv = match fast {
                    Some(h) => LVal::Field(h, *fi),
                    None => self.lvalue(f, target)?,
                };
                let nv = match op {
                    AssignOp::Set => v,
                    o => {
                        let cur = self.read_lval(&lv);
                        let bop = match o {
                            AssignOp::Add => BinOp::Add,
                            AssignOp::Sub => BinOp::Sub,
                            AssignOp::Mul => BinOp::Mul,
                            _ => BinOp::Div,
                        };
                        self.binop(bop, cur, v)?
                    }
                };
                self.write_lval(&lv, nv).map_err(|e| match e {
                    Flow::Err(mut x) => {
                        if x.stack.is_empty() {
                            x.stack.push(format!("line {}", stmt.line));
                        }
                        Flow::Err(x)
                    }
                    o => o,
                })
            }
            CStmt::Expr { stmt, discard, value } => {
                self.count_step()?;
                self.cur_line = stmt.line;
                self.discard = *discard;
                let r = value(self, f);
                self.discard = false;
                r.map_err(|e| match e {
                    Flow::Err(mut x) => {
                        if x.stack.is_empty() {
                            x.stack.push(format!("line {}", stmt.line));
                        }
                        Flow::Err(x)
                    }
                    o => o,
                })?;
                Ok(())
            }
            CStmt::Assign { stmt, name, var, op, value } => {
                let Some(slot) = var_at(f, *var, name).map(|v| (v.slot.clone(), v.ty)) else {
                    // layout differs from the one compiled against: the tree-walker (counts the step)
                    return self.exec(f, stmt);
                };
                self.count_step()?;
                self.cur_line = stmt.line;
                let v = value(self, f)?;
                let lv = LVal::Slot(slot.0, slot.1);
                let nv = match op {
                    AssignOp::Set => v,
                    o => {
                        let cur = self.read_lval(&lv);
                        let bop = match o {
                            AssignOp::Add => BinOp::Add,
                            AssignOp::Sub => BinOp::Sub,
                            AssignOp::Mul => BinOp::Mul,
                            _ => BinOp::Div,
                        };
                        self.binop(bop, cur, v)?
                    }
                };
                self.write_lval(&lv, nv).map_err(|e| match e {
                    Flow::Err(mut x) => {
                        if x.stack.is_empty() {
                            x.stack.push(format!("line {}", stmt.line));
                        }
                        Flow::Err(x)
                    }
                    o => o,
                })
            }
        }
    }
}

#[inline]
fn var_index(r: VarRef) -> u32 {
    match r {
        VarRef::Local(i) | VarRef::Global(i) => i,
    }
}

/// Table id of a variable declared `Record <table>` (not RecordRef, not temporary-ness relevant).
fn record_var_table(cat: &'static Catalog, f: &Frame, r: VarRef) -> Option<i64> {
    let v = match r {
        VarRef::Local(i) => f.locals.get(i as usize)?,
        VarRef::Global(i) => f.globals.vars.get(i as usize)?,
    };
    match v.ty? {
        TypeRef::Object { kind, target, .. } if kind.eq_ignore_ascii_case("record") => cat.table_by_name(target.trim_matches('"')).map(|t| t.id),
        _ => None,
    }
}

/// `Base.Name` / `Base.Name(args)` with Base a Record variable: field read by index, or the
/// record method directly (eval_member's path for a Record value, minus the name lookups).
fn compile_member(cat: &'static Catalog, f: &Frame, e: &'static Expr, base: &'static Expr, name: &'static str, args: Option<&'static Vec<Expr>>) -> CExpr {
    let Expr::Ident(bn) = base else { return interp_expr(e) };
    let Some(r) = resolve(f, bn) else { return compile_rec_member(cat, f, e, bn, name, args) };
    let Some(tid) = record_var_table(cat, f, r) else { return interp_expr(e) };
    let bn: &'static str = bn;
    let lname = name.to_ascii_lowercase();
    // eval_member's special cases for argument-less members: IsXxx variant tests and RecordRef Close
    let argless = args.is_none_or(|a| a.is_empty());
    if argless && (lname.starts_with("is") || lname == "close") {
        return interp_expr(e);
    }
    let field = cat.tables.get(&tid).and_then(|t| t.field(name));
    match (args, field) {
        // a field (with or without empty parentheses: eval_member reads the field first)
        (_, Some(fi)) if argless => Box::new(move |s, f| {
            if let Some(v) = var_at(f, r, bn) {
                if let Val::Record(h) = &*v.slot.borrow() {
                    let b = h.borrow();
                    if b.tdef.id == tid {
                        return Ok(b.get_val(fi));
                    }
                }
            }
            s.eval(f, e)
        }),
        (Some(a), _) => Box::new(move |s, f| {
            let h = match var_at(f, r, bn).map(|v| v.slot.borrow().clone()) {
                Some(Val::Record(h)) if h.borrow().tdef.id == tid => h,
                _ => return s.eval(f, e),
            };
            s.record_method(f, &h, name, a)
        }),
        _ => interp_expr(e),
    }
}

/// `Name(args)`: call_named's resolution decided once where it is static.
fn compile_call(cat: &'static Catalog, f: &Frame, e: &'static Expr, n: &'static str, args: &'static Vec<Expr>) -> CExpr {
    let cands = |code: u32| -> Vec<u32> {
        let co = &cat.code[code as usize];
        co.find(n).iter().copied().filter(|&i| co.procs[i as usize].kind != ProcKind::Trigger && co.procs[i as usize].params.len() == args.len()).collect()
    };
    let co = &cat.code[f.code as usize];
    let own = cands(f.code);
    if own.len() == 1 {
        // resolve_call with one candidate: no overload scoring
        let pr = ProcRef { code: f.code, proc: own[0] };
        let params = &cat.code[pr.code as usize].procs[pr.proc as usize].params;
        // per argument: a variable resolved now (read by slot), or the tree-walker's eval_arg
        let cargs: Vec<(Option<VarRef>, bool)> = args
            .iter()
            .zip(params.iter())
            .map(|(a, prm)| {
                let var = match a {
                    Expr::Ident(n) => resolve(f, n),
                    _ => None,
                };
                (var, prm.by_ref || crate::dotnet::is_dotnet_param(&prm.ty))
            })
            .collect();
        return Box::new(move |s, f| {
            let discard = s.discard;
            s.discard = false;
            let mut evald = Vec::with_capacity(args.len());
            for (a, (var, by_ref)) in args.iter().zip(cargs.iter()) {
                // eval_arg of a variable: (Some(LVal::Slot(slot, ty)), its value); a `var` parameter only
                // uses the slot, a by-value parameter only the value
                let n = match a {
                    Expr::Ident(n) => &**n,
                    _ => "",
                };
                match var.and_then(|r| var_at(f, r, n)) {
                    Some(v) if *by_ref => evald.push((Some(LVal::Slot(v.slot.clone(), v.ty)), Val::Void)),
                    Some(v) => {
                        let val = v.slot.borrow().clone();
                        evald.push((None, val))
                    }
                    None => evald.push(s.eval_arg(f, a)?),
                }
            }
            let p = &s.code(pr.code).procs[pr.proc as usize];
            let mut out = Vec::with_capacity(args.len());
            for (prm, (lv, v)) in p.params.iter().zip(evald) {
                out.push(match (prm.by_ref, lv) {
                    (true, Some(lv)) => Arg::Ref(lv),
                    (true, None) => Arg::Ref(LVal::Slot(slot(v), None)),
                    (false, Some(lv @ LVal::Slot(..))) if crate::dotnet::is_dotnet_param(&prm.ty) => Arg::Ref(lv),
                    (false, _) => Arg::Val(v),
                });
            }
            s.pending_discard = discard;
            s.invoke(pr, out, f.inst.clone(), f.rec.clone(), f.xrec.clone())
        });
    }
    if !own.is_empty() || co.kind == ObjectKind::Query {
        return interp_expr(e);
    }
    if let Some(&bc) = co.extends.and_then(|b| cat.code_by_key.get(&b)) {
        if !cands(bc).is_empty() {
            return interp_expr(e);
        }
    }
    // no procedure of the object: an implicit Rec method, a table procedure, or a builtin
    let rec = f.rec.as_ref().map(|r| (r.borrow().tdef.id, r.borrow().tdef.code));
    match rec {
        Some((tid, _)) if crate::recops::is_record_method(n) => Box::new(move |s, f| {
            let h = match &f.rec {
                Some(h) if h.borrow().tdef.id == tid => h.clone(),
                _ => return s.eval(f, e),
            };
            s.record_method(f, &h, n, args)
        }),
        Some((_, tcode)) if tcode != f.code && !cands(tcode).is_empty() => interp_expr(e),
        _ => {
            let tid = rec.map(|r| r.0);
            Box::new(move |s, f| {
                // the frame must look as it did when this was decided
                if f.rec.as_ref().map(|r| r.borrow().tdef.id) != tid {
                    return s.eval(f, e);
                }
                s.builtin(f, n, args)
            })
        }
    }
}

/// Assignment targets exec treats specially (page LookupMode, Rec.Mark / MarkedOnly).
fn special_assign_name(n: &str) -> bool {
    n.eq_ignore_ascii_case("lookupmode") || n.eq_ignore_ascii_case("mark") || n.eq_ignore_ascii_case("markedonly")
}

/// `Rec.Name` / `Rec.Name(args)` on the implicit Rec (eval_member: `Rec` is not a variable, not a
/// field, not a static class, .NET type or enum, so eval_ident gives the frame's record).
fn compile_rec_member(cat: &'static Catalog, f: &Frame, e: &'static Expr, bn: &'static str, name: &'static str, args: Option<&'static Vec<Expr>>) -> CExpr {
    if !bn.eq_ignore_ascii_case("rec") {
        return interp_expr(e);
    }
    let Some((tid, rec_field)) = f.rec.as_ref().map(|r| (r.borrow().tdef.id, r.borrow().tdef.field(bn).is_some())) else { return interp_expr(e) };
    if rec_field || crate::builtins::is_static_class("rec") || crate::dotnet::is_static_type("rec") || cat.enum_by_name(bn).is_some() {
        return interp_expr(e);
    }
    let lname = name.to_ascii_lowercase();
    let argless = args.is_none_or(|a| a.is_empty());
    if argless && (lname.starts_with("is") || lname == "close") {
        return interp_expr(e);
    }
    let field = cat.tables.get(&tid).and_then(|t| t.field(name));
    // the frame's record, when nothing could shadow `Rec` (a `with` record's field named Rec)
    fn frame_rec(f: &Frame, tid: i64) -> Option<crate::record::RecHandle> {
        match &f.rec {
            Some(h) if f.with.is_empty() && h.borrow().tdef.id == tid => Some(h.clone()),
            _ => None,
        }
    }
    match (args, field) {
        (_, Some(fi)) if argless => Box::new(move |s, f| match frame_rec(f, tid) {
            Some(h) => Ok(h.borrow().get_val(fi)),
            None => s.eval(f, e),
        }),
        (Some(a), _) => Box::new(move |s, f| match frame_rec(f, tid) {
            Some(h) => s.record_method(f, &h, name, a),
            None => s.eval(f, e),
        }),
        _ => interp_expr(e),
    }
}
