//! TestPage: drives a page the way the BC test framework does. Opening runs OnInit/OnOpenPage and
//! positions on a record (OnAfterGetRecord/OnAfterGetCurrRecord); field controls read their source
//! expression and SetValue validates the field, runs the control's OnValidate and saves the record
//! (insert on the first change of a new record unless DelayedInsert, modify otherwise); actions run
//! OnAction; parts are sub-TestPages filtered by SubPageLink; OK/Cancel/Close end modal handlers.

use crate::interp::*;
use crate::record::*;
use crate::value::*;
use al_model::{CondKind, Control, FieldClass, FieldType, PageDef, ProcRef};
use al_syntax::{Expr, ObjectKind};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

pub type TpRef = Rc<RefCell<TestPage>>;

pub struct TestPage {
    pub page: i64,
    pub inst: Rc<Instance>,
    /// Page extension code instances (sharing the page's Rec).
    pub ext: Vec<Rc<Instance>>,
    pub open: bool,
    pub editable: bool,
    pub lookup: bool,
    /// The current record is new and not inserted yet (OpenNew / New()).
    pub new_pending: bool,
    /// Closing action ("ok", "cancel", "lookupok", "lookupcancel", "yes", "no").
    pub action: Option<&'static str>,
    pub parts: Vec<(String, TpRef)>,
    pub parent: Option<(Weak<RefCell<TestPage>>, &'static Control)>,
    /// TestRequestPage: the report whose request page this is (`inst` is the report instance).
    pub req: Option<i64>,
    /// The new record as OnNewRecord left it: it is inserted only once the user changes it.
    pub new_row: Option<bc_store::Row>,
    /// The new record is below the record the page was on (Next past the last record):
    /// OnNewRecord and OnInsertRecord get BelowxRec = true.
    pub new_below: bool,
    /// Validation errors recorded on controls (GetValidationError): (control name, message).
    pub verrors: Vec<(String, String)>,
}

/// Pseudo control name for TestPage.Filter; TestActions (OK(), Cancel(), ...) use "\0ok", ...
pub const FILTER_CTL: &str = "\u{0}filter";

/// The controls from the top of `tree` down to the control `name` (itself included).
fn ctl_path<'a>(tree: &'a [Control], name: &str, path: &mut Vec<&'a Control>) -> bool {
    for c in tree {
        path.push(c);
        if c.name.eq_ignore_ascii_case(name) || ctl_path(&c.children, name, path) {
            return true;
        }
        path.pop();
    }
    false
}

/// The page's repeater (the first one in the layout).
fn first_repeater(tree: &[Control]) -> Option<&Control> {
    for c in tree {
        if c.kind.eq_ignore_ascii_case("repeater") {
            return Some(c);
        }
        if let Some(r) = first_repeater(&c.children) {
            return Some(r);
        }
    }
    None
}

fn find_ctl<'a>(tree: &'a [Control], name: &str) -> Option<&'a Control> {
    for c in tree {
        if c.name.eq_ignore_ascii_case(name) {
            return Some(c);
        }
        if let Some(x) = find_ctl(&c.children, name) {
            return Some(x);
        }
    }
    None
}

fn act(name: &str) -> Val {
    Val::Opt { ord: system_option_ord("action", name), enum_id: None }
}

const PAGE_METHODS: &[&str] = &[
    "openedit", "openview", "opennew", "close", "ok", "cancel", "yes", "no", "lookupok", "lookupcancel", "edit", "view", "trap", "gotorecord",
    "gotokey", "first", "next", "previous", "prev", "last", "new", "expand", "isexpanded", "filter", "caption", "editable", "findfirstfield",
    "findnextfield", "findpreviousfield", "validationerrorcount", "getvalidationerror", "runpagebackgroundtask", "saveasxml", "saveasexcel",
    "saveaspdf", "getfield", "activate",
];

/// TestRequestPage-only methods (on a TestPage these names are controls/actions).
const REQUEST_PAGE_METHODS: &[&str] = &["preview", "print", "schedule", "saveasword"];

/// TestRequestPage data item filters are controls named "\0dataitem:<index>".
const DATAITEM_CTL: &str = "\u{0}dataitem:";

