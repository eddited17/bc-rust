//! Page background tasks: `CurrPage.EnqueueBackgroundTask`, `CurrPage.CancelBackgroundTask`,
//! `Page.GetBackgroundParameters`, `Page.SetBackgroundTaskResult`, the page triggers
//! `OnPageBackgroundTaskCompleted` / `OnPageBackgroundTaskError` and
//! `TestPage.RunPageBackgroundTask`.
//!
//! Behaviour of the BC 29.1 service tier (NavForm.EnqueueBackgroundTask, PageBackgroundTask,
//! PageBackgroundChildSessionTask, NavChildSessionTaskRuntime, PageBackgroundTaskCompletion*):
//!
//! - The task's codeunit runs in a **child session**: a new session of the same user, company
//!   and work date, read-only (`IsReadOnly`, read-only connection intent), without client
//!   callbacks. It reads its input with `Page.GetBackgroundParameters()` and hands back a
//!   `Dictionary of [Text, Text]` with `Page.SetBackgroundTaskResult()`. Its transaction is
//!   rolled back at the end.
//! - **Timeout**: the Timeout argument in ms; without it the server's
//!   PageBackgroundTaskDefaultTimeout (2 min); larger values are cut to
//!   PageBackgroundTaskMaxTimeout (10 min); 0 or less means none. A task that runs out of time
//!   ends with error code `ChildSessionTaskTimeout` ("Child Session task was terminated because
//!   of a timeout.").
//! - **Completion** runs on the page, in the parent session: `OnPageBackgroundTaskCompleted(TaskId,
//!   Results)` when the codeunit succeeded, else `OnPageBackgroundTaskError(TaskId, ErrorCode,
//!   ErrorText, ErrorCallStack, var IsHandled)` with empty results. An error that no trigger
//!   handled is raised again unless the task's PageBackgroundTaskErrorLevel is Ignore (the web
//!   client shows it as a notification "<page caption>: <message>"). Neither trigger runs when
//!   the task was cancelled (CancelBackgroundTask, the page closed) or the page's current record
//!   changed since the task was enqueued (CancelOnRecordChange is always on for AL's tasks).
//! - **Where it runs**: a client session runs the child session on a thread of its own and tells
//!   the client when it is done (bc-nst: `PageBackgroundTaskCompletedNotification`, then the
//!   client's `InvokePageBackgroundTaskCompletion` runs the completion trigger). Test sessions
//!   (and every session without such a host) run the task at once, inside EnqueueBackgroundTask,
//!   and the completion trigger right after it, as BC does under test execution
//!   (PageBackgroundTask.CanPageBackgroundTaskRunAsync is false there).
//!
//! Hosts: set [`PbtState::host`], take the tasks from [`PbtState::queued`] after each call, run
//! them with [`spawn_child`] and finish them with [`Session::pbt_complete`].

use crate::interp::*;
use al_model::ProcRef;
use crate::record::RecHandle;
use crate::value::*;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// PageBackgroundTaskDefaultTimeout (00:02:00).
pub const DEFAULT_TIMEOUT_MS: i64 = 120_000;
/// PageBackgroundTaskMaxTimeout (00:10:00).
pub const MAX_TIMEOUT_MS: i64 = 600_000;
/// GetLastErrorCode of a task that ran out of time [src:X05].
pub const TIMEOUT_CODE: &str = "ChildSessionTaskTimeout";
/// The error text of a task that ran out of time [src:X05].
pub const TIMEOUT_TEXT: &str = "Child Session task was terminated because of a timeout.";
/// Native stack of a child session thread (as workers.rs).
const STACK: usize = 256 << 20;

/// PageBackgroundTaskErrorLevel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorLevel {
    Error,
    Warning,
    Ignore,
}

/// How a task's codeunit ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The results it set with Page.SetBackgroundTaskResult (empty when it set none).
    Done(Vec<(String, String)>),
    /// GetLastErrorCode / GetLastErrorText / GetLastErrorCallStack of the child session.
    Failed { code: String, text: String, stack: String },
}

impl Outcome {
    pub fn timeout() -> Outcome {
        Outcome::Failed { code: TIMEOUT_CODE.into(), text: TIMEOUT_TEXT.into(), stack: String::new() }
    }
}

