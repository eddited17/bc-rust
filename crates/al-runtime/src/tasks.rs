//! TaskScheduler (CreateTask, TaskExists, CancelTask, SetTaskReady, CanCreateTask) as BC's
//! ALTaskScheduler does it: a task is a row of the system table "Scheduled Task" (2000000175, not
//! per company) with its codeunit, failure codeunit, company, Is Ready, Not Before (now when empty
//! or in the past) and record. CanCreateTask is false under test isolation (IsTestIsolation), as in
//! Microsoft's CI and as bc-rust always answered.
//!
//! Running tasks: BC's task scheduler starts a ready task whose Not Before has passed in a new
//! background session once the creating transaction has committed. bc-rust runs the due tasks when
//! the session yields with Sleep(): Test Job Queue / Test Background Posting commit and then poll
//! with Sleep until the job's log entry or posted document appears (they waited 200-300 s per test
//! for tasks that never ran). Each task runs like StartSession does (crate::builtins start_session):
//! a background context without the test's UI handlers, in the task's company, on its own savepoint;
//! when it fails, its writes are rolled back and the failure codeunit runs with the task's record
//! and the error as GetLastErrorText. The task's row is removed when it starts. Tasks created while
//! tasks run wait for the next Sleep.

use crate::interp::*;
use crate::record::*;
use crate::value::*;
use al_syntax::Expr;

pub const SCHEDULED_TASK: i64 = 2000000175;

fn set_field(h: &RecHandle, name: &str, v: Val) -> R<()> {
    let mut b = h.borrow_mut();
    if let Some(fi) = b.tdef.field(name) {
        let sv = to_store(b.field(fi), &v).map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
        b.row[fi] = sv;
    }
    Ok(())
}

fn get_field(h: &RecHandle, name: &str) -> Val {
    let b = h.borrow();
    b.tdef.field(name).map(|fi| b.get_val(fi)).unwrap_or(Val::Void)
}

impl<'r> Session<'r> {
    /// The Scheduled Task record with id `id`, loaded (None: no such task).
    fn task_record(&mut self, id: &Val) -> R<Option<RecHandle>> {
        if !self.cat.tables.contains_key(&SCHEDULED_TASK) || matches!(id, Val::Guid(g) if *g == [0; 16]) {
            return Ok(None);
        }
        let h = self.new_record(SCHEDULED_TASK, false);
        set_field(&h, "ID", id.clone())?;
        Ok(match self.rec_get_pk_r(&h)? {
            Some(row) => {
                h.borrow_mut().row = (row).into();
                Some(h)
            }
            None => None,
        })
    }

    /// The record a RecordId points to, loaded (None: empty RecordId or no such record).
    fn record_of_id(&mut self, rid: &Val) -> Option<RecHandle> {
        let Val::RecordId(t, k) = rid else { return None };
        if *t == 0 || !self.cat.tables.contains_key(t) {
            return None;
        }
        let h = self.new_record(*t, false);
        let pkf = h.borrow().tdef.pk().fields.clone();
        for (j, &fi) in pkf.iter().enumerate() {
            if let Some(v) = k.get(j) {
                h.borrow_mut().row[fi] = v.clone();
            }
        }
        let row = self.rec_get_pk(&h)?;
        h.borrow_mut().row = (row).into();
        Some(h)
    }

    /// TaskScheduler.<m>(...) (`m` lower-cased).
    pub fn task_scheduler(&mut self, f: &mut Frame, m: &str, args: &[Expr]) -> R<Val> {
        let mut a = Vec::with_capacity(args.len());
        for e in args {
            a.push(self.eval(f, e)?);
        }
        match m {
            // true only while bc serve runs the background pool (crate::workers), never in tests
            "cancreatetask" => Ok(Val::Bool(self.test.is_none() && crate::workers::pool().is_some())),
            "createtask" => self.create_task(&a),
            "taskexists" => Ok(Val::Bool(self.task_record(a.first().unwrap_or(&Val::Void))?.is_some())),
            "canceltask" => match self.task_record(a.first().unwrap_or(&Val::Void))? {
                Some(h) => {
                    self.rec_delete(&h, false)?.map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
                    Ok(Val::Bool(true))
                }
                None => Ok(Val::Bool(false)),
            },
            "settaskready" => match self.task_record(a.first().unwrap_or(&Val::Void))? {
                Some(h) => {
                    set_field(&h, "Is Ready", Val::Bool(true))?;
                    if let Some(nb @ Val::DateTime(t)) = a.get(1) {
                        if *t != bc_store::DATETIME_UNDEFINED {
                            set_field(&h, "Not Before", nb.clone())?;
                        }
                    }
                    self.rec_modify(&h, false)?.map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
                    Ok(Val::Bool(true))
                }
                None => Ok(Val::Bool(false)),
            },
            _ => err(format!("Not implemented in bc-rust: TaskScheduler.{m}")),
        }
    }

