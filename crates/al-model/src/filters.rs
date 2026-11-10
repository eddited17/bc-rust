//! Declarative table filters: CalcFormula, TableRelation, SubPageLink,
//! RunPageLink, SourceTableView, DataItemLink.

use crate::props::tokens_text;
use al_syntax::{Tk, Token};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub enum CondKind {
    /// `const(X)` — exact value (as text, converted per target field type).
    Const,
    /// `filter(X)` — filter expression text.
    Filter,
    /// `field(X)` — value (or filter for FlowFilter fields) of a field on the source record.
    Field,
}

#[derive(Debug, Clone, Serialize)]
pub struct Cond {
    pub field: String,
    pub kind: CondKind,
    pub value: String,
    /// `field(upperlimit(X))`
    pub upper_limit: bool,
    /// `field(filter(X))`
    pub as_filter: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum CalcMethod {
    Sum,
    Count,
    Exist,
    Lookup,
    Min,
    Max,
    Average,
}

#[derive(Debug, Clone, Serialize)]
pub struct CalcFormula {
    pub method: CalcMethod,
    pub reverse_sign: bool,
    pub table: String,
    pub field: Option<String>,
    pub conds: Vec<Cond>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationBranch {
    pub when: Vec<Cond>,
    pub table: String,
    pub field: Option<String>,
    pub conds: Vec<Cond>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TableView {
    pub sorting: Vec<String>,
    pub descending: bool,
    pub conds: Vec<Cond>,
}

struct Cur<'a> {
    t: &'a [Token],
    p: usize,
}

impl<'a> Cur<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.t.get(self.p)
    }
    fn kw(&self, k: &str) -> bool {
        self.peek().is_some_and(|t| t.is_kw(k))
    }
    fn at(&self, k: Tk) -> bool {
        self.peek().is_some_and(|t| t.kind == k)
    }
    fn eat(&mut self, k: Tk) -> bool {
        if self.at(k) {
            self.p += 1;
            true
        } else {
            false
        }
    }
    fn name(&mut self) -> Option<String> {
        let t = self.peek()?;
        if t.is_name() || t.kind == Tk::Int {
            self.p += 1;
            Some(t.text.to_string())
        } else {
            None
        }
    }
    /// Qualified name `A.B."C"` — returns (table, optional trailing field).
    fn table_field(&mut self) -> Option<(String, Option<String>)> {
        let mut parts = vec![self.name()?];
        while self.at(Tk::Dot) {
            self.p += 1;
            parts.push(self.name()?);
        }
        // Namespaced tables (Microsoft.Sales.Customer."Cust. Ledger Entry".Amount) are rare;
        // treat the last two parts as table.field when more than one part exists.
        if parts.len() == 1 {
            Some((parts.pop().unwrap(), None))
        } else {
            let f = parts.pop();
            Some((parts.pop().unwrap(), f))
        }
    }
    /// Tokens inside a balanced (...) group; cursor must be at '('.
    fn group(&mut self) -> Option<&'a [Token]> {
        if !self.eat(Tk::LParen) {
            return None;
        }
        let start = self.p;
        let mut depth = 0;
        while let Some(t) = self.peek() {
            match t.kind {
                Tk::LParen => depth += 1,
                Tk::RParen if depth == 0 => {
                    let g = &self.t[start..self.p];
                    self.p += 1;
                    return Some(g);
                }
                Tk::RParen => depth -= 1,
                _ => {}
            }
            self.p += 1;
        }
        None
    }
}

/// The text of a `filter(...)` group: a quoted name with filter operator characters (an option
/// member such as "Very Low (Hidden)") is one value, quoted for the filter parser.
fn filter_text(g: &[Token]) -> String {
    let special = |t: &Token| t.kind == Tk::QIdent && t.text.contains(|ch: char| "()|&<>=*@'".contains(ch));
    if !g.iter().any(special) {
        return tokens_text(g);
    }
    let mut s = String::new();
    for (i, t) in g.iter().enumerate() {
        if special(t) {
            s.push('\'');
            s.push_str(&t.text.replace('\'', "''"));
            s.push('\'');
        } else {
            s.push_str(&tokens_text(&g[i..i + 1]));
        }
    }
    s
}

