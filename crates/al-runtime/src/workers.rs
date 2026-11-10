//! Background work for `bc serve` (opt-in, docs/job-queue.md): a pool of worker threads that runs
//! due Scheduled Tasks (TaskScheduler.CreateTask, the job queue's dispatcher) and StartSession
//! sessions concurrently, each in a session and database transaction of its own.
//!
//! - **Claiming.** A poller reads the committed, ready and due rows of "Scheduled Task" and claims
//!   a task with `Backend::try_lock_key("bc-task:<id>")` (on PostgreSQL a session-level advisory
//!   lock on a connection of its own). The lock is held for the whole run, across the task's own
//!   Commit() calls, and PostgreSQL drops it when the server process dies. Any number of workers
//!   and server processes can poll the same database; a task runs in one place at a time.
//! - **The row stays while the task runs** (TaskExists is true, as BC's states 0..3) and is
//!   deleted, in a transaction of its own, when the run ends. A server that dies mid-run leaves
//!   the row, so the next poll (here or in another process) starts the task again; start counts
//!   in `shared.bc_task_attempt` stop a task that keeps killing its server after
//!   `max_attempts` starts: its failure codeunit runs instead.
//! - **Transactions.** The task's AL Commit() calls are real commits. On an error, only the open
//!   transaction is rolled back (work committed earlier survives, as in BC); the failure codeunit
//!   then runs in a fresh transaction of the same session, with the error as GetLastErrorText.
//!   Transient database errors (deadlock, serialization failure, lock timeout) leave the task
//!   scheduled for another try instead, as BC's task scheduler retries them.
//! - **StartSession** starts a thread with a new session at once (not queued behind tasks); at
//!   most `max_sessions` run at a time, beyond that StartSession returns false.
//! - **StopSession / timeouts** set the session's cancel flag. A background session's step limit is
//!   a checkpoint every 1,024 AL statements, where the (cold) step-limit branch checks the flag and
//!   fails the session with an error; other sessions pay nothing per statement. A database wait is
//!   cut short by the lock timeout, not by the flag.
//!
//! al-test never starts the pool: tasks keep running inline at Sleep there (crate::tasks).

use crate::interp::*;
use crate::record::*;
use crate::value::*;
use bc_store::{KeyLock, Row};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

const SCHEDULED_TASK: i64 = crate::tasks::SCHEDULED_TASK;
/// Native stack of a background thread (AL recursion needs about 5 KB per call level).
const STACK: usize = 256 << 20;

/// Pool settings (`bc serve --bg-workers N`, BC_BG_* environment variables).
#[derive(Clone, Debug)]
pub struct Config {
    /// Threads that run scheduled tasks (BC's TaskSchedulerMaxConcurrentRunningTasks).
    pub workers: usize,
    /// How often the poller looks for due tasks when nothing wakes it (ms).
    pub poll_ms: u64,
    /// Timeout of a task whose row gives none (ms; 0 = none). BC's job queue passes 12 hours.
    pub task_timeout_ms: u64,
    /// Starts of one task (server crashes, transient errors) before it is given up.
    pub max_attempts: i32,
    /// StartSession sessions running at a time.
    pub max_sessions: usize,
    /// Company the poller reads with (Scheduled Task is not per company).
    pub company: String,
}

impl Config {
    /// `workers` threads, the rest from BC_BG_POLL_MS, BC_BG_TASK_TIMEOUT_S, BC_BG_MAX_ATTEMPTS,
    /// BC_BG_MAX_SESSIONS or defaults (1,000 ms, 12 h, 3, 64).
    pub fn with_workers(workers: usize, company: &str) -> Config {
        fn env<T: std::str::FromStr>(k: &str, d: T) -> T {
            std::env::var(k).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(d)
        }
        Config {
            workers,
            poll_ms: env("BC_BG_POLL_MS", 1_000u64).max(10),
            task_timeout_ms: env("BC_BG_TASK_TIMEOUT_S", 12 * 3600u64) * 1_000,
            max_attempts: env("BC_BG_MAX_ATTEMPTS", 3i32).max(1),
            max_sessions: env("BC_BG_MAX_SESSIONS", 64usize).max(1),
            company: company.to_string(),
        }
    }
}

