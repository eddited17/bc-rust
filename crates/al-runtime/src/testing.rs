//! Test-codeunit support: UI handlers ([MessageHandler], [ConfirmHandler], ...), test isolation
//! (Commit becomes a savepoint, everything is rolled back after the test) and the per-test context.

use crate::interp::*;
use crate::value::*;
use al_model::{Catalog, ProcRef};
use al_syntax::{ObjectKind, TypeRef};
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiKind {
    Message,
    Confirm,
    StrMenu,
    ModalPage,
    Page,
    RequestPage,
    Report,
    SendNotification,
    RecallNotification,
    Hyperlink,
    SessionSettings,
    FilterPage,
    /// [HttpClientHandler]: answers HttpClient requests in tests (crate::http drives it).
    HttpClient,
}

impl UiKind {
    pub fn from_attr(name: &str) -> Option<UiKind> {
        Some(match name.to_ascii_lowercase().as_str() {
            "messagehandler" => UiKind::Message,
            "confirmhandler" => UiKind::Confirm,
            "strmenuhandler" => UiKind::StrMenu,
            "modalpagehandler" => UiKind::ModalPage,
            "pagehandler" => UiKind::Page,
            "requestpagehandler" => UiKind::RequestPage,
            "reporthandler" => UiKind::Report,
            "sendnotificationhandler" => UiKind::SendNotification,
            "recallnotificationhandler" => UiKind::RecallNotification,
            "hyperlinkhandler" => UiKind::Hyperlink,
            "sessionsettingshandler" => UiKind::SessionSettings,
            "filterpagehandler" => UiKind::FilterPage,
            "httpclienthandler" => UiKind::HttpClient,
            _ => return None,
        })
    }
}

pub struct UiHandler {
    pub kind: UiKind,
    pub name: String,
    pub proc: ProcRef,
    /// Page (ModalPage/Page handlers) or report (Report/RequestPage handlers) the handler is for.
    pub target: Option<i64>,
    pub called: bool,
}

/// State of the test procedure that is running.
pub struct TestCtx {
    /// The test codeunit instance (its globals are shared by the test, handlers and its subscribers).
    pub inst: Rc<Instance>,
    pub handlers: Vec<UiHandler>,
    /// Savepoint of the last Commit() (test isolation: errors roll back to it, the test end rolls back all).
    pub commit_mark: usize,
    /// The test's transaction (BC: the logical transaction the test method runs in, a new one per
    /// test): its TransactionType and whether it wrote since its last Commit() (crate::sysfuncs
    /// TxnState).
    pub txn: crate::sysfuncs::TxnState,
}

fn handler_target(cat: &Catalog, ty: &TypeRef) -> Option<i64> {
    match ty {
        TypeRef::Object { kind, target, .. } => {
            let k = match kind.as_str() {
                "testpage" | "page" => ObjectKind::Page,
                "testrequestpage" | "report" => ObjectKind::Report,
                _ => return None,
            };
            cat.id_of(k, target)
        }
        _ => None,
    }
}

impl TestCtx {
    /// Context for running test procedure `proc` of the test codeunit `inst`, with the
    /// handlers named in its [HandlerFunctions('...')] attribute.
    pub fn new(cat: &'static Catalog, inst: Rc<Instance>, proc: u32) -> Result<TestCtx, String> {
        let co = &cat.code[inst.code as usize];
        let p = &co.procs[proc as usize];
        let mut handlers = vec![];
        for a in &p.attributes {
            if !a.name.eq_ignore_ascii_case("handlerfunctions") {
                continue;
            }
            for arg in &a.args {
                let al_syntax::Expr::Str(list) = arg else { continue };
                for name in list.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                    let Some(&hi) = co.find(name).first() else { return Err(format!("Handler function {name} does not exist in {}", co.name)) };
                    let hp = &co.procs[hi as usize];
                    let Some(kind) = hp.attributes.iter().find_map(|a| UiKind::from_attr(&a.name)) else {
                        return Err(format!("{name} is not a handler function"));
                    };
                    let target = hp.params.first().and_then(|x| handler_target(cat, &x.ty));
                    handlers.push(UiHandler { kind, name: hp.name.clone(), proc: ProcRef { code: inst.code, proc: hi }, target, called: false });
                }
            }
        }
        Ok(TestCtx { inst, handlers, commit_mark: 0, txn: Default::default() })
    }

    pub fn not_executed(&self) -> Vec<String> {
        self.handlers.iter().filter(|h| !h.called).map(|h| h.name.clone()).collect()
    }
}

