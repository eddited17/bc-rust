//! `MasterPage` generation: page properties, the content and factbox areas, field
//! controls with the NST-side property solver applied, expression columns (`MP_10`) and
//! page triggers (`MP_11`). The command bar lives in `actions.rs`.

use super::alprop::{self, app_area, bool_lit, cbool, enum_val, expr_src, ident_src, src, word};
use super::el::El;
use super::ids::{ControlEntry, ControlIndex, Section};
use super::table::{self, fbool, field_of, fprop, meta_type_code, option_info, META_OPTION};
use al_model::{Catalog, CodeObject, Cond, Control, FieldClass, FieldDef, FieldType, PageDef, RelationBranch, TableDef, TableView};
use al_syntax::{BinOp, Expr, ObjectKind, ProcKind, TypeRef, UnOp, VarDecl};
use serde_json::Value;

/// One expression column of a page (`MP_10` entry). The dataset code must add a column
/// with this name to every row it sends for the page and fill it with the value of
/// `source` (an AL expression in the page's scope: page variables, `Rec`, procedures).
#[derive(Debug, Clone, PartialEq)]
pub struct ExpressionColumn {
    /// `DFD_1`: the column name, also the string that `CDD_11` or a dynamic property refers to.
    pub name: String,
    /// AL source of the value. For [`ExpressionKind::DynamicCaption`] this is the CaptionClass
    /// expression; the column holds the resolved caption text.
    pub source: String,
    pub kind: ExpressionKind,
    /// `DFD_2`, `DataType` numbering: Integer 0, Decimal 5, Option 6, Boolean 7, Date 9,
    /// Time 10, BLOB 12, DateFormula 13, BigInteger 15, Duration 16, GUID 17, DateTime 19,
    /// Text 21, Code 23.
    pub data_type: i64,
    pub length: u32,
    /// `IDED_1` of the control that introduced the column (first one if shared).
    pub control_id: Option<i32>,
    /// Defined inside a repeater, so it varies per row.
    pub in_repeater: bool,
    /// `(table id, field no)` when the expression is `Var.Field` of a record variable.
    pub table_field: Option<(i64, i64)>,
    /// A variable or record field the client may assign (`DFD_7`).
    pub assignable: bool,
    pub option_names: Vec<String>,
    pub option_captions: Vec<String>,
    pub option_values: Vec<i64>,
    pub enum_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExpressionKind {
    /// Value of a field control whose source is not a field of the page's table.
    SourceExpression,
    /// A page variable referenced by a dynamic property (`Visible = ShowX`).
    PageVariable,
    /// A synthesized column for a non-trivial property expression, e.g. `Visible = not X`
    /// becomes column `Control<id>_Visible` and the control's property refers to it.
    Property { property: String },
    /// `Control<id>_DynamicCaption`, for controls with a `CaptionClass`.
    DynamicCaption,
    /// `Control<id>_Format` of a decimal control (`CD2_2`): the .NET format string of the
    /// control's AutoFormatType/AutoFormatExpression or DecimalPlaces, per row
    /// (crate::form, NavForm's format column).
    AutoFormat,
    /// `Control<id>_CurrencyCode` of a decimal control (`CD2_3`).
    CurrencyCode,
}

#[derive(Debug, Clone, Default)]
struct ColType {
    dt: i64,
    len: u32,
    names: Vec<String>,
    captions: Vec<String>,
    values: Vec<i64>,
    enum_id: Option<i64>,
    field: Option<(i64, i64)>,
    assignable: bool,
}

impl ColType {
    fn of(dt: i64) -> ColType {
        ColType { dt, ..Default::default() }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Ctx {
    pub in_repeater: bool,
    /// An ancestor group has `Visible = false`; every descendant then gets Visible "0".
    pub hidden: bool,
    /// Group nesting depth (0 = top-level FastTab).
    pub depth: usize,
}

pub(crate) struct Gen<'a> {
    pub cat: &'a Catalog,
    pub page: &'a PageDef,
    pub table: Option<&'a TableDef>,
    pub ix: ControlIndex,
    pub token: i64,
    codes: Vec<&'a CodeObject>,
    pub exprs: Vec<ExpressionColumn>,
    page_editable: bool,
    pub saw_actionref: bool,
}

const PAGE_TRIGGERS: &[(&str, i64)] = &[
    ("OnInit", 0),
    ("OnOpenPage", 1),
    ("OnClosePage", 2),
    ("OnFindRecord", 3),
    ("OnNextRecord", 4),
    ("OnAfterGetRecord", 5),
    ("OnNewRecord", 6),
    ("OnInsertRecord", 7),
    ("OnModifyRecord", 8),
    ("OnDeleteRecord", 9),
    ("OnQueryClosePage", 10),
    ("OnAfterGetCurrRecord", 11),
    ("OnPageBackgroundTaskCompleted", 12),
    ("OnPageBackgroundTaskError", 13),
];

const CONTROL_TRIGGERS: &[(&str, i64)] = &[("OnValidate", 0), ("OnLookup", 1), ("OnDrillDown", 2), ("OnAssistEdit", 3), ("OnAfterLookup", 5)];

/// First string literal of a property source (`'Text', Comment = '...'` -> `Text`).
pub(crate) fn text_prop(s: &str) -> String {
    let toks = alprop::lex_src(s);
    match toks.first() {
        Some(t) if t.kind == al_syntax::Tk::Str => t.text.to_string(),
        _ => s.to_string(),
    }
}

pub(crate) fn ctext(c: &Control, k: &str) -> Option<String> {
    src(c, k).map(text_prop).filter(|s| !s.is_empty())
}

impl<'a> Gen<'a> {
    pub fn new(cat: &'a Catalog, page: &'a PageDef, token: i64) -> Gen<'a> {
        let table = page.source_table.and_then(|id| cat.tables.get(&id));
        let codes = std::iter::once(page.code).chain(page.ext_code.iter().copied()).filter_map(|i| cat.code.get(i as usize)).collect();
        let page_editable = page.props.get("editable").and_then(|s| bool_lit(s)) != Some(false);
        Gen { cat, page, table, ix: ControlIndex::build(page), token, codes, exprs: vec![], page_editable, saw_actionref: false }
    }

    // ------------------------------------------------------------------ page

