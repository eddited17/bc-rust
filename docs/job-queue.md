# Background work: job queue, TaskScheduler and StartSession on PostgreSQL

Status (2026-11-08): the background pool is opt-in. `bc serve --bg-workers N` (or
`BC_BG_WORKERS=N`; bc-nst takes the same switch since I14, 2026-11-09) runs due Scheduled Tasks and StartSession sessions concurrently, each in a
session and PostgreSQL transaction of its own. The PostgreSQL locking contract below is on by
default for every PostgreSQL backend, with or without the pool (`BC_PG_TABLE_LOCKS=0` turns it
off), because concurrent API sessions need it too. Without the switch nothing changes: tasks run inline when a session calls `Sleep`, as al-test
needs them (crates/al-runtime/src/tasks.rs). Code: crates/al-runtime/src/workers.rs, the claim lock
in crates/bc-store (`Backend::try_lock_key`), the locking contract in crates/bc-store/src/pg.rs.

## What BC does

Sources: [src:TODO] and the Base
App's job queue (codeunits 448 Job Queue Dispatcher, 449 Job Queue Start Codeunit, 450 Job Queue
Error Handler, table 472 Job Queue Entry, 474 Job Queue Log Entry, Job Queue Management).

- **Task scheduler.** `TaskScheduler.CreateTask` writes a task (`$ndo$taskscheduling`, states
  NotReady 0, Ready 1, Starting 2, Running 3, Done 4). Every service tier polls for due tasks
  (`WITH (READPAST, READCOMMITTED) WHERE not_before < @now ORDER BY tasktype, timestamp`) and claims
  one with a conditional `UPDATE ... SET state = @new WHERE state = @old`. At most
  `TaskSchedulerMaxConcurrentRunningTasks` run per tier. `TaskExists` is true in states 0 to 3, so
  a running task still exists. The task is deleted when its run ends
  (`TaskRunner.DeleteScheduledTaskAsync`).
- **Errors.** The codeunit runs in a new background session. On an error its open transaction is
  rolled back; work it committed stays. Exceptions the platform calls retriable (SQL deadlocks,
  timeouts, lost connections) are retried up to `MaxRetryCount` = 10 times. Any other error runs
  the failure codeunit with the task's record, GetLastErrorText still set. A tier that crashes
  leaves Running tasks, and another tier recovers them.
- **Job queue.** `Job Queue Entry.ScheduleTask` creates a task for the dispatcher with the error
  handler as failure codeunit and a 12-hour timeout. The dispatcher sets the entry In Process,
  commits, inserts an In Process log entry, commits, then runs the job with a plain
  `Codeunit.Run(Job Queue Start Codeunit)`. A job error therefore leaves both committed rows, and
  the error handler marks them Error. A recurring entry gets its next task from inside the run
  (`Reschedule`, a new task id). Entries with a category run one at a time: the dispatcher takes
  the category row with `ReadIsolation(UpdLock)` as a semaphore and reschedules itself as Waiting.
  `FindStaleJobsAndSetError` (Job Queue Cleanup Tasks) sets entries to Error that are In Process
  with no task for 10 minutes. `UpdateRetriableFailedJobQueueLogEntry` closes the In Process log
  entry of an attempt that died, recognising it by a different service instance id or session id.
- **StartSession** starts a new session at once, with its own transaction; `IsSessionActive` and
  `StopSession` act on it.

## The bc-rust model

### A session per task, BC's transaction rule

