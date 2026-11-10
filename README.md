<img src="assets/icon.svg" width="64" height="64" alt="">

# bc-rust

bc-rust is the service tier of Microsoft Dynamics 365 Business Central 29.1, rewritten in Rust. Its AL interpreter runs Microsoft's own AL source unmodified (Base Application, System Application, Business Foundation, API v2.0), and it stores data in PostgreSQL instead of SQL Server. Microsoft's unmodified web client connects to it through `bc-nst`, and bc-rust also serves a web client of its own.

## Live demos

Both demos are on the maintainer's tailnet, not on the public internet. They hold Microsoft's CRONUS demo company, built by Microsoft's Contoso demo tool, and the 89 apps of a BC 29.1 W1 sandbox.

- https://bc-msclient.rockitpro.ai/BC/ is Microsoft's web client on bc-nst. Sign in as `admin`. Password checks are off, so any password works.
- https://bc.rockitpro.ai is bc-rust's own web client.

## Status

| | bc-rust | Real BC 29.1 | Date | Commit |
|---|---:|---:|---|---|
| Microsoft's Base App tests on the legacy W1 demo company, CI-comparable | 27,724 of 30,029 pass (92.3%) | | 2026-11-10 | b41a391 |
| System App, Business Foundation and Base App tests on Contoso, CI-comparable | 28,679 of 31,592 pass (90.8%) | | 2026-11-10 | b41a391 |
| Run all 42,146 Base App tests on W1 (one 48-core machine, 10 parallel jobs) | 12.5 min, 7.2 GB peak memory | | 2026-11-10 | b41a391 |
| Run all 44,430 tests on Contoso (12 parallel jobs) | 13.3 min, 9.6 GB peak memory | | 2026-11-10 | b41a391 |
| API v2.0 with 99,467 customers: list 20 | 5.1 ms | 18.6 ms | 2026-11-10 | b41a391 |
| API v2.0 with 99,467 customers: get one by id | 4.8 ms | 18.1 ms | 2026-11-10 | b41a391 |
| API v2.0 with 99,467 customers: filter on a city, about 1% of the rows | 94.6 ms, 960 rows | 55.3 ms, 813 rows | 2026-11-10 | b41a391 |
| API v2.0 with 99,467 customers: create a customer | 9.6 ms | 32.8 ms | 2026-11-10 | b41a391 |
| Post a 1-line sales order over API v2.0 (create, add the line, post) | 93.8 ms | 246.5 ms | 2026-11-10 | b41a391 |
| Post a 10-line sales order the same way | 356.8 ms | 909.3 ms | 2026-11-10 | b41a391 |
| Microsoft's web client: sign in and show the role center | 910 ms | 1,296 ms | 2026-11-07 | before the first commit |
| Memory, idle | 1.89 GB, plus about 0.7 GB for PostgreSQL | 3.75 GiB for the service tier and SQL Server | 2026-11-08 | fc41a44 |
| Cold start to the first API answer | 1.60 to 1.75 s | about 14 s, measured on 2026-11-07 | 2026-11-08 | fc41a44 |

CI-comparable means the tests Microsoft's own CI runs, which leaves out the tests on the DisabledTest lists in BCApps. Microsoft's CI runs the Base App tests on the legacy W1 company, so the W1 row is the closer comparison. The figure assumes that every one of these tests passes on real BC. Nobody has checked that on the same data yet. The results also count flagged passes: tests that passed while touching a guessed default. At b41a391 they are 425 of the W1 passes and 467 of the Contoso passes, down from 5,862 and 5,933 at 8a1ba68.

Times are medians. For the API and posting rows both systems ran in the same Windows VM, one at a time, each limited to 2 logical processors, the default of real BC's container. bc-rust ran with sign-in and permission checks on (`BC_AUTH=NavUserPassword`, `BC_PERMISSIONS=enforce`), as a user with the permission set D365 BUS FULL ACCESS; real BC ran as a user with the same set. The 99,467 customers are real company records from the GLEIF database, loaded through the API on both sides. Real BC runs in a Hyper-V-isolated container, which adds a virtualization layer that bc-rust doesn't have. bc-rust doesn't check object Execute permissions yet. On the city filter real BC matches case-sensitively and bc-rust doesn't, so they return different row counts. The memory and cold start rows are from 2026-11-08, with bc-rust on the Linux host. The web client row uses Microsoft's web server on Linux against each service tier, with different demo data on the two sides.