    pub fn master_page(&mut self) -> Value {
        let mut content = self.area_controls("content");
        content.extend(self.role_center_area());
        let factboxes = self.area_controls("factboxes");
        let command_bar = self.command_bar();
        let props = self.page_properties();
        let page = self.page;
        let mut el = El::new("MasterPage");
        el.num_bit("IDED_1", page.id, 0);
        el.put("NED_1", page.name.as_str());
        if let Some(ns) = self.codes.first().and_then(|c| c.namespace.as_deref()) {
            el.str("NED_2", ns);
        }
        el.put("NED_3", if page.caption.is_empty() { page.name.as_str() } else { page.caption.as_str() });
        if let Some(cb) = command_bar {
            el.put("MP_1", cb);
        }
        let mut ca = El::new("ContentAreaDefinition");
        ca.list("ABD0_1", content);
        el.put("MP_3", ca.wrap());
        let has_factboxes = !factboxes.is_empty();
        if has_factboxes {
            // the real service tier wraps the factboxes in one ControlContainerPlaceHolder
            // with ContainerType 1 (CCPH_2, EDSpec bit 9); with the parts directly in
            // ABD0_1 the web client lays them out below the content, not in the FactBox pane
            let mut ph = El::new("ControlContainerPlaceHolder");
            ph.list("CCPH_1", factboxes);
            ph.num_bit("CCPH_2", 1, 9);
            ph.list("CBD_1", vec![]);
            ph.list("CBD_2", vec![]);
            ph.put("UIED_5", "019af70b-3d83-4cf5-9929-a33ebaf0c844");
            let mut ia = El::new("InfopartsAreaDefinition");
            ia.list("ABD0_1", vec![ph.wrap()]);
            el.put("MP_7", ia.wrap());
        }
        el.put("MP_8", props);
        let exprs: Vec<Value> = self.exprs.iter().map(dfd).collect();
        el.list_nonempty("MP_10", exprs);
        el.list_nonempty("MP_11", self.triggers());
        el.num_bit("MP_15", self.token, 2);
        if let Some(sf) = page.props.get("showfilter").and_then(|s| bool_lit(s)) {
            el.flag("MP_16", sf, 3);
        }
        // MP_17 ShowInfoPartPane: without it the web client lays the factboxes out
        // below the content instead of in the FactBox pane (BC 29.1 sends true for
        // page 21; EDSpec bit 4)
        if has_factboxes {
            el.flag("MP_17", true, 4);
        }
        el.wrap()
    }

    fn has_subscriber(&self, event: &str, element: &str) -> bool {
        self.cat.subscribers.contains_key(&(ObjectKind::Page, self.page.id, event.to_ascii_lowercase(), element.to_ascii_lowercase()))
    }

    fn has_page_trigger(&self, name: &str) -> bool {
        self.codes.iter().any(|co| co.trigger(name).is_some()) || self.has_subscriber(&format!("{name}Event"), "")
    }

    fn triggers(&self) -> Vec<Value> {
        PAGE_TRIGGERS
            .iter()
            .filter(|(n, _)| self.has_page_trigger(n))
            .map(|&(n, ty)| {
                let mut el = El::new("PageTriggerDefinition");
                el.num_bit("PTD_1", ty, 0);
                el.put("PTD_2", n);
                el.wrap()
            })
            .collect()
    }

    fn page_properties(&mut self) -> Value {
        let p = self.page;
        let w = |k: &str| p.props.get(k).map(|s| s.as_str());
        let b = |k: &str| w(k).and_then(bool_lit);
        let mut el = El::new("PageProperties");
        el.put("PP_1", self.source_object());
        // PageType is always specified, so Card (0) still sets bit 0
        el.num_bit("PP_7", enum_val(alprop::PAGE_TYPES, &p.page_type).unwrap_or(0), 0);
        if b("extensible") == Some(false) {
            el.put("PP_8", false);
        }
        if self.saw_actionref {
            el.put("PP_9", true);
        }
        if self.has_subscriber("OnOpenPageEvent", "") {
            el.flag("PP_16", true, 1);
        }
        if self.has_page_trigger("OnAfterGetCurrRecord") {
            el.flag("PP_17", true, 2);
        }
        if b("editable") == Some(false) {
            el.put("PP_18", false);
        }
        if w("dataaccessintent").is_some_and(|s| s.eq_ignore_ascii_case("readonly")) {
            el.num_bit("PP_20", 1, 3);
        }
        if let Some(id) = table::page_id_of(self.cat, p.card_page.as_deref()) {
            el.num_bit("PP_21", id, 4);
        }
        if b("refreshonactivate") == Some(true) {
            el.put("PP_23", true);
        }
        el.put("PP_26", super::ids::page_guid(p.id).to_string());
        for (k, wire) in [("instructionaltext", "PP_27"), ("abouttext", "PP_29"), ("abouttitle", "PP_31")] {
            if let Some(s) = p.prop_src.get(k) {
                el.str(wire, &text_prop(s));
            }
        }
        if b("analysismodeenabled") == Some(false) {
            el.put("PP_47", false);
        }
        if let Some(a) = w("applicationarea") {
            el.str("PP_48", &app_area(a));
        }
        if let Some(u) = w("usagecategory").and_then(|s| enum_val(alprop::USAGE_CATEGORIES, s)) {
            el.num_bit("PP_49", u, 8);
        }
        if let Some(q) = p.prop_src.get("querycategory") {
            el.str("PP_50", &text_prop(q));
        }
        el.wrap()
    }

    fn source_object(&self) -> Value {
        let p = self.page;
        let b = |k: &str| p.props.get(k).and_then(|s| bool_lit(s));
        let mut el = El::new("SourceObjectDefinition");
        if let Some(v) = self.view_def(&p.view, self.table) {
            el.put("SOD_1", v);
        }
        if let Some(t) = self.table {
            el.num_bit("SOD_2", t.id, 0);
        }
        for (k, wire) in [("insertallowed", "SOD_3"), ("modifyallowed", "SOD_4"), ("linksallowed", "SOD_5"), ("deleteallowed", "SOD_6")] {
            if b(k) == Some(false) {
                el.put(wire, false);
            }
        }
        if let Some(v) = b("showfilter") {
            el.flag("SOD_7", v, 1);
        }
        if b("delayedinsert") == Some(true) {
            el.put("SOD_8", true);
        }
        let multiple = b("multiplenewlines");
        if let Some(v) = multiple {
            el.flag("SOD_9", v, 2);
        }
        if let Some(v) = b("savevalues") {
            el.flag("SOD_10", v, 3);
        }
        // solver: AutoSplitKey defaults to true when MultipleNewLines is set
        match b("autosplitkey") {
            Some(v) => el.flag("SOD_11", v, 4),
            None if multiple == Some(true) => el.flag("SOD_11", true, 4),
            None => {}
        }
        let temporary = b("sourcetabletemporary") == Some(true) || self.table.is_some_and(|t| t.temporary_only);
        if temporary {
            el.put("SOD_12", true);
        }
        if let (Some(t), Some(dcf)) = (self.table, p.prop_src.get("datacaptionfields")) {
            let ids: Vec<String> = dcf.split(',').filter_map(|n| field_of(t, n.trim().trim_matches('"'))).map(|f| f.id.to_string()).collect();
            if !ids.is_empty() {
                el.put("SOD_14", ids.join(","));
            }
        }
        el.wrap()
    }

    /// `ViewDefinition` of a `SourceTableView` / `SubPageView` / `RunPageView`.
    pub(crate) fn view_def(&self, v: &TableView, t: Option<&TableDef>) -> Option<Value> {
        if v.sorting.is_empty() && v.conds.is_empty() && !v.descending {
            return None;
        }
        let mut vd = El::new("ViewDefinition");
        if !v.sorting.is_empty() || v.descending {
            let mut s = El::new("ViewDefinitionSorting");
            let keys: Vec<String> = v.sorting.iter().filter_map(|n| t.and_then(|t| field_of(t, n))).map(|f| format!("Field{}", f.id)).collect();
            if !keys.is_empty() {
                s.put("VDS_1", keys.join(","));
            }
            if v.descending {
                s.put("VDS_2", false);
                s.flag("VDS_3", true, 0);
            }
            if !keys.is_empty() {
                s.flag("VDS_4", true, 1);
            }
            vd.put("VD_1", s.wrap());
        }
        vd.list_nonempty("VD_2", self.filter_defs(&v.conds, t, None));
        Some(vd.wrap())
    }

