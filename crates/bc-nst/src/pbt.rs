//! Page background tasks on the client services endpoint (the AL side is al-runtime's
//! `pbt` module).
//!
//! The protocol of the BC 29.1 service tier (data/capture/nst-real, role center parts 1310 and
//! 1393; [src:TODO]):
//!
//! 1. A page enqueues a task during any call (OnOpenPage of the activities part 1310 enqueues
//!    five). The NST starts a child session for it at once.
//! 2. When the child session ends, the NST sends the JSON-RPC request
//!    `PageBackgroundTaskCompletedNotification({FormHandle, TaskId, ClientActivityId})`. The web
//!    server answers `null` right away, finds the form by its handle and, once idle, calls
//! 3. `InvokePageBackgroundTaskCompletion({TaskId, DataSet}, NavRecordState)`. The NST runs
//!    OnPageBackgroundTaskCompleted (or ...Error) on the form and returns a `CodeUnitResponse`:
//!    `{requests, DataSetState, DataSet, VersionNumber}` with the page's record, so the client
//!    shows the new values. A completion error comes back as a fault, which the web server
//!    shows as a notification "<page caption>: <message>".
//!
//! Here each task runs in a child session on a thread of its own (`al_runtime::pbt::spawn_child`).
//! Its outcome goes to a queue and wakes the session's worker (`Job::Wake`), which sends the
//! notification between two calls. Closing a form cancels its tasks.

use al_runtime::pbt::{ChildSpec, ErrorLevel, Outcome, Task};
use al_runtime::{Runtime, Session};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A task of one of the session's forms.
pub struct BgTask {
    pub handle: String,
    pub codeunit: i64,
    pub level: ErrorLevel,
    pub bookmark: Option<Vec<bc_store::Value>>,
    pub cancel: Arc<AtomicBool>,
    /// ClientActivityId of the call that enqueued it (the web server parses it as a Guid).
    pub activity: String,
    pub started: Instant,
    /// Set when the child session ended; the client then asks for the completion.
    pub outcome: Option<Outcome>,
}

/// The session's tasks (shared by the worker and a RunModal's nested worker through `UiLink`).
#[derive(Default)]
pub struct Host {
    pub tasks: HashMap<i32, BgTask>,
    /// Outcomes of child sessions that ended, filled by their threads.
    pub done: Arc<Mutex<Vec<(i32, Outcome)>>>,
}

/// The form that shows `t`'s page (the page instance or one of its extension instances).
pub fn form_of_task(forms: &HashMap<String, crate::form::PageForm>, t: &Task) -> Option<String> {
    forms.iter().find(|(_, f)| Rc::ptr_eq(&f.inst, &t.page) || f.ext.iter().any(|x| Rc::ptr_eq(x, &t.page))).map(|(h, _)| h.clone())
}

impl Host {
    /// Start the tasks the session's pages enqueued during the last call.
    #[allow(clippy::too_many_arguments)]
    pub fn start_queued(&mut self, rt: &'static Runtime, s: &mut Session<'static>, forms: &HashMap<String, crate::form::PageForm>, activity: &str, wake: &tokio::sync::mpsc::UnboundedSender<()>, sid: i32) {
        if s.sys.pbt.queued.is_empty() {
            return;
        }
        let queued = std::mem::take(&mut s.sys.pbt.queued);
        for t in queued {
            let Some(handle) = form_of_task(forms, &t) else {
                // a page instance that no form of this worker shows (a RunModal's nested worker
                // has its own forms): nobody could ask for its completion
                eprintln!("bc-nst[{sid}]: page background task {} (codeunit {}) of a page without an open form: not started", t.id, t.codeunit);
                t.cancel.store(true, Ordering::Relaxed);
                s.sys.pbt.live.remove(&t.id);
                continue;
            };
            let spec = ChildSpec {
                id: t.id,
                company: s.company.clone(),
                user: s.user_id.clone(),
                work_date: s.work_date,
                language: s.global_language,
                codeunit: t.codeunit,
                params: t.params.clone(),
                timeout_ms: t.timeout_ms,
                cancel: t.cancel.clone(),
            };
            let done = self.done.clone();
            let wake = wake.clone();
            let id = t.id;
            let started = al_runtime::pbt::spawn_child(
                rt,
                spec,
                Box::new(move |o| {
                    done.lock().unwrap_or_else(|e| e.into_inner()).push((id, o));
                    let _ = wake.send(());
                }),
            );
            if let Err(e) = started {
                eprintln!("bc-nst[{sid}]: page background task {id} could not start: {e}");
                s.sys.pbt.live.remove(&id);
                continue;
            }
            self.tasks.insert(t.id, BgTask { handle, codeunit: t.codeunit, level: t.level, bookmark: t.bookmark, cancel: t.cancel, activity: activity.to_string(), started: Instant::now(), outcome: None });
        }
    }

    /// Outcomes that arrived since the last look: the `(form handle, task id, activity)` of each
    /// task whose client must be told. Cancelled tasks are dropped.
    pub fn collect(&mut self, sid: i32) -> Vec<(String, i32, String)> {
        let done = std::mem::take(&mut *self.done.lock().unwrap_or_else(|e| e.into_inner()));
        let mut out = vec![];
        for (id, o) in done {
            let Some(t) = self.tasks.get_mut(&id) else { continue };
            if t.cancel.load(Ordering::Relaxed) {
                self.tasks.remove(&id);
                continue;
            }
            let ms = t.started.elapsed().as_secs_f64() * 1000.0;
            match &o {
                Outcome::Done(r) => eprintln!("bc-nst[{sid}]: page background task {id} (codeunit {}) done ({ms:.1} ms): {}", t.codeunit, crate::rpc::clip(&format!("{r:?}"), 300)),
                Outcome::Failed { code, text, .. } => eprintln!("bc-nst[{sid}]: page background task {id} (codeunit {}) failed ({ms:.1} ms): {code} {}", t.codeunit, crate::rpc::clip(text, 300)),
            }
            t.outcome = Some(o);
            out.push((t.handle.clone(), id, t.activity.clone()));
        }
        out
    }