/// A task a page enqueued, for the host to run.
pub struct Task {
    pub id: i32,
    /// The page instance (an extension's call is the page's task too).
    pub page: Rc<Instance>,
    pub codeunit: i64,
    pub params: Vec<(String, String)>,
    /// None: no timeout.
    pub timeout_ms: Option<u64>,
    pub level: ErrorLevel,
    /// Primary key of the page's record when the task was enqueued (CancelOnRecordChange).
    pub bookmark: Option<Vec<bc_store::Value>>,
    /// Set by CancelBackgroundTask; the host also sets it when the page closes. A running child
    /// session stops at its next checkpoint (crate::workers::watch_cancel).
    pub cancel: Arc<AtomicBool>,
}

/// The input and output of the task running in this (child) session.
pub struct ChildCtx {
    pub params: Vec<(String, String)>,
    pub results: Vec<(String, String)>,
}

/// Page background task state of a session (`SysState::pbt`).
#[derive(Default)]
pub struct PbtState {
    /// The session's host runs tasks in child sessions of their own (bc-nst). Off: tasks run at
    /// once inside EnqueueBackgroundTask (tests, and sessions without a client).
    pub host: bool,
    /// Tasks enqueued since the host last looked.
    pub queued: Vec<Task>,
    /// Cancel flags of the tasks enqueued and not finished, by id (CancelBackgroundTask).
    pub live: HashMap<i32, (Weak<Instance>, Arc<AtomicBool>)>,
    /// Set while this session runs a task's codeunit (Page.GetBackgroundParameters).
    pub child: Option<ChildCtx>,
    /// A read-only (child) session: writes to database tables fail.
    pub read_only: bool,
    /// Page instances and their extension instances (TestPages register theirs), so a
    /// completion trigger also runs in the page's extensions.
    pub exts: Vec<(Weak<Instance>, Vec<Weak<Instance>>)>,
}

static NEXT_TASK_ID: AtomicI32 = AtomicI32::new(0);

/// A new task id (process-wide, as BC's static counter).
pub fn next_task_id() -> i32 {
    NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed) + 1
}

/// Error codes as `&'static str` (AlError::code); a handful of distinct codes in practice.
fn intern(code: &str) -> &'static str {
    static CODES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    if code.is_empty() {
        return "";
    }
    let mut v = CODES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = v.iter().find(|c| **c == code) {
        return c;
    }
    let c: &'static str = Box::leak(code.to_string().into_boxed_str());
    v.push(c);
    c
}

/// The message of writes in a read-only (child) session.
pub const READ_ONLY_TEXT: &str = "The operation could not complete because the session is read-only. Page background tasks run in read-only child sessions and cannot write to the database.";

fn dict_pairs(v: &Val) -> Vec<(String, String)> {
    match v {
        Val::Dict(d) => d.borrow().iter().map(|(k, v)| (k.to_text(), v.to_text())).collect(),
        _ => vec![],
    }
}

/// A `Dictionary of [Text, Text]`.
pub fn dict_val(pairs: &[(String, String)]) -> Val {
    Val::Dict(Rc::new(std::cell::RefCell::new(pairs.iter().map(|(k, v)| (Val::Text(k.clone()), Val::Text(v.clone()))).collect())))
}

/// Primary key values of a record (the task's bookmark).
pub fn bookmark_of(h: &RecHandle) -> Vec<bc_store::Value> {
    let b = h.borrow();
    b.tdef.pk().fields.iter().map(|&fi| b.row[fi].clone()).collect()
}

fn level_of(v: &Val) -> ErrorLevel {
    let ord = match v {
        Val::Opt { ord, .. } => *ord as i64,
        other => other.as_i64(),
    };
    match ord {
        1 | 10 => ErrorLevel::Warning,
        2 | 20 => ErrorLevel::Ignore,
        _ => ErrorLevel::Error,
    }
}

/// The Timeout argument as the service tier applies it (ValidatePageBackgroundTaskTimeout).
pub fn effective_timeout(ms: i64) -> Option<u64> {
    let ms = ms.min(MAX_TIMEOUT_MS);
    (ms > 0).then_some(ms as u64)
}

