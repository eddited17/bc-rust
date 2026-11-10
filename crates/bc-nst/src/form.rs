//! The server side of a web client form: one open page with its AL instance (page
//! globals, `Rec`), the instances of its page extensions, and the page's expression
//! columns. `rpc` keeps one [`PageForm`] per `ServerFormHandle`.
//!
//! What runs where (as the real service tier does it):
//! - OpenForm: SourceTableView, OnInit, OnOpenPage (page first, then extensions).
//! - GetPage: every row returned is loaded into the page's `Rec`, OnAfterGetRecord
//!   runs, then the expression columns (`MP_10`) are evaluated in the page's scope and
//!   sent as extra row cells ([`FormRows`]).
//! - AfterGetCurrRecord (an `InvokeApplicationMethod` call), ValidateField,
//!   ModifyRecord, InsertRecord, ActionField: see `rpc`.

use crate::dataset::{self, Cell, Column, CellTypeCode, PageRowHook};
use crate::metadata::{ControlIndex, ExpressionKind};
use al_model::{Catalog, Control, FieldClass, PageDef, ProcRef};
use al_runtime::interp::{Arg, Frame};
use al_runtime::{AlError, Flow, Instance, RecHandle, Session, Val};
use al_syntax::Expr;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// One `MP_10` column with its source parsed.
pub struct ExprCol {
    pub name: String,
    pub ty: CellTypeCode,
    pub expr: Option<Expr>,
    pub kind: ExpressionKind,
    /// `Control<id>_Format` columns: where the control's format comes from.
    pub fmt: Option<FormatSrc>,
}

/// The format sources of a decimal control, in the order the service tier asks them
/// (NavForm's format column): the control's AutoFormatType/AutoFormatExpression, the field's,
/// then DecimalPlaces (control and field), else the amount format AutoFormatTranslate(1, '').
pub struct FormatSrc {
    ctl_auto: Option<(i64, Option<Expr>)>,
    fld_auto: Option<(i64, Option<Expr>)>,
    ctl_places: Option<String>,
    fld_places: Option<String>,
}

fn parse_src(src: &str) -> Option<Expr> {
    al_syntax::lex(src, &Default::default()).ok().and_then(|t| al_syntax::parse_expr(&t).ok())
}

fn auto_format_of(ty: Option<&String>, expr: Option<&String>) -> Option<(i64, Option<Expr>)> {
    let t = ty?.trim().parse::<i64>().ok()?;
    Some((t, expr.filter(|e| !e.trim().is_empty()).and_then(|e| parse_src(e))))
}