/// Counters of the pool (log line every minute while anything happens).
#[derive(Default, Debug)]
pub struct Stats {
    pub tasks_ok: AtomicU64,
    pub tasks_failed: AtomicU64,
    pub tasks_retried: AtomicU64,
    pub sessions_ok: AtomicU64,
    pub sessions_failed: AtomicU64,
}

struct Active {
    cancel: Arc<AtomicBool>,
    started: Instant,
    deadline: Option<Instant>,
    what: String,
}

enum Job {
    Task { id: [u8; 16], lock: Box<dyn KeyLock> },
}

pub struct Pool {
    rt: &'static Runtime,
    pub cfg: Config,
    /// ServiceInstanceId() while the pool runs: new for every server start, so the job queue can
    /// tell a run that died with an earlier server from its own (Job Queue Management's
    /// UpdateRetriableFailedJobQueueLogEntry).
    pub instance_id: i32,
    next_sid: AtomicI32,
    active: Mutex<HashMap<i32, Active>>,
    running: Mutex<HashSet<[u8; 16]>>,
    sessions: AtomicUsize,
    idle: AtomicUsize,
    wake: (Mutex<bool>, Condvar),
    tx: Mutex<mpsc::Sender<Job>>,
    pub stats: Stats,
}

static POOL: OnceLock<&'static Pool> = OnceLock::new();

thread_local! {
    /// SessionId() of the session on this thread (1 outside background sessions).
    static SID: Cell<i32> = const { Cell::new(1) };
    /// Cancel flag of the background session on this thread.
    static CANCEL: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// The running pool (None: background work runs inline, as in al-test).
pub fn pool() -> Option<&'static Pool> {
    POOL.get().copied()
}

/// SessionId() on this thread.
pub fn current_session_id() -> i32 {
    SID.with(|c| c.get())
}

/// ServiceInstanceId(): 1 without the pool.
pub fn service_instance_id() -> i32 {
    pool().map(|p| p.instance_id).unwrap_or(1)
}

/// IsSessionActive(id) for a background session of the pool.
pub fn is_active(id: i64) -> bool {
    pool().is_some_and(|p| p.active.lock().unwrap().contains_key(&(id as i32)))
}

/// StopSession(id): ask a background session to stop (false: no such session).
pub fn stop(id: i64) -> bool {
    match pool() {
        Some(p) => match p.active.lock().unwrap().get(&(id as i32)) {
            Some(a) => {
                a.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        },
        None => false,
    }
}

/// True when the background session on this thread was stopped (StopSession or its timeout).
#[inline]
pub fn cancel_requested() -> bool {
    CANCEL.with(|c| c.borrow().as_ref().is_some_and(|f| f.load(Ordering::Relaxed)))
}

pub const CANCEL_MESSAGE: &str = "The background session was stopped (StopSession or the task's timeout).";

/// AL statements between two stop checks of a background session: its `step_limit` is the next
/// checkpoint, so other sessions pay nothing per statement.
const CHECK_EVERY: u64 = 1_024;

impl Session<'_> {
    /// `steps` passed `step_limit`: a background session's checkpoint (stopped: an error, else the
    /// next checkpoint), or the step limit's error for every other session.
    #[cold]
    #[inline(never)]
    pub(crate) fn step_limit_reached(&mut self) -> R<()> {
        if CANCEL.with(|c| c.borrow().is_some()) {
            if cancel_requested() {
                return err(CANCEL_MESSAGE);
            }
            self.step_limit = self.steps + CHECK_EVERY;
            return Ok(());
        }
        err(format!("Step limit exceeded ({} AL statements)", self.step_limit))
    }
}