Each task or StartSession call gets a new `Session` on a worker thread, with its own PostgreSQL
connection and transaction, in the task's company and as the task's user (Scheduled Task
"User Name"). The AL code's `Commit()` is a real commit. On an error only the open transaction is
rolled back, so committed work survives (the dispatcher's In Process entry and log entry). The
failure codeunit then runs in a fresh transaction of the same session, with the error as
GetLastErrorText. Background sessions have no UI (GuiAllowed false, no UI host). Each one gets its
own `SessionId()`. `ServiceInstanceId()` is new for every server start, so the job queue can tell
an attempt that died with an earlier server from its own.

### Claiming tasks across workers and servers

A poller reads the committed, ready and due Scheduled Task rows (a fresh session for each poll, so
no session cache can hide new rows). It claims a task with `Backend::try_lock_key("bc-task:<id>")`.
On PostgreSQL that is `pg_try_advisory_lock` on a connection of its own, which stays held for the
whole run.

Why not `SELECT ... FOR UPDATE SKIP LOCKED` on the row? The claim must outlive the task's own
commits, and it must not block the task's own writes. A row lock does neither: the job queue
cancels and reschedules tasks from inside a run (`CancelTask` deletes the Scheduled Task row), and
a row lock held by the claiming connection would make that wait on itself. A session-level
advisory lock skips the same way SKIP LOCKED does (a task another worker or server holds is passed
over), and PostgreSQL releases it when the holding connection ends, including a killed server.

The row stays while the task runs (`TaskExists` is true, BC's states 0 to 3). When the run ends,
the row is deleted and committed, and only then is the lock released. A worker that finds the row
gone when it claims (finished elsewhere, or cancelled) does nothing.

### The locking contract AL relies on, the PostgreSQL way

We don't emulate SQL Server's locks (no lock escalation, no shared read locks, no READPAST). What
AL business code observably relies on, and how bc-rust provides it:

| AL | What the code relies on | bc-rust on PostgreSQL |
|---|---|---|
| plain reads | a consistent committed view, never blocked by writers | MVCC, READ COMMITTED |
| `LockTable()` then reads | rows read stay unchanged by others until commit; a re-read sees the latest committed version | `FOR UPDATE` on every row read from the table in this transaction; per-session caches bypassed |
| `LockTable()` / `UpdLock` + `FindLast` + `Insert(last + 1)` (legacy Entry No.) | two sessions never compute the same next number | a locked read from the end of a key range first takes `pg_advisory_xact_lock` on that range's end (table, key, equality filters of the key's leading fields), like a key-range lock on SQL Server. Appends to one range run one after the other and the second reads the first's committed row; appends to other ranges (another order's lines) don't wait |
| `ReadIsolation(IsolationLevel::UpdLock)` (No. Series GetNextNo, job queue semaphores) | the row read is locked until commit | row locks as after LockTable, for that table |
| other isolation levels | — | MVCC reads (nothing needs dirty reads or shared locks) |
| AutoIncrement | unique numbers across sessions (SQL IDENTITY), a rolled-back number is not reused | a PostgreSQL sequence per table (`recops::identity_next`) |
| `NumberSequence` | non-transactional numbers | PostgreSQL sequences (already so) |
| Modify / Delete / Rename of a stale record | another session's change since the record was read is not overwritten: "Another user has modified the record for this %1 after you retrieved it from the database. ..." (DB:RecordChanged) | off by default (`BC_STALE_CHECK=1` enables it): locked read of the stored row, compared with the version the record carries; see "Stale records" |
| deadlocks | an error in one of the sessions: "This transaction deadlocked with another session on table %1 and was rolled back. Try again." (own wording, docs/sources.md row X22) | PostgreSQL's deadlock detector, mapped to BC's text; a task retries it as a transient error |
| lock waits | a session waits at most the lock timeout (10 s by default), then fails: "Could not save: a record in table '%1' is locked by another session's transaction. ..." (own wording, docs/sources.md row X22) | `SET lock_timeout` on every session connection (`BC_PG_LOCK_TIMEOUT_MS`, default 10,000; 0 waits without limit), SQLSTATE 55P03 mapped to BC's text with the table of the waiting statement |