impl FormatSrc {
    fn of(cat: &Catalog, page: &PageDef, c: &Control) -> FormatSrc {
        let fd = c.field.and_then(|fi| page.source_table.and_then(|t| cat.tables.get(&t)).and_then(|t| t.fields.get(fi)));
        let cprop = |k: &str| c.prop_src.get(k).or_else(|| c.props.get(k));
        FormatSrc {
            ctl_auto: auto_format_of(cprop("autoformattype"), cprop("autoformatexpression")),
            fld_auto: fd.and_then(|f| auto_format_of(f.prop_src.get("autoformattype"), f.prop_src.get("autoformatexpression"))),
            ctl_places: cprop("decimalplaces").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            fld_places: fd.and_then(|f| f.decimal_places.clone()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        }
    }
}

/// The largest number of decimals of a DecimalPlaces value (`2:5` → 5, `1` → 1).
fn decimal_max(p: &str) -> i64 {
    let hi = p.rsplit(':').next().unwrap_or(p).trim();
    hi.parse().unwrap_or(0)
}

/// Per-page data that does not change while bc-nst runs: expression columns and the
/// control index. Built once per page id ([`PageStatics::get`]).
pub struct PageStatics {
    pub exprs: Vec<ExprCol>,
    pub ix: ControlIndex,
    /// FlowFields bound to field controls (field indexes): calculated for every row.
    pub flow_fields: Vec<usize>,
}

thread_local! {
    static STATICS: RefCell<HashMap<i64, Rc<PageStatics>>> = RefCell::new(HashMap::new());
}

impl PageStatics {
    pub fn get(cat: &Catalog, page: &PageDef) -> Rc<PageStatics> {
        if let Some(p) = STATICS.with(|m| m.borrow().get(&page.id).cloned()) {
            return p;
        }
        let ix = ControlIndex::build(page);
        let exprs = crate::metadata::expression_columns(cat, page.id)
            .into_iter()
            .map(|x| {
                if matches!(x.kind, ExpressionKind::AutoFormat | ExpressionKind::CurrencyCode) {
                    let fmt = (x.kind == ExpressionKind::AutoFormat)
                        .then(|| x.control_id.and_then(|id| ix.by_id(id)).and_then(|e| ix.control(page, e)).map(|c| FormatSrc::of(cat, page, c)))
                        .flatten();
                    return ExprCol { ty: data_type_code(x.data_type), name: x.name, expr: None, kind: x.kind, fmt };
                }
                let mut expr = al_syntax::lex(&x.source, &Default::default()).ok().and_then(|t| al_syntax::parse_expr(&t).ok());
                if expr.is_none() && x.kind == ExpressionKind::DynamicCaption {
                    // a literal CaptionClass written without quotes in the metadata ('1,2,1')
                    let quoted = format!("'{}'", x.source.replace('\'', "''"));
                    expr = al_syntax::lex(&quoted, &Default::default()).ok().and_then(|t| al_syntax::parse_expr(&t).ok());
                }
                if expr.is_none() {
                    eprintln!("bc-nst: page {}: cannot parse expression column {} = {:?}", page.id, x.name, x.source);
                }
                ExprCol { ty: data_type_code(x.data_type), name: x.name, expr, kind: x.kind, fmt: None }
            })
            .collect();
        let mut flow_fields = vec![];
        if let Some(t) = page.source_table.and_then(|t| cat.tables.get(&t)) {
            collect_flow_fields(&page.layout, t, &mut flow_fields);
        }
        let p = Rc::new(PageStatics { exprs, ix, flow_fields });
        STATICS.with(|m| m.borrow_mut().insert(page.id, p.clone()));
        p
    }
}

fn collect_flow_fields(tree: &[Control], tdef: &al_model::TableDef, out: &mut Vec<usize>) {
    for c in tree {
        if let Some(fi) = c.field {
            if tdef.fields.get(fi).is_some_and(|f| f.class == FieldClass::FlowField) && !out.contains(&fi) {
                out.push(fi);
            }
        }
        if c.kind != "part" {
            collect_flow_fields(&c.children, tdef, out);
        }
    }
}

/// `DataFieldDefinition.Datatype` (`DataType` enum) → the column type the web server
/// gives the expression column (`CommonTypeInformation.DataTypeToNavType` then
/// `ResolveClrType`).
pub fn data_type_code(dt: i64) -> CellTypeCode {
    use CellTypeCode as T;
    match dt {
        0 | 6 => T::Int32,                 // Integer, Option
        5 => T::Decimal,                   // Decimal
        7 => T::Boolean,                   // Boolean
        8 => T::Byte,                      // Byte
        9 | 10 | 19 => T::DateTime,        // Date, Time, DateTime
        11 | 12 | 18 => T::ByteArray,      // Binary, BLOB, RecordID
        15 => T::Int64,                    // BigInteger
        16 => T::TimeSpan,                 // Duration
        17 => T::Guid,                     // GUID
        24 => T::Char,                     // Char
        25 => T::Media,                    // Media
        26 => T::MediaSet,                 // MediaSet
        _ => T::String,                    // OemText, TextConst, Label, OemCode, DateFormula, TableFilter, BigText, Text, String, Code
    }
}

pub struct PageForm {
    pub page_id: i64,
    pub page: &'static PageDef,
    pub inst: Rc<Instance>,
    /// Page extension instances; they share the page's `Rec`.
    pub ext: Vec<Rc<Instance>>,
    pub rec: Option<RecHandle>,
    pub statics: Rc<PageStatics>,
    /// Server form handle of the main form, for parts.
    pub main: Option<String>,
    /// Expression columns that failed to evaluate (logged once each).
    failed: RefCell<Vec<bool>>,
    /// Which instance (0 = page, 1.. = extensions) an expression column evaluates in.
    home: RefCell<Vec<Option<usize>>>,
    /// AutoFormatTranslate results (crate::form format columns), by (type, expression value).
    formats: RefCell<HashMap<(i64, String), String>>,
}

fn flow_err(e: Flow) -> AlError {
    match e {
        Flow::Err(e) => *e,
        _ => AlError::new("unexpected control flow".into()),
    }
}

impl PageForm {
    /// A form for `page`. `inst` reuses an existing page instance (a part the parent
    /// page's code already reached through `CurrPage.<Part>.Page`).
    pub fn new(s: &mut Session<'_>, page: &'static PageDef, inst: Option<Rc<Instance>>, main: Option<String>) -> PageForm {
        let cat = s.cat;
        let inst = inst.unwrap_or_else(|| Instance::new(page.code));
        let temporary = page.props.get("sourcetabletemporary").is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let rec = page.source_table.filter(|t| cat.tables.contains_key(t)).map(|t| {
            let existing = inst.rec.borrow().clone();
            match existing {
                Some(h) => h,
                None => {
                    let h = s.new_record(t, temporary);
                    *inst.rec.borrow_mut() = Some(h.clone());
                    h
                }
            }
        });
        let ext: Vec<Rc<Instance>> = page
            .ext_code
            .iter()
            .map(|&c| {
                let i = Instance::new(c);
                *i.rec.borrow_mut() = rec.clone();
                i
            })
            .collect();
        let statics = PageStatics::get(cat, page);
        let n = statics.exprs.len();
        PageForm { page_id: page.id, page, inst, ext, rec, statics, main, failed: RefCell::new(vec![false; n]), home: RefCell::new(vec![None; n]), formats: Default::default() }
    }

    fn instances(&self) -> impl Iterator<Item = &Rc<Instance>> {
        std::iter::once(&self.inst).chain(self.ext.iter())
    }

    /// The instance whose code object is `code` (the page or one of its extensions).
    pub fn instance_for(&self, code: u32) -> Rc<Instance> {
        self.instances().find(|i| i.code == code).cloned().unwrap_or_else(|| self.inst.clone())
    }

    pub fn has_trigger(&self, s: &Session, name: &str) -> bool {
        self.instances().any(|i| s.code(i.code).trigger(name).is_some())
    }

    /// Run a page trigger on the page, then on each extension. Returns the page's result.
    pub fn trigger(&self, s: &mut Session<'_>, name: &str, args: &[Val]) -> Result<Option<Val>, Flow> {
        let mut out = None;
        for i in self.instances() {
            let Some(p) = s.code(i.code).trigger(name) else { continue };
            let v = s.invoke(ProcRef { code: i.code, proc: p }, args.iter().cloned().map(Arg::Val).collect(), Some(i.clone()), self.rec.clone(), None)?;
            if out.is_none() {
                out = Some(v);
            }
        }
        Ok(out)
    }

    /// Run OnInsertRecord / OnModifyRecord / OnDeleteRecord and tell whether the
    /// default action (the write) goes ahead. A trigger answers only through
    /// `exit(<value>)`: one without such an exit lets the write happen, as on the
    /// service tier (the interpreter's return slot would hold the Boolean default,
    /// false). Every implementation (page and extensions) must allow it.
    pub fn record_trigger_allows(&self, s: &mut Session<'_>, name: &str, args: &[Val]) -> Result<bool, Flow> {
        let mut allow = true;
        for i in self.instances() {
            let co = s.code(i.code);
            let Some(p) = co.trigger(name) else { continue };
            let explicit = co.procs[p as usize].body.as_deref().is_some_and(has_value_exit);
            let v = s.invoke(ProcRef { code: i.code, proc: p }, args.iter().cloned().map(Arg::Val).collect(), Some(i.clone()), self.rec.clone(), None)?;
            if explicit && matches!(v, Val::Bool(false)) {
                allow = false;
            }
        }
        Ok(allow)
    }