Every full test run since 2026-11-07 is in [docs/parity-history.md](docs/parity-history.md). All measurements and their caveats are in [docs/performance.md](docs/performance.md).

## What works

- The AL parser reads 37,213 of the 37,214 files in Microsoft's BCApps repository (15.9M lines) in about 1 s on 48 cores. The one failure is a test file that is broken upstream.
- The interpreter runs Base App code as it is: table and field triggers, events and subscribers, interfaces, overloads, single-instance codeunits and TryFunctions. The compiled engine, the default, resolves names once per procedure ([docs/engine.md](docs/engine.md)).
- A sales order posts through Microsoft's codeunit 80 "Sales-Post". It writes the posted invoice and shipment, the customer, G/L, VAT, item and value entries, and the G/L entries balance.
- Microsoft's BC 29.1 web client runs unchanged on bc-nst: role center, lists, cards with FactBoxes, lookups, drill-downs, New/Edit/Delete, and posting with its menu and Confirm dialog. Microsoft's web server runs on Linux too (`tools/msweb-linux`).
- bc-rust's own web client renders pages from the AL page definitions, with editable line subpages and actions that run their `OnAction` trigger. The Post button on the Sales Order works.
- The REST API v2.0 runs on Microsoft's unmodified API pages: GET, POST, PATCH and DELETE with `$filter`, `$top` and `$skip`, plus bound actions such as `Microsoft.NAV.shipAndInvoice`.
- PostgreSQL holds one schema per company and one table per BC table, with a case-insensitive ICU collation. Concurrent sessions follow a locking contract, and 2 clients posting 10 sales orders each at the same time gave 0 errors ([docs/deploy-db.md](docs/deploy-db.md)).
- `--apps standard-prod` loads the 89 apps of a BC 29.1 W1 sandbox: 2,426 tables and 5,006 event subscribers. Each boot installs or upgrades apps through their own Install and Upgrade codeunits ([docs/standard-apps.md](docs/standard-apps.md)).
- TaskScheduler and StartSession run in a background worker pool (`bc serve --bg-workers N`) that claims tasks with PostgreSQL advisory locks. In a load test, each of 600 background postings ran exactly once, also when the server was killed mid-run ([docs/job-queue.md](docs/job-queue.md)).
- `demo-data` builds Microsoft's CRONUS company with Microsoft's own Contoso demo tool: 50,027 rows in 398 tables, in 11.6 s in memory ([docs/demo-data.md](docs/demo-data.md)).
- `al-test` runs Microsoft's test codeunits (1,606 codeunits, 44,430 tests) the way Microsoft's CI does: one session per test codeunit, Random() seeded to 1 with .NET's sequence, and the DisabledTest lists skipped.
- bc-rust builds and passes its workspace tests on Linux and on Windows 11 (MSVC): 235 of 235 on Windows at 8df2a2c, 2026-11-10 ([docs/platforms.md](docs/platforms.md)).

Each item in more detail: [docs/features.md](docs/features.md).

## Known gaps and risks

A peer review on 2026-11-09 found the largest risks in places the test suite can't see. Iteration I14 worked on them:

- Lost updates on PostgreSQL are fixed. BC's "Another user has modified the record" check is on by default with BC's rule (only a newer stored version is a conflict), a locked read holds its lock until commit, and the read cache drops rows that other sessions committed. A 24-minute mixed load test lost no change. API v2.0 PATCH and DELETE check `If-Match` as BC does (a stale write is 409). Document lines (for example salesOrderLines) don't yet get a new ETag after a change, so a stale line write is still accepted.
- Each session has its own user, and logins are recorded. Sign-in and permission checks are implemented but off on both demos: everyone is SUPER there, and any password works. There is no licensing check.
- Microsoft's tests run on the in-memory engine by default. `al-test --pg` runs them on PostgreSQL. The first full runs (2026-11-10, 8df2a2c) passed 27,708 tests on W1 and 28,593 on Contoso, against 27,698 and 28,579 in memory. They found 5 causes of PostgreSQL-only failures, all fixed. One was a live bug: a loop could still see rows that a second record variable had deleted or changed (63cf322). 43 text columns sort differently on the two backends ([docs/pg-vs-memory.md](docs/pg-vs-memory.md)).
- Some builtins and DotNet members still return a neutral value instead of an error. A pass that touched one is reported as a flagged pass; the largest ones now give BC's real answer ([docs/assumptions.md](docs/assumptions.md)).
- The ledger comparison with real BC covers 20 documents (`tools/oracle`, 2026-11-10). Both systems posted 19 and refused the same one. In 211 ledger-entry pairs every field matches, except where the two CRONUS companies' starting stock differs, and the currency factor, which bc-rust stores with 27 decimals and BC with 20. Item tracking and item journals aren't covered yet ([tools/oracle/README.md](tools/oracle/README.md)).
- A rare heap corruption: one abort under glibc in about 1,750 test runs since 2026-11-08, and four mimalloc crashes before that. AddressSanitizer runs over about 15,000 tests found nothing. The cause is open. Full test runs and both demo servers run with `tools/crashcap`, which writes every thread's backtrace if it happens again.

Other gaps:

- Of the tests Microsoft's CI runs, 9.5% still fail on Contoso and 7.8% of the Base App tests on the W1 company. A missing builtin fails with `Not implemented in bc-rust: <name>`.
- DotNet types are emulated in Rust for what the loaded AL uses. Other DotNet types return neutral values, and HttpClient requests are blocked unless `BC_RUST_HTTP=allow` is set.
- Reports produce their dataset. PDF, Word and HTML output hold the dataset as text. Only RDLC layouts saved as Excel go through a layout engine ([docs/reports.md](docs/reports.md)).
- bc-rust's own web client doesn't run `OnOpenPage`, `OnAfterGetRecord` or page variables, and has no FactBoxes, lookups or filter pane. Neither web client has personalization. In Microsoft's web client the headline area of the Business Manager role center is empty.
- API v2.0 has no `$expand`, `$select`, `$orderby`, nested parts or webhooks.
- bc-nst runs the background pool only when `BC_BG_WORKERS` is set.
- `Rename` doesn't cascade to related tables. FlowFields are summed from the ledger on every read, with no SIFT totals.
- PostgreSQL round trips dominate posting time, and inserts aren't batched. The embedded engine's WAL stores every field of every row.
- A test that fails with a global TestPage open leaves it open for the next tests. Two runs of one build differ in a few tests that compare timestamps.
- The added apps' own tests haven't been run. macOS builds type-check but have not been linked or run.

The full list, with the failing test groups, is in [docs/features.md](docs/features.md).

## Progress

Newest first. Pass rates are the CI-comparable figures of the full runs on the W1 company and on Contoso. Times are UTC. I1 to I7 predate the public repository. Commit IDs in the Status table and in rows dated before the clean-room row refer to the repository's earlier history, which was withdrawn on 2026-11-10; the current history starts with a single commit.

<!-- New rows go at the top: one row per landing, with the UTC time and the commit. -->