    /// `FilterDefinition` list: `FD_2` is a field of `target`, FIELD values refer to `source`.
    pub(crate) fn filter_defs(&self, conds: &[Cond], target: Option<&TableDef>, source: Option<&TableDef>) -> Vec<Value> {
        self.filter_defs_in(conds, target, source, None)
    }

    /// [`Self::filter_defs`] with `FD_1` FilterGroup: a SubPageLink is group 4 (BC 29.1
    /// metadata). Without it the web client looks the linked field up in the wrong
    /// group and fails with "The reference to the field 'Document No.' could not be
    /// resolved" on a new document.
    pub(crate) fn filter_defs_in(&self, conds: &[Cond], target: Option<&TableDef>, source: Option<&TableDef>, group: Option<i64>) -> Vec<Value> {
        conds
            .iter()
            .filter_map(|cd| {
                let tf = target.and_then(|t| field_of(t, &cd.field))?;
                let mut el = El::new("FilterDefinition");
                if let Some(g) = group {
                    el.num_bit("FD_1", g, 0);
                }
                el.num_bit("FD_2", tf.id, 1);
                el.num_bit("FD_3", table::filter_type(&cd.kind), 2);
                el.put("FD_4", table::cond_value(self.cat, cd, Some(tf), source));
                if cd.as_filter {
                    el.flag("FD_5", true, 3);
                }
                Some(el.wrap())
            })
            .collect()
    }

    // ------------------------------------------------------------------ variables and typing

    fn var(&self, name: &str) -> Option<&'a VarDecl> {
        self.codes.iter().find_map(|co| co.globals.iter().find(|v| v.name.eq_ignore_ascii_case(name)))
    }

