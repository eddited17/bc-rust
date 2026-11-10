//! RDLC binding (stage 3 of the report layout engine; see docs/reports.md): the layout's data
//! regions over the dataset rows, the way ReportViewer's processing does it.
//!
//! - A tablix expands its row hierarchy: a static member once, a group once per distinct value of
//!   its group expressions (in the order the values first appear), the details group once per row;
//!   group and data region filters drop instances/rows first. Leaf members map, in document order,
//!   to the tablix's body rows. A member whose Hidden expression is true for an instance drops it.
//! - Data regions nested in a cell (a rectangle in a list) bind to the cell's scope rows.
//! - Expressions see the current row (the first row of the scope), aggregates over the scope or a
//!   named enclosing group, the report parameters and the layout's standard custom code (BC's
//!   `<Code>` boilerplate: BlankZero & co, SetData/GetData, IsNewPage, GetGroupPageNumber).
//!
//! The output is a tree of bound nodes with the layout's geometry, which the page layout stage
//! positions. Errors (an unsupported function, an unknown field) are returned so the caller can fall
//! back to its previous output instead of guessing.

use crate::rdlc::{Filter, Item, Kind, Layout, Member, Tablix};
use crate::rdlc_expr::{self as ex, Ctx, Expr, V};
use std::collections::HashMap;

/// The report's dataset as the layout sees it: column names and the rows' values.
#[derive(Debug, Default, Clone)]
pub struct Dataset {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<V>>,
    /// Each column's AL type class (Unknown when the run didn't say).
    pub types: Vec<ColType>,
    index: HashMap<String, usize>,
}

impl Dataset {
    pub fn new(columns: Vec<String>, rows: Vec<Vec<V>>) -> Dataset {
        let index = columns.iter().enumerate().map(|(i, c)| (c.to_ascii_lowercase(), i)).collect();
        let types = vec![ColType::Unknown; columns.len()];
        Dataset { columns, rows, types, index }
    }
    /// The columns' AL type classes, by column name (case-insensitive).
    pub fn with_types(mut self, types: &[(String, ColType)]) -> Dataset {
        for (n, t) in types {
            if let Some(&i) = self.index.get(&n.to_ascii_lowercase()) {
                self.types[i] = *t;
            }
        }
        self
    }
    pub fn value(&self, row: usize, column: &str) -> Option<V> {
        let i = *self.index.get(&column.to_ascii_lowercase())?;
        Some(self.rows.get(row).and_then(|r| r.get(i)).cloned().unwrap_or(V::Null))
    }
    pub fn column_type(&self, column: &str) -> ColType {
        self.index.get(&column.to_ascii_lowercase()).and_then(|&i| self.types.get(i)).copied().unwrap_or_default()
    }
}

/// The AL type class of a dataset column, as far as the Excel export needs it: whether a value
/// is a number, and whether an integral one (spec R-KIND, R-CELL-1).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ColType {
    #[default]
    Unknown,
    /// Code, Text, Option and Enum captions, Guid, ... (the DataSet column is a string)
    Text,
    /// Integer, BigInteger
    Integral,
    /// Decimal
    Fractional,
    /// Boolean, Date, Time, DateTime
    Other,
}

/// What a textbox value is, by the dataset columns its Value expression read: R-KIND (Integral
/// only for integral AL types or an integer expression, else Fractional) and R-CELL-1 (a value
/// of an AL text type is not a number).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ValType {
    /// Not traced to a column (a literal, a parameter, custom code state, mixed sources).
    #[default]
    Unknown,
    Text,
    Integral,
    Fractional,
}

/// An evaluated Language (Style.Language or Report.Language) and whether it is an assumption
/// (the textbox has no Language anywhere: O-2).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Lang {
    pub name: String,
    pub assumed: bool,
}

/// The fields a textbox Value expression read: their types, and (up to [MAX_READS]) their values.
#[derive(Debug, Default)]
pub struct Reads {
    types: Vec<ColType>,
    values: Vec<(ColType, String)>,
}

const MAX_READS: usize = 64;

impl Reads {
    fn add(&mut self, t: ColType, v: &V) {
        if !self.types.contains(&t) {
            self.types.push(t);
        }
        if self.values.len() < MAX_READS {
            self.values.push((t, v.as_text()));
        }
    }
}