| Date | Iteration | What changed | W1 / Contoso | Commit |
|---|---|---|---|---|
| 2026-11-10 13:00 | Clean-room | The date formula module and the Excel number format conversion are rewritten from public sources and probes on a licensed BC 29.1. Microsoft texts are reworded, the .NET shim is built from dotnet/runtime source, the own web client uses its own branding, and the repository history starts again. Not redeployed yet. | | first commit |
| 2026-11-10 07:58 | I15 live | Both demos run b41a391. The click-through passed 70 of 70 steps, including a posting, and a 30-minute watch was clean. Crash capture is on, and PostgreSQL ends transactions left idle for 15 minutes. | | b41a391 |
| 2026-11-10 07:20 | I15 batch 2 | 8 commits, none rejected. API v2.0 errors use BC's body and codes, and API and background sessions report their client type. `LockTimeoutDuration` is honoured. More guessed defaults answer from the tables (user account, call stack). al-test's Contoso company gets the legacy W1 setup it lacks. Two TestPage fixes. Final runs: 0 must-pass tests lost; flagged passes down from 5,862 to 425 (W1) and from 5,933 to 467 (Contoso). A full PostgreSQL run showed no backend differences. | 92.3% / 90.8% | 5c0969a to b41a391 |
| 2026-11-10 05:34 | I15 batch 1 | 12 commits through the new landing gate (workspace tests, quick slices on both datasets, a PostgreSQL slice; one patch rejected). API v2.0 PATCH and DELETE take `If-Match` as BC does (a stale write is 409). The servers survive a PostgreSQL restart, and a cold boot no longer races the schema sync. `LockTimeout(false)` is honoured. AL decimals keep 18 significant digits, as BC's do. Five guessed defaults now give BC's answer. Saved request-page values are restored where BC restores them. Crash capture (`tools/crashcap`). The ledger oracle covers 40 documents. | | 71a6c09 to 06d6612 |
| 2026-11-10 03:30 | I15 starts | Plan by a second peer review: the heap corruption, write-path robustness, guessed defaults, the real-BC oracle, cheap test fixes, plus one stream that runs the gates and full runs. | | |
| 2026-11-10 02:53 | I14 live | Hotfix on both demos: the PostgreSQL loop fix (63cf322), and `Get(RecordId)` with another table's RecordId no longer aborts the transaction (ffd9f0c). The click-through passed 68 of 68 steps, and a 15-minute watch was clean. | | ffd9f0c |
| 2026-11-10 02:23 | I14 | On PostgreSQL, a `repeat ... until Next() = 0` loop could still see up to 50 rows that a second record variable had deleted or changed (for example, planning failed with "The Reservation Entry does not exist"). Found by the first full test run on PostgreSQL. | | 63cf322 |
| 2026-11-10 01:53 | I14 | Ledger comparison with real BC 29.1: 211 entry pairs from 20 documents match field by field, except for differences in the starting data and the currency-factor precision. Windows: 235 of 235 workspace tests pass. | | d6e8061 |
| 2026-11-10 00:11 | I14 live | Both demos run 7a23b1c. Microsoft's web client now runs background jobs (2 workers). The click-through passed 68 of 68 steps, and in a 30-minute watch memory stayed flat with no conflicts, deadlocks or stuck transactions. The PostgreSQL container got 1 GB of shared memory, after a full 64 MB /dev/shm crashed it for 39 s at 20:17. | | 7a23b1c, 2583909 |
| 2026-11-10 00:08 | I14 | The stale check's own-writes log now ends with its session. Under load it had grown server memory by about 400 MB per hour. Final full runs at 8a1ba68: +248 tests on W1 and +247 on Contoso over fe6a4ed, CI-comparable. | 92.2% / 90.5% | 7a23b1c |
| 2026-11-09 23:15 | I14 | Held-back test fixes: TestPage.Next moves onto a new row as in BC (21 approval tests no longer loop to the step limit), AssertEquals with BlankZero, TestPage saves and renames records, a blank date in filter text, option captions through a Variant. The request-page options change (669dc33) is reverted: it cost 6 report tests. | | abbc8f8 to aa43da9 |
| 2026-11-09 23:07 | I14 | The stale-record check follows BC's rule: only a newer stored version is a conflict. Its false positives in Microsoft's tests are gone (47 on W1 and 22 on Contoso before). In a 4-writer test, no change was lost silently. | | a03e29f |
| 2026-11-09 23:04 | I14 | API v2.0 no longer drops input. Page-variable fields such as `currencyCode` are served, properties apply in page control order (currency, line discounts, purchase lines), journal lines are stored, line PATCH works, and journal lists are filtered by template. | | 49adb06 |
| 2026-11-09 22:12 | I14 | Each session has its own user. `UserSecurityId()`, SystemCreatedBy and SystemModifiedBy name the signed-in user, and logins are recorded in User Login. IsolatedStorage stores per scope and encrypts `SetEncrypted` values at rest. Permission checks (opt-in) run after the triggers, as in BC. | | 6c7b852, 09b4ceb, 45150b2 |
| 2026-11-09 21:51 | I14 | A locked read on PostgreSQL starts the transaction, so its row lock holds until commit. 10-minute mixed load test: 0 silent overwrites, against 10+ in 11 minutes without it. | | 2ec03c2 |
| 2026-11-09 21:47 | I14 | Test results now say when a pass relied on a guessed default (flagged pass) or on a swallowed "not implemented" error. Must-pass lists per dataset (`tools/ratchet`). Decimal overflow raises BC's error instead of crashing. A GitHub CI workflow was added. | | 71104f1 |
| 2026-11-09 21:44 | I14 | Four held-back interpreter fixes: `RecRef.RecordId`, saved request-page options, `Clear(Variant)`, `[TryFunction]` event publishers. Slice A/B: +75 tests on each dataset, none lost. | | 91d7721 to b75c523 |
| 2026-11-09 21:40 | I14 | The read cache is now invalidated by any session's commit to the same table. Posting a sales order costs 148 SQL statements, against 145 before the write-safety change. | | a57b43f |
| 2026-11-09 21:37 | I14 | `al-test --pg` runs Microsoft's tests on PostgreSQL. On a 79-codeunit slice (3,592 tests) both backends now give the same outcomes. Two savepoint bugs it found are fixed. | | c81788f |
| 2026-11-09 21:30 | I14 | `tools/oracle` posts the same documents on bc-rust and on real BC and compares the ledger entries field by field. The bc-rust half ran (19 of 20 documents); the real-BC half is pending. It found 9 API v2.0 input gaps, which are being fixed. | | 8059cef |
| 2026-11-09 21:18 | I14 | Concurrent posting no longer deadlocks on PostgreSQL. The G/L Entry, G/L Register and Item Ledger Entry number ranges share one lock. Load test with 2 to 4 sessions: 242 of 1,992 requests deadlocked before, 0 of 4,095 after. | | b4ac20c |
| 2026-11-09 21:18 | I14 | Microsoft's web client gets the background job-queue pool (`BC_BG_WORKERS`, off by default). "Post with Job Queue" now posts. | | 016786b |
| 2026-11-09 21:13 | I14 | BC's "Another user has modified the record" check is on by default on PostgreSQL, and the per-session read cache ends with the transaction. | | 76d5ea4 |
| 2026-11-09 20:14 | I14 starts | Plan from a peer review: lost updates, per-user identity, Microsoft's tests on PostgreSQL, a load and crash soak, background jobs behind Microsoft's web client, held-back test fixes, a check that no test may get worse, a ledger comparison with real BC, a Windows check. The last two are not done: the VM never got enough memory. | | |
| 2026-11-08 21:21 to 23:52 | after I13 | Both demos switch to Microsoft's full CRONUS company. Object names resolve as the AL compiler does. Web client fixes on the complete company, page background tasks, role center cues. | 91.4% / 89.7% | c8b3e76 to fe6a4ed |
| 2026-11-08 19:30 | I13 | TestPage saves and traps, SignedXml, sign-in (off by default), stale-record check (off by default). Test companies built from scratch. | 91.3% / 89.6% | a4e60b2 |
| 2026-11-08 16:30 | I12 | API v2.0 bound actions, PostgreSQL locking contract on by default, background pool (opt-in), generic plans, permission model (opt-in). | 90.6% / 88.9% | 18c449f |
| 2026-11-08 12:45 | I11 | TestPage lifecycle, a first RDLC layout engine for Excel, filters on FlowFields, DeleteAll(true) as in BC. | 89.6% / 87.4% | fc41a44 |
| 2026-11-08 09:30 | I10 | Codeunit test isolation as in Microsoft's CI. Copy-on-write rows: 25.5% less CPU per AL statement on the posting loop. | 87.8% / 85.7% | bb06348 |
| 2026-11-08 08:00 | I9 | The headline counts only the tests Microsoft's CI runs. A TestPage inserts a new record when focus leaves its key fields. BC's error codes. More legacy setup in the Contoso test company. | 85.9% / 83.0% | bee5fce |
| 2026-11-08 06:30 | I8 | Compiled engine by default. TaskScheduler. Session leak fixed: a full Contoso run peaks at 8.5 GB instead of 21.8 GB. | 84.5% / 81.4% | 84538f7 |
| 2026-11-08 03:15 | I7 | Random() follows .NET's System.Random, so test data matches Microsoft's CI. Reports save as PDF, Word, Excel and HTML. The workspace tests pass on Windows 11. | 82.1% / 78.9% | |
| 2026-11-08 00:55 | I6 | Deterministic runs: Random() seeded to 1 and number sequences reset before every test. First upgrade of the demos without a database reset. | 78.4% / 75.1% | |
| 2026-11-07 22:40 | I5 | The 89 standard apps go live on both demos. Text evaluates into a DateFormula (210 tests). Nested Validate keeps the outer xRec. | 77.5% / 74.2% | |
| 2026-11-07 20:55 | I4 | File downloads, cryptography types. A nested-Validate xRec change was reverted because it made W1 service tests recurse: Contoso net -22 tests. | 74.8% / 71.6% | |
| 2026-11-07 | I3 | The Base App tests also run on Microsoft's legacy W1 demo company, as in Microsoft's CI. Assembly and CRM integration fixes. | 74.5% / 71.6% | |
| 2026-11-07 17:40 | I2 | CRM integration, workflow and ERM fixes: 2,329 tests started passing, 129 stopped. | / 69.5% | |
| 2026-11-07 | I1 | Warehouse, assembly and planning fixes: 4,801 tests started passing, 157 stopped. | / 64.6% | |
| 2026-11-07 | before I1 | First full run of Microsoft's test codeunits. | / 54.5% | |

