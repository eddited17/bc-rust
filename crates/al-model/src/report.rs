//! Report metadata: the dataset (data item tree with views, links and triggers), the request page
//! (as a [`PageDef`] whose code is the report's) and report extensions merged into their base.
//!
//! Every data item also becomes a global record variable of the code object that declares it, named
//! like the data item, so report code can address `"Sales Header".SetRange(...)` and the runtime
//! iterates exactly that record.

use crate::catalog::*;
use crate::filters::*;
use crate::props::*;
use al_syntax::*;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
pub struct ReportColumn {
    pub name: String,
    pub source: String,
    #[serde(skip)]
    pub expr: Option<Expr>,
    /// Code object (report or report extension) whose context evaluates the expression.
    pub code: u32,
    /// IncludeCaption: the caption becomes the report label `<name>Caption`.
    pub include_caption: bool,
    pub caption: Option<String>,
    pub auto_format_type: Option<i64>,
    #[serde(skip)]
    pub auto_format_expr: Option<Expr>,
    pub decimal_places: Option<String>,
    pub option_caption: Option<String>,
    /// AutoCalcField (default true): a FlowField source of the data item's record is calculated
    /// for each record.
    pub auto_calc: bool,
}

/// A report label (`labels { Name = 'Caption'; }`).
#[derive(Debug, Clone, Serialize)]
pub struct ReportLabel {
    pub name: String,
    pub caption: String,
}

fn report_column(c: &Element, code: u32) -> ReportColumn {
    let b = &c.body;
    ReportColumn {
        name: arg_name(c, 0).unwrap_or_default(),
        source: c.args.get(1).map(|a| tokens_text(a)).unwrap_or_default(),
        expr: c.args.get(1).and_then(|a| parse_expr(a).ok()),
        code,
        include_caption: prop_bool(b, "IncludeCaption").unwrap_or(false),
        caption: prop_str(b, "Caption"),
        auto_format_type: prop(b, "AutoFormatType").and_then(|p| tokens_text(&p.value).parse().ok()),
        auto_format_expr: prop(b, "AutoFormatExpression").and_then(|p| parse_expr(&p.value).ok()),
        decimal_places: prop(b, "DecimalPlaces").map(|p| tokens_text(&p.value)),
        option_caption: prop_str(b, "OptionCaption"),
        auto_calc: prop_bool(b, "AutoCalcField").unwrap_or(true),
    }
}