    /// Run a control or action trigger (`pr` belongs to the page or an extension).
    pub fn run_proc(&self, s: &mut Session<'_>, pr: ProcRef, args: Vec<Arg>) -> Result<Val, Flow> {
        let inst = self.instance_for(pr.code);
        s.invoke(pr, args, Some(inst), self.rec.clone(), None)
    }

    /// Evaluate an expression in the scope of instance `i` (its globals and `Rec`).
    pub fn eval_in(&self, s: &mut Session<'_>, i: &Rc<Instance>, e: &Expr) -> Result<Val, Flow> {
        let globals = s.instance_globals(i);
        let rec = i.rec.borrow().clone();
        let mut f = Frame { code: i.code, locals: vec![], globals, rec, xrec: None, with: vec![], inst: Some(i.clone()), ret: None, ret_ty: None, curr_field: 0 };
        s.eval(&mut f, e)
    }

    /// SourceTableView, OnInit, OnOpenPage.
    pub fn open(&self, s: &mut Session<'_>) -> Result<(), AlError> {
        if let Some(h) = &self.rec {
            dataset::apply_page_view(s, self.page, h)?;
        }
        self.trigger(s, "OnInit", &[]).map_err(flow_err)?;
        self.trigger(s, "OnOpenPage", &[]).map_err(flow_err)?;
        Ok(())
    }

    /// Value of every expression column, as cells.
    pub fn expression_cells(&self, s: &mut Session<'_>) -> Vec<Cell> {
        let mut cells = Vec::with_capacity(self.statics.exprs.len());
        for (k, x) in self.statics.exprs.iter().enumerate() {
            // format columns of decimal controls (the currency column stays empty, as on the
            // real NST)
            if let Some(src) = &x.fmt {
                let f = self.format_string(s, k, src);
                cells.push(Cell::String(f));
                continue;
            }
            let Some(e) = &x.expr else {
                cells.push(Cell::Null);
                continue;
            };
            if matches!(x.ty, CellTypeCode::Media | CellTypeCode::MediaSet) {
                cells.push(Cell::Null);
                continue;
            }
            let v = self.eval_expr(s, k, e);
            let v = match (v, &x.kind) {
                (Ok(v), ExpressionKind::DynamicCaption) => Ok(Val::Text(resolve_caption_class(s, &v.to_text()))),
                (v, _) => v,
            };
            if std::env::var("BCNST_TRACE_EXPR").is_ok_and(|t| t.eq_ignore_ascii_case(&x.name)) {
                eprintln!("bc-nst: page {}: {} = {:?}", self.page_id, x.name, v.as_ref().map(|v| v.to_text()).map_err(|e| match e { Flow::Err(e) => e.msg.clone(), _ => "flow".into() }));
            }
            let cell = match v {
                Ok(v) => dataset::val_cell(x.ty, &v).unwrap_or(Cell::Null),
                Err(e) => {
                    if !std::mem::replace(&mut self.failed.borrow_mut()[k], true) {
                        eprintln!("bc-nst: page {}: expression column {} failed: {}", self.page_id, x.name, flow_err(e).msg);
                    }
                    Cell::Null
                }
            };
            cells.push(cell);
        }
        cells
    }

    /// The .NET format string of a decimal control for the current row (NavForm's
    /// `Control<id>_Format`): the control's AutoFormat, else the field's (its expression is
    /// evaluated on `Rec`: a field, a table procedure, a literal), else DecimalPlaces (of the
    /// control and the field, the one allowing fewer decimals), else the amount format
    /// AutoFormatTranslate(1, ''). AL formats go through codeunit "Auto Format" and become
    /// .NET formats as the AL compiler converts them (ConvertToDotNetFormatString).
    fn format_string(&self, s: &mut Session<'_>, k: usize, src: &FormatSrc) -> String {
        let mut al = String::new();
        if let Some((t, e)) = &src.ctl_auto {
            let v = match e {
                Some(e) => self.eval_expr(s, k, e).map(|v| v.to_text()).unwrap_or_default(),
                None => String::new(),
            };
            al = self.auto_format(s, *t, &v);
        }
        if al.is_empty() {
            if let Some((t, e)) = &src.fld_auto {
                let v = match e {
                    Some(e) => self.eval_on_rec(s, e).unwrap_or_default(),
                    None => String::new(),
                };
                al = self.auto_format(s, *t, &v);
            }
        }
        if al.is_empty() {
            let places = match (&src.ctl_places, &src.fld_places) {
                (Some(c), Some(f)) => Some(if decimal_max(c) >= decimal_max(f) { f } else { c }),
                (Some(c), None) => Some(c),
                (None, f) => f.as_ref(),
            };
            if let Some(p) = places {
                al = format!("<Precision,{p}><Standard Format,0>");
            }
        }
        if al.is_empty() {
            al = self.auto_format(s, 1, "");
        }
        al_runtime::report::dotnet_format(&al)
    }