fn unhandled<T>(what: String) -> R<T> {
    err_code("MissingUIHandler", format!("Unhandled UI: {what}"))
}

impl<'r> Session<'r> {
    pub fn in_test(&self) -> bool {
        self.test.is_some()
    }

    /// Find the active handler for a UI event (marking it called).
    pub(crate) fn take_handler(&mut self, kind: UiKind, target: Option<i64>) -> Option<(ProcRef, Rc<Instance>)> {
        let t = self.test.as_mut()?;
        let h = t
            .handlers
            .iter_mut()
            .find(|h| h.kind == kind && (target.is_none() || h.target.is_none() || h.target == target))?;
        h.called = true;
        Some((h.proc, t.inst.clone()))
    }

    /// Is there a handler for a UI event (without marking it called)?
    pub(crate) fn has_handler(&self, kind: UiKind, target: Option<i64>) -> bool {
        self.test.as_ref().is_some_and(|t| t.handlers.iter().any(|h| h.kind == kind && (target.is_none() || h.target.is_none() || h.target == target)))
    }

    /// Message(): MessageHandler in tests, the session's message list otherwise.
    pub fn ui_message(&mut self, msg: String) -> R<()> {
        if !self.in_test() {
            self.messages.push(msg);
            return Ok(());
        }
        match self.take_handler(UiKind::Message, None) {
            Some((pr, inst)) => {
                self.invoke(pr, vec![Arg::Val(Val::Text(msg))], Some(inst), None, None)?;
                Ok(())
            }
            None => unhandled(format!("Message '{msg}'")),
        }
    }

    /// Confirm(): ConfirmHandler(Question; var Reply) in tests, the default answer otherwise.
    pub fn ui_confirm(&mut self, question: String, default: bool) -> R<bool> {
        if !self.in_test() {
            return Ok(default);
        }
        match self.take_handler(UiKind::Confirm, None) {
            Some((pr, inst)) => {
                let reply = slot(Val::Bool(default));
                self.invoke(pr, vec![Arg::Val(Val::Text(question)), Arg::Ref(LVal::Slot(reply.clone(), None))], Some(inst), None, None)?;
                let b = reply.borrow().as_bool();
                Ok(b)
            }
            None => unhandled(format!("Confirm '{question}'")),
        }
    }

    /// StrMenu(): StrMenuHandler(Options; var Choice; Instruction) in tests, the default otherwise.
    pub fn ui_strmenu(&mut self, options: String, default: i32, instruction: String) -> R<i32> {
        if !self.in_test() {
            return Ok(default);
        }
        match self.take_handler(UiKind::StrMenu, None) {
            Some((pr, inst)) => {
                let choice = slot(Val::Int(default));
                let p = &self.code(pr.code).procs[pr.proc as usize];
                let mut args = vec![Arg::Val(Val::Text(options)), Arg::Ref(LVal::Slot(choice.clone(), None))];
                if p.params.len() > 2 {
                    args.push(Arg::Val(Val::Text(instruction)));
                }
                self.invoke(pr, args, Some(inst), None, None)?;
                let c = choice.borrow().as_i64() as i32;
                Ok(c)
            }
            None => unhandled(format!("StrMenu '{instruction}'")),
        }
    }

    /// Hyperlink(): HyperlinkHandler in tests.
    pub fn ui_hyperlink(&mut self, url: String) -> R<()> {
        if let Some((pr, inst)) = self.take_handler(UiKind::Hyperlink, None) {
            self.invoke(pr, vec![Arg::Val(Val::Text(url))], Some(inst), None, None)?;
        }
        Ok(())
    }

    /// Report.Run / RunModal in tests: a [ReportHandler] for the report takes the run (the report
    /// itself does not run). Returns whether a handler was called.
    pub fn ui_report_handler(&mut self, report_id: i64, inst: &Rc<Instance>) -> R<bool> {
        if !self.in_test() {
            return Ok(false);
        }
        let Some((pr, hinst)) = self.take_handler(UiKind::Report, Some(report_id)) else { return Ok(false) };
        self.invoke(pr, vec![Arg::Ref(LVal::Slot(slot(Val::Object(inst.clone())), None))], Some(hinst), None, None)?;
        Ok(true)
    }

