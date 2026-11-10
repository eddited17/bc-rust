//! Report execution: data processing and triggers, without layout rendering.
//!
//! A run is: OnInitReport, the request page (a TestRequestPage driven by the test's
//! [RequestPageHandler]; skipped with UseRequestPage(false)), OnPreReport, the data item tree,
//! OnPostReport. Each data item applies its DataItemTableView (filter group 2) and DataItemLink
//! (group 4, from the linked data item's current record), runs OnPreDataItem, iterates its record
//! (Find('-') / Next) with OnAfterGetRecord and the child data items per record, then
//! OnPostDataItem. CurrReport.Skip/Break/Quit unwind as internal errors caught by the loop.
//! In tests a [ReportHandler] replaces Report.Run / RunModal entirely.
//!
//! The report dataset ([`Dataset`]) is produced like the service tier's DataItemIterator does it:
//! a row is emitted when a leaf data item gets a record (or a data item without records under a
//! parent that is not PrintOnlyIfDetail), and the row carries the columns of every data item that
//! has a current record. Two XML forms exist:
//! - Report.SaveAs(..., ReportFormat::Xml, ...) / Report.SaveAsXml: the `ReportDataSet` tree
//!   (`DataItems/DataItem/Columns/Column`, values formatted for the report's region, decimal
//!   columns with a `decimalformatter` .NET format string, labels in `Labels/Label`);
//! - TestRequestPage.SaveAsXml(ParametersFile, DataSetFile): the flat RDLC dataset, a .NET
//!   DataSet (`DataSet` with an inline XSD and one `Result` element per row, XmlConvert values,
//!   a `<Column>Format` column per decimal column) and the labels as `ArrayOfReportParameter`.
//!
//! Rendered output (SaveAsPdf/Word/Excel/Html, SaveAs(format)) runs the report fully and writes a
//! structurally valid file of the format with the dataset as text (`render_report`): there is no
//! layout engine yet (an RDLC report saved as Excel is not laid out). As in BC
//! (ReportResultSetProcessorFactory) the default layout's type limits the formats, and an empty
//! dataset is an error for PDF, Word and HTML.

use crate::interp::*;
use crate::record::*;
use crate::value::*;
use al_model::{Catalog, FieldDef, FieldType, ProcRef, ReportColumn, ReportDataItem, ReportDef};
use al_syntax::{BinOp, Expr, ObjectKind, TypeRef, UnOp};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// Per-instance report settings (Instance::report).
#[derive(Default)]
pub struct ReportState {
    /// UseRequestPage(x) / `UseRequestPage := x` on the variable or CurrReport (Void = not set).
    /// A slot so the property assignment form can write it directly.
    pub use_request_page: Option<Slot>,
    /// While running: whether this run shows the request page (CurrReport.UseRequestPage()).
    pub effective_request_page: Option<bool>,
    /// Report extension instances (their globals include the base report's).
    pub ext: Vec<Rc<Instance>>,
    /// For report extension instances: the base report instance.
    pub base: Option<Weak<Instance>>,
    pub language: i32,
    pub format_region: String,
    /// OnInitReport ran for this instance (Session::rep_init_once).
    pub init_report_ran: bool,
    /// Created without the request page's saved values: BC's static Report.Execute/SaveAs/Print/
    /// RunRequestPage with empty Parameters (NCLMetaReport.CreateObjectInstance(session,
    /// skipRestore: string.IsNullOrEmpty(parameters))) and the SaveAsXxx(FileName) forms.
    pub skip_saved_values: bool,
    /// Report.SaveAs(Xml) / SaveAsXml: where the next run writes its dataset.
    pub target: Option<Target>,
    /// TestRequestPage.SaveAsXml(ParametersFileName, DataSetFileName) in the running request page.
    pub test_xml: Option<(String, String)>,
    /// The dataset of the running report (only while a run produces XML).
    pub ds: Option<Box<Dataset>>,
    /// While running: the run previews (BC NavReport.preview: ReportIntent Preview, i.e. the
    /// request page closed with Preview), CurrReport.Preview.
    pub preview: bool,
}

pub(crate) const SKIP: &str = "ReportSkip";
pub(crate) const BREAK: &str = "ReportBreak";
pub(crate) const QUIT: &str = "ReportQuit";

fn is_flow(e: &Flow, code: &str) -> bool {
    matches!(e, Flow::Err(x) if x.code == code)
}

/// What the caller wants from the run besides the processing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Output {
    /// Run / RunModal / Execute: print to the default destination (nothing is rendered).
    Default,
    Preview,
    Print,
    Pdf,
    Word,
    Excel,
    Html,
    Xml,
    /// Processing only, no output at all (e.g. Execute of a ProcessingOnly report).
    None,
}

impl Output {
    fn from_format(ord: i64) -> Output {
        // ReportFormat: Pdf, Word, Excel, Xml, Html
        match ord {
            0 => Output::Pdf,
            1 => Output::Word,
            2 => Output::Excel,
            3 => Output::Xml,
            _ => Output::Html,
        }
    }
    fn rendered(self) -> Option<&'static str> {
        Some(match self {
            Output::Pdf => "PDF",
            Output::Word => "Word",
            Output::Excel => "Excel",
            Output::Html => "HTML",
            Output::Xml => "dataset XML",
            _ => return None,
        })
    }
}

thread_local! {
    static SKIP_RENDERING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// While on (for this thread), a report asked for a rendered layout (SaveAsPdf, SaveAs with a
/// format, ...) runs its data processing and then skips the rendering instead of failing with
/// "Not implemented in bc-rust: report rendering". Only for callers that run a report for its
/// side effects and never read the output: Microsoft's legacy demo tool posts the VAT settlement
/// with Report 20.SaveAsPdf (bc-server demo::w1). Tests keep the error.
pub fn set_skip_rendering(on: bool) {
    SKIP_RENDERING.with(|c| c.set(on));
}

pub struct RunReq {
    /// RequestWindow argument (Report.Run(Id, false) suppresses the request page).
    pub request_page: bool,
    /// Report.Run(..., Rec): the caller's view for the data item of the record's table.
    pub rec: Option<RecHandle>,
    /// Execute / SaveAs / Print: request page parameters XML.
    pub params: Option<String>,
    pub output: Output,
    /// Report.Run / RunModal: a [ReportHandler] in tests takes the run.
    pub handlers: bool,
}

impl RunReq {
    fn run(request_page: bool, rec: Option<RecHandle>) -> RunReq {
        RunReq { request_page, rec, params: None, output: Output::Default, handlers: true }
    }
    fn background(params: Option<String>, rec: Option<RecHandle>, output: Output) -> RunReq {
        RunReq { request_page: false, rec, params, output, handlers: false }
    }
}

fn rec_arg(v: Option<&Val>) -> Option<RecHandle> {
    match v {
        Some(Val::Record(r)) => Some(r.clone()),
        _ => None,
    }
}

/// The RunMode option of Reporting Triggers.SubstituteReport.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunMode {
    Normal = 0,
    ParametersOnly = 1,
    Execute = 2,
    Print = 3,
    SaveAs = 4,
    RunModal = 5,
}

/// Reporting Triggers (platform/system/System.Environment.al).
const REPORTING_TRIGGERS: i64 = 2000000005;

fn stream_target(v: Option<&Val>) -> Option<Target> {
    match v {
        Some(Val::OutStream(t)) => Some(Target::Stream(t.clone())),
        _ => None,
    }
}