/// `A = field(B), C = const(D), E = filter(<>'')`
pub fn parse_conds(toks: &[Token]) -> Vec<Cond> {
    let mut out = Vec::new();
    let mut c = Cur { t: toks, p: 0 };
    while c.peek().is_some() {
        let Some(field) = c.name() else { break };
        if !c.eat(Tk::Eq) {
            break;
        }
        let kind_tok = match c.peek() {
            Some(t) => t,
            None => break,
        };
        let kind = if kind_tok.is_kw("const") {
            CondKind::Const
        } else if kind_tok.is_kw("filter") {
            CondKind::Filter
        } else if kind_tok.is_kw("field") {
            CondKind::Field
        } else {
            break;
        };
        c.p += 1;
        let Some(g) = c.group() else { break };
        let mut cond = Cond { field, kind, value: String::new(), upper_limit: false, as_filter: false };
        if cond.kind == CondKind::Field && g.len() >= 3 && (g[0].is_kw("upperlimit") || g[0].is_kw("filter")) {
            cond.upper_limit = g[0].is_kw("upperlimit");
            cond.as_filter = g[0].is_kw("filter");
            cond.value = tokens_text(&g[2..g.len() - 1]);
        } else if cond.kind == CondKind::Const && g.len() == 1 {
            cond.value = g[0].text.to_string();
        } else if cond.kind == CondKind::Filter {
            cond.value = filter_text(g);
        } else {
            cond.value = tokens_text(g);
        }
        out.push(cond);
        if !c.eat(Tk::Comma) {
            break;
        }
    }
    out
}

fn where_clause(c: &mut Cur) -> Vec<Cond> {
    if c.kw("where") {
        c.p += 1;
        if let Some(g) = c.group() {
            return parse_conds(g);
        }
    }
    vec![]
}

pub fn parse_calc_formula(toks: &[Token]) -> Option<CalcFormula> {
    let mut c = Cur { t: toks, p: 0 };
    let reverse_sign = c.eat(Tk::Minus);
    let m = c.name()?.to_ascii_lowercase();
    let method = match m.as_str() {
        "sum" => CalcMethod::Sum,
        "count" => CalcMethod::Count,
        "exist" => CalcMethod::Exist,
        "lookup" => CalcMethod::Lookup,
        "min" => CalcMethod::Min,
        "max" => CalcMethod::Max,
        "average" => CalcMethod::Average,
        _ => return None,
    };
    let inner = c.group()?;
    let mut ic = Cur { t: inner, p: 0 };
    let (table, field) = ic.table_field()?;
    let conds = where_clause(&mut ic);
    Some(CalcFormula { method, reverse_sign, table, field, conds })
}

pub fn parse_table_relation(toks: &[Token]) -> Vec<RelationBranch> {
    let mut out = Vec::new();
    let mut c = Cur { t: toks, p: 0 };
    loop {
        let mut when = vec![];
        if c.kw("if") {
            c.p += 1;
            match c.group() {
                Some(g) => when = parse_conds(g),
                None => break,
            }
        }
        let Some((table, field)) = c.table_field() else { break };
        let conds = where_clause(&mut c);
        out.push(RelationBranch { when, table, field, conds });
        if c.kw("else") {
            c.p += 1;
            continue;
        }
        break;
    }
    out
}

pub fn parse_table_view(toks: &[Token]) -> TableView {
    let mut v = TableView::default();
    let mut c = Cur { t: toks, p: 0 };
    while let Some(t) = c.peek() {
        if t.is_kw("sorting") {
            c.p += 1;
            if let Some(g) = c.group() {
                v.sorting = crate::props::split_commas(g).into_iter().map(tokens_text).collect();
            }
        } else if t.is_kw("order") {
            c.p += 1;
            if let Some(g) = c.group() {
                v.descending = g.first().is_some_and(|t| t.is_kw("descending"));
            }
        } else if t.is_kw("where") {
            v.conds = where_clause(&mut c);
        } else {
            c.p += 1;
        }
    }
    v
}