/// An expression whose value is an integer in RDL: counts, row numbers, page numbers, CInt/CLng.
fn integer_expr(e: &Expr) -> bool {
    match e {
        Expr::Call(n, _) => matches!(n.as_str(), "count" | "countrows" | "countdistinct" | "rownumber" | "cint" | "clng" | "convert.toint32" | "convert.toint64"),
        Expr::Global(n) => matches!(n.to_ascii_lowercase().as_str(), "pagenumber" | "totalpages" | "overallpagenumber" | "overalltotalpages"),
        _ => false,
    }
}

/// The type of a textbox value `v` computed by `expr`, which read `reads`.
pub fn val_type(v: &V, reads: &Reads, expr: Option<&Expr>) -> ValType {
    let of = |types: &mut dyn Iterator<Item = ColType>| -> ValType {
        let types: Vec<ColType> = types.collect();
        if types.is_empty() {
            return ValType::Unknown;
        }
        if types.iter().all(|t| *t == ColType::Text) {
            return ValType::Text;
        }
        if types.iter().all(|t| *t == ColType::Integral) {
            return ValType::Integral;
        }
        if types.iter().all(|t| matches!(t, ColType::Integral | ColType::Fractional)) {
            return ValType::Fractional;
        }
        ValType::Unknown
    };
    match v {
        V::Num(_) if expr.is_some_and(integer_expr) => ValType::Integral,
        // a computed number is never text
        V::Num(_) => match of(&mut reads.types.iter().copied()) {
            ValType::Integral => ValType::Integral,
            _ => ValType::Fractional,
        },
        V::Text(s) => {
            // the column(s) the value came from: those whose value it is, else all it read
            let mut same = reads.values.iter().filter(|(_, x)| x == s).map(|(t, _)| *t).peekable();
            if same.peek().is_some() {
                of(&mut same)
            } else {
                of(&mut reads.types.iter().copied())
            }
        }
        _ => ValType::Unknown,
    }
}

/// A bound item: the layout item's geometry (mm, relative to its container) and its content.
#[derive(Debug, Clone)]
pub struct BNode {
    pub name: String,
    pub top: f64,
    pub left: f64,
    pub width: f64,
    pub height: f64,
    pub kind: BKind,
}

#[derive(Debug, Clone)]
pub enum BKind {
    /// A textbox: its value, its Format (as evaluated, not trimmed), its Language (TextRun or
    /// Textbox level; None: the report's) and what the value is.
    Text { value: V, format: Option<String>, language: Option<Lang>, vtype: ValType },
    /// A rectangle (or a list cell's contents): its children.
    Box(Vec<BNode>),
    /// A tablix: its column widths and its row instances.
    Rows { columns: Vec<f64>, rows: Vec<BRow> },
    Image,
    Line,
}

#[derive(Debug, Clone)]
pub struct BRow {
    /// The body row this instance repeats.
    pub body_row: usize,
    /// A group's page break comes before this row: it starts a new page (a new worksheet).
    pub page_break_before: bool,
    pub height: f64,
    pub cells: Vec<BCell>,
}

#[derive(Debug, Clone)]
pub struct BCell {
    /// First column (index into the tablix's columns) and span.
    pub col: usize,
    pub col_span: usize,
    pub content: Option<BNode>,
}

/// Shared state of one report run: report parameters, the custom code's shared variables, the
/// functions the layout's `<Code>` block declares.
#[derive(Debug, Default)]
pub struct RunState {
    pub params: HashMap<String, V>,
    pub code_functions: Vec<String>,
    data: [V; 4],
    pictures: [V; 4],
    current_groups: [V; 4],
    page_offset: f64,
    pub user_id: String,
    /// What `User!Language` evaluates to: the report's language tag (T4 probe P3).
    pub user_language: String,
    /// While a textbox Value is evaluated: the fields it reads.
    reads: Option<Reads>,
    /// The layout's <Code> as a VB module (rdlc_vb), for the functions that aren't BC's standard
    /// ones; None when the block doesn't parse (those calls then fail and the report falls back).
    pub vb: Option<crate::rdlc_vb::VbState>,
    /// A group asked for a page break before the next row.
    pending_break: bool,
    /// SetData's state at the end of each page of the body (the page header of page n reads it).
    page_states: Vec<[V; 4]>,
    /// The page the page header is rendered for (0-based) and the page count.
    page: usize,
    pages: usize,
}