impl<'r> Session<'r> {
    fn rep_eval_args(&mut self, f: &mut Frame, args: &[Expr]) -> R<Vec<Val>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            out.push(self.eval(f, a)?);
        }
        Ok(out)
    }

    fn rep_miss(&mut self, what: String) -> R<Val> {
        *self.missing.entry(what.clone()).or_default() += 1;
        err(format!("Not implemented in bc-rust: {what}"))
    }

    pub fn report_def(&self, inst: &Rc<Instance>) -> Option<&'static ReportDef> {
        let co = self.code(inst.code);
        if co.kind != ObjectKind::Report {
            return None;
        }
        self.cat.reports.get(&co.id)
    }

    /// The report instance a CurrReport call in frame `f` refers to (report or report extension code).
    fn rep_current(&self, f: &Frame) -> Option<Rc<Instance>> {
        let inst = f.inst.clone()?;
        let base = inst.report.borrow().as_ref().and_then(|s| s.base.as_ref().and_then(|b| b.upgrade()));
        match base {
            Some(b) => Some(b),
            None if self.code(inst.code).kind == ObjectKind::Report => Some(inst),
            None => None,
        }
    }

    fn rep_state<T>(&self, inst: &Rc<Instance>, g: impl FnOnce(&mut ReportState) -> T) -> T {
        let mut st = inst.report.borrow_mut();
        g(st.get_or_insert_with(Default::default))
    }

    fn rep_urp_slot(&self, inst: &Rc<Instance>) -> Slot {
        self.rep_state(inst, |s| s.use_request_page.get_or_insert_with(|| slot(Val::Void)).clone())
    }

    /// The UseRequestPage setting of a report instance (None = not set).
    fn rep_urp(&self, inst: &Rc<Instance>) -> Option<bool> {
        match &*self.rep_urp_slot(inst).borrow() {
            Val::Void => None,
            v => Some(v.as_bool()),
        }
    }

    /// `Report.UseRequestPage := x` / `CurrReport.UseRequestPage := x`: the setting's slot.
    pub fn report_use_request_page_slot(&self, f: &Frame, target: &Val) -> Option<Slot> {
        let inst = match target {
            Val::Object(i) if self.code(i.code).kind == ObjectKind::Report => i.clone(),
            Val::Opaque("CurrPage") => self.rep_current(f)?,
            _ => return None,
        };
        Some(self.rep_urp_slot(&inst))
    }

    /// Globals of the report and its extension instances (bound to the base report's globals).
    pub fn rep_exts(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef) -> Vec<Rc<Instance>> {
        self.instance_globals(inst);
        let have = self.rep_state(inst, |s| s.ext.clone());
        if have.len() == rd.ext_code.len() {
            return have;
        }
        let mut exts = vec![];
        for &c in &rd.ext_code {
            let e = Instance::new(c);
            self.bind_ext_globals(&e, inst);
            *e.report.borrow_mut() = Some(Box::new(ReportState { base: Some(Rc::downgrade(inst)), ..Default::default() }));
            exts.push(e);
        }
        self.rep_state(inst, |s| s.ext = exts.clone());
        exts
    }

    fn rep_inst_for(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef, code: u32) -> Rc<Instance> {
        if code == inst.code {
            return inst.clone();
        }
        self.rep_exts(inst, rd).into_iter().find(|e| e.code == code).unwrap_or_else(|| inst.clone())
    }

    /// The record variable of data item `idx`.
    pub fn rep_item_rec(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef, idx: usize) -> Option<RecHandle> {
        let di = &rd.dataitems[idx];
        let owner = self.rep_inst_for(inst, rd, di.code);
        let g = self.instance_globals(&owner);
        let v = g.vars.iter().find(|v| v.name.eq_ignore_ascii_case(&di.name))?;
        let val = v.slot.borrow().clone();
        match val {
            Val::Record(r) => Some(r),
            _ => None,
        }
    }

    /// SetTableView(Rec) / Report.Run(..., Rec): the record's view goes to the first data item of its table.
    fn rep_set_table_view(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef, src: &RecHandle) {
        let tid = src.borrow().tdef.id;
        let Some(idx) = rd.dataitems.iter().position(|d| d.table == Some(tid)) else { return };
        let Some(r) = self.rep_item_rec(inst, rd, idx) else { return };
        let s = src.borrow();
        let mut c = r.borrow_mut();
        c.filters = s.filters.clone();
        c.key = s.key;
        c.ascending = s.ascending;
        c.marks = s.marks.clone();
        c.mark_only = s.mark_only;
        if s.temp.is_some() && c.temp.is_some() {
            c.temp = s.temp.clone();
        }
    }

    fn rep_apply_view(&mut self, r: &RecHandle, view: &al_model::TableView) -> R<()> {
        if !view.conds.is_empty() {
            self.apply_conds(r, &view.conds, None, 2)?;
        }
        if !view.sorting.is_empty() {
            let fields: Vec<usize> = view.sorting.iter().filter_map(|n| r.borrow().tdef.field(n)).collect();
            let mut rb = r.borrow_mut();
            if rb.key == 0 {
                if let Some(k) = rb.tdef.keys.iter().position(|k| k.fields.starts_with(&fields)) {
                    let kf = rb.tdef.keys[k].fields.clone();
                    rb.key = rb.schema.keys.iter().position(|sk| *sk == kf).unwrap_or(0);
                }
            }
            if view.descending {
                rb.ascending = false;
            }
        }
        Ok(())
    }

    /// OnInitReport runs once per report instance. BC runs it when it constructs the report (the
    /// generated report class's InitializeComponent -> EndInitialization -> OnInitReport), so it
    /// comes before the caller's InitializeRequest(...) and does not run again on a second
    /// RunModal of the same variable; Clear gives a new instance. bc-rust creates instances
    /// lazily, so the trigger runs at the instance's first use: a method call on the report
    /// variable (Session::object_method, `quit_ok`: a Quit there only ends the trigger) or its run.
    pub fn rep_init_once(&mut self, inst: &Rc<Instance>, quit_ok: bool) -> R<()> {
        // its own flag: Instance::inited belongs to the request page's OnInit (testpage.rs)
        if self.rep_state(inst, |s| std::mem::replace(&mut s.init_report_ran, true)) {
            return Ok(());
        }
        let Some(rd) = self.report_def(inst) else { return Ok(()) };
        self.rep_exts(inst, rd);
        let r = match self.rep_report_trigger(inst, rd, "oninitreport") {
            Err(e) if quit_ok && is_flow(&e, QUIT) => Ok(()),
            r => r,
        };
        // then the request page's saved values (crate::testpage report_restore_saved_values), unless
        // the static call site creates the report without them (ReportState::skip_saved_values)
        if r.is_ok() && self.in_test() && !self.rep_state(inst, |s| s.skip_saved_values) {
            let id = self.code(inst.code).id;
            self.report_restore_saved_values(inst, id);
        }
        r
    }

    /// Run a report-level trigger (OnInitReport, OnPreReport, OnPostReport) of the report and its
    /// extensions. Skip/Break leave the trigger; Quit propagates.
    fn rep_report_trigger(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef, name: &str) -> R<()> {
        let mut codes = vec![inst.code];
        codes.extend(rd.ext_code.iter().copied());
        for c in codes {
            let Some(p) = self.code(c).trigger(name) else { continue };
            let i = self.rep_inst_for(inst, rd, c);
            match self.invoke(ProcRef { code: c, proc: p }, vec![], Some(i), None, None) {
                Ok(_) => {}
                Err(e) if is_flow(&e, SKIP) || is_flow(&e, BREAK) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Data item trigger `on<what>` with the report extensions' onbefore<what> / onafter<what>.
    fn rep_item_trigger(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef, idx: usize, what: &str, r: &RecHandle) -> R<()> {
        let di = &rd.dataitems[idx];
        let before = format!("onbefore{what}");
        let base = format!("on{what}");
        let after = format!("onafter{what}");
        let mut procs: Vec<ProcRef> = di.ext_triggers.iter().filter(|(n, _)| *n == before).map(|(_, p)| *p).collect();
        procs.extend(di.triggers.get(&base).copied());
        procs.extend(di.ext_triggers.iter().filter(|(n, _)| *n == after).map(|(_, p)| *p));
        for pr in procs {
            let i = self.rep_inst_for(inst, rd, pr.code);
            self.invoke(pr, vec![], Some(i), Some(r.clone()), None)?;
        }
        Ok(())
    }

    fn rep_item(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef, idx: usize) -> R<()> {
        let di = &rd.dataitems[idx];
        let Some(r) = self.rep_item_rec(inst, rd, idx) else { return Ok(()) };
        self.rep_apply_view(&r, &di.view)?;
        if !di.links.is_empty() {
            let src = di.link_ref.as_deref().and_then(|n| rd.dataitem(n)).or(di.parent);
            if let Some(src) = src.and_then(|s| self.rep_item_rec(inst, rd, s)) {
                // a link to a FlowFilter field takes the parent's filter on it (none: no filter),
                // BC RecordImplementation.SetDataItemLink; other links filter on the parent's value
                let flow_src = |c: &al_model::Cond| {
                    let s = src.borrow();
                    let sf = s.tdef.field(&c.value)?;
                    (c.kind == al_model::CondKind::Field && s.tdef.fields[sf].class == al_model::FieldClass::FlowFilter).then_some(sf)
                };
                let mut other = vec![];
                for c in &di.links {
                    let (Some(sf), Some(fi)) = (flow_src(c), r.borrow().tdef.field(&c.field)) else {
                        other.push(c.clone());
                        continue;
                    };
                    let copied = {
                        let s = src.borrow();
                        s.filters.iter().rev().find(|f| f.field == sf && f.group == s.filter_group).map(|f| (f.node.clone(), f.text.clone()))
                    };
                    let mut rr = r.borrow_mut();
                    let g = std::mem::replace(&mut rr.filter_group, 4);
                    match copied {
                        Some((node, text)) => rr.set_filter(fi, Some(node), text),
                        None => rr.set_filter(fi, None, String::new()),
                    }
                    rr.filter_group = g;
                }
                self.apply_conds(&r, &other, Some(&src), 4)?;
            }
        }
        // CurrReport.Break() in OnPreDataItem: no records, but OnPostDataItem still runs
        let broke = match self.rep_item_trigger(inst, rd, idx, "predataitem", &r) {
            Ok(()) => false,
            Err(e) if is_flow(&e, BREAK) => true,
            Err(e) if is_flow(&e, SKIP) => false,
            Err(e) => return Err(e),
        };
        let ds = self.rep_ds_on(inst);
        // BC DataItem.FindCalcFields: the CalcFields property plus the FlowFields its columns show
        // (a column whose source is a field of the data item's record, AutoCalcField true)
        let mut calc: Vec<usize> = di.calc_fields.iter().filter_map(|n| r.borrow().tdef.field(n)).collect();
        {
            let rb = r.borrow();
            for col in di.columns.iter().filter(|c| c.auto_calc) {
                let name = match &col.expr {
                    Some(Expr::Ident(n)) => Some(n.as_ref()),
                    Some(Expr::Member(b, n)) if matches!(&**b, Expr::Ident(d) if d.eq_ignore_ascii_case(&di.name) || d.eq_ignore_ascii_case("rec")) => Some(n.as_ref()),
                    _ => None,
                };
                if let Some(fi) = name.and_then(|n| rb.tdef.field(n)) {
                    if rb.tdef.fields[fi].class == al_model::FieldClass::FlowField && !calc.contains(&fi) {
                        calc.push(fi);
                    }
                }
            }
        }
        let mut found = !broke && self.find_first(&r, false)?;
        if found && ds {
            self.rep_ds_path(inst, Some(idx));
        } else if ds && !broke && !(di.print_only_if_detail && di.children.is_empty()) {
            // no records: the parent's row is still written (when it shows rows without detail)
            let parent_shows = di.parent.is_some_and(|p| !rd.dataitems[p].print_only_if_detail);
            if parent_shows && !di.columns.is_empty() && self.rep_ds_depth(inst) > 0 {
                self.rep_ds_row(inst, rd)?;
            }
        }
        let had = found;
        let mut n = 0i64;
        while found {
            for &fi in &calc {
                self.calc_field(&r, fi)?;
            }
            match self.rep_item_trigger(inst, rd, idx, "aftergetrecord", &r) {
                Ok(()) => {
                    // after a child, this record's row is added once if none was added for it yet
                    // (BC DataItemIterator.LoopChildDataItemsAsync; every dataset: flat, tree, rendered)
                    let rows0 = if ds { self.rep_ds(inst, |d| d.nrows) } else { None };
                    for &c in &di.children {
                        self.rep_item(inst, rd, c)?;
                        if let Some(n0) = rows0 {
                            if !di.print_only_if_detail && !di.columns.is_empty() && self.rep_ds(inst, |d| d.nrows) == Some(n0) {
                                self.rep_ds_row(inst, rd)?;
                            }
                        }
                    }
                    if ds {
                        if di.children.is_empty() && !di.print_only_if_detail && !di.columns.is_empty() {
                            self.rep_ds_row(inst, rd)?;
                        }
                        self.rep_ds_end_record(inst, rd, idx);
                    }
                }
                Err(e) if is_flow(&e, SKIP) => {}
                Err(e) if is_flow(&e, BREAK) => break,
                Err(e) => return Err(e),
            }
            n += 1;
            if di.max_iteration.is_some_and(|m| n >= m) {
                break;
            }
            found = self.next(&r, 1)? != 0;
        }
        if had && ds {
            self.rep_ds_path(inst, None);
        }
        match self.rep_item_trigger(inst, rd, idx, "postdataitem", &r) {
            Err(e) if !is_flow(&e, SKIP) && !is_flow(&e, BREAK) => Err(e),
            _ => Ok(()),
        }
    }

    /// Reporting Triggers.SubstituteReport: the report to run instead of report `id`. BC raises it
    /// for the static Report.Run / RunModal / Execute / Print / SaveAs* / RunRequestPage calls,
    /// GetSubstituteReportId and page actions with RunObject = Report, before it creates the
    /// report instance; report variables (`Rep.Run()`) are never substituted. NewReportId starts
    /// at -1, and only a different id counts.
    pub fn report_substitute(&mut self, id: i64, mode: RunMode, params: &str, rec: Option<&RecHandle>) -> R<i64> {
        if !self.cat.subscribers.contains_key(&(ObjectKind::Codeunit, REPORTING_TRIGGERS, "substitutereport".to_string(), String::new())) {
            return Ok(id);
        }
        let record_ref = match rec {
            Some(r) => self.as_recordref(Val::Record(r.clone())),
            None => self.new_recordref(),
        };
        let new_id = slot(Val::Int(-1));
        let args = vec![
            ("ReportId", Arg::Val(Val::Int(id as i32))),
            ("RunMode", Arg::Val(Val::Opt { ord: mode as i32, enum_id: None })),
            ("RequestPageXml", Arg::Val(Val::Text(params.to_string()))),
            ("RecordRef", Arg::Val(record_ref)),
            ("NewReportId", Arg::Ref(LVal::Slot(new_id.clone(), None))),
        ];
        self.raise_event(ObjectKind::Codeunit, REPORTING_TRIGGERS, "SubstituteReport", "", args, None, None)?;
        let n = new_id.borrow().as_i64();
        Ok(if n != -1 && n != id { n } else { id })
    }

    /// Run a report on instance `inst`.
    pub fn report_run(&mut self, inst: &Rc<Instance>, mut rq: RunReq) -> R<()> {
        let Some(rd) = self.report_def(inst) else {
            let co = self.code(inst.code);
            return err(format!("Report {} does not exist", co.name));
        };
        self.rep_exts(inst, rd);
        if rq.handlers && self.ui_report_handler(rd.id, inst)? {
            return Ok(());
        }
        if let Some(r) = rq.rec.clone() {
            self.rep_set_table_view(inst, rd, &r);
        }
        let processed = self.rep_process(inst, rd, &mut rq);
        let ds = self.rep_state(inst, |s| {
            s.target = None;
            s.test_xml = None;
            s.preview = false;
            s.ds.take()
        });
        match processed {
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(e) if is_flow(&e, QUIT) => return Ok(()),
            Err(e) => return Err(e),
        }
        if let Some(ds) = ds {
            if ds.render.is_some() && SKIP_RENDERING.with(|c| c.get()) {
                *self.missing.entry("report rendering skipped".into()).or_default() += 1;
                crate::assume::hit("Report.OutputDiscarded");
                return Ok(());
            }
            return self.rep_ds_finish(rd, *ds);
        }
        if let Some(what) = rq.output.rendered() {
            if rd.processing_only {
                return Ok(());
            }
            if SKIP_RENDERING.with(|c| c.get()) {
                *self.missing.entry(format!("report rendering ({what}) skipped")).or_default() += 1;
                crate::assume::hit("Report.OutputDiscarded");
                return Ok(());
            }
            return self.rep_miss(format!("report rendering ({what}) of report {} \"{}\"", rd.id, rd.name)).map(|_| ());
        }
        if !rd.processing_only && matches!(rq.output, Output::Default | Output::Preview | Output::Print) {
            // printed output is discarded: only the data processing ran
            *self.missing.entry("report layout rendering (output discarded)".into()).or_default() += 1;
            crate::assume::hit("Report.OutputDiscarded");
        }
        Ok(())
    }

    /// Triggers and data items; false when the request page was cancelled.
    fn rep_process(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef, rq: &mut RunReq) -> R<bool> {
        self.rep_init_once(inst, false)?;
        if let Some(p) = rq.params.clone() {
            self.rep_apply_params(inst, rd, &p)?;
            // BC ApplyXmlViews: the record's filters after the parameters' data item views
            if let Some(r) = rq.rec.clone() {
                self.rep_set_table_view(inst, rd, &r);
            }
        }
        let show = rq.request_page && self.rep_urp(inst).unwrap_or(rd.use_request_page) && rd.has_request_page();
        self.rep_state(inst, |s| s.effective_request_page = Some(show));
        if show {
            let action = self.ui_request_page(rd.id, inst)?;
            match action {
                "cancel" | "lookupcancel" | "no" => return Ok(false),
                "preview" => rq.output = Output::Preview,
                "print" => rq.output = Output::Print,
                "saveasxml" => rq.output = Output::Xml,
                "saveaspdf" => rq.output = Output::Pdf,
                "saveasword" => rq.output = Output::Word,
                "saveasexcel" => rq.output = Output::Excel,
                "schedule" => return Ok(false),
                _ => {}
            }
        }
        // BC RunReportInternalAsync: preview = (ReportIntent == Preview); then BeginTransaction and
        // CurrentTransactionType := the report's TransactionType property, default UpdateNoLocks
        // (crate::sysfuncs TxnState). A type other than the current one and UpdateNoLocks makes BC
        // commit first (RunReportCoreAsync BeginTransactionWorld), so it always takes effect.
        let preview = rq.output == Output::Preview;
        self.rep_state(inst, |s| s.preview = preview);
        let ty = rd.props.get("transactiontype").map(|t| crate::interp::system_option_ord("transactiontype", t.trim().trim_matches('"'))).filter(|&t| t < 5).unwrap_or(0);
        self.set_transaction_type(ty, ty != 0)?;
        if rq.output == Output::Xml {
            self.rep_ds_start(inst, rd)?;
        } else if matches!(rq.output, Output::Pdf | Output::Word | Output::Excel | Output::Html) && !rd.processing_only {
            // a rendered format: collect the dataset to write the output from it
            self.rep_ds_start(inst, rd)?;
            let o = rq.output;
            self.rep_ds(inst, |d| d.render = Some(o));
        }
        self.rep_report_trigger(inst, rd, "onprereport")?;
        for &i in &rd.roots {
            self.rep_item(inst, rd, i)?;
        }
        self.rep_report_trigger(inst, rd, "onpostreport")?;
        Ok(true)
    }

    /// RunRequestPage: show the request page only and return its parameters ('' when cancelled).
    pub fn report_run_request_page(&mut self, inst: &Rc<Instance>, params: Option<String>) -> R<String> {
        let Some(rd) = self.report_def(inst) else { return Ok(String::new()) };
        self.rep_exts(inst, rd);
        match self.rep_init_once(inst, false) {
            Err(e) if is_flow(&e, QUIT) => return Ok(String::new()),
            r => r?,
        }
        if let Some(p) = params.filter(|p| !p.trim().is_empty()) {
            self.rep_apply_params(inst, rd, &p)?;
        }
        let action = self.ui_request_page(rd.id, inst)?;
        if matches!(action, "cancel" | "lookupcancel" | "no") {
            return Ok(String::new());
        }
        Ok(self.rep_params_xml(inst, rd))
    }

    // ------------------------------------------------------------------ request page parameters

    /// Request page option fields: (variable name, control name, owning code).
    fn rep_option_fields(rd: &'static ReportDef) -> Vec<(&'static str, &'static str, u32)> {
        fn walk(cs: &'static [al_model::Control], code: u32, out: &mut Vec<(&'static str, &'static str, u32)>) {
            for c in cs {
                if c.kind == "field" {
                    if let Some(Expr::Ident(v)) = &c.source_expr {
                        let owner = c.triggers.values().next().map(|p| p.code).unwrap_or(code);
                        out.push((v, c.name.as_str(), owner));
                    }
                }
                walk(&c.children, code, out);
            }
        }
        let mut out = vec![];
        walk(&rd.request_page.layout, rd.code, &mut out);
        out
    }

    fn rep_global(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef, code: u32, name: &str) -> Option<Var> {
        let owner = self.rep_inst_for(inst, rd, code);
        let g = self.instance_globals(&owner);
        g.vars.iter().find(|v| v.name.eq_ignore_ascii_case(name)).cloned()
    }

    /// The request page values as BC's ReportParameters XML.
    pub fn rep_params_xml(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef) -> String {
        let mut x = format!("<?xml version=\"1.0\" standalone=\"yes\"?><ReportParameters name=\"{}\" id=\"{}\"><Options>", xml_esc(&rd.name), rd.id);
        for (var, _, code) in Self::rep_option_fields(rd) {
            if let Some(v) = self.rep_global(inst, rd, code, var) {
                let val = v.slot.borrow().clone();
                let text = match &val {
                    Val::Opt { ord, enum_id: None } => ord.to_string(),
                    Val::Opt { .. } => crate::builtins::format_val(&val, 9),
                    Val::DateFormula(s) => s.clone(),
                    v => crate::builtins::format_val(v, 9),
                };
                x.push_str(&format!("<Field name=\"{}\">{}</Field>", xml_esc(var), xml_esc(&text)));
            }
        }
        x.push_str("</Options><DataItems>");
        for i in 0..rd.dataitems.len() {
            let Some(r) = self.rep_item_rec(inst, rd, i) else { continue };
            let view = view_text(&r.borrow());
            x.push_str(&format!("<DataItem name=\"{}\">{}</DataItem>", xml_esc(&rd.dataitems[i].name), xml_esc(&view)));
        }
        x.push_str("</DataItems></ReportParameters>");
        x
    }

    /// Execute / SaveAs parameters: option values and data item views.
    fn rep_apply_params(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef, xml: &str) -> R<()> {
        let fields = Self::rep_option_fields(rd);
        for (name, text) in xml_elems(xml, "Field") {
            let Some(&(var, _, code)) = fields.iter().find(|(v, c, _)| v.eq_ignore_ascii_case(&name) || c.eq_ignore_ascii_case(&name)) else { continue };
            let Some(v) = self.rep_global(inst, rd, code, var) else { continue };
            let cur = v.slot.borrow().clone();
            if let Some(nv) = self.text_to_var(&text, &cur, v.ty) {
                *v.slot.borrow_mut() = nv;
            }
        }
        for (name, view) in xml_elems(xml, "DataItem") {
            let Some(i) = rd.dataitem(&name) else { continue };
            let Some(r) = self.rep_item_rec(inst, rd, i) else { continue };
            let globals = self.instance_globals(inst);
            let mut f = Frame { code: inst.code, locals: vec![], globals, rec: None, xrec: None, with: vec![], inst: Some(inst.clone()), ret: None, ret_ty: None, curr_field: 0 };
            self.record_method(&mut f, &r, "SetView", &[Expr::Str(view.into())])?;
        }
        Ok(())
    }

    /// Convert request page / parameter text into a value of the variable's type.
    pub fn text_to_var(&mut self, text: &str, cur: &Val, ty: Option<&'static TypeRef>) -> Option<Val> {
        let t = text.trim();
        Some(match (ty, cur) {
            (Some(TypeRef::Option(members)), _) => {
                let i = members.iter().position(|m| m.trim().eq_ignore_ascii_case(t)).map(|i| i as i32).or_else(|| t.parse().ok())?;
                Val::Opt { ord: i, enum_id: None }
            }
            (Some(TypeRef::Object { kind, target, .. }), _) if kind == "enum" => {
                let e = self.cat.enum_by_name(target)?;
                let v = e.values.iter().find(|v| v.name.eq_ignore_ascii_case(t) || v.caption.eq_ignore_ascii_case(t)).map(|v| v.ordinal).or_else(|| t.parse().ok())?;
                Val::Opt { ord: v as i32, enum_id: Some(e.id) }
            }
            (_, Val::Int(_)) => Val::Int(parse_num(t).and_then(|d| rust_decimal::prelude::ToPrimitive::to_i32(&d))?),
            (_, Val::BigInt(_)) => Val::BigInt(parse_num(t).and_then(|d| rust_decimal::prelude::ToPrimitive::to_i64(&d))?),
            (_, Val::Dec(_)) => Val::Dec(parse_num(t)?),
            (_, Val::Bool(_)) => Val::Bool(match t.to_ascii_lowercase().as_str() {
                "yes" | "true" | "1" => true,
                "no" | "false" | "0" | "" => false,
                _ => return None,
            }),
            (_, Val::Date(_)) => Val::Date(parse_date_text(t, self.work_date)?),
            (_, Val::Time(_)) => Val::Time(parse_time_text(t)?),
            (_, Val::DateTime(_)) => Val::DateTime(parse_datetime_text(t)?),
            // saved request page values hold the stored text (rep_saved_values above)
            (_, Val::DateFormula(_)) => Val::DateFormula(crate::datefml::parse_invariant(t).ok()?.stored()),
            (_, Val::Guid(_)) => Val::Guid(parse_guid(t)?),
            (_, Val::Opt { enum_id, .. }) => Val::Opt { ord: t.parse().ok()?, enum_id: *enum_id },
            (Some(TypeRef::Simple { name, .. }), _) if name.eq_ignore_ascii_case("code") => Val::Text(t.to_uppercase()),
            _ => Val::Text(text.to_string()),
        })
    }

    // ------------------------------------------------------------------ AL entry points

    /// Report.X(...) static methods.
    pub fn report_static(&mut self, f: &mut Frame, m: &str, args: &[Expr]) -> R<Val> {
        // the record argument of Run/RunModal/... is passed by var
        let a = self.rep_eval_args(f, args)?;
        let id = match a.first() {
            Some(Val::Text(t)) => self.cat.id_of(ObjectKind::Report, t).unwrap_or(0),
            Some(v) => v.as_i64(),
            None => 0,
        };
        // SubstituteReport: run mode, request page XML and record as BC passes them per method
        let sub = match m {
            "run" => Some((RunMode::Normal, None, rec_arg(a.get(3)))),
            "runmodal" => Some((RunMode::RunModal, None, rec_arg(a.get(3)))),
            "execute" => Some((RunMode::Execute, a.get(1), rec_arg(a.get(2)))),
            "print" => Some((RunMode::Print, a.get(1), rec_arg(a.get(3)))),
            "saveas" => Some((RunMode::SaveAs, a.get(1), rec_arg(a.get(4)))),
            "saveaspdf" | "saveasword" | "saveasexcel" | "saveashtml" | "saveasxml" => Some((RunMode::SaveAs, None, rec_arg(a.get(2)))),
            "runrequestpage" => Some((RunMode::ParametersOnly, None, None)),
            "getsubstitutereportid" => Some((RunMode::Normal, None, None)),
            _ => None,
        };
        let id = match sub {
            Some((mode, params, rec)) => self.report_substitute(id, mode, &params.map(|v| v.to_text()).unwrap_or_default(), rec.as_ref())?,
            None => id,
        };
        let ci = self.cat.code_by_key.get(&(ObjectKind::Report, id)).copied();
        // saved request page values are restored when BC creates the report object, except for
        // these static calls without parameters (stream O, answer (c)); Run/RunModal
        // restore with or without the request page
        let skip_saved = match m {
            "execute" | "saveas" | "print" | "runrequestpage" => a.get(1).is_none_or(|p| p.to_text().trim().is_empty()),
            "saveaspdf" | "saveasword" | "saveasexcel" | "saveashtml" | "saveasxml" => true,
            _ => false,
        };
        let inst = || -> R<Rc<Instance>> {
            match ci {
                Some(ci) => {
                    let i = Instance::new(ci);
                    if skip_saved {
                        i.report.borrow_mut().get_or_insert_with(Default::default).skip_saved_values = true;
                    }
                    Ok(i)
                }
                // own wording [src:X19]
                None => err(format!("There is no report with ID {id}.")),
            }
        };
        match m {
            "run" | "runmodal" => {
                let req = a.get(1).map(|v| v.as_bool()).unwrap_or(true);
                let rec = rec_arg(a.get(3));
                self.report_run(&inst()?, RunReq::run(req, rec))?;
                Ok(Val::Void)
            }
            "execute" => {
                let params = a.get(1).map(|v| v.to_text());
                let rec = rec_arg(a.get(2));
                self.report_run(&inst()?, RunReq::background(params, rec, Output::None))?;
                Ok(Val::Void)
            }
            "saveas" => {
                // Report.SaveAs(Number, RequestPageParameters, ReportFormat, OutStream [, RecordRef])
                let params = a.get(1).map(|v| v.to_text());
                let format = a.get(2).map(|v| v.as_i64()).unwrap_or(0);
                let rec = rec_arg(a.get(4));
                self.rep_run_xml(&inst()?, RunReq::background(params, rec, Output::from_format(format)), stream_target(a.get(3)))?;
                Ok(Val::Bool(true))
            }
            "saveaspdf" | "saveasword" | "saveasexcel" | "saveashtml" | "saveasxml" => {
                // Report.SaveAsXxx(Number, FileName [, Record])
                let out = match m {
                    "saveaspdf" => Output::Pdf,
                    "saveasword" => Output::Word,
                    "saveasexcel" => Output::Excel,
                    "saveashtml" => Output::Html,
                    _ => Output::Xml,
                };
                let rec = rec_arg(a.get(2));
                let target = a.get(1).map(|v| Target::File(v.to_text()));
                self.rep_run_xml(&inst()?, RunReq::background(None, rec, out), target)?;
                Ok(Val::Bool(true))
            }
            "print" => {
                let params = a.get(1).map(|v| v.to_text());
                let rec = rec_arg(a.get(3));
                self.report_run(&inst()?, RunReq::background(params, rec, Output::Print))?;
                Ok(Val::Void)
            }
            "runrequestpage" => {
                let params = a.get(1).map(|v| v.to_text());
                Ok(Val::Text(self.report_run_request_page(&inst()?, params)?))
            }
            // the id after SubstituteReport (above)
            "getsubstitutereportid" => Ok(Val::Int(id as i32)),
            "defaultlayout" => {
                crate::assume::hit("Report.DefaultLayout");
                Ok(Val::Opt { ord: system_option_ord("defaultlayout", "rdlc"), enum_id: None })
            }
            "rdlclayout" | "wordlayout" | "excellayout" => {
                crate::assume::hit("Report.HasLayout");
                Ok(Val::Bool(false))
            }
            // Report.WordXmlPart(Id [, ExtendedInformation]): crate::dotnet::doclayout
            "wordxmlpart" => Ok(Val::Text(crate::dotnet::doclayout::word_xml_part(self.cat, id, a.get(1).is_some_and(|v| v.as_bool())))),
            "validateandpreparelayout" => {
                crate::assume::hit("Report.ValidateAndPrepareLayout");
                Ok(Val::Void)
            }
            _ => self.rep_miss(format!("report.{m}")),
        }
    }

    /// Methods on a Report variable (after the report's own procedures).
    pub fn report_method(&mut self, f: &mut Frame, inst: &Rc<Instance>, name: &str, args: &[Expr]) -> R<Val> {
        let co = self.code(inst.code);
        if let Some(rd) = self.report_def(inst) {
            for &ec in &rd.ext_code {
                if let Some((pr, a)) = self.resolve_call(f, ec, name, args)? {
                    let e = self.rep_inst_for(inst, rd, ec);
                    return self.invoke(pr, a, Some(e), None, None);
                }
            }
        }
        let m = name.to_ascii_lowercase();
        match m.as_str() {
            "settableview" => {
                let Some(x) = args.first() else { return Ok(Val::Void) };
                let lv = self.lvalue(f, x)?;
                if let (Val::Record(src), Some(rd)) = (self.read_lval(&lv), self.report_def(inst)) {
                    self.instance_globals(inst);
                    self.rep_set_table_view(inst, rd, &src);
                }
                Ok(Val::Void)
            }
            "userequestpage" | "usereqeuestpage" => match args.first() {
                Some(x) => {
                    let on = self.eval(f, x)?.as_bool();
                    *self.rep_urp_slot(inst).borrow_mut() = Val::Bool(on);
                    Ok(Val::Void)
                }
                None => {
                    let d = self.report_def(inst).map(|r| r.use_request_page).unwrap_or(true);
                    Ok(Val::Bool(self.rep_urp(inst).unwrap_or(d)))
                }
            },
            "run" | "runmodal" => {
                self.report_run(inst, RunReq::run(true, None))?;
                Ok(Val::Void)
            }
            _ => {
                let a = self.rep_eval_args(f, args)?;
                match m.as_str() {
                    "execute" => {
                        let params = a.first().map(|v| v.to_text());
                        self.report_run(inst, RunReq::background(params, rec_arg(a.get(1)), Output::None))?;
                        Ok(Val::Void)
                    }
                    "saveas" => {
                        // Rep.SaveAs(RequestPageParameters, ReportFormat, OutStream [, RecordRef])
                        let params = a.first().map(|v| v.to_text());
                        let format = a.get(1).map(|v| v.as_i64()).unwrap_or(0);
                        self.rep_run_xml(inst, RunReq::background(params, rec_arg(a.get(3)), Output::from_format(format)), stream_target(a.get(2)))?;
                        Ok(Val::Bool(true))
                    }
                    "saveaspdf" | "saveasword" | "saveasexcel" | "saveashtml" | "saveasxml" => {
                        // Rep.SaveAsXxx(FileName)
                        let out = match m.as_str() {
                            "saveaspdf" => Output::Pdf,
                            "saveasword" => Output::Word,
                            "saveasexcel" => Output::Excel,
                            "saveashtml" => Output::Html,
                            _ => Output::Xml,
                        };
                        let target = a.first().map(|v| Target::File(v.to_text()));
                        self.rep_run_xml(inst, RunReq::background(None, None, out), target)?;
                        Ok(Val::Bool(true))
                    }
                    "print" => {
                        let params = a.first().map(|v| v.to_text());
                        self.report_run(inst, RunReq::background(params, None, Output::Print))?;
                        Ok(Val::Void)
                    }
                    "runrequestpage" => {
                        let params = a.first().map(|v| v.to_text());
                        Ok(Val::Text(self.report_run_request_page(inst, params)?))
                    }
                    "objectid" => Ok(Val::Text(if a.first().is_some_and(|v| v.as_bool()) { format!("Report {}", co.name) } else { format!("Report {}", co.id) })),
                    "language" => self.rep_lang(inst, a.first()),
                    "formatregion" => self.rep_region(inst, a.first()),
                    "printonlyifdetail" | "preview" | "istraced" => {
                        crate::assume::hit("Report.PrintOnlyIfDetail");
                        Ok(Val::Bool(false))
                    }
                    "defaultlayout" => {
                        crate::assume::hit("Report.DefaultLayout");
                        Ok(Val::Opt { ord: system_option_ord("defaultlayout", "rdlc"), enum_id: None })
                    }
                    "targetformat" => {
                        crate::assume::hit("Report.TargetFormat");
                        Ok(Val::Opt { ord: 0, enum_id: None })
                    }
                    "caption" => Ok(Val::Text(self.report_def(inst).map(|r| r.caption.clone()).unwrap_or_default())),
                    _ => err(format!("Method '{name}' not found on report '{}'", co.name)),
                }
            }
        }
    }

    fn rep_lang(&mut self, inst: &Rc<Instance>, set: Option<&Val>) -> R<Val> {
        let gl = self.global_language;
        Ok(Val::Int(self.rep_state(inst, |s| {
            if let Some(v) = set {
                s.language = v.as_i64() as i32;
            }
            if s.language == 0 {
                gl
            } else {
                s.language
            }
        })))
    }

    fn rep_region(&mut self, inst: &Rc<Instance>, set: Option<&Val>) -> R<Val> {
        Ok(Val::Text(self.rep_state(inst, |s| {
            if let Some(v) = set {
                s.format_region = v.to_text();
            }
            s.format_region.clone()
        })))
    }

    /// CurrReport.X inside report code.
    pub fn curr_report(&mut self, f: &mut Frame, m: &str, args: &[Expr]) -> R<Val> {
        match m {
            "skip" => return err_code(SKIP, "CurrReport.Skip() was called outside a data item."),
            "break" => return err_code(BREAK, "CurrReport.Break() was called outside a data item."),
            "quit" => return err_code(QUIT, "CurrReport.Quit()"),
            _ => {}
        }
        let a = self.rep_eval_args(f, args)?;
        let cur = self.rep_current(f);
        match m {
            "userequestpage" => match (&cur, a.first()) {
                (Some(i), Some(v)) => {
                    *self.rep_urp_slot(i).borrow_mut() = Val::Bool(v.as_bool());
                    Ok(Val::Void)
                }
                (Some(i), None) => {
                    let eff = self.rep_state(i, |s| s.effective_request_page);
                    Ok(Val::Bool(eff.or_else(|| self.rep_urp(i)).unwrap_or(true)))
                }
                (None, _) => Ok(Val::Bool(false)),
            },
            "objectid" => {
                let co = cur.as_ref().map(|i| self.code(i.code));
                Ok(Val::Text(match co {
                    Some(co) if a.first().is_some_and(|v| v.as_bool()) => format!("Report {}", co.name),
                    Some(co) => format!("Report {}", co.id),
                    None => String::new(),
                }))
            }
            "language" => match &cur {
                Some(i) => self.rep_lang(i, a.first()),
                None => Ok(Val::Int(self.global_language)),
            },
            "formatregion" => match &cur {
                Some(i) => self.rep_region(i, a.first()),
                None => Ok(Val::Text(String::new())),
            },
            // CurrReport.Preview: the running report previews (BC NavReport.Preview)
            "preview" => Ok(Val::Bool(cur.as_ref().is_some_and(|i| self.rep_state(i, |s| s.preview)))),
            "isreadonly" | "printonlyifdetail" => {
                crate::assume::hit("CurrReport.IsReadOnly");
                Ok(Val::Bool(false))
            }
            "showoutput" => {
                crate::assume::hit("CurrReport.ShowOutput");
                Ok(Val::Bool(true))
            }
            "pageno" => {
                crate::assume::hit("CurrReport.PageNo");
                Ok(Val::Int(1))
            }
            "totalscausedby" => {
                crate::assume::hit("CurrReport.TotalsCausedBy");
                Ok(Val::Int(0))
            }
            "targetformat" => {
                crate::assume::hit("Report.TargetFormat");
                Ok(Val::Opt { ord: 0, enum_id: None })
            }
            "settableview" | "setselectionfilter" => {
                crate::assume::hit("CurrReport.SetTableView");
                Ok(Val::Void)
            }
            "createtotals" | "newpage" | "newpagepergroup" | "printed" => Ok(Val::Void),
            "requestoptionspage" => Ok(Val::Opaque("CurrPage")),
            _ => {
                *self.missing.entry(format!("CurrReport.{m}")).or_default() += 1;
                crate::assume::hit("CurrReport.Unknown");
                Ok(Val::Void)
            }
        }
    }
}

// ---------------------------------------------------------------------- dataset (SaveAs Xml)

/// Where Report.SaveAs(Xml) / SaveAsXml writes the dataset.
#[derive(Clone)]
pub enum Target {
    /// A server file (the session's [crate::files::Vfs]).
    File(String),
    Stream(Rc<crate::streams::OutTarget>),
}

/// Type of a dataset column: the CLR type of BC's DataSet column (Option, Enum, Duration and
/// RecordId columns hold their text).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ct {
    Unknown,
    Text,
    Int,
    BigInt,
    Dec,
    Bool,
    Date,
    Time,
    DateTime,
    Guid,
    Blob,
}

impl Ct {
    fn xs(self) -> &'static str {
        match self {
            Ct::Int => "xs:int",
            Ct::BigInt => "xs:long",
            Ct::Dec => "xs:decimal",
            Ct::Bool => "xs:boolean",
            Ct::Date | Ct::Time | Ct::DateTime => "xs:dateTime",
            Ct::Blob => "xs:base64Binary",
            _ => "xs:string",
        }
    }

    /// The type class the RDLC Excel export needs (number, integral number, text).
    fn col_type(self) -> crate::rdlc_bind::ColType {
        use crate::rdlc_bind::ColType;
        match self {
            Ct::Int | Ct::BigInt => ColType::Integral,
            Ct::Dec => ColType::Fractional,
            Ct::Text | Ct::Guid | Ct::Blob => ColType::Text,
            Ct::Bool | Ct::Date | Ct::Time | Ct::DateTime => ColType::Other,
            Ct::Unknown => ColType::Unknown,
        }
    }

    fn of_val(v: &Val) -> Ct {
        match v {
            Val::Int(_) => Ct::Int,
            Val::BigInt(_) => Ct::BigInt,
            Val::Dec(_) => Ct::Dec,
            Val::Bool(_) => Ct::Bool,
            Val::Date(_) => Ct::Date,
            Val::Time(_) => Ct::Time,
            Val::DateTime(_) => Ct::DateTime,
            Val::Guid(_) => Ct::Guid,
            Val::Blob(_) => Ct::Blob,
            _ => Ct::Text,
        }
    }
}