    /// Show a report's request page: in tests the [RequestPageHandler] drives it as a
    /// TestRequestPage; returns the closing action ("ok" when the handler closes nothing, and
    /// always outside tests, where no client can answer).
    pub fn ui_request_page(&mut self, report_id: i64, inst: &Rc<Instance>) -> R<&'static str> {
        if !self.in_test() {
            return Ok("ok");
        }
        let Some((pr, hinst)) = self.take_handler(UiKind::RequestPage, Some(report_id)) else {
            let name = self.cat.code_of(ObjectKind::Report, report_id).map(|c| c.name.clone()).unwrap_or_else(|| report_id.to_string());
            return unhandled(format!("RequestPage {name}"));
        };
        let tp = self.new_request_test_page(report_id, inst.clone());
        self.tp_open(&tp, "edit")?;
        self.invoke(pr, vec![Arg::Ref(LVal::Slot(slot(Val::TestPage(tp.clone())), None))], Some(hinst), None, None)?;
        let closed = tp.borrow().action;
        Ok(match closed {
            Some(a) => a,
            None => {
                if tp.borrow().open {
                    self.tp_close(&tp, "ok")?;
                }
                "ok"
            }
        })
    }

    /// Notification.Send(): SendNotificationHandler(var Notification): Boolean in tests.
    pub fn ui_send_notification(&mut self, n: Val) -> R<()> {
        if let Some((pr, inst)) = self.take_handler(UiKind::SendNotification, None) {
            self.invoke(pr, vec![Arg::Ref(LVal::Slot(slot(n), None))], Some(inst), None, None)?;
        }
        Ok(())
    }

    pub fn ui_recall_notification(&mut self, n: Val) -> R<()> {
        if let Some((pr, inst)) = self.take_handler(UiKind::RecallNotification, None) {
            self.invoke(pr, vec![Arg::Ref(LVal::Slot(slot(n), None))], Some(inst), None, None)?;
        }
        Ok(())
    }

    /// An instance value for an object variable (codeunit/report/page).
    pub fn object_value(&mut self, kind: ObjectKind, id: i64) -> Val {
        match self.cat.code_by_key.get(&(kind, id)) {
            Some(&ci) => {
                if kind == ObjectKind::Codeunit && self.code(ci).single_instance {
                    Val::Object(self.single_instance(ci))
                } else {
                    Val::Object(Instance::new(ci))
                }
            }
            None => Val::Opaque("Object"),
        }
    }

    /// Commit(): a real commit, or a savepoint under test isolation.
    pub fn al_commit(&mut self) -> R<()> {
        // the next statement starts a new transaction: its type may change again (sysfuncs TxnState)
        self.txn_mut().wrote = false;
        if self.test.is_some() || self.bg_depth > 0 {
            let sp = self.db.savepoint().map_err(Self::store_err)?;
            if let Some(t) = self.test.as_mut() {
                t.commit_mark = sp;
            } else {
                self.bg_commit_mark = Some(sp);
            }
            return Ok(());
        }
        self.db.commit().map_err(Self::store_err)
    }

    /// Undo a trapped error's writes back to `mark` (Codeunit.Run with a result, an isolated
    /// subscriber). A background task's or the running test's last-Commit mark past `mark` no
    /// longer exists (the Commit() was inside the trapped code): it moves to `mark` (still a valid
    /// savepoint). On PostgreSQL a later rollback to the stale mark would fail and abort the
    /// transaction (docs/pg-vs-memory.md).
    pub(crate) fn rollback_trapped(&mut self, mark: usize) -> R<()> {
        self.db.rollback_to(mark).map_err(Self::store_err)?;
        if self.bg_commit_mark.is_some_and(|c| c > mark) {
            self.bg_commit_mark = Some(mark);
        }
        if let Some(t) = self.test.as_mut().filter(|t| t.commit_mark > mark) {
            t.commit_mark = mark;
        }
        Ok(())
    }

    /// Roll back to the last Commit() of the running test (an error ends the write transaction).
    pub fn rollback_to_commit(&mut self) {
        self.txn_mut().wrote = false;
        if let Some(sp) = self.test.as_ref().map(|t| t.commit_mark) {
            let _ = self.db.rollback_to(sp);
        }
    }
}