impl Session<'_> {
    /// The page instance that `inst` belongs to (itself, or the page of an extension instance).
    fn pbt_page_of(&mut self, inst: &Rc<Instance>) -> Rc<Instance> {
        self.sys.pbt.exts.retain(|(p, _)| p.strong_count() > 0);
        for (p, xs) in &self.sys.pbt.exts {
            if xs.iter().any(|x| x.upgrade().is_some_and(|x| Rc::ptr_eq(&x, inst))) {
                if let Some(p) = p.upgrade() {
                    return p;
                }
            }
        }
        inst.clone()
    }

    /// The page instance and its extension instances (as registered with [`Session::pbt_register`]).
    fn pbt_instances(&self, page: &Rc<Instance>) -> Vec<Rc<Instance>> {
        let mut out = vec![page.clone()];
        if let Some((_, xs)) = self.sys.pbt.exts.iter().find(|(p, _)| p.upgrade().is_some_and(|p| Rc::ptr_eq(&p, page))) {
            out.extend(xs.iter().filter_map(|x| x.upgrade()));
        }
        out
    }

    /// Register a page instance's extension instances (TestPage open).
    pub fn pbt_register(&mut self, page: &Rc<Instance>, exts: &[Rc<Instance>]) {
        self.sys.pbt.exts.retain(|(p, _)| p.strong_count() > 0 && !p.upgrade().is_some_and(|p| Rc::ptr_eq(&p, page)));
        if !exts.is_empty() {
            self.sys.pbt.exts.push((Rc::downgrade(page), exts.iter().map(Rc::downgrade).collect()));
        }
    }

    /// `CurrPage.EnqueueBackgroundTask(var TaskId; CodeunitId; [Parameters]; [Timeout]; [ErrorLevel]): Boolean`.
    pub(crate) fn pbt_enqueue(&mut self, f: &mut Frame, args: &[al_syntax::Expr]) -> R<Val> {
        let Some(inst) = f.inst.clone() else {
            return err("EnqueueBackgroundTask can only be called on a page.");
        };
        if args.len() < 2 {
            return err("EnqueueBackgroundTask needs a task id variable and a codeunit.");
        }
        let lv = self.lvalue(f, &args[0])?;
        let codeunit = self.eval(f, &args[1])?.as_i64();
        let params = match args.get(2) {
            Some(a) => dict_pairs(&self.eval(f, a)?),
            None => vec![],
        };
        let timeout = match args.get(3) {
            Some(a) => self.eval(f, a)?.as_i64(),
            None => DEFAULT_TIMEOUT_MS,
        };
        let level = match args.get(4) {
            Some(a) => level_of(&self.eval(f, a)?),
            None => ErrorLevel::Error,
        };
        if !self.cat.code_by_key.contains_key(&(al_syntax::ObjectKind::Codeunit, codeunit)) {
            return err(format!("The Codeunit {codeunit} does not exist."));
        }
        let page = self.pbt_page_of(&inst);
        let id = next_task_id();
        self.write_lval(&lv, Val::Int(id))?;
        let rec = page.rec.borrow().clone();
        let bookmark = rec.as_ref().map(bookmark_of);
        if self.sys.pbt.host {
            let cancel = Arc::new(AtomicBool::new(false));
            self.sys.pbt.live.insert(id, (Rc::downgrade(&page), cancel.clone()));
            self.sys.pbt.queued.push(Task { id, page, codeunit, params, timeout_ms: effective_timeout(timeout), level, bookmark, cancel });
            return Ok(Val::Bool(true));
        }
        // no host (tests): the task runs now, then its completion trigger (BC under test execution:
        // NavChildSessionTaskRuntime.RunAsync awaited, PageBackgroundTaskCompletionImmediateTrigger)
        let outcome = self.pbt_run_here(codeunit, params);
        let insts = self.pbt_instances(&page);
        self.pbt_complete(&insts, rec.as_ref(), bookmark.as_deref(), id, outcome, level)?;
        Ok(Val::Bool(true))
    }

    /// `CurrPage.CancelBackgroundTask(TaskId): Boolean` (false: no such task of this session).
    pub(crate) fn pbt_cancel(&mut self, f: &mut Frame, args: &[al_syntax::Expr]) -> R<Val> {
        let id = match args.first() {
            Some(a) => self.eval(f, a)?.as_i64() as i32,
            None => return Ok(Val::Bool(false)),
        };
        match self.sys.pbt.live.remove(&id) {
            Some((_, flag)) => {
                flag.store(true, Ordering::Relaxed);
                Ok(Val::Bool(true))
            }
            None => Ok(Val::Bool(false)),
        }
    }

    /// `Page.GetBackgroundParameters()` / `Page.SetBackgroundTaskResult(Results)`.
    pub(crate) fn pbt_static(&mut self, f: &mut Frame, m: &str, args: &[al_syntax::Expr]) -> R<Val> {
        let what = if m == "getbackgroundparameters" { "PAGE.GETBACKGROUNDPARAMETERS" } else { "PAGE.SETBACKGROUNDTASKRESULT" };
        if self.sys.pbt.child.is_none() {
            // own wording [src:X17]
            return err(format!("{what} can only run inside a page background task."));
        }
        if m == "getbackgroundparameters" {
            return Ok(dict_val(&self.sys.pbt.child.as_ref().map(|c| c.params.clone()).unwrap_or_default()));
        }
        let v = match args.first() {
            Some(a) => self.eval(f, a)?,
            None => Val::Void,
        };
        let pairs = dict_pairs(&v);
        if let Some(c) = self.sys.pbt.child.as_mut() {
            c.results = pairs;
        }
        Ok(Val::Void)
    }

    /// Run a task's codeunit in this session as its child session (no host): its writes are
    /// rolled back, its errors come back as the outcome.
    pub fn pbt_run_here(&mut self, codeunit: i64, params: Vec<(String, String)>) -> Outcome {
        let saved = self.sys.pbt.child.replace(ChildCtx { params, results: vec![] });
        let saved_ro = std::mem::replace(&mut self.sys.pbt.read_only, true);
        let mark = self.db.savepoint().ok();
        let r = self.trapped(|s| s.run_codeunit(codeunit, None));
        let ctx = std::mem::replace(&mut self.sys.pbt.child, saved);
        self.sys.pbt.read_only = saved_ro;
        if let Some(m) = mark {
            let _ = self.rollback_trapped(m);
        }
        match r {
            Err(Flow::Err(e)) => {
                self.note_error(&e);
                Outcome::Failed { code: self.last_error_code.clone(), text: e.msg.clone(), stack: self.last_error_stack.clone() }
            }
            _ => Outcome::Done(ctx.map(|c| c.results).unwrap_or_default()),
        }
    }

    /// The completion of task `id` on the page whose instances (page first, then extensions) are
    /// `insts` and whose record is `rec`: OnPageBackgroundTaskCompleted or OnPageBackgroundTaskError
    /// (PageBackgroundTaskCompletionTriggerBase.RaiseOnPageCompletionTriggerAsync). Returns false when
    /// the triggers did not run because the page's record changed. An unhandled error is raised
    /// unless `level` is Ignore.
    pub fn pbt_complete(&mut self, insts: &[Rc<Instance>], rec: Option<&RecHandle>, bookmark: Option<&[bc_store::Value]>, id: i32, outcome: Outcome, level: ErrorLevel) -> R<bool> {
        self.sys.pbt.live.remove(&id);
        if let (Some(r), Some(b)) = (rec, bookmark) {
            if bookmark_of(r) != b {
                return Ok(false);
            }
        }
        match outcome {
            Outcome::Done(results) => {
                for i in insts {
                    let Some(p) = self.code(i.code).trigger("OnPageBackgroundTaskCompleted") else { continue };
                    let args = vec![Arg::Val(Val::Int(id)), Arg::Val(dict_val(&results))];
                    self.invoke(ProcRef { code: i.code, proc: p }, args, Some(i.clone()), rec.cloned(), None)?;
                }
            }
            Outcome::Failed { code, text, stack } => {
                let handled: Slot = Rc::new(std::cell::RefCell::new(Val::Bool(false)));
                // the trigger gets the text with line breaks as backslashes (NavDialog.ReplaceCRLFWithBackSlash)
                let shown = text.replace("\r\n", "\\").replace('\n', "\\");
                for i in insts {
                    let Some(p) = self.code(i.code).trigger("OnPageBackgroundTaskError") else { continue };
                    let args = vec![Arg::Val(Val::Int(id)), Arg::Val(Val::Text(code.clone())), Arg::Val(Val::Text(shown.clone())), Arg::Val(Val::Text(stack.clone())), Arg::Ref(LVal::Slot(handled.clone(), None))];
                    self.invoke(ProcRef { code: i.code, proc: p }, args, Some(i.clone()), rec.cloned(), None)?;
                }
                let is_handled = handled.borrow().as_bool();
                if !is_handled && level != ErrorLevel::Ignore {
                    let mut e = AlError::new(text);
                    e.code = intern(&code);
                    return Err(Flow::Err(Box::new(e)));
                }
            }
        }
        Ok(true)
    }

    /// `TestPage.RunPageBackgroundTask(CodeunitId; [Parameters]; [RunCompletionTrigger]): Dictionary
    /// of [Text, Text]` (NavTestPage.ALRunPageBackgroundTask): runs the codeunit as the page's
    /// background task and returns its results. Without the completion trigger a failed task's error
    /// is raised; with it the trigger handles the error.
    pub fn pbt_test_run(&mut self, page: Option<Rc<Instance>>, rec: Option<RecHandle>, codeunit: i64, params: Vec<(String, String)>, run_completion: bool) -> R<Val> {
        let outcome = self.pbt_run_here(codeunit, params);
        let results = match &outcome {
            Outcome::Done(r) => r.clone(),
            Outcome::Failed { .. } => vec![],
        };
        if run_completion {
            if let Some(p) = page {
                let insts = self.pbt_instances(&p);
                self.pbt_complete(&insts, rec.as_ref(), None, next_task_id(), outcome, ErrorLevel::Error)?;
            }
        } else if let Outcome::Failed { code, text, .. } = outcome {
            let mut e = AlError::new(text);
            e.code = intern(&code);
            return Err(Flow::Err(Box::new(e)));
        }
        Ok(dict_val(&results))
    }
}