/// One column of the flat dataset (in DataSet order: data items in tree order, a decimal column
/// followed by its `<Name>Format` column).
struct FlatCol {
    name: String,
    ct: Ct,
}

/// A data item column: its flat column, that of its decimal formatter, its static type.
#[derive(Clone, Copy)]
struct ColPlan {
    flat: usize,
    fmt: Option<usize>,
    ct: Ct,
}

/// An evaluated column of the current row.
struct Cell {
    plan: ColPlan,
    name: &'static str,
    /// SaveAs(Xml): the value formatted for the report's region (None: DBNull, empty).
    tree: String,
    /// TestRequestPage.SaveAsXml: XmlConvert form (None: DBNull, the element is left out).
    flat: Option<String>,
    /// .NET format string of a decimal column.
    fmt: Option<String>,
}

/// The dataset of one report run.
pub struct Dataset {
    /// The RDLC DataSet of TestRequestPage.SaveAsXml (else the ReportDataSet tree).
    flat: bool,
    target: Option<Target>,
    params_file: Option<String>,
    /// Per data item, per column.
    plan: Vec<Vec<ColPlan>>,
    cols: Vec<FlatCol>,
    /// Data items with a current record (BC's metaDataItems), outermost first, and whether the
    /// current record's DataItem element has been written (tree).
    path: Vec<(usize, bool)>,
    xml: XmlOut,
    rows: Vec<Vec<(usize, String)>>,
    labels: Vec<(String, String)>,
    /// AutoFormat translations: (AutoFormatType, AutoFormatExpression) → AL format.
    formats: HashMap<(i64, String), String>,
    language: String,
    region: String,
    /// The rendered format the dataset is collected for (PDF, Word, Excel, HTML); None: the
    /// dataset XML itself.
    render: Option<Output>,
    /// The rows of a rendered report, in output order: (depth, data item, column name/value).
    lines: Vec<(usize, &'static str, Vec<(&'static str, String)>)>,
    /// Rows written so far (every rep_ds_row call, in any dataset mode).
    nrows: usize,
}

