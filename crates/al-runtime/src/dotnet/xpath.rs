//! XPath 1.0 over the xml.rs DOM: location paths with all axes except namespace, predicates,
//! unions, comparisons and arithmetic, and the core function library. Prefixes resolve through
//! the namespace manager given by the caller; unprefixed names match no-namespace nodes.

use super::xml::{self, Node, NK};
use std::rc::Rc;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Name(String),
    Star,
    At,
    Slash,
    DSlash,
    Dot,
    DDot,
    LBr,
    RBr,
    LP,
    RP,
    Comma,
    Pipe,
    Axis(String),
    Op(&'static str),
    Str(String),
    Num(f64),
    Var(String),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        // '*' and operator names are operators when they follow an operand
        let after_operand = matches!(out.last(), Some(Tok::Name(_) | Tok::Star | Tok::Dot | Tok::DDot | Tok::RBr | Tok::RP | Tok::Str(_) | Tok::Num(_) | Tok::Var(_)));
        match ch {
            '/' if c.get(i + 1) == Some(&'/') => {
                out.push(Tok::DSlash);
                i += 2;
            }
            '/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            '.' if c.get(i + 1) == Some(&'.') => {
                out.push(Tok::DDot);
                i += 2;
            }
            '.' if !c.get(i + 1).is_some_and(|d| d.is_ascii_digit()) => {
                out.push(Tok::Dot);
                i += 1;
            }
            '@' => {
                out.push(Tok::At);
                i += 1;
            }
            '[' => {
                out.push(Tok::LBr);
                i += 1;
            }
            ']' => {
                out.push(Tok::RBr);
                i += 1;
            }
            '(' => {
                out.push(Tok::LP);
                i += 1;
            }
            ')' => {
                out.push(Tok::RP);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '|' => {
                out.push(Tok::Pipe);
                i += 1;
            }
            '*' if after_operand => {
                out.push(Tok::Op("*"));
                i += 1;
            }
            '*' => {
                out.push(Tok::Star);
                i += 1;
            }
            '=' => {
                out.push(Tok::Op("="));
                i += 1;
            }
            '!' if c.get(i + 1) == Some(&'=') => {
                out.push(Tok::Op("!="));
                i += 2;
            }
            '<' if c.get(i + 1) == Some(&'=') => {
                out.push(Tok::Op("<="));
                i += 2;
            }
            '>' if c.get(i + 1) == Some(&'=') => {
                out.push(Tok::Op(">="));
                i += 2;
            }
            '<' => {
                out.push(Tok::Op("<"));
                i += 1;
            }
            '>' => {
                out.push(Tok::Op(">"));
                i += 1;
            }
            '+' => {
                out.push(Tok::Op("+"));
                i += 1;
            }
            '-' => {
                out.push(Tok::Op("-"));
                i += 1;
            }
            '"' | '\'' => {
                let mut j = i + 1;
                while j < c.len() && c[j] != ch {
                    j += 1;
                }
                if j >= c.len() {
                    return Err(format!("'{s}' has an invalid token."));
                }
                out.push(Tok::Str(c[i + 1..j].iter().collect()));
                i = j + 1;
            }
            '$' => {
                let mut j = i + 1;
                while j < c.len() && (c[j].is_alphanumeric() || "_-.:".contains(c[j])) {
                    j += 1;
                }
                out.push(Tok::Var(c[i + 1..j].iter().collect()));
                i = j;
            }
            d if d.is_ascii_digit() || d == '.' => {
                let mut j = i;
                while j < c.len() && (c[j].is_ascii_digit() || c[j] == '.') {
                    j += 1;
                }
                let t: String = c[i..j].iter().collect();
                out.push(Tok::Num(t.parse().map_err(|_| format!("'{s}' has an invalid token."))?));
                i = j;
            }
            a if a.is_alphabetic() || a == '_' => {
                let mut j = i;
                while j < c.len() && (c[j].is_alphanumeric() || "_-.".contains(c[j]) || (c[j] == ':' && c.get(j + 1) != Some(&':') && c.get(j + 1).is_some_and(|x| x.is_alphabetic() || *x == '_' || *x == '*'))) {
                    if c[j] == ':' && c.get(j + 1) == Some(&'*') {
                        j += 2;
                        break;
                    }
                    j += 1;
                }
                let name: String = c[i..j].iter().collect();
                // axis::
                let mut k = j;
                while k < c.len() && c[k].is_whitespace() {
                    k += 1;
                }
                if c.get(k) == Some(&':') && c.get(k + 1) == Some(&':') {
                    out.push(Tok::Axis(name));
                    i = k + 2;
                    continue;
                }
                if after_operand && matches!(name.as_str(), "and" | "or" | "div" | "mod") {
                    out.push(Tok::Op(match name.as_str() {
                        "and" => "and",
                        "or" => "or",
                        "div" => "div",
                        _ => "mod",
                    }));
                } else {
                    out.push(Tok::Name(name));
                }
                i = j;
            }
            _ => return Err(format!("'{s}' has an invalid token.")),
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
enum Test {
    Name(String, String),
    /// `*` or `prefix:*`
    Any(Option<String>),
    Node,
    Text,
    Comment,
    PI(Option<String>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Axis {
    Child,
    Descendant,
    DescendantOrSelf,
    Self_,
    Parent,
    Ancestor,
    AncestorOrSelf,
    FollowingSibling,
    PrecedingSibling,
    Following,
    Preceding,
    Attribute,
}

#[derive(Debug, Clone)]
struct Step {
    axis: Axis,
    test: Test,
    preds: Vec<Ex>,
}

#[derive(Debug, Clone)]
enum Ex {
    Bin(&'static str, Box<Ex>, Box<Ex>),
    Neg(Box<Ex>),
    Union(Box<Ex>, Box<Ex>),
    /// absolute, steps
    Path(bool, Vec<Step>),
    /// primary expression, predicates, then relative steps
    Filter(Box<Ex>, Vec<Ex>, Vec<Step>),
    Str(String),
    Num(f64),
    Call(String, Vec<Ex>),
    Var(String),
}

struct P {
    t: Vec<Tok>,
    i: usize,
    src: String,
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
    fn err<T>(&self) -> Result<T, String> {
        Err(format!("'{}' has an invalid token.", self.src))
    }
    fn expr(&mut self) -> Result<Ex, String> {
        self.binary(0)
    }
    fn binary(&mut self, level: usize) -> Result<Ex, String> {
        const LEVELS: &[&[&str]] = &[&["or"], &["and"], &["=", "!="], &["<", "<=", ">", ">="], &["+", "-"], &["*", "div", "mod"]];
        if level >= LEVELS.len() {
            return self.unary();
        }
        let mut l = self.binary(level + 1)?;
        while let Some(Tok::Op(op)) = self.peek().cloned() {
            if !LEVELS[level].contains(&op) {
                break;
            }
            self.i += 1;
            let r = self.binary(level + 1)?;
            l = Ex::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<Ex, String> {
        if self.eat(&Tok::Op("-")) {
            return Ok(Ex::Neg(Box::new(self.unary()?)));
        }
        let mut l = self.path()?;
        while self.eat(&Tok::Pipe) {
            let r = self.path()?;
            l = Ex::Union(Box::new(l), Box::new(r));
        }
        Ok(l)
    }
    fn path(&mut self) -> Result<Ex, String> {
        match self.peek() {
            Some(Tok::Slash) => {
                self.i += 1;
                let steps = if self.starts_step() { self.rel_steps()? } else { vec![] };
                Ok(Ex::Path(true, steps))
            }
            Some(Tok::DSlash) => {
                self.i += 1;
                let mut steps = vec![Step { axis: Axis::DescendantOrSelf, test: Test::Node, preds: vec![] }];
                steps.extend(self.rel_steps()?);
                Ok(Ex::Path(true, steps))
            }
            Some(Tok::Str(_)) | Some(Tok::Num(_)) | Some(Tok::LP) | Some(Tok::Var(_)) => self.filter(),
            Some(Tok::Name(n)) if self.t.get(self.i + 1) == Some(&Tok::LP) && !matches!(n.as_str(), "node" | "text" | "comment" | "processing-instruction") => self.filter(),
            _ => Ok(Ex::Path(false, self.rel_steps()?)),
        }
    }
    fn filter(&mut self) -> Result<Ex, String> {
        let prim = match self.next() {
            Some(Tok::Str(s)) => Ex::Str(s),
            Some(Tok::Num(n)) => Ex::Num(n),
            Some(Tok::Var(v)) => Ex::Var(v),
            Some(Tok::LP) => {
                let e = self.expr()?;
                if !self.eat(&Tok::RP) {
                    return self.err();
                }
                e
            }
            Some(Tok::Name(n)) => {
                self.i += 1; // (
                let mut args = vec![];
                if !self.eat(&Tok::RP) {
                    loop {
                        args.push(self.expr()?);
                        if self.eat(&Tok::RP) {
                            break;
                        }
                        if !self.eat(&Tok::Comma) {
                            return self.err();
                        }
                    }
                }
                Ex::Call(n, args)
            }
            _ => return self.err(),
        };
        let mut preds = vec![];
        while self.eat(&Tok::LBr) {
            preds.push(self.expr()?);
            if !self.eat(&Tok::RBr) {
                return self.err();
            }
        }
        let mut steps = vec![];
        loop {
            if self.eat(&Tok::Slash) {
                steps.push(self.step()?);
            } else if self.eat(&Tok::DSlash) {
                steps.push(Step { axis: Axis::DescendantOrSelf, test: Test::Node, preds: vec![] });
                steps.push(self.step()?);
            } else {
                break;
            }
        }
        if preds.is_empty() && steps.is_empty() {
            return Ok(prim);
        }
        Ok(Ex::Filter(Box::new(prim), preds, steps))
    }
    fn starts_step(&self) -> bool {
        matches!(self.peek(), Some(Tok::Name(_) | Tok::Star | Tok::At | Tok::Dot | Tok::DDot | Tok::Axis(_)))
    }
    fn rel_steps(&mut self) -> Result<Vec<Step>, String> {
        let mut steps = vec![self.step()?];
        loop {
            if self.eat(&Tok::Slash) {
                steps.push(self.step()?);
            } else if self.eat(&Tok::DSlash) {
                steps.push(Step { axis: Axis::DescendantOrSelf, test: Test::Node, preds: vec![] });
                steps.push(self.step()?);
            } else {
                break;
            }
        }
        Ok(steps)
    }
    fn step(&mut self) -> Result<Step, String> {
        if self.eat(&Tok::Dot) {
            return Ok(Step { axis: Axis::Self_, test: Test::Node, preds: vec![] });
        }
        if self.eat(&Tok::DDot) {
            return Ok(Step { axis: Axis::Parent, test: Test::Node, preds: vec![] });
        }
        let mut axis = Axis::Child;
        if self.eat(&Tok::At) {
            axis = Axis::Attribute;
        } else if let Some(Tok::Axis(a)) = self.peek().cloned() {
            self.i += 1;
            axis = match a.as_str() {
                "child" => Axis::Child,
                "descendant" => Axis::Descendant,
                "descendant-or-self" => Axis::DescendantOrSelf,
                "self" => Axis::Self_,
                "parent" => Axis::Parent,
                "ancestor" => Axis::Ancestor,
                "ancestor-or-self" => Axis::AncestorOrSelf,
                "following-sibling" => Axis::FollowingSibling,
                "preceding-sibling" => Axis::PrecedingSibling,
                "following" => Axis::Following,
                "preceding" => Axis::Preceding,
                "attribute" => Axis::Attribute,
                _ => return self.err(),
            };
        }
        let test = match self.next() {
            Some(Tok::Star) => Test::Any(None),
            Some(Tok::Name(n)) => {
                if self.peek() == Some(&Tok::LP) && matches!(n.as_str(), "node" | "text" | "comment" | "processing-instruction") {
                    self.i += 1;
                    let arg = match self.peek().cloned() {
                        Some(Tok::Str(s)) => {
                            self.i += 1;
                            Some(s)
                        }
                        _ => None,
                    };
                    if !self.eat(&Tok::RP) {
                        return self.err();
                    }
                    match n.as_str() {
                        "node" => Test::Node,
                        "text" => Test::Text,
                        "comment" => Test::Comment,
                        _ => Test::PI(arg),
                    }
                } else if let Some(p) = n.strip_suffix(":*") {
                    Test::Any(Some(p.to_string()))
                } else {
                    match n.split_once(':') {
                        Some((p, l)) => Test::Name(p.into(), l.into()),
                        None => Test::Name(String::new(), n),
                    }
                }
            }
            _ => return self.err(),
        };
        let mut preds = vec![];
        while self.eat(&Tok::LBr) {
            preds.push(self.expr()?);
            if !self.eat(&Tok::RBr) {
                return self.err();
            }
        }
        Ok(Step { axis, test, preds })
    }
}

#[derive(Clone)]
pub enum XV {
    Nodes(Vec<Node>),
    Str(String),
    Num(f64),
    Bool(bool),
}

struct Ctx<'a> {
    ns: &'a [(String, String)],
    root: Node,
    order: std::collections::HashMap<*const std::cell::RefCell<xml::NodeData>, usize>,
}

fn string_value(n: &Node) -> String {
    xml::inner_text(n)
}

fn to_str(v: &XV) -> String {
    match v {
        XV::Nodes(n) => n.first().map(string_value).unwrap_or_default(),
        XV::Str(s) => s.clone(),
        XV::Num(n) => num_to_str(*n),
        XV::Bool(b) => b.to_string(),
    }
}

fn num_to_str(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn to_num(v: &XV) -> f64 {
    match v {
        XV::Num(n) => *n,
        XV::Bool(b) => *b as i32 as f64,
        other => to_str(other).trim().parse().unwrap_or(f64::NAN),
    }
}

fn to_bool(v: &XV) -> bool {
    match v {
        XV::Nodes(n) => !n.is_empty(),
        XV::Str(s) => !s.is_empty(),
        XV::Num(n) => *n != 0.0 && !n.is_nan(),
        XV::Bool(b) => *b,
    }
}

fn is_xpath_child(n: &Node) -> bool {
    !matches!(xml::kind(n), NK::Declaration | NK::DocType)
}

impl Ctx<'_> {
    fn resolve(&self, prefix: &str) -> Result<String, String> {
        if prefix.is_empty() {
            return Ok(String::new());
        }
        if prefix == "xml" {
            return Ok(xml::XML_NS.into());
        }
        self.ns
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u.clone())
            .ok_or_else(|| "Namespace Manager or XsltContext needed. This query has a prefix, variable, or user-defined function.".to_string())
    }

    fn matches(&self, n: &Node, test: &Test, axis: Axis) -> Result<bool, String> {
        let k = xml::kind(n);
        let principal = if axis == Axis::Attribute { NK::Attribute } else { NK::Element };
        Ok(match test {
            Test::Node => true,
            Test::Text => matches!(k, NK::Text | NK::CData | NK::Whitespace),
            Test::Comment => k == NK::Comment,
            Test::PI(t) => k == NK::PI && t.as_ref().is_none_or(|t| n.borrow().local == *t),
            Test::Any(None) => k == principal && !(k == NK::Attribute && n.borrow().ns == xml::XMLNS),
            Test::Any(Some(p)) => k == principal && n.borrow().ns == self.resolve(p)?,
            Test::Name(p, l) => {
                if k != principal {
                    return Ok(false);
                }
                let b = n.borrow();
                if b.local != *l {
                    return Ok(false);
                }
                if k == NK::Attribute && b.ns == xml::XMLNS {
                    return Ok(false);
                }
                b.ns == self.resolve(p)?
            }
        })
    }

    fn axis_nodes(&self, n: &Node, axis: Axis) -> Vec<Node> {
        let mut out = vec![];
        match axis {
            Axis::Child => out = xml::children(n).into_iter().filter(is_xpath_child).collect(),
            Axis::Descendant | Axis::DescendantOrSelf => {
                if axis == Axis::DescendantOrSelf {
                    out.push(n.clone());
                }
                let mut d = vec![];
                xml::descendants(n, &mut d);
                out.extend(d.into_iter().filter(is_xpath_child));
            }
            Axis::Self_ => out.push(n.clone()),
            Axis::Parent => {
                if let Some(p) = xml::parent(n) {
                    out.push(p);
                }
            }
            Axis::Ancestor | Axis::AncestorOrSelf => {
                if axis == Axis::AncestorOrSelf {
                    out.push(n.clone());
                }
                let mut cur = xml::parent(n);
                while let Some(c) = cur {
                    out.push(c.clone());
                    cur = xml::parent(&c);
                }
            }
            Axis::FollowingSibling | Axis::PrecedingSibling => {
                if xml::kind(n) == NK::Attribute {
                    return out;
                }
                if let Some(p) = xml::parent(n) {
                    let sibs: Vec<Node> = xml::children(&p).into_iter().filter(is_xpath_child).collect();
                    if let Some(i) = sibs.iter().position(|x| Rc::ptr_eq(x, n)) {
                        if axis == Axis::FollowingSibling {
                            out.extend(sibs[i + 1..].iter().cloned());
                        } else {
                            out.extend(sibs[..i].iter().rev().cloned());
                        }
                    }
                }
            }
            Axis::Following | Axis::Preceding => {
                let mut all = vec![];
                xml::descendants(&self.root, &mut all);
                let all: Vec<Node> = all.into_iter().filter(is_xpath_child).collect();
                let pos = |x: &Node| all.iter().position(|y| Rc::ptr_eq(x, y));
                let anchor = if xml::kind(n) == NK::Attribute { xml::parent(n).unwrap_or(n.clone()) } else { n.clone() };
                if let Some(i) = pos(&anchor) {
                    if axis == Axis::Following {
                        let mut skip = vec![];
                        xml::descendants(&anchor, &mut skip);
                        out.extend(all[i + 1..].iter().filter(|x| !skip.iter().any(|s| Rc::ptr_eq(s, x))).cloned());
                    } else {
                        let mut anc = vec![];
                        let mut cur = xml::parent(&anchor);
                        while let Some(c) = cur {
                            anc.push(c.clone());
                            cur = xml::parent(&c);
                        }
                        out.extend(all[..i].iter().rev().filter(|x| !anc.iter().any(|a| Rc::ptr_eq(a, x))).cloned());
                    }
                }
            }
            Axis::Attribute => {
                if xml::kind(n) == NK::Element {
                    out = xml::attributes(n);
                }
            }
        }
        out
    }

    fn doc_order(&mut self, nodes: &mut Vec<Node>) {
        if self.order.is_empty() {
            let mut all = vec![self.root.clone()];
            fn walk(n: &Node, out: &mut Vec<Node>) {
                for a in xml::attributes(n) {
                    out.push(a);
                }
                for c in xml::children(n) {
                    out.push(c.clone());
                    walk(&c, out);
                }
            }
            walk(&self.root, &mut all);
            for (i, n) in all.iter().enumerate() {
                self.order.insert(Rc::as_ptr(n), i);
            }
        }
        let mut seen = std::collections::HashSet::new();
        nodes.retain(|n| seen.insert(Rc::as_ptr(n)));
        nodes.sort_by_key(|n| self.order.get(&Rc::as_ptr(n)).copied().unwrap_or(usize::MAX));
    }

    fn steps(&mut self, start: Vec<Node>, steps: &[Step]) -> Result<Vec<Node>, String> {
        let mut cur = start;
        for st in steps {
            let mut next = vec![];
            for n in &cur {
                let mut cands = vec![];
                for x in self.axis_nodes(n, st.axis) {
                    if self.matches(&x, &st.test, st.axis)? {
                        cands.push(x);
                    }
                }
                for p in &st.preds {
                    cands = self.filter_pred(cands, p)?;
                }
                next.extend(cands);
            }
            self.doc_order(&mut next);
            cur = next;
        }
        Ok(cur)
    }

    fn filter_pred(&mut self, nodes: Vec<Node>, p: &Ex) -> Result<Vec<Node>, String> {
        let size = nodes.len();
        let mut out = vec![];
        for (i, n) in nodes.into_iter().enumerate() {
            let v = self.eval(p, &n, i + 1, size)?;
            let keep = match v {
                XV::Num(k) => (k - (i + 1) as f64).abs() < f64::EPSILON,
                other => to_bool(&other),
            };
            if keep {
                out.push(n);
            }
        }
        Ok(out)
    }

    fn eval(&mut self, e: &Ex, n: &Node, pos: usize, size: usize) -> Result<XV, String> {
        Ok(match e {
            Ex::Str(s) => XV::Str(s.clone()),
            Ex::Num(x) => XV::Num(*x),
            Ex::Var(v) => return Err(format!("XsltContext is needed for this query because of an unknown function or variable ${v}.")),
            Ex::Neg(x) => XV::Num(-to_num(&self.eval(x, n, pos, size)?)),
            Ex::Path(abs, steps) => {
                let start = if *abs { vec![self.root.clone()] } else { vec![n.clone()] };
                XV::Nodes(self.steps(start, steps)?)
            }
            Ex::Filter(prim, preds, steps) => {
                let v = self.eval(prim, n, pos, size)?;
                let XV::Nodes(mut nodes) = v else {
                    if preds.is_empty() && steps.is_empty() {
                        return Ok(v);
                    }
                    return Err("Expression must evaluate to a node-set.".into());
                };
                for p in preds {
                    nodes = self.filter_pred(nodes, p)?;
                }
                XV::Nodes(self.steps(nodes, steps)?)
            }
            Ex::Union(a, b) => {
                let (XV::Nodes(mut x), XV::Nodes(y)) = (self.eval(a, n, pos, size)?, self.eval(b, n, pos, size)?) else {
                    return Err("Expression must evaluate to a node-set.".into());
                };
                x.extend(y);
                self.doc_order(&mut x);
                XV::Nodes(x)
            }
            Ex::Bin(op, a, b) => {
                if *op == "and" {
                    return Ok(XV::Bool(to_bool(&self.eval(a, n, pos, size)?) && to_bool(&self.eval(b, n, pos, size)?)));
                }
                if *op == "or" {
                    return Ok(XV::Bool(to_bool(&self.eval(a, n, pos, size)?) || to_bool(&self.eval(b, n, pos, size)?)));
                }
                let (x, y) = (self.eval(a, n, pos, size)?, self.eval(b, n, pos, size)?);
                match *op {
                    "+" => XV::Num(to_num(&x) + to_num(&y)),
                    "-" => XV::Num(to_num(&x) - to_num(&y)),
                    "*" => XV::Num(to_num(&x) * to_num(&y)),
                    "div" => XV::Num(to_num(&x) / to_num(&y)),
                    "mod" => XV::Num(to_num(&x) % to_num(&y)),
                    cmp => XV::Bool(compare(cmp, &x, &y)),
                }
            }
            Ex::Call(name, args) => self.call(name, args, n, pos, size)?,
        })
    }

    fn call(&mut self, name: &str, args: &[Ex], n: &Node, pos: usize, size: usize) -> Result<XV, String> {
        let mut v = vec![];
        for a in args {
            v.push(self.eval(a, n, pos, size)?);
        }
        let s = |i: usize| -> String { v.get(i).map(to_str).unwrap_or_else(|| string_value(n)) };
        let node_arg = |i: usize| -> Option<Node> {
            match v.get(i) {
                Some(XV::Nodes(ns)) => ns.first().cloned(),
                Some(_) => None,
                None => Some(n.clone()),
            }
        };
        Ok(match name {
            "last" => XV::Num(size as f64),
            "position" => XV::Num(pos as f64),
            "count" => XV::Num(match v.first() {
                Some(XV::Nodes(x)) => x.len() as f64,
                _ => return Err("Expression must evaluate to a node-set.".into()),
            }),
            "local-name" => XV::Str(node_arg(0).map(|x| if matches!(xml::kind(&x), NK::Element | NK::Attribute | NK::PI) { x.borrow().local.clone() } else { String::new() }).unwrap_or_default()),
            "name" => XV::Str(node_arg(0).map(|x| if matches!(xml::kind(&x), NK::Element | NK::Attribute | NK::PI) { xml::qname(&x) } else { String::new() }).unwrap_or_default()),
            "namespace-uri" => XV::Str(node_arg(0).map(|x| x.borrow().ns.clone()).unwrap_or_default()),
            "string" => XV::Str(s(0)),
            "concat" => XV::Str(v.iter().map(to_str).collect()),
            "starts-with" => XV::Bool(s(0).starts_with(&s(1))),
            "ends-with" => XV::Bool(s(0).ends_with(&s(1))),
            "contains" => XV::Bool(s(0).contains(&s(1))),
            "substring-before" => XV::Str(s(0).split_once(&s(1)).map(|x| x.0.to_string()).unwrap_or_default()),
            "substring-after" => XV::Str(s(0).split_once(&s(1)).map(|x| x.1.to_string()).unwrap_or_default()),
            "substring" => {
                let st = s(0);
                let start = to_num(&v[1]).round();
                let len = v.get(2).map(|x| to_num(x).round()).unwrap_or(f64::INFINITY);
                XV::Str(st.chars().enumerate().filter(|(i, _)| {
                    let p = (*i + 1) as f64;
                    p >= start && p < start + len
                }).map(|(_, c)| c).collect())
            }
            "string-length" => XV::Num(s(0).chars().count() as f64),
            "normalize-space" => XV::Str(s(0).split_whitespace().collect::<Vec<_>>().join(" ")),
            "translate" => {
                let (from, to): (Vec<char>, Vec<char>) = (s(1).chars().collect(), s(2).chars().collect());
                XV::Str(s(0).chars().filter_map(|c| match from.iter().position(|f| *f == c) {
                    Some(i) => to.get(i).copied(),
                    None => Some(c),
                }).collect())
            }
            "upper-case" => XV::Str(s(0).to_uppercase()),
            "lower-case" => XV::Str(s(0).to_lowercase()),
            "not" => XV::Bool(!v.first().map(to_bool).unwrap_or(false)),
            "true" => XV::Bool(true),
            "false" => XV::Bool(false),
            "boolean" => XV::Bool(v.first().map(to_bool).unwrap_or(false)),
            "number" => XV::Num(v.first().map(to_num).unwrap_or_else(|| string_value(n).trim().parse().unwrap_or(f64::NAN))),
            "sum" => XV::Num(match v.first() {
                Some(XV::Nodes(x)) => x.iter().map(|y| string_value(y).trim().parse::<f64>().unwrap_or(f64::NAN)).sum(),
                _ => return Err("Expression must evaluate to a node-set.".into()),
            }),
            "floor" => XV::Num(to_num(&v[0]).floor()),
            "ceiling" => XV::Num(to_num(&v[0]).ceil()),
            "round" => XV::Num((to_num(&v[0]) + 0.5).floor()),
            "lang" => XV::Bool(false),
            "id" => XV::Nodes(vec![]),
            other => return Err(format!("'{other}()' is an unknown XSLT function.")),
        })
    }
}

fn compare(op: &str, x: &XV, y: &XV) -> bool {
    let cmp_atoms = |a: &XV, b: &XV| -> bool {
        match op {
            "=" | "!=" => {
                let eq = match (a, b) {
                    (XV::Bool(_), _) | (_, XV::Bool(_)) => to_bool(a) == to_bool(b),
                    (XV::Num(_), _) | (_, XV::Num(_)) => to_num(a) == to_num(b),
                    _ => to_str(a) == to_str(b),
                };
                if op == "=" {
                    eq
                } else {
                    !eq
                }
            }
            _ => {
                let (p, q) = (to_num(a), to_num(b));
                match op {
                    "<" => p < q,
                    "<=" => p <= q,
                    ">" => p > q,
                    _ => p >= q,
                }
            }
        }
    };
    match (x, y) {
        (XV::Nodes(a), XV::Nodes(b)) => a.iter().any(|p| b.iter().any(|q| cmp_atoms(&XV::Str(string_value(p)), &XV::Str(string_value(q))))),
        (XV::Nodes(a), XV::Bool(_)) => cmp_atoms(&XV::Bool(!a.is_empty()), y),
        (XV::Bool(_), XV::Nodes(b)) => cmp_atoms(x, &XV::Bool(!b.is_empty())),
        (XV::Nodes(a), other) => a.iter().any(|p| {
            let sv = XV::Str(string_value(p));
            let sv = if matches!(other, XV::Num(_)) { XV::Num(to_num(&sv)) } else { sv };
            cmp_atoms(&sv, other)
        }),
        (other, XV::Nodes(b)) => b.iter().any(|q| {
            let sv = XV::Str(string_value(q));
            let sv = if matches!(other, XV::Num(_)) { XV::Num(to_num(&sv)) } else { sv };
            cmp_atoms(other, &sv)
        }),
        _ => cmp_atoms(x, y),
    }
}

fn root_of(n: &Node) -> Node {
    let mut cur = n.clone();
    while let Some(p) = xml::parent(&cur) {
        cur = p;
    }
    cur
}

pub fn evaluate(ctx: &Node, xpath: &str, ns: &[(String, String)]) -> Result<XV, String> {
    let toks = lex(xpath)?;
    if toks.is_empty() {
        return Err("Expression must evaluate to a node-set.".into());
    }
    let mut p = P { t: toks, i: 0, src: xpath.to_string() };
    let e = p.expr()?;
    if p.i != p.t.len() {
        return Err(format!("'{xpath}' has an invalid token."));
    }
    let mut c = Ctx { ns, root: root_of(ctx), order: Default::default() };
    c.eval(&e, ctx, 1, 1)
}

/// SelectNodes: the nodes an expression selects, in document order.
pub fn select(ctx: &Node, xpath: &str, ns: &[(String, String)]) -> Result<Vec<Node>, String> {
    match evaluate(ctx, xpath, ns)? {
        XV::Nodes(n) => Ok(n),
        _ => Err("Expression must evaluate to a node-set.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dotnet::xml::{parse, ParseOptions};

    #[test]
    fn paths_and_predicates() {
        let d = parse(r#"<r xmlns:n="urn:n"><a id="1">x</a><a id="2"><b>y</b></a><n:c>z</n:c></r>"#, &ParseOptions { preserve_whitespace: false }).unwrap();
        let ns = vec![("p".to_string(), "urn:n".to_string())];
        assert_eq!(select(&d, "/r/a", &ns).unwrap().len(), 2);
        assert_eq!(select(&d, "//a[@id='2']/b", &ns).unwrap().len(), 1);
        assert_eq!(select(&d, "//a[2]", &ns).unwrap().len(), 1);
        assert_eq!(xml::inner_text(&select(&d, "//p:c", &ns).unwrap()[0]), "z");
        assert_eq!(select(&d, "count(//a)", &ns).err().is_some(), true);
        assert!(matches!(evaluate(&d, "count(//a) = 2", &ns).unwrap(), XV::Bool(true)));
        assert_eq!(select(&d, "//*[local-name()='c']", &ns).unwrap().len(), 1);
        assert_eq!(select(&d, "/r/a/@id", &ns).unwrap().len(), 2);
        assert_eq!(select(&d, "//a[last()]/@id", &ns).unwrap()[0].borrow().value, "2");
        assert!(select(&d, "//q:c", &ns).is_err());
    }
}