/// What a child session needs from its parent.
pub struct ChildSpec {
    pub id: i32,
    pub company: String,
    pub user: String,
    pub work_date: i32,
    pub language: i32,
    pub codeunit: i64,
    pub params: Vec<(String, String)>,
    pub timeout_ms: Option<u64>,
    pub cancel: Arc<AtomicBool>,
}

/// Run a task in a child session on a thread of its own; `done` gets the outcome (on another
/// thread) once the codeunit ends or the timeout passes. A timed-out or cancelled child stops at
/// its next checkpoint.
pub fn spawn_child(rt: &'static Runtime, spec: ChildSpec, done: Box<dyn FnOnce(Outcome) + Send>) -> std::io::Result<()> {
    let id = spec.id;
    std::thread::Builder::new().name(format!("bc-pbt-wait-{id}")).spawn(move || {
        let (tx, rx) = mpsc::channel::<Outcome>();
        let timeout = spec.timeout_ms;
        let cancel = spec.cancel.clone();
        let started = std::thread::Builder::new().name(format!("bc-pbt-{id}")).stack_size(STACK).spawn(move || {
            crate::stackguard::register(STACK);
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_child(rt, spec)));
            let out = r.unwrap_or_else(|_| Outcome::Failed { code: String::new(), text: "Internal error in bc-rust while running the page background task.".into(), stack: String::new() });
            let _ = tx.send(out);
        });
        let out = match started {
            Err(e) => Outcome::Failed { code: String::new(), text: format!("The page background task could not start: {e}"), stack: String::new() },
            Ok(_) => match timeout {
                Some(ms) => match rx.recv_timeout(Duration::from_millis(ms)) {
                    Ok(o) => o,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        cancel.store(true, Ordering::Relaxed);
                        Outcome::timeout()
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => Outcome::Failed { code: String::new(), text: "The page background task ended unexpectedly.".into(), stack: String::new() },
                },
                None => rx.recv().unwrap_or_else(|_| Outcome::Failed { code: String::new(), text: "The page background task ended unexpectedly.".into(), stack: String::new() }),
            },
        };
        done(out);
    })?;
    Ok(())
}