    /// AutoFormatTranslate(Type, Expression) through codeunit 45 "Auto Format" (cached per form).
    fn auto_format(&self, s: &mut Session<'_>, ty: i64, expr: &str) -> String {
        if ty == 0 {
            return String::new();
        }
        let key = (ty, expr.to_string());
        if let Some(v) = self.formats.borrow().get(&key) {
            return v.clone();
        }
        let mut out = String::new();
        let cat = s.cat;
        if let Some(&ci) = cat.code_by_key.get(&(al_syntax::ObjectKind::Codeunit, 45)) {
            if let Some(&p) = cat.code[ci as usize].find("ResolveAutoFormat").first() {
                let enum_id = cat.enum_by_name("Auto Format").map(|e| e.id);
                let args = vec![Arg::Val(Val::Opt { ord: ty as i32, enum_id }), Arg::Val(Val::Text(expr.to_string()))];
                match s.invoke(ProcRef { code: ci, proc: p }, args, Some(Instance::new(ci)), None, None) {
                    Ok(v) => out = v.to_text(),
                    Err(e) => eprintln!("bc-nst: page {}: AutoFormatTranslate({ty}, {expr:?}) failed: {}", self.page_id, flow_err(e).msg),
                }
            }
        }
        self.formats.borrow_mut().insert(key, out.clone());
        out
    }

    /// A field's AutoFormatExpression, evaluated on the page's `Rec`: a field of the table
    /// (`"Currency Code"`, `Rec."Currency Code"`), a table procedure without arguments
    /// (`GetAmountFormat()`), or a constant.
    fn eval_on_rec(&self, s: &mut Session<'_>, e: &Expr) -> Option<String> {
        let rec = self.rec.clone()?;
        let field = |n: &str| {
            let r = rec.borrow();
            r.tdef.field(n.trim_matches('"')).map(|i| r.get_val(i).to_text())
        };
        match e {
            Expr::Ident(n) => field(n),
            Expr::Member(b, n) if matches!(&**b, Expr::Ident(r) if r.eq_ignore_ascii_case("rec")) => field(n),
            Expr::Str(t) => Some(t.to_string()),
            Expr::Call(f, args) if args.is_empty() => {
                let name = match &**f {
                    Expr::Ident(n) => n.to_string(),
                    Expr::Member(b, n) if matches!(&**b, Expr::Ident(r) if r.eq_ignore_ascii_case("rec")) => n.to_string(),
                    _ => return None,
                };
                let code = rec.borrow().tdef.code;
                let p = *s.cat.code[code as usize].find(&name).first()?;
                match s.invoke(ProcRef { code, proc: p }, vec![], None, Some(rec.clone()), None) {
                    Ok(v) => Some(v.to_text()),
                    Err(e) => {
                        eprintln!("bc-nst: page {}: AutoFormatExpression {name}() failed: {}", self.page_id, flow_err(e).msg);
                        None
                    }
                }
            }
            _ => None,
        }
    }

