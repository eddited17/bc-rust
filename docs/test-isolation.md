# Test isolation: Microsoft's test runner and al-test

Microsoft runs a test codeunit as one unit: one session, one codeunit instance, and one rollback at the end. al-test runs each test method alone. This page has the evidence and a design for running the Base App tests the way Microsoft's CI does.

Status as of 2026-11-08 09:05 UTC: `--isolation codeunit` is the default; `--isolation test` keeps the per-test model. With the binding lifetime fix (p27c), the per-method ApplicationArea restore, deterministic runs and the Microsoft CI headline (disabled tests not run), the full suites on the same binary gave: w1demo Base App 25,962 against 25,948 of 30,029 enabled tests (+154/−140, net +14), Contoso all suites 26,611 against 26,471 of 31,592 (+304/−164, net +140), and slightly faster runs. The codeunit model gains on Initialize setup that BC runs once per codeunit (SCM Kitting - D3 - Part 1: 0 → 38 passing; SCM-Assembly +42). Remaining losses, the same on both datasets: General Journal (−29), CRM integration (net −12; a Basic experience set by one test reaches the next ones in CRM Setup Test and CDS Connection Setup Test), "The object is already bound to the event subscriptions" (13) and "Requisition Line does not exist" (17). The history below is from 2026-11-07, when the codeunit model lost about 390 tests and was therefore off by default.

## What Microsoft's CI uses for the W1 Legacy buckets

- `build/scripts/RunTestsInBcContainer.ps1` sets no test runner for the Legacy test type. The only override is the UnitTest pass "DisableTestIsolation", which sets `testRunnerCodeunitId = 130451` and `requiredTestIsolation = Disabled`. `ParallelTestExecution.psm1` does the same for single RequiredTestIsolation=Disabled codeunits.
- BcContainerHelper's `Run-TestsInBcContainer` defaults `testRunnerCodeunitId` to `""` (BcContainerHelper 6.1.19 on the VM, AppHandling/Run-TestsInNavContainer.ps1 line 125). That keeps the test suite's own runner.
- The suite's runner defaults to `TestRunnerMgt.GetDefaultTestRunner()` (table "AL Test Suite", field "Test Runner Id", OnInsert), which is `GetCodeIsolationTestRunner()`: **codeunit 130450 "Test Runner - Isol. Codeunit", `TestIsolation = Codeunit`** (vendor/bcapps/src/Tools/Test Framework/Test Runner).
- 130450 calls `TestRunnerMgt.RunTests`. That runs `CODEUNIT.Run(<test codeunit>)` once per test codeunit. The platform runs OnRun and then every `[Test]` method on that one codeunit instance, and calls the runner's OnBeforeTestRun and OnAfterTestRun around each method. With `TestIsolation = Codeunit`, the database changes are rolled back when the whole test codeunit has run, not after each method.
- Inside a codeunit, a method's `[TransactionModel]` decides what happens to its changes. AutoCommit, the default, keeps them for the methods that follow; AutoRollback rolls the method back. The W1 tests use AutoRollback 1,268 times and AutoCommit 116 times. 7 test codeunits set their own `TestIsolation`.
- One test session runs many codeunits, so SingleInstance codeunits live across codeunits. "AL Test Runner Reset Environment" (codeunit ALTestRunnerResetEnvironment) resets selected state when a suite starts.

## What al-test does

`run_case` (crates/bc-server/src/bin/al-test.rs) works per test method:
- a new `Session` (so SingleInstance codeunits, bound subscribers and registered table connections start fresh);
- a new codeunit `Instance` (so globals such as `IsInitialized` are reset, and every test runs Initialize in full);
- OnRun before every test method, not once per codeunit;
- `Session::rollback()` after every method, so no test sees another test's data;
- `[TransactionModel]` is ignored, since the rollback is per method anyway.

## How the two differ