## Running it

```sh
# BC sources (MIT licensed, BCApps main = v30)
git clone --depth 1 https://github.com/microsoft/BCApps vendor/bcapps

cargo build --release
docker run -d --name bc-rust-postgres -e POSTGRES_USER=bc -e POSTGRES_PASSWORD=... -e POSTGRES_DB=bc \
  -p 127.0.0.1:5442:5432 -v bc-rust-pg:/var/lib/postgresql/data postgres:17.11-alpine
echo '...' > data/pg/password                               # or set BC_PG="host=... user=... password=..."
./target/release/bc serve --pg --host 127.0.0.1 --port 8740  # seeds a demo company on first start
./target/release/bc serve --pg --apps standard-prod         # the 89 apps of a BC 29.1 W1 sandbox, installed on boot
./target/release/bc serve --db data/cronus.wal               # embedded engine instead
BC_ENGINE=tree ./target/release/bc serve --pg                # AL tree-walker instead of the compiled engine (the default)
./target/release/bc bench                                 # embedded engine micro-benchmarks (docs/performance.md)
POST=1 ./target/release/bc sales                          # create and post a sales order from the CLI
./target/release/alcheck vendor/bcapps/src                # parse-check any AL tree
./target/release/al-test --suite system,foundation,baseapp # Microsoft's test codeunits, results in data/parity/
                                                          # (baseapp on the legacy W1 demo company, the others on Contoso; --data overrides)
                                                          # deterministic by default: Random() seeded to 1 and number sequences reset before every test, as in Microsoft's CI,
                                                          # so two runs of one build give the same outcome per test (--no-bc-runner: the old random mode)
                                                          # each test codeunit runs as Microsoft's runner 130450 runs it (--isolation codeunit,
                                                          # the default: one session per codeunit); --isolation test runs every method alone
                                                          # runs queue by memory and share the machine (tickets in /tmp/bc-rust-al-test.d);
                                                          # --test-timeout SECS (default 600) records a hung test as an error and goes on
                                                          # tests Microsoft's CI disables (vendor/bcapps/src/DisabledTests) are not run: the
                                                          # headline counts the tests CI runs; --include-disabled runs them too (all-tests figure)
./target/release/al-test --serve DIR                      # opt-in: one process runs queued al-test runs and keeps catalogs and companies loaded;
./target/release/al-test --client DIR --suite ...         # hands its arguments to it (runs standalone without a server), see docs/al-test-serve.md
./target/release/demo-data --memory --tool w1demo         # create a demo company (default --tool contoso), see docs/demo-data*.md
python3 -I tools/parity/diff-runs.py A-results.jsonl B-results.jsonl  # compare two al-test runs: net, movers by app and cause (GUIDs/timestamps masked)
./target/release/bc-nst --port 7185                       # service tier for Microsoft's web server (database bc_nst)
```