/// An indenting XML writer with .NET XmlWriter's output conventions (CRLF, two spaces, `<x />`).
#[derive(Default)]
struct XmlOut {
    s: String,
    open: Vec<&'static str>,
    /// the last start tag is not closed yet (`>` or ` />` follows)
    pending: bool,
}

impl XmlOut {
    fn indent(&mut self) {
        self.s.push_str("\r\n");
        for _ in 0..self.open.len() {
            self.s.push_str("  ");
        }
    }

    fn close_pending(&mut self) {
        if self.pending {
            self.s.push('>');
            self.pending = false;
        }
    }

    fn start(&mut self, name: &'static str, attrs: &[(&str, &str)]) {
        self.close_pending();
        self.indent();
        self.s.push('<');
        self.s.push_str(name);
        for (k, v) in attrs {
            self.s.push_str(&format!(" {k}=\"{}\"", xml_attr(v)));
        }
        self.open.push(name);
        self.pending = true;
    }

    fn text_elem(&mut self, name: &str, attrs: &[(&str, &str)], text: &str) {
        self.close_pending();
        self.indent();
        self.s.push('<');
        self.s.push_str(name);
        for (k, v) in attrs {
            self.s.push_str(&format!(" {k}=\"{}\"", xml_attr(v)));
        }
        self.s.push('>');
        self.s.push_str(&xml_text(text));
        self.s.push_str(&format!("</{name}>"));
    }