/// The child session itself (NavChildSessionTaskRuntime.RunAsync): same user, company, work date
/// and language as the parent, read-only, no client; rolled back at the end.
fn run_child(rt: &'static Runtime, spec: ChildSpec) -> Outcome {
    let mut s = match Session::try_new(rt, &spec.company) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed { code: String::new(), text: format!("The child session could not connect to the database: {e}"), stack: String::new() },
    };
    s.user_id = spec.user;
    crate::userauth::resolve_session_user(&mut s);
    s.work_date = spec.work_date;
    if spec.language != 0 && spec.language != s.global_language {
        s.global_language = spec.language;
    }
    s.gui_allowed = false;
    s.sys.pbt.read_only = true;
    s.sys.pbt.child = Some(ChildCtx { params: spec.params, results: vec![] });
    crate::workers::watch_cancel(&mut s, spec.cancel.clone());
    let r = s.run_codeunit(spec.codeunit, None);
    let out = match r {
        Err(Flow::Err(e)) => {
            s.note_error(&e);
            Outcome::Failed { code: s.last_error_code.clone(), text: e.msg.clone(), stack: s.last_error_stack.clone() }
        }
        _ => Outcome::Done(s.sys.pbt.child.take().map(|c| c.results).unwrap_or_default()),
    };
    s.rollback();
    crate::workers::watch_cancel_end();
    out
}