/// `labels { Name = 'Caption', Comment = '...'; }`
fn report_labels(el: &Element) -> Vec<ReportLabel> {
    props(&el.body)
        .map(|p| ReportLabel { name: p.name.clone(), caption: p.value.iter().find(|t| t.kind == Tk::Str).map(|t| t.text.to_string()).unwrap_or_default() })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportDataItem {
    pub name: String,
    /// Table id (None when the table is not loaded).
    pub table: Option<i64>,
    pub parent: Option<usize>,
    /// Child data items in run order (indices into `ReportDef::dataitems`).
    pub children: Vec<usize>,
    /// DataItemTableView.
    pub view: TableView,
    /// DataItemLink: `field(X)` refers to a field of the linked (parent) data item's record.
    pub links: Vec<Cond>,
    /// DataItemLinkReference (default: the parent data item).
    pub link_ref: Option<String>,
    pub request_filter_fields: Vec<String>,
    /// UseTemporary.
    pub temporary: bool,
    pub max_iteration: Option<i64>,
    pub print_only_if_detail: bool,
    /// CalcFields: FlowFields calculated for every record.
    pub calc_fields: Vec<String>,
    /// onpredataitem, onaftergetrecord, onpostdataitem.
    pub triggers: HashMap<String, ProcRef>,
    /// Report extension `modify(...)` triggers: (lower-cased name, proc), e.g. onbeforeaftergetrecord.
    pub ext_triggers: Vec<(String, ProcRef)>,
    pub columns: Vec<ReportColumn>,
    /// Code object (report or report extension) that declares the data item's record variable.
    pub code: u32,
}

#[derive(Debug, Serialize)]
pub struct ReportDef {
    pub id: i64,
    pub name: String,
    pub caption: String,
    pub code: u32,
    pub ext_code: Vec<u32>,
    pub processing_only: bool,
    /// UseRequestPage property (default true).
    pub use_request_page: bool,
    pub props: HashMap<String, String>,
    pub dataitems: Vec<ReportDataItem>,
    /// Top-level data items in run order.
    pub roots: Vec<usize>,
    /// The request page (empty when the report has none): id = report id, code = the report's code,
    /// no source table.
    pub request_page: PageDef,
    /// Request page triggers (oninit, onopenpage, onqueryclosepage, onclosepage, ...) of the report
    /// and its extensions, in run order.
    pub request_triggers: Vec<(String, ProcRef)>,
    /// Layouts (rendering section entries and RDLCLayout/WordLayout/ExcelLayout properties).
    pub layouts: Vec<String>,
    /// The layouts the report and its extensions define, as the platform lists them in table
    /// "Report Layout List".
    pub layout_list: Vec<ReportLayout>,
    /// The `labels` section of the report and its extensions.
    pub labels: Vec<ReportLabel>,
}

/// A layout of a report: a `rendering { layout(Name) { ... } }` entry, or a legacy
/// RDLCLayout/WordLayout/ExcelLayout property (named after its file).
#[derive(Debug, Clone, Serialize)]
pub struct ReportLayout {
    pub name: String,
    /// Layout type: RDLC, Word, Excel or Custom.
    pub kind: String,
    pub caption: String,
    pub summary: String,
    pub file: String,
    pub mime_type: String,
    /// Code object (report or report extension) that defines it.
    pub code: u32,
}

fn report_layouts(b: &Body, code: u32) -> Vec<ReportLayout> {
    let mut out = vec![];
    for el in elements(b).filter(|e| e.kind == "rendering") {
        for l in elements(&el.body).filter(|l| l.kind == "layout") {
            let Some(name) = arg_name(l, 0) else { continue };
            let file = prop_str(&l.body, "LayoutFile").unwrap_or_default();
            let kind = prop_word(&l.body, "Type").unwrap_or_else(|| layout_kind_of_file(&file).into());
            out.push(ReportLayout {
                caption: prop_str(&l.body, "Caption").unwrap_or_else(|| name.clone()),
                summary: prop_str(&l.body, "Summary").unwrap_or_default(),
                mime_type: prop_str(&l.body, "MimeType").unwrap_or_else(|| layout_mime(&kind).into()),
                name,
                kind,
                file,
                code,
            });
        }
    }
    for (prop, kind) in [("RDLCLayout", "RDLC"), ("WordLayout", "Word"), ("ExcelLayout", "Excel")] {
        if let Some(file) = prop_str(b, prop) {
            let name = file.rsplit(['/', '\\']).next().unwrap_or(&file).to_string();
            if !out.iter().any(|l| l.name.eq_ignore_ascii_case(&name)) {
                out.push(ReportLayout { caption: name.clone(), summary: String::new(), mime_type: layout_mime(kind).into(), name, kind: kind.into(), file, code });
            }
        }
    }
    out
}

fn layout_kind_of_file(file: &str) -> &'static str {
    let f = file.to_ascii_lowercase();
    if f.ends_with(".docx") {
        "Word"
    } else if f.ends_with(".xlsx") {
        "Excel"
    } else if f.ends_with(".rdlc") || f.ends_with(".rdl") {
        "RDLC"
    } else {
        "Custom"
    }
}

fn layout_mime(kind: &str) -> &'static str {
    match kind.to_ascii_lowercase().as_str() {
        "word" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "excel" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        _ => "",
    }
}

impl ReportDef {
    pub fn dataitem(&self, name: &str) -> Option<usize> {
        self.dataitems.iter().position(|d| d.name.eq_ignore_ascii_case(name))
    }

    /// The request page has something to show: controls or data item filter fields.
    pub fn has_request_page(&self) -> bool {
        !self.request_page.layout.is_empty() || self.dataitems.iter().any(|d| !d.request_filter_fields.is_empty())
    }
}

fn record_var(name: &str, table: &str, temporary: bool) -> VarDecl {
    VarDecl { name: name.to_string(), ty: TypeRef::Object { kind: "record".into(), target: table.to_string(), temporary }, attributes: vec![] }
}