    fn end(&mut self) {
        let Some(name) = self.open.pop() else { return };
        if self.pending {
            self.s.push_str(" />");
            self.pending = false;
        } else {
            self.indent();
            self.s.push_str(&format!("</{name}>"));
        }
    }
}

fn xml_text(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            c => o.push(c),
        }
    }
    o
}

fn xml_attr(s: &str) -> String {
    xml_text(s).replace('"', "&quot;").replace('\r', "&#xD;").replace('\n', "&#xA;").replace('\t', "&#x9;")
}

/// XmlConvert.EncodeLocalName for DataSet element names.
fn xml_name(n: &str) -> String {
    let mut o = String::new();
    for (i, c) in n.chars().enumerate() {
        let ok = c.is_alphanumeric() && (i > 0 || !c.is_numeric()) || c == '_' || (i > 0 && (c == '-' || c == '.'));
        if ok {
            o.push(c);
        } else {
            o.push_str(&format!("_x{:04X}_", c as u32));
        }
    }
    o
}

/// .NET `DateTime` parts of a date (`d`), time-of-day ms (`t`).
fn naive_dt(v: &Val) -> Option<chrono::NaiveDateTime> {
    match v {
        Val::Date(d) => date_to_naive(*d).map(|n| n.and_hms_opt(0, 0, 0).unwrap()),
        Val::Time(t) if *t > 0 => {
            let ms = (*t - 1) as u32;
            chrono::NaiveDate::from_ymd_opt(1899, 12, 30).unwrap().and_hms_milli_opt(ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, 0)
        }
        Val::DateTime(t) if *t != bc_store::DATETIME_UNDEFINED && *t != 0 => chrono::DateTime::from_timestamp_millis(*t).map(|d| d.naive_utc()),
        _ => None,
    }
}

fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            o.push(if k <= c.len() { A[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
        }
    }
    o
}

fn guid_d(g: &[u8; 16]) -> String {
    format_guid(g).trim_matches(|c| c == '{' || c == '}').to_lowercase()
}

fn dec_text(d: rust_decimal::Decimal) -> String {
    d.normalize().to_string()
}

/// en-US .NET `DateTime.ToString()` forms: `d` (M/d/yyyy), `T` (h:mm:ss tt), general (both).
fn us_date(n: &chrono::NaiveDateTime) -> String {
    n.format("%-m/%-d/%Y").to_string()
}
fn us_time(n: &chrono::NaiveDateTime) -> String {
    n.format("%-I:%M:%S %p").to_string()
}

/// The two value forms of a column value of type `ct`; `text` is the text of Option/Enum values.
fn cell_texts(v: &Val, ct: Ct, text: Option<String>) -> (String, Option<String>) {
    if let Some(t) = text {
        return (t.clone(), Some(t));
    }
    let num = |v: &Val| -> Option<rust_decimal::Decimal> {
        match v {
            Val::Dec(d) => Some(*d),
            Val::Int(i) => Some((*i).into()),
            Val::BigInt(i) => Some((*i).into()),
            _ => None,
        }
    };
    match ct {
        Ct::Dec => {
            let s = num(v).map(dec_text).unwrap_or_else(|| v.to_text());
            (s.clone(), Some(s))
        }
        Ct::Int | Ct::BigInt => {
            let s = match v {
                Val::Dec(d) => d.trunc().to_string(),
                o => o.to_text(),
            };
            (s.clone(), Some(s))
        }
        Ct::Bool => {
            let b = v.as_bool();
            ((if b { "True" } else { "False" }).into(), Some(b.to_string()))
        }
        Ct::Date | Ct::Time | Ct::DateTime => match naive_dt(v) {
            None => (String::new(), None),
            Some(n) => {
                let tree = match ct {
                    Ct::Date => us_date(&n),
                    Ct::Time => us_time(&n),
                    _ => format!("{} {}", us_date(&n), us_time(&n)),
                };
                let frac = n.format("%.f").to_string();
                let frac = frac.trim_end_matches('0').trim_end_matches('.');
                (tree, Some(format!("{}{frac}+00:00", n.format("%Y-%m-%dT%H:%M:%S"))))
            }
        },
        Ct::Guid => {
            let s = match v {
                Val::Guid(g) => guid_d(g),
                o => o.to_text(),
            };
            (s.clone(), Some(s))
        }
        Ct::Blob => match v {
            Val::Blob(b) if !b.is_empty() => {
                let s = base64(b);
                (s.clone(), Some(s))
            }
            _ => (String::new(), None),
        },
        Ct::Text | Ct::Unknown => {
            let s = match v {
                Val::DateFormula(_) | Val::Opt { .. } => crate::builtins::format_val(v, 0),
                Val::Guid(g) => guid_d(g),
                o => o.to_text(),
            };
            (s.clone(), Some(s))
        }
    }
}

fn ct_of_field(fd: &FieldDef) -> Ct {
    match &fd.ty {
        FieldType::Integer => Ct::Int,
        FieldType::BigInteger => Ct::BigInt,
        FieldType::Decimal => Ct::Dec,
        FieldType::Boolean => Ct::Bool,
        FieldType::Date => Ct::Date,
        FieldType::Time => Ct::Time,
        FieldType::DateTime => Ct::DateTime,
        FieldType::Guid => Ct::Guid,
        FieldType::Blob | FieldType::Media | FieldType::MediaSet => Ct::Blob,
        _ => Ct::Text,
    }
}

fn ct_of_type(cat: &Catalog, t: &TypeRef) -> Ct {
    match t {
        TypeRef::Simple { name, .. } => match name.to_ascii_lowercase().as_str() {
            "integer" | "byte" => Ct::Int,
            "biginteger" => Ct::BigInt,
            "decimal" => Ct::Dec,
            "boolean" => Ct::Bool,
            "date" => Ct::Date,
            "time" => Ct::Time,
            "datetime" => Ct::DateTime,
            "guid" => Ct::Guid,
            "code" | "text" | "bigtext" | "secrettext" | "textbuilder" | "char" | "duration" | "dateformula" | "recordid" | "label" => Ct::Text,
            _ => Ct::Unknown,
        },
        TypeRef::Object { kind, .. } if kind == "enum" => Ct::Text,
        TypeRef::Option(_) | TypeRef::Label { .. } | TypeRef::TextConst(_) => Ct::Text,
        TypeRef::Array { elem, .. } => ct_of_type(cat, elem),
        _ => Ct::Unknown,
    }
}

/// Return types of well-known functions used in column expressions.
fn ct_of_builtin(name: &str) -> Ct {
    match name.to_ascii_lowercase().as_str() {
        "format" | "strsubstno" | "copystr" | "uppercase" | "lowercase" | "delchr" | "padstr" | "selectstr" | "convertstr" | "incstr" | "delstr" | "insstr"
        | "companyname" | "userid" | "tenantid" | "getlasterrortext" | "getfilters" | "getfilter" | "tablecaption" | "fieldcaption" | "displayname"
        | "tablename" | "fieldname" | "getview" | "objectid" | "serialnumber" | "applicationpath" | "url" | "geturl" | "trim" | "trimstart" | "trimend"
        | "replace" | "substring" | "totext" | "caption" => Ct::Text,
        "today" | "workdate" | "dmy2date" | "calcdate" | "closingdate" | "normaldate" | "dwy2date" | "dt2date" | "variant2date" => Ct::Date,
        "time" | "dt2time" | "variant2time" => Ct::Time,
        "currentdatetime" | "createdatetime" | "rounddatetime" => Ct::DateTime,
        "strlen" | "strpos" | "date2dmy" | "date2dwy" | "maxstrlen" | "count" | "countapprox" | "indexof" | "lastindexof" | "pageno" | "language" => Ct::Int,
        "round" | "abs" | "power" => Ct::Dec,
        "createguid" | "systemid" => Ct::Guid,
        "isempty" | "find" | "findfirst" | "findlast" | "findset" | "get" | "hasfilter" | "contains" | "startswith" | "endswith" | "guiallowed" => Ct::Bool,
        _ => Ct::Unknown,
    }
}

/// Static typing of column expressions (the type BC's compiler gives the column).
struct ColTyper<'a> {
    cat: &'a Catalog,
    rd: &'a ReportDef,
    /// code objects searched for globals and procedures: the column's, then the base report's
    codes: [u32; 2],
    table: Option<i64>,
}

impl<'a> ColTyper<'a> {
    fn global(&self, n: &str) -> Option<&'a TypeRef> {
        self.codes.iter().find_map(|&c| self.cat.code[c as usize].globals.iter().find(|v| v.name.eq_ignore_ascii_case(n)).map(|v| &v.ty))
    }

    fn proc_ret(&self, code: u32, n: &str) -> Option<Ct> {
        let co = &self.cat.code[code as usize];
        let &p = co.find(n).first()?;
        Some(co.procs[p as usize].ret.as_ref().map(|t| ct_of_type(self.cat, t)).unwrap_or(Ct::Unknown))
    }

    fn own_proc_ret(&self, n: &str) -> Option<Ct> {
        self.codes.iter().find_map(|&c| self.proc_ret(c, n))
    }

    /// The table of a record variable or data item named `n`.
    fn record_table(&self, n: &str) -> Option<i64> {
        if let Some(i) = self.rd.dataitem(n) {
            return self.rd.dataitems[i].table;
        }
        match self.global(n)? {
            TypeRef::Object { kind, target, .. } if kind == "record" => self.cat.id_of(ObjectKind::Table, target),
            _ => None,
        }
    }

    fn codeunit(&self, n: &str) -> Option<u32> {
        match self.global(n)? {
            TypeRef::Object { kind, target, .. } if kind == "codeunit" => {
                let id = self.cat.id_of(ObjectKind::Codeunit, target)?;
                self.cat.code_by_key.get(&(ObjectKind::Codeunit, id)).copied()
            }
            _ => None,
        }
    }

    /// The field an expression refers to (`Field`, `"Field"`, `DataItem.Field`, `RecVar.Field`).
    fn field(&self, e: &Expr) -> Option<&'a FieldDef> {
        match e {
            Expr::Ident(n) if self.global(n).is_none() => {
                let t = self.cat.tables.get(&self.table?)?;
                t.field(n).map(|i| &t.fields[i])
            }
            Expr::Member(b, n) => {
                let Expr::Ident(x) = &**b else { return None };
                let t = self.cat.tables.get(&self.record_table(x)?)?;
                t.field(n).map(|i| &t.fields[i])
            }
            _ => None,
        }
    }

    fn ct(&self, e: &Expr) -> Ct {
        if let Some(fd) = self.field(e) {
            return ct_of_field(fd);
        }
        match e {
            Expr::Int(_) => Ct::Int,
            Expr::BigInt(_) => Ct::BigInt,
            Expr::Dec(_) => Ct::Dec,
            Expr::Str(_) => Ct::Text,
            Expr::Bool(_) => Ct::Bool,
            Expr::Date(_) => Ct::Date,
            Expr::Time(_) => Ct::Time,
            Expr::DateTime(_) => Ct::DateTime,
            Expr::Ident(n) => match self.global(n) {
                Some(t) => ct_of_type(self.cat, t),
                None => self.own_proc_ret(n).unwrap_or_else(|| ct_of_builtin(n)),
            },
            Expr::Member(b, n) => self.member_ct(b, n),
            Expr::Call(callee, _) => match &**callee {
                Expr::Ident(n) => self.own_proc_ret(n).unwrap_or_else(|| ct_of_builtin(n)),
                Expr::Member(b, n) => self.member_ct(b, n),
                _ => Ct::Unknown,
            },
            Expr::Binary(op, l, r) => match op {
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::And | BinOp::Or | BinOp::Xor | BinOp::In => Ct::Bool,
                BinOp::Div => Ct::Dec,
                BinOp::IntDiv | BinOp::Mod => Ct::Int,
                BinOp::Add | BinOp::Sub | BinOp::Mul => {
                    let (a, b) = (self.ct(l), self.ct(r));
                    match (a, b) {
                        (Ct::Dec, _) | (_, Ct::Dec) => Ct::Dec,
                        (Ct::Text, _) | (_, Ct::Text) => Ct::Text,
                        (Ct::Date, Ct::Date) => Ct::Int,
                        (Ct::Date, _) => Ct::Date,
                        (Ct::BigInt, _) | (_, Ct::BigInt) => Ct::BigInt,
                        (Ct::Int, Ct::Int) => Ct::Int,
                        _ => Ct::Unknown,
                    }
                }
            },
            Expr::Unary(UnOp::Not, _) => Ct::Bool,
            Expr::Unary(_, x) => self.ct(x),
            Expr::Scope(..) => Ct::Text,
            Expr::Ternary(_, a, _) => self.ct(a),
            Expr::Index(b, _) => match &**b {
                Expr::Ident(n) => match self.global(n) {
                    Some(TypeRef::Array { elem, .. }) => ct_of_type(self.cat, elem),
                    _ => Ct::Unknown,
                },
                _ => Ct::Unknown,
            },
            _ => Ct::Unknown,
        }
    }

    /// `X.Name` / `X.Name(...)`: a record method or table procedure, a codeunit procedure.
    fn member_ct(&self, b: &Expr, n: &str) -> Ct {
        let Expr::Ident(x) = b else { return ct_of_builtin(n) };
        if let Some(t) = self.record_table(x).and_then(|t| self.cat.tables.get(&t)) {
            if let Some(i) = t.field(n) {
                return ct_of_field(&t.fields[i]);
            }
            return self.proc_ret(t.code, n).unwrap_or_else(|| ct_of_builtin(n));
        }
        if let Some(cu) = self.codeunit(x) {
            return self.proc_ret(cu, n).unwrap_or(Ct::Unknown);
        }
        ct_of_builtin(n)
    }
}