    /// CreateTask(CodeunitId, FailureCodeunitId [, IsReady, Company, NotBefore, RecordId, Timeout]).
    fn create_task(&mut self, a: &[Val]) -> R<Val> {
        let cu = a.first().map(|v| v.as_i64()).unwrap_or(0);
        let fail = a.get(1).map(|v| v.as_i64()).unwrap_or(0);
        for c in [cu, fail] {
            if c != 0 && !self.cat.code_by_key.contains_key(&(al_syntax::ObjectKind::Codeunit, c)) {
                return err(format!("Codeunit {c} does not exist"));
            }
        }
        let ready = a.get(2).map(|v| v.as_bool()).unwrap_or(true);
        let company = a.get(3).map(|v| v.to_text()).filter(|c| !c.trim().is_empty()).unwrap_or_else(|| self.company.clone());
        let now = crate::clock::now_ms(crate::clock::at(self, self.steps));
        let not_before = match a.get(4) {
            Some(Val::DateTime(t)) if *t != bc_store::DATETIME_UNDEFINED && *t >= now - 60_000 => *t,
            _ => now,
        };
        let rid = a.get(5).cloned().unwrap_or(Val::RecordId(0, vec![]));
        if matches!(&rid, Val::RecordId(t, _) if *t != 0) && self.record_of_id(&rid).is_none() {
            return err("The record that the task refers to does not exist.");
        }
        if !self.cat.tables.contains_key(&SCHEDULED_TASK) {
            return Ok(Val::Guid([0; 16]));
        }
        let id = Val::Guid(crate::clock::new_guid());
        let h = self.new_record(SCHEDULED_TASK, false);
        set_field(&h, "ID", id.clone())?;
        set_field(&h, "User Name", Val::Text(self.user_id.clone()))?;
        set_field(&h, "Company", Val::Text(company))?;
        set_field(&h, "Is Ready", Val::Bool(ready))?;
        set_field(&h, "Not Before", Val::DateTime(not_before))?;
        set_field(&h, "Run Codeunit", Val::Int(cu as i32))?;
        set_field(&h, "Failure Codeunit", Val::Int(fail as i32))?;
        set_field(&h, "Record", rid)?;
        // Timeout: the background pool (crate::workers) stops the task past it
        match a.get(6) {
            Some(t @ Val::Duration(d)) if *d > 0 => set_field(&h, "Timeout", t.clone())?,
            Some(Val::Int(ms)) if *ms > 0 => set_field(&h, "Timeout", Val::Duration(*ms as i64))?,
            Some(Val::BigInt(ms)) if *ms > 0 => set_field(&h, "Timeout", Val::Duration(*ms))?,
            _ => {}
        }
        self.rec_insert(&h, false)?.map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
        Ok(id)
    }

    /// Run the tasks that are ready and due (Sleep). Not inside a task or background session.
    pub fn run_due_tasks(&mut self) -> R<()> {
        // with the background pool (crate::workers) tasks run in sessions of their own
        if self.bg_depth > 0 || crate::workers::pool().is_some() || !self.cat.tables.contains_key(&SCHEDULED_TASK) {
            return Ok(());
        }
        let all = self.new_record(SCHEDULED_TASK, false);
        let rows = self.fetch_all(&all)?;
        let now = crate::clock::peek_ms();
        for row in rows {
            let h = self.new_record(SCHEDULED_TASK, false);
            h.borrow_mut().row = (row).into();
            let ready = get_field(&h, "Is Ready").as_bool();
            // stored DateTimes are rounded: a task created "now" may read a few ms ahead
            let due = matches!(get_field(&h, "Not Before"), Val::DateTime(t) if t <= now + 1_000);
            if !ready || !due {
                continue;
            }
            // the task may have been cancelled by a task that ran before it
            let id = get_field(&h, "ID");
            let Some(h) = self.task_record(&id)? else { continue };
            let cu = get_field(&h, "Run Codeunit").as_i64();
            let fail = get_field(&h, "Failure Codeunit").as_i64();
            let company = get_field(&h, "Company").to_text();
            let rid = get_field(&h, "Record");
            self.rec_delete(&h, false)?.map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
            self.run_task(cu, fail, &company, &rid)?;
        }
        Ok(())
    }

    /// One task in a background context; the failure codeunit when it fails.
    fn run_task(&mut self, cu: i64, fail: i64, company: &str, rid: &Val) -> R<()> {
        let saved_company = (!company.is_empty() && company != self.company).then(|| std::mem::replace(&mut self.company, company.to_string()));
        let saved_test = self.test.take();
        let saved_err = (self.last_error.clone(), self.last_error_code.clone(), self.last_error_stack.clone());
        self.bg_depth += 1;
        let saved_commit = self.bg_commit_mark.take();
        let result = (|| -> R<()> {
            let mark = self.db.savepoint().map_err(Self::store_err)?;
            let rec = self.record_of_id(rid);
            match self.run_codeunit(cu, rec) {
                Ok(()) => Ok(()),
                Err(Flow::Err(e)) => {
                    // the error rolls back what the task did since its last Commit(); the failure
                    // codeunit then runs in a new transaction (Job Queue Error Handler finds the
                    // log entry the dispatcher committed)
                    self.note_error(&e);
                    let back = self.bg_commit_mark.take().unwrap_or(mark);
                    self.db.rollback_to(back).map_err(Self::store_err)?;
                    if fail != 0 {
                        let mark2 = self.db.savepoint().map_err(Self::store_err)?;
                        let rec = self.record_of_id(rid);
                        if let Err(Flow::Err(e2)) = self.run_codeunit(fail, rec) {
                            self.note_error(&e2);
                            let back = self.bg_commit_mark.take().unwrap_or(mark2);
                            self.db.rollback_to(back).map_err(Self::store_err)?;
                        }
                    }
                    Ok(())
                }
                Err(o) => Err(o),
            }
        })();
        self.bg_commit_mark = saved_commit;
        self.bg_depth -= 1;
        self.test = saved_test;
        if let Some(c) = saved_company {
            self.company = c;
        }
        (self.last_error, self.last_error_code, self.last_error_stack) = saved_err;
        result
    }
}