- **Initialize runs every time in al-test.** In BC the first test pays for it, and later tests reuse its setup data and its globals: lazily created number series, the posting setup it fixed, `LibraryVariableStorage` contents. Tests that only pass because an earlier test in the same codeunit created something fail in al-test. Tests that fail in BC because of leftovers pass in al-test.
- **Commit() inside tests.** Many Initialize procedures end with `Commit()`. With BC's codeunit isolation the commit stays inside the isolation and is undone at the end of the codeunit. al-test rolls back per test.
- **SingleInstance and session state** (table connections, as P9 found; bound subscriptions, the Test Runner's state) last a whole codeunit or longer in BC, but only one test in al-test.
- **Run time.** al-test repeats Initialize for every method, which costs time.

## `al-test --isolation codeunit` (implemented)

What it does (`run_method` and `worker` in al-test.rs):
- One `Session` and one codeunit `Instance` per test codeunit. OnRun runs once, before the first selected method, and the methods follow in order, sharing globals, single instances and bound subscribers.
- Before each method a savepoint is taken and becomes the test's `commit_mark` (in tests `Commit()` is already a savepoint). A failing method rolls back to its last Commit. A `[TransactionModel(AutoRollback)]` method rolls back to its start. AutoCommit and None keep the changes for the following methods.
- At the codeunit end `Session::rollback()` returns the company to the copy-on-write snapshot.
- The step limit is re-armed per method, and UI handlers are per method (`TestCtx` per method).
- On a watchdog timeout, the codeunit's remaining tests are recorded as errors ("not run: the codeunit was abandoned after ... timed out"). That matches BC, where the codeunit's test session is gone. Rerunning them on a new instance would hide the hang's effect.
- `--seed-random` seeds Random() before every method from the codeunit and test name, in both models. `Randomize(Seed)` is still a no-op, so the default path is unchanged. Making it seed, as in BC, is the next step: `LibraryRandom.SetSeed` and `Any.SetSeed` use it.

The original design notes:
1. Keep one `Session` and one `Instance` per test codeunit (a group in al-test). Run OnRun once, then the `[Test]` methods in declaration order, as BC's platform does.
2. Take a savepoint at the start of the codeunit, and roll back to it when the codeunit is done. Inside, `Commit()` must not end the isolation. It can become a savepoint, as StartSession already does (`Session::bg_depth`).
3. Per method: take a savepoint first. Roll back to it when the method's TransactionModel is AutoRollback or the method failed. A failed method's changes are undone in BC because its error rolls the transaction back. With AutoCommit and success, keep the changes for the next method.
4. Reset the test context (UI handlers, `TestCtx`) per method, but keep the instance's globals and the session's single instances.
5. The watchdog (`--test-timeout`) then abandons a whole codeunit's session. The rest of that codeunit must restart from OnRun on a new worker.

A/B plan: run 4–5 large areas (ERM, SCM, SCM-Warehouse, ERM-Sales+SMB, Misc) with `--isolation codeunit` and with per-test isolation, on both datasets, using the same binary. Compare the test-level gains and losses, and make the codeunit model the default if it wins, as it is what Microsoft's CI runs.

## A/B (2026-11-07)

Six areas (ERM, SCM-Warehouse, ERM-Sales, SMB, CRM integration, Workflow; 10,834 tests), `--seed-random --jobs 8`, one binary for both models (`analyze.py`):

| Dataset | `--isolation test` | `--isolation codeunit` | Net | Gained | Lost |
|---|---:|---:|---:|---:|---:|
| w1demo | 8,121 | 7,727 | −394 | 54 | 448 |
| contoso | 7,813 | 7,421 | −392 | 84 | 476 |

Per area on w1demo: ERM −118, SCM-Warehouse −188, ERM-Sales −75, SMB −14, Workflow −2, CRM integration +3. Contoso has the same shape.

The codeunit model runs the tests in 45 s against 624 s on 8 jobs, because Initialize runs once per codeunit.

The top loss causes (w1demo):
- item tracking "Qty. to Handle (Base) in the item tracking assigned to the document line ... must be" (about 165);
- errors with an empty message (66, probably `Error('')` from journal template selection now that earlier tests' templates remain);
- `The TestPage "G/L Posting Preview" is not open` and `GenJnlPostPreview.IsActive() before preview` (34): the SingleInstance posting preview state left by a failed preview;
- `OnBeforeSendICDocument should not be called` (16).

What the gains had failed on: missing setup that an earlier test in the codeunit creates (Microsoft Dynamics 365 Connection Setup, Payment Registration Setup, Warehouse Journal Template).

## What Microsoft's runner does before every method

The test runner raises "Test Runner - Mgt".OnBeforeTestMethodRun before each test method. Codeunit 130453 "ALTestRunner Reset Environment" subscribes to it:
- it calls ClearLastError and `ApplicationArea('')`;
- it runs codeunit 130301 "Reset State Before Test Run": **`Library - Random.SetSeed(1)`**, `RunLegacyPermissionSet` (the permission test catalog) and `ClearTemporaryNotificationContext`;
- it runs codeunit 130302 "Set Permissions State Before Test Run", unless the method has `TestPermissions = Disabled`;
- it binds codeunit 132553 "Block Changes to System Tables".

After each codeunit it restores WorkDate and the application area.

So in Microsoft's CI every test method starts with Random seeded to 1. `Library - Random` is SingleInstance and also calls `SetSeed(1)` on first use. bc-rust's Random() draws from UUIDs, and its `Randomize` is a no-op, which is one source of al-test's run-to-run noise.

`al-test --bc-runner` (opt-in) emulates the reset without its permission part, before every test method in both isolation models:
- ClearLastError;
- `ApplicationArea('')`: al-test restores the session's application area after every method instead, as BC's NavTestCodeunit does (it saves `ApplicationAreas` before each method);
- `Library - Random`.SetSeed(1), with `Randomize(Seed)` seeding Random() on that thread;
- `Library - Notification Mgt.`.ClearTemporaryNotificationContext;
- codeunit 132553 when it is loaded.

A first version that ran all of 130301 cost 682 tests on w1demo and 819 on contoso ("The Permission Set does not exist"; see the next section).

## The permission part of the runner reset (gap)

Update 2026-11-08 14:43 UTC: the Permission Set rows and the PermissionTestHelper state exist now, and `al-test --runner-permissions` / `--enforce-permissions` run this part (opt-in). Design, enforcement rules and measurements: [permissions.md](permissions.md). The text below is the original gap analysis.

Codeunit 130301's `RunLegacyPermissionSet` and codeunit 130302 "Set State Test Run" set up test permissions:
- 130301: `Permission Test Catalog`.InitializePermissionSetForTest(TestPermissions::Disabled), which calls `Library - Lower Permissions`.StartLoggingNAVPermissions(), which calls `Permissions Mock` (131006).Start(). That creates the DotNet `Microsoft.Dynamics.Nav.Runtime.PermissionTestHelper`.
- 130302 (only for methods whose TestPermissions is not Disabled): InitializePermissionSetForTest(Restrictive), which calls StartLoggingNAVPermissions('D365 Full Access') and then PushPermissionSetInternal. That runs `PermissionsMock.Set(RoleID)`: Clear, then `Assign('All Objects')` and `Assign(RoleID)`, plus `Assign('TEST TABLES')`, `Assign('D365 BASIC')` and `'LOCAL'` when it exists.
- `PermissionsMock.Assign(RoleID)` does `PermissionSet.Get(RoleID)` on the legacy table **"Permission Set"** (2000000004) and then `PermissionTestHelper.AddEffectivePermissionSet(RoleID)`. During the test, `Library - Lower Permissions`.SetO365Basic, PushPermissionSet, AddPermissionSet and friends go through the same Set and Assign calls.

So bc-rust needs two things from the platform:
1. **"Permission Set" rows for the PermissionSet objects of the loaded apps**, with Role ID = the permission set's name ('All Objects' from the Permissions Mock app, 'D365 Full Access', 'D365 BASIC', 'TEST TABLES' and the other AL permission sets). It could be a catalog-computed table, like AllObj or Application Object Metadata. Today it has no rows, hence the 682 (w1demo) and 819 (contoso) "The Permission Set does not exist. Role ID=..." errors.
2. **A `PermissionTestHelper` emulation** (DotNet; constructor, Clear, AddEffectivePermissionSet, Dispose). A recording no-op would do, since bc-rust doesn't enforce permissions.

Real permission checks (tests that expect a permission error under `TestPermissions = Restrictive`) are a separate, larger gap. The runner's after-test hook `GetPermissionErrors` (the "must change permissions" check) also isn't emulated. Permission Set Link, Tenant Permission Set and Access Control aren't used by these calls.

## Round 5 A/B: `--bc-runner` without the permission part (2026-11-07 22:50 UTC)

Same six areas, one snapshot binary, `--jobs 8`. Baseline `--seed-random` against `--bc-runner`:

| Codeunit model | `--seed-random` | `--bc-runner` | Net |
|---|---:|---:|---:|
| w1demo | 7,979 | 7,969 | −10 (24 gained, 34 lost) |
| contoso | 7,669 | 7,664 | −5 (25 gained, 30 lost) |

So the permission-free reset is neutral in the codeunit model. It doesn't close the gap to the per-test model. These leaks remain (w1demo, tests that pass per test but fail in the codeunit model):
- item tracking "Qty. to Handle (Base) ... must be" (173): SCM Warehouse - Receiving 41, SCM Warehouse Management II 25, SCM Warehouse - Shipping 17, SCM Warehouse IV 16;
- empty-message errors (66): `Error Message Management`.StopTransaction raising `Error('')` in O365 Correct Purchase/Sales Invoice, Sales Correct Cr. Memo and ERM Sales Document;
- posting preview (20, Test Sales Post Preview) and `GenJnlPostPreview.IsActive()` (14);
- `OnBeforeSendICDocument should not be called` (16, ERM Intercompany III).

**Likely common cause: BindSubscription lifetime.** In BC a manual subscription ends when the bound codeunit instance goes out of scope, for example a local `ErrorMessageHandler` or a posting-preview subscriber variable. bc-rust keeps every bound instance in `Session::bound` (a `Vec<Rc<Instance>>`) until `UnbindSubscription`. In the codeunit model, which keeps one session, earlier tests' handlers stay bound. Error-message collection then stops with `Error('')`, preview and IC subscribers keep firing, and so on. The per-test model hides this because every test gets a new session. The fix is to hold `Weak<Instance>` in `Session::bound` and skip dead entries. It touches three sites: interp.rs:177 (the field), builtins.rs:502–515 (bind/unbind) and interp.rs:1140 (event dispatch). It changes the default path too, so it needs its own A/B.

## Deterministic runs are the default (2026-11-08)

`--bc-runner` was net-neutral with `--isolation test`, against `--seed-random` on the same binary and the same six areas: w1demo 8,383 → 8,372 (−11; 11 gained, 22 lost) and contoso 8,073 → 8,062 (−11, −0.10%; 8 gained, 19 lost). Every difference came from different random test data: rounding precision, min/max quantities, serial-number quantities.

So since 2026-11-08 al-test does this by default, before every test method, in both isolation models:
- Microsoft's runner reset (above): Random() is seeded to 1 through `Library - Random`.SetSeed(1).
- The demo company's NumberSequences go back to the snapshot (`demo::restore_sequences` with `Master.seqs`): before every test with `--isolation test`, before every codeunit with `--isolation codeunit`. NumberSequences are not transactional, as in BC, so a sequence that an earlier test in the same worker deleted or advanced would otherwise carry over (P9: "Contact already exists CT0000xx" in Contoso's sequence-based No. Series). Within a test they stay non-transactional.

The summary header says `bc-runner: on`. `--no-bc-runner` gives the old behaviour, with Random() from UUIDs and sequences carried over, for comparisons with runs before 2026-11-08.


## The same random sequence as Microsoft's CI (2026-11-08)

P11 ported .NET's seeded `System.Random` (Knuth's subtractive generator, `NetRandom` in builtins.rs; unit test `crates/al-runtime/tests/p11_net_random.rs`). BC's `Randomize(Seed)` creates `new System.Random(Seed)`, and `Random(n)` is `Next(n) + 1`. Random() uses the port once seeded, and unseeded calls still draw from UUIDs.