/// ALCompiler.ConvertToDotNetFormatString for a report (no currency symbol).
pub fn dotnet_format(format: &str) -> String {
    const FAIL: &str = "#,##0.00";
    let strip = |f: &str, tag: &str| -> String {
        match f.find(tag) {
            Some(i) => match f[i..].find('>') {
                Some(j) => format!("{}{}", &f[..i], &f[i + j + 1..]),
                None => f.to_string(),
            },
            None => f.to_string(),
        }
    };
    let mut f = strip(&strip(format, "<C,"), "<S,");
    match f.to_ascii_lowercase().as_str() {
        "<precision,2:2><standard format,0>" => return "#,##0.00".into(),
        "<precision,2:5><standard format,0>" => return "#,##0.00###".into(),
        "<precision,0:0><standard format,0>" => return "#,##0".into(),
        "<precision,0:3><standard format,0>" => return "#,##0.###".into(),
        "<precision,3:3><standard format,0>" => return "#,##0.000".into(),
        _ => {}
    }
    if f.starts_with("<Custom,") && f.ends_with('>') {
        f = f["<Custom,".len()..f.len() - 1].to_string();
    }
    if let Some(rest) = f.strip_prefix("<FormatString>") {
        return if rest.is_empty() { FAIL.into() } else { rest.into() };
    }
    if f.is_empty() {
        return FAIL.into();
    }
    let lower = f.to_ascii_lowercase();
    let (Some(p), Some(s)) = (lower.rfind("<precision,"), lower.rfind("<standard format,")) else { return FAIL.into() };
    let (Some(s_end), Some(p_end)) = (f[s..].find('>').map(|x| x + s), f[p..].find('>').map(|x| x + p)) else { return FAIL.into() };
    let prec = &f[p + "<Precision,".len()..p_end];
    let (mut lo, mut hi) = match prec.split_once(':') {
        None => (prec.to_string(), prec.to_string()),
        Some((a, b)) => (a.to_string(), b.to_string()),
    };
    if lo.is_empty() && hi.is_empty() {
        lo = "2".into();
        hi = "2".into();
    }
    if lo.is_empty() {
        lo = "0".into();
    }
    if hi.is_empty() {
        hi = lo.clone();
    }
    let (Ok(lo), Ok(hi), Ok(std)) = (lo.trim().parse::<usize>(), hi.trim().parse::<usize>(), f[s + "<Standard Format,".len()..s_end].trim().parse::<i32>()) else {
        return FAIL.into();
    };
    let ret = match std {
        1 | 2 | 9 => "###0.{0}",
        3 => "#,##0.{0};#,##0.{0}-",
        4 => "###0.{0};###0.{0}-",
        _ => "#,##0.{0}",
    };
    let digits: String = (1..=hi).map(|i| if i <= lo { '0' } else { '#' }).collect();
    let full = ret.replace("{0}", &digits);
    let (mut pos, mut neg) = match full.split_once(';') {
        Some((a, b)) => (a.to_string(), Some(b.to_string())),
        None => (full, None),
    };
    let first = p.min(s);
    if first > 0 {
        let pre = &f[..first];
        neg = Some(match neg {
            Some(n) => format!("{pre}{n}"),
            None => format!("{pre}-{pos}"),
        });
        pos = format!("{pre}{pos}");
    }
    let last = p_end.max(s_end);
    if last + 1 < f.len() {
        let post = &f[last + 1..];
        pos.push_str(post);
        if let Some(n) = neg.as_mut() {
            n.push_str(post);
        }
    }
    match neg {
        Some(n) => format!("{pos};{n}"),
        None => pos,
    }
}

impl<'r> Session<'r> {
    /// Run a report that writes its dataset to `target` (Report.SaveAs Xml / SaveAsXml).
    fn rep_run_xml(&mut self, inst: &Rc<Instance>, rq: RunReq, target: Option<Target>) -> R<()> {
        self.rep_state(inst, |s| s.target = target);
        let r = self.report_run(inst, rq);
        self.rep_state(inst, |s| s.target = None);
        r
    }

    /// TestRequestPage.SaveAsXml(ParametersFileName, DataSetFileName): the report run that follows
    /// writes its request parameters and its (flat) dataset to these server files.
    pub fn report_set_test_xml(&mut self, inst: &Rc<Instance>, params_file: String, dataset_file: String) {
        self.rep_state(inst, |s| s.test_xml = Some((params_file, dataset_file)));
    }

    /// TestRequestPage.SaveAsPdf / SaveAsWord / SaveAsExcel(FileName): the run writes its rendered
    /// output to that server file.
    pub fn report_set_target_file(&mut self, inst: &Rc<Instance>, file: String) {
        self.rep_state(inst, |s| s.target = Some(Target::File(file)));
    }

    fn rep_ds_on(&self, inst: &Rc<Instance>) -> bool {
        inst.report.borrow().as_ref().is_some_and(|s| s.ds.is_some())
    }

    fn rep_ds<T>(&self, inst: &Rc<Instance>, g: impl FnOnce(&mut Dataset) -> T) -> Option<T> {
        let mut st = inst.report.borrow_mut();
        st.as_mut().and_then(|s| s.ds.as_mut()).map(|d| g(d))
    }

    fn rep_ds_depth(&self, inst: &Rc<Instance>) -> usize {
        self.rep_ds(inst, |d| d.path.len()).unwrap_or(0)
    }

    /// Some(i): data item i got its first record; None: the innermost data item is done.
    fn rep_ds_path(&self, inst: &Rc<Instance>, idx: Option<usize>) {
        self.rep_ds(inst, |d| match idx {
            Some(i) => d.path.push((i, false)),
            None => {
                d.path.pop();
            }
        });
    }

    /// After the record of data item `idx` (and its children): close its DataItem element (tree).
    fn rep_ds_end_record(&self, inst: &Rc<Instance>, rd: &'static ReportDef, idx: usize) {
        self.rep_ds(inst, |d| {
            let Some(p) = d.path.iter_mut().rev().find(|p| p.0 == idx) else { return };
            if std::mem::take(&mut p.1) && !d.flat {
                if !rd.dataitems[idx].children.is_empty() {
                    d.xml.end();
                }
                d.xml.end();
            }
        });
    }

    fn rep_lang_tag(lcid: i32) -> String {
        crate::locales::WINDOWS_LANGUAGES.iter().find(|l| l.0 == lcid).map(|l| l.2.to_string()).unwrap_or_else(|| "en-US".into())
    }

    /// Data items in tree order (BC's MetaReport.DataItems).
    fn rep_tree_order(rd: &ReportDef) -> Vec<usize> {
        fn walk(rd: &ReportDef, i: usize, out: &mut Vec<usize>) {
            out.push(i);
            for &c in &rd.dataitems[i].children {
                walk(rd, c, out);
            }
        }
        let mut out = vec![];
        for &r in &rd.roots {
            walk(rd, r, &mut out);
        }
        out
    }

    fn rep_typer<'a>(&self, rd: &'a ReportDef, di: &'a ReportDataItem, col: &ReportColumn) -> ColTyper<'a>
    where
        'r: 'a,
    {
        ColTyper { cat: self.cat, rd, codes: [col.code, rd.code], table: di.table }
    }

    /// Start collecting the dataset of a run that produces XML.
    fn rep_ds_start(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef) -> R<()> {
        let (target, test_xml) = self.rep_state(inst, |s| (s.target.clone(), s.test_xml.clone()));
        let flat = test_xml.is_some();
        let mut cols = vec![];
        let mut plan: Vec<Vec<ColPlan>> = vec![vec![]; rd.dataitems.len()];
        let mut captions = vec![];
        for i in Self::rep_tree_order(rd) {
            let di = &rd.dataitems[i];
            for col in &di.columns {
                let typer = self.rep_typer(rd, di, col);
                let ct = col.expr.as_ref().map(|e| typer.ct(e)).unwrap_or(Ct::Text);
                let flat_i = cols.len();
                cols.push(FlatCol { name: col.name.clone(), ct });
                let fmt = (ct == Ct::Dec).then(|| {
                    cols.push(FlatCol { name: format!("{}Format", col.name), ct: Ct::Text });
                    cols.len() - 1
                });
                plan[i].push(ColPlan { flat: flat_i, fmt, ct });
                if col.include_caption {
                    let fd = col.expr.as_ref().and_then(|e| typer.field(e));
                    let cap = fd.map(|f| f.caption.clone()).filter(|c| !c.is_empty()).or_else(|| col.caption.clone()).unwrap_or_default();
                    captions.push((format!("{}Caption", col.name), cap));
                }
            }
        }
        let mut labels: Vec<(String, String)> = rd.labels.iter().map(|l| (l.name.clone(), if l.caption.is_empty() { l.name.clone() } else { l.caption.clone() })).collect();
        labels.extend(captions.into_iter().map(|(n, c)| if c.is_empty() { (n.clone(), n) } else { (n, c) }));
        let lang = self.rep_lang(inst, None)?.as_i64() as i32;
        let region = self.rep_state(inst, |s| s.format_region.clone());
        let language = Self::rep_lang_tag(lang);
        let region = if region.is_empty() { language.clone() } else { region };
        let (target, params_file) = match test_xml {
            Some((p, d)) => (Some(Target::File(d)), Some(p).filter(|p| !p.is_empty())),
            None => (target, None),
        };
        let mut d = Dataset { flat, target, params_file, plan, cols, path: vec![], xml: XmlOut::default(), rows: vec![], labels, formats: HashMap::new(), language, region, render: None, lines: vec![], nrows: 0 };
        if !flat {
            d.xml.s.push_str("<?xml version=\"1.0\" standalone=\"yes\"?>");
            let id = rd.id.to_string();
            let (lang, region) = (d.language.clone(), d.region.clone());
            d.xml.start("ReportDataSet", &[("name", &rd.name), ("id", &id), ("language", &lang), ("formatRegion", &region)]);
            if !d.labels.is_empty() {
                d.xml.start("Labels", &[]);
                let labels = d.labels.clone();
                for (n, c) in &labels {
                    d.xml.text_elem("Label", &[("name", n)], c);
                }
                d.xml.end();
            }
            d.xml.start("DataItems", &[]);
        }
        self.rep_state(inst, |s| s.ds = Some(Box::new(d)));
        Ok(())
    }