    /// Evaluate expression column `k`: in the instance that resolved it last time,
    /// else the page first, then each extension (a column can come from an extension
    /// control, whose variables only the extension instance has).
    fn eval_expr(&self, s: &mut Session<'_>, k: usize, e: &Expr) -> Result<Val, Flow> {
        let all: Vec<Rc<Instance>> = self.instances().cloned().collect();
        if let Some(i) = self.home.borrow()[k] {
            return self.eval_in(s, &all[i], e);
        }
        let mut first_err = None;
        for (n, i) in all.iter().enumerate() {
            match self.eval_in(s, i, e) {
                Ok(v) => {
                    self.home.borrow_mut()[k] = Some(n);
                    return Ok(v);
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        Err(first_err.unwrap_or_else(|| Flow::Err(Box::new(AlError::new("no page instance".into())))))
    }

    /// Load `row` into the page's `Rec` (as a record read from the database).
    pub fn load_row(&self, row: &bc_store::Row) {
        if let Some(r) = &self.rec {
            let mut rb = r.borrow_mut();
            rb.row = row.clone().into();
            rb.xrow = Some(rb.row.clone());
        }
    }

    /// Calculate the FlowFields the page shows on its `Rec`.
    pub fn calc_flow_fields(&self, s: &mut Session<'_>) -> Result<(), Flow> {
        if let Some(r) = &self.rec {
            for &fi in &self.statics.flow_fields {
                s.calc_field(r, fi)?;
            }
        }
        Ok(())
    }

    /// The control with id `id` (`IDED_1`).
    pub fn control(&self, id: i32) -> Option<&'static Control> {
        let e = self.statics.ix.by_id(id)?;
        self.statics.ix.control(self.page, e)
    }

    /// `NavRecordState.FormVariables`: one table `<table id>_NonRow` with one row of the
    /// page's DynamicCaption columns (what the real NST puts there; the page variables
    /// travel in the record rows).
    pub fn form_variables(&self, s: &mut Session<'_>) -> dataset::NavDataSet {
        let tid = self.rec.as_ref().map(|r| r.borrow().tdef.id).unwrap_or(0);
        let mut cols = vec![];
        let mut cells = vec![];
        for (k, x) in self.statics.exprs.iter().enumerate() {
            if x.kind != ExpressionKind::DynamicCaption {
                continue;
            }
            let Some(e) = &x.expr else { continue };
            let text = match self.eval_expr(s, k, e) {
                Ok(v) => resolve_caption_class(s, &v.to_text()),
                Err(_) => String::new(),
            };
            cols.push(Column::new(x.name.clone(), CellTypeCode::String));
            cells.push(Cell::String(text));
        }
        let mut ds = dataset::NavDataSet::new();
        if let Ok(mut t) = dataset::DataTable::new(format!("{tid}_NonRow"), cols) {
            let _ = t.push_row(cells);
            let _ = ds.add_table(t);
        }
        ds
    }

    /// Members the real NST sets on every state it returns for a form: the page
    /// caption and the form variables.
    pub fn finish_state(&self, s: &mut Session<'_>, st: &mut serde_json::Map<String, serde_json::Value>) {
        if !self.page.caption.is_empty() {
            st.insert("PageCaption".into(), serde_json::json!(self.page.caption));
        }
        if let Ok(v) = serde_json::to_value(&self.form_variables(s)) {
            st.insert("FormVariables".into(), v);
        }
    }

    /// The page's `Rec` as a one-row data set with expression columns.
    pub fn record_dataset(&self, s: &mut Session<'_>) -> Result<dataset::NavDataSet, AlError> {
        let Some(rec) = self.rec.clone() else {
            return Ok(dataset::NavDataSet::new());
        };
        let calc = self.statics.flow_fields.clone();
        let mut hook = FormRows { form: self, after_get: false };
        dataset::record_dataset(s, &rec, &calc, &mut hook)
    }
}

/// The single-value filters of a client state's view (`TableView.CurrentFilters`,
/// `IsExactValue`): field index and stored value. A subpage's link to its document
/// arrives this way (filter group 4), the page's SourceTableView as group 2.
pub fn state_defaults(s: &Session<'_>, rec: &RecHandle, state: &serde_json::Value) -> Vec<(usize, bc_store::Value)> {
    let tdef = rec.borrow().tdef;
    let mut out = vec![];
    let groups = state.pointer("/TableView/CurrentFilters").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for g in groups {
        for f in g.get("Filters").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
            let exact = f.get("IsExactValue").and_then(|v| v.as_bool()).unwrap_or(false);
            let (Some(id), Some(text)) = (f.get("FilterField").and_then(|v| v.as_i64()), f.get("FilterValue").and_then(|v| v.as_str())) else { continue };
            let Some(fi) = tdef.field_no(id) else { continue };
            let fd = &tdef.fields[fi];
            if fd.class != FieldClass::Normal {
                continue;
            }
            let v = if exact {
                let ev = |n: &str| s.enum_ordinal_for_field(fd, n);
                al_runtime::record::text_to_store(fd, text, &ev, s.work_date)
            } else {
                // a plain value without filter operators is a fixed value too
                match s.parse_field_filter(fd, text) {
                    Ok(n) => n.eq_value().cloned(),
                    Err(_) => None,
                }
            };
            if let Some(v) = v {
                out.retain(|(x, _)| *x != fi);
                out.push((fi, v));
            }
        }
    }
    out
}

/// The client's TableView filter groups above 0 on the page's `Rec`, as the NST's
/// `NavRecord.SetTableView` puts the state's view on the form record before a trigger runs.
/// SubPageLink filters travel in group 4, and AL reads them there: page 1286 "Payment Rec.
/// Match Details" does `FilterGroup(4)` + `Evaluate(LineNo, GetFilter("Statement Line No."))`
/// in OnAfterGetCurrRecord, and failed with "The value '' can't be evaluated into type
/// Integer" without them. Group 0 (the user's filters) and -1 (search) stay with GetPage, which
/// applies them to its own copy of the record.
pub fn apply_state_filter_groups(s: &Session<'_>, rec: &RecHandle, state: &serde_json::Value) {
    let tdef = rec.borrow().tdef;
    for g in state.pointer("/TableView/CurrentFilters").and_then(|v| v.as_array()).into_iter().flatten() {
        let group = g.get("FilterGroupNo").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        if group <= 0 {
            continue;
        }
        for f in g.get("Filters").and_then(|v| v.as_array()).into_iter().flatten() {
            let (Some(id), Some(text)) = (f.get("FilterField").and_then(|v| v.as_i64()), f.get("FilterValue").and_then(|v| v.as_str())) else { continue };
            let Some(fi) = tdef.field_no(id) else { continue };
            if text.is_empty() {
                continue;
            }
            let fd = &tdef.fields[fi];
            let node = if f.get("IsExactValue").and_then(|v| v.as_bool()).unwrap_or(false) {
                let ev = |n: &str| s.enum_ordinal_for_field(fd, n);
                al_runtime::record::text_to_store(fd, text, &ev, s.work_date).map(bc_store::filter::FNode::Eq)
            } else {
                s.parse_field_filter(fd, text).ok()
            };
            let Some(node) = node else { continue };
            let mut r = rec.borrow_mut();
            let saved = r.filter_group;
            r.filter_group = group;
            r.set_filter(fi, Some(node), text.to_string());
            r.filter_group = saved;
        }
    }
}

/// Does a statement list contain `exit(<value>)` anywhere?
fn has_value_exit(stmts: &[al_syntax::Stmt]) -> bool {
    use al_syntax::StmtKind as K;
    stmts.iter().any(|st| match &st.kind {
        K::Exit(Some(_)) => true,
        K::Block(b) | K::Repeat { body: b, .. } => has_value_exit(b),
        K::If { then, els, .. } => has_value_exit(std::slice::from_ref(then)) || els.as_ref().is_some_and(|e| has_value_exit(std::slice::from_ref(e))),
        K::Case { branches, els, .. } => branches.iter().any(|b| has_value_exit(std::slice::from_ref(&b.body))) || els.as_ref().is_some_and(|e| has_value_exit(e)),
        K::While { body, .. } | K::For { body, .. } | K::ForEach { body, .. } | K::With { body, .. } => has_value_exit(std::slice::from_ref(body)),
        K::AssertError(b) => has_value_exit(std::slice::from_ref(b)),
        _ => false,
    })
}

fn al_err(msg: String) -> Flow {
    Flow::Err(Box::new(AlError::new(msg)))
}

impl PageForm {
    /// Position the page's `Rec` on the record the client sent (row 0 of `ds`) and
    /// take over the client's field values. The record is read from the database by
    /// its bookmark when it has one (so xRec is the stored record); then every
    /// normal field cell with a value is applied, except `skip` (the field being
    /// validated, which keeps its old value until `Validate`). Returns whether the
    /// record exists in the database.
    pub fn load_client_record(&self, s: &mut Session<'_>, ds: &dataset::NavDataSet, skip: Option<usize>) -> Result<bool, Flow> {
        self.load_client_record_with(s, ds, skip, &[])
    }