impl<'r> Session<'r> {
    fn pdef(&self, tp: &TpRef) -> &'static PageDef {
        let t = tp.borrow();
        match t.req {
            Some(r) => &self.cat.reports[&r].request_page,
            None => &self.cat.pages[&t.page],
        }
    }

    /// Request page triggers of a TestRequestPage (report and report extensions).
    fn tp_req_triggers(&self, tp: &TpRef, name: &str) -> Option<Vec<ProcRef>> {
        let r = tp.borrow().req?;
        Some(self.cat.reports[&r].request_triggers.iter().filter(|(n, _)| n == name).map(|(_, p)| *p).collect())
    }

    /// A closed TestPage for page `page_id` (the default value of a TestPage variable).
    pub fn new_test_page(&mut self, page_id: i64, inst: Option<Rc<Instance>>) -> Option<TpRef> {
        let pd = self.cat.pages.get(&page_id)?;
        let inst = inst.unwrap_or_else(|| Instance::new(pd.code));
        let tp = Rc::new(RefCell::new(TestPage {
            page: page_id,
            inst: inst.clone(),
            ext: vec![],
            open: false,
            editable: true,
            lookup: false,
            new_pending: false,
            action: None,
            parts: vec![],
            parent: None,
            req: None,
            new_row: None,
            new_below: false,
            verrors: vec![],
        }));
        inst.test_page.replace(Some(Rc::downgrade(&tp)));
        Some(tp)
    }

    /// The request page of report `report_id` on the report instance `inst` (a TestRequestPage).
    pub fn new_request_test_page(&mut self, report_id: i64, inst: Rc<Instance>) -> TpRef {
        let ext = match self.cat.reports.get(&report_id) {
            Some(rd) => self.rep_exts(&inst, rd),
            None => vec![],
        };
        Rc::new(RefCell::new(TestPage {
            page: report_id,
            inst,
            ext,
            open: false,
            editable: true,
            lookup: false,
            new_pending: false,
            action: None,
            parts: vec![],
            parent: None,
            req: Some(report_id),
            new_row: None,
            new_below: false,
            verrors: vec![],
        }))
    }

    fn tp_rec(&mut self, tp: &TpRef) -> Option<RecHandle> {
        let inst = tp.borrow().inst.clone();
        self.instance_globals(&inst);
        let r = inst.rec.borrow().clone();
        r
    }

    /// Run a page trigger on the page and its extensions; returns the base trigger's result.
    /// Boolean page triggers that return true unless they exit(false) (BC's default for these).
    fn default_true(name: &str) -> bool {
        matches!(name, "oninsertrecord" | "onmodifyrecord" | "ondeleterecord" | "onqueryclosepage")
    }

    fn tp_trigger(&mut self, tp: &TpRef, name: &str, args: &[Val]) -> R<Option<Val>> {
        self.tp_trigger_x(tp, name, args, None)
    }

    /// A page trigger with an explicit xRec (the record before the change).
    fn tp_trigger_x(&mut self, tp: &TpRef, name: &str, args: &[Val], xrec: Option<RecHandle>) -> R<Option<Val>> {
        let (inst, ext) = {
            let t = tp.borrow();
            (t.inst.clone(), t.ext.clone())
        };
        let procs: Vec<(ProcRef, Rc<Instance>)> = match self.tp_req_triggers(tp, name) {
            Some(ps) => ps.into_iter().map(|p| (p, self.tp_inst_for(tp, p.code))).collect(),
            None => std::iter::once(inst).chain(ext).filter_map(|i| self.code(i.code).trigger(name).map(|p| (ProcRef { code: i.code, proc: p }, i))).collect(),
        };
        let mut out = None;
        for (pr, i) in procs {
            self.ret_true = Self::default_true(name);
            let v = self.invoke(pr, args.iter().cloned().map(Arg::Val).collect(), Some(i), None, xrec.clone());
            self.ret_true = false;
            let v = v?;
            if out.is_none() {
                out = Some(v);
            }
        }
        Ok(out)
    }

    fn tp_has_trigger(&self, tp: &TpRef, name: &str) -> bool {
        if let Some(ps) = self.tp_req_triggers(tp, name) {
            return !ps.is_empty();
        }
        let t = tp.borrow();
        std::iter::once(&t.inst).chain(t.ext.iter()).any(|i| self.code(i.code).trigger(name).is_some())
    }

    fn tp_inst_for(&self, tp: &TpRef, code: u32) -> Rc<Instance> {
        let t = tp.borrow();
        if t.inst.code == code {
            t.inst.clone()
        } else {
            t.ext.iter().find(|i| i.code == code).cloned().unwrap_or_else(|| t.inst.clone())
        }
    }

    /// Evaluate an expression in the page's context (Rec, page globals).
    fn tp_eval(&mut self, tp: &TpRef, code: u32, e: &Expr) -> R<Val> {
        let inst = self.tp_inst_for(tp, code);
        let globals = self.instance_globals(&inst);
        let rec = inst.rec.borrow().clone();
        let mut f = Frame { code: inst.code, locals: vec![], globals, rec, xrec: None, with: vec![], inst: Some(inst), ret: None, ret_ty: None, curr_field: 0 };
        self.eval(&mut f, e)
    }

    pub(crate) fn apply_conds(&mut self, rec: &RecHandle, conds: &[al_model::Cond], src: Option<&RecHandle>, group: i32) -> R<()> {
        let tdef = rec.borrow().tdef;
        for c in conds {
            let Some(fi) = tdef.field(&c.field) else { continue };
            let fd = &tdef.fields[fi];
            let node = match c.kind {
                CondKind::Const => {
                    let ev = |n: &str| self.enum_ordinal_for_field(fd, n);
                    text_to_store(fd, &c.value, &ev, self.work_date).map(crate::filter::FNode::Eq)
                }
                CondKind::Filter => Some(self.parse_field_filter(fd, &c.value)?),
                CondKind::Field => match src {
                    Some(s) => {
                        let s = s.borrow();
                        s.tdef.field(&c.value).map(|sf| crate::filter::FNode::Eq(s.row[sf].clone()))
                    }
                    None => None,
                },
            };
            if let Some(n) = node {
                let text = match &n {
                    crate::filter::FNode::Eq(v) => to_val(fd, v).to_text(),
                    _ => c.value.clone(),
                };
                let mut r = rec.borrow_mut();
                let g = std::mem::replace(&mut r.filter_group, group);
                r.set_filter(fi, Some(n), text);
                r.filter_group = g;
            }
        }
        Ok(())
    }

    /// Open the page: OnInit, OnOpenPage, position on a record (or a new one).
    pub fn tp_open(&mut self, tp: &TpRef, mode: &str) -> R<()> {
        let pd = self.pdef(tp);
        let rec = self.tp_rec(tp);
        // the page's source record: its xRec in table code is the page's xRec (Session::xrec_of)
        if let Some(r) = &rec {
            self.page_open_recs.retain(|w| w.strong_count() > 0 && !w.upgrade().is_some_and(|h| Rc::ptr_eq(&h, r)));
            self.page_open_recs.push(Rc::downgrade(r));
        }
        {
            let mut t = tp.borrow_mut();
            if t.req.is_none() && t.ext.len() != pd.ext_code.len() {
                t.ext = pd.ext_code.iter().map(|&c| Instance::new(c)).collect();
            }
            t.editable = mode != "view" && !pd.props.get("editable").is_some_and(|v| v.eq_ignore_ascii_case("false"));
            // opened again after a close: new subpages, as BC creates them with each open
            if t.action.is_some() && t.req.is_none() {
                t.parts.clear();
                t.inst.parts.borrow_mut().clear();
            }
            t.action = None;
            t.new_pending = false;
            t.lookup = t.inst.lookup.get();
        }
        // completion triggers of page background tasks run in the extensions too (crate::pbt)
        if !pd.ext_code.is_empty() {
            let (i, x) = {
                let t = tp.borrow();
                (t.inst.clone(), t.ext.clone())
            };
            self.pbt_register(&i, &x);
        }
        if let Some(r) = &rec {
            if !pd.view.conds.is_empty() {
                self.apply_conds(r, &pd.view.conds, None, 2)?;
            }
            if !pd.view.sorting.is_empty() {
                let fields: Vec<usize> = pd.view.sorting.iter().filter_map(|n| r.borrow().tdef.field(n)).collect();
                let mut rb = r.borrow_mut();
                if let Some(k) = rb.tdef.keys.iter().position(|k| k.fields.starts_with(&fields)) {
                    let kf = rb.tdef.keys[k].fields.clone();
                    rb.key = rb.schema.keys.iter().position(|sk| *sk == kf).unwrap_or(0);
                }
                rb.ascending = !pd.view.descending;
            }
        }
        let (base, exts) = {
            let t = tp.borrow();
            (t.inst.clone(), t.ext.clone())
        };
        if tp.borrow().req.is_none() {
            for e in &exts {
                self.bind_ext_globals(e, &base);
            }
        }
        // a part's OnInit already ran with its parent (tp_init_parts); its extensions' did not
        if !base.inited.replace(true) {
            self.tp_trigger(tp, "oninit", &[])?;
        } else {
            for e in &exts {
                if let Some(p) = self.code(e.code).trigger("oninit") {
                    self.invoke(ProcRef { code: e.code, proc: p }, vec![], Some(e.clone()), None, None)?;
                }
            }
        }
        if tp.borrow().req.is_none() {
            self.tp_init_parts(tp)?;
            self.tp_apply_saved_values(tp);
        }
        self.tp_trigger(tp, "onopenpage", &[])?;
        tp.borrow_mut().open = true;
        if mode == "new" {
            self.tp_new_record(tp)?;
            return Ok(());
        }
        if rec.is_some() {
            // keep a position set by SetRecord, otherwise the first record in the view
            let found = self.tp_find(tp, "=")? || self.tp_find(tp, "-")?;
            if found {
                self.tp_after_get(tp)?;
            } else if mode == "edit" {
                // an empty page opened for editing is on a new record
                self.tp_new_record(tp)?;
            } else {
                // an empty page opened for viewing is on its initialized record, and the client
                // still asks for OnAfterGetCurrRecord (Service NSFormApplicationCode runs it unless
                // a record the dataset had is gone from the database). Journals compute their
                // batch's approval status there, which a SaveValues page would otherwise keep from
                // its last close (WFW Item Journal Batch ApprovalStatusMustBeBlank...)
                self.tp_trigger(tp, "onaftergetcurrrecord", &[])?;
                self.tp_take_update(tp);
            }
        } else if tp.borrow().req.is_none() {
            // a page without a source table runs OnAfterGetRecord / OnAfterGetCurrRecord once on
            // open (Data Encryption Management sets its actions' Enabled state there)
            self.tp_after_get(tp)?;
        }
        Ok(())
    }

    fn tp_after_get(&mut self, tp: &TpRef) -> R<()> {
        // a loaded record: the page's xRec is no longer the new-record one
        tp.borrow().inst.page_xrec.replace(None);
        tp.borrow_mut().new_below = false;
        if let Some(r) = self.tp_rec(tp) {
            // FlowFields shown on the page are calculated
            let pd = self.pdef(tp);
            let mut flows = vec![];
            collect_flow_fields(&pd.layout, r.borrow().tdef, &mut flows);
            for fi in flows {
                self.calc_field(&r, fi)?;
            }
        }
        self.tp_trigger(tp, "onaftergetrecord", &[])?;
        self.tp_trigger(tp, "onaftergetcurrrecord", &[])?;
        // this was the refresh a CurrPage.Update in these triggers asks for
        self.tp_take_update(tp);
        Ok(())
    }

    /// Whether the page's code (page or extension) called CurrPage.Update since the last check.
    fn tp_take_update(&self, tp: &TpRef) -> bool {
        let t = tp.borrow();
        std::iter::once(&t.inst).chain(t.ext.iter()).fold(false, |any, i| i.update.replace(false) || any)
    }

    /// The page refresh after CurrPage.Update: the current record again, or the first one in the
    /// view when changed filters left it outside (a journal's batch name changes the filter and
    /// updates), then OnAfterGetRecord / OnAfterGetCurrRecord. A new row nothing was typed into
    /// gives way too; one with entered values stays.
    fn tp_refresh(&mut self, tp: &TpRef) -> R<()> {
        if !tp.borrow().open || self.tp_rec(tp).is_none() {
            return Ok(());
        }
        // the page's code inserted the new record itself (a card's OnAfterGetCurrRecord creates the
        // customer/item from a template, Rec.Copy, CurrPage.Update): the page is on that record now
        if tp.borrow().new_pending {
            let r = self.tp_rec(tp).unwrap();
            if let Some(row) = self.rec_get_pk(&r) {
                tp.borrow_mut().new_pending = false;
                let mut rb = r.borrow_mut();
                let row: crate::record::CowRow = row.into();
                rb.xrow = Some(row.clone());
                rb.row = row;
            }
        }
        let blank_new = tp.borrow().new_pending && !self.tp_new_dirty(tp);
        if tp.borrow().new_pending {
            // a new row stays, unless nothing was entered and changed filters (a journal's new
            // batch name) left it outside the view
            let in_view = self.tp_rec(tp).is_some_and(|r| {
                let rb = r.borrow();
                rb.row_matches(&rb.row)
            });
            if !blank_new || in_view {
                return Ok(());
            }
            tp.borrow_mut().new_pending = false;
        }
        if self.tp_find(tp, "=")? || self.tp_find(tp, "-")? {
            self.tp_after_get(tp)?;
        } else if blank_new {
            // the untouched new row left the view and the view is empty (a journal's or
            // worksheet's new batch name): the page starts a new row from the new filters, as
            // BC's client drops the draft and creates another. Keeping the old row left Rec on the
            // old batch, so Req. Worksheet's Calculate Plan (SetTemplAndWorksheet(Rec."Journal
            // Batch Name")) planned into the batch SaveValues had restored
            // (SCM Inventory Miscellaneous TransferOrderCreatedFromRequisitionWorksheet)
            self.tp_new_record(tp)?;
        } else {
            self.tp_to_new_row(tp, false)?;
        }
        Ok(())
    }

    /// The page's new (not inserted) record has values the user entered.
    fn tp_new_dirty(&mut self, tp: &TpRef) -> bool {
        let Some(r) = self.tp_rec(tp) else { return false };
        let rb = r.borrow();
        let base = match tp.borrow().new_row.clone() {
            Some(b) => b,
            None => {
                let mut fresh = rb.schema.new_row();
                self.init_row(rb.tdef, &mut fresh);
                fresh
            }
        };
        rb.row.iter().zip(base.iter()).enumerate().any(|(i, (a, b))| a != b && !rb.filters.iter().any(|f| f.field == i) && !rb.tdef.fields[i].is_system())
    }

    fn tp_find(&mut self, tp: &TpRef, which: &str) -> R<bool> {
        if self.tp_has_trigger(tp, "onfindrecord") {
            return Ok(self.tp_trigger(tp, "onfindrecord", &[Val::text(which)])?.is_some_and(|v| v.as_bool()));
        }
        let Some(r) = self.tp_rec(tp) else { return Ok(false) };
        match which {
            "=" => match self.rec_get_pk(&r) {
                Some(row) if r.borrow().row_matches(&row) => {
                    let mut rb = r.borrow_mut();
                    let row: crate::record::CowRow = row.into();
                    rb.xrow = Some(row.clone());
                    rb.row = row;
                    Ok(true)
                }
                _ => Ok(false),
            },
            "+" => self.find_first(&r, true),
            _ => self.find_first(&r, false),
        }
    }

    fn tp_step(&mut self, tp: &TpRef, steps: i64) -> R<bool> {
        let blank_new = self.tp_save_new(tp)?;
        if self.tp_has_trigger(tp, "onnextrecord") {
            let n = self.tp_trigger(tp, "onnextrecord", &[Val::Int(steps as i32)])?.map(|v| v.as_i64()).unwrap_or(0);
            if n != 0 {
                self.tp_after_get(tp)?;
            }
            return Ok(n != 0);
        }
        let Some(r) = self.tp_rec(tp) else { return Ok(false) };
        let moved = if blank_new && steps < 0 && self.tp_has_new_row(tp) {
            // Previous from the new row: the last record
            self.tp_find(tp, "+")?
        } else if blank_new && steps > 0 {
            false
        } else {
            self.next(&r, steps)? != 0
        };
        if moved {
            self.tp_after_get(tp)?;
        } else if steps > 0 {
            // Next past the last record: the new row of an insertable list, below xRec
            tp.borrow_mut().new_below = true;
            self.tp_to_new_row(tp, blank_new)?;
            // BC's TestPageProxy.Move: from a record (not an untouched draft) the repeater's
            // MoveNext lands on the new row and returns true; only Next from the clean new row
            // returns false (Discount Setup Notifications T100 expects the 3rd line)
            if !blank_new && self.tp_has_new_row(tp) {
                return Ok(true);
            }
        }
        Ok(moved)
    }

    /// tp_save, telling whether the page was on its new row with nothing entered (a blank new
    /// row is not inserted and leaves no record position).
    fn tp_save_new(&mut self, tp: &TpRef) -> R<bool> {
        let was_new = tp.borrow().new_pending;
        self.tp_save(tp)?;
        Ok(was_new && self.tp_rec(tp).is_some_and(|r| r.borrow().xrow.is_none()))
    }

    /// An editable list page that allows inserts ends in a new, empty row, as in the BC client:
    /// navigating past the last record (Next, First/Last on an empty page) lands there.
    fn tp_has_new_row(&self, tp: &TpRef) -> bool {
        let pd = self.pdef(tp);
        let listlike = matches!(pd.page_type.to_ascii_lowercase().as_str(), "list" | "listpart" | "worksheet");
        if !(listlike && tp.borrow().editable && pd.source_table.is_some() && !pd.props.get("insertallowed").is_some_and(|v| v.eq_ignore_ascii_case("false"))) {
            return false;
        }
        // BC draws the draft row only where changes are allowed in the repeater
        // (Nav.Client.UIPatterns DraftLinePattern.AllowChangesInRepeater: Repeater.Editable and the
        // binding's Editable; RepeaterBuilder: a lookup (Select mode) repeater of a page that isn't
        // editable in that mode is not editable)
        if tp.borrow().lookup {
            return false;
        }
        if first_repeater(&pd.layout).is_some_and(|r| r.props.get("editable").is_some_and(|v| v.eq_ignore_ascii_case("false"))) {
            return false;
        }
        // a part in the parent's FactBox area is read-only
        let parent = tp.borrow().parent.as_ref().map(|(w, c)| (w.clone(), *c));
        if let Some((w, part)) = parent {
            if let Some(ptp) = w.upgrade() {
                let ppd = self.pdef(&ptp);
                let in_factboxes = ppd.layout.iter().any(|area| area.kind.eq_ignore_ascii_case("area") && area.name.eq_ignore_ascii_case("factboxes") && find_ctl(&area.children, &part.name).is_some());
                if in_factboxes {
                    return false;
                }
            }
        }
        true
    }

    /// Move to the new row (see tp_has_new_row); a blank new row the page was on stays.
    fn tp_to_new_row(&mut self, tp: &TpRef, blank_new: bool) -> R<()> {
        if !self.tp_has_new_row(tp) {
            return Ok(());
        }
        if blank_new {
            tp.borrow_mut().new_pending = true;
            return Ok(());
        }
        self.tp_new_record(tp)
    }

    /// Start a new record: Init, values of single-value filters, OnNewRecord.
    fn tp_new_record(&mut self, tp: &TpRef) -> R<()> {
        let Some(r) = self.tp_rec(tp) else { return Ok(()) };
        // xRec in OnNewRecord: the record the page was on (a new line inherits from the line
        // above), or a blank record when there is none
        let xrec = copy_record(&r);
        let was_on_record = !tp.borrow().new_pending && r.borrow().xrow.is_some();
        let populate_all = self.pdef(tp).props.get("populateallfields").is_some_and(|v| v.eq_ignore_ascii_case("true"));
        {
            let mut rb = r.borrow_mut();
            let mut blank = rb.schema.new_row();
            self.init_row(rb.tdef, &mut blank);
            if !was_on_record {
                xrec.borrow_mut().row = (blank.clone()).into();
            }
            let mut fresh = blank;
            let pk = rb.tdef.pk().fields.clone();
            for fl in rb.filters.clone() {
                // a non-key field's filter in a private filter group (the page's own, e.g. 25)
                // does not fill the new record (BC InitRecordFromFilters)
                if !populate_all && !pk.contains(&fl.field) && !matches!(fl.group, 0 | 2 | 4) {
                    continue;
                }
                if let Some(v) = fl.node.eq_value() {
                    fresh[fl.field] = v.clone();
                }
            }
            rb.row = (fresh).into();
            rb.xrow = None;
        }
        // the page's own xRec on the new record: the record it was on, else the new record itself
        // (initialized, with the filter values)
        let page_x = if was_on_record { copy_record(&xrec) } else { copy_record(&r) };
        tp.borrow().inst.page_xrec.replace(Some(page_x));
        tp.borrow_mut().new_pending = true;
        let below = tp.borrow().new_below;
        self.tp_trigger_x(tp, "onnewrecord", &[Val::Bool(below)], Some(xrec))?;
        // the new row is the current record now: OnAfterGetCurrRecord runs, as in the client
        // (subforms set the editability of a new line's fields there, e.g. CurrPageIsEditable)
        self.tp_trigger(tp, "onaftergetcurrrecord", &[])?;
        let row = r.borrow().row.to_vec();
        tp.borrow_mut().new_row = Some(row);
        if self.tp_take_update(tp) {
            self.tp_refresh(tp)?;
        }
        Ok(())
    }

    /// Write pending changes of the current record (and the parts' current records).
    pub fn tp_save(&mut self, tp: &TpRef) -> R<()> {
        let parts: Vec<TpRef> = tp.borrow().parts.iter().map(|(_, p)| p.clone()).collect();
        for p in parts {
            self.tp_save(&p)?;
        }
        if tp.borrow().new_pending {
            let dirty = match self.tp_rec(tp) {
                Some(r) => {
                    let rb = r.borrow();
                    let base = match tp.borrow().new_row.clone() {
                        Some(b) => b,
                        None => {
                            let mut fresh = rb.schema.new_row();
                            self.init_row(rb.tdef, &mut fresh);
                            fresh
                        }
                    };
                    rb.row.iter().zip(base.iter()).enumerate().any(|(i, (a, b))| a != b && !rb.filters.iter().any(|f| f.field == i) && !rb.tdef.fields[i].is_system())
                }
                None => false,
            };
            if dirty {
                self.tp_insert(tp)?;
            } else {
                tp.borrow_mut().new_pending = false;
            }
        }
        Ok(())
    }

    fn tp_insert(&mut self, tp: &TpRef) -> R<()> {
        let Some(r) = self.tp_rec(tp) else { return Ok(()) };
        let pd = self.pdef(tp);
        if pd.props.get("autosplitkey").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            let last = r.borrow().tdef.pk().fields.last().copied();
            if let Some(lf) = last {
                let probe = copy_record(&r);
                probe.borrow_mut().key = 0;
                let n = if self.find_first(&probe, true)? { probe.borrow().get_val(lf).as_i64() } else { 0 };
                let fd = r.borrow().field(lf);
                if let Ok(v) = to_store(fd, &Val::Int((n + 10000) as i32)) {
                    r.borrow_mut().row[lf] = v;
                }
            }
        }
        tp.borrow_mut().new_pending = false;
        let below = std::mem::take(&mut tp.borrow_mut().new_below);
        let go = self.tp_trigger(tp, "oninsertrecord", &[Val::Bool(below)])?;
        if !matches!(go, Some(Val::Bool(false))) {
            // OnInsert's xRec: the record as the page saves it, not a fresh one
            if let Err(m) = self.rec_op_from_code(&r, true, 'i')? {
                return err(m);
            }
        }
        if let Some(row) = self.rec_get_pk(&r) {
            let mut rb = r.borrow_mut();
            let row: crate::record::CowRow = row.into();
            rb.xrow = Some(row.clone());
            rb.row = row;
        }
        self.tp_after_get(tp)
    }

    /// The page's stored record got new key values: BC renames it (NavForm.SaveRecordAsync,
    /// RecordId <> OldRecord.RecordId): OnRename with the old record as xRec, the old key removed,
    /// the record stored under the new key. True when it renamed.
    fn tp_rename_if_key_changed(&mut self, r: &RecHandle) -> R<bool> {
        let old = {
            let rb = r.borrow();
            let Some(x) = rb.xrow.clone() else { return Ok(false) };
            if !rb.tdef.pk().fields.iter().any(|&i| rb.row[i] != x[i]) {
                return Ok(false);
            }
            x
        };
        let xr = copy_record(r);
        xr.borrow_mut().row = old;
        if self.rec_get_pk(&xr).is_none() || self.rec_get_pk(r).is_some() {
            return Ok(false);
        }
        let on_rename = r.borrow().tdef.triggers.on_rename;
        if let Some(pr) = on_rename {
            self.invoke(pr, vec![], None, Some(r.clone()), Some(xr.clone()))?;
        }
        if let Err(m) = self.rec_delete(&xr, false)? {
            return err(m);
        }
        if let Err(m) = self.rec_insert(r, false)? {
            return err(m);
        }
        let row = r.borrow().row.clone();
        r.borrow_mut().xrow = Some(row);
        Ok(true)
    }

    fn tp_modify(&mut self, tp: &TpRef) -> R<()> {
        self.tp_modify_keep(tp, false)
    }

    /// tp_modify; `keep_unchanged` (a field's SetValue): when nothing is written the page keeps
    /// its record instead of re-reading it (BC keeps the form's buffer after a field change that
    /// saves nothing; actions and page variables' triggers still re-read).
    fn tp_modify_keep(&mut self, tp: &TpRef, keep_unchanged: bool) -> R<()> {
        let Some(r) = self.tp_rec(tp) else { return Ok(()) };
        let renamed = self.tp_rename_if_key_changed(&r)?;
        let Some(stored) = self.rec_get_pk(&r) else { return Ok(()) };
        // changed by the page: compared with the record the page last read (BC OldRecord)
        let changed = {
            let rb = r.borrow();
            let base: &[bc_store::Value] = match &rb.xrow {
                Some(x) => &x[..],
                None => &stored[..],
            };
            rb.row.iter().zip(base.iter()).enumerate().any(|(i, (a, b))| a != b && rb.tdef.fields[i].class == FieldClass::Normal && !rb.tdef.fields[i].is_system())
        };
        if changed {
            let x = copy_record(&r);
            x.borrow_mut().row = (stored.clone()).into();
            let go = self.tp_trigger_x(tp, "onmodifyrecord", &[], Some(x))?;
            if !matches!(go, Some(Val::Bool(false))) {
                if let Err(m) = self.rec_modify(&r, true)? {
                    return err(m);
                }
            }
        }
        // re-read after a save; after a field change that saves nothing the page keeps its record
        // (BC keeps the form's buffer: a change another variable stored meanwhile is not shown)
        if changed || renamed || !keep_unchanged {
            self.tp_reread(&r);
        }
        self.tp_after_get(tp)
    }

    /// The page's record as stored (row and xRec), after the page or its code wrote it.
    fn tp_reread(&mut self, r: &RecHandle) {
        if let Some(row) = self.rec_get_pk(r) {
            let mut rb = r.borrow_mut();
            let row: crate::record::CowRow = row.into();
            rb.xrow = Some(row.clone());
            rb.row = row;
        }
    }

    /// SaveValues = true (BC NavForm.StoreSaveValues, ApplySourceTableViewAndSavedValuesAsync):
    /// when the page closes, other than with Cancel or No, the page variables its controls show
    /// (writable source expressions, not table fields) are saved; the next open of the page sets
    /// them again, after OnInit and before OnOpenPage. BC keeps them per user and page in the
    /// database; bc-rust per session and page. Create Payment and Standard Cost Worksheet reopen
    /// on the last batch / worksheet name this way.
    fn tp_save_values(&mut self, tp: &TpRef, action: &str) {
        if action == "cancel" || action == "no" {
            return;
        }
        let req = tp.borrow().req;
        let pd = self.pdef(tp);
        if !pd.props.get("savevalues").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return;
        }
        let mut names = vec![];
        save_value_sources(&pd.layout, &mut names);
        if names.is_empty() {
            return;
        }
        let inst = tp.borrow().inst.clone();
        let g = self.instance_globals(&inst);
        let mut saved = vec![];
        for v in &g.vars {
            if !names.iter().any(|n| n.eq_ignore_ascii_case(v.name)) {
                continue;
            }
            let val = v.slot.borrow().clone();
            // values, not references (records, objects, lists, streams, ...)
            if matches!(
                val,
                Val::Bool(_) | Val::Int(_) | Val::BigInt(_) | Val::Dec(_) | Val::Text(_) | Val::Char(_) | Val::Date(_) | Val::Time(_) | Val::DateTime(_) | Val::Duration(_) | Val::Guid(_) | Val::Opt { .. } | Val::DateFormula(_)
            ) {
                saved.push((v.name, val));
            }
        }
        match req {
            // a request page: the report's options (BC NavForm.StoreSaveValues -> SaveReportOptions
            // -> NavObjectOptions.Save, table 2000000196 in the session's transaction)
            Some(report_id) => {
                static REPORT_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let token = REPORT_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                self.sys.report_saved_values.insert(token, saved);
                let _ = self.report_options_write(report_id, token);
            }
            None => {
                let page = tp.borrow().page;
                self.sys.page_saved_values.insert(page, saved);
            }
        }
    }

    /// The report's "Last used options and filters" row in Object Options (table 2000000196; key
    /// Parameter Name, Object ID, Object Type, User Name, Company Name), as BC's NavObjectOptions
    /// for a report's saved request page values.
    fn report_options_rec(&mut self, report_id: i64) -> Option<(RecHandle, usize)> {
        const OBJECT_OPTIONS: i64 = 2000000196;
        if !self.cat.tables.contains_key(&OBJECT_OPTIONS) || !self.rt.schemas.contains_key(&OBJECT_OPTIONS) {
            return None;
        }
        let h = self.new_record(OBJECT_OPTIONS, false);
        let td = h.borrow().tdef;
        let data = td.field("Option Data")?;
        let (pn, oid, ot, cn) = (td.field("Parameter Name")?, td.field("Object ID")?, td.field("Object Type")?, td.field("Company Name")?);
        let company = self.company.clone();
        {
            let mut r = h.borrow_mut();
            r.row[pn] = bc_store::Value::Text("Last used options and filters".into());
            r.row[oid] = bc_store::Value::Int(report_id as i32);
            r.row[ot] = bc_store::Value::Option(3);
            r.row[cn] = bc_store::Value::Text(company);
        }
        Some((h, data))
    }

    fn report_options_write(&mut self, report_id: i64, token: u64) -> R<()> {
        let Some((h, data)) = self.report_options_rec(report_id) else { return Ok(()) };
        let blob = bc_store::Value::Blob(token.to_string().into_bytes());
        match self.rec_get_pk(&h) {
            Some(row) => {
                h.borrow_mut().row = row.into();
                h.borrow_mut().row[data] = blob;
                let _ = self.rec_modify(&h, false)?;
            }
            None => {
                h.borrow_mut().row[data] = blob;
                let _ = self.rec_insert(&h, false)?;
            }
        }
        Ok(())
    }

    /// BC NCLMetaReport.CreateObjectInstance: right after the report is constructed (its
    /// OnInitReport), NavReport.InitializeReportValues applies the request page's saved values
    /// (RequestPageBase.InitializeRequestPageWithCustomValues: SaveValues, not in background
    /// sessions), before the caller's InitializeRequest and the request page's OnOpenPage.
    /// Planning's "Calculate Plan - Plan. Wksh." derives MRP from the MPS restored here.
    pub(crate) fn report_restore_saved_values(&mut self, inst: &Rc<Instance>, report_id: i64) {
        let Some(rd) = self.cat.reports.get(&report_id) else { return };
        if !rd.request_page.props.get("savevalues").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return;
        }
        let Some((h, data)) = self.report_options_rec(report_id) else { return };
        let Some(row) = self.rec_get_pk(&h) else { return };
        let token: Option<u64> = match &row[data] {
            bc_store::Value::Blob(b) => std::str::from_utf8(b).ok().and_then(|t| t.parse().ok()),
            _ => None,
        };
        let Some(saved) = token.and_then(|t| self.sys.report_saved_values.get(&t).cloned()) else { return };
        let g = self.instance_globals(inst);
        for (n, val) in saved {
            if let Some(v) = g.vars.iter().find(|v| v.name.eq_ignore_ascii_case(n)) {
                *v.slot.borrow_mut() = val;
            }
        }
    }

    fn tp_apply_saved_values(&mut self, tp: &TpRef) {
        let page = tp.borrow().page;
        let Some(saved) = self.sys.page_saved_values.get(&page).cloned() else { return };
        let inst = tp.borrow().inst.clone();
        let g = self.instance_globals(&inst);
        for (n, val) in saved {
            if let Some(v) = g.vars.iter().find(|v| v.name.eq_ignore_ascii_case(n)) {
                *v.slot.borrow_mut() = val;
            }
        }
    }

    pub(crate) fn tp_close(&mut self, tp: &TpRef, action: &'static str) -> R<()> {
        if !tp.borrow().open {
            return Ok(());
        }
        if let Some(r) = self.tp_rec(tp) {
            self.page_open_recs.retain(|w| w.upgrade().is_some_and(|h| !Rc::ptr_eq(&h, &r)));
        }
        if action != "cancel" && action != "lookupcancel" {
            self.tp_save(tp)?;
        }
        let ok = self.tp_trigger(tp, "onqueryclosepage", &[act(action)])?;
        if matches!(ok, Some(Val::Bool(false))) {
            // own wording [src:X24]
            return err("OnQueryClosePage did not allow the page to close.");
        }
        self.tp_trigger(tp, "onclosepage", &[])?;
        self.tp_save_values(tp, action);
        let mut t = tp.borrow_mut();
        t.open = false;
        t.action = Some(action);
        // a later open of the page (and of its parts) runs OnInit again
        t.inst.inited.set(false);
        for (_, p) in t.inst.parts.borrow().iter() {
            p.inited.set(false);
        }
        Ok(())
    }

    /// TestField.Value := X (property assignment): the same as SetValue(X).
    pub(crate) fn testctl_assign(&mut self, f: &mut Frame, tp: &TpRef, ctl: &str, v: Val) -> R<()> {
        self.tp_parent_autoinsert(tp)?;
        let pd = self.pdef(tp);
        let Some(c) = find_ctl(&pd.layout, ctl).or_else(|| find_ctl(&pd.actions, ctl)) else {
            return err(format!("TestPage control '{ctl}' does not exist on page '{}'", pd.name));
        };
        self.tp_check_open(tp)?;
        let v = match v {
            Val::TestCtl(otp, octl) => self.testctl_method(f, &otp, &octl, "value", &[])?,
            o => o,
        };
        let res = self.tp_set_value(tp, c, v);
        self.tp_note_validation(tp, &c.name, &res);
        res
    }

    /// BC creates a page's subpages with it: each part's OnInit runs after the page's OnInit and
    /// before its OnOpenPage, on the instance CurrPage.<Part>.Page returns.
    fn tp_init_parts(&mut self, tp: &TpRef) -> R<()> {
        let pd = self.pdef(tp);
        let mut parts = vec![];
        collect_parts(&pd.layout, &mut parts);
        let pinst = tp.borrow().inst.clone();
        for c in parts {
            let Some(pid) = c.part_page.as_deref().and_then(|n| self.cat.id_of(ObjectKind::Page, n)) else { continue };
            let Some(ppd) = self.cat.pages.get(&pid) else { continue };
            let mut existing = pinst.parts.borrow().iter().find(|(n, _)| n.eq_ignore_ascii_case(&c.name)).map(|(_, i)| i.clone());
            // opened before (its part TestPage exists): the page's subpages are created again
            let reopened = tp.borrow().parts.iter().any(|(n, _)| n.eq_ignore_ascii_case(&c.name));
            if reopened && existing.as_ref().is_some_and(|i| !i.inited.get()) {
                pinst.parts.borrow_mut().retain(|(n, _)| !n.eq_ignore_ascii_case(&c.name));
                tp.borrow_mut().parts.retain(|(n, _)| !n.eq_ignore_ascii_case(&c.name));
                existing = None;
            }
            let pi = match existing {
                Some(i) => i,
                None => {
                    let i = Instance::new(ppd.code);
                    pinst.parts.borrow_mut().push((c.name.as_str().into(), i.clone()));
                    i
                }
            };
            if pi.inited.replace(true) {
                continue;
            }
            self.instance_globals(&pi);
            if let Some(p) = self.code(pi.code).trigger("oninit") {
                self.invoke(ProcRef { code: pi.code, proc: p }, vec![], Some(pi.clone()), None, None)?;
            }
        }
        Ok(())
    }

    /// BC fills a page's visible subpages when it opens: each visible part in the content area
    /// gets its SubPageLink filters and its first record. A page the code runs keeps them after
    /// its handler closes it, and the code reads them then (Posted Purchase Document Lines:
    /// CurrPage.PostedInvoices.Page.GetSelectedLine after LookupOK). A hidden part (its Visible,
    /// or a group's around it) is filled when it is used (Get Post.Doc - S.ShptLn Sbfrm finds
    /// nothing while hidden; wizard steps).
    fn tp_open_parts(&mut self, tp: &TpRef) -> R<()> {
        let pd = self.pdef(tp);
        let Some(content) = pd.layout.iter().find(|c| c.kind.eq_ignore_ascii_case("area") && c.name.eq_ignore_ascii_case("content")) else { return Ok(()) };
        let mut parts = vec![];
        collect_parts(&content.children, &mut parts);
        'parts: for c in parts {
            if tp.borrow().parts.iter().any(|(n, _)| n.eq_ignore_ascii_case(&c.name)) || !self.tp_app_area_visible(tp, c) {
                continue;
            }
            let mut path = vec![];
            ctl_path(&pd.layout, c.name.as_str(), &mut path);
            for g in path {
                if !self.tp_prop_bool(tp, g, "visible")? {
                    continue 'parts;
                }
            }
            self.tp_part(tp, c)?;
        }
        Ok(())
    }

    /// Focus moves to control `c`: on a new record that is not inserted yet, a field outside the
    /// key inserts it (BC client AutoInsertPattern: a draft row is saved when the active control
    /// leaves the key fields; DelayedInsert pages and read-only pages wait).
    fn tp_activate(&mut self, tp: &TpRef, c: &'static Control) -> R<()> {
        let Some(fi) = c.field else { return Ok(()) };
        {
            let t = tp.borrow();
            if !t.open || !t.new_pending || !t.editable || t.req.is_some() {
                return Ok(());
            }
        }
        let Some(r) = self.tp_rec(tp) else { return Ok(()) };
        if r.borrow().tdef.pk().fields.contains(&fi) {
            return Ok(());
        }
        let pd = self.pdef(tp);
        if pd.props.get("delayedinsert").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return Ok(());
        }
        self.tp_insert(tp)
    }

    /// The caption CaptionClass gives control `c`: the control's CaptionClass evaluated on the
    /// page, else its table field's evaluated on the record, translated by CaptionClassTranslate.
    /// None without a CaptionClass.
    fn tp_caption_class(&mut self, tp: &TpRef, c: &'static Control) -> R<Option<String>> {
        let rec = self.tp_rec(tp);
        let (src, field_rec) = if let Some(s) = c.prop_src.get("captionclass") {
            (s.clone(), None)
        } else if let (Some(fi), Some(r)) = (c.field, &rec) {
            let s = r.borrow().field(fi).prop_src.get("captionclass").cloned();
            match s {
                Some(s) => (s, Some(r.clone())),
                None => return Ok(None),
            }
        } else {
            return Ok(None);
        };
        // a property that is one string literal is stored as its text ('1,2,1')
        let parsed = al_syntax::lex(&src, &Default::default()).ok().and_then(|t| al_syntax::parse_expr(&t).ok());
        let text = match parsed {
            Some(e) => {
                let v = match &field_rec {
                    Some(r) => {
                        let code = r.borrow().tdef.code;
                        let globals = self.record_globals(r, code);
                        let mut f = Frame { code, locals: vec![], globals, rec: Some(r.clone()), xrec: None, with: vec![], inst: None, ret: None, ret_ty: None, curr_field: 0 };
                        self.eval(&mut f, &e)
                    }
                    None => {
                        let code = self.ctl_code(tp, c);
                        self.tp_eval(tp, code, &e)
                    }
                };
                match v {
                    Ok(v) => v.to_text(),
                    Err(_) => src.clone(),
                }
            }
            None => src.clone(),
        };
        Ok(Some(self.caption_class_translate(&text)?))
    }

    /// CaptionClassTranslate: BC raises "UI Helper Triggers".CaptionClassTranslate(Language,
    /// CaptionExpr, var Translation); the System Application resolves '3,<text>' and hands other
    /// areas to OnResolveCaptionClass subscribers. Without a subscriber the expression stays.
    pub(crate) fn caption_class_translate(&mut self, expr: &str) -> R<String> {
        let t = slot(Val::Text(String::new()));
        let args = vec![
            ("Language", Arg::Val(Val::Int(self.global_language))),
            ("CaptionExpr", Arg::Val(Val::Text(expr.to_string()))),
            ("Translation", Arg::Ref(LVal::Slot(t.clone(), None))),
        ];
        self.raise_event(ObjectKind::Codeunit, 2000000004, "CaptionClassTranslate", "", args, None, None)?;
        let out = t.borrow().to_text();
        Ok(if out.is_empty() { expr.to_string() } else { out })
    }

    /// Focus moving into part `tp` inserts its parent page's new record first (BC client
    /// AutoInsertPattern.HostedForm_ActiveControlChanged; not with DelayedInsert, not on a
    /// read-only page); the part then follows the inserted record, and a blank new line takes the
    /// link values again.
    fn tp_parent_autoinsert(&mut self, tp: &TpRef) -> R<()> {
        let parent = tp.borrow().parent.as_ref().and_then(|(p, c)| p.upgrade().map(|p| (p, *c)));
        let Some((ptp, c)) = parent else { return Ok(()) };
        let draft = {
            let t = ptp.borrow();
            t.open && t.new_pending && t.editable && t.req.is_none()
        };
        if !draft || self.tp_rec(&ptp).is_none() {
            return Ok(());
        }
        let pd = self.pdef(&ptp);
        if pd.props.get("delayedinsert").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            return Ok(());
        }
        self.tp_insert(&ptp)?;
        self.tp_part(&ptp, c)?;
        if tp.borrow().new_pending && !self.tp_new_dirty(tp) {
            self.tp_new_record(tp)?;
        }
        Ok(())
    }

    /// CurrPage.Update(true) / Update() (BC NavForm.SaveRecordAsync, called from the Update): the
    /// page's record is written when its normal fields differ from the record the page last read
    /// (its OldRecord): OnModifyRecord, then Modify(true). A new record is left to the page's
    /// insert logic and a changed key (a rename in BC) is not handled. Tests only.
    pub(crate) fn page_update_save(&mut self, inst: &Rc<Instance>) -> R<()> {
        if !self.in_test() {
            return Ok(());
        }
        let Some(r) = inst.rec.borrow().clone() else { return Ok(()) };
        let old = {
            let rb = r.borrow();
            let Some(x) = rb.xrow.clone() else { return Ok(()) };
            let normal = |i: usize| rb.tdef.fields[i].class == FieldClass::Normal && !rb.tdef.fields[i].is_system();
            let changed = rb.row.iter().zip(x.iter()).enumerate().any(|(i, (a, b))| a != b && normal(i));
            let key_changed = rb.tdef.pk().fields.iter().any(|&i| rb.row[i] != x[i]);
            if !changed {
                return Ok(());
            }
            if key_changed {
                drop(rb);
                self.tp_rename_if_key_changed(&r)?;
                return Ok(());
            }
            x
        };
        let xr = copy_record(&r);
        xr.borrow_mut().row = old;
        let go = match self.code(inst.code).trigger("onmodifyrecord") {
            Some(p) => {
                self.ret_true = Self::default_true("onmodifyrecord");
                self.invoke(ProcRef { code: inst.code, proc: p }, vec![], Some(inst.clone()), None, Some(xr))?
            }
            None => Val::Void,
        };
        if !matches!(go, Val::Bool(false)) {
            if let Err(m) = self.rec_modify(&r, true)? {
                return err(m);
            }
        }
        if let Some(row) = self.rec_get_pk(&r) {
            let mut rb = r.borrow_mut();
            let row: crate::record::CowRow = row.into();
            rb.xrow = Some(row.clone());
            rb.row = row;
        }
        Ok(())
    }

    /// A control's validation error stays recorded on it (BC: the client keeps the field's
    /// validation error, NavTestField.CheckError raises it); a successful SetValue clears it.
    fn tp_note_validation(&mut self, tp: &TpRef, ctl: &str, res: &R<()>) {
        let mut t = tp.borrow_mut();
        t.verrors.retain(|(n, _)| !n.eq_ignore_ascii_case(ctl));
        if let Err(Flow::Err(e)) = res {
            if e.code == "TestValidation" {
                let prefix = format!("Validation error for Field: {ctl},  Message = '");
                let inner = e.msg.strip_prefix(prefix.as_str()).and_then(|m| m.strip_suffix('\'')).unwrap_or(&e.msg);
                t.verrors.push((ctl.to_string(), inner.to_string()));
            }
        }
    }

    /// After a page-variable control's trigger: the page's record is saved when it is still the
    /// same record and differs from its copy `before` (BC NavForm SaveRecordAsync,
    /// CompareAllNormalFields). A trigger that moved the page to another record navigated.
    /// CurrPage.Update(false) was called since the flag was last taken (and clears it): BC re-reads
    /// the record without saving it (NavForm.UpdateAsync with saveRecord = false).
    fn tp_take_nosave(&self, tp: &TpRef) -> bool {
        let t = tp.borrow();
        std::iter::once(&t.inst).chain(t.ext.iter()).fold(false, |any, i| i.update_nosave.replace(false) || any)
    }

    fn tp_save_if_changed(&mut self, tp: &TpRef, before: &[bc_store::Value]) -> R<()> {
        // the trigger called CurrPage.Update(false): the page re-reads the record, so a change the
        // trigger did not store itself is dropped, not saved (Manager Time Sheet's CurrTimeSheetNo
        // assigns the key, then updates; Merge Duplicate's Conflicts drill-down stores Rec itself)
        if self.tp_take_nosave(tp) {
            return self.tp_refresh(tp);
        }
        if tp.borrow().new_pending || tp.borrow().req.is_some() {
            return Ok(());
        }
        let Some(r) = self.tp_rec(tp) else { return Ok(()) };
        let (same_record, key_assigned, normal_change) = {
            let rb = r.borrow();
            let pk = &rb.tdef.pk().fields;
            let comparable = rb.row.len() == before.len();
            let same_record = comparable && pk.iter().all(|&i| rb.row[i] == before[i]);
            // key fields changed while the page's xRec still has the old key: the trigger assigned
            // them (a Find or Get would have moved xRec too)
            let key_assigned = comparable && !same_record && rb.xrow.as_ref().is_some_and(|x| pk.iter().all(|&i| x[i] == before[i]));
            let normal_change = comparable && rb.row.iter().zip(before.iter()).enumerate().any(|(i, (a, b))| a != b && rb.tdef.fields[i].class == FieldClass::Normal && !rb.tdef.fields[i].is_system());
            (same_record, key_assigned, normal_change)
        };
        // new key values that no stored record has: saved as a rename by tp_modify (BC: RecordId <>
        // OldRecord.RecordId -> RenameAsync); a key that another record has is that record, not
        // this one's change (Manager Time Sheet's SetTimeSheetNo moves Rec to the sheet's lines)
        let renamed = key_assigned && self.rec_get_pk(&r).is_none();
        let changed = (same_record || renamed) && normal_change;
        if changed {
            self.tp_modify(tp)?;
        }
        Ok(())
    }

    /// CurrPage.SaveRecord() on a pending new line (BC NavForm.SaveRecordAsync): inserted when it
    /// differs from the new line the page started (OnInsertRecord, BelowxRec); an unchanged new
    /// line stays a draft (tp_save would abandon it: General Journal validates Account Type to its
    /// default and calls SaveRecord, then Next() must still insert the line).
    pub(crate) fn page_save_new(&mut self, tp: &TpRef) -> R<()> {
        if self.tp_new_dirty(tp) {
            self.tp_insert(tp)?;
        }
        Ok(())
    }

    fn tp_check_open(&self, tp: &TpRef) -> R<()> {
        if tp.borrow().open {
            Ok(())
        } else {
            // BC Lang.TestPageNotOpened (NavTestPageNotOpenedException): no page name
            err("The TestPage is not open.")
        }
    }

    /// Part control → sub-TestPage filtered by SubPageLink on the parent's current record.
    fn tp_part(&mut self, tp: &TpRef, c: &'static Control) -> R<Option<TpRef>> {
        let Some(pname) = &c.part_page else { return Ok(None) };
        let Some(pid) = self.cat.id_of(ObjectKind::Page, pname) else { return Ok(None) };
        let existing = tp.borrow().parts.iter().find(|(n, _)| n.eq_ignore_ascii_case(&c.name)).map(|(_, p)| p.clone());
        let parent_rec = self.tp_rec(tp);
        let sub = match existing {
            Some(s) => s,
            None => {
                // the part's instance is the one CurrPage.<Part>.Page returns in the parent's code
                let pinst = tp.borrow().inst.clone();
                let existing_inst = pinst.parts.borrow().iter().find(|(n, _)| n.eq_ignore_ascii_case(&c.name)).map(|(_, i)| i.clone());
                let Some(s) = self.new_test_page(pid, existing_inst.clone()) else { return Ok(None) };
                if existing_inst.is_none() {
                    pinst.parts.borrow_mut().push((c.name.as_str().into(), s.borrow().inst.clone()));
                }
                s.borrow_mut().parent = Some((Rc::downgrade(tp), c));
                tp.borrow_mut().parts.push((c.name.clone(), s.clone()));
                if let (Some(pr), Some(sr)) = (&parent_rec, self.tp_rec(&s)) {
                    self.apply_conds(&sr, &c.sub_page_link, Some(pr), 4)?;
                }
                let editable = tp.borrow().editable;
                self.tp_open(&s, if editable { "edit" } else { "view" })?;
                s
            }
        };
        // follow the parent's current record
        if let (Some(pr), Some(sr)) = (parent_rec, self.tp_rec(&sub)) {
            let before = format!("{:?}", sr.borrow().filters);
            self.apply_conds(&sr, &c.sub_page_link, Some(&pr), 4)?;
            let changed = before != format!("{:?}", sr.borrow().filters);
            let pending = sub.borrow().new_pending;
            if changed && !pending {
                if self.tp_find(&sub, "-")? {
                    self.tp_after_get(&sub)?;
                } else if sub.borrow().editable {
                    // an empty editable part is on a new line (with the link values)
                    self.tp_new_record(&sub)?;
                } else {
                    let mut rb = sr.borrow_mut();
                    let fresh = rb.schema.new_row();
                    rb.row = (fresh).into();
                }
            }
        }
        let (ed, open) = {
            let t = tp.borrow();
            (t.editable, t.open)
        };
        sub.borrow_mut().editable = ed;
        sub.borrow_mut().open = open;
        Ok(Some(sub))
    }

    /// `TestPage.Name`: a TestPage method, a part (sub-TestPage) or a control (TestField/TestAction).
    pub fn testpage_member(&mut self, f: &mut Frame, tp: &TpRef, name: &str, args: Option<&[Expr]>) -> R<Val> {
        let l = name.to_ascii_lowercase();
        // TestPart.Visible() / Enabled(): the part control's property on the parent page
        if l == "visible" || l == "enabled" {
            let parent = tp.borrow().parent.as_ref().and_then(|(p, c)| p.upgrade().map(|p| (p, *c)));
            if let Some((ptp, c)) = parent {
                return self.testctl_method(f, &ptp, &c.name, &l, &[]);
            }
        }
        if PAGE_METHODS.contains(&l.as_str()) || (tp.borrow().req.is_some() && REQUEST_PAGE_METHODS.contains(&l.as_str())) {
            return self.testpage_method(f, tp, &l, args.unwrap_or(&[]));
        }
        let pd = self.pdef(tp);
        let Some(c) = find_ctl(&pd.layout, name).or_else(|| find_ctl(&pd.actions, name)) else {
            // TestRequestPage.<DataItem>: the data item's filters
            if let Some(i) = tp.borrow().req.and_then(|r| self.cat.reports[&r].dataitem(name)) {
                return Ok(Val::TestCtl(tp.clone(), format!("{DATAITEM_CTL}{i}").into()));
            }
            return err(format!("TestPage control '{name}' does not exist on page '{}'", pd.name));
        };
        if !self.tp_app_area_visible(tp, c) {
            let what = match (c.field, self.tp_rec(tp)) {
                (Some(fi), Some(r)) => format!("The field with ID = {} is not found on the page.", r.borrow().field(fi).id),
                _ => format!("The control {} is not found on the page.", c.name),
            };
            return err(what);
        }
        if c.kind == "part" || c.kind == "systempart" {
            self.tp_check_open(tp)?;
            if let Some(s) = self.tp_part(tp, c)? {
                return Ok(Val::TestPage(s));
            }
        }
        Ok(Val::TestCtl(tp.clone(), c.name.as_str().into()))
    }

    /// Whether control `c` is on the page in the session's application area: with an area set
    /// (ApplicationArea('#Basic,#Suite')), a control whose ApplicationArea (or the page's default)
    /// names none of the session's areas (or All) is left out of the page, as is one without any.
    fn tp_app_area_visible(&self, tp: &TpRef, c: &Control) -> bool {
        let Some(session) = self.sys.app_area.as_deref() else { return true };
        let set: Vec<String> = session.split(',').map(|s| s.trim().trim_start_matches('#').replace(' ', "").to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
        if set.is_empty() {
            return true;
        }
        let pd = self.pdef(tp);
        let Some(areas) = c.props.get("applicationarea").or_else(|| pd.props.get("applicationarea")) else { return false };
        areas.split(',').map(|a| a.trim().trim_start_matches('#').replace(' ', "").to_ascii_lowercase()).any(|a| a == "all" || set.contains(&a))
    }

    /// The name of the control or action `name` on the TestPage (layout first, then actions).
    pub fn testpage_control(&self, tp: &TpRef, name: &str) -> Option<Rc<str>> {
        let pd = self.pdef(tp);
        find_ctl(&pd.layout, name).or_else(|| find_ctl(&pd.actions, name)).map(|c| c.name.as_str().into())
    }

    fn testpage_method(&mut self, f: &mut Frame, tp: &TpRef, m: &str, args: &[Expr]) -> R<Val> {
        if m.starts_with("find") {
            return self.tp_find_field(f, tp, m, args);
        }
        let mut a = Vec::with_capacity(args.len());
        for x in args {
            a.push(self.eval(f, x)?);
        }
        match m {
            "openedit" | "openview" | "opennew" => {
                if tp.borrow().open {
                    return err("The TestPage is already open.");
                }
                // each open is a new page: BC's NavTestPageBase.Open clears the TestPage (form,
                // filters, controls) and NavTestPage.Open has the client create the page again.
                // A TestPage opened and closed before starts over on a new page instance (globals,
                // Rec, filters, extensions): ERM TestMultipleGenJnlLines reopens the General
                // Journal after OK, and its OnOpenPage must run the template selection again.
                {
                    let mut t = tp.borrow_mut();
                    if t.action.is_some() && t.req.is_none() && t.parent.is_none() {
                        if let Some(pd) = self.cat.pages.get(&t.page) {
                            t.inst = Instance::new(pd.code);
                            t.ext.clear();
                            t.new_row = None;
                        }
                    }
                }
                self.tp_open(tp, &m[4..])?;
                Ok(Val::Void)
            }
            "close" => {
                self.tp_check_open(tp)?;
                let lookup = tp.borrow().lookup;
                self.tp_close(tp, if lookup { "lookupcancel" } else { "ok" })?;
                Ok(Val::Void)
            }
            "ok" | "cancel" | "yes" | "no" | "lookupok" | "lookupcancel" | "edit" | "view" | "preview" | "print" | "schedule" => {
                Ok(Val::TestCtl(tp.clone(), format!("\u{0}{m}").into()))
            }
            // TestRequestPage.SaveAsXml/Pdf/Excel/Word(...): close the request page; the report then
            // runs; SaveAsXml writes its dataset, layout rendering is not supported (crate::report)
            "saveasxml" | "saveaspdf" | "saveasexcel" | "saveasword" if tp.borrow().req.is_some() => {
                self.tp_check_open(tp)?;
                if m == "saveasxml" {
                    // SaveAsXml(ParametersFileName, DataSetFileName): the run writes both files
                    let inst = tp.borrow().inst.clone();
                    let text = |i: usize| a.get(i).map(|v| v.to_text()).unwrap_or_default();
                    self.report_set_test_xml(&inst, text(0), text(1));
                } else if let Some(file) = a.first().map(|v| v.to_text()).filter(|f| !f.is_empty()) {
                    // SaveAsPdf/Word/Excel(FileName): the run writes the rendered output there
                    let inst = tp.borrow().inst.clone();
                    self.report_set_target_file(&inst, file);
                }
                let a: &'static str = match m {
                    "saveasxml" => "saveasxml",
                    "saveaspdf" => "saveaspdf",
                    "saveasexcel" => "saveasexcel",
                    _ => "saveasword",
                };
                self.tp_close(tp, a)?;
                Ok(Val::Void)
            }
            "trap" => {
                // a TestPage whose variable is gone was disposed, and its trap with it (BC
                // NavTestPage.Dispose -> RemoveTrap): only this list still holds it
                self.traps.retain(|t| Rc::strong_count(t) > 1);
                // BC NavTestExecution.AddTrap: once per TestPage
                if !self.traps.iter().any(|t| Rc::ptr_eq(t, tp)) {
                    self.traps.push(tp.clone());
                }
                Ok(Val::Void)
            }
            "filter" => Ok(Val::TestCtl(tp.clone(), FILTER_CTL.into())),
            "caption" => {
                let base = self.pdef(tp).caption.clone();
                let data = self.tp_data_caption(tp)?;
                Ok(Val::Text(if data.trim().is_empty() { base } else { format!("{base} - {data}") }))
            }
            "editable" => Ok(Val::Bool(tp.borrow().editable)),
            "expand" | "activate" => Ok(Val::Void),
            // TestPage.RunPageBackgroundTask(CodeunitId; [Parameters]; [RunCompletionTrigger]) (crate::pbt)
            "runpagebackgroundtask" => {
                let cu = match args.first() {
                    Some(a) => self.eval(f, a)?.as_i64(),
                    None => return err("RunPageBackgroundTask needs a codeunit."),
                };
                let params = match args.get(1) {
                    Some(a) => match self.eval(f, a)? {
                        Val::Dict(d) => d.borrow().iter().map(|(k, v)| (k.to_text(), v.to_text())).collect(),
                        _ => vec![],
                    },
                    None => vec![],
                };
                let completion = match args.get(2) {
                    Some(a) => self.eval(f, a)?.as_bool(),
                    None => false,
                };
                let inst = tp.borrow().inst.clone();
                let rec = inst.rec.borrow().clone();
                self.pbt_test_run(Some(inst), rec, cu, params, completion)
            }
            "isexpanded" => Ok(Val::Bool(true)),
            "validationerrorcount" => Ok(Val::Int(tp.borrow().verrors.len() as i32)),
            "getvalidationerror" => {
                let errs: Vec<String> = tp.borrow().verrors.iter().map(|(_, m)| m.clone()).collect();
                let i = match args.first() {
                    Some(e) => Some(self.eval(f, e)?),
                    None => None,
                };
                validation_error_at(&errs, i.as_ref())
            }
            "first" | "last" => {
                self.tp_check_open(tp)?;
                let blank_new = self.tp_save_new(tp)?;
                let ok = self.tp_find(tp, if m == "first" { "-" } else { "+" })?;
                if ok {
                    self.tp_after_get(tp)?;
                } else {
                    // an empty insertable list: on its new row
                    self.tp_to_new_row(tp, blank_new)?;
                }
                Ok(Val::Bool(ok))
            }
            "next" => {
                self.tp_check_open(tp)?;
                Ok(Val::Bool(self.tp_step(tp, 1)?))
            }
            "previous" | "prev" => {
                self.tp_check_open(tp)?;
                Ok(Val::Bool(self.tp_step(tp, -1)?))
            }
            "new" => {
                self.tp_check_open(tp)?;
                self.tp_save(tp)?;
                tp.borrow_mut().new_below = false;
                self.tp_new_record(tp)?;
                Ok(Val::Void)
            }
            "gotorecord" | "gotokey" => {
                self.tp_check_open(tp)?;
                self.tp_save(tp)?;
                let Some(r) = self.tp_rec(tp) else { return Ok(Val::Bool(false)) };
                let tdef = r.borrow().tdef;
                let keyvals: Vec<Val> = if m == "gotorecord" {
                    match a.first() {
                        Some(Val::Record(src)) => {
                            let s = src.borrow();
                            tdef.pk().fields.iter().map(|&fi| s.tdef.field_no(tdef.fields[fi].id).map(|si| s.get_val(si)).unwrap_or(Val::Void)).collect()
                        }
                        _ => return err("GoToRecord requires a record"),
                    }
                } else {
                    a.clone()
                };
                let saved = r.borrow().row.clone();
                for (k, fi) in tdef.pk().fields.iter().enumerate() {
                    if let Some(v) = keyvals.get(k) {
                        let sv = to_store(&tdef.fields[*fi], v).map_err(|e| Flow::Err(Box::new(AlError::new(e))))?;
                        r.borrow_mut().row[*fi] = sv;
                    }
                }
                let ok = self.tp_find(tp, "=")?;
                if ok {
                    self.tp_after_get(tp)?;
                } else {
                    r.borrow_mut().row = saved;
                }
                Ok(Val::Bool(ok))
            }
            "getfield" => {
                let no = a.first().map(|v| v.as_i64()).unwrap_or(0);
                let pd = self.pdef(tp);
                let r = self.tp_rec(tp);
                let fi = r.as_ref().and_then(|r| r.borrow().tdef.field_no(no));
                match fi.and_then(|fi| find_ctl_by_field(&pd.layout, fi)) {
                    Some(c) => Ok(Val::TestCtl(tp.clone(), c.name.as_str().into())),
                    None => err(format!("TestPage control for field {no} does not exist on page '{}'", pd.name)),
                }
            }
            _ => self.miss_val(format!("TestPage.{m}")),
        }
    }

    /// FindFirstField / FindNextField / FindPreviousField(Control, Value).
    fn tp_find_field(&mut self, f: &mut Frame, tp: &TpRef, m: &str, args: &[Expr]) -> R<Val> {
        self.tp_check_open(tp)?;
        let fname: &str = match args.first() {
            Some(Expr::Ident(n)) | Some(Expr::Member(_, n)) => n,
            _ => return err(format!("{m} requires a control")),
        };
        let want = match args.get(1) {
            Some(e) => self.eval(f, e)?,
            None => Val::Void,
        };
        let pd = self.pdef(tp);
        let Some(r) = self.tp_rec(tp) else { return Ok(Val::Bool(false)) };
        let fi = find_ctl(&pd.layout, fname).and_then(|c| c.field).or_else(|| r.borrow().tdef.field(fname));
        let Some(fi) = fi else { return err(format!("TestPage control '{fname}' does not exist on page '{}'", pd.name)) };
        self.tp_save(tp)?;
        let first = m == "findfirstfield";
        if first && !self.tp_find(tp, "-")? {
            return Ok(Val::Bool(false));
        }
        let step = if m == "findpreviousfield" { -1 } else { 1 };
        let mut check_current = first;
        loop {
            if !check_current && self.next(&r, step)? == 0 {
                return Ok(Val::Bool(false));
            }
            check_current = false;
            let cur = r.borrow().get_val(fi);
            let fd = r.borrow().field(fi);
            if equals(&cur, &want) || self.format_field_val(fd, &cur) == want.to_text() {
                self.tp_after_get(tp)?;
                return Ok(Val::Bool(true));
            }
        }
    }

    fn miss_val(&mut self, what: String) -> R<Val> {
        *self.missing.entry(what.clone()).or_default() += 1;
        err(format!("Not implemented in bc-rust: {what}"))
    }

    /// The code object (page or extension) whose context control `c` evaluates in.
    fn ctl_code(&self, tp: &TpRef, c: &'static Control) -> u32 {
        c.triggers.values().next().map(|pr| pr.code).unwrap_or_else(|| tp.borrow().inst.code)
    }

    /// Value of a field control (typed) and its field definition.
    fn tp_ctl_value(&mut self, tp: &TpRef, c: &'static Control) -> R<(Val, Option<&'static al_model::FieldDef>)> {
        let r = self.tp_rec(tp);
        if let (Some(fi), Some(r)) = (c.field, &r) {
            let fd = r.borrow().field(fi);
            if fd.class == FieldClass::FlowField {
                self.calc_field(r, fi)?;
            }
            let v = r.borrow().get_val(fi);
            return Ok((v, Some(fd)));
        }
        match &c.source_expr {
            Some(e) => {
                let code = self.ctl_code(tp, c);
                // an Option field of a record variable: formatted with the field's metadata, as for Rec
                if let Expr::Member(base, fname) = e {
                    if matches!(&**base, Expr::Ident(_)) {
                        if let Val::Record(h) = self.tp_eval(tp, code, base)? {
                            let fi = h.borrow().tdef.field(fname);
                            // Option fields: their captions (an Enum value carries its own)
                            if let Some(fi) = fi.filter(|&fi| matches!(h.borrow().field(fi).ty, FieldType::Option)) {
                                let b = h.borrow();
                                return Ok((b.get_val(fi), Some(b.field(fi))));
                            }
                        }
                    }
                }
                Ok((self.tp_eval(tp, code, e)?, None))
            }
            None => Ok((Val::Text(String::new()), None)),
        }
    }

    fn tp_format(&self, v: &Val, fd: Option<&'static al_model::FieldDef>) -> String {
        match (v, fd) {
            (Val::Opt { .. }, Some(fd)) => self.format_field_val(fd, v),
            (Val::Opt { .. }, None) => crate::builtins::format_val(v, 0),
            (Val::Int(0), Some(fd)) if fd.blank_zero => String::new(),
            (Val::Dec(d), Some(fd)) if fd.blank_zero && d.is_zero() => String::new(),
            _ => v.to_text(),
        }
    }

    fn tp_prop_bool(&mut self, tp: &TpRef, c: &'static Control, prop: &str) -> R<bool> {
        let Some(src) = c.prop_src.get(prop).or_else(|| c.props.get(prop)) else { return Ok(true) };
        match src.trim().to_ascii_lowercase().as_str() {
            "true" => return Ok(true),
            "false" => return Ok(false),
            _ => {}
        }
        let Ok(toks) = al_syntax::lex(src, &Default::default()) else { return Ok(true) };
        let Ok(e) = al_syntax::parse_expr(&toks) else { return Ok(true) };
        let code = self.ctl_code(tp, c);
        Ok(self.tp_eval(tp, code, &e)?.as_bool())
    }

    fn tp_run_trigger(&mut self, tp: &TpRef, pr: ProcRef, args: Vec<Arg>) -> R<Val> {
        let inst = self.tp_inst_for(tp, pr.code);
        self.invoke(pr, args, Some(inst), None, None)
    }

    /// Methods of a TestField / TestAction / TestFilter.
    pub fn testctl_method(&mut self, f: &mut Frame, tp: &TpRef, ctl: &str, name: &str, args: &[Expr]) -> R<Val> {
        let m = name.to_ascii_lowercase();
        // these move the focus to the control; on a part that inserts the parent's new record
        if matches!(m.as_str(), "setvalue" | "activate" | "invoke" | "drilldown" | "lookup" | "assistedit") {
            self.tp_parent_autoinsert(tp)?;
        }
        if ctl == FILTER_CTL {
            let Some(r) = self.tp_rec(tp) else { return err("The page has no source table") };
            return self.tp_filter_method(f, tp, &r, &m, args);
        }
        if let Some(i) = ctl.strip_prefix(DATAITEM_CTL).and_then(|i| i.parse::<usize>().ok()) {
            let inst = tp.borrow().inst.clone();
            let rd = tp.borrow().req.and_then(|r| self.cat.reports.get(&r));
            let Some(r) = rd.and_then(|rd| self.rep_item_rec(&inst, rd, i)) else { return err("The data item has no record") };
            return self.tp_filter_method(f, tp, &r, &m, args);
        }
        let mut a = Vec::with_capacity(args.len());
        for x in args {
            a.push(self.eval(f, x)?);
        }
        if let Some(action) = ctl.strip_prefix('\u{0}') {
            return match m.as_str() {
                "invoke" => {
                    self.tp_check_open(tp)?;
                    match action {
                        "edit" | "view" => {
                            // on a list with CardPageID, Edit/View open the card on the current record
                            let pd = self.pdef(tp);
                            let card = pd.card_page.as_deref().and_then(|c| self.cat.id_of(ObjectKind::Page, c)).filter(|_| tp.borrow().req.is_none());
                            match card {
                                Some(cid) => {
                                    self.tp_save(tp)?;
                                    let rec = self.tp_rec(tp).map(|r| copy_record(&r));
                                    if !self.in_test() || self.traps.iter().any(|t| t.borrow().page == cid) || self.has_handler(crate::testing::UiKind::Page, Some(cid)) {
                                        self.ui_run_page_mode(cid, None, rec, false, action)?;
                                    }
                                }
                                None => tp.borrow_mut().editable = action == "edit",
                            }
                            Ok(Val::Void)
                        }
                        _ => {
                            let lookup = tp.borrow().lookup;
                            let a: &'static str = match (action, lookup) {
                                ("ok", true) | ("lookupok", _) => "lookupok",
                                ("cancel", true) | ("lookupcancel", _) => "lookupcancel",
                                ("ok", false) => "ok",
                                ("cancel", false) => "cancel",
                                ("yes", _) => "yes",
                                ("preview", _) => "preview",
                                ("print", _) => "print",
                                ("schedule", _) => "schedule",
                                _ => "no",
                            };
                            self.tp_close(tp, a)?;
                            Ok(Val::Void)
                        }
                    }
                }
                "visible" | "enabled" => Ok(Val::Bool(true)),
                "caption" => Ok(Val::Text(action.to_uppercase())),
                _ => self.miss_val(format!("TestAction.{name}")),
            };
        }
        let pd = self.pdef(tp);
        let Some(c) = find_ctl(&pd.layout, ctl).or_else(|| find_ctl(&pd.actions, ctl)) else {
            return err(format!("TestPage control '{ctl}' does not exist on page '{}'", pd.name));
        };
        let is_action = matches!(c.kind.as_str(), "action" | "systemaction" | "fileuploadaction" | "customaction");
        match m.as_str() {
            "invoke" if is_action => {
                self.tp_check_open(tp)?;
                // a disabled action does nothing (BC: Framework.UI ActionControl.Invoke returns when
                // not Enabled; TestActionProxy raises no error)
                if !self.tp_prop_bool(tp, c, "enabled")? {
                    return Ok(Val::Void);
                }
                // a pending new row is inserted or abandoned by tp_save; only a record the page was
                // on is written (an abandoned draft whose key a stored record has would overwrite it)
                let was_new = tp.borrow().new_pending;
                self.tp_save(tp)?;
                if !was_new && self.tp_rec(tp).is_some() {
                    self.tp_modify(tp)?;
                }
                self.tp_take_update(tp);
                let ran = if let Some(pr) = c.triggers.get("onaction").copied() {
                    self.tp_run_trigger(tp, pr, vec![]).map(|_| ())
                } else if let Some(obj) = c.prop_src.get("runobject").or_else(|| c.props.get("runobject")) {
                    self.tp_run_object(tp, c, obj)
                } else {
                    Ok(())
                };
                match ran {
                    // Error('') in an action (a posting preview always ends with one) undoes the
                    // write transaction but shows nothing: Invoke returns normally (Microsoft's
                    // tests call Preview.Invoke() outside asserterror)
                    Err(Flow::Err(e)) if e.msg.is_empty() => {
                        self.note_error(&e);
                        self.rollback_to_commit();
                        return Ok(Val::Void);
                    }
                    r => r?,
                }
                // CurrPage.Close in the action: the page closes as the client closes it (journal
                // view switches close the page and run it again: ERM General Journal UT
                // ValidateBlankDocumentTypeOnPurchaselJournalPage reopens it)
                let close = {
                    let t = tp.borrow();
                    std::iter::once(&t.inst).chain(t.ext.iter()).fold(false, |any, i| i.close.replace(false) || any)
                };
                if close && tp.borrow().open {
                    let lookup = tp.borrow().lookup;
                    self.tp_close(tp, if lookup { "lookupcancel" } else { "ok" })?;
                    return Ok(Val::Void);
                }
                // the page refreshes its current record (CurrPage.Update: following changed filters)
                if self.tp_take_update(tp) && tp.borrow().open {
                    self.tp_refresh(tp)?;
                } else if let Some(r) = self.tp_rec(tp) {
                    if !tp.borrow().new_pending && tp.borrow().open {
                        if let Some(row) = self.rec_get_pk(&r) {
                            r.borrow_mut().row = (row).into();
                            self.tp_after_get(tp)?;
                        }
                    }
                }
                Ok(Val::Void)
            }
            // Visible dominates: a group that is not visible hides its controls and actions
            "visible" => {
                let pd = self.pdef(tp);
                let mut path = vec![];
                if !ctl_path(&pd.layout, c.name.as_str(), &mut path) {
                    path.clear();
                    ctl_path(&pd.actions, c.name.as_str(), &mut path);
                }
                for g in path {
                    if !self.tp_prop_bool(tp, g, "visible")? {
                        return Ok(Val::Bool(false));
                    }
                }
                Ok(Val::Bool(true))
            }
            "enabled" => Ok(Val::Bool(self.tp_prop_bool(tp, c, "enabled")?)),
            "editable" => {
                let page_ed = tp.borrow().editable;
                let field_ed = match (c.field, self.tp_rec(tp)) {
                    (Some(fi), Some(r)) => {
                        let fd = r.borrow().field(fi);
                        fd.editable && fd.class != FieldClass::FlowField
                    }
                    _ => true,
                };
                Ok(Val::Bool(page_ed && field_ed && !is_action && self.tp_prop_bool(tp, c, "editable")?))
            }
            "caption" => {
                // CaptionClass (the control's, else the table field's) decides the caption
                if let Some(cap) = self.tp_caption_class(tp, c)? {
                    return Ok(Val::Text(cap));
                }
                let r = self.tp_rec(tp);
                Ok(Val::Text(match (&c.caption, c.field, r) {
                    (Some(cap), _, _) => cap.clone(),
                    (None, Some(fi), Some(r)) => r.borrow().field(fi).caption.clone(),
                    _ => c.name.clone(),
                }))
            }
            "hidevalue" => Ok(Val::Bool(false)),
            // the control's recorded validation errors (Ncl NavTestField.ALGetValidationError)
            "validationerrorcount" => Ok(Val::Int(tp.borrow().verrors.iter().filter(|(n, _)| n.eq_ignore_ascii_case(&c.name)).count() as i32)),
            "getvalidationerror" => {
                let errs: Vec<String> = tp.borrow().verrors.iter().filter(|(n, _)| n.eq_ignore_ascii_case(&c.name)).map(|(_, m)| m.clone()).collect();
                validation_error_at(&errs, a.first())
            }
            "activate" => {
                self.tp_activate(tp, c)?;
                Ok(Val::Void)
            }
            "value" if a.is_empty() => {
                self.tp_check_open(tp)?;
                let (v, fd) = self.tp_ctl_value(tp, c)?;
                // BlankZero (on the control or the field): the client shows a zero as blank, and
                // TestField.Value returns what it shows (Config. Worksheet "Table ID" on the new row)
                let blank_zero = c.props.get("blankzero").is_some_and(|x| x.eq_ignore_ascii_case("true")) || fd.is_some_and(|f| f.blank_zero);
                if blank_zero && matches!(&v, Val::Int(0) | Val::BigInt(0)) || blank_zero && matches!(&v, Val::Dec(d) if d.is_zero()) {
                    return Ok(Val::Text(String::new()));
                }
                // an Option variable shown with the control's own OptionCaption
                if let (None, Val::Opt { ord, .. }, Some(caps)) = (fd, &v, ctl_option_captions(c)) {
                    if let Some(x) = usize::try_from(*ord).ok().and_then(|i| caps.get(i)) {
                        return Ok(Val::Text(x.clone()));
                    }
                }
                Ok(Val::Text(self.tp_format(&v, fd)))
            }
            "valuevariant" => Ok(self.tp_ctl_value(tp, c)?.0),
            // GetOption(Index): the caption of the control's Index-th option (1-based position)
            "getoption" | "optioncount" => {
                let (v, fd) = self.tp_ctl_value(tp, c)?;
                let caps: Vec<String> = match (fd.map(|fd| &fd.ty), &v) {
                    (None, Val::Opt { .. }) if ctl_option_captions(c).is_some() => ctl_option_captions(c).unwrap(),
                    (Some(FieldType::Option), _) => {
                        let fd = fd.unwrap();
                        fd.options.iter().enumerate().map(|(i, o)| fd.option_captions.get(i).filter(|c| !c.is_empty()).cloned().unwrap_or_else(|| o.clone())).collect()
                    }
                    (Some(FieldType::Enum(en)), _) => self.cat.enum_by_name(en).map(|e| e.values.iter().map(|x| x.caption.clone()).collect()).unwrap_or_default(),
                    (_, Val::Opt { enum_id: Some(e), .. }) if *e >= 0 => self.cat.enums.get(e).map(|e| e.values.iter().map(|x| x.caption.clone()).collect()).unwrap_or_default(),
                    (_, Val::Opt { enum_id: Some(e), .. }) => crate::optmeta::get(*e)
                        .map(|m| m.names.iter().enumerate().map(|(i, n)| m.captions.get(i).cloned().unwrap_or_else(|| n.clone())).collect())
                        .unwrap_or_default(),
                    _ => vec![],
                };
                if m == "optioncount" {
                    return Ok(Val::Int(caps.len() as i32));
                }
                let i = a.first().map(|v| v.as_i64()).unwrap_or(0);
                match usize::try_from(i - 1).ok().and_then(|i| caps.get(i)) {
                    Some(c) => Ok(Val::Text(c.clone())),
                    None => err(format!("The option index {i} is outside the options of {}.", c.name)),
                }
            }
            "asdecimal" | "asinteger" | "asboolean" | "asdate" | "astime" | "asdatetime" => {
                self.tp_check_open(tp)?;
                let (v, _) = self.tp_ctl_value(tp, c)?;
                Ok(match v {
                    Val::Text(s) => {
                        let t = s.trim();
                        match m.as_str() {
                            "asdecimal" => Val::Dec(parse_num(t).unwrap_or_default()),
                            "asinteger" => Val::Int(parse_num(t).and_then(|d| rust_decimal::prelude::ToPrimitive::to_i32(&d)).unwrap_or(0)),
                            "asboolean" => Val::Bool(matches!(t.to_ascii_lowercase().as_str(), "yes" | "true" | "1")),
                            "asdate" => Val::Date(parse_date_text(t, self.work_date).unwrap_or(0)),
                            "astime" => Val::Time(parse_time_text(t).unwrap_or(0)),
                            _ => Val::DateTime(parse_datetime_text(t).unwrap_or(bc_store::DATETIME_UNDEFINED)),
                        }
                    }
                    v => match m.as_str() {
                        "asdecimal" => Val::Dec(v.as_dec()),
                        "asinteger" => Val::Int(v.as_i64() as i32),
                        "asboolean" => Val::Bool(v.as_bool()),
                        _ => v,
                    },
                })
            }
            "setvalue" | "value" => {
                self.tp_check_open(tp)?;
                let v = a.into_iter().next().unwrap_or(Val::Void);
                // SetValue(OtherPage.Field): the other control's value, as its text
                let v = match v {
                    Val::TestCtl(otp, octl) => self.testctl_method(f, &otp, &octl, "value", &[])?,
                    // an Option value on a control that does not hold an Option (a text field) is
                    // entered as its formatted text, its member caption; an Option control takes it as is
                    o @ Val::Opt { enum_id: Some(_), .. } if !matches!(self.tp_ctl_value(tp, c).map(|(cur, _)| cur), Ok(Val::Opt { .. })) => {
                        Val::Text(crate::builtins::format_val(&o, 0))
                    }
                    o => o,
                };
                let res = self.tp_set_value(tp, c, v);
                self.tp_note_validation(tp, &c.name, &res);
                res?;
                Ok(Val::Void)
            }
            "assertequals" => {
                self.tp_check_open(tp)?;
                let want = a.into_iter().next().unwrap_or(Val::Void);
                // AssertEquals(OtherPage.Field): the other control's value, as its text
                let want = match want {
                    Val::TestCtl(otp, octl) => self.testctl_method(f, &otp, &octl, "value", &[])?,
                    o => o,
                };
                let (cur, fd) = self.tp_ctl_value(tp, c)?;
                // BlankZero (control or field): a zero shows as blank, as in TestField.Value
                let blank_zero = c.props.get("blankzero").is_some_and(|x| x.eq_ignore_ascii_case("true")) || fd.is_some_and(|f| f.blank_zero);
                let shown = if blank_zero && (matches!(&cur, Val::Int(0) | Val::BigInt(0)) || matches!(&cur, Val::Dec(d) if d.is_zero())) {
                    String::new()
                } else {
                    self.tp_format(&cur, fd)
                };
                let same = match (&want, &cur) {
                    (Val::Text(w), _) => *w == shown || (matches!(fd.map(|x| &x.ty), Some(FieldType::Code(_))) && w.to_uppercase() == shown),
                    (w, c) if w.is_numeric() => w.as_dec() == c.as_dec() || Some(w.as_dec()) == parse_num(shown.trim()),
                    (w, c) => equals(w, c) || self.tp_format(w, fd) == shown,
                };
                if !same {
                    let cap = match (&c.caption, fd) {
                        (Some(cap), _) => cap.clone(),
                        (None, Some(fd)) => fd.caption.clone(),
                        _ => c.name.clone(),
                    };
                    let w = self.tp_format(&want, fd);
                    return err(format!("AssertEquals for Field: {cap} Expected = '{w}', Actual = '{shown}'"));
                }
                Ok(Val::Void)
            }
            "lookup" | "drilldown" | "assistedit" => {
                self.tp_check_open(tp)?;
                let trig = match m.as_str() {
                    "lookup" => "onlookup",
                    "drilldown" => "ondrilldown",
                    _ => "onassistedit",
                };
                if let Some(pr) = c.triggers.get(trig).copied() {
                    // a page variable's trigger that changes the record (G/L Account Card's
                    // subcategory lookup): saved like after its OnValidate
                    let before = if c.field.is_none() { self.tp_rec(tp).map(|r| r.borrow().row.to_vec()) } else { None };
                    self.tp_take_nosave(tp);
                    if self.code(pr.code).procs[pr.proc as usize].params.is_empty() {
                        self.tp_run_trigger(tp, pr, vec![])?;
                        if let Some(b) = &before {
                            self.tp_save_if_changed(tp, b)?;
                        }
                    } else {
                        // OnLookup(var Text: Text): Boolean
                        let text = slot(Val::Text(String::new()));
                        let ok = self.tp_run_trigger(tp, pr, vec![Arg::Ref(LVal::Slot(text.clone(), None))])?;
                        if ok.as_bool() {
                            let t = text.borrow().clone();
                            self.tp_set_value(tp, c, t)?;
                        } else if let Some(b) = &before {
                            self.tp_save_if_changed(tp, b)?;
                        }
                    }
                    // CurrPage.Close in the trigger: the page closes as after an action's
                    let close = {
                        let t = tp.borrow();
                        std::iter::once(&t.inst).chain(t.ext.iter()).fold(false, |any, i| i.close.replace(false) || any)
                    };
                    if close && tp.borrow().open {
                        let lookup = tp.borrow().lookup;
                        self.tp_close(tp, if lookup { "lookupcancel" } else { "ok" })?;
                    }
                    // the field operation ends by checking that the page is still open (a
                    // drilldown that closed its page raises "The TestPage is not open.")
                    self.tp_check_open(tp)?;
                    return Ok(Val::Void);
                }
                let (Some(fi), Some(r)) = (c.field, self.tp_rec(tp)) else { return Ok(Val::Void) };
                let fd = r.borrow().field(fi);
                if m == "lookup" {
                    if let Some(pr) = fd.on_lookup {
                        self.invoke(pr, vec![], None, Some(r.clone()), None)?;
                        return Ok(Val::Void);
                    }
                    self.tp_relation_lookup(tp, c, &r, fi)?;
                } else if m == "drilldown" && fd.class == FieldClass::FlowField {
                    self.tp_flowfield_drilldown(&r, fi)?;
                }
                Ok(Val::Void)
            }
            _ => self.miss_val(format!("TestField.{name}")),
        }
    }

    /// Text typed into a DateFormula control: user input, read like Evaluate in the session
    /// language (R45, Q13); a refused text is wrapped as the page does (LT 72879
    /// DateFormulaControl_Spellings, LT 68665 DateFormula_RefusedAndLeadingTokenSpellings).
    fn tp_date_formula_input(&self, text: &str, ctl: &str) -> R<Val> {
        let lang = self.global_language;
        match crate::datefml::evaluate(text, lang, None) {
            Ok(stored) => Ok(Val::DateFormula(stored)),
            Err(e) => match e.message(lang) {
                Ok(m) => Err(validation_text(
                    Flow::Err(Box::new(AlError { msg: format!("Your entry of '{text}' is not an acceptable value for '{ctl}'. {m}"), stack: vec![], code: "TestValidation" })),
                    ctl,
                )),
                Err(p) => crate::builtins::df_unsettled(p),
            },
        }
    }

    /// SetValue on a field control: validate the field (or assign the page variable), run the
    /// control's OnValidate, then save like the client does after leaving the field.
    fn tp_set_value(&mut self, tp: &TpRef, c: &'static Control, v: Val) -> R<()> {
        // neither view mode, a page's Editable = false nor a control's Editable stops SetValue:
        // BC's TestFieldProxy Value setter (Activate; Control.StringValue = value) has no Editable
        // check (WF Demo Cust Cred Lmt Approval sets Credit Limit after OpenView; Service Order
        // Statistics' Editable = false "Total Incl. VAT_General"; a TestRequestPage's
        // SellToCustNo / ColumnDimensions)
        let r = self.tp_rec(tp);
        // the record's version before this change: a validation that stores the record itself
        // (Rec.Modify in OnValidate) moves it
        let version_before = r.as_ref().and_then(|r| r.borrow().xrow.as_ref().map(|x| x.to_vec()));
        // the control's OnValidate sees xRec = the record before this change
        let xrec = r.as_ref().map(copy_record);
        let before = r.as_ref().map(|r| r.borrow().row.to_vec());
        if let (Some(fi), Some(r)) = (c.field, &r) {
            let fd = r.borrow().field(fi);
            let v = match v {
                Val::Text(s) if matches!(fd.ty, FieldType::DateFormula) => self.tp_date_formula_input(&s, &c.name)?,
                Val::Text(s) if !matches!(fd.ty, FieldType::Code(_) | FieldType::Text(_) | FieldType::Other(_)) => {
                    let ev = |n: &str| self.enum_ordinal_for_field(fd, n);
                    match text_to_store(fd, &s, &ev, self.work_date) {
                        Some(sv) => to_val(fd, &sv),
                        None => return err_code("TestValidation", format!("The value \"{s}\" can't be evaluated into the type of {}.", fd.caption)),
                    }
                }
                // a Date on a DateTime control: BC evaluates its text as a DateTime, the date at
                // 00:00 (NavDateTimeEvaluator: no time part = NavTime.Default); 0D stays undefined
                Val::Date(d) if matches!(fd.ty, FieldType::DateTime) => Val::DateTime(if d == 0 { bc_store::DATETIME_UNDEFINED } else { date_time_to_ms(d, 0) }),
                o => o,
            };
            // a field changed through the page: CurrFieldNo is set while it validates
            self.ui_validate(r, fi, Some(v)).map_err(wrap_validation).map_err(|e| validation_text(e, &c.name))?;
        } else if let Some(Expr::Ident(var)) = &c.source_expr {
            let inst = self.tp_inst_for(tp, self.ctl_code(tp, c));
            let g = self.instance_globals(&inst);
            if let Some(gv) = g.vars.iter().find(|x| x.name.eq_ignore_ascii_case(var)).cloned() {
                let cur = gv.slot.borrow().clone();
                // an Option variable whose control has its own OptionCaption takes those captions
                let v = match (v, &cur, ctl_option_captions(c)) {
                    (Val::Text(s), Val::Opt { enum_id, .. }, Some(caps)) if caps.iter().any(|x| x.trim().eq_ignore_ascii_case(s.trim())) => {
                        let i = caps.iter().position(|x| x.trim().eq_ignore_ascii_case(s.trim())).unwrap();
                        Val::Opt { ord: i as i32, enum_id: *enum_id }
                    }
                    (v, _, _) => v,
                };
                let v = match v {
                    Val::Text(s) if matches!(cur, Val::DateFormula(_)) => self.tp_date_formula_input(&s, &c.name)?,
                    Val::Text(s) if !matches!(cur, Val::Text(_)) => match self.text_to_var(&s, &cur, gv.ty) {
                        Some(x) => x,
                        None => return err_code("TestValidation", format!("The value \"{s}\" can't be evaluated into the type of {}.", c.name)),
                    },
                    o => o,
                };
                self.write_lval(&LVal::Slot(gv.slot.clone(), gv.ty), v).map_err(wrap_validation).map_err(|e| validation_text(e, &c.name))?;
            }
        } else if let Some(e @ (Expr::Member(..) | Expr::Index(..))) = &c.source_expr {
            // a field of a global record (BankAccount."No."): the page assigns it, without the
            // table field's OnValidate (only Rec fields are bound to the table); an array element
            // (RoundingMethod[1]) is assigned the same way
            let inst = self.tp_inst_for(tp, self.ctl_code(tp, c));
            let globals = self.instance_globals(&inst);
            let rec = inst.rec.borrow().clone();
            let mut f = Frame { code: inst.code, locals: vec![], globals, rec, xrec: None, with: vec![], inst: Some(inst), ret: None, ret_ty: None, curr_field: 0 };
            let lv = self.lvalue(&mut f, e)?;
            let cur = self.read_lval(&lv);
            let fd = match &lv {
                LVal::Field(r, i) => Some(r.borrow().field(*i)),
                _ => None,
            };
            let v = match (v, fd) {
                (Val::Text(s), Some(fd)) if matches!(fd.ty, FieldType::DateFormula) => self.tp_date_formula_input(&s, &c.name)?,
                (Val::Text(s), None) if matches!(cur, Val::DateFormula(_)) => self.tp_date_formula_input(&s, &c.name)?,
                (Val::Text(s), Some(fd)) if !matches!(fd.ty, FieldType::Code(_) | FieldType::Text(_) | FieldType::Other(_)) => {
                    let ev = |n: &str| self.enum_ordinal_for_field(fd, n);
                    match text_to_store(fd, &s, &ev, self.work_date) {
                        Some(sv) => to_val(fd, &sv),
                        None => return err_code("TestValidation", format!("The value \"{s}\" can't be evaluated into the type of {}.", c.name)),
                    }
                }
                (v, _) => v,
            };
            let v = match v {
                Val::Text(s) if !matches!(cur, Val::Text(_)) => match self.text_to_var(&s, &cur, None) {
                    Some(x) => x,
                    None => return err_code("TestValidation", format!("The value \"{s}\" can't be evaluated into the type of {}.", c.name)),
                },
                o => o,
            };
            self.write_lval(&lv, v).map_err(wrap_validation).map_err(|e| validation_text(e, &c.name))?;
        }
        self.tp_take_update(tp);
        self.tp_take_nosave(tp);
        if let Some(pr) = c.triggers.get("onvalidate").copied() {
            let inst = self.tp_inst_for(tp, pr.code);
            self.invoke(pr, vec![], Some(inst), None, xrec).map_err(wrap_validation).map_err(|e| validation_text(e, &c.name))?;
        }
        // a page variable's OnValidate that calls CurrPage.Update refreshes the page (field
        // controls re-read their record when it is saved below). Seeded A/B (P10): +21 subform
        // invoice-discount tests, -7 "ChangingSellTo...SetsDiscountToZero" (open gap).
        let update = self.tp_take_update(tp);
        // a page variable's OnValidate that changed the stored record (Rec.Validate(...)): saved
        // like a field change
        if c.field.is_none() {
            if let Some(before) = &before {
                self.tp_save_if_changed(tp, before)?;
            }
        }
        if c.field.is_none() && update {
            self.tp_refresh(tp)?;
        }
        if c.field.is_some() {
            let pd = self.pdef(tp);
            let delayed = pd.props.get("delayedinsert").is_some_and(|v| v.eq_ignore_ascii_case("true"));
            if tp.borrow().new_pending {
                if !delayed {
                    self.tp_insert(tp)?;
                }
            } else {
                // the validation stored the record itself (Rec.Modify): saved and re-read as before,
                // so the page shows what the code wrote through other variables too (Payment
                // Reconciliation Journal's Account No. applies the line and sets its Applied Amount)
                let written = r.as_ref().is_some_and(|r| r.borrow().xrow.as_ref().map(|x| x.to_vec()) != version_before);
                self.tp_modify_keep(tp, !written)?;
            }
        }
        // CurrPage.Update refreshes the subpages too: each open part re-reads its current record
        // (a header's vendor change recreated the lines) and runs its triggers
        if update {
            let parts: Vec<TpRef> = tp.borrow().parts.iter().map(|(_, p)| p.clone()).collect();
            for p in parts {
                self.tp_refresh(&p)?;
            }
        }
        Ok(())
    }

    fn tp_filter_method(&mut self, f: &mut Frame, tp: &TpRef, r: &RecHandle, m: &str, args: &[Expr]) -> R<Val> {
        let r = r.clone();
        let is_page_rec = self.tp_rec(tp).is_some_and(|pr| Rc::ptr_eq(&pr, &r));
        let tdef = r.borrow().tdef;
        let field_of = |e: &Expr| -> R<usize> {
            let n: &str = match e {
                Expr::Ident(n) | Expr::Member(_, n) => n,
                _ => return err("Expected a field reference"),
            };
            match tdef.field(n) {
                Some(i) => Ok(i),
                None => err(format!("Field '{n}' does not exist in table '{}'", tdef.name)),
            }
        };
        match m {
            "setfilter" => {
                let fi = field_of(&args[0])?;
                let text = self.eval(f, &args[1])?.to_text();
                let fd = &tdef.fields[fi];
                let node = if text.is_empty() { None } else { Some(self.parse_field_filter(fd, &text)?) };
                // the client's filter: filter group 0, whatever group the page's code left active
                let mut rb = r.borrow_mut();
                let g = std::mem::replace(&mut rb.filter_group, 0);
                rb.set_filter(fi, node, text);
                rb.filter_group = g;
                drop(rb);
                if is_page_rec {
                    self.tp_save(tp)?;
                    if self.tp_find(tp, "-")? {
                        self.tp_after_get(tp)?;
                    } else if self.tp_has_new_row(tp) {
                        // nothing in the view: an insertable list is on its new row
                        self.tp_to_new_row(tp, false)?;
                    } else {
                        // otherwise the page has no current record
                        let mut rb = r.borrow_mut();
                        let fresh = rb.schema.new_row();
                        rb.row = (fresh).into();
                        rb.xrow = None;
                    }
                }
                Ok(Val::Void)
            }
            "getfilter" => {
                let fi = field_of(&args[0])?;
                let rb = r.borrow();
                Ok(Val::Text(rb.filters.iter().filter(|x| x.field == fi && x.group == 0).map(|x| x.text.clone()).collect::<Vec<_>>().join("&")))
            }
            "getfilters" => {
                let rb = r.borrow();
                Ok(Val::Text(self.filters_text(&rb)))
            }
            "ascending" => {
                let a = self.eval(f, &args[0])?.as_bool();
                r.borrow_mut().ascending = a;
                if is_page_rec && self.tp_find(tp, "-")? {
                    self.tp_after_get(tp)?;
                }
                Ok(Val::Void)
            }
            "setcurrentkey" => {
                let mut fields = vec![];
                for e in args {
                    fields.push(field_of(e)?);
                }
                {
                    let mut rb = r.borrow_mut();
                    if let Some(k) = tdef.keys.iter().position(|k| k.fields.starts_with(&fields)) {
                        let kf = tdef.keys[k].fields.clone();
                        rb.key = rb.schema.keys.iter().position(|sk| *sk == kf).unwrap_or(0);
                    }
                }
                if is_page_rec && self.tp_find(tp, "-")? {
                    self.tp_after_get(tp)?;
                }
                Ok(Val::Void)
            }
            _ => self.miss_val(format!("TestFilter.{m}")),
        }
    }

    /// The page's data caption (shown after its caption): DataCaptionExpression, else the
    /// DataCaptionFields of the current record or of single-value filters; a related field shows
    /// the related record's own caption fields.
    fn tp_data_caption(&mut self, tp: &TpRef) -> R<String> {
        let pd = self.pdef(tp);
        if let Some(src) = pd.prop_src.get("datacaptionexpression") {
            let Ok(toks) = al_syntax::lex(src, &Default::default()) else { return Ok(String::new()) };
            let Ok(e) = al_syntax::parse_expr(&toks) else { return Ok(String::new()) };
            return Ok(self.tp_eval(tp, pd.code, &e)?.to_text());
        }
        let Some(r) = self.tp_rec(tp) else { return Ok(String::new()) };
        let tdef = r.borrow().tdef;
        let names: Vec<String> = match pd.prop_src.get("datacaptionfields") {
            Some(s) => s.split(',').map(|n| n.trim().trim_matches('"').to_string()).collect(),
            None => tdef.data_caption_fields.clone(),
        };
        let on_record = !tp.borrow().new_pending && r.borrow().xrow.is_some();
        let mut parts: Vec<String> = vec![];
        for n in &names {
            let Some(fi) = tdef.field(n) else { continue };
            let fd = &tdef.fields[fi];
            let filtered = r.borrow().filters.iter().find_map(|f| if f.field == fi { f.node.eq_value().cloned() } else { None });
            let val = match filtered {
                Some(sv) if !on_record => store_to_val(&sv),
                _ => r.borrow().get_val(fi),
            };
            let text = crate::builtins::format_val(&val, 0);
            if text.trim().is_empty() {
                continue;
            }
            // a field related to another table: that record's caption fields
            let rel = fd.relation.first().filter(|b| b.when.is_empty()).and_then(|b| self.cat.table_by_name(&b.table).map(|rt| (b, rt)));
            if let Some((b, rt)) = rel.filter(|(_, rt)| !rt.data_caption_fields.is_empty()) {
                let kf = b.field.as_ref().and_then(|x| rt.field(x)).or(rt.pk().fields.last().copied());
                if let Some(kf) = kf {
                    if let Ok(kv) = to_store(&rt.fields[kf], &val) {
                        let probe = self.new_record(rt.id, false);
                        probe.borrow_mut().set_filter(kf, Some(bc_store::filter::FNode::Eq(kv)), text.clone());
                        if self.find_first(&probe, false)? {
                            for cn in &rt.data_caption_fields {
                                if let Some(ci) = rt.field(cn) {
                                    let t = crate::builtins::format_val(&probe.borrow().get_val(ci), 0);
                                    if !t.trim().is_empty() {
                                        parts.push(t);
                                    }
                                }
                            }
                            continue;
                        }
                    }
                }
            }
            parts.push(text);
        }
        Ok(parts.join(" \u{b7} "))
    }

    /// Lookup on a field with a TableRelation: open the related table's lookup page modally and
    /// take the selected record's key on LookupOK.
    fn tp_relation_lookup(&mut self, tp: &TpRef, c: &'static Control, r: &RecHandle, fi: usize) -> R<()> {
        let fd = r.borrow().field(fi);
        let Some(rel) = fd.relation.first() else { return Ok(()) };
        let Some(rt) = self.cat.table_by_name(&rel.table) else { return Ok(()) };
        let Some(page) = self.lookup_page_of(rt) else { return Ok(()) };
        let inst = Instance::new(self.cat.pages[&page].code);
        inst.lookup.set(true);
        let target = self.new_record(rt.id, false);
        *inst.rec.borrow_mut() = Some(target);
        let res = self.ui_run_page(page, Some(inst.clone()), None, true)?;
        if equals(&res, &act("lookupok")) {
            let sel = inst.rec.borrow().clone();
            if let Some(sel) = sel {
                let tf = rel.field.as_ref().and_then(|x| rt.field(x)).or(rt.pk().fields.last().copied());
                if let Some(tf) = tf {
                    let v = sel.borrow().get_val(tf);
                    self.tp_set_value(tp, c, v)?;
                }
            }
        }
        Ok(())
    }

    /// DrillDown on a FlowField: run the calculated table's drill-down page on the formula's filters.
    fn tp_flowfield_drilldown(&mut self, r: &RecHandle, fi: usize) -> R<()> {
        let fd = r.borrow().field(fi);
        let Some(cf) = &fd.calc else { return Ok(()) };
        let Some(tt) = self.cat.table_by_name(&cf.table) else { return Ok(()) };
        let page = tt.drilldown_page.as_deref().or(tt.lookup_page.as_deref()).and_then(|p| self.cat.id_of(ObjectKind::Page, p));
        let Some(page) = page else { return Ok(()) };
        let target = self.new_record(tt.id, false);
        let consts: Vec<al_model::Cond> = cf.conds.iter().filter(|c| c.kind != CondKind::Field).cloned().collect();
        self.apply_conds(&target, &consts, None, 0)?;
        for c in cf.conds.iter().filter(|c| c.kind == CondKind::Field) {
            let (Some(tfi), Some(sfi)) = (tt.field(&c.field), r.borrow().tdef.field(&c.value)) else { continue };
            if r.borrow().tdef.fields[sfi].class != FieldClass::FlowFilter {
                let v = r.borrow().row[sfi].clone();
                target.borrow_mut().set_filter(tfi, Some(crate::filter::FNode::Eq(v)), String::new());
            }
        }
        self.ui_run_page(page, None, Some(target), false)?;
        Ok(())
    }

    /// RunObject action without OnAction: open the target object on the current record
    /// (RunPageOnRec), the RunPageLink/RunPageView filters, in RunPageMode.
    fn tp_run_object(&mut self, tp: &TpRef, c: &'static Control, obj: &str) -> R<()> {
        // `Page "Sales Statistics"`, `Codeunit 80`, `Report Microsoft.Foo."Create Fiscal Year"`
        let toks = al_syntax::lex(obj, &Default::default()).unwrap_or_default();
        let kind = toks.first().map(|t| t.text.to_string()).unwrap_or_else(|| "Page".into());
        let Some(name) = toks.iter().skip(1).filter(|t| t.is_name() || t.kind == al_syntax::Tk::Int).last().map(|t| t.text.to_string()) else { return Ok(()) };
        let name = name.as_str();
        let cur = self.tp_rec(tp);
        match kind.to_ascii_lowercase().as_str() {
            "page" => {
                let Some(pid) = self.cat.id_of(ObjectKind::Page, name) else { return Ok(()) };
                let on_rec = c.props.get("runpageonrec").is_some_and(|v| v.eq_ignore_ascii_case("true"));
                let rec = match (self.cat.pages[&pid].source_table, &cur) {
                    (Some(t), Some(cr)) if on_rec && cr.borrow().tdef.id == t => {
                        // the page runs on the current record (its filters and position)
                        Some(copy_record(cr))
                    }
                    (Some(t), _) => {
                        let target = self.new_record(t, false);
                        if let Some(view) = c.prop_src.get("runpageview").or_else(|| c.props.get("runpageview")) {
                            if let Ok(toks) = al_syntax::lex(view, &Default::default()) {
                                let v = al_model::parse_table_view(&toks);
                                self.apply_conds(&target, &v.conds, None, 0)?;
                                if !v.sorting.is_empty() {
                                    let fields: Vec<usize> = v.sorting.iter().filter_map(|n| target.borrow().tdef.field(n)).collect();
                                    let mut rb = target.borrow_mut();
                                    if let Some(k) = rb.tdef.keys.iter().position(|k| k.fields.starts_with(&fields)) {
                                        let kf = rb.tdef.keys[k].fields.clone();
                                        rb.key = rb.schema.keys.iter().position(|sk| *sk == kf).unwrap_or(0);
                                    }
                                    rb.ascending = !v.descending;
                                }
                            }
                        }
                        if let Some(link) = c.prop_src.get("runpagelink").or_else(|| c.props.get("runpagelink")) {
                            if let Ok(toks) = al_syntax::lex(link, &Default::default()) {
                                let conds = al_model::parse_conds(&toks);
                                self.apply_conds(&target, &conds, cur.as_ref(), 0)?;
                            }
                        }
                        Some(target)
                    }
                    (None, _) => None,
                };
                // a non-modal page opened by an action that no Trap/PageHandler waits for just opens
                // in the client; the test goes on (BC tests rely on this, e.g. RunPageMode = Create)
                if self.in_test() && !self.traps.iter().any(|t| t.borrow().page == pid) && !self.has_handler(crate::testing::UiKind::Page, Some(pid)) {
                    return Ok(());
                }
                let mode = match c.props.get("runpagemode").map(|m| m.to_ascii_lowercase()) {
                    Some(m) if m == "create" => "new",
                    Some(m) if m == "view" => "view",
                    _ => "edit",
                };
                self.ui_run_page_mode(pid, None, rec, false, mode)?;
            }
            "codeunit" => {
                if let Some(id) = self.cat.id_of(ObjectKind::Codeunit, name) {
                    self.run_codeunit(id, cur)?;
                }
            }
            "report" => {
                if let Some(id) = self.cat.id_of(ObjectKind::Report, name) {
                    let rec = if c.props.get("runpageonrec").is_some_and(|v| v.eq_ignore_ascii_case("true")) { cur.as_ref().map(copy_record) } else { None };
                    // Reporting Triggers.SubstituteReport (as for Report.Run)
                    let id = self.report_substitute(id, crate::report::RunMode::Normal, "", rec.as_ref())?;
                    if let Some(&ci) = self.cat.code_by_key.get(&(ObjectKind::Report, id)) {
                        let inst = Instance::new(ci);
                        self.report_run(&inst, crate::report::RunReq { request_page: true, rec, params: None, output: crate::report::Output::Default, handlers: true })?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Page.Run / RunModal (static, or on a page variable `inst`): a trapped TestPage, the matching
    /// (Modal)PageHandler, or "Unhandled UI" in tests. Returns the closing action.
    pub fn ui_run_page(&mut self, page_id: i64, inst: Option<Rc<Instance>>, rec: Option<RecHandle>, modal: bool) -> R<Val> {
        self.ui_run_page_mode(page_id, inst, rec, modal, "edit")
    }

    /// Page.Run in a given mode ("edit", "view", "new" for RunPageMode = Create).
    pub fn ui_run_page_mode(&mut self, page_id: i64, inst: Option<Rc<Instance>>, rec: Option<RecHandle>, modal: bool, mode: &str) -> R<Val> {
        let Some(pd) = self.cat.pages.get(&page_id) else { return err(format!("Page {page_id} does not exist")) };
        if !self.in_test() {
            self.messages.push(format!("(UI) page {} opened", pd.name));
            return Ok(act("lookupcancel"));
        }
        let inst = inst.unwrap_or_else(|| Instance::new(pd.code));
        self.instance_globals(&inst);
        if let Some(src) = rec {
            // the page shows the caller's view, positioned on the caller's record
            *inst.rec.borrow_mut() = Some(src);
        }
        let lookup = inst.lookup.get();
        let kind = if modal { crate::testing::UiKind::ModalPage } else { crate::testing::UiKind::Page };
        // RunModal: a ModalPageHandler first (BC TestHandleModalForm), then the client, where a
        // trapped TestPage takes it; Page.Run: the trap first (TestHandleForm)
        let mut handler = if modal { self.take_handler(kind, Some(page_id)) } else { None };
        // a trapped TestPage takes the page (Page.Run continues while the test drives it)
        // the most recent trap of the page (BC NavTestExecution.FindTrap searches from the end);
        // traps of disposed TestPages (no variable holds them any more) are gone
        self.traps.retain(|t| Rc::strong_count(t) > 1);
        if let Some(pos) = self.traps.iter().rposition(|t| t.borrow().page == page_id).filter(|_| handler.is_none()) {
            let tp = self.traps.remove(pos);
            {
                let mut t = tp.borrow_mut();
                t.inst = inst;
                t.parts.clear();
                t.open = false;
            }
            self.tp_open(&tp, mode)?;
            return Ok(act(if lookup { "lookupcancel" } else { "ok" }));
        }
        if handler.is_none() {
            handler = self.take_handler(kind, Some(page_id));
        }
        let Some((pr, hinst)) = handler else {
            // BC creates the page object before it looks for a trap or handler: the generated page
            // constructor runs InitializeForm (OnInit), then NavForm.RunAsync calls TestHandleForm.
            // So OnInit runs, and can fail, before "Unhandled UI" (CRM Setup Test
            // CannotOpenPageIfAppAreaBasic expects CheckAppAreaOnlyBasic's error from OnInit; when
            // it failed, the Basic experience it left in Application Area Cache reached the
            // codeunit's later tests)
            if !inst.inited.replace(true) {
                if let Some(tp) = self.new_test_page(page_id, Some(inst.clone())) {
                    let exts: Vec<Rc<Instance>> = pd.ext_code.iter().map(|&c| Instance::new(c)).collect();
                    for e in &exts {
                        self.bind_ext_globals(e, &inst);
                    }
                    tp.borrow_mut().ext = exts;
                    self.tp_trigger(&tp, "oninit", &[])?;
                }
            }
            // the Error Messages page (table "Error Message") hides the real errors: show the first
            // ones in the failure, so that the cause is visible in the results
            let mut shown = String::new();
            let src = inst.rec.borrow().clone();
            if let Some(h) = src.filter(|h| h.borrow().tdef.name == "Error Message") {
                if let Some(fi) = h.borrow().tdef.field("Message") {
                    let probe = crate::record::copy_record(&h);
                    let mut msgs = vec![];
                    if self.find_first(&probe, false).unwrap_or(false) {
                        loop {
                            msgs.push(probe.borrow().get_val(fi).to_text());
                            if msgs.len() >= 3 || self.next(&probe, 1).unwrap_or(0) == 0 {
                                break;
                            }
                        }
                    }
                    if !msgs.is_empty() {
                        shown = format!(" (errors: {})", msgs.join(" | "));
                    }
                }
            }
            return err_code("MissingUIHandler", format!("Unhandled UI: {} {}{shown}", if modal { "ModalPage" } else { "Page" }, pd.name));
        };
        let Some(tp) = self.new_test_page(page_id, Some(inst)) else { return Ok(act("cancel")) };
        self.tp_open(&tp, mode)?;
        self.tp_open_parts(&tp)?;
        let param_is_testpage = matches!(self.code(pr.code).procs[pr.proc as usize].params.first().map(|p| &p.ty), Some(al_syntax::TypeRef::Object { kind, .. }) if kind == "testpage");
        let arg = if param_is_testpage { Val::TestPage(tp.clone()) } else { Val::Object(tp.borrow().inst.clone()) };
        // a handler on a Page variable gets `var Response: Action` and RunModal returns what it
        // leaves there (BC NavTestExecution.TestHandleModalForm); -1 = not assigned
        let resp = slot(Val::Opt { ord: -1, enum_id: None });
        let mut hargs = vec![Arg::Ref(LVal::Slot(slot(arg), None))];
        if !param_is_testpage && self.code(pr.code).procs[pr.proc as usize].params.len() >= 2 {
            hargs.push(Arg::Ref(LVal::Slot(resp.clone(), None)));
        }
        self.invoke(pr, hargs, Some(hinst), None, None)?;
        const ACTIONS: [&str; 8] = ["ok", "cancel", "lookupok", "lookupcancel", "yes", "no", "runobject", "runsystem"];
        let response = match &*resp.borrow() {
            Val::Opt { ord, .. } if *ord >= 0 => ACTIONS.get(*ord as usize).copied(),
            _ => None,
        };
        let closed = tp.borrow().action;
        let action = match closed {
            Some(a) => a,
            None => {
                // the handler returned without closing the page: BC closes it like the client
                // does, writing the current record (a pending new row of a DelayedInsert page
                // is inserted)
                let a = response.unwrap_or(if lookup { "lookupcancel" } else { "cancel" });
                if tp.borrow().open {
                    self.tp_save(&tp)?;
                    self.tp_close(&tp, a)?;
                }
                a
            }
        };
        Ok(act(action))
    }
}

/// Validation errors raised through a TestPage report GetLastErrorCode = 'TestValidation'.
fn wrap_validation(e: Flow) -> Flow {
    match e {
        Flow::Err(mut x) => {
            // BC: NavTestValidationException (no inner exception), so GetLastErrorCode is
            // "TestValidation" whatever the validation raised (a missing UI handler stays a
            // harness failure)
            if x.code != "MissingUIHandler" {
                x.code = "TestValidation";
            }
            Flow::Err(x)
        }
        o => o,
    }
}

/// The text of a TestPage validation error (after wrap_validation): BC's
/// NavTestValidationException says 'Validation error for Field: <control>,  Message = '<text>''.
fn validation_text(e: Flow, ctl: &str) -> Flow {
    match e {
        Flow::Err(mut x) if x.code == "TestValidation" && !x.msg.is_empty() => {
            x.msg = format!("Validation error for Field: {ctl},  Message = '{}'", x.msg);
            Flow::Err(x)
        }
        o => o,
    }
}

/// The part controls of a page layout (subpages, not system parts).
/// The source expressions of field controls that are not table fields, unquoted (the page
/// variables a SaveValues page keeps; expressions match no variable name).
fn save_value_sources(tree: &[Control], out: &mut Vec<String>) {
    for c in tree {
        if c.field.is_none() {
            if let Some(src) = c.source.as_deref() {
                let n = src.trim().trim_matches('"');
                if !n.is_empty() {
                    out.push(n.to_string());
                }
            }
        }
        save_value_sources(&c.children, out);
    }
}

fn collect_parts<'a>(tree: &'a [Control], out: &mut Vec<&'a Control>) {
    for c in tree {
        if c.kind == "part" && c.part_page.is_some() {
            out.push(c);
        }
        collect_parts(&c.children, out);
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

/// The control's own OptionCaption property, split. The catalog keeps a plain string value
/// unquoted (`A,B,C`) and other forms as source text (`'A,B,C', Comment = '...'`).
fn ctl_option_captions(c: &Control) -> Option<Vec<String>> {
    let raw = c.props.get("optioncaption")?;
    let Some(q) = raw.find('\'') else {
        return Some(raw.split(',').map(|x| x.to_string()).collect());
    };
    let start = q + 1;
    let mut out = String::new();
    let mut it = raw[start..].chars().peekable();
    while let Some(ch) = it.next() {
        if ch == '\'' {
            if it.peek() == Some(&'\'') {
                it.next();
                out.push('\'');
                continue;
            }
            break;
        }
        out.push(ch);
    }
    Some(out.split(',').map(|x| x.to_string()).collect())
}

fn find_ctl_by_field(tree: &[Control], fi: usize) -> Option<&Control> {
    for c in tree {
        if c.kind == "field" && c.field == Some(fi) {
            return Some(c);
        }
        if let Some(x) = find_ctl_by_field(&c.children, fi) {
            return Some(x);
        }
    }
    None
}

/// GetValidationError([Index]): the Index-th recorded error (1-based), or all of them, one per line
/// (Ncl NavTestField.ALGetValidationError); an index out of range is an error.
fn validation_error_at(errs: &[String], index: Option<&Val>) -> R<Val> {
    match index {
        None => Ok(Val::Text(errs.join("\n"))),
        Some(v) => {
            let i = v.as_i64();
            match usize::try_from(i - 1).ok().and_then(|k| errs.get(k)) {
                Some(m) => Ok(Val::Text(m.clone())),
                None => err(format!("The index {i} is outside the range of the validation errors ({}).", errs.len())),
            }
        }
    }
}