    /// AutoFormatTranslate(Type, Expression) through codeunit "Auto Format" (cached per run).
    fn rep_auto_format(&mut self, inst: &Rc<Instance>, ty: i64, expr: &str) -> R<String> {
        let key = (ty, expr.to_string());
        if let Some(s) = self.rep_ds(inst, |d| d.formats.get(&key).cloned()).flatten() {
            return Ok(s);
        }
        let mut out = String::new();
        if let Some(&ci) = self.cat.code_by_key.get(&(ObjectKind::Codeunit, 45)) {
            if let Some(&p) = self.code(ci).find("ResolveAutoFormat").first() {
                let enum_id = self.cat.enum_by_name("Auto Format").map(|e| e.id);
                let args = vec![Arg::Val(Val::Opt { ord: ty as i32, enum_id }), Arg::Val(Val::Text(expr.to_string()))];
                match self.invoke(ProcRef { code: ci, proc: p }, args, Some(Instance::new(ci)), None, None) {
                    Ok(v) => out = v.to_text(),
                    Err(Flow::Err(_)) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        self.rep_ds(inst, |d| d.formats.insert(key, out.clone()));
        Ok(out)
    }

    /// The `decimalformatter` of a decimal column (DataItemIterator.GetDecimalString): the
    /// column's AutoFormat, the field's AutoFormat, the column's then the field's DecimalPlaces,
    /// else the amount format.
    fn rep_dec_format(&mut self, inst: &Rc<Instance>, fr: &mut Frame, col: &ReportColumn, field: Option<&'static FieldDef>, rec: Option<&RecHandle>) -> R<String> {
        let mut s = String::new();
        if let Some(t) = col.auto_format_type {
            let expr = match &col.auto_format_expr {
                Some(e) => self.eval(fr, e)?.to_text(),
                None => String::new(),
            };
            s = self.rep_auto_format(inst, t, &expr)?;
        }
        if s.is_empty() {
            if let Some(fd) = field {
                if let Some(t) = fd.prop_src.get("autoformattype").and_then(|t| t.trim().parse::<i64>().ok()) {
                    // AutoFormatExpression: a field of the same record (usually "Currency Code")
                    let expr = match (fd.prop_src.get("autoformatexpression"), rec) {
                        (Some(x), Some(r)) => {
                            let name = x.trim().trim_start_matches("Rec.").trim_matches('"').to_string();
                            let fi = r.borrow().tdef.field(&name);
                            match fi {
                                Some(i) => r.borrow().get_val(i).to_text(),
                                None if x.trim().starts_with('\'') => x.trim().trim_matches('\'').to_string(),
                                None => String::new(),
                            }
                        }
                        _ => String::new(),
                    };
                    s = self.rep_auto_format(inst, t, &expr)?;
                }
            }
        }
        let places = |dp: &str| if dp.trim().is_empty() { String::new() } else { format!("<Precision,{}><Standard Format,0>", dp.trim()) };
        if s.is_empty() {
            s = col.decimal_places.as_deref().map(places).unwrap_or_default();
        }
        if s.is_empty() {
            s = field.and_then(|f| f.decimal_places.as_deref()).map(places).unwrap_or_default();
        }
        if s.is_empty() {
            s = self.rep_auto_format(inst, 1, "")?;
        }
        Ok(dotnet_format(&s))
    }

    /// Evaluate the columns of data item `idx` for its current record.
    fn rep_ds_columns(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef, idx: usize) -> R<(Vec<Cell>, Option<String>)> {
        let di = &rd.dataitems[idx];
        let Some(r) = self.rep_item_rec(inst, rd, idx) else { return Ok((vec![], None)) };
        let sysid = {
            let rb = r.borrow();
            match rb.tdef.field("SystemId").map(|i| rb.get_val(i)) {
                Some(Val::Guid(g)) if g != [0; 16] => Some(guid_d(&g)),
                _ => None,
            }
        };
        let plans = self.rep_ds(inst, |d| d.plan[idx].clone()).unwrap_or_default();
        let mut cells = Vec::with_capacity(di.columns.len());
        for (ci, col) in di.columns.iter().enumerate() {
            let Some(&plan) = plans.get(ci) else { continue };
            let owner = self.rep_inst_for(inst, rd, col.code);
            let globals = self.instance_globals(&owner);
            let mut fr = Frame { code: col.code, locals: vec![], globals, rec: Some(r.clone()), xrec: None, with: vec![], inst: Some(owner), ret: None, ret_ty: None, curr_field: 0 };
            let Some(e) = &col.expr else {
                cells.push(Cell { plan, name: col.name.as_str(), tree: String::new(), flat: Some(String::new()), fmt: None });
                continue;
            };
            let field = self.field_of_expr(&mut fr, e)?;
            let v = match &field {
                Some((fr_rec, fi)) => fr_rec.borrow().get_val(*fi),
                None => self.eval(&mut fr, e)?,
            };
            let fd: Option<&'static FieldDef> = field.as_ref().map(|(fr_rec, fi)| fr_rec.borrow().field(*fi));
            let mut ct = plan.ct;
            if ct == Ct::Unknown {
                ct = Ct::of_val(&v);
                if !matches!(v, Val::Void) {
                    self.rep_ds(inst, |d| d.cols[plan.flat].ct = ct);
                }
            }
            // Option / Enum values show their caption (OptionCaption of the column first)
            let text = match &v {
                Val::Opt { ord, .. } => Some(match (&col.option_caption, fd) {
                    (Some(oc), _) => oc.split(',').nth(*ord as usize).map(|s| s.to_string()).unwrap_or_else(|| ord.to_string()),
                    (None, Some(fd)) => self.format_field_val(fd, &v),
                    (None, None) => match self.rep_opt_members(&fr, e) {
                        Some(m) => m.get(*ord as usize).map(|s| s.trim().to_string()).unwrap_or_else(|| ord.to_string()),
                        None => crate::builtins::format_val(&v, 0),
                    },
                }),
                _ => None,
            };
            let (tree, flat) = cell_texts(&v, ct, text);
            let fmt = match plan.fmt {
                Some(_) => Some(self.rep_dec_format(inst, &mut fr, col, fd, field.as_ref().map(|x| &x.0))?),
                None => None,
            };
            cells.push(Cell { plan, name: col.name.as_str(), tree, flat, fmt });
        }
        Ok((cells, sysid))
    }

    /// Option members of an Option-typed variable expression.
    fn rep_opt_members(&self, f: &Frame, e: &Expr) -> Option<&'static [String]> {
        let Expr::Ident(n) = e else { return None };
        let v = f.globals.vars.iter().find(|v| v.name.eq_ignore_ascii_case(n))?;
        match v.ty? {
            TypeRef::Option(m) => Some(m.as_slice()),
            _ => None,
        }
    }

    /// DataItemIterator.ExecuteReportDataItem: a dataset row for the current records.
    fn rep_ds_row(&mut self, inst: &Rc<Instance>, rd: &'static ReportDef) -> R<()> {
        let Some((path, flat)) = self.rep_ds(inst, |d| {
            d.nrows += 1;
            (d.path.clone(), d.flat)
        }) else {
            return Ok(());
        };
        let mut row: Vec<(usize, String)> = vec![];
        for (k, &(idx, done)) in path.iter().enumerate() {
            if done && !flat {
                continue;
            }
            let (cells, sysid) = self.rep_ds_columns(inst, rd, idx)?;
            let di = &rd.dataitems[idx];
            self.rep_ds(inst, |d| {
                if flat {
                    for c in cells {
                        if let Some(v) = c.flat {
                            row.push((c.plan.flat, v));
                        }
                        if let (Some(fi), Some(fv)) = (c.plan.fmt, c.fmt) {
                            row.push((fi, fv));
                        }
                    }
                    return;
                }
                let table = di.table.filter(|t| *t > 0).map(|t| t.to_string());
                let mut attrs: Vec<(&str, &str)> = vec![];
                let (lang, region) = (d.language.clone(), d.region.clone());
                if di.parent.is_none() {
                    attrs.push(("language", &lang));
                    attrs.push(("formatRegion", &region));
                }
                attrs.push(("name", &di.name));
                if let Some(t) = &table {
                    attrs.push(("tableId", t));
                }
                if let Some(g) = &sysid {
                    attrs.push(("systemId", g));
                }
                if d.render.is_some() {
                    d.lines.push((k, di.name.as_str(), cells.iter().map(|c| (c.name, c.tree.clone())).collect()));
                }
                d.xml.start("DataItem", &attrs);
                if !di.columns.is_empty() {
                    d.xml.start("Columns", &[]);
                    for c in &cells {
                        match &c.fmt {
                            Some(f) => d.xml.text_elem("Column", &[("name", c.name), ("decimalformatter", f)], &c.tree),
                            None => d.xml.text_elem("Column", &[("name", c.name)], &c.tree),
                        }
                    }
                    d.xml.end();
                }
                if !di.children.is_empty() {
                    d.xml.start("DataItems", &[]);
                }
                d.path[k].1 = true;
            });
        }
        if flat {
            row.sort_by_key(|c| c.0);
            self.rep_ds(inst, |d| d.rows.push(row));
        }
        Ok(())
    }

    /// Write the finished dataset to its target.
    fn rep_ds_finish(&mut self, rd: &'static ReportDef, mut d: Dataset) -> R<()> {
        if let Some(fmt) = d.render {
            // the layout decides the formats (ReportResultSetProcessorFactory): an Excel layout
            // renders only Excel, a Word layout no Excel
            let layout = default_layout_kind(rd);
            if (fmt == Output::Excel && layout == "word") || (fmt != Output::Excel && layout == "excel") {
                let name = match fmt {
                    Output::Pdf => "Pdf",
                    Output::Word => "Word",
                    Output::Excel => "Excel",
                    _ => "Html",
                };
                // own wording [src:X20]
                return err(format!("The {name} format cannot be produced from this report's layout."));
            }
            // BC's renderers refuse an empty dataset (Word, RDLC and HTML output)
            if d.lines.is_empty() && fmt != Output::Excel {
                return err("The report couldn\u{2019}t be generated, because it was empty. Adjust your filters and try again.");
            }
            // an RDLC layout's Excel output: the layout engine (crate::rdlc_xlsx), else the dataset
            let bytes = match fmt {
                Output::Excel => {
                    // the layout engine's cells need the format region and each column's AL type
                    let env = crate::rdlc_xlsx::ExportEnv { user_language: d.language.clone(), format_region: d.region.clone(), column_types: d.cols.iter().map(|c| (c.name.clone(), c.ct.col_type())).collect() };
                    self.rdlc_excel(rd, &d.lines, &d.labels, &env).unwrap_or_else(|| render_report(fmt, rd, &d.lines))
                }
                _ => render_report(fmt, rd, &d.lines),
            };
            self.rep_write_target(&d.target, bytes);
            return Ok(());
        }
        let xml = if d.flat {
            flat_dataset_xml(&d)
        } else {
            while !d.xml.open.is_empty() {
                d.xml.end();
            }
            std::mem::take(&mut d.xml.s)
        };
        // BC_RUST_DATASET_DUMP=<dir>: keep a copy of every dataset (debugging)
        if let Some(dir) = std::env::var_os("BC_RUST_DATASET_DUMP") {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let kind = if d.flat { "flat" } else { "tree" };
            let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{}-{n}-report{}-{kind}.xml", std::process::id(), rd.id)), &xml);
        }
        if let Some(p) = &d.params_file {
            self.files.write(p, report_parameters_xml(&d.labels).into_bytes());
        }
        match &d.target {
            Some(Target::File(f)) => self.files.write(f, xml.into_bytes()),
            Some(Target::Stream(t)) => match &**t {
                crate::streams::OutTarget::Buffer(b) => b.borrow_mut().extend_from_slice(xml.as_bytes()),
                crate::streams::OutTarget::Field(r, fi) => {
                    let mut rb = r.borrow_mut();
                    match &mut rb.row[*fi] {
                        bc_store::Value::Blob(b) => b.extend_from_slice(xml.as_bytes()),
                        other => *other = bc_store::Value::Blob(xml.into_bytes()),
                    }
                }
            },
            None => {}
        }
        Ok(())
    }
}

impl<'r> Session<'r> {
    /// Write report output to the SaveAs target (a server file or an OutStream).
    /// The Excel workbook of a report whose layout is RDLC, from the layout engine; None (with the
    /// reason in Session::missing) when the engine has no answer.
    fn rdlc_excel(&mut self, rd: &'static ReportDef, lines: &[(usize, &'static str, Vec<(&'static str, String)>)], labels: &[(String, String)], env: &crate::rdlc_xlsx::ExportEnv) -> Option<Vec<u8>> {
        if default_layout_kind(rd) != "rdlc" {
            return None;
        }
        let unq = |v: &str| v.trim().trim_matches('"').to_string();
        let wanted = rd.props.get("defaultrenderinglayout").map(|v| unq(v));
        let layout = rd
            .layout_list
            .iter()
            .find(|l| wanted.as_deref().is_some_and(|w| l.name.eq_ignore_ascii_case(w)) && l.kind.eq_ignore_ascii_case("rdlc"))
            .or_else(|| rd.layout_list.iter().find(|l| l.kind.eq_ignore_ascii_case("rdlc")))?;
        // the layout file is relative to the app's folder (the one with app.json)
        let src = self.code(layout.code).file.clone();
        let Some(dir) = src.ancestors().skip(1).find(|d| d.join("app.json").exists()) else {
            *self.missing.entry(format!("RDLC layout app folder of {}", src.display())).or_default() += 1;
            return None;
        };
        let path = dir.join(layout.file.trim_start_matches("./"));
        let xml = match std::fs::read_to_string(&path) {
            Ok(x) => x,
            Err(_) => {
                *self.missing.entry(format!("RDLC layout file {}", layout.file)).or_default() += 1;
                return None;
            }
        };
        match crate::rdlc_xlsx::excel_with(&xml, lines, labels, &self.user_id, env) {
            Ok(b) => Some(b),
            Err(e) => {
                *self.missing.entry(format!("RDLC layout engine: {e}")).or_default() += 1;
                None
            }
        }
    }

    fn rep_write_target(&mut self, target: &Option<Target>, bytes: Vec<u8>) {
        match target {
            Some(Target::File(f)) => self.files.write(f, bytes),
            Some(Target::Stream(t)) => match &**t {
                crate::streams::OutTarget::Buffer(b) => b.borrow_mut().extend_from_slice(&bytes),
                crate::streams::OutTarget::Field(r, fi) => {
                    let mut rb = r.borrow_mut();
                    match &mut rb.row[*fi] {
                        bc_store::Value::Blob(b) => b.extend_from_slice(&bytes),
                        other => *other = bc_store::Value::Blob(bytes),
                    }
                }
            },
            None => {}
        }
    }
}

/// The type of the layout a report renders with by default (lower-cased): its
/// DefaultRenderingLayout's type, else the legacy DefaultLayout property (RDLC unless Word/Excel).
pub(crate) fn default_layout_kind(rd: &ReportDef) -> String {
    let unq = |v: &str| v.trim().trim_matches('"').to_string();
    if let Some(n) = rd.props.get("defaultrenderinglayout") {
        let n = unq(n);
        if let Some(l) = rd.layout_list.iter().find(|l| l.name.eq_ignore_ascii_case(&n)) {
            return l.kind.to_ascii_lowercase();
        }
    }
    match rd.props.get("defaultlayout").map(|v| unq(v).to_ascii_lowercase()) {
        Some(k) if k == "word" || k == "excel" => k,
        _ if rd.layout_list.is_empty() || rd.layout_list.iter().any(|l| l.kind.eq_ignore_ascii_case("rdlc")) => "rdlc".into(),
        _ => rd.layout_list[0].kind.to_ascii_lowercase(),
    }
}

/// Rendered output of a report. bc-rust has no layout engine yet: the document is a structurally
/// valid file of the format with the dataset as text, one line per data item row (indented by
/// nesting), so that layout fidelity can be added per format later.
fn render_report(fmt: Output, rd: &ReportDef, lines: &[(usize, &'static str, Vec<(&'static str, String)>)]) -> Vec<u8> {
    let text: Vec<String> = lines
        .iter()
        .map(|(depth, di, cols)| {
            let vals = cols.iter().filter(|(_, v)| !v.is_empty()).map(|(n, v)| format!("{n}: {v}")).collect::<Vec<_>>().join("; ");
            format!("{}{di}: {vals}", "  ".repeat(*depth))
        })
        .collect();
    match fmt {
        Output::Pdf => render_pdf(&rd.caption, &text),
        Output::Html => render_html(&rd.caption, lines).into_bytes(),
        Output::Word => render_docx(&rd.caption, &text),
        Output::Excel => render_xlsx(lines),
        _ => vec![],
    }
}

fn render_text(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' => o.push(' '),
            c => o.push(c),
        }
    }
    o
}