    /// The finished task `id` of form `handle`, for its completion trigger.
    pub fn take(&mut self, handle: &str, id: i32) -> Option<BgTask> {
        match self.tasks.get(&id) {
            Some(t) if t.handle == handle && t.outcome.is_some() => self.tasks.remove(&id),
            _ => None,
        }
    }

    /// Tasks of form `handle` still running (`NavRecordState.PendingBackgroundTasks`).
    pub fn pending(&self, handle: &str) -> Vec<i32> {
        let mut v: Vec<i32> = self.tasks.iter().filter(|(_, t)| t.handle == handle && t.outcome.is_none()).map(|(id, _)| *id).collect();
        v.sort_unstable();
        v
    }

    /// The form closed: its tasks are cancelled (PageBackgroundTask's parent-form token).
    /// Returns their ids.
    pub fn cancel_form(&mut self, handle: &str) -> Vec<i32> {
        let ids: Vec<i32> = self.tasks.iter().filter(|(_, t)| t.handle == handle).map(|(id, _)| *id).collect();
        for id in &ids {
            if let Some(t) = self.tasks.remove(id) {
                t.cancel.store(true, Ordering::Relaxed);
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    //! The activities part of the Business Manager role center (page 1310) on the embedded store
    //! (bc-server's seed): OnOpenPage enqueues its cue tasks, child sessions run codeunit 1310
    //! "O365 Activities Dictionary", the completion trigger stores the values. Skipped without
    //! vendor/bcapps.
    use super::*;
    use crate::form::PageForm;
    use std::time::Duration;

    #[test]
    fn activities_part_runs_its_cue_tasks_in_child_sessions() {
        let _store = crate::dataset::tests::STORE.write().unwrap_or_else(|e| e.into_inner());
        let Some(rt) = crate::dataset::tests::runtime() else {
            eprintln!("vendor/bcapps missing, skipping");
            return;
        };
        let mut s = Session::new(rt, "CRONUS");
        s.sys.pbt.host = true;
        let page = &s.cat.pages[&1310];
        let f = PageForm::new(&mut s, page, None, None);
        f.open(&mut s).expect("OnInit/OnOpenPage");
        let _ = s.commit();
        let ids: Vec<i32> = s.sys.pbt.queued.iter().map(|t| t.id).collect();
        assert!(!ids.is_empty(), "OnOpenPage enqueues the cue tasks");
        assert!(s.sys.pbt.queued.iter().all(|t| t.codeunit == 1310 && t.timeout_ms == Some(2000)));
        let mut forms = HashMap::new();
        forms.insert("h1".to_string(), f);
        let (wake, mut woken) = tokio::sync::mpsc::unbounded_channel::<()>();
        let mut host = Host::default();
        host.start_queued(rt, &mut s, &forms, "6bb24215-4062-5244-a733-bb96d15a7d13", &wake, 0);
        assert!(s.sys.pbt.queued.is_empty());
        assert_eq!(host.pending("h1").len(), ids.len());
        // every child session reports back
        let t0 = std::time::Instant::now();
        let mut ended = vec![];
        while ended.len() < ids.len() && t0.elapsed() < Duration::from_secs(60) {
            if woken.try_recv().is_err() {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            ended.extend(host.collect(0));
        }
        assert_eq!(ended.len(), ids.len(), "all tasks ended");
        assert!(ended.iter().all(|(h, _, a)| h == "h1" && a == "6bb24215-4062-5244-a733-bb96d15a7d13"));
        assert!(host.pending("h1").is_empty());
        // the client asks for each completion: the trigger stores the values in Activities Cue
        let f = &forms["h1"];
        let insts: Vec<Rc<al_runtime::Instance>> = std::iter::once(f.inst.clone()).chain(f.ext.iter().cloned()).collect();
        for (_, id, _) in &ended {
            let t = host.take("h1", *id).expect("finished task");
            assert!(matches!(t.outcome, Some(Outcome::Done(_))), "{:?}", t.outcome);
            match s.pbt_complete(&insts, f.rec.as_ref(), t.bookmark.as_deref(), *id, t.outcome.unwrap(), t.level) {
                Ok(ran) => assert!(ran),
                Err(al_runtime::Flow::Err(e)) => panic!("completion of {id}: {}", e.msg),
                Err(_) => panic!("completion of {id}: unexpected flow"),
            }
            let _ = s.commit();
        }
        assert!(host.take("h1", ended[0].1).is_none(), "a completion runs once");
        // the last completion stamps the cached values (PBTList empty)
        let rec = f.rec.clone().unwrap();
        let stamped = {
            let r = rec.borrow();
            let fi = r.tdef.field("Last Date/Time Modified").unwrap();
            r.row[fi].clone()
        };
        assert!(!matches!(stamped, bc_store::Value::DateTime(0)), "Last Date/Time Modified is set: {stamped:?}");
        // closing the form cancels what is left
        assert!(host.cancel_form("h1").is_empty());
        s.rollback();
    }
}