impl RunState {
    /// Before rendering page `page` of `pages`'s header: SetData's state as that page's body left it.
    pub fn set_page(&mut self, page: usize, pages: usize) {
        if let Some(d) = self.page_states.get(page).or(self.page_states.last()) {
            self.data = d.clone();
        }
        self.page = page;
        self.pages = pages;
    }

    /// The function names the layout's custom code declares (`Public Function Name(`).
    pub fn with_code(code: &str) -> RunState {
        let mut fns = vec![];
        for line in code.lines() {
            let l = line.trim_start();
            let lower = l.to_ascii_lowercase();
            if let Some(rest) = lower.strip_prefix("public function ").or_else(|| lower.strip_prefix("public shared function ")) {
                if let Some(name) = rest.split('(').next() {
                    fns.push(name.trim().to_string());
                }
            }
        }
        let vb = crate::rdlc_vb::VbState::new(code).ok();
        RunState { code_functions: fns, vb, ..Default::default() }
    }
}

/// The expression context of one scope.
struct Scope<'a> {
    ds: &'a Dataset,
    /// The rows in scope; the current row is the first.
    rows: &'a [usize],
    /// Enclosing named scopes (group / data region names) for scoped aggregates, innermost last.
    named: &'a [(String, Vec<usize>)],
    st: &'a mut RunState,
    items: &'a HashMap<String, V>,
}