/// PDF 1.4, A4 pages of 60 lines in Helvetica (WinAnsi; other characters as '?').
fn render_pdf(title: &str, lines: &[String]) -> Vec<u8> {
    let lit = |s: &str| -> String {
        let mut o = String::new();
        for ch in s.chars().take(120) {
            let c = if (ch as u32) < 256 { ch } else { '?' };
            match c {
                '(' | ')' | '\\' => {
                    o.push('\\');
                    o.push(c);
                }
                c if (c as u32) < 32 => o.push(' '),
                c if (c as u32) < 128 => o.push(c),
                c => o.push_str(&format!("\\{:03o}", c as u32)),
            }
        }
        o
    };
    let mut all = vec![title.to_string(), String::new()];
    all.extend(lines.iter().cloned());
    let pages: Vec<&[String]> = all.chunks(60).collect();
    let n = pages.len();
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec(),
    ];
    for (i, page) in pages.iter().enumerate() {
        let mut c = String::from("BT /F1 9 Tf 11 TL 40 800 Td\n");
        for l in page.iter() {
            c.push_str(&format!("({}) Tj T*\n", lit(l)));
        }
        c.push_str("ET");
        objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", 5 + 2 * i).into_bytes());
        let mut st = format!("<< /Length {} >>\nstream\n", c.len()).into_bytes();
        st.extend_from_slice(c.as_bytes());
        st.extend_from_slice(b"\nendstream");
        objs.push(st);
    }
    let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offs = vec![];
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn render_html(title: &str, lines: &[(usize, &'static str, Vec<(&'static str, String)>)]) -> String {
    let mut h = format!("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>{t}</title></head><body>\n<h1>{t}</h1>\n", t = render_text(title));
    for (depth, di, cols) in lines {
        h.push_str(&format!("<div class=\"dataitem\" style=\"margin-left:{}em\"><b>{}</b>", depth * 2, render_text(di)));
        for (n, v) in cols.iter().filter(|(_, v)| !v.is_empty()) {
            h.push_str(&format!(" <span title=\"{}\">{}</span>", render_text(n), render_text(v)));
        }
        h.push_str("</div>\n");
    }
    h.push_str("</body></html>\n");
    h
}

fn render_docx(title: &str, lines: &[String]) -> Vec<u8> {
    let paras: String = std::iter::once(title)
        .chain(lines.iter().map(|s| s.as_str()))
        .map(|l| format!("<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>", render_text(l)))
        .collect();
    let doc = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{paras}<w:sectPr/></w:body></w:document>");
    let ct = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
    crate::dotnet::zip::write_zip(&[("[Content_Types].xml".into(), ct.as_bytes().to_vec()), ("_rels/.rels".into(), rels.as_bytes().to_vec()), ("word/document.xml".into(), doc.into_bytes())])
}

/// A workbook with the dataset on a "Data" sheet, as an Excel layout's data sheet: a header row with
/// the column names, then one row per data item row with its ancestors' columns (flattened).
fn render_xlsx(lines: &[(usize, &'static str, Vec<(&'static str, String)>)]) -> Vec<u8> {
    let mut header: Vec<&'static str> = vec![];
    let mut rows: Vec<Vec<(&'static str, String)>> = vec![];
    let mut stack: Vec<&Vec<(&'static str, String)>> = vec![];
    for (depth, _, cols) in lines {
        stack.truncate(*depth);
        stack.push(cols);
        let row: Vec<(&'static str, String)> = stack.iter().flat_map(|c| c.iter().cloned()).collect();
        for (n, _) in &row {
            if !header.contains(n) {
                header.push(n);
            }
        }
        rows.push(row);
    }
    let col = |mut i: usize| -> String {
        let mut s = String::new();
        loop {
            s.insert(0, (b'A' + (i % 26) as u8) as char);
            if i < 26 {
                break;
            }
            i = i / 26 - 1;
        }
        s
    };
    let cell = |r: usize, c: usize, v: &str| format!("<c r=\"{}{r}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>", col(c), render_text(v));
    let mut sd = String::from("<row r=\"1\">");
    for (c, n) in header.iter().enumerate() {
        sd.push_str(&cell(1, c, n));
    }
    sd.push_str("</row>");
    for (i, row) in rows.iter().enumerate() {
        sd.push_str(&format!("<row r=\"{}\">", i + 2));
        for (c, n) in header.iter().enumerate() {
            if let Some((_, v)) = row.iter().find(|(x, _)| x == n) {
                sd.push_str(&cell(i + 2, c, v));
            }
        }
        sd.push_str("</row>");
    }
    let sheet = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>{sd}</sheetData></worksheet>");
    let wb = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"Data\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>";
    let wbrels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>";
    let ct = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/></Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>";
    crate::dotnet::zip::write_zip(&[
        ("[Content_Types].xml".into(), ct.as_bytes().to_vec()),
        ("_rels/.rels".into(), rels.as_bytes().to_vec()),
        ("xl/workbook.xml".into(), wb.as_bytes().to_vec()),
        ("xl/_rels/workbook.xml.rels".into(), wbrels.as_bytes().to_vec()),
        ("xl/worksheets/sheet1.xml".into(), sheet.into_bytes()),
    ])
}

/// The flat RDLC dataset as DataSet.WriteXml(WriteSchema) writes it.
fn flat_dataset_xml(d: &Dataset) -> String {
    let mut x = String::from("<?xml version=\"1.0\" standalone=\"yes\"?>\r\n<DataSet>\r\n");
    x.push_str("  <xs:schema id=\"DataSet\" xmlns=\"\" xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:msdata=\"urn:schemas-microsoft-com:xml-msdata\">\r\n");
    x.push_str("    <xs:element name=\"DataSet\" msdata:IsDataSet=\"true\" msdata:MainDataTable=\"Result\" msdata:UseCurrentLocale=\"true\">\r\n");
    x.push_str("      <xs:complexType>\r\n        <xs:choice minOccurs=\"0\" maxOccurs=\"unbounded\">\r\n          <xs:element name=\"Result\">\r\n            <xs:complexType>\r\n");
    x.push_str("              <xs:sequence>\r\n");
    for c in &d.cols {
        let guid = if c.ct == Ct::Guid { " msdata:DataType=\"System.Guid, System.Private.CoreLib, Version=8.0.0.0, Culture=neutral, PublicKeyToken=7cec85d7bea7798e\"" } else { "" };
        x.push_str(&format!("                <xs:element name=\"{}\"{guid} type=\"{}\" minOccurs=\"0\" />\r\n", xml_name(&c.name), c.ct.xs()));
    }
    x.push_str("              </xs:sequence>\r\n            </xs:complexType>\r\n          </xs:element>\r\n        </xs:choice>\r\n      </xs:complexType>\r\n    </xs:element>\r\n  </xs:schema>\r\n");
    for row in &d.rows {
        if row.is_empty() {
            x.push_str("  <Result />\r\n");
            continue;
        }
        x.push_str("  <Result>\r\n");
        for (i, v) in row {
            let n = xml_name(&d.cols[*i].name);
            x.push_str(&format!("    <{n}>{}</{n}>\r\n", xml_text(v)));
        }
        x.push_str("  </Result>\r\n");
    }
    x.push_str("</DataSet>");
    x
}

/// The report parameters (labels) of TestRequestPage.SaveAsXml, as XmlSerializer writes a
/// `ReportParameter[]`.
fn report_parameters_xml(labels: &[(String, String)]) -> String {
    let mut x = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<ArrayOfReportParameter xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">");
    for (n, v) in labels {
        x.push_str(&format!("\r\n  <ReportParameter>\r\n    <Name>{}</Name>\r\n    <Values>\r\n      <string>{}</string>\r\n    </Values>\r\n    <Visible>true</Visible>\r\n  </ReportParameter>", xml_text(n), xml_text(v)));
    }
    x.push_str("\r\n</ArrayOfReportParameter>");
    x
}

/// The record's view in GetView format (user filters, filter group 0).
fn view_text(r: &RecState) -> String {
    let sorting = r.schema.keys.get(r.key).map(|k| k.iter().map(|&i| format!("\"{}\"", r.tdef.fields[i].name)).collect::<Vec<_>>().join(",")).unwrap_or_default();
    let wh = r
        .filters
        .iter()
        .filter(|x| x.group == 0 && !x.text.is_empty())
        .map(|x| format!("\"{}\"=FILTER({})", r.tdef.fields[x.field].name, x.text))
        .collect::<Vec<_>>()
        .join(",");
    let mut v = format!("SORTING({sorting})");
    if !r.ascending {
        v.push_str(" ORDER(Descending)");
    }
    if !wh.is_empty() {
        v.push_str(&format!(" WHERE({wh})"));
    }
    v
}

fn xml_esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn xml_unesc(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// `<Tag name="N">text</Tag>` elements of a ReportParameters document: (name, text).
fn xml_elems(xml: &str, tag: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let open = format!("<{tag} ");
    let close = format!("</{tag}>");
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        rest = &rest[i + open.len()..];
        let Some(gt) = rest.find('>') else { break };
        let attrs = &rest[..gt];
        let name = attrs.split_once("name=\"").and_then(|(_, r)| r.split_once('"')).map(|(n, _)| xml_unesc(n)).unwrap_or_default();
        if attrs.ends_with('/') {
            out.push((name, String::new()));
            rest = &rest[gt + 1..];
            continue;
        }
        rest = &rest[gt + 1..];
        let Some(end) = rest.find(&close) else { break };
        out.push((name, xml_unesc(&rest[..end])));
        rest = &rest[end + close.len()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_xml_elements() {
        let x = r#"<?xml version="1.0" standalone="yes"?><ReportParameters name="R" id="1"><Options><Field name="NoOfPeriods">12</Field><Field name="E"/></Options><DataItems><DataItem name="Customer">VERSION(1) SORTING(Field1) WHERE(Field1=1(&lt;&gt;''))</DataItem></DataItems></ReportParameters>"#;
        assert_eq!(xml_elems(x, "Field"), vec![("NoOfPeriods".to_string(), "12".to_string()), ("E".to_string(), String::new())]);
        assert_eq!(xml_elems(x, "DataItem"), vec![("Customer".to_string(), "VERSION(1) SORTING(Field1) WHERE(Field1=1(<>''))".to_string())]);
    }
}