    /// [`Self::load_client_record`]; a new record also takes `defaults` (field index and
    /// value: the single-value filters of the client's view, e.g. a subpage's link to
    /// its document) where the client sent none.
    pub fn load_client_record_with(&self, s: &mut Session<'_>, ds: &dataset::NavDataSet, skip: Option<usize>, defaults: &[(usize, bc_store::Value)]) -> Result<bool, Flow> {
        let Some(rec) = &self.rec else { return Ok(false) };
        let Some(t) = ds.tables.first().filter(|t| !t.rows.is_empty()) else { return Ok(false) };
        let schema = rec.borrow().schema.clone();
        let tdef = rec.borrow().tdef;
        let mut in_db = false;
        if let Some(Cell::Bytes(b)) = t.get(0, dataset::page::COL_BOOKMARK) {
            if !b.is_empty() {
                if let Ok(pk) = dataset::decode_bookmark(&schema, b) {
                    let probe = al_runtime::record::copy_record(rec);
                    probe.borrow_mut().row = (dataset::pk_row(&schema, &pk)).into();
                    if let Some(stored) = s.rec_get_pk(&probe) {
                        self.load_row(&stored);
                        in_db = true;
                    }
                }
            }
        }
        if !in_db {
            // the bookmark names no record (the client can still hold the one from before
            // an insert numbered the record): try the primary key in the client's row
            let pk = tdef.pk().fields.clone();
            let probe = al_runtime::record::copy_record(rec);
            let mut complete = true;
            for &fi in &pk {
                let cell = t.get(0, &tdef.fields[fi].id.to_string());
                match cell.and_then(|c| dataset::cell_val(&tdef.fields[fi], c)) {
                    Some(v) => match al_runtime::record::to_store(&tdef.fields[fi], &v) {
                        Ok(sv) => probe.borrow_mut().row[fi] = sv,
                        Err(_) => complete = false,
                    },
                    None => complete = false,
                }
            }
            if complete {
                if let Some(stored) = s.rec_get_pk(&probe) {
                    self.load_row(&stored);
                    in_db = true;
                }
            }
        }
        if !in_db {
            // a new record: start from its initial values and the view's fixed values
            let mut rb = rec.borrow_mut();
            if rb.xrow.is_some() || rb.row.len() != schema.columns.len() {
                let mut fresh = schema.new_row();
                s.init_row(tdef, &mut fresh);
                rb.row = (fresh).into();
                rb.xrow = None;
            }
        }
        for (ci, col) in t.columns.iter().enumerate() {
            let Ok(id) = col.name.parse::<i64>() else { continue };
            let Some(fi) = tdef.field_no(id) else { continue };
            let fd = &tdef.fields[fi];
            if Some(fi) == skip || fd.class != FieldClass::Normal || fd.is_system() {
                continue;
            }
            let Some(v) = dataset::cell_val(fd, &t.rows[0][ci]) else { continue };
            let sv = al_runtime::record::to_store(fd, &v).map_err(al_err)?;
            rec.borrow_mut().row[fi] = sv;
        }
        if !in_db && !defaults.is_empty() {
            // the view's fixed values (a subpage's link to its document) win over the
            // blanks the client sends for them in a new line
            let mut fresh = schema.new_row();
            s.init_row(tdef, &mut fresh);
            let mut rb = rec.borrow_mut();
            for (fi, v) in defaults {
                if rb.row[*fi] == fresh[*fi] {
                    rb.row[*fi] = v.clone();
                }
            }
        }
        Ok(in_db)
    }

    /// EvaluateRelation: the index of the first table relation branch of field `fi`
    /// whose WHERE conditions the page's `Rec` meets (the client then uses that
    /// branch's lookup), -1 when none applies.
    pub fn relation_index(&self, s: &mut Session<'_>, fi: usize) -> Result<i32, Flow> {
        let Some(rec) = &self.rec else { return Ok(-1) };
        let tdef = rec.borrow().tdef;
        let fd = &tdef.fields[fi];
        'branch: for (i, b) in fd.relation.iter().enumerate() {
            for c in &b.when {
                let Some(cf) = tdef.field(&c.field) else { continue 'branch };
                let cfd = &tdef.fields[cf];
                let cur = rec.borrow().row[cf].clone();
                let ok = match c.kind {
                    al_model::CondKind::Const => {
                        let ev = |n: &str| s.enum_ordinal_for_field(cfd, n);
                        al_runtime::record::text_to_store(cfd, &c.value, &ev, s.work_date).is_some_and(|v| v == cur)
                    }
                    al_model::CondKind::Filter => s.parse_field_filter(cfd, &c.value)?.matches(&cur),
                    al_model::CondKind::Field => true,
                };
                if !ok {
                    continue 'branch;
                }
            }
            return Ok(i as i32);
        }
        Ok(-1)
    }

    /// Position the page's `Rec` on the record a bookmark names (read from the
    /// database). Returns whether it exists.
    pub fn position_on_bookmark(&self, s: &mut Session<'_>, bookmark: &[u8]) -> bool {
        let Some(rec) = &self.rec else { return false };
        let schema = rec.borrow().schema.clone();
        let Ok(pk) = dataset::decode_bookmark(&schema, bookmark) else { return false };
        let probe = al_runtime::record::copy_record(rec);
        probe.borrow_mut().row = (dataset::pk_row(&schema, &pk)).into();
        match s.rec_get_pk(&probe) {
            Some(row) => {
                self.load_row(&row);
                true
            }
            None => false,
        }
    }

    /// DeleteRecord for one record (a bookmark from `SelectedRecords` / `CurrentRecord`):
    /// position on it, OnDeleteRecord (a `false` result skips the delete), then
    /// `Rec.Delete(true)` (table OnDelete). Returns whether the record was deleted.
    pub fn delete(&self, s: &mut Session<'_>, bookmark: &[u8]) -> Result<bool, Flow> {
        let Some(rec) = &self.rec else { return Ok(false) };
        if !self.position_on_bookmark(s, bookmark) {
            let t = rec.borrow().tdef.caption.clone();
            return Err(al_err(format!("The {t} record does not exist.")));
        }
        if !self.record_trigger_allows(s, "OnDeleteRecord", &[])? {
            return Ok(false);
        }
        if let Err(m) = s.rec_delete(rec, true)? {
            return Err(al_err(m));
        }
        Ok(true)
    }