/// Make `flag` the stop flag of the session on this thread (a page background task's child
/// session: crate::pbt), checked at the step-limit checkpoints as a background session's.
pub(crate) fn watch_cancel(s: &mut Session<'_>, flag: Arc<AtomicBool>) {
    CANCEL.with(|c| *c.borrow_mut() = Some(flag));
    s.step_limit = s.steps + CHECK_EVERY;
}

/// End [`watch_cancel`] on this thread.
pub(crate) fn watch_cancel_end() {
    CANCEL.with(|c| *c.borrow_mut() = None);
}

/// Start the pool (once per process). Returns it.
pub fn start(rt: &'static Runtime, cfg: Config) -> &'static Pool {
    if let Some(p) = pool() {
        return p;
    }
    // AutoIncrement from a database sequence (SQL Server IDENTITY semantics): with sessions running
    // at the same time, "table maximum + 1" hands two sessions the same Entry No. (the first load
    // test failed "The Job Queue Log Entry already exists. Entry No.='4'")
    crate::recops::set_identity_autoincrement(true);
    // the PostgreSQL locking contract (bc_store::pg::lock_range_end, ReadIsolation(UpdLock)) is on by
    // default for PostgreSQL (PgBackend::connect_url); BC_PG_TABLE_LOCKS=0 turns it off here too
    let (tx, rx) = mpsc::channel::<Job>();
    let instance_id = new_instance_id(rt, &cfg.company);
    // session ids of different server processes don't meet either (the job queue compares both)
    let first_sid = if instance_id < 20_000 { 1_000 + instance_id * 100_000 } else { 1_000 };
    let p: &'static Pool = Box::leak(Box::new(Pool {
        rt,
        instance_id,
        next_sid: AtomicI32::new(first_sid),
        active: Mutex::new(HashMap::new()),
        running: Mutex::new(HashSet::new()),
        sessions: AtomicUsize::new(0),
        idle: AtomicUsize::new(0),
        wake: (Mutex::new(false), Condvar::new()),
        tx: Mutex::new(tx),
        stats: Stats::default(),
        cfg,
    }));
    let _ = POOL.set(p);
    let rx = Arc::new(Mutex::new(rx));
    for i in 0..p.cfg.workers {
        let rx = rx.clone();
        std::thread::Builder::new()
            .name(format!("bc-task-{i}"))
            .stack_size(STACK)
            .spawn(move || p.worker_loop(rx))
            .expect("task worker thread");
    }
    std::thread::Builder::new().name("bc-task-poller".into()).spawn(move || p.poll_loop()).expect("task poller thread");
    eprintln!(
        "background tasks: {} workers, poll {} ms, task timeout {} s, {} attempts, {} StartSession sessions, service instance {}",
        p.cfg.workers,
        p.cfg.poll_ms,
        p.cfg.task_timeout_ms / 1000,
        p.cfg.max_attempts,
        p.cfg.max_sessions,
        p.instance_id
    );
    p
}

