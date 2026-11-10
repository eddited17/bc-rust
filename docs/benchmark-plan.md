# Benchmark plan: bc-rust vs Business Central 29.1

Status: plan for iteration I16, written 2026-11-10. Nothing in this file has been measured yet. The numbers quoted are from preparation runs on a busy development host, and they show that the tools work, not how fast either system is.

The question a sceptic asks is fair: what are the speed claims based on? So far we compared with home-made scripts against a generated customer list. This plan replaces both:

- **Workload**: Microsoft's own Business Central Performance Toolkit (BCPT) and its sample scenarios, unchanged, on both systems.
- **Data**: real, messy company names and addresses from the GLEIF LEI register, loaded the same way into the same demo company on both systems.

Microsoft says what BCPT is for. It "helps answer questions such as, 'Does my solution for Business Central support X number of users doing this, that, and the other thing at the same time?'" and it "doesn't answer questions such as, 'How many orders can Business Central process per hour?'" (Performance Toolkit docs). The samples "are intended to provide inspiration". So the results are latencies of Microsoft's scenarios under a given number of concurrent sessions. They aren't an orders-per-hour figure, and we won't present them as one.

## What runs where

| | bc-rust | BC 29.1 |
|---|---|---|
| service tier | `bc serve --pg` (Windows build), background pool on | `bcside` container, BC 29.1.57326.57497 W1 sandbox, single tenant |
| database | PostgreSQL | SQL Server Express (the container's default) |
| machine | the same Windows 11 VM `bc-sidebyside` | the same VM, Hyper-V isolated container |
| AL | BCApps main (30.0.0.0) | 29.1.57326.57497 |
| BCPT apps | Performance Toolkit + Samples from BCApps, loaded through an app manifest | Performance Toolkit + Samples from the artifact (`C:\Applications\TestFramework\PerformanceToolkit`) |

Only one system runs at a time. The other is stopped (its process or its container), so neither competes for the VM's CPU, memory or disk.

## Starting data

1. **The same company on both sides.** The sandbox's own CRONUS is not the company bc-rust serves (they differ in posted documents and some items), so neither existing CRONUS is used. Both systems create a new company with Microsoft's Contoso Demo Tool (codeunit 5193, `CreateAllDemoData`), with the same modules and the same work date.
   bc-rust needs a `demo-data` switch that skips its legacy-W1 additions for this (they exist for Microsoft's tests, not for this benchmark). A comparison script (`tools/oracle/setup.mjs` style: row counts and hashes per table) checks that the two companies match before anything else runs.
2. **100,000 GLEIF customers.** `tools/benchdata/gleif_customers.py` picks them from the GLEIF golden copy by a fixed seed and writes API v2.0 `customers` bodies. The same loader posts them to both systems, one at a time, and `verify` compares what each system stored, column by column, with the prediction and with the other system.
   The data stays messy:
   - 13 % of names have non-ASCII characters (Latin accents, CJK, Greek, Cyrillic, Arabic);
   - names run up to 295 characters, and 0.24 % are longer than BC's 100;
   - 1.6 % of all names repeat an earlier one.
   The script cleans only what BC requires (the rules are in [tools/benchdata/README.md](../tools/benchdata/README.md)).
   First check, 2026-11-10, with 99,467 of these customers loaded into both systems. BC stored exactly the values the script predicts, in every column. bc-rust matched in every column except Search Name: 120 rows differ, and 2 customers failed. Both come from gap G4 below.
3. **Setup that both sides change the same way before loading.** Clear the Ending No. of number series CONT, because each customer creates a contact and Contoso's series stops at CT100000. Set the background session limit (BC's `BackgroundSessionsMaxConcurrent`, bc-rust's `BC_BG_MAX_SESSIONS`) to the same value, at least the largest session count of the plan.
4. **Back up** the database on each side. Every run starts from a restore of that backup, because BCPT runs write documents and ledger entries.

The customer load is itself a first measurement. It is API ingestion of 100,000 messy records, sequential, with latency per request (p50/p95/p99) and total time.

## Suites

The suite definitions are JSON files in the repo, posted to `bcptSuites` (with lines) on both systems.

**SINGLE.** It measures latency without contention. There is one suite per scenario, each with one background line and one session, started in Single Run mode one after another, each after the previous one finished.
The scenarios are the samples that work on a Contoso company on both systems: 149104 Create SO (10 lines), 149118 Post Sales (10 lines), 149105 Create SQ, 149103 Create PO, 149119 Purch. Post (10 lines), 149141 Post Sales Invoices, 149122 Post Purch. Inv., 149126 Create Customer, 149125 Create Vendor, 149127 Create Item, 149123 Adjust Cost, 149124 Calc. Plan Worksheet, 149107 Detail Trial Balance, 149202 Warehouse Receipt, and the two calibration scenarios 149117 SQL Ping and 149116 Sleep 1s.
We repeat this 10 times (BCPT versions 1-10) per system and repetition.

**TRADE-n.** It measures latency under concurrency. These are timed BCPT runs (Duration 10 minutes) with background sessions:

| line | TRADE-5 | TRADE-10 | TRADE-20 |
|---|---:|---:|---:|
| 149118 Post Sales with 10 lines | 2 | 4 | 8 |
| 149104 Create SO with 10 lines | 1 | 2 | 4 |
| 149119 Purch. Post with 10 lines | 1 | 2 | 4 |
| 149126 Create Customer | | 1 | 2 |
| 149117 SQL Ping | 1 | 1 | 2 |

The suites use BCPT's default user delays (100-1,000 ms between steps) and 5 s fixed between iterations. TRADE-20 needs the background session limit raised on both systems.

**Left out, with reasons.**
- The page scenarios (Open Customer List and the others) are test codeunits that need a client session and run in the foreground only. bc-rust doesn't run them correctly outside its test runner yet (gap G7 below).
- 149140 and 149120 (item journals) expect items of Microsoft's older W1 demo data, and they write the same journal line, so they collide with each other.
- 149121 Post G/L Entries needs a number series on the general journal batch that Contoso's batch doesn't seem to have (to be confirmed on BC).
- The job queue scenarios measure the scheduler's polling, not work.
- 149108 calls the internet.

**Optional, clearly labelled as ours.** A BCPT scenario codeunit, written by us, that posts sales orders for random GLEIF customers. Microsoft's samples always sell to customer 10000, so they barely touch the 100,000 new customers.

## Starting a run

On both systems the run starts from the BCPT Setup Card in Microsoft's web client (BC's own web server for BC; Microsoft's web server on bc-nst for bc-rust). Buttons are "Start" and "Start in Single Run mode". The `tools/webcheck` browser harness clicks them, or a person does.
All lines are background lines, so the client session only starts the sessions. Microsoft's PowerShell runner (`RunBCPTTests.ps1`) is a second option for BC. It needs PowerShell 7 with the BC 29 artifacts, because Windows PowerShell 5.1 fails on them (navcontainerhelper issue #4234). It is untested against bc-rust.

## Order and repetitions

- ABAB. One system runs a whole step (all SINGLE suites, or one TRADE-n), then the other, and so on. Three repetitions per system and step.
- Before each run: restore the backup, start the system, wait until it answers, then 2 minutes idle.
- During each run, record the VM's CPU, memory and disk every 5 s (`typeperf`), plus the host's load, so a disturbed run can be spotted and repeated.
- Nothing else runs on the host during the window.

## Comparing

Each system's BCPT log entries are exported in the shape of BC's `bcptLogEntries` API. On BC that is the API itself. On bc-rust it is a query until it serves that route; see `tools/benchdata/bcpt_report.py export-sql`. `tools/benchdata/bcpt_report.py` then reports per scenario and operation:

- iterations and errors per system (BCPT's own success/error status);
- duration p50, p95 and mean in ms, user waits excluded (BCPT subtracts them);
- the ratio of medians with a bootstrap 95 % interval (fixed seed);
- SQL statements per operation, each system's own count. These are not comparable between the systems: BC counts SQL Server statements behind its data cache, bc-rust counts PostgreSQL statements behind its own cache. They are shown, never divided.

The first 2 minutes of every TRADE run are dropped, and each session's first iteration is reported separately as "cold".

**What counts as a result.** "bc-rust is X times faster at Y" is said only when all three repetitions agree in direction and the 95 % interval excludes 1. Everything goes into the repo's data directory and the report: raw log entries, suite files, versions, binary checksums and VM load. That includes scenarios where bc-rust is slower and runs with errors.

## What bc-rust needs first

A preparation run on 2026-11-10 (b41a391) showed that BCPT already runs end to end on bc-rust:
- the apps install;
- suites validate, including the parameter interface;
- Start and Start in Single Run mode work;
- 21 StartSession background sessions ran at once;
- the timed loop, the user waits and the log entries all work.

These gaps remain, smallest first:

| # | gap | effort |
|---|---|---|
| G5 | `demo-data` can't build a pure Contoso company (it always adds its legacy-W1 setup) | 0.25 d |
| G4 | Code values are upper-cased with the full Unicode mapping (`ß` -> `SS`). BC maps one character at a time. 121 of the 100,000 GLEIF names get a different Search Name. 2 become too long for Code[100] on bc-rust only, and seeding confirmed it on 2026-11-10: on bc-rust exactly those 2 of 99,950 failed with "The length of the string is 102/101". | 0.5 d |
| G1 | `FindSet(true)` takes no update lock (BC 29.1 sets ReadIsolation UpdLock). Two samples starting together both edit the same number series line, and on bc-rust one fails with "Another user has modified the record". Production code has the same pattern. | 0.5 d + record-semantics gate |
| G6 | StartSession beyond the pool limit returns false. BC queues the session. | 0.5 d (or the same limit on both sides) |
| G2 | `SessionInformation.SqlStatementsExecuted` is always 0, so BCPT's SQL column is empty on bc-rust | 0.5-1 d |
| G3 | Only `/api/v2.0` is routed, so BCPT's own API (`/api/microsoft/performancToolkit/v1.0`) gives 404. It is needed to import suites and read results the same way on both systems. | 0.5-1 d, deep insert +0.5 d |
| G7 | Test codeunits started from a test runner codeunit outside al-test don't run their test methods, and TestPage works only in al-test. Page scenarios report success without opening a page. | 2-3 d (or leave page scenarios out, as above) |
| G8 | Microsoft's PowerShell runner against bc-nst | unknown, not needed |
| G9 | Same AL on both sides (see caveats) | 3-5 d+, optional |

G1, G4 and G5 should land before the benchmark. G2 and G3 make the comparison cleaner. G7 and G8 can wait.

## Caveats to publish with the numbers

- **Different AL.** bc-rust runs Microsoft's BCApps main branch (version 30.0.0.0, November 2026), and BC runs 29.1's AL. The toolkit and the sample scenarios are identical in both (one line differs, in a reset action), but the Base Application under them is not the same code.
- **Different virtualisation.** BC runs in a Hyper-V isolated container inside a Windows VM that itself runs under KVM. bc-rust runs as a process in the same VM. Both share the VM's cores and memory, but BC pays one more layer.
- **Different database edition.** The container uses SQL Server Express, which caps its buffer pool at about 1.4 GB and 4 cores. Production BC runs on full SQL Server or Azure SQL. PostgreSQL runs inside the same VM with a stated configuration. Running it on the host, as earlier side-by-side runs did, would move its CPU outside the VM's budget.
- **Permissions.** bc-rust checks table permissions only when `BC_PERMISSIONS=enforce` is set and doesn't check object Execute permissions at all. BC always checks both. The runs use enforce on bc-rust and say what it doesn't check.
- **Caches.** BC's data cache and bc-rust's read cache both count. They are part of each system.
- **Collation.** The container's CRONUS database compares text case-sensitively (Latin1_General_100_CS_AS), and bc-rust's PostgreSQL compares case-insensitively. A filter on a text value can return different rows: on the GLEIF customers, `city eq 'George Town'` gave 813 on BC and 960 on bc-rust. Scenarios and checks must not depend on it, and this difference itself is a parity question for bc-rust.
- **Telemetry.** Neither system sends telemetry in these runs, because the container has no Application Insights key.
- **Licence.** The container's BC licence allows 10 full users. Background sessions that BCPT starts run as the user who pressed Start. Microsoft's PowerShell runner opens client sessions, and those may count against the limit (not checked).
- **Contention differs already at load time.** Seeding real BC with 7 users × 5 parallel requests hit SQL deadlocks on the Contact table on the first attempt (409); the retry had none. On bc-rust the only failures were the two Search Name cases above. The plan loads sequentially, so both systems get the same contact numbers.
- **Scope.** Microsoft's scenarios and our data, one company, one VM, up to 20 sessions. That says nothing about other workloads or hardware.

## Data licence

The customers are derived from the GLEIF golden copy, which GLEIF publishes under CC0 1.0. Per GLEIF's terms, the converted records are not presented as GLEIF data. They are cut to BC's field lengths and their countries are mapped. The generated files stay outside the repo, and the converter documents how to rebuild them byte for byte from a given golden copy.