With the default `--bc-runner`, which runs `Library - Random`.SetSeed(1) before every test method, al-test's test data are now the random sequence Microsoft's CI generates, not merely a deterministic one. Expect outcome shifts in the first full run with the port (I7); I6's snapshot predates it.

The seed modes:
- **default (`--bc-runner`)**: seed 1 before every method, as Microsoft's runner does. This is the CI-faithful mode.
- **`--seed-random`** (with `--no-bc-runner`): a seed hashed from the codeunit and test name. It is deterministic but not CI-faithful; use it only to compare with runs from before 2026-11-08.
- **`--no-bc-runner` alone**: Random() from UUIDs, the old non-deterministic behaviour.

## Page state between methods (2026-11-08)

With codeunit isolation the methods share a session, so state a page leaves behind reaches the next methods. Three page rules decide what carries over. They come from the NST (`NavTestPageBase.Open`, `NavForm.RunAsync`, `NavForm.StoreSaveValues`):

- **Each open is a new page.** `OpenEdit`/`OpenView`/`OpenNew` on a TestPage that was opened and closed before starts over: a new page instance with its own globals, Rec, filters and extensions. BC's `Open` clears the TestPage and the client creates the page again. bc-rust used to keep the old instance, so a reopened General Journal skipped its template selection. ERM TestMultipleGenJnlLines left a template name in `Library - Variable Storage`, and the codeunit's later methods failed on it (General Journal +12 per dataset).
- **OnInit runs before "Unhandled UI".** BC creates the page object, whose constructor runs OnInit, before it looks for a trap or handler. A page with no trap or handler still runs OnInit, and OnInit can fail. CRM Setup Test `CannotOpenPageIfAppAreaBasic` expects `CheckAppAreaOnlyBasic`'s error from OnInit. While it failed, it never reached its `ClearApplicationAreaCache()`, and the Basic experience it left in the SingleInstance Application Area Cache failed 22 later CRM/CDS setup tests per dataset.
- **A filter change starts a new row on an empty page.** When a journal's or worksheet's new batch name leaves an untouched new row outside the view and the view is empty, the page starts a new row from the new filters, as BC's client replaces the draft. bc-rust kept the old row, so Rec stayed on the old batch: Req. Worksheet's Calculate Plan planned into the batch SaveValues had restored (SCM Inventory Miscellaneous TransferOrderCreatedFromRequisitionWorksheet; full A/B +9 per dataset, with ERM General Journal UT, Job Journal and ERM Dimension Priority tests).
- **CurrPage.Close() closes the page.** It used to do nothing, so a page whose action closes itself and runs again (a journal's simple/classic view switch) stayed open, and the test's next OpenEdit failed with "The TestPage is already open". The page now closes, with OnQueryClosePage and OnClosePage, once the action returns.
- **SaveValues.** A page with `SaveValues = true` keeps the page variables its controls show. They are saved when the page closes, unless with Cancel or No, and set again on the next open, after OnInit and before OnOpenPage. BC stores them per user and page in the database; bc-rust stores them per session and page (`SysState::page_saved_values`). Examples: Create Payment's batch, Standard Cost Worksheet's name. A page variable shown in a read-only control is saved too (BC saves every source expression with a setter), so a journal's approval status comes back with the page; the journal recomputes it in OnAfterGetCurrRecord, which runs on an empty page opened for viewing as well (Service `NSFormApplicationCode`).

## Deterministic clock and Guids (`--clock virtual`, 2026-11-08)

With Random() seeded and the number sequences restored, the flips left between two runs of one build came from the clock and from Guids:

- **Strictly later timestamps.** Email Retry Test asserts `EmailOutbox."Date Queued" > ScheduledDateTime`, where both are CurrentDateTime readings taken a few statements apart. Two readings in the same millisecond fail it.
- **Gaps measured on the clock.** TypeHelper.CompareDateTime treats DateTimes less than 10 ms apart as equal, so CRM/CDS synch tests that check "modified after the last synch" depend on how fast the host gets from one stamp to the next. AutoResolveDeletedRecord* needs its Sleep(20) to separate SystemModifiedAt values.
- **Guid order.** Guid keys are random, so FindFirst/FindLast and sort order on them change from run to run (CRM Statistics Job Test, the MCP configuration tables), as they do in BC.
- **Time() and the date.** `LibraryRandom.Init()` seeds Random() from Time in six codeunits, a run that crosses midnight changes Today() halfway, and job queue and schedule logic compares against the time of day.

`al_runtime::clock` is the one source of CurrentDateTime, Time, Today, Sleep, CreateGuid, SystemId, SystemCreatedAt/SystemModifiedAt, Record Link timestamps, TaskScheduler due times, notification and media ids, and .NET DateTime.Now/UtcNow/Today and Guid.NewGuid. Stopwatch and Environment.TickCount stay on the real clock, since they measure durations. By default it is the wall clock with random version 4 Guids, as in BC; `bc serve` and bc-nst always use that. A thread can switch to a deterministic mode:

- **A virtual clock.** Every reading is at least 1 ms after the previous one, so time never stands still or goes back. Work moves it on at 10 µs per AL statement, so a 14,000-statement posting takes 140 ms, the order of BC on SQL Server, and gaps like CompareDateTime's 10 ms come out as they would in BC rather than depending on the host. Sleep(ms) moves it on by ms without waiting, so a test's Sleep(60500) costs nothing. Today() and Time() are the local date and time of the virtual clock. Reading Today() or checking due tasks does not move it.
- **Seeded Guids.** A SplitMix64 stream gives valid, distinct, random-looking version 4 / RFC 4122 values.

`al-test --clock virtual` starts both at the start of every isolation unit, that is, every codeunit (or every test with `--isolation test`). The clock starts at 10:00 local time on the run date, and the Guid stream starts from a seed hashed from the codeunit id (and test name). Within a unit the clock keeps running and the stream goes on, so a codeunit's later methods see later times than its earlier ones and never repeat a Guid. The demo company is built on a virtual clock that starts at 01:00 on the run date, with its own seed, so its rows are older than anything a test creates. Its cache key covers the clock mode. Two runs on the same day see the same timestamps and Guids in every test. On another day only the date differs, as it does for Today() in Microsoft's CI. The summary header says `clock: virtual` or `clock: wall`.

Hooks: `al_runtime::clock::{set_virtual, set_wall, now_ms, peek_ms, sleep, new_guid, local, local_to_ms}`. al-test calls `set_virtual` in `start_unit_clock` (worker, per unit), in the company build thread and before a worker builds its own database (`--data seed|none`). The rest are the call sites listed above. Tests: `crates/al-runtime/src/clock.rs` (unit) and `crates/al-runtime/tests/stab12_clock.rs` (AL readings, Sleep, Today, CreateGuid and the repeat with the same seed).

**Status (2026-11-08 15:20 UTC):** opt-in (`--clock virtual`). `--clock wall` stays the default until two identical full runs per dataset show identical outcomes and no new failures; those runs wait until the host is free after I12. Evidence so far, all on fc41a44 + this change, each codeunit run twice with `--clock virtual`:

| Codeunit (dataset) | I11, wall clock | virtual, run 1 / run 2 | between the two virtual runs |
|---|---|---|---|
| Integration Table Synch. Test (w1demo) | 39 / 54 | 47 / 47 | identical outcomes and AL step counts |
| Email View Policy Tests (contoso) | 20 / 27 | 23 / 23 | identical |
| Email E2E Tests (contoso) | 11 / 16 | 13 / 13 | identical |
| ERM Financial Report Schedules (w1demo) | 10 / 14 | 12 / 12 | identical |
| CRM Statistics Job Test (w1demo) | 6 / 13 | 7 / 7 | identical |
| Pos. Pay Test Unit (w1demo) | 13 / 14 | 13 / 13 | identical |
| Email Retry Test (contoso) | 17 / 17 | 17 / – | |
| MCP Config Test (contoso) | 86 / 102 | 85 / 86 | one flip: duplicate SystemIds, see below |

A full contoso run stopped at 32,519 of 44,430 tests (host OOM at 14:41 UTC, not this run's fault alone) had 8 gains (the Integration Table Synch. CompareDateTime tests) and 1 loss against I11. The loss is the CI-disabled Email Test ShowSourceRecordInOutbox: `Any.SetDefaultSeed()` seeds Random() from `Time() - 000000T`, so its "random" table id is now always 2487, which does not exist.

**Full run on I12 (2026-11-08 17:43-18:02 UTC, I12's al-test, contoso, `--clock virtual`)** against I12's own wall-clock full run: CI-comparable 28,071 → 28,081, 11 gained (9 Integration Table Synch. Test, Email Retry, Financial Report Schedules), 1 lost: Metrics Collection Test TimerMetrics, which brackets `Sleep(1000)` with two CurrentDateTime readings and allows ±500 ms, measured 1,970 ms. A second run of the same binary (18:02-18:24) matched the first exactly: 44,430 tests, no outcome, message or AL step-count difference. Two flaws of the first virtual clock, fixed since 18:05 UTC (not in I12):
- **Work per session.** bc-rust runs background sessions and due scheduled tasks inline, each with its own statement counter. The clock kept one counter per thread, so a reading after another session's reading charged the difference of the two counters as work. It now keeps the last counter per session (`clock::at(&session, steps)`, the Session's address).
- **Sleep overlaps the tasks it runs.** Sleep runs the due scheduled tasks first; in BC they run in parallel while the session sleeps. Sleep now ends at its start + ms or where the tasks left the clock, whichever is later (`clock::sleep_from`).

MCP Config Test still flips because bc-rust's `Insert` keeps a SystemId the record already has. BC gives every non-temporary record a new SystemId unless `Insert(RunTrigger, true)` asks to keep it (`RecordImplementation.PopulateSystemdId(onlyOverwriteIfBlank: insertWithSystemId)`). So `Copy` + `Insert` in `CopyConfiguration` leaves two configurations with one SystemId, and which one `GetBySystemId` finds depends on row order. The fix (landed after I12, 2026-11-08 16:30 UTC: `record_method` "insert" in recops.rs clears a non-temporary record's SystemId unless the second argument is true, and the stamp assigns a new one; test `crates/al-runtime/tests/stab12_systemid.rs`) gives MCP Config Test 88 / 88 with identical runs and changes nothing in ERM Sales Order, ERM Purchase Order, ERM Sales Documents III, Document Attachment Tests and Workflow Imp./Exp. Tests (797 tests, same outcomes and step counts).