The live demos run as systemd user units: `bc-rust.service` (127.0.0.1:8740, database `bc`), `bc-nst.service` (127.0.0.1:7185, database `bc_nst`) and `msweb-bcnst-8193.service` (Microsoft's web server, `tools/msweb-linux/run.sh` on 127.0.0.1:8193). `bc-rust.service` and `bc-nst.service` both run `--apps standard-prod` and reach PostgreSQL over its Unix socket ([docs/deploy-db.md](docs/deploy-db.md)).

## BC sources and licence

bc-rust is under the MIT License ([LICENSE](LICENSE)). It is an independent project, and Microsoft does not make, endorse or support it. The AL it runs comes from Microsoft's [BCApps](https://github.com/microsoft/BCApps) repository (MIT), which is not in this repository: the first command above clones it into `vendor/`, which git ignores. `platform/` holds AL generated from Microsoft's System.app symbol package (BC 29.1.57326.57497, MIT). Microsoft's web server and web client are not included; `tools/msweb-linux` runs a copy you already have. Details are in [NOTICE](NOTICE).

Until 2026-11-10, development used decompiled Microsoft binaries as a reference. The code that closely followed decompiled code (date formulas, Excel number formats) has been rewritten clean-room from public documentation and tests run on a licensed Business Central. Microsoft texts taken from them were replaced with our own wording, the decompiled .NET shim is now built from dotnet/runtime source, and the earlier history of this repository was withdrawn. bc-rust no longer uses decompilation. Many runtime behaviours were first learned that way; their re-sourcing is not finished, and code comments mark the open ones with `[src:TODO]`.

[SOURCES.md](SOURCES.md) says where bc-rust's behaviour may come from; [docs/sources.md](docs/sources.md) records the evidence per area.

## Docs

- [architecture.md](docs/architecture.md): the crates, how storage maps to PostgreSQL, the compiled engine and the tree-walker.
- [features.md](docs/features.md): what works and what doesn't, in detail, as of I13.
- [parity-history.md](docs/parity-history.md): every full run of Microsoft's tests since 2026-11-07.
- [performance.md](docs/performance.md): side by side with real BC 29.1, the web client, Windows, posting and micro-benchmarks.
- [engine.md](docs/engine.md): the execution engines and their measurement rounds.
- [al-test-serve.md](docs/al-test-serve.md): `al-test --serve`, one process that keeps catalogs and companies loaded.
- [test-isolation.md](docs/test-isolation.md): how Microsoft's test runner isolates tests, and how al-test does it.
- [demo-data.md](docs/demo-data.md): building CRONUS with Microsoft's Contoso demo tool.
- [demo-data-w1.md](docs/demo-data-w1.md): the legacy W1 demo company that Microsoft's CI uses for the Base App tests.
- [standard-apps.md](docs/standard-apps.md): the apps of a BC 29.1 W1 install and how bc-rust loads them.
- [job-queue.md](docs/job-queue.md): the background pool, TaskScheduler, StartSession and the PostgreSQL locking contract.
- [deploy-db.md](docs/deploy-db.md): how the live services connect to PostgreSQL.
- [permissions.md](docs/permissions.md): BC's permission model and bc-rust's.
- [authentication.md](docs/authentication.md): how BC signs users in, and how bc-rust does it.
- [reports.md](docs/reports.md): the report layout engine, RDLC to Excel first.
- [platform-tables.md](docs/platform-tables.md): the platform system tables bc-rust defines in AL.
- [platforms.md](docs/platforms.md): building and testing on Linux, Windows and macOS.
- [nst-protocol-transport.md](docs/nst-protocol-transport.md): the protocol between Microsoft's web server and the service tier, transport and calls.
- [nst-protocol-metadata.md](docs/nst-protocol-metadata.md): the page metadata and record data bc-nst sends.
