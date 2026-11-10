//! Recursive-descent parser for AL.

use crate::ast::*;
use crate::lexer::{Tk, Token};
use std::sync::LazyLock;

#[derive(Debug, Clone)]
pub struct ParseError {
    pub msg: String,
    pub line: u32,
    pub col: u32,
}

pub type PResult<T> = Result<T, ParseError>;

static EOF: LazyLock<Token> = LazyLock::new(|| Token { kind: Tk::Eof, text: "".into(), line: 0, col: 0 });

const RESERVED: &[&str] = &[
    "begin", "end", "if", "then", "else", "case", "of", "while", "do", "repeat", "until", "for", "to", "downto",
    "foreach", "in", "exit", "with", "and", "or", "xor", "div", "mod", "not", "asserterror", "true", "false",
];

fn is_reserved(t: &Token) -> bool {
    t.kind == Tk::Ident && RESERVED.iter().any(|k| t.text.eq_ignore_ascii_case(k))
}

pub struct Parser<'t> {
    t: &'t [Token],
    p: usize,
}

impl<'t> Parser<'t> {
    pub fn new(t: &'t [Token]) -> Self {
        Parser { t, p: 0 }
    }

    #[inline]
    fn peek(&self) -> &'t Token {
        self.t.get(self.p).unwrap_or(&EOF)
    }
    #[inline]
    fn peek_at(&self, n: usize) -> &'t Token {
        self.t.get(self.p + n).unwrap_or(&EOF)
    }
    #[inline]
    fn bump(&mut self) -> &'t Token {
        let t = self.t.get(self.p).unwrap_or(&EOF);
        if self.p < self.t.len() {
            self.p += 1;
        }
        t
    }
    #[inline]
    fn at(&self, k: Tk) -> bool {
        self.peek().kind == k
    }
    #[inline]
    fn at_kw(&self, kw: &str) -> bool {
        self.peek().is_kw(kw)
    }
    fn eat(&mut self, k: Tk) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn eat_kw(&mut self, kw: &str) -> bool {
        if self.at_kw(kw) {
            self.bump();
            true
        } else {
            false
        }
    }
    pub fn at_eof(&self) -> bool {
        self.at(Tk::Eof)
    }

    fn err<T>(&self, msg: impl Into<String>) -> PResult<T> {
        let t = self.peek();
        let shown = match t.kind {
            Tk::Eof => "end of input".to_string(),
            Tk::Ident | Tk::QIdent | Tk::Int | Tk::Dec => format!("'{}'", t.text),
            Tk::Str => format!("string '{}'", t.text),
            k => format!("{k:?}"),
        };
        Err(ParseError { msg: format!("{} (found {})", msg.into(), shown), line: t.line, col: t.col })
    }

    fn expect(&mut self, k: Tk) -> PResult<&'t Token> {
        if self.at(k) {
            Ok(self.bump())
        } else {
            self.err(format!("expected {k:?}"))
        }
    }
    fn expect_kw(&mut self, kw: &str) -> PResult<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            self.err(format!("expected '{kw}'"))
        }
    }
    fn name(&mut self) -> PResult<String> {
        let t = self.peek();
        if t.is_name() {
            self.bump();
            Ok(t.text.to_string())
        } else {
            self.err("expected identifier")
        }
    }
    fn dotted_name(&mut self) -> PResult<String> {
        let mut s = self.name()?;
        while self.at(Tk::Dot) && self.peek_at(1).is_name() {
            self.bump();
            s.push('.');
            s.push_str(&self.name()?);
        }
        Ok(s)
    }

    // ------------------------------------------------------------------ file

    pub fn file(&mut self) -> PResult<SourceFile> {
        let mut f = SourceFile::default();
        loop {
            if self.at_eof() {
                break;
            }
            if self.eat_kw("namespace") {
                f.namespace = Some(self.dotted_name()?);
                self.expect(Tk::Semi)?;
            } else if self.eat_kw("using") {
                f.usings.push(self.dotted_name()?);
                self.expect(Tk::Semi)?;
            } else if self.eat(Tk::Semi) {
            } else {
                let o = self.object()?;
                f.objects.push(o);
            }
        }
        Ok(f)
    }

    fn object(&mut self) -> PResult<Object> {
        let t = self.peek();
        let line = t.line;
        let kind = match (t.kind == Tk::Ident).then(|| ObjectKind::from_kw(&t.text)).flatten() {
            Some(k) => k,
            None => return self.err("expected object declaration"),
        };
        self.bump();
        let mut obj = Object {
            kind,
            id: None,
            name: String::new(),
            extends: vec![],
            implements: vec![],
            customizes: None,
            body: Body::default(),
            line,
        };
        if kind != ObjectKind::DotNet {
            if self.at(Tk::Int) {
                obj.id = self.bump().text.parse().ok();
            }
            obj.name = self.name()?;
            loop {
                if self.eat_kw("extends") {
                    obj.extends.push(self.name()?);
                    while self.eat(Tk::Comma) {
                        obj.extends.push(self.name()?);
                    }
                } else if self.eat_kw("implements") {
                    obj.implements.push(self.name()?);
                    while self.eat(Tk::Comma) {
                        obj.implements.push(self.name()?);
                    }
                } else if self.eat_kw("customizes") {
                    obj.customizes = Some(self.name()?);
                } else {
                    break;
                }
            }
        }
        obj.body = self.body()?;
        Ok(obj)
    }

    // ------------------------------------------------------------------ declarative body

    fn body(&mut self) -> PResult<Body> {
        self.expect(Tk::LBrace)?;
        let mut members = Vec::new();
        while !self.at(Tk::RBrace) {
            if self.at_eof() {
                return self.err("unexpected end of input in block");
            }
            if let Some(m) = self.member()? {
                members.push(m);
            }
        }
        self.bump();
        Ok(Body { members })
    }

    fn member(&mut self) -> PResult<Option<Member>> {
        let t = self.peek();
        let t1 = self.peek_at(1);
        if t.kind == Tk::Semi {
            self.bump();
            return Ok(None);
        }
        if t.kind == Tk::LBrack {
            let attrs = self.attributes()?;
            let mut p = self.procedure()?;
            p.attributes = attrs;
            return Ok(Some(Member::Procedure(p)));
        }
        if t.is_kw("var") {
            self.bump();
            return Ok(Some(Member::Vars { protected: false, vars: self.var_section()? }));
        }
        if t.is_kw("protected") && t1.is_kw("var") {
            self.bump();
            self.bump();
            return Ok(Some(Member::Vars { protected: true, vars: self.var_section()? }));
        }
        if (t.is_kw("local") || t.is_kw("internal") || t.is_kw("protected")) && t1.is_kw("procedure")
            || t.is_kw("procedure") && t1.is_name()
            || t.is_kw("trigger") && t1.is_name()
            || t.is_kw("event") && t1.is_name() && self.peek_at(2).kind == Tk::LParen
        {
            return Ok(Some(Member::Procedure(self.procedure()?)));
        }
        if t.is_name() {
            match t1.kind {
                Tk::Eq => {
                    self.bump();
                    self.bump();
                    let value = self.raw_until_semi();
                    return Ok(Some(Member::Property(Property { name: t.text.to_string(), value, line: t.line })));
                }
                Tk::LParen | Tk::LBrace => return Ok(Some(Member::Element(self.element()?))),
                _ => {}
            }
        }
        self.err("expected property, element, procedure or trigger")
    }

    fn raw_until_semi(&mut self) -> Vec<Token> {
        let mut out = Vec::new();
        let mut depth = 0i32;
        loop {
            let t = self.peek();
            match t.kind {
                Tk::Eof => break,
                Tk::Semi if depth == 0 => {
                    self.bump();
                    break;
                }
                Tk::RBrace if depth == 0 => break,
                Tk::LParen | Tk::LBrack => depth += 1,
                Tk::RParen | Tk::RBrack => depth -= 1,
                _ => {}
            }
            out.push(t.clone());
            self.bump();
        }
        out
    }

    fn element(&mut self) -> PResult<Element> {
        let kt = self.bump();
        let mut el = Element { kind: kt.text.to_ascii_lowercase(), args: vec![], body: Body::default(), line: kt.line };
        if self.eat(Tk::LParen) {
            let mut cur = Vec::new();
            let mut depth = 0i32;
            loop {
                let t = self.peek();
                match t.kind {
                    Tk::Eof => return self.err("unterminated element arguments"),
                    Tk::RParen if depth == 0 => {
                        self.bump();
                        break;
                    }
                    Tk::Semi if depth == 0 => {
                        self.bump();
                        el.args.push(std::mem::take(&mut cur));
                        continue;
                    }
                    Tk::LParen | Tk::LBrack => depth += 1,
                    Tk::RParen | Tk::RBrack => depth -= 1,
                    _ => {}
                }
                cur.push(t.clone());
                self.bump();
            }
            el.args.push(cur);
        }
        if self.at(Tk::LBrace) {
            el.body = self.body()?;
        } else {
            self.eat(Tk::Semi);
        }
        Ok(el)
    }

    // ------------------------------------------------------------------ attributes / procedures / vars

    fn attributes(&mut self) -> PResult<Vec<Attribute>> {
        let mut out = Vec::new();
        while self.eat(Tk::LBrack) {
            loop {
                let name = self.name()?;
                let mut args = Vec::new();
                if self.eat(Tk::LParen) {
                    if !self.at(Tk::RParen) {
                        args.push(self.expr()?);
                        while self.eat(Tk::Comma) {
                            args.push(self.expr()?);
                        }
                    }
                    self.expect(Tk::RParen)?;
                }
                out.push(Attribute { name, args });
                if !self.eat(Tk::Comma) {
                    break;
                }
            }
            self.expect(Tk::RBrack)?;
        }
        Ok(out)
    }

    fn procedure(&mut self) -> PResult<Procedure> {
        let line = self.peek().line;
        let mut access = Access::Public;
        if self.eat_kw("local") {
            access = Access::Local;
        } else if self.eat_kw("internal") {
            access = Access::Internal;
        } else if self.eat_kw("protected") {
            access = Access::Protected;
        }
        let kind = if self.eat_kw("procedure") {
            ProcKind::Procedure
        } else if self.eat_kw("trigger") {
            ProcKind::Trigger
        } else if self.eat_kw("event") {
            ProcKind::Event
        } else {
            return self.err("expected 'procedure' or 'trigger'");
        };
        let mut name = self.name()?;
        if self.eat(Tk::ColonColon) {
            name.push_str("::");
            name.push_str(&self.name()?);
        }
        let mut params = Vec::new();
        self.expect(Tk::LParen)?;
        if !self.at(Tk::RParen) {
            loop {
                let by_ref = self.eat_kw("var");
                let mut names = vec![self.name()?];
                while self.eat(Tk::Comma) {
                    names.push(self.name()?);
                }
                self.expect(Tk::Colon)?;
                let ty = self.type_ref()?;
                for n in names {
                    params.push(Param { name: n, by_ref, ty: ty.clone() });
                }
                if !self.eat(Tk::Semi) {
                    break;
                }
            }
        }
        self.expect(Tk::RParen)?;
        let mut ret = None;
        let mut ret_name = None;
        if self.eat(Tk::Colon) {
            ret = Some(self.type_ref()?);
        } else if self.peek().is_name() && !is_reserved(self.peek()) && self.peek_at(1).kind == Tk::Colon {
            ret_name = Some(self.name()?);
            self.bump();
            ret = Some(self.type_ref()?);
        }
        self.eat(Tk::Semi);
        let mut vars = Vec::new();
        if self.eat_kw("var") {
            vars = self.var_section()?;
        }
        let body = if self.at_kw("begin") {
            let b = self.block()?;
            self.eat(Tk::Semi);
            Some(b)
        } else {
            None
        };
        Ok(Procedure { name, kind, access, attributes: vec![], params, ret_name, ret, vars, body, line })
    }

    fn var_section(&mut self) -> PResult<Vec<VarDecl>> {
        let mut out = Vec::new();
        loop {
            let save = self.p;
            let attrs = if self.at(Tk::LBrack) { self.attributes()? } else { vec![] };
            let t = self.peek();
            let t1 = self.peek_at(1);
            if !(t.is_name() && !is_reserved(t) && (t1.kind == Tk::Colon || t1.kind == Tk::Comma)) {
                self.p = save;
                break;
            }
            let mut names = vec![self.name()?];
            while self.eat(Tk::Comma) {
                names.push(self.name()?);
            }
            self.expect(Tk::Colon)?;
            let ty = self.type_ref()?;
            self.expect(Tk::Semi)?;
            for n in names {
                out.push(VarDecl { name: n, ty: ty.clone(), attributes: attrs.clone() });
            }
        }
        Ok(out)
    }

    pub fn type_ref(&mut self) -> PResult<TypeRef> {
        let t = self.peek();
        if !t.is_name() {
            return self.err("expected type");
        }
        self.bump();
        let lower = t.text.to_ascii_lowercase();
        Ok(match lower.as_str() {
            "array" => {
                self.expect(Tk::LBrack)?;
                let mut dims = vec![self.int_lit()?];
                while self.eat(Tk::Comma) {
                    dims.push(self.int_lit()?);
                }
                self.expect(Tk::RBrack)?;
                self.expect_kw("of")?;
                TypeRef::Array { dims, elem: Box::new(self.type_ref()?) }
            }
            "list" if self.at_kw("of") => {
                self.bump();
                self.expect(Tk::LBrack)?;
                let e = self.type_ref()?;
                self.expect(Tk::RBrack)?;
                TypeRef::List(Box::new(e))
            }
            "dictionary" if self.at_kw("of") => {
                self.bump();
                self.expect(Tk::LBrack)?;
                let k = self.type_ref()?;
                self.expect(Tk::Comma)?;
                let v = self.type_ref()?;
                self.expect(Tk::RBrack)?;
                TypeRef::Dictionary(Box::new(k), Box::new(v))
            }
            "option" => {
                let mut members = Vec::new();
                let t0 = self.peek();
                if (t0.is_name() && !is_reserved(t0) && !t0.is_kw("var")) || self.at(Tk::Comma) {
                    loop {
                        let t = self.peek();
                        if t.is_name() && !is_reserved(t) && !t.is_kw("var") && !t.is_kw("temporary") {
                            members.push(self.bump().text.to_string());
                        } else {
                            members.push(String::new());
                        }
                        if !self.eat(Tk::Comma) {
                            break;
                        }
                    }
                }
                TypeRef::Option(members)
            }
            "textconst" if self.peek().kind == Tk::Ident && self.peek_at(1).kind == Tk::Eq => {
                // legacy: TextConst ENU = 'x', DEU = 'y'
                let mut text = String::new();
                loop {
                    let lang = self.name()?;
                    self.expect(Tk::Eq)?;
                    let v = self.expect(Tk::Str)?.text.to_string();
                    if text.is_empty() || lang.eq_ignore_ascii_case("enu") {
                        text = v;
                    }
                    if !self.eat(Tk::Comma) {
                        break;
                    }
                }
                TypeRef::TextConst(text)
            }
            "label" | "textconst" if self.at(Tk::Str) => {
                let text = self.bump().text.to_string();
                let mut locked = false;
                let mut max_len = None;
                while self.eat(Tk::Comma) {
                    if self.at(Tk::Semi) {
                        break;
                    }
                    let pn = self.name()?;
                    self.expect(Tk::Eq)?;
                    let v = self.bump();
                    if pn.eq_ignore_ascii_case("locked") {
                        locked = v.is_kw("true");
                    } else if pn.eq_ignore_ascii_case("maxlength") {
                        max_len = v.text.parse().ok();
                    }
                }
                if lower == "label" {
                    TypeRef::Label { text, locked, max_len }
                } else {
                    TypeRef::TextConst(text)
                }
            }
            "record" | "codeunit" | "page" | "report" | "query" | "xmlport" | "enum" | "interface" | "testpage"
            | "testrequestpage" | "dotnet" | "testpermissions" | "controladdin"
                if self.peek().is_name() || self.at(Tk::Int) =>
            {
                let target = if self.at(Tk::Int) { self.bump().text.to_string() } else { self.dotted_name()? };
                let temporary = self.eat_kw("temporary");
                TypeRef::Object { kind: lower, target, temporary }
            }
            _ => {
                let mut len = None;
                if self.eat(Tk::LBrack) {
                    len = Some(self.int_lit()?);
                    self.expect(Tk::RBrack)?;
                }
                TypeRef::Simple { name: t.text.to_string(), len }
            }
        })
    }

    fn int_lit(&mut self) -> PResult<u32> {
        let t = self.expect(Tk::Int)?;
        t.text.parse().map_err(|_| ParseError { msg: "bad integer".into(), line: t.line, col: t.col })
    }

    // ------------------------------------------------------------------ statements

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect_kw("begin")?;
        let s = self.stmt_list(&["end"])?;
        self.expect_kw("end")?;
        Ok(s)
    }

    fn at_any_kw(&self, kws: &[&str]) -> bool {
        kws.iter().any(|k| self.at_kw(k))
    }

    fn stmt_list(&mut self, terms: &[&str]) -> PResult<Vec<Stmt>> {
        let mut out = Vec::new();
        loop {
            while self.eat(Tk::Semi) {}
            if self.at_any_kw(terms) || self.at_eof() {
                break;
            }
            out.push(self.stmt()?);
            if self.eat(Tk::Semi) {
                continue;
            }
            if self.at_any_kw(terms) {
                break;
            }
            return self.err("expected ';'");
        }
        Ok(out)
    }

    fn opt_stmt(&mut self) -> PResult<Stmt> {
        if self.at(Tk::Semi) || self.at_kw("else") || self.at_kw("end") || self.at_kw("until") {
            Ok(Stmt { kind: StmtKind::Empty, line: self.peek().line })
        } else {
            self.stmt()
        }
    }

    pub fn stmt(&mut self) -> PResult<Stmt> {
        let t = self.peek();
        let line = t.line;
        let kind = if t.kind == Tk::Ident {
            match t.text.to_ascii_lowercase().as_str() {
                "begin" => StmtKind::Block(self.block()?),
                "if" => {
                    self.bump();
                    let cond = self.expr()?;
                    self.expect_kw("then")?;
                    let then = Box::new(self.opt_stmt()?);
                    let els = if self.eat_kw("else") { Some(Box::new(self.opt_stmt()?)) } else { None };
                    StmtKind::If { cond, then, els }
                }
                "case" => {
                    self.bump();
                    let subject = self.expr()?;
                    self.expect_kw("of")?;
                    let mut branches = Vec::new();
                    let mut els = None;
                    loop {
                        while self.eat(Tk::Semi) {}
                        if self.eat_kw("end") {
                            break;
                        }
                        if self.eat_kw("else") {
                            els = Some(self.stmt_list(&["end"])?);
                            self.expect_kw("end")?;
                            break;
                        }
                        let mut values = vec![self.case_value()?];
                        while self.eat(Tk::Comma) {
                            values.push(self.case_value()?);
                        }
                        self.expect(Tk::Colon)?;
                        let body = self.opt_stmt()?;
                        branches.push(CaseBranch { values, body });
                        if !self.at(Tk::Semi) && !self.at_kw("end") && !self.at_kw("else") {
                            return self.err("expected ';' after case branch");
                        }
                    }
                    StmtKind::Case { subject, branches, els }
                }
                "while" => {
                    self.bump();
                    let cond = self.expr()?;
                    self.expect_kw("do")?;
                    StmtKind::While { cond, body: Box::new(self.opt_stmt()?) }
                }
                "repeat" => {
                    self.bump();
                    let body = self.stmt_list(&["until"])?;
                    self.expect_kw("until")?;
                    StmtKind::Repeat { body, until: self.expr()? }
                }
                "for" => {
                    self.bump();
                    let var = self.postfix()?;
                    self.expect(Tk::Assign)?;
                    let from = self.expr()?;
                    let down = if self.eat_kw("to") {
                        false
                    } else if self.eat_kw("downto") {
                        true
                    } else {
                        return self.err("expected 'to' or 'downto'");
                    };
                    let to = self.expr()?;
                    self.expect_kw("do")?;
                    StmtKind::For { var, from, to, down, body: Box::new(self.opt_stmt()?) }
                }
                "foreach" => {
                    self.bump();
                    let var = self.postfix()?;
                    self.expect_kw("in")?;
                    let iter = self.expr()?;
                    self.expect_kw("do")?;
                    StmtKind::ForEach { var, iter, body: Box::new(self.opt_stmt()?) }
                }
                "with" => {
                    self.bump();
                    let rec = self.expr()?;
                    self.expect_kw("do")?;
                    StmtKind::With { rec, body: Box::new(self.opt_stmt()?) }
                }
                "exit" => {
                    self.bump();
                    if self.eat(Tk::LParen) {
                        let v = if self.at(Tk::RParen) { None } else { Some(self.expr()?) };
                        self.expect(Tk::RParen)?;
                        StmtKind::Exit(v)
                    } else {
                        StmtKind::Exit(None)
                    }
                }
                "asserterror" => {
                    self.bump();
                    StmtKind::AssertError(Box::new(self.opt_stmt()?))
                }
                "break" if self.stmt_end_follows(1) => {
                    self.bump();
                    StmtKind::Break
                }
                "continue" if self.stmt_end_follows(1) => {
                    self.bump();
                    StmtKind::Continue
                }
                _ => self.simple_stmt()?,
            }
        } else {
            self.simple_stmt()?
        };
        Ok(Stmt { kind, line })
    }

    fn stmt_end_follows(&self, n: usize) -> bool {
        let t = self.peek_at(n);
        t.kind == Tk::Semi || t.is_kw("end") || t.is_kw("else") || t.is_kw("until")
    }

    fn simple_stmt(&mut self) -> PResult<StmtKind> {
        let target = self.expr()?;
        let op = match self.peek().kind {
            Tk::Assign => AssignOp::Set,
            Tk::PlusAssign => AssignOp::Add,
            Tk::MinusAssign => AssignOp::Sub,
            Tk::MulAssign => AssignOp::Mul,
            Tk::DivAssign => AssignOp::Div,
            _ => return Ok(StmtKind::Expr(target)),
        };
        self.bump();
        let value = self.expr()?;
        Ok(StmtKind::Assign { target, op, value })
    }

    fn case_value(&mut self) -> PResult<Expr> {
        if self.eat(Tk::DotDot) {
            return Ok(Expr::Range(None, Some(Box::new(self.additive()?))));
        }
        let v = self.expr()?;
        if self.eat(Tk::DotDot) {
            let hi = if self.at(Tk::Colon) || self.at(Tk::Comma) { None } else { Some(Box::new(self.additive()?)) };
            return Ok(Expr::Range(Some(Box::new(v)), hi));
        }
        Ok(v)
    }

    // ------------------------------------------------------------------ expressions

    pub fn expr(&mut self) -> PResult<Expr> {
        let c = self.relational()?;
        if self.eat(Tk::Question) {
            let a = self.expr()?;
            self.expect(Tk::Colon)?;
            let b = self.expr()?;
            return Ok(Expr::Ternary(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    fn relational(&mut self) -> PResult<Expr> {
        let mut l = self.additive()?;
        loop {
            let op = match self.peek().kind {
                Tk::Eq => BinOp::Eq,
                Tk::Ne => BinOp::Ne,
                Tk::Lt => BinOp::Lt,
                Tk::Le => BinOp::Le,
                Tk::Gt => BinOp::Gt,
                Tk::Ge => BinOp::Ge,
                Tk::Ident if self.at_kw("in") => BinOp::In,
                _ => return Ok(l),
            };
            self.bump();
            let r = self.additive()?;
            l = Expr::Binary(op, Box::new(l), Box::new(r));
        }
    }

    fn additive(&mut self) -> PResult<Expr> {
        let mut l = self.mult()?;
        loop {
            let op = match self.peek().kind {
                Tk::Plus => BinOp::Add,
                Tk::Minus => BinOp::Sub,
                Tk::Ident if self.at_kw("or") => BinOp::Or,
                Tk::Ident if self.at_kw("xor") => BinOp::Xor,
                _ => return Ok(l),
            };
            self.bump();
            let r = self.mult()?;
            l = Expr::Binary(op, Box::new(l), Box::new(r));
        }
    }

    fn mult(&mut self) -> PResult<Expr> {
        let mut l = self.unary()?;
        loop {
            let op = match self.peek().kind {
                Tk::Star => BinOp::Mul,
                Tk::Slash => BinOp::Div,
                Tk::Ident if self.at_kw("div") => BinOp::IntDiv,
                Tk::Ident if self.at_kw("mod") => BinOp::Mod,
                Tk::Ident if self.at_kw("and") => BinOp::And,
                _ => return Ok(l),
            };
            self.bump();
            let r = self.unary()?;
            l = Expr::Binary(op, Box::new(l), Box::new(r));
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        if self.eat_kw("not") {
            return Ok(Expr::Unary(UnOp::Not, Box::new(self.unary()?)));
        }
        if self.eat(Tk::Minus) {
            return Ok(Expr::Unary(UnOp::Neg, Box::new(self.unary()?)));
        }
        if self.eat(Tk::Plus) {
            return Ok(Expr::Unary(UnOp::Plus, Box::new(self.unary()?)));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            match self.peek().kind {
                Tk::Dot => {
                    self.bump();
                    let t = self.peek();
                    if !t.is_name() {
                        return self.err("expected member name");
                    }
                    self.bump();
                    e = Expr::Member(Box::new(e), t.text.clone());
                }
                Tk::ColonColon => {
                    self.bump();
                    let t = self.peek();
                    if !(t.is_name() || t.kind == Tk::Int) {
                        return self.err("expected name after '::'");
                    }
                    self.bump();
                    e = Expr::Scope(Box::new(e), t.text.clone());
                }
                Tk::LParen => {
                    self.bump();
                    let mut args = Vec::new();
                    if !self.at(Tk::RParen) {
                        args.push(self.expr()?);
                        while self.eat(Tk::Comma) {
                            args.push(self.expr()?);
                        }
                    }
                    self.expect(Tk::RParen)?;
                    e = Expr::Call(Box::new(e), args);
                }
                Tk::LBrack => {
                    self.bump();
                    let mut idx = vec![self.expr()?];
                    while self.eat(Tk::Comma) {
                        idx.push(self.expr()?);
                    }
                    self.expect(Tk::RBrack)?;
                    e = Expr::Index(Box::new(e), idx);
                }
                Tk::Ident if self.at_kw("is") || self.at_kw("as") => {
                    let is = self.at_kw("is");
                    self.bump();
                    let ty: Box<str> = self.dotted_name()?.into();
                    e = if is { Expr::Is(Box::new(e), ty) } else { Expr::As(Box::new(e), ty) };
                }
                _ => return Ok(e),
            }
        }
    }

    fn primary(&mut self) -> PResult<Expr> {
        let t = self.peek();
        let e = match t.kind {
            Tk::Int => Expr::Int(t.text.parse().unwrap_or(i64::MAX)),
            Tk::BigInt => Expr::BigInt(t.text.parse().unwrap_or(i64::MAX)),
            Tk::Dec => Expr::Dec(t.text.clone()),
            Tk::Str => Expr::Str(t.text.clone()),
            Tk::Date => Expr::Date(t.text.clone()),
            Tk::Time => Expr::Time(t.text.clone()),
            Tk::DateTime => Expr::DateTime(t.text.clone()),
            Tk::QIdent => Expr::Ident(t.text.clone()),
            Tk::LParen => {
                self.bump();
                let e = self.expr()?;
                self.expect(Tk::RParen)?;
                return Ok(e);
            }
            Tk::LBrack => {
                self.bump();
                let mut items = Vec::new();
                if !self.at(Tk::RBrack) {
                    loop {
                        items.push(self.case_value()?);
                        if !self.eat(Tk::Comma) {
                            break;
                        }
                    }
                }
                self.expect(Tk::RBrack)?;
                return Ok(Expr::Set(items));
            }
            Tk::Ident => {
                if t.is_kw("true") {
                    Expr::Bool(true)
                } else if t.is_kw("false") {
                    Expr::Bool(false)
                } else if t.is_kw("this") {
                    Expr::This
                } else if is_reserved(t) {
                    return self.err("expected expression");
                } else {
                    Expr::Ident(t.text.clone())
                }
            }
            _ => return self.err("expected expression"),
        };
        self.bump();
        Ok(e)
    }
}

/// Parse a standalone expression from raw tokens (element args, property values).
pub fn parse_expr(tokens: &[Token]) -> PResult<Expr> {
    let mut p = Parser::new(tokens);
    let e = p.expr()?;
    if !p.at_eof() {
        return p.err("trailing tokens after expression");
    }
    Ok(e)
}

/// Parse a standalone type reference from raw tokens (table field types).
pub fn parse_type(tokens: &[Token]) -> PResult<TypeRef> {
    let mut p = Parser::new(tokens);
    let t = p.type_ref()?;
    if !p.at_eof() {
        return p.err("trailing tokens after type");
    }
    Ok(t)
}