All of this is on by default for every PostgreSQL backend (bc serve, bc-nst, the bc tools), with or
without the pool; `BC_PG_TABLE_LOCKS=0` turns off the range-end locks, the UpdLock row locks and
AutoIncrement from sequences (the lock timeout has its own setting). `Database.LockTimeout(false)`
lets the session's transactions wait for locks without limit, and `LockTimeoutDuration(seconds)` sets
the session's timeout (one value with LockTimeout, as in BC; 0 or less is no limit): `SET LOCAL
lock_timeout` at BEGIN, so a pooled connection never keeps it (since I15). Without the contract, concurrent
API sessions posting at the same time break: in the
load test, 2 parallel API clients posting sales orders failed 30% of the time with "savepoint sp1
does not exist". Behind that error were duplicate Item Ledger Entry numbers (LockTable +
FindLast), duplicate posted document numbers (UpdLock ignored), and duplicate Job Queue Log Entry
numbers (AutoIncrement as table maximum + 1). With the contract on: 0 errors.

A first version locked the whole table at LockTable (one advisory lock per table, held to commit).
That serialized too much and deadlocked concurrent order entry (Sales Line against table 5495 in
opposite orders; 444 of 600 requests failed). The range-end lock replaced it.

The posting ledgers' ends share one lock (I14, 2026-11-09): the range ends of G/L Entry, G/L
Register and Item Ledger Entry (whole table, primary key) are one advisory lock (`lock_range_end`).
BC takes them in different orders: journal posting locks G/L Register first
(Gen. Jnl.-Post Batch.FindNextGLRegisterNo), document posting G/L Entry first (legacy
Sales/Purch.-Post LockTables, Gen. Jnl.-Post Line.InitNextEntryNo), and item and G/L posting meet
on Item Ledger Entry and G/L Entry in both orders. On SQL Server that is BC's deadlock error on
the G/L Entry table under concurrent posting,
and a deadlock inside a background posting is caught by codeunit 88's own `Codeunit.Run`, so its
job queue entry ends in Error. With one lock, postings serialize where they first meet; G/L
Entry's lock serialized them anyway. Mixed-load A/B (6 minutes, 2 then 4 API sessions posting sales,
purchases and general journals, 60 background postings, run side by side on fresh CRONUS copies):
before, 242 of 1,992 requests failed with a deadlock, background tasks hit 21 more, 5 background
postings ended in Error, and with 4 sessions 66 sales posted per minute at a post p95 of 2.0 s;
after, 0 deadlocks and 0 errors, 295 sales per minute at a post p95 of 149 ms
(crates/bc-store/tests/pg_gl_lock_group.rs; the evidence of runs glab-A2 and glab-D2 is not in the
repository).

### Operational behaviour

- **Poll and wake.** The poller wakes every `BC_BG_POLL_MS` (default 1,000 ms) and whenever a task
  finishes (a recurring dispatcher has just created its next task). It claims at most as many tasks
  as there are idle workers, in Not Before order.
- **Timeouts.** CreateTask stores its Timeout argument in the task's Timeout field (the job queue
  passes the entry's Job Timeout, 12 hours by default); a task without one gets
  `BC_BG_TASK_TIMEOUT_S` (default 12 hours). Past it, the session's cancel flag is
  set. A background session runs with a step limit that serves as a checkpoint: every 1,024 AL
  statements the (cold) step-limit branch checks the flag and moves the checkpoint on, so other
  sessions don't pay anything per statement. A stopped session fails with an error, which runs the
  failure codeunit. A session waiting on a database lock is
  not interrupted.
- **Retries.** Transient database errors (deadlock detected, serialization failure, lock timeout)
  leave the task scheduled and release the claim, so the next poll tries again. Any other error runs
  the failure codeunit and removes the task, as in BC.
- **Recurrence.** Done by the job queue's AL (`Reschedule` creates the next task), not by the pool.
- **Crash recovery.** A killed server leaves its running tasks' rows; PostgreSQL drops its advisory
  locks and rolls back its open transactions. The next poll, on the restarted server or another
  one, runs the task again. The job queue sees an In Process entry, runs its cleanup, and marks the
  dead attempt's log entry Error ("attempt n"), because the service instance id differs. Starts are
  counted in `shared.bc_task_attempt` (bc-rust's own table). A task that has been started
  `BC_BG_MAX_ATTEMPTS` times (default 3) without finishing runs only its failure codeunit and is
  removed, so a task that crashes the server can't loop.
- **Exactly once.** A task's work is committed by its own AL. If the server dies after the task
  committed but before its row was deleted, the task runs again (at least once, as in BC). The job
  queue copes: the second run of a posting job finds the document posted and gone and ends in
  Error, without posting twice.
- **StartSession** (the global function and `Session.StartSession`, which the Base App uses) runs
  on a thread of its own at once (not queued behind tasks), at most
  `BC_BG_MAX_SESSIONS` (default 64) at a time; past that it returns false. `IsSessionActive(id)` is
  true while it runs. `StopSession(id)` sets its cancel flag.
- **Failure codeunit and log entries.** Before the failure codeunit runs, an error the runtime
  raised without a BC error code gets BC's generic "Dialog": the Base App's Job Queue Error
  Handler logs the error only when GetLastErrorCode() is not empty (Error Message.LogLastError).
  Checked: "Codeunit 0 does not exist" and a runtime error inside the job both reach the entry's
  and the log entry's Error Message (they were empty).
- **ReadIsolation** takes the locks in both forms, `Rec.ReadIsolation(IsolationLevel::UpdLock)` and
  `Rec.ReadIsolation := IsolationLevel::UpdLock`.
- **Logging.** Failures go to stderr (journal) with the task id and codeunit; every minute with
  activity, a summary line (tasks ok/failed/retried, sessions, running).
- **Several server processes** on one database can run pools together; the advisory lock keeps
  each task in one place (tested with one process at a time, killed and restarted).

### Configuration

| Setting | Default | Meaning |
|---|---|---|
| `--bg-workers N` / `BC_BG_WORKERS` | 0 (off) | task worker threads (BC: TaskSchedulerMaxConcurrentRunningTasks), in bc serve and bc-nst |
| `BC_BG_POLL_MS` | 1000 | poll interval |
| `BC_BG_TASK_TIMEOUT_S` | 43200 | timeout of a task without its own |
| `BC_BG_MAX_ATTEMPTS` | 3 | starts before a task is given up |
| `BC_BG_MAX_SESSIONS` | 64 | concurrent StartSession sessions |
| `BC_PG_TABLE_LOCKS=0` | on | turns the PostgreSQL locking contract off (range-end locks, UpdLock row locks, AutoIncrement from sequences) |
| `BC_PG_LOCK_TIMEOUT_MS` | 10000 | lock timeout of every PostgreSQL session (BC's default); 0 waits without limit |

`TaskScheduler.CanCreateTask` answers true while the pool runs (outside tests), so standard code
that checks it (analysis views, telemetry, Change Global Dimensions) starts using tasks.

### al-test

al-test never starts the pool. Tasks keep running inline at `Sleep`, in the test's session, on a
savepoint, so runs stay deterministic and single-threaded. `CanCreateTask` stays false under test
isolation, as in Microsoft's CI. The memory backend's `try_lock_key` is a lock within the process,
and its `lock_table` is a no-op (writes are serialized), so none of the PostgreSQL locking applies.

The job-queue test codeunits (134663, 134893, 139018, 139019, 139020, 139026, 139027, 139030,
139032, 139033, 139189; 228 tests that Microsoft's CI runs), before and after this change: W1
206 and 206 pass, identical outcomes; Contoso 201 and 202. The one gain (Test Job Queue ::
InteractionLogEntryNotCreatedWhenRunJobQueueWithIntLogDisabled_ServiceQuote, "Service Quote Nos.
must have a value") comes from other changes in the tree between the two snapshots, because the
pool is off in al-test.

## Load test (2026-11-08, scratch databases bc_jobq*, 4 workers, busy shared host)

Driver: loadtest.py, not in the repository (HTTP against a scratch `bc serve`, checks in
PostgreSQL). "Post with Job Queue" on: API clients create sales orders (pages 42/46) and Post, which
enqueues them ("Sales Post via Job Queue", codeunit 88). Then the flag is off, and API clients
create and post orders inline while the pool drains the queue.

- **Throughput:** 600 orders enqueued in 17 s (35/s) by 4 clients while the pool was already
  posting. 600 background postings in 35.4 s, **16.9 postings/s**, every one posted exactly once
  (0 posted twice, 0 missing, 0 orders left, 600 Success log entries).
- **Interactive impact:** inline posting through the API while the pool drained: 1,566 orders in
  45 s on 2 clients, 0 errors, post median 30.9 ms, p95 46 ms. With an idle pool: median 31.6 ms,
  p95 88.6 ms. The difference is within this host's noise.
- **Concurrent inline posting (contract on):** 2 clients 28 postings/s, 4 clients 41/s, 0 errors.
  Before: 30% errors.
- **Recurring entries:** 10 entries every minute (codeunit 1509): no two runs of one entry
  overlapped.
- **Crash:** 600 more orders enqueued, then the server was killed with SIGKILL mid-drain (465
  dispatcher tasks queued, one posting In Process) and restarted 2 s later on the same database.
  The queue drained in 70 s, boot included. All 1,200 orders were posted exactly once (0 twice,
  0 missing). The killed attempt's log entry was set to Error by the job queue itself ("Something
  went wrong and the job has stopped... (Attempt 1)", because the new server has another
  ServiceInstanceId), and the rerun posted the order. No In Process entries and no start counts
  were left. The recurring entries carried on across the restart (38 runs of 10 entries in about
  4 minutes).

- **Categories:** with legacy posting (the seeded setup), every background sales posting gets
  category SALESBCKGR, so BC's dispatcher runs them one at a time: the others reschedule as Waiting
  and are activated by the one that finishes. A rerun with the final build: 297 postings in 12.3 s
  (24/s), all exactly once. On a fresh database, 3 of the first concurrent enqueues failed with
  "The Job Queue Category already exists. Code='SALESBCKGR'". That race is Microsoft's own
  (`Job Queue Category.InsertRec`: `if Get then exit; Insert`), and SQL Server gives the same error
  to the losing session.

- **Two servers on one database** (2026-11-08, 2 workers each, scratch databases bc_jobq11/12):
  - Each process got its own ServiceInstanceId from the database sequence (1 and 2; a restart got 3).
  - 800 postings were enqueued through both servers' APIs at once and posted exactly once each.
    The work split 76 / 758 / 5 runs over the first, second and restarted process.
  - Server A was killed with SIGKILL while holding a claim and 673 queued tasks. Server B ran A's
    claimed task and drained the queue in 28 s.
  - 10 recurring entries ran every minute on both processes, through the kill and the restart: 4
    runs each in 3 minutes, every entry kept its next task, no two runs of one entry overlapped,
    and all 41 log entries were Success.
  - One trap found on the way: a new entry is Ready, and every run-parameter change reschedules it
    (OnModify). An entry edited field by field while Ready therefore runs half configured, and the
    edits race with the dispatcher's own Modify. Configure entries On Hold, then Set Status to
    Ready, as on the card. BC's optimistic concurrency stops the lost update there; see "Stale
    records".

crates/al-runtime/tests/workers_pool.rs covers the pool on the embedded engine: 6 tasks on 3
workers, committed work surviving a later error, the failure codeunit with GetLastErrorText,
finished tasks removed, `Session.StartSession` returning at once, `IsSessionActive` and
`StopSession`.

## Known gaps and risks

- A session blocked on a database lock ignores StopSession and its task timeout until the lock
  comes or the 10 s lock timeout fails it.
- Database errors (lock timeout, deadlock, lost connection) are AL errors in Get, Find('='),
  Modify, Delete, Rename and the other primary-key lookups of functions that can fail (media,
  XMLports, platform rows, tasks). Two lookups in functions that can't fail (user personalization,
  a task's record) and the TestPage lookups (al-test only) still read an error as "not found".
  As a safety net, a COMMIT after a failed statement in the same transaction rolls back and fails
  ("The changes were not saved, because a database operation in the same transaction failed:
  ..."). PostgreSQL would otherwise answer it with ROLLBACK and no error.
- Single-client cost of the contract: +2 SQL statements per posting (138 → 140), see
  docs/deploy-db.md.
- A failed StartSession session only logs to stderr. BC also writes it to the event log and
  telemetry.
- The Get cache of unlocked reads works like BC's server data cache (since I14, 2026-11-09): a
  transaction that wrote a table bumps the table's commit counter when it commits, and every
  session of the process drops what it read of that table before; rollback and rollback to a
  savepoint end the cache. Before, the per-session cache ignored other sessions' commits and
  served rows hours old to long-lived web client sessions, which then wrote them back whole.
  Dropping the whole cache at every commit instead cost about 23 re-reads of setup rows per
  posting request (posting commits mid-request: 171 statements instead of 145). Another
  process's commits are seen at the next request: bc-nst ends its read caches after every call,
  and API requests and background tasks get a new session (crates/bc-store/tests/pg_tx_cache.rs).
  Inside a transaction,
  `SelectLatestVersion([TableNo])` (and `Database.SelectLatestVersion`) flush the caches for that
  table or all tables, keeping rows the transaction wrote or locked (BC:
  `NavSession.FlushDataCache`; crates/bc-store/tests/pg_flush.rs).
- `Database.LockTimeout(false)` / `LockTimeoutDuration` are not honoured: every session has the
  10 s timeout.
- A range-end lock is taken for every locked reverse scan, including Next(-1) batches: one round
  trip each (about 20 µs).
- bc-nst runs the same pool with the same settings (since I14, 2026-11-09), so "Post with Job
  Queue", scheduled tasks and job queue entries made in Microsoft's web client run there. Without
  the switch, an entry made in the web client waits for a process that runs the pool on the same
  database. Proof on a scratch CRONUS copy: the web client's Post (Ship and Invoice) of order
  101021 answered "Order 101021 has been scheduled for posting.", and the pool posted it
  (codeunit 88 log entry Success, invoice 103297, shipment 102297, order deleted).

## Enabling the pool on the live `bc` service (proposal, 2026-11-08)

Not applied; the coordinator decides. The live unit runs `bc serve --pg ... --apps standard-prod`
on database `bc`, on-premises (`IsSaaS()` false). The PostgreSQL locking contract is already
on there since I12.

**What the database has** (the 12:49 dump of `bc`, restored into the scratch database
bc_livecopy2, not the live database):

| Job queue entry | Object | Recurrence | Status | Task |
|---|---|---|---|---|
| (no description), category EDOCUMENT | codeunit 6248 E-Doc. Payment Occ. Dispatcher | every 5 min | On Hold | none |
| Update Power BI Dimension Set Entries, category PBI | codeunit 36952 Update Dim. Set Entries | every 60 min | Ready | none (System Task ID empty) |

The standard-prod install created both. The Scheduled Task table is empty.

**What would run.** Right after the switch, nothing: no task exists, and both entries need a
Restart or "Set Status to Ready" to get one. The SaaS-only task creators (Telemetry Management,
OData Initializer at login) stay off on-premises. From then on, tasks come from user actions:
- a job queue entry set to Ready, or Restart;
- Post with Job Queue, if a setup turns it on (it's off);
- sending email through the outbox;
- batch jobs that check `CanCreateTask`, such as Adjust Cost and Change Global Dimensions in
  parallel.

The first dispatcher run also creates BC's own daily "Clean up failed jobs" entry.

**Settings:** a drop-in `~/.config/systemd/user/bc-rust.service.d/bg.conf` with
`Environment=BC_BG_WORKERS=2` and `Environment=BC_BG_TASK_TIMEOUT_S=1800`. With 1,800 s, a job
that loops occupies a worker for half an hour instead of 12 hours. Everything else keeps its
defaults.

**Risks:**
- A job that hits AL bc-rust doesn't implement yet ends in Error, with a log entry and its error
  message. Nothing else is affected.
- A deploy or restart while a job runs starts that job again, at least once, as in BC. Posting
  jobs are safe (the second run finds the document posted). A custom job with outside effects (an
  email) could do them twice.
- Tasks run as the user who created them (Scheduled Task "User Name"). bc-rust enforces no
  permissions there yet.
- Two more sessions use little memory: each background session is a normal session, and the
  catalog is shared.
- bc-nst (database bc_nst) needs its own switch (the same `BC_BG_WORKERS`, a drop-in of its own);
  without it, tasks created in its sessions stay unrun there and `CanCreateTask` stays false.

**Check after the switch:**
1. The boot line reads `background tasks: 2 workers, ... service instance N`.
2. Restart the Power BI entry on page 673 and watch its Job Queue Log Entry: Success, or Error
   with a message.
3. The journal shows a summary line every minute with activity.

**Rollback:** move bg.conf away (no deletion), `systemctl --user daemon-reload`, restart. The pool
is off again; Scheduled Task rows and `shared.bc_task_attempt` stay and do no harm.

## Stale records: BC's optimistic concurrency (2026-11-08)

**Status: on by default since I14 (2026-11-09); `BC_STALE_CHECK=0` turns it off.** The W1
DemoTool false positive (PostGeneralJournalLines: `GenJournalLine.DeleteAll()` after a Find/Next
loop deleted through a copy that carried the last row's version) is fixed: a version only counts
for the row it was read from, and DeleteAll gives every row the version it was fetched with.

**BC's rule** (Ncl `DataAccess.PerformModifyAsync`, `HandleStaleRecordAsync`). A record buffer
carries the row version (SQL timestamp) it was read with. Modify, Delete and Rename compare it with
the stored version; a newer stored version is a stale record and fails with error 85132286,
GetLastErrorCode `DB:RecordChanged`. A per-session timestamp log picks the message: another
session's change gives "Another user has modified the record for this <table> after you retrieved
it from the database. Enter your changes again in the updated window, or start the interrupted
activity again."; the session's own change through another variable has its own text.

**bc-rust.** `RecState.xrow` is the row as the record variable last read it (Get, Find, Next) or
wrote it (Insert, Modify), and record copies carry it, as BC's buffers carry the timestamp. Modify,
Delete and Rename read the stored row with a row lock (`FOR UPDATE` on PostgreSQL) and compare it
with `xrow`, DateTimes rounded the way the database keeps them. If they differ, the operation fails
with BC's text and `DB:RecordChanged`. The lock means no other session can change the row between
the check and the write. Comparing the row is the version check: every Modify stamps
SystemModifiedAt, so a changed row never compares equal. It works the same on the embedded engine,
so al-test sees BC's semantics, and it needs no version column.

Not checked:
- records without a version (Init, then a blind Modify by key) and temporary records;
- ModifyAll and DeleteAll, as in BC, which updates sets without buffers;
- the session's own change through another variable, inside a trigger of the same Modify.

Every stale error uses the "another user" text; bc-rust has no timestamp log.

Three exceptions keep it to BC's behaviour:
- Only a newer stored version is stale (Ncl HandleStaleRecordAsync). A stored row that the
  session's own writes had replaced, and that is current again because those writes were rolled
  back (an error, `asserterror`, a rollback to a savepoint), is older than the record's version:
  Modify goes through. The own-writes log keeps each write's pre-image for this. Microsoft's tests
  do "Get; Modify; asserterror ...; Modify" everywhere (the I14 quick slices lost 47 W1 and 22
  Contoso tests to it before this rule).
- A newer stored row that this session wrote itself (another variable of the same record) is no
  conflict, as in BC for records read without a lock: each session remembers a fingerprint of the
  row it last wrote, per table and primary key (the oldest of 32,768 are forgotten first, never
  all at once). Without this, posting failed on its own Sales Header: the release changes it
  through another variable, and then the posting record writes it.
- ModifyAll gives every row it walks the version it was fetched with.

BC raises its "this user" error for the session's own change on a record read with a lock;
bc-rust doesn't.

Measured (2026-11-08):
- al-test A/B with the same binary, check off vs on, 32 codeunits (Sales/Purchase documents,
  General Journal, VAT, warehouse, item tracking, costing, dimensions, workflow, change log, job
  queue): W1 1,323 and 1,323 of 1,468 CI-comparable tests pass, Contoso 1,305 and 1,305, with 0
  outcome and 0 message changes.
- Posting on PostgreSQL runs 143 SQL statements instead of 140, with identical AL work.

What users see: with three API clients posting orders for the same item on a fresh database, 2 of
the first postings failed with "Another user has modified the record for this Item ..." and the
next 581 went through. Item Jnl.-Post Line (SetItemAdjmtProperties) reads the Item without a
lock, then calls LockTable, then Modify when "Cost is Adjusted" flips, so two concurrent first
postings of an item conflict; BC gives the same error there. Before the check, bc-rust silently
overwrote the other session's change.

`BC_STALE_CHECK=0` turns the check off (it is on by default, see Status above). The cost is one locked read per Modify, Delete and Rename
of a record that has a version: the unlocked read it replaces could be answered from the session
cache, which may itself be stale.

crates/al-runtime/tests/stale_rows.rs shows the lost update without the check and BC's error with
it, on the embedded engine and on PostgreSQL (BC_PG_TEST).