impl Ctx for Scope<'_> {
    fn field(&mut self, name: &str) -> Option<V> {
        // BC's layouts only reference fields of their DataSet; a column without rows (its data item
        // wrote none) reads as Nothing
        let v = self.rows.first().and_then(|&r| self.ds.value(r, name)).unwrap_or(V::Null);
        if let Some(reads) = &mut self.st.reads {
            reads.add(self.ds.column_type(name), &v);
        }
        Some(v)
    }
    fn param(&mut self, name: &str) -> V {
        self.st.params.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or(V::Null)
    }
    fn report_item(&mut self, name: &str) -> V {
        self.items.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone()).unwrap_or(V::Null)
    }
    fn global(&mut self, name: &str) -> V {
        match name.to_ascii_lowercase().as_str() {
            "pagenumber" | "overallpagenumber" => V::Num((self.st.page + 1) as f64),
            "totalpages" | "overalltotalpages" => V::Num(self.st.pages.max(1) as f64),
            "userid" => V::Text(self.st.user_id.clone()),
            // User!Language
            "language" => V::Text(self.st.user_language.clone()),
            _ => V::Null,
        }
    }
    fn aggregate(&mut self, func: &str, arg: Option<&Expr>, scope: Option<&str>) -> Result<V, String> {
        let rows: Vec<usize> = match scope {
            Some(n) => match self.named.iter().rev().find(|(k, _)| k.eq_ignore_ascii_case(n)) {
                Some((_, r)) => r.clone(),
                None => self.rows.to_vec(),
            },
            None => self.rows.to_vec(),
        };
        if func == "countrows" {
            return Ok(V::Num(rows.len() as f64));
        }
        if func == "rownumber" {
            return Ok(V::Num(1.0));
        }
        let Some(arg) = arg else { return Err(format!("{func}: missing argument")) };
        let mut vals = vec![];
        for r in &rows {
            let one = [*r];
            let mut sub = Scope { ds: self.ds, rows: &one, named: self.named, st: &mut *self.st, items: self.items };
            vals.push(ex::eval(arg, &mut sub)?);
        }
        // Max/Min of Booleans compare False < True (VB's numeric True is -1)
        let bools = !vals.is_empty() && vals.iter().all(|v| matches!(v, V::Bool(_)));
        Ok(match func {
            "max" if bools => V::Bool(vals.iter().any(|v| v.as_bool())),
            "min" if bools => V::Bool(vals.iter().all(|v| v.as_bool())),
            "first" => vals.first().cloned().unwrap_or(V::Null),
            "last" => vals.last().cloned().unwrap_or(V::Null),
            "sum" | "runningvalue" => V::Num(vals.iter().map(|v| v.as_num()).sum()),
            "count" => V::Num(vals.iter().filter(|v| !v.is_null()).count() as f64),
            "countdistinct" => {
                let mut seen: Vec<String> = vals.iter().filter(|v| !v.is_null()).map(|v| v.as_text()).collect();
                seen.sort();
                seen.dedup();
                V::Num(seen.len() as f64)
            }
            "max" => vals.iter().map(|v| v.as_num()).fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.max(x)))).map(V::Num).unwrap_or(V::Null),
            "min" => vals.iter().map(|v| v.as_num()).fold(None, |m: Option<f64>, x| Some(m.map_or(x, |m| m.min(x)))).map(V::Num).unwrap_or(V::Null),
            "avg" if !vals.is_empty() => V::Num(vals.iter().map(|v| v.as_num()).sum::<f64>() / vals.len() as f64),
            "avg" => V::Null,
            _ => return Err(format!("unsupported aggregate {func}")),
        })
    }
    fn code(&mut self, name: &str, args: Vec<V>) -> Result<V, String> {
        let lname = name.to_ascii_lowercase();
        let native = crate::rdlc_vb::NATIVE.contains(&lname.as_str());
        if !native {
            // the layout's own functions: interpreted (the module is taken out while it runs, so
            // its calls of the standard functions come back here)
            let Some(mut vb) = self.st.vb.take() else {
                return Err(format!("Code.{name}: the layout's <Code> isn't supported"));
            };
            let r = if vb.has(&lname) { vb.call(&lname, args, self) } else { Err(format!("Code.{name} is not defined in the layout")) };
            self.st.vb = Some(vb);
            return r;
        }
        if !self.st.code_functions.iter().any(|f| *f == lname) {
            return Err(format!("Code.{name} is not defined in the layout"));
        }
        let a = |i: usize| args.get(i).cloned().unwrap_or(V::Null);
        let group = |v: V| (v.as_num() as usize).clamp(1, 3);
        Ok(match lname.as_str() {
            // BC's standard custom code (the <Code> boilerplate of its RDLC layouts)
            "blankzero" => blank_if(a(0), |n| n == 0.0),
            "blankpos" => blank_if(a(0), |n| n > 0.0),
            "blankzeroandpos" => blank_if(a(0), |n| n >= 0.0),
            "blankneg" => blank_if(a(0), |n| n < 0.0),
            "blanknegandzero" => blank_if(a(0), |n| n <= 0.0),
            "setdata" => {
                let v = a(0);
                if !v.as_text().is_empty() {
                    self.st.data[group(a(1))] = v;
                }
                V::Bool(true)
            }
            "getdata" => {
                let num = a(0).as_num() as usize;
                let data = self.st.data[group(a(1))].as_text();
                V::Text(data.split('\u{b1}').nth(num.saturating_sub(1)).unwrap_or_default().to_string())
            }
            "setpicture" => {
                self.st.pictures[group(a(1))] = a(0);
                V::Bool(true)
            }
            "getpicture" => self.st.pictures[group(a(0))].clone(),
            "isnewpage" => {
                let g = [a(0), a(1), a(2)];
                let mut new_page = false;
                for (k, v) in g.iter().enumerate() {
                    if new_page || *v != self.st.current_groups[k + 1] {
                        if !new_page && *v != self.st.current_groups[k + 1] {
                            new_page = true;
                        }
                        self.st.current_groups[k + 1] = v.clone();
                    }
                }
                V::Bool(new_page)
            }
            "getgrouppagenumber" => {
                let page = a(1).as_num();
                if a(0).as_bool() {
                    self.st.page_offset = page - 1.0;
                }
                V::Num(page - self.st.page_offset)
            }
            _ => return Err(format!("Code.{name} is not a standard BC layout function")),
        })
    }
}

fn blank_if(v: V, f: impl Fn(f64) -> bool) -> V {
    if f(v.as_num()) {
        V::Text(String::new())
    } else {
        v
    }
}

/// Evaluate an expression in a scope.
fn eval_in(src: &str, ds: &Dataset, rows: &[usize], named: &[(String, Vec<usize>)], st: &mut RunState, items: &HashMap<String, V>) -> Result<V, String> {
    let e = ex::parse(src)?;
    let mut cx = Scope { ds, rows, named, st, items };
    ex::eval(&e, &mut cx)
}