    fn table_codes(&self, t: &TableDef) -> Vec<&'a CodeObject> {
        std::iter::once(t.code).chain(t.ext_code.iter().copied()).filter_map(|i| self.cat.code.get(i as usize)).collect()
    }

    fn proc_ret(codes: &[&'a CodeObject], name: &str) -> Option<&'a TypeRef> {
        codes.iter().find_map(|co| co.find(name).iter().map(|&i| &co.procs[i as usize]).find(|p| p.kind == ProcKind::Procedure).and_then(|p| p.ret.as_ref()))
    }

    fn record_table(&self, e: &Expr) -> Option<&'a TableDef> {
        let Expr::Ident(n) = e else { return None };
        if n.eq_ignore_ascii_case("rec") || n.eq_ignore_ascii_case("xrec") {
            return self.table;
        }
        match &self.var(n)?.ty {
            TypeRef::Object { kind, target, .. } if kind.eq_ignore_ascii_case("record") => self.cat.table_by_name(target),
            _ => None,
        }
    }

    fn field_col(&self, t: &TableDef, f: &FieldDef) -> ColType {
        let mut ct = ColType { dt: table::data_type(&f.ty), len: table::field_length(&f.ty), field: Some((t.id, f.id)), ..Default::default() };
        if let Some(o) = option_info(self.cat, f) {
            ct.names = o.names;
            ct.captions = o.captions;
            ct.values = o.values;
            ct.enum_id = o.enum_def.map(|e| e.id);
        }
        ct
    }

    fn type_ref_col(&self, t: &TypeRef) -> ColType {
        match t {
            TypeRef::Simple { name, len } => {
                let dt = match name.to_ascii_lowercase().as_str() {
                    "integer" => 0,
                    "decimal" => 5,
                    "option" => 6,
                    "boolean" => 7,
                    "byte" => 8,
                    "date" => 9,
                    "time" => 10,
                    "dateformula" => 13,
                    "biginteger" => 15,
                    "duration" => 16,
                    "guid" => 17,
                    "datetime" => 19,
                    "code" => 23,
                    "char" => 24,
                    _ => 21,
                };
                ColType { dt, len: len.unwrap_or(0), ..Default::default() }
            }
            TypeRef::Option(members) => {
                ColType { dt: 6, names: members.clone(), captions: members.clone(), values: (0..members.len() as i64).collect(), ..Default::default() }
            }
            TypeRef::Object { kind, target, .. } if kind.eq_ignore_ascii_case("enum") => match self.cat.enum_by_name(target) {
                Some(e) => {
                    let o = table::enum_options(e);
                    ColType { dt: 6, names: o.names, captions: o.captions, values: o.values, enum_id: Some(e.id), ..Default::default() }
                }
                None => ColType::of(6),
            },
            _ => ColType::of(21),
        }
    }

    fn expr_type(&self, e: &Expr) -> ColType {
        match e {
            Expr::Bool(_) => ColType::of(7),
            Expr::Int(_) => ColType::of(0),
            Expr::BigInt(_) => ColType::of(15),
            Expr::Dec(_) => ColType::of(5),
            Expr::Date(_) => ColType::of(9),
            Expr::Time(_) => ColType::of(10),
            Expr::DateTime(_) => ColType::of(19),
            Expr::Ident(n) => {
                if let Some(v) = self.var(n) {
                    let mut ct = self.type_ref_col(&v.ty);
                    ct.assignable = true;
                    return ct;
                }
                if let Some((t, f)) = self.table.and_then(|t| field_of(t, n).map(|f| (t, f))) {
                    return self.field_col(t, f);
                }
                Self::proc_ret(&self.codes, n).map(|r| self.type_ref_col(r)).unwrap_or_else(|| builtin_type(n))
            }
            Expr::Member(base, f) => {
                if let Some(t) = self.record_table(base) {
                    if let Some(fd) = field_of(t, f) {
                        let mut ct = self.field_col(t, fd);
                        ct.assignable = matches!(&**base, Expr::Ident(n) if !n.eq_ignore_ascii_case("xrec"));
                        return ct;
                    }
                    if let Some(r) = Self::proc_ret(&self.table_codes(t), f) {
                        return self.type_ref_col(r);
                    }
                }
                ColType::of(21)
            }
            Expr::Call(f, _) => match &**f {
                Expr::Ident(n) => Self::proc_ret(&self.codes, n).map(|r| self.type_ref_col(r)).unwrap_or_else(|| builtin_type(n)),
                Expr::Member(base, m) => match self.record_table(base) {
                    Some(t) => Self::proc_ret(&self.table_codes(t), m).map(|r| self.type_ref_col(r)).unwrap_or_else(|| ColType::of(21)),
                    None => ColType::of(21),
                },
                _ => ColType::of(21),
            },
            Expr::Unary(UnOp::Not, _) | Expr::Is(..) => ColType::of(7),
            Expr::Unary(_, x) => ColType { assignable: false, ..self.expr_type(x) },
            Expr::Binary(op, a, b) => match op {
                BinOp::And | BinOp::Or | BinOp::Xor | BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::In => ColType::of(7),
                _ => {
                    let (ta, tb) = (self.expr_type(a).dt, self.expr_type(b).dt);
                    let dt = if ta == 21 || tb == 21 || ta == 23 || tb == 23 {
                        21
                    } else if ta == 9 || ta == 19 {
                        ta
                    } else if ta == 0 && tb == 0 {
                        0
                    } else {
                        5
                    };
                    ColType::of(dt)
                }
            },
            _ => ColType::of(21),
        }
    }

    // ------------------------------------------------------------------ expression columns

    fn add_expr(&mut self, mut col: ExpressionColumn) -> String {
        if let Some(existing) = self.exprs.iter().find(|x| x.name.eq_ignore_ascii_case(&col.name)) {
            if existing.source.eq_ignore_ascii_case(&col.source) && existing.kind == col.kind {
                return existing.name.clone();
            }
            col.name = match col.control_id {
                Some(id) => format!("Control{id}_{}", col.name),
                None => format!("{}_{}", col.name, self.exprs.len()),
            };
        }
        let name = col.name.clone();
        self.exprs.push(col);
        name
    }

    fn column(name: String, source: String, kind: ExpressionKind, ct: ColType, control_id: Option<i32>, in_repeater: bool) -> ExpressionColumn {
        ExpressionColumn {
            name,
            source,
            kind,
            data_type: ct.dt,
            length: ct.len,
            control_id,
            in_repeater,
            table_field: ct.field,
            assignable: ct.assignable,
            option_names: ct.names,
            option_captions: ct.captions,
            option_values: ct.values,
            enum_id: ct.enum_id,
        }
    }

    /// Wire value of a Visible/Enabled/Editable-style property: literals as written, a page
    /// variable or table field name as is (with an expression column for the variable), and
    /// anything else as a generated `Control<id>_<Prop>` column holding the expression.
    pub(crate) fn dyn_prop(&mut self, c: &Control, key: &str, label: &str, id: i32, ctx: Ctx) -> Option<String> {
        let s = src(c, key)?.to_string();
        let e = alprop::expr_of(&s);
        match &e {
            Some(Expr::Bool(b)) => return Some(b.to_string()),
            None => {
                if let Some(b) = bool_lit(&s) {
                    return Some(b.to_string());
                }
            }
            Some(Expr::Ident(n)) => {
                if let Some(v) = self.var(n) {
                    let mut ct = self.type_ref_col(&v.ty);
                    ct.assignable = true;
                    let col = Self::column(v.name.clone(), ident_src(&v.name), ExpressionKind::PageVariable, ct, Some(id), ctx.in_repeater);
                    return Some(self.add_expr(col));
                }
                if let Some(f) = self.table.and_then(|t| field_of(t, n)) {
                    return Some(f.name.clone());
                }
            }
            _ => {}
        }
        let (source, ct) = match &e {
            Some(e) => (expr_src(e), self.expr_type(e)),
            None => (s.clone(), ColType::of(7)),
        };
        let ct = if label == "StyleExpr" { ct } else { ColType { dt: 7, ..Default::default() } };
        let col = Self::column(format!("Control{id}_{label}"), source, ExpressionKind::Property { property: label.to_string() }, ct, Some(id), ctx.in_repeater);
        Some(self.add_expr(col))
    }

    // ------------------------------------------------------------------ controls

    fn area_controls(&mut self, area: &str) -> Vec<Value> {
        let page = self.page;
        let mut out = vec![];
        for (i, a) in page.layout.iter().enumerate() {
            if a.kind == "area" && a.name.eq_ignore_ascii_case(area) {
                for (j, c) in a.children.iter().enumerate() {
                    if let Some(v) = self.control(c, &[i, j], Ctx::default()) {
                        out.push(v);
                    }
                }
            }
        }
        out
    }

    /// `area(RoleCenter)`: the parts go into a `ControlContainerPlaceHolder` with
    /// `ContainerType = RoleCenterArea (2)`, which the web client's homepage builder looks for.
    fn role_center_area(&mut self) -> Vec<Value> {
        let page = self.page;
        let mut out = vec![];
        for (i, a) in page.layout.iter().enumerate() {
            if a.kind != "area" || !a.name.eq_ignore_ascii_case("rolecenter") {
                continue;
            }
            // the real NST sends this container without id or name and with a fixed GUID
            let mut el = El::new("ControlContainerPlaceHolder");
            el.put("UIED_5", "09df02e8-d8d3-4188-bf67-871320a3645e");
            let kids = self.children(a, &[i], Ctx::default());
            el.list("CCPH_1", kids);
            el.num_bit("CCPH_2", 2, 9);
            out.push(el.wrap());
        }
        out
    }

    fn children(&mut self, c: &Control, path: &[usize], ctx: Ctx) -> Vec<Value> {
        let mut out = vec![];
        for (i, k) in c.children.iter().enumerate() {
            let mut p = path.to_vec();
            p.push(i);
            if let Some(v) = self.control(k, &p, ctx) {
                out.push(v);
            }
        }
        out
    }

    fn control(&mut self, c: &Control, path: &[usize], ctx: Ctx) -> Option<Value> {
        let e = self.ix.at(Section::Layout, path)?.clone();
        Some(match c.kind.as_str() {
            "group" => self.group(c, path, &e, ctx, "ControlGroupDefinition"),
            "grid" => self.group(c, path, &e, ctx, "GridLayoutDefinition"),
            "fixed" => self.group(c, path, &e, ctx, "ColumnLayoutDefinition"),
            "cuegroup" => self.group(c, path, &e, ctx, "StackGroupDefinition"),
            "repeater" => self.repeater(c, path, &e, ctx),
            "field" | "label" | "usercontrol" => self.field(c, &e, ctx),
            "part" => self.part(c, &e, ctx),
            // Notes and Links factboxes only with the captured parts (data/nst-templates): from bc-nst's
            // own part the web server's LinksListBuilder fails (no binding manager), and that error
            // abandons the web session, which signs the user out on the next page load
            "systempart" if !super::system_part_available(c.part_page.as_deref().unwrap_or("")) => return None,
            "systempart" => self.system_part(c, &e, ctx),
            _ => return None,
        })
    }

    /// IDED/NED/UIED members shared by controls and actions.
    pub(crate) fn ui_base(&mut self, el: &mut El, c: &Control, e: &ControlEntry, ctx: Ctx, caption: &str) {
        el.num_bit("IDED_1", e.id as i64, 0);
        el.put("NED_1", c.name.as_str());
        el.str("NED_3", caption);
        let visible = if ctx.hidden { Some("0".to_string()) } else { self.dyn_prop(c, "visible", "Visible", e.id, ctx) };
        if let Some(v) = visible.filter(|v| !v.eq_ignore_ascii_case("true")) {
            el.put("UIED_2", v);
        }
        el.put("UIED_5", e.guid.to_string());
        if let Some(v) = self.dyn_prop(c, "enabled", "Enabled", e.id, ctx).filter(|v| !v.eq_ignore_ascii_case("true")) {
            el.put("UIED_6", v);
        }
    }

    pub(crate) fn about(el: &mut El, c: &Control) {
        if let Some(t) = ctext(c, "abouttext") {
            el.put("CBD_15", t);
        }
        if let Some(t) = ctext(c, "abouttitle") {
            el.put("CBD_17", t);
        }
    }

    fn group(&mut self, c: &Control, path: &[usize], e: &ControlEntry, ctx: Ctx, ty: &'static str) -> Value {
        let mut el = El::new(ty);
        // The solver falls back to the name for an empty caption. bc-rust applies that only
        // to top-level groups (FastTabs); nested groups without a caption stay unlabeled.
        let caption = c.caption.clone().unwrap_or_else(|| if ctx.depth == 0 && ty == "ControlGroupDefinition" { c.name.clone() } else { String::new() });
        self.ui_base(&mut el, c, e, ctx, &caption);
        Self::about(&mut el, c);
        if let Some(t) = ctext(c, "instructionaltext") {
            el.put("CGBD_2", t);
        }
        if let Some(v) = self.dyn_prop(c, "editable", "Editable", e.id, ctx).filter(|v| !v.eq_ignore_ascii_case("true")) {
            el.put("CGBD_4", v);
        }
        if cbool(c, "showcaption") == Some(false) {
            el.put("CGBD_5", false);
        }
        match ty {
            "GridLayoutDefinition" => {
                if let Some(l) = word(c, "gridlayout").and_then(|s| enum_val(alprop::GRID_LAYOUTS, s)) {
                    el.num_bit("GLD_1", l, 9);
                }
            }
            "StackGroupDefinition" => {
                if let Some(l) = word(c, "layout").and_then(|s| enum_val(alprop::GRID_LAYOUTS, s)) {
                    el.num_bit("SGD_1", l, 9);
                }
            }
            _ => {}
        }
        let hidden = ctx.hidden || cbool(c, "visible") == Some(false);
        let kids = self.children(c, path, Ctx { hidden, depth: ctx.depth + 1, ..ctx });
        el.list("CGBD_1", kids);
        el.wrap()
    }

    fn repeater(&mut self, c: &Control, path: &[usize], e: &ControlEntry, ctx: Ctx) -> Value {
        let mut el = El::new("RepeaterDefinition");
        let caption = c.caption.clone().unwrap_or_else(|| c.name.clone());
        self.ui_base(&mut el, c, e, ctx, &caption);
        if let Some(v) = self.dyn_prop(c, "editable", "Editable", e.id, ctx).filter(|v| !v.eq_ignore_ascii_case("true")) {
            el.put("CGBD_4", v);
        }
        if cbool(c, "showcaption") == Some(false) {
            el.put("CGBD_5", false);
        }
        if let Some(v) = cbool(c, "showastree") {
            el.flag("RD_1", v, 9);
        }
        if let Some(s) = word(c, "treeinitialstate").and_then(|s| enum_val(alprop::TREE_STATES, s)).filter(|&v| v != 0) {
            el.put("RD_2", s);
        }
        if let Some(id) = src(c, "freezecolumn")
            .map(|s| s.trim_matches(|ch| ch == '(' || ch == ')').trim().trim_matches('"').to_string())
            .and_then(|n| self.ix.id_of(Section::Layout, &n))
        {
            el.num_bit("RD_3", id as i64, 10);
        }
        if let Some(s) = src(c, "indentationcolumn").map(str::to_string) {
            let col = match alprop::expr_of(&s) {
                Some(Expr::Member(b, f)) if matches!(&*b, Expr::Ident(r) if r.eq_ignore_ascii_case("rec")) => {
                    self.table.and_then(|t| field_of(t, &f)).map(|f| f.id.to_string())
                }
                Some(Expr::Ident(n)) => match (self.table.and_then(|t| field_of(t, &n)), self.var(&n)) {
                    (Some(f), _) => Some(f.id.to_string()),
                    // a page variable set per row (Chart of Accounts: `NameIndent := Rec.Indentation`
                    // in OnAfterGetRecord): the client reads it from every row to compute the tree
                    // depth and closes the page if no column has that name
                    (None, Some(v)) => {
                        let mut ct = self.type_ref_col(&v.ty);
                        ct.assignable = true;
                        let col = Self::column(v.name.clone(), ident_src(&v.name), ExpressionKind::PageVariable, ct, Some(e.id), true);
                        Some(self.add_expr(col))
                    }
                    (None, None) => Some(n.to_string()),
                },
                _ => None,
            };
            if let Some(col) = col {
                el.put("RD_4", col);
            }
        }
        if let Some(s) = src(c, "indentationcontrols") {
            // control ids (TreeControlBuilder.GetIndentationControlIDs parses each as an int)
            let ids: Vec<String> = s
                .split(',')
                .filter_map(|n| self.ix.id_of(Section::Layout, n.trim().trim_matches('"')))
                .map(|id| id.to_string())
                .collect();
            if !ids.is_empty() {
                el.put("RD_5", ids.join(","));
            }
        }
        let hidden = ctx.hidden || cbool(c, "visible") == Some(false);
        let kids = self.children(c, path, Ctx { in_repeater: true, hidden, depth: ctx.depth + 1 });
        el.list("CGBD_1", kids);
        el.wrap()
    }

    /// The table field a field control is bound to (`Rec."No."` or a bare field name).
    pub(crate) fn bound_field(&self, c: &Control) -> Option<&'a FieldDef> {
        let t = self.table?;
        if c.kind != "field" {
            return None;
        }
        if let Some(i) = c.field {
            return t.fields.get(i);
        }
        let name = match c.source_expr.as_ref()? {
            Expr::Member(b, f) if matches!(&**b, Expr::Ident(r) if r.eq_ignore_ascii_case("rec")) => f.to_string(),
            Expr::Ident(f) if self.var(f).is_none() => f.to_string(),
            _ => return None,
        };
        field_of(t, &name)
    }

    fn control_relations(&self, c: &Control) -> Vec<RelationBranch> {
        src(c, "tablerelation").map(|s| al_model::parse_table_relation(&alprop::lex_src(s))).unwrap_or_default()
    }

    fn table_relation_defs(&self, rels: &[RelationBranch]) -> Vec<Value> {
        rels.iter()
            .filter_map(|b| {
                let (tt, tf) = table::relation_target(self.cat, b)?;
                let mut el = El::new("TableRelationDefinition");
                let conds: Vec<Value> = b
                    .when
                    .iter()
                    .filter_map(|cd| {
                        let sf = self.table.and_then(|t| field_of(t, &cd.field))?;
                        let mut c0 = El::new("ConditionDefinition");
                        c0.num_bit("CD0_1", sf.id, 0);
                        c0.num_bit("CD0_2", table::filter_type(&cd.kind), 1);
                        c0.put("CD0_3", table::cond_value(self.cat, cd, Some(sf), self.table));
                        Some(c0.wrap())
                    })
                    .collect();
                el.list_nonempty("TRD_1", conds);
                el.list_nonempty("TRD_2", self.filter_defs(&b.conds, Some(tt), self.table));
                el.num_bit("TRD_3", tt.id, 0);
                el.put("TRD_4", tt.name.as_str());
                if let Some(tf) = tf {
                    el.num_bit("TRD_5", tf.id, 1);
                }
                Some(el.wrap())
            })
            .collect()
    }

    fn related_object(ty: i64, id: i64, mask: i64, by: i64) -> Value {
        let mut el = El::new("RelatedObjectDefinition");
        el.num_bit("ROD_1", ty, 0);
        el.num_bit("ROD_2", id, 1);
        el.num_bit("ROD_3", mask, 2);
        el.num_bit("ROD_4", by, 3);
        el.wrap()
    }

    /// Data column and assignability of a field control (`CDD_11`, `CDD_15`).
    fn data_column(&mut self, c: &Control, f: Option<&FieldDef>, e: &ControlEntry, ctx: Ctx) -> (Option<String>, bool) {
        if let Some(f) = f {
            return (Some(f.id.to_string()), true);
        }
        if c.kind != "field" {
            return (None, true);
        }
        let Some(expr) = c.source_expr.clone() else { return (None, false) };
        let ct = self.expr_type(&expr);
        let assignable = ct.assignable;
        let name = match &expr {
            Expr::Ident(n) => self.var(n).map(|v| v.name.clone()).unwrap_or_else(|| n.to_string()),
            _ => c.name.clone(),
        };
        let col = Self::column(name, expr_src(&expr), ExpressionKind::SourceExpression, ct, Some(e.id), ctx.in_repeater);
        (Some(self.add_expr(col)), assignable)
    }

    fn field(&mut self, c: &Control, e: &ControlEntry, ctx: Ctx) -> Value {
        let id = e.id;
        let f = self.bound_field(c);
        let mut el = El::new("ControlDefinition");
        let show_caption = cbool(c, "showcaption") != Some(false);
        // solver: explicit caption, else the field caption, else the name (empty without ShowCaption)
        let caption = c.caption.clone().filter(|s| !s.is_empty()).or_else(|| f.map(|f| f.caption.clone())).unwrap_or_else(|| {
            if show_caption {
                c.name.clone()
            } else {
                String::new()
            }
        });
        self.ui_base(&mut el, c, e, ctx, &caption);

        // --- ControlBaseDefinition
        let cc_ctl = src(c, "captionclass").map(text_prop);
        let cc_field = f.and_then(|f| fprop(f, "captionclass")).map(str::to_string);
        if let Some(cc) = cc_ctl.clone().or(cc_field) {
            let name = format!("Control{id}_DynamicCaption");
            let col = Self::column(name, cc, ExpressionKind::DynamicCaption, ColType::of(21), Some(id), ctx.in_repeater);
            let name = self.add_expr(col);
            el.put("CBD_3", name);
            if let Some(cc) = cc_ctl {
                el.put("CBD_4", cc);
            }
        }
        let trig = |n: &str| c.triggers.contains_key(n);
        let ctl_rels = self.control_relations(c);
        let drill = cbool(c, "drilldown")
            .unwrap_or_else(|| trig("ondrilldown") || src(c, "drilldownpageid").is_some() || f.is_some_and(|f| f.class == FieldClass::FlowField));
        el.flag("CBD_5", drill, 2);
        let lookup = cbool(c, "lookup")
            .unwrap_or_else(|| trig("onlookup") || f.is_some_and(|f| f.on_lookup.is_some() || !f.relation.is_empty()) || !ctl_rels.is_empty());
        el.flag("CBD_6", lookup, 3);
        el.flag("CBD_7", cbool(c, "assistedit").unwrap_or_else(|| trig("onassistedit")), 4);
        if let Some(f) = f {
            el.flag("CBD_8", meta_type_code(&f.ty) == META_OPTION, 5);
            el.flag("CBD_9", fbool(f, "closingdates") == Some(true), 6);
            el.flag("CBD_10", fbool(f, "numeric") == Some(true), 7);
            if matches!(f.ty, FieldType::DateFormula) {
                el.put("CBD_11", true);
            }
        }
        Self::about(&mut el, c);

        // --- ControlDataboundDefinition
        let trds = self.table_relation_defs(&ctl_rels);
        el.list_nonempty("CDD_1", trds);
        let mut rods = vec![];
        let abp = src(c, "accessbypermission").and_then(|s| alprop::access_by_permission(self.cat, s));
        if let Some((ty, oid, mask)) = abp.or_else(|| f.and_then(|f| fprop(f, "accessbypermission")).and_then(|s| alprop::access_by_permission(self.cat, s))) {
            if mask != 0 {
                rods.push(Self::related_object(ty, oid, mask, 3));
            }
        }
        if let Some(f) = f {
            if let Some(ct) = table::calc_table(self.cat, f) {
                rods.push(Self::related_object(5, ct.id, 32, 2));
            }
            if ctl_rels.is_empty() {
                for b in &f.relation {
                    if let Some((tt, _)) = table::relation_target(self.cat, b) {
                        rods.push(Self::related_object(5, tt.id, 32, 1));
                    }
                }
            }
        }
        el.list_nonempty("CDD_2", rods);
        let mut trigs: Vec<(i64, String)> = c
            .triggers
            .iter()
            .map(|(k, r)| {
                let name = self.cat.proc(*r).name.clone();
                let ty = CONTROL_TRIGGERS.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|&(_, t)| t).unwrap_or(4);
                (ty, name)
            })
            .collect();
        trigs.sort();
        let ctds: Vec<Value> = trigs
            .into_iter()
            .map(|(ty, name)| {
                let mut t = El::new("ControlTriggerDefinition");
                t.num_bit("CTD_1", ty, 0);
                t.put("CTD_2", name);
                t.wrap()
            })
            .collect();
        el.list_nonempty("CDD_3", ctds);
        if let Some(s) = word(c, "style").and_then(|s| enum_val(alprop::STYLES, s)) {
            el.num_bit("CDD_4", s, 9);
        }
        if let Some(v) = self.dyn_prop(c, "styleexpr", "StyleExpr", id, ctx) {
            el.put("CDD_5", v);
        }
        let ext = word(c, "extendeddatatype")
            .and_then(|s| enum_val(alprop::EXTENDED_DATATYPES, s))
            .or_else(|| f.map(|f| fprop(f, "extendeddatatype").and_then(|s| enum_val(alprop::EXTENDED_DATATYPES, s)).unwrap_or(1)));
        if let Some(x) = ext {
            el.num_bit("CDD_6", x, 10);
        }
        if let Some(f) = f {
            el.num_bit("CDD_7", fprop(f, "masktype").and_then(|s| enum_val(alprop::MASK_TYPES, s)).unwrap_or(0), 11);
        }
        if let Some(w) = word(c, "width").and_then(|s| s.parse::<i64>().ok()) {
            el.num_bit("CDD_8", w, 12);
        }
        if let Some(v) = self.dyn_prop(c, "showmandatory", "ShowMandatory", id, ctx).filter(|v| !v.eq_ignore_ascii_case("false")) {
            el.put("CDD_9", v);
        }
        let (column, assignable) = self.data_column(c, f, e, ctx);
        if let Some(col) = column {
            el.put("CDD_11", col);
        }
        // a decimal control gets its format and currency columns (the real NST's compiled page
        // metadata: `CD2_2` / `CD2_3` and two `MP_10` entries, DataType OemText / Text)
        let decimal = match f {
            Some(f) => matches!(f.ty, FieldType::Decimal),
            None => c.kind == "field" && c.source_expr.as_ref().is_some_and(|x| self.expr_type(x).dt == 5),
        };
        let format_cols = decimal.then(|| {
            let fmt = Self::column(format!("Control{id}_Format"), format!("Control{id}_Format"), ExpressionKind::AutoFormat, ColType::of(1), Some(id), ctx.in_repeater);
            let cur = Self::column(format!("Control{id}_CurrencyCode"), format!("Control{id}_CurrencyCode"), ExpressionKind::CurrencyCode, ColType::of(21), Some(id), ctx.in_repeater);
            (self.add_expr(fmt), self.add_expr(cur))
        });
        // solver: SolveEditable
        el.flag("CDD_13", f.map(|f| f.editable).unwrap_or(true), 13);
        let aic = f
            .and_then(|f| fprop(f, "allowincustomizations"))
            .and_then(|s| enum_val(&[("ToBeClassified", 0), ("Never", 1), ("AsReadOnly", 2), ("AsReadWrite", 3)], s))
            .unwrap_or(0);
        el.num_bit("CDD_14", aic, 14);
        let editable = if !assignable {
            el.put("CDD_15", false);
            "False".to_string()
        } else {
            match self.dyn_prop(c, "editable", "Editable", id, ctx) {
                None => {
                    if f.is_none_or(|f| f.editable) {
                        "True".into()
                    } else {
                        "False".into()
                    }
                }
                Some(v) if bool_lit(&v) != Some(false) && !self.page_editable => "False".into(),
                Some(v) => v,
            }
        };
        el.put("CDD_12", editable);
        let (min, max) = self.min_max(c, f);
        if let Some(v) = min {
            el.put("CDD_16", v);
        }
        if let Some(v) = max {
            el.put("CDD_17", v);
        }
        // solver: SolveDataboundControlImage turns a missing image into ""
        el.put(
            "CDD_18",
            match word(c, "image") {
                None => String::new(),
                Some(i) if i.eq_ignore_ascii_case("stack") => String::new(),
                Some(i) if i.eq_ignore_ascii_case("none") => String::new(),
                Some(i) => format!("Cue_{i}_56x46"),
            },
        );

        // --- ControlDefinition
        if let Some((fmt, cur)) = format_cols {
            el.put("CD2_2", fmt);
            el.put("CD2_3", cur);
        }
        if let Some(d) = src(c, "decimalplaces") {
            el.put("CD2_4", d);
        }
        if let Some(oc) = self.option_caption(c, f) {
            el.put("CD2_6", oc);
        }
        if let Some(t) = ctext(c, "tooltip").or_else(|| f.and_then(|f| f.tooltip.clone())) {
            el.put("CD2_8", t);
        }
        let not_blank = cbool(c, "notblank") == Some(true) || f.is_some_and(|f| f.not_blank);
        if f.is_some() || not_blank {
            el.flag("CD2_11", not_blank, 17);
        }
        if let Some(s) = ctext(c, "charallowed").or_else(|| f.and_then(|f| fprop(f, "charallowed")).map(str::to_string)) {
            el.put("CD2_12", s);
        }
        if let Some(s) = ctext(c, "valuesallowed").or_else(|| f.and_then(|f| fprop(f, "valuesallowed")).map(str::to_string)) {
            el.put("CD2_13", s);
        }
        let bn = word(c, "blanknumbers")
            .and_then(|s| enum_val(alprop::BLANK_NUMBERS, s))
            .or_else(|| f.map(|f| fprop(f, "blanknumbers").and_then(|s| enum_val(alprop::BLANK_NUMBERS, s)).unwrap_or(0)));
        if let Some(bn) = bn {
            el.num_bit("CD2_14", bn, 18);
        }
        let bz = cbool(c, "blankzero").or_else(|| f.map(|f| f.blank_zero));
        if let Some(bz) = bz {
            el.flag("CD2_15", bz, 19);
        }
        if cbool(c, "multiline") == Some(true) {
            el.put("CD2_16", true);
        }
        if let Some(i) = word(c, "importance").and_then(|s| enum_val(alprop::IMPORTANCE, s)) {
            el.num_bit("CD2_17", i, 20);
        }
        if let Some(id) = table::page_id_of(self.cat, src(c, "drilldownpageid")) {
            el.num_bit("CD2_18", id, 21);
        }
        if let Some(id) = table::page_id_of(self.cat, src(c, "lookuppageid")) {
            el.num_bit("CD2_19", id, 22);
        }
        if let Some(v) = self.dyn_prop(c, "hidevalue", "HideValue", id, ctx).filter(|v| !v.eq_ignore_ascii_case("false")) {
            el.put("CD2_21", v);
        }
        if c.kind == "usercontrol" {
            if let Some(t) = &c.target {
                el.put("CD2_22", t.trim_matches('"'));
            }
        }
        if let Some((ty, oid, mask)) = abp {
            el.num_bit("CD2_23", oid, 24);
            el.num_bit("CD2_24", ty, 25);
            el.num_bit("CD2_25", mask, 26);
        }
        if let Some(a) = word(c, "applicationarea") {
            el.str("CD2_26", &app_area(a));
        }
        if let Some(v) = self.dyn_prop(c, "quickentry", "QuickEntry", id, ctx).filter(|v| !v.eq_ignore_ascii_case("true")) {
            el.put("CD2_27", v);
        }
        if !show_caption {
            el.put("CD2_28", false);
        }
        for (k, wire) in [("rowspan", "CD2_29"), ("columnspan", "CD2_30")] {
            if let Some(n) = word(c, k).and_then(|s| s.parse::<i64>().ok()).filter(|&n| n != 1) {
                el.put(wire, n);
            }
        }
        if let Some(t) = ctext(c, "instructionaltext") {
            el.put("CD2_31", t);
        }
        el.wrap()
    }

    /// solver: SolveOptionString — option names overridden by the field's option captions,
    /// then by the control's `OptionCaption`.
    fn option_caption(&self, c: &Control, f: Option<&FieldDef>) -> Option<String> {
        let o = option_info(self.cat, f?)?;
        let mut arr: Vec<String> = o.names.clone();
        let override_with = |arr: &mut Vec<String>, caps: &[String]| {
            if caps.is_empty() {
                return;
            }
            for (i, t) in arr.iter_mut().enumerate() {
                if !t.is_empty() {
                    *t = caps.get(i).cloned().unwrap_or_default();
                }
            }
        };
        override_with(&mut arr, &o.captions);
        if let Some(oc) = ctext(c, "optioncaption") {
            let caps: Vec<String> = oc.split(',').map(str::to_string).collect();
            override_with(&mut arr, &caps);
        }
        Some(arr.join(","))
    }

    /// solver: SolveMinMax — the tighter of control and field bounds for numeric types.
    fn min_max(&self, c: &Control, f: Option<&FieldDef>) -> (Option<String>, Option<String>) {
        let cmin = word(c, "minvalue").map(str::to_string);
        let cmax = word(c, "maxvalue").map(str::to_string);
        let ratio = word(c, "extendeddatatype").is_some_and(|s| s.eq_ignore_ascii_case("ratio"));
        let Some(f) = f else {
            if ratio && cmax.is_none() {
                return (cmin, Some("10000".into()));
            }
            return (cmin, cmax);
        };
        let fmin = fprop(f, "minvalue").map(str::to_string);
        let fmax = fprop(f, "maxvalue").map(str::to_string);
        let numeric = |a: Option<String>, b: Option<String>, low: bool| -> Option<String> {
            let pa = a.as_deref().and_then(|s| s.trim().parse::<rust_decimal::Decimal>().ok());
            let pb = b.as_deref().and_then(|s| s.trim().parse::<rust_decimal::Decimal>().ok());
            match (pa, pb) {
                (Some(x), Some(y)) => Some(if low { x.max(y) } else { x.min(y) }.to_string()),
                (Some(x), None) => Some(x.to_string()),
                (None, Some(y)) => Some(y.to_string()),
                (None, None) => a.or(b),
            }
        };
        match f.ty {
            FieldType::Integer | FieldType::BigInteger | FieldType::Option | FieldType::Enum(_) | FieldType::Duration | FieldType::Decimal => {
                let max = numeric(cmax, fmax, false).or_else(|| ratio.then(|| "10000".to_string()));
                (numeric(cmin, fmin, true), max)
            }
            _ => (cmin.or(fmin), cmax.or(fmax)),
        }
    }

    fn part(&mut self, c: &Control, e: &ControlEntry, ctx: Ctx) -> Value {
        let cat = self.cat;
        let pp = c.part_page.as_deref().and_then(|n| cat.page_by_name(n));
        let pt = pp.and_then(|p| p.source_table).and_then(|id| cat.tables.get(&id));
        let mut el = El::new("InfopartPageDefinition");
        // parts are not caption-solved by the NST; the part page's caption is a guess
        let caption = c.caption.clone().or_else(|| pp.map(|p| p.caption.clone())).unwrap_or_default();
        self.ui_base(&mut el, c, e, ctx, &caption);
        Self::about(&mut el, c);
        // FIELD() values of a factbox with a Provider refer to the provider part's table
        // (e.g. the Sales Line Details factbox on page 42 links to the SalesLines part)
        let provider_table = word(c, "provider")
            .and_then(|n| self.ix.find(Section::Layout, n.trim_matches('"')))
            .and_then(|pe| self.ix.control(self.page, pe))
            .and_then(|pc| pc.part_page.as_deref())
            .and_then(|n| cat.page_by_name(n))
            .and_then(|p| p.source_table)
            .and_then(|id| cat.tables.get(&id));
        let link_source = provider_table.or(self.table);
        el.list_nonempty("IPD_1", self.filter_defs_in(&c.sub_page_link, pt, link_source, Some(4)));
        if let Some(s) = src(c, "subpageview") {
            let v = al_model::parse_table_view(&alprop::lex_src(s));
            if let Some(vd) = self.view_def(&v, pt) {
                el.put("IPD_2", vd);
            }
        }
        if let Some(v) = self.dyn_prop(c, "editable", "Editable", e.id, ctx).filter(|v| !v.eq_ignore_ascii_case("true")) {
            el.put("IPD_3", v);
        }
        if cbool(c, "showfilter") == Some(false) {
            el.put("IPD_4", "false");
        }
        if let Some(id) = word(c, "provider").and_then(|n| self.ix.id_of(Section::Layout, n.trim_matches('"'))) {
            el.num_bit("IPD_5", id as i64, 9);
        }
        if let Some(p) = pp {
            el.num_bit("IPD_6", p.id, 10);
        }
        if let Some((ty, oid, mask)) = src(c, "accessbypermission").and_then(|s| alprop::access_by_permission(cat, s)) {
            el.num_bit("IPD_7", oid, 11);
            el.num_bit("IPD_8", ty, 12);
            el.num_bit("IPD_9", mask, 13);
        }
        if let Some(a) = word(c, "applicationarea") {
            el.str("IPD_10", &app_area(a));
        }
        if let Some(t) = pt {
            el.num_bit("IPD_11", t.id, 14);
        }
        if let Some(u) = word(c, "updatepropagation").and_then(|s| enum_val(alprop::UPDATE_PROPAGATION, s)) {
            el.num_bit("IPD_12", u, 15);
        }
        el.wrap()
    }

    fn system_part(&mut self, c: &Control, e: &ControlEntry, ctx: Ctx) -> Value {
        let mut el = El::new("InfopartSystemDefinition");
        self.ui_base(&mut el, c, e, ctx, c.caption.as_deref().unwrap_or(""));
        if let Some(t) = c.part_page.as_deref().and_then(|s| enum_val(alprop::SYSTEM_PARTS, s)) {
            el.num_bit("ISD_1", t, 9);
        }
        if let Some(u) = word(c, "updatepropagation").and_then(|s| enum_val(alprop::UPDATE_PROPAGATION, s)) {
            el.num_bit("ISD_2", u, 10);
        }
        if let Some(a) = word(c, "applicationarea") {
            el.str("ISD_3", &app_area(a));
        }
        el.wrap()
    }
}