/// ServiceInstanceId of this server start: the next value of the database sequence
/// "$ServiceInstance", so every server process on one database gets its own (BC numbers its
/// Server Instance rows the same way). Falls back to a clock value when the database can't.
fn new_instance_id(rt: &'static Runtime, company: &str) -> i32 {
    use bc_store::SeqOp;
    const NAME: &str = "$ServiceInstance";
    if let Ok(mut s) = Session::try_new(rt, company) {
        for _ in 0..3 {
            let r = (|| -> Result<Option<i64>, bc_store::StoreError> {
                if s.db.seq(SeqOp::Exists(NAME))? != Some(1) {
                    s.db.seq(SeqOp::Create(NAME, 1, 1))?;
                }
                s.db.seq(SeqOp::Next(NAME))
            })();
            let _ = s.db.commit();
            match r {
                Ok(Some(n)) if n > 0 && n < i32::MAX as i64 => return n as i32,
                // another server created the sequence at the same moment: try again
                _ => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    (secs.saturating_sub(1_700_000_000) % (i32::MAX as u64)) as i32
}

fn uuid_text(id: &[u8; 16]) -> String {
    uuid::Uuid::from_bytes(*id).to_string()
}

fn field_val(h: &RecHandle, name: &str) -> Val {
    let b = h.borrow();
    b.tdef.field(name).map(|fi| b.get_val(fi)).unwrap_or(Val::Void)
}

/// The Scheduled Task row `id`, read in session `s`.
fn task_row(s: &mut Session<'_>, id: &[u8; 16]) -> Option<RecHandle> {
    let h = s.new_record(SCHEDULED_TASK, false);
    {
        let mut b = h.borrow_mut();
        let fi = b.tdef.field("ID")?;
        b.row[fi] = bc_store::Value::Guid(*id);
    }
    let row = s.rec_get_pk(&h)?;
    h.borrow_mut().row = row.into();
    Some(h)
}

/// The record a RecordId points to, read in session `s` (None: empty RecordId or no record).
fn record_of_id(s: &mut Session<'_>, rid: &Val) -> Option<RecHandle> {
    let Val::RecordId(t, k) = rid else { return None };
    if *t == 0 || !s.cat.tables.contains_key(t) {
        return None;
    }
    let h = s.new_record(*t, false);
    let pkf = h.borrow().tdef.pk().fields.clone();
    for (j, &fi) in pkf.iter().enumerate() {
        if let Some(v) = k.get(j) {
            h.borrow_mut().row[fi] = v.clone();
        }
    }
    let row = s.rec_get_pk(&h)?;
    h.borrow_mut().row = row.into();
    Some(h)
}

/// Errors worth another try: the database gave up on the transaction, not the AL code.
fn transient(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    m.contains("deadlock detected")
        || m.contains("deadlocked with another session")
        || m.contains("is locked by another session's transaction")
        || m.contains("could not serialize access")
        || m.contains("lock timeout")
        || m.contains("lock_not_available")
}

impl Pool {
    fn next_sid(&self) -> i32 {
        self.next_sid.fetch_add(1, Ordering::Relaxed)
    }

    /// Wake the poller (a task finished, so new ones may be due).
    pub fn wake(&self) {
        let (m, c) = &self.wake;
        *m.lock().unwrap() = true;
        c.notify_one();
    }

    /// Ready, due tasks in Not Before order (ids), read committed in a fresh session.
    fn due_tasks(&self) -> Result<Vec<[u8; 16]>, String> {
        let mut s = Session::try_new(self.rt, &self.cfg.company).map_err(|e| e.to_string())?;
        if !s.cat.tables.contains_key(&SCHEDULED_TASK) {
            return Ok(vec![]);
        }
        let all = s.new_record(SCHEDULED_TASK, false);
        let rows = s.fetch_all(&all).map_err(|f| match f {
            Flow::Err(e) => e.msg,
            _ => "unexpected control flow".into(),
        })?;
        let now = chrono::Utc::now().timestamp_millis();
        let mut due = vec![];
        for row in rows {
            let h = s.new_record(SCHEDULED_TASK, false);
            h.borrow_mut().row = row.into();
            if !field_val(&h, "Is Ready").as_bool() {
                continue;
            }
            // stored DateTimes are rounded: a task created "now" may read a few ms ahead
            let nb = match field_val(&h, "Not Before") {
                Val::DateTime(t) if t == bc_store::DATETIME_UNDEFINED => i64::MIN,
                Val::DateTime(t) => t,
                _ => i64::MIN,
            };
            if nb > now + 1_000 {
                continue;
            }
            if let Val::Guid(g) = field_val(&h, "ID") {
                due.push((nb, g));
            }
        }
        s.rollback();
        due.sort();
        Ok(due.into_iter().map(|(_, g)| g).collect())
    }

    fn poll_loop(&'static self) {
        let mut last_log = Instant::now();
        let mut last_counts = (0, 0, 0, 0, 0);
        let mut last_err = String::new();
        loop {
            {
                let (m, c) = &self.wake;
                let g = m.lock().unwrap();
                let (mut g, _) = c.wait_timeout_while(g, Duration::from_millis(self.cfg.poll_ms), |w| !*w).unwrap();
                *g = false;
            }
            self.watchdog();
            if last_log.elapsed() >= Duration::from_secs(60) {
                last_log = Instant::now();
                let st = &self.stats;
                let counts = (
                    st.tasks_ok.load(Ordering::Relaxed),
                    st.tasks_failed.load(Ordering::Relaxed),
                    st.tasks_retried.load(Ordering::Relaxed),
                    st.sessions_ok.load(Ordering::Relaxed),
                    st.sessions_failed.load(Ordering::Relaxed),
                );
                if counts != last_counts {
                    last_counts = counts;
                    eprintln!(
                        "background tasks: {} ok, {} failed, {} retried; sessions {} ok, {} failed; {} running",
                        counts.0,
                        counts.1,
                        counts.2,
                        counts.3,
                        counts.4,
                        self.active.lock().unwrap().len()
                    );
                }
            }
            let idle = self.idle.load(Ordering::Relaxed);
            if idle == 0 {
                continue;
            }
            let due = match self.due_tasks() {
                Ok(d) => d,
                Err(e) => {
                    if e != last_err {
                        eprintln!("background tasks: cannot read Scheduled Task: {e}");
                        last_err = e;
                    }
                    continue;
                }
            };
            let mut sent = 0;
            for id in due {
                if sent >= idle {
                    break;
                }
                if self.running.lock().unwrap().contains(&id) {
                    continue;
                }
                match self.rt.db.try_lock_key(&format!("bc-task:{}", uuid_text(&id))) {
                    Ok(Some(lock)) => {
                        self.running.lock().unwrap().insert(id);
                        if self.tx.lock().unwrap().send(Job::Task { id, lock }).is_err() {
                            return;
                        }
                        sent += 1;
                    }
                    Ok(None) => {} // another worker or server runs it
                    Err(e) => eprintln!("background tasks: cannot claim a task: {e}"),
                }
            }
        }
    }

    /// Stop sessions past their deadline.
    fn watchdog(&self) {
        let now = Instant::now();
        for (sid, a) in self.active.lock().unwrap().iter() {
            if a.deadline.is_some_and(|d| now > d) && !a.cancel.swap(true, Ordering::Relaxed) {
                eprintln!("background tasks: session {sid} ({}) timed out after {:?}; stopping it", a.what, a.started.elapsed());
            }
        }
    }

    fn worker_loop(&'static self, rx: Arc<Mutex<mpsc::Receiver<Job>>>) {
        crate::stackguard::register(STACK);
        loop {
            self.idle.fetch_add(1, Ordering::Relaxed);
            let job = rx.lock().unwrap().recv();
            self.idle.fetch_sub(1, Ordering::Relaxed);
            match job {
                Ok(Job::Task { id, mut lock }) => {
                    let sid = self.next_sid();
                    SID.with(|c| c.set(sid));
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_task(sid, &id, &mut *lock)));
                    if r.is_err() {
                        eprintln!("background tasks: task {} panicked; it stays scheduled", uuid_text(&id));
                    }
                    CANCEL.with(|c| *c.borrow_mut() = None);
                    SID.with(|c| c.set(1));
                    self.active.lock().unwrap().remove(&sid);
                    drop(lock); // after the row is deleted and committed
                    self.running.lock().unwrap().remove(&id);
                    self.wake();
                }
                Err(_) => return,
            }
        }
    }

    fn run_task(&self, sid: i32, id: &[u8; 16], lock: &mut dyn KeyLock) {
        let mut s = match Session::try_new(self.rt, &self.cfg.company) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("background tasks: no database session for task {}: {e}", uuid_text(id));
                std::thread::sleep(Duration::from_millis(self.cfg.poll_ms));
                return;
            }
        };
        // a task runs in a background session (BC: ClientType::Background)
        s.client_type = "background";
        // finished or cancelled since the poller saw it
        let Some(h) = task_row(&mut s, id) else {
            lock.finish();
            return;
        };
        if !field_val(&h, "Is Ready").as_bool() {
            return;
        }
        let cu = field_val(&h, "Run Codeunit").as_i64();
        let fail = field_val(&h, "Failure Codeunit").as_i64();
        let company = field_val(&h, "Company").to_text();
        let user = field_val(&h, "User Name").to_text();
        let rid = field_val(&h, "Record");
        let timeout_ms = match field_val(&h, "Timeout") {
            Val::Duration(d) if d > 0 => d as u64,
            _ => self.cfg.task_timeout_ms,
        };
        s.rollback();
        let attempt = match lock.attempt() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("background tasks: start count of task {} not kept: {e}", uuid_text(id));
                1
            }
        };
        if !company.trim().is_empty() {
            s.company = company.clone();
        }
        if !user.trim().is_empty() {
            // the user who started it: UserId() and UserSecurityId()
            crate::userauth::set_session_user(&mut s, &user);
        }
        s.step_limit = CHECK_EVERY;
        let cancel = Arc::new(AtomicBool::new(false));
        self.active.lock().unwrap().insert(
            sid,
            Active {
                cancel: cancel.clone(),
                started: Instant::now(),
                deadline: (timeout_ms > 0).then(|| Instant::now() + Duration::from_millis(timeout_ms)),
                what: format!("task {} codeunit {cu}", uuid_text(id)),
            },
        );
        CANCEL.with(|c| *c.borrow_mut() = Some(cancel));
        let outcome: Result<(), String> = if attempt > self.cfg.max_attempts {
            Err(format!("The task was started {} times without finishing, so it is not started again.", attempt - 1))
        } else {
            let rec = record_of_id(&mut s, &rid);
            match s.run_codeunit(cu, rec) {
                Err(Flow::Err(e)) => {
                    s.note_error(&e);
                    Err(e.msg.clone())
                }
                _ => s.db.commit().map_err(|e| e.to_string()),
            }
        };
        CANCEL.with(|c| *c.borrow_mut() = None);
        // the failure codeunit and the clean-up run without checkpoints (not stoppable)
        s.step_limit = u64::MAX;
        if let Err(msg) = outcome {
            s.rollback();
            if s.last_error.is_empty() || attempt > self.cfg.max_attempts {
                s.last_error = msg.clone();
            }
            // BC gives every error a code; the Base App's failure codeunits log the error only with
            // one (Error Message.LogLastError checks GetLastErrorCode() <> ''), and some errors the
            // runtime raises itself have none yet
            if s.last_error_code.is_empty() {
                s.last_error_code = "Dialog".into();
            }
            if transient(&msg) && attempt < self.cfg.max_attempts {
                self.stats.tasks_retried.fetch_add(1, Ordering::Relaxed);
                eprintln!("background tasks: task {} codeunit {cu} hit a transient error (attempt {attempt}), retrying: {msg}", uuid_text(id));
                return;
            }
            self.stats.tasks_failed.fetch_add(1, Ordering::Relaxed);
            eprintln!("background tasks: task {} codeunit {cu} failed: {}", uuid_text(id), msg.lines().next().unwrap_or(""));
            if fail != 0 {
                let rec = record_of_id(&mut s, &rid);
                match s.run_codeunit(fail, rec) {
                    Err(Flow::Err(e2)) => {
                        s.rollback();
                        eprintln!("background tasks: failure codeunit {fail} of task {} failed: {}", uuid_text(id), e2.msg.lines().next().unwrap_or(""));
                    }
                    _ => {
                        if let Err(e) = s.db.commit() {
                            s.rollback();
                            eprintln!("background tasks: failure codeunit {fail} of task {} could not commit: {e}", uuid_text(id));
                        }
                    }
                }
            }
        } else {
            self.stats.tasks_ok.fetch_add(1, Ordering::Relaxed);
        }
        // the run is over: remove the task (unless the AL code cancelled it already), then forget
        // its start count; the claim lock is released by the caller after this commit
        s.company = self.cfg.company.clone();
        if let Some(h) = task_row(&mut s, id) {
            match s.rec_delete(&h, false) {
                Ok(Ok(())) | Err(Flow::Exit) => {}
                Ok(Err(e)) => eprintln!("background tasks: task {} not removed: {e}", uuid_text(id)),
                Err(Flow::Err(e)) => eprintln!("background tasks: task {} not removed: {}", uuid_text(id), e.msg),
                Err(_) => {}
            }
        }
        if let Err(e) = s.db.commit() {
            s.rollback();
            eprintln!("background tasks: task {} not removed: {e}", uuid_text(id));
            return;
        }
        lock.finish();
    }

    /// StartSession(var Id; Codeunit; Company; Record): a new session on a thread of its own.
    /// None when `max_sessions` already run.
    pub fn start_session(&'static self, cu: i64, company: String, user: String, rec: Option<(i64, Row)>) -> Option<i32> {
        if self.sessions.fetch_add(1, Ordering::Relaxed) >= self.cfg.max_sessions {
            self.sessions.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        let sid = self.next_sid();
        let cancel = Arc::new(AtomicBool::new(false));
        self.active.lock().unwrap().insert(
            sid,
            Active {
                cancel: cancel.clone(),
                started: Instant::now(),
                deadline: None,
                what: format!("StartSession codeunit {cu}"),
            },
        );
        let spawned = std::thread::Builder::new().name(format!("bc-session-{sid}")).stack_size(STACK).spawn(move || {
            crate::stackguard::register(STACK);
            SID.with(|c| c.set(sid));
            CANCEL.with(|c| *c.borrow_mut() = Some(cancel));
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run_session(cu, &company, user, rec)));
            if r.is_err() {
                eprintln!("background session {sid} (codeunit {cu}) panicked");
            }
            CANCEL.with(|c| *c.borrow_mut() = None);
            self.active.lock().unwrap().remove(&sid);
            self.sessions.fetch_sub(1, Ordering::Relaxed);
        });
        if spawned.is_err() {
            self.active.lock().unwrap().remove(&sid);
            self.sessions.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        Some(sid)
    }

    fn run_session(&self, cu: i64, company: &str, user: String, rec: Option<(i64, Row)>) {
        let company = if company.trim().is_empty() { self.cfg.company.as_str() } else { company };
        let mut s = match Session::try_new(self.rt, company) {
            Ok(s) => s,
            Err(e) => {
                self.stats.sessions_failed.fetch_add(1, Ordering::Relaxed);
                eprintln!("background session (codeunit {cu}): no database session: {e}");
                return;
            }
        };
        // StartSession: a background session (BC: ClientType::Background)
        s.client_type = "background";
        if !user.trim().is_empty() {
            // the user who started it: UserId() and UserSecurityId()
            crate::userauth::set_session_user(&mut s, &user);
        }
        s.step_limit = CHECK_EVERY;
        let rec = rec.filter(|(t, _)| s.cat.tables.contains_key(t)).map(|(t, row)| {
            let h = s.new_record(t, false);
            h.borrow_mut().row = row.into();
            h
        });
        match s.run_codeunit(cu, rec) {
            Err(Flow::Err(e)) => {
                s.rollback();
                self.stats.sessions_failed.fetch_add(1, Ordering::Relaxed);
                eprintln!("background session (codeunit {cu}) failed: {}", e.msg.lines().next().unwrap_or(""));
            }
            _ => match s.db.commit() {
                Ok(()) => {
                    self.stats.sessions_ok.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    s.rollback();
                    self.stats.sessions_failed.fetch_add(1, Ordering::Relaxed);
                    eprintln!("background session (codeunit {cu}) could not commit: {e}");
                }
            },
        }
    }
}