fn hidden(h: &Option<String>, ds: &Dataset, rows: &[usize], named: &[(String, Vec<usize>)], st: &mut RunState) -> Result<bool, String> {
    match h.as_deref().map(str::trim) {
        None | Some("") => Ok(false),
        Some(h) if h.eq_ignore_ascii_case("true") => Ok(true),
        Some(h) if h.eq_ignore_ascii_case("false") => Ok(false),
        Some(h) => Ok(eval_in(h, ds, rows, named, st, &HashMap::new())?.as_bool()),
    }
}

fn passes(filters: &[Filter], ds: &Dataset, row: usize, named: &[(String, Vec<usize>)], st: &mut RunState) -> Result<bool, String> {
    for f in filters {
        let v = eval_in(&f.expr, ds, &[row], named, st, &HashMap::new())?;
        let vals: Vec<V> = f.values.iter().map(|x| eval_in(x, ds, &[row], named, st, &HashMap::new())).collect::<Result<_, _>>()?;
        let first = vals.first().cloned().unwrap_or(V::Null);
        let cmp = |a: &V, b: &V| {
            if a.as_text().trim().parse::<f64>().is_ok() && b.as_text().trim().parse::<f64>().is_ok() {
                a.as_num().partial_cmp(&b.as_num()).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                a.as_text().cmp(&b.as_text())
            }
        };
        use std::cmp::Ordering::*;
        let ok = match f.op.to_ascii_lowercase().as_str() {
            "equal" => cmp(&v, &first) == Equal,
            "notequal" => cmp(&v, &first) != Equal,
            "greaterthan" => cmp(&v, &first) == Greater,
            "greaterthanorequal" => cmp(&v, &first) != Less,
            "lessthan" => cmp(&v, &first) == Less,
            "lessthanorequal" => cmp(&v, &first) != Greater,
            "in" => vals.iter().any(|x| cmp(&v, x) == Equal),
            "between" => vals.len() == 2 && cmp(&v, &vals[0]) != Less && cmp(&v, &vals[1]) != Greater,
            other => return Err(format!("unsupported filter operator {other}")),
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Bind a list of items in a scope.
fn bind_items(items: &[Item], ds: &Dataset, rows: &[usize], named: &mut Vec<(String, Vec<usize>)>, st: &mut RunState) -> Result<Vec<BNode>, String> {
    let mut out = vec![];
    let mut values: HashMap<String, V> = HashMap::new();
    for it in items {
        if let Some(n) = bind_item(it, ds, rows, named, st, &mut values)? {
            out.push(n);
        }
    }
    Ok(out)
}

fn bind_item(it: &Item, ds: &Dataset, rows: &[usize], named: &mut Vec<(String, Vec<usize>)>, st: &mut RunState, values: &mut HashMap<String, V>) -> Result<Option<BNode>, String> {
    // A data region's Hidden runs in its own instance: its first row after its filters, and with no
    // rows every field is Nothing (ReportItem.EvaluateStartHidden sets up the data region's scope,
    // DataRegionInstance.SetupEnvironment / ScopeInstance.SetupFields)
    let own: Vec<usize>;
    let vis_rows: &[usize] = match &it.kind {
        Kind::Tablix(t) => {
            let mut v = vec![];
            for &r in rows {
                if passes(&t.filters, ds, r, named, st)? {
                    v.push(r);
                }
            }
            own = v;
            &own[..]
        }
        _ => rows,
    };
    if hidden(&it.hidden, ds, vis_rows, named, st)? {
        // a hidden item isn't rendered, but the expressions inside it still run: BC's layouts call
        // Code.SetData from the Hidden expressions in a hidden table to feed the page header's
        // Code.GetData (Archived Sales Quote's table3)
        let mut scratch = HashMap::new();
        let _ = bind_content(it, ds, rows, named, st, &mut scratch);
        return Ok(None);
    }
    bind_content(it, ds, rows, named, st, values).map(Some)
}

fn bind_content(it: &Item, ds: &Dataset, rows: &[usize], named: &mut Vec<(String, Vec<usize>)>, st: &mut RunState, values: &mut HashMap<String, V>) -> Result<BNode, String> {
    let kind = match &it.kind {
        Kind::Textbox { value, format, language } => {
            let outer = st.reads.replace(Reads::default());
            let v = eval_in(value, ds, rows, named, st, values);
            let reads = std::mem::replace(&mut st.reads, outer).unwrap_or_default();
            let v = v?;
            let vtype = val_type(&v, &reads, ex::parse(value).ok().as_ref());
            values.insert(it.name.clone(), v.clone());
            // Format is often an expression (=Fields!AmountFormat.Value): per instance. A literal
            // Format is used as written: whitespace makes a .NET format string custom (W-TRIM)
            let format = match format.as_deref() {
                Some(f) if f.trim_start().starts_with('=') => eval_in(f.trim(), ds, rows, named, st, values).ok().map(|v| v.as_text()).filter(|f| !f.is_empty()),
                Some(f) if !f.is_empty() => Some(f.to_string()),
                _ => None,
            };
            // Language likewise ([MS-RDL] Style.Language); an empty one leaves the report's
            let language = match language.as_deref().map(str::trim) {
                Some(l) if l.starts_with('=') => {
                    let name = eval_in(l, ds, rows, named, st, values).ok().map(|v| v.as_text());
                    name.filter(|n| !n.trim().is_empty()).map(|name| Lang { name: name.trim().to_string(), assumed: false })
                }
                Some(l) if !l.is_empty() => Some(Lang { name: l.to_string(), assumed: false }),
                _ => None,
            };
            BKind::Text { value: v, format, language, vtype }
        }
        Kind::Rectangle(children) => BKind::Box(bind_items(children, ds, rows, named, st)?),
        Kind::Tablix(t) => BKind::Rows { columns: t.columns.clone(), rows: bind_tablix(&it.name, t, ds, rows, named, st)? },
        Kind::Image => BKind::Image,
        Kind::Line => BKind::Line,
        Kind::Other(k) => return Err(format!("report item {k} is not supported")),
    };
    Ok(BNode { name: it.name.clone(), top: it.top, left: it.left, width: it.width, height: it.height, kind })
}

/// Expand a tablix's row hierarchy over the scope rows.
pub fn bind_tablix(name: &str, t: &Tablix, ds: &Dataset, scope: &[usize], named: &mut Vec<(String, Vec<usize>)>, st: &mut RunState) -> Result<Vec<BRow>, String> {
    let mut rows: Vec<usize> = vec![];
    for &r in scope {
        if passes(&t.filters, ds, r, named, st)? {
            rows.push(r);
        }
    }
    named.push((name.to_string(), rows.clone()));
    let mut out = vec![];
    let mut leaf = 0usize;
    let res = expand(&t.row_members, t, ds, &rows, named, st, &mut leaf, &mut out);
    named.pop();
    res?;
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn expand(members: &[Member], t: &Tablix, ds: &Dataset, rows: &[usize], named: &mut Vec<(String, Vec<usize>)>, st: &mut RunState, leaf: &mut usize, out: &mut Vec<BRow>) -> Result<(), String> {
    for m in members {
        let first_leaf = *leaf;
        let n_leaves = m.leaves();
        // the instances of this member: (scope rows)
        let instances: Vec<Vec<usize>> = match &m.group {
            None => vec![rows.to_vec()],
            Some(g) => {
                let mut kept = vec![];
                for &r in rows {
                    if passes(&g.filters, ds, r, named, st)? {
                        kept.push(r);
                    }
                }
                if g.exprs.is_empty() {
                    kept.into_iter().map(|r| vec![r]).collect()
                } else {
                    let mut keys: Vec<(Vec<String>, Vec<usize>)> = vec![];
                    for r in kept {
                        let key = g.exprs.iter().map(|e| eval_in(e, ds, &[r], named, st, &HashMap::new()).map(|v| v.as_text())).collect::<Result<Vec<_>, _>>()?;
                        match keys.iter_mut().find(|(k, _)| *k == key) {
                            Some((_, v)) => v.push(r),
                            None => keys.push((key, vec![r])),
                        }
                    }
                    keys.into_iter().map(|(_, v)| v).collect()
                }
            }
        };
        // the group's page break (BreakLocation; Disabled per instance)
        let brk = m.group.as_ref().and_then(|g| g.page_break.as_ref());
        let n_inst = instances.len();
        for (i, inst) in instances.iter().enumerate() {
            if let Some((at, disabled)) = brk {
                let off = match disabled {
                    Some(d) => hidden(&Some(d.clone()), ds, inst, named, st)?,
                    None => false,
                };
                if !off && ((at == "between" && i > 0) || ((at == "start" || at == "startandend") && i == 0)) {
                    st.pending_break = true;
                }
            }
            if let Some(g) = &m.group {
                named.push((g.name.clone(), inst.clone()));
            }
            let hide = hidden(&m.hidden, ds, inst, named, st);
            let res = match hide {
                Ok(true) => Ok(()),
                Ok(false) if m.children.is_empty() => bind_row(t, first_leaf, ds, inst, named, st).map(|r| out.push(r)),
                Ok(false) => {
                    let mut l = first_leaf;
                    expand(&m.children, t, ds, inst, named, st, &mut l, out)
                }
                Err(e) => Err(e),
            };
            if m.group.is_some() {
                named.pop();
            }
            res?;
            if let Some((at, _)) = brk {
                if (at == "end" || at == "startandend") && i + 1 == n_inst {
                    st.pending_break = true;
                }
            }
        }
        *leaf = first_leaf + n_leaves;
    }
    Ok(())
}

fn bind_row(t: &Tablix, body_row: usize, ds: &Dataset, rows: &[usize], named: &mut Vec<(String, Vec<usize>)>, st: &mut RunState) -> Result<BRow, String> {
    let Some(row) = t.rows.get(body_row) else { return Err(format!("tablix body row {body_row} missing")) };
    let page_break_before = std::mem::take(&mut st.pending_break);
    if page_break_before {
        st.page_states.push(st.data.clone());
    }
    let mut cells = vec![];
    let mut col = 0usize;
    let mut values: HashMap<String, V> = HashMap::new();
    // RDL writes one TablixCell per column: a cell spanning N columns is followed by N-1 empty
    // placeholder cells, so the cell's index is its column (the running sum only for rows without
    // placeholders)
    let one_per_column = row.cells.len() == t.columns.len();
    for (i, cell) in row.cells.iter().enumerate() {
        if one_per_column {
            col = i;
        }
        let span = cell.col_span.max(1);
        let width: f64 = t.columns.iter().skip(col).take(span).sum();
        let content = match &cell.item {
            Some(it) => bind_item(it, ds, rows, named, st, &mut values)?.map(|mut n| {
                n.width = if n.width > 0.0 { n.width } else { width };
                n.height = if n.height > 0.0 { n.height } else { row.height };
                n
            }),
            None => None,
        };
        cells.push(BCell { col, col_span: span, content });
        col += span;
    }
    Ok(BRow { body_row, page_break_before, height: row.height, cells })
}

/// Bind the layout's body over all dataset rows.
pub fn bind_body(layout: &Layout, ds: &Dataset, st: &mut RunState) -> Result<Vec<BNode>, String> {
    let nodes = bind_section(&layout.body.items, ds, st)?;
    // the last page's SetData state; a break nothing followed doesn't start a page
    st.page_states.push(st.data.clone());
    st.pending_break = false;
    Ok(nodes)
}

/// The report's Language (Report.Language, as written): the culture of textboxes without a
/// Language of their own ([MS-RDL] Style.Language). Without one, the FormatRegion, which "applies
/// to any format operation on ... decimal values" (Learn BC, devenv-report-localization): an
/// assumption (O-2).
pub fn report_language(language: Option<&str>, ds: &Dataset, st: &mut RunState, format_region: &str) -> Lang {
    let fallback = || Lang { name: format_region.to_string(), assumed: true };
    match language.map(str::trim) {
        Some(l) if l.starts_with('=') => {
            let name = eval_in(l, ds, &[], &[], st, &HashMap::new()).ok().map(|v| v.as_text());
            match name.filter(|n| !n.trim().is_empty()) {
                Some(n) => Lang { name: n.trim().to_string(), assumed: false },
                None => fallback(),
            }
        }
        Some(l) if !l.is_empty() => Lang { name: l.to_string(), assumed: false },
        _ => fallback(),
    }
}

/// Bind a section's items (body, page header or footer) over all dataset rows.
pub fn bind_section(items: &[Item], ds: &Dataset, st: &mut RunState) -> Result<Vec<BNode>, String> {
    let all: Vec<usize> = (0..ds.rows.len()).collect();
    let mut named = vec![];
    bind_items(items, ds, &all, &mut named, st)
}