    /// `Rec.Init()` (InitValues), as the page's record before any is read.
    pub fn init_record(&self, s: &mut Session<'_>) {
        let Some(rec) = &self.rec else { return };
        let mut rb = rec.borrow_mut();
        let mut fresh = rb.schema.new_row();
        s.init_row(rb.tdef, &mut fresh);
        rb.row = (fresh).into();
        rb.xrow = None;
    }

    /// The value the client sent for field `fi` (row 0 of `ds`).
    fn client_value(&self, ds: &dataset::NavDataSet, fi: usize) -> Option<Val> {
        let rec = self.rec.as_ref()?;
        let fd = rec.borrow().field(fi);
        let t = ds.tables.first()?;
        dataset::cell_val(fd, t.get(0, &fd.id.to_string())?)
    }

    /// ValidateField for control `c`: `Rec.Validate(field, value)` for a field
    /// control, an assignment for a page-variable control, then the control's
    /// OnValidate. Returns whether the record exists in the database.
    pub fn validate_control(&self, s: &mut Session<'_>, ds: &dataset::NavDataSet, c: &'static Control) -> Result<bool, Flow> {
        self.validate_control_with(s, ds, c, &[])
    }

    pub fn validate_control_with(&self, s: &mut Session<'_>, ds: &dataset::NavDataSet, c: &'static Control, defaults: &[(usize, bc_store::Value)]) -> Result<bool, Flow> {
        let in_db;
        if let (Some(fi), Some(rec)) = (c.field, &self.rec) {
            in_db = self.load_client_record_with(s, ds, Some(fi), defaults)?;
            let v = self.client_value(ds, fi);
            // a field changed in the web client: CurrFieldNo is set while it validates
            s.ui_validate(rec, fi, v)?;
        } else {
            in_db = self.load_client_record_with(s, ds, None, defaults)?;
            // a control on a page variable: the value is in the expression column the
            // control is bound to (DataColumnName = the variable name)
            if let Some(Expr::Ident(var)) = &c.source_expr {
                let t = ds.tables.first();
                let cell = t.and_then(|t| t.get(0, var));
                let code = c.triggers.values().next().map(|pr| pr.code).unwrap_or(self.inst.code);
                let inst = self.instance_for(code);
                let g = s.instance_globals(&inst);
                if let (Some(gv), Some(cell)) = (g.vars.iter().find(|x| x.name.eq_ignore_ascii_case(var)).cloned(), cell) {
                    let cur = gv.slot.borrow().clone();
                    let v = match (cell, &cur) {
                        (Cell::Bool(b), _) => Val::Bool(*b),
                        (Cell::Int32(i), Val::Opt { enum_id, .. }) => Val::Opt { ord: *i, enum_id: *enum_id },
                        (Cell::Int32(i), _) => Val::Int(*i),
                        (Cell::Decimal(d), _) => Val::Dec(*d),
                        (Cell::String(t), _) => Val::Text(t.clone()),
                        (Cell::DateTime { ticks, .. }, Val::Date(_)) => Val::Date(dataset::convert::ticks_to_date(*ticks)),
                        _ => cur.clone(),
                    };
                    s.write_lval(&al_runtime::interp::LVal::Slot(gv.slot.clone(), gv.ty), v)?;
                }
            }
        }
        if let Some(pr) = c.triggers.get("onvalidate").copied() {
            self.run_proc(s, pr, vec![])?;
        }
        Ok(in_db)
    }

    /// ModifyRecord: the client's record, OnModifyRecord (a `false` result skips the
    /// write), `Rec.Modify(true)`, then the stored record is read back.
    pub fn modify(&self, s: &mut Session<'_>, ds: &dataset::NavDataSet) -> Result<(), Flow> {
        let Some(rec) = &self.rec else { return Ok(()) };
        if !self.load_client_record(s, ds, None)? {
            let r = rec.borrow();
            let pk: Vec<String> = r.tdef.pk().fields.iter().map(|&fi| format!("{:?}", r.row[fi])).collect();
            eprintln!("bc-nst: page {}: ModifyRecord: no {} record {}", self.page_id, r.tdef.name, pk.join(","));
            return Err(al_err(format!("The {} record does not exist.", r.tdef.caption)));
        }
        if self.record_trigger_allows(s, "OnModifyRecord", &[])? {
            if let Err(m) = s.rec_modify(rec, true)? {
                return Err(al_err(m));
            }
        }
        if let Some(row) = s.rec_get_pk(rec) {
            self.load_row(&row);
        }
        self.calc_flow_fields(s)?;
        self.trigger(s, "OnAfterGetRecord", &[])?;
        self.trigger(s, "OnAfterGetCurrRecord", &[])?;
        Ok(())
    }