/// Result type of common AL built-in functions used as control sources.
fn builtin_type(n: &str) -> ColType {
    let dt = match n.to_ascii_lowercase().as_str() {
        "today" | "workdate" | "calcdate" | "dmy2date" | "dt2date" | "closingdate" | "normaldate" => 9,
        "time" | "dt2time" => 10,
        "currentdatetime" | "createdatetime" => 19,
        "abs" | "round" | "power" => 5,
        "strlen" | "strpos" | "date2dmy" | "date2dwy" | "maxstrlen" => 0,
        "isnullguid" => 7,
        "createguid" => 17,
        _ => 21,
    };
    ColType::of(dt)
}

/// `DataFieldDefinition` (`MP_10` entry) of an expression column.
fn dfd(x: &ExpressionColumn) -> Value {
    let mut el = El::new("DataFieldDefinition");
    el.put("DFD_1", x.name.as_str());
    el.num_bit("DFD_2", x.data_type, 0);
    if x.length > 0 {
        el.num_bit("DFD_3", x.length as i64, 1);
    }
    if !x.option_names.is_empty() {
        el.put("DFD_4", x.option_names.join(","));
    }
    if let Some(id) = x.enum_id {
        el.num_bit("DFD_5", id, 2);
    }
    // `DFD_6` (IsDataSetVar) makes the client type the column as a UserDefinedVariable, i.e.
    // a form variable. Its expression engine requires that (or a table field) for every
    // column a Visible/Enabled/Editable/Style expression reads when the control is bound to
    // a page variable or source expression; otherwise it throws "Column binder: X is
    // expected to be of a form variable type" and the web server drops the session (role
    // center part 6325, `Visible = not LockedToFirstElement`). The real NST writes such
    // expressions inline over page-variable columns (DFD_6) and sets DFD_6 on the per-control
    // columns it synthesizes itself (`Control<id>_DynamicCueStyle`).
    if matches!(x.kind, ExpressionKind::PageVariable | ExpressionKind::Property { .. }) {
        el.flag("DFD_6", true, 3);
    }
    if x.assignable {
        el.flag("DFD_7", true, 4);
    }
    if !matches!(x.kind, ExpressionKind::DynamicCaption | ExpressionKind::CurrencyCode) {
        el.str("DFD_8", &x.source);
    }
    if x.in_repeater {
        el.flag("DFD_9", true, 5);
    }
    if let Some((t, f)) = x.table_field {
        el.num_bit("DFD_10", t, 6);
        el.num_bit("DFD_11", f, 7);
    }
    match &x.kind {
        ExpressionKind::Property { .. } | ExpressionKind::AutoFormat | ExpressionKind::CurrencyCode => el.num_bit("DFD_12", 3, 8),
        ExpressionKind::PageVariable => el.num_bit("DFD_12", 5, 8),
        _ => {}
    }
    if !x.option_captions.is_empty() {
        el.put("DFD_14", x.option_captions.join(","));
    }
    if !x.option_values.is_empty() {
        el.put("DFD_15", x.option_values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","));
    }
    el.wrap()
}