impl Catalog {
    fn report_dataitem(&mut self, el: &Element, code: u32, parent: Option<usize>, out: &mut Vec<ReportDataItem>, vars: &mut Vec<VarDecl>) -> usize {
        let b = &el.body;
        let name = arg_name(el, 0).unwrap_or_default();
        let table_name = arg_name(el, 1).unwrap_or_default();
        let temporary = prop_bool(b, "UseTemporary").unwrap_or(false);
        vars.push(record_var(&name, &table_name, temporary));
        let mut triggers = HashMap::new();
        let trig: Vec<Procedure> = Self::nested_triggers(b).cloned().collect();
        for t in trig {
            let r = self.add_nested_trigger(code, &t);
            triggers.insert(t.name.to_ascii_lowercase(), r);
        }
        let columns = elements(b).filter(|c| c.kind == "column").map(|c| report_column(c, code)).collect();
        let item = ReportDataItem {
            name,
            table: self.id_of(ObjectKind::Table, &table_name),
            parent,
            children: vec![],
            view: prop(b, "DataItemTableView").map(|p| parse_table_view(&p.value)).unwrap_or_default(),
            links: prop(b, "DataItemLink").map(|p| parse_conds(&p.value)).unwrap_or_default(),
            link_ref: prop_word(b, "DataItemLinkReference"),
            request_filter_fields: prop_list(b, "RequestFilterFields"),
            temporary,
            max_iteration: prop(b, "MaxIteration").and_then(|p| tokens_text(&p.value).parse().ok()),
            print_only_if_detail: prop_bool(b, "PrintOnlyIfDetail").unwrap_or(false),
            calc_fields: prop_list(b, "CalcFields"),
            triggers,
            ext_triggers: vec![],
            columns,
            code,
        };
        let me = out.len();
        out.push(item);
        let kids: Vec<Element> = elements(b).filter(|c| c.kind == "dataitem").cloned().collect();
        for k in &kids {
            let ci = self.report_dataitem(k, code, Some(me), out, vars);
            out[me].children.push(ci);
        }
        me
    }

    fn request_page_def(&mut self, id: i64, name: &str, caption: &str, code: u32, b: &Body, triggers: &mut Vec<(String, ProcRef)>) -> PageDef {
        let (layout, actions) = self.page_sections(b, code, None);
        let trig: Vec<Procedure> = Self::nested_triggers(b).cloned().collect();
        for t in trig {
            let r = self.add_nested_trigger(code, &t);
            triggers.push((t.name.to_ascii_lowercase(), r));
        }
        let mut pp = HashMap::new();
        for p in props(b) {
            let v = if p.value.len() == 1 && p.value[0].kind == Tk::Str { p.value[0].text.to_string() } else { tokens_text(&p.value) };
            pp.insert(p.name.to_ascii_lowercase(), v);
        }
        PageDef {
            id,
            name: name.to_string(),
            caption: caption.to_string(),
            page_type: "RequestPage".into(),
            source_table: None,
            code,
            ext_code: vec![],
            card_page: None,
            props: pp,
            view: TableView::default(),
            layout,
            actions,
            prop_src: props(b).map(|p| (p.name.to_ascii_lowercase(), prop_value_src(p))).collect(),
        }
    }

    /// A report object (its code object `code` exists already).
    pub(crate) fn add_report(&mut self, code: u32, o: &Object) {
        let Some(id) = o.id else { return };
        let b = &o.body;
        let caption = prop_str(b, "Caption").unwrap_or_else(|| o.name.clone());
        let mut dataitems = vec![];
        let mut roots = vec![];
        let mut vars = vec![];
        let mut request_page = None;
        let mut request_triggers = vec![];
        let mut layouts = vec![];
        let mut labels = vec![];
        let els: Vec<Element> = elements(b).cloned().collect();
        for el in &els {
            match el.kind.as_str() {
                "dataset" => {
                    for d in elements(&el.body).filter(|d| d.kind == "dataitem") {
                        let i = self.report_dataitem(d, code, None, &mut dataitems, &mut vars);
                        roots.push(i);
                    }
                }
                "requestpage" => request_page = Some(self.request_page_def(id, &o.name, &caption, code, &el.body, &mut request_triggers)),
                "rendering" => layouts.extend(elements(&el.body).filter(|l| l.kind == "layout").filter_map(|l| arg_name(l, 0))),
                "labels" => labels.extend(report_labels(el)),
                _ => {}
            }
        }
        for p in ["RDLCLayout", "WordLayout", "ExcelLayout", "DefaultLayout", "DefaultRenderingLayout"] {
            if let Some(v) = prop_str(b, p) {
                layouts.push(v);
            }
        }
        let mut pp = HashMap::new();
        for p in props(b) {
            let v = if p.value.len() == 1 && p.value[0].kind == Tk::Str { p.value[0].text.to_string() } else { tokens_text(&p.value) };
            pp.insert(p.name.to_ascii_lowercase(), v);
        }
        let request_page = match request_page {
            Some(p) => p,
            None => self.request_page_def(id, &o.name, &caption, code, &Body::default(), &mut vec![]),
        };
        self.code[code as usize].globals.extend(vars);
        self.reports.insert(
            id,
            ReportDef {
                id,
                name: o.name.clone(),
                caption,
                code,
                ext_code: vec![],
                processing_only: prop_bool(b, "ProcessingOnly").unwrap_or(false),
                use_request_page: prop_bool(b, "UseRequestPage").unwrap_or(true),
                props: pp,
                dataitems,
                roots,
                request_page,
                request_triggers,
                layouts,
                layout_list: report_layouts(b, code),
                labels,
            },
        );
    }