    /// InsertRecord: the client's record, OnInsertRecord(BelowxRec) (a `false` result
    /// skips the write), `Rec.Insert(true)`, then the stored record is read back.
    pub fn insert(&self, s: &mut Session<'_>, ds: &dataset::NavDataSet, below: bool, defaults: &[(usize, bc_store::Value)]) -> Result<(), Flow> {
        let Some(rec) = &self.rec else { return Ok(()) };
        self.load_client_record_with(s, ds, None, defaults)?;
        // AutoSplitKey: the last primary key field numbers the new line (after the last
        // one of the same document, in steps of 10000)
        if self.page.props.get("autosplitkey").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            let tdef = rec.borrow().tdef;
            let pk = tdef.pk().fields.clone();
            if let Some(&last) = pk.last() {
                let cur = rec.borrow().row[last].clone();
                if matches!(cur, bc_store::Value::Int(0)) {
                    let probe = s.new_record(tdef.id, false);
                    {
                        let mut p = probe.borrow_mut();
                        for &fi in &pk[..pk.len() - 1] {
                            let v = rec.borrow().row[fi].clone();
                            let t = al_runtime::record::to_val(&tdef.fields[fi], &v).to_text();
                            p.set_filter(fi, Some(al_runtime::filter::FNode::Eq(v)), t);
                        }
                    }
                    let n = if s.find_first(&probe, true)? { probe.borrow().get_val(last).as_i64() } else { 0 };
                    rec.borrow_mut().row[last] = bc_store::Value::Int((n + 10000) as i32);
                }
            }
        }
        if self.record_trigger_allows(s, "OnInsertRecord", &[Val::Bool(below)])? {
            if let Err(m) = s.rec_insert(rec, true)? {
                return Err(al_err(m));
            }
        }
        if let Some(row) = s.rec_get_pk(rec) {
            self.load_row(&row);
        }
        {
            let r = rec.borrow();
            let pk: Vec<String> = r.tdef.pk().fields.iter().map(|&fi| format!("{:?}", r.row[fi])).collect();
            eprintln!("bc-nst: page {}: inserted {} {}", self.page_id, r.tdef.name, pk.join(","));
        }
        self.calc_flow_fields(s)?;
        self.trigger(s, "OnAfterGetRecord", &[])?;
        self.trigger(s, "OnAfterGetCurrRecord", &[])?;
        Ok(())
    }

    /// NewRecord: `Rec.Init()`, values of single-value filters, OnNewRecord(BelowxRec).
    pub fn new_record(&self, s: &mut Session<'_>, ds: Option<&dataset::NavDataSet>, below: bool) -> Result<(), Flow> {
        let Some(rec) = &self.rec else { return Ok(()) };
        let _ = ds;
        {
            let mut rb = rec.borrow_mut();
            let mut fresh = rb.schema.new_row();
            s.init_row(rb.tdef, &mut fresh);
            for fl in rb.filters.clone() {
                if let Some(v) = fl.node.eq_value() {
                    fresh[fl.field] = v.clone();
                }
            }
            rb.row = (fresh).into();
            rb.xrow = None;
        }
        self.trigger(s, "OnNewRecord", &[Val::Bool(below)])?;
        Ok(())
    }
}

/// CaptionClass translation, as the platform does it through
/// `"UI Helper Triggers".CaptionClassTranslate` and the System Application's
/// codeunit 55 "Caption Class Impl.": split at the first comma; area 3 is the text
/// itself; any other area goes to codeunit 42 "Caption Class".OnResolveCaptionClass,
/// whose Base App subscribers resolve dimension (1), VAT (2), currency and other
/// areas; unresolved classes show as written.
pub fn resolve_caption_class(s: &mut Session<'_>, class: &str) -> String {
    // UI Helper Triggers.CaptionClassTranslate, as the platform raises it: Caption Class Impl.
    // handles area 3 and raises "Caption Class".OnResolveCaptionClass for the others
    let lang = s.global_language;
    match crate::platform::caption_class_translate(s, lang, class) {
        Ok(Some(t)) => return t,
        Ok(None) => {}
        Err(e) => eprintln!("bc-nst: CaptionClassTranslate({class}) failed: {}", crate::platform::flow_message(&e)),
    }
    // nobody subscribes: what Caption Class Impl. does
    let Some((area, expr)) = class.split_once(',') else { return class.to_string() };
    if area == "3" {
        return expr.to_string();
    }
    let caption = al_runtime::value::slot(Val::Text(String::new()));
    let resolved = al_runtime::value::slot(Val::Bool(false));
    let args = vec![
        ("CaptionArea", Arg::Val(Val::text(area))),
        ("CaptionExpr", Arg::Val(Val::text(expr))),
        ("Language", Arg::Val(Val::Int(1033))),
        ("Caption", Arg::Ref(al_runtime::interp::LVal::Slot(caption.clone(), None))),
        ("Resolved", Arg::Ref(al_runtime::interp::LVal::Slot(resolved.clone(), None))),
    ];
    let r = s.raise_event(al_syntax::ObjectKind::Codeunit, 42, "OnResolveCaptionClass", "", args, None, None);
    if r.is_ok() && resolved.borrow().as_bool() {
        return caption.borrow().to_text();
    }
    class.to_string()
}

/// GetPage row hook of an open form: OnAfterGetRecord on the page's `Rec`, then the
/// expression columns.
pub struct FormRows<'a> {
    pub form: &'a PageForm,
    /// Run OnAfterGetRecord for each row (false when encoding the current record again).
    pub after_get: bool,
}

impl PageRowHook for FormRows<'_> {
    fn columns(&mut self, _tdef: &'static al_model::TableDef) -> Vec<Column> {
        self.form.statics.exprs.iter().map(|x| Column::new(x.name.clone(), x.ty)).collect()
    }

    fn row(&mut self, s: &mut Session, h: &RecHandle) -> Result<Vec<Cell>, AlError> {
        // Session<'_> is the only kind bc-nst creates
        let f = self.form;
        if self.after_get {
            let row = h.borrow().row.clone();
            if let Some(r) = &f.rec {
                if !Rc::ptr_eq(r, h) {
                    f.load_row(&row);
                }
            }
            if let Err(e) = f.trigger(s, "OnAfterGetRecord", &[]) {
                return Err(flow_err(e));
            }
            // OnAfterGetRecord may change Rec in memory; the row shows those values
            if let Some(r) = &f.rec {
                if !Rc::ptr_eq(r, h) {
                    let row = r.borrow().row.clone();
                    h.borrow_mut().row = row;
                }
            }
        }
        Ok(f.expression_cells(s))
    }
}