    /// A report extension: added data items, `modify` triggers, request page additions.
    pub(crate) fn add_report_ext(&mut self, code: u32, o: &Object) {
        let Some(base) = o.extends.first().and_then(|b| self.id_of(ObjectKind::Report, b)) else { return };
        let Some(rd) = self.reports.remove(&base) else { return };
        let mut rd = rd;
        rd.ext_code.push(code);
        rd.layout_list.extend(report_layouts(&o.body, code));
        let mut vars = vec![];
        let els: Vec<Element> = elements(&o.body).cloned().collect();
        for el in &els {
            match el.kind.as_str() {
                "dataset" => {
                    for change in elements(&el.body) {
                        let anchor = arg_name(change, 0).unwrap_or_default();
                        let at = rd.dataitem(&anchor);
                        match change.kind.as_str() {
                            "add" | "modify" => {
                                let Some(ai) = at else { continue };
                                let trig: Vec<Procedure> = Self::nested_triggers(&change.body).cloned().collect();
                                for t in trig {
                                    let r = self.add_nested_trigger(code, &t);
                                    rd.dataitems[ai].ext_triggers.push((t.name.to_ascii_lowercase(), r));
                                }
                                for c in elements(&change.body).filter(|c| c.kind == "column") {
                                    rd.dataitems[ai].columns.push(report_column(c, code));
                                }
                            }
                            "addfirst" | "addlast" | "addafter" | "addbefore" => {
                                let parent = match change.kind.as_str() {
                                    "addfirst" | "addlast" => at,
                                    _ => at.and_then(|a| rd.dataitems[a].parent),
                                };
                                let mut new_items = vec![];
                                let kids: Vec<Element> = elements(&change.body).filter(|d| d.kind == "dataitem").cloned().collect();
                                for k in &kids {
                                    let mut tmp = std::mem::take(&mut rd.dataitems);
                                    let i = self.report_dataitem(k, code, parent, &mut tmp, &mut vars);
                                    rd.dataitems = tmp;
                                    new_items.push(i);
                                }
                                let list = match parent {
                                    Some(p) => &mut rd.dataitems[p].children,
                                    None => &mut rd.roots,
                                };
                                let pos = match (change.kind.as_str(), at.and_then(|a| list.iter().position(|&x| x == a))) {
                                    ("addfirst", _) => 0,
                                    ("addbefore", Some(p)) => p,
                                    ("addafter", Some(p)) => p + 1,
                                    _ => list.len(),
                                };
                                for (k, i) in new_items.into_iter().enumerate() {
                                    list.insert((pos + k).min(list.len()), i);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                "requestpage" => {
                    let mut trigs = vec![];
                    let trig: Vec<Procedure> = Self::nested_triggers(&el.body).cloned().collect();
                    for t in trig {
                        let r = self.add_nested_trigger(code, &t);
                        trigs.push((t.name.to_ascii_lowercase(), r));
                    }
                    rd.request_triggers.extend(trigs);
                    let mut edits: Vec<(bool, Element, Vec<Control>)> = vec![];
                    for sec in elements(&el.body) {
                        let is_layout = match sec.kind.as_str() {
                            "layout" => true,
                            "actions" => false,
                            _ => continue,
                        };
                        for change in elements(&sec.body) {
                            let kids: Vec<Element> = elements(&change.body).cloned().collect();
                            let built = kids.iter().map(|k| self.control(k, code, None)).collect();
                            edits.push((is_layout, change.clone(), built));
                        }
                    }
                    {
                        let pg = &mut rd.request_page;
                        pg.ext_code.push(code);
                        for (is_layout, change, built) in edits {
                            let tree = if is_layout { &mut pg.layout } else { &mut pg.actions };
                            let anchor = arg_name(&change, 0).unwrap_or_default();
                            if matches!(change.kind.as_str(), "addfirst" | "addlast" | "addafter" | "addbefore") {
                                apply_add(tree, &change.kind, &anchor, &mut Some(built));
                            }
                        }
                    }
                }
                "labels" => rd.labels.extend(report_labels(el)),
                _ => {}
            }
        }
        self.code[code as usize].globals.extend(vars);
        self.reports.insert(base, rd);
    }
}
