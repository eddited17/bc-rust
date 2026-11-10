# Demo data

bc-rust creates the full CRONUS demo company the way Microsoft does. It runs codeunit 2 "Company-Initialize" and then codeunit 5193 "Contoso Demo Tool".CreateAllDemoData from the W1 Contoso Coffee Demo Dataset app (`vendor/bcapps/src/Apps/W1/ContosoCoffeeDemoDataset/app`). The interpreter runs that AL unmodified, on any backend.

One step after the tool is not Contoso's. `legacy_demo_tool_setup` copies four setup values from Microsoft's older W1 demo tool ("Create Sales & Receivables Setup".Finalize and "Create Purchases & Payables Setup"): the posted prepayment invoice and credit memo series reuse the posted invoice and credit memo series, in sales and in purchases. Contoso leaves them blank, and 317–321 prepayment tests then stop at "You cannot get a No. Series Line with empty No. Series Code". It only fills blank fields, and only in the Contoso company. It keeps Base App runs on Contoso (`--data contoso`) comparable. The Base App tests' default company, the legacy W1 one ([demo-data-w1.md](demo-data-w1.md)), sets these values itself. A removal on 2026-11-07 broke 317 ERM Prepayment tests on Contoso and was reverted the same hour. Since 2026-11-08 it also creates the legacy tool's three warehouse worksheet templates ("Create Whse. Wksh.-Template": PICK, PUT-AWAY and MOVEMENT), one for each type that has none. Contoso creates none, and `Library - Warehouse`.SelectWhseWorksheetTemplate takes the first template of a type. On the SCM-Warehouse tests on Contoso this gained 124 tests and lost none ("There is no Whse. Worksheet Template within the filter"; 251 on the full Contoso run). Two more steps followed on 2026-11-08. It removes Contoso's warehouse employees: the legacy W1 company has none. Contoso makes the demo user a default warehouse employee, so `Library - Warehouse`.CreateWarehouseEmployee(.., IsDefault = true) left the test's location without one. Removing them gained 93 SCM-Assembly tests and lost none ("Assigned User ID ... Warehouse Employee"). Two SCM-Warehouse tests now fail with "Queue underflow", as they do on w1demo. It also fills a blank Inventory Account (Interim) or WIP Account in Inventory Posting Setup with the account Contoso uses most for that field. The legacy tool sets both on every row. That gained 51 tests on SCM-Manufacturing and SCM-Costing and lost none. From the Contoso-only failure causes of I8 (812 enabled tests fail only on Contoso, 25 only on w1demo), it also creates what the legacy tool creates and Contoso lacks. Each was A/B-tested on Contoso, and none lost a test:

- resource journal templates RESOURCES and RECURRING;
- warehouse journal templates ADJMT, RECLASS and PHYSINVT;
- the cost journal template COSTACCT;
- a G/L budget for the work date's year;
- the number series INT-MOVE for Internal Movement Nos.;
- cost types from the chart of accounts (Cost Account Mgt.GetCostTypesFromChartDirect);
- service costs START and TRAVEL;
- one service item of a customer and an item that are not blocked;
- a General Posting Setup row for every business x product posting group, as the legacy W1 company has: a missing row copies the business group's MISC row. Contoso's FREIGHT product group had no rows at all, and EXPORT x MANUFACT was missing;
- an Inventory Posting Setup row for every storage location x inventory posting group (WHITE and EAST had no FINISHED row), copied from the group's blank-location row;
- Phys. Invt. Counting Periods FAST, NORMAL and SLOW; the SM-QUOTE series for Service Quote Nos.; item budget names DEFAULT (sales, purchase); a Purchase Variance Account on every general posting setup row; the DEFAULT batch of the RESOURCES resource journal template (each the legacy tool's, from I11's Contoso-only failure groups);
- the legacy item journal templates for every type Contoso has none of (PHYS. INV. and REVAL with their number series, RECLASS, RECURRING, CONSUMP, OUTPUT, CAPACITY when missing), each with a DEFAULT batch: tests that take a batch by its template type found none ("The Item Journal Template does not exist ... Name=''" in ERM Dimension Report once FlowField filters applied) or one of another type;
- no table-level default dimensions on projects, as in the legacy W1 company. Contoso's InsertJob validates the bill-to customer before the project has its number, so Job.CopyDefaultDimensionsFromCustomer (BC does the same) leaves the last customer's defaults, an AREA with Code Mandatory among them, on the Job table itself, and posting WIP for a test's project failed with "The AREA dimension is the default dimension, and it must have a value";
- prepayment accounts on the DOMESTIC general posting setups (the legacy tool's UpdatePrepmtAccounts): the Customer / Vendor Prepayments VAT account of the setup's product posting group, else the 25 % one. Contoso has these accounts but no setup uses them. `Library - ERM`.FindGeneralPostingSetupInvtFull then finds no full setup and gives one new G/L accounts without a VAT product group, and the prepayment tests stop at "The VAT Posting Setup does not exist ... 'DOMESTIC',''".

Gains: templates, budget and number series +57 (Resource, Cost Accounting, SCM-Warehouse, SCM-Service, SCM-Assembly), cost types +39 (Cost Accounting), service data +48 (SCM-Service), prepayment accounts +22 on Prepayment, posting setup grid +13 and inventory posting setup grid +12, the further legacy setup +60, item journal templates +45, project default dimensions +47 on the full Contoso run (2026-11-08).

Status as of 2026-11-07.

## Entry points

- `demo-data` (`crates/bc-server/src/bin/demo-data.rs`) creates the company in memory (`--memory`), in a WAL file (`--db FILE`) or in PostgreSQL (`--pg CONN`, or `--pg-default` for the server's own connection, which is the live database `bc`). It refuses a company whose Company Information already has a row. It prints the rows per module and level, the largest tables and the platform calls that bc-rust shimmed. `--json FILE` writes the whole report, `--setup-only` runs CreateSetupDemoData instead.
- `demo-data --db FILE --extra DIR --call ID:PROC` runs one parameterless procedure of a scratch AL app (a directory with `app.json`) against an existing database. Copy the WAL first, the call writes to it. This is the quickest way to look at data or reproduce a bug, see the probes in "Fixed" below.
- `bc_server::demo` (`crates/bc-server/src/demo.rs`) has the code: `create_demo_company`, `seed`, `seed_database`, `default_work_date` (LogInManagement.GetDefaultWorkDate, the last G/L entry's date), `number_sequences` and `restore_sequences`, and the two ways to give a test worker its own view of a company that was created once (`copy_mem_backend`, `cow_backend`).
- `al_model::demo_tool_dirs` returns the app directory, `demo::demo_app_dirs` the default apps plus that app.
- `al-test` creates the company once per run and caches it as a WAL file in `data/parity/cache/<dataset>-<key>.wal`. The key covers the executable and the loaded AL, so a rebuilt runtime creates a fresh one. A cache loads in 0.3 s. `--demo-cache FILE` picks the file, `--demo-cache none` turns caching off. Each suite gets the dataset Microsoft's CI uses: `system` and `foundation` run on Contoso, `baseapp` on the legacy W1 company (`w1demo`). `--data contoso|w1demo|seed|none` puts every suite on one dataset. A run with suites on both datasets runs them one after the other in the same process, each phase with its own catalog and company, and writes one report. The summary shows the dataset per suite (`data: system, foundation -> contoso (...)`), and every result line in the JSONL carries `data`.
- `al-test --data w1demo` and `demo-data --tool w1demo` use Microsoft's legacy W1 demo tool instead (`bc_server::demo::w1`). That is the company BCApps' CI runs the Base App tests on. See [demo-data-w1.md](demo-data-w1.md).
- `crates/bc-server/src/seed.rs` is the old small hand-made company (`al-test --data seed`, `bc sales`). `bc serve --pg` skips it when Company Information already has a row, so a database seeded with `demo-data --pg` serves CRONUS.

For PostgreSQL experiments use the scratch database `bc_demo`, never `bc`. Dropping the company schema can fail with "out of shared memory" once about 1,500 tables exist. Drop the tables in batches of 200 first.

## What it creates

Measured with `demo-data --memory` on a release build:

- 48,714 rows in 376 tables.
- 153.7 million AL statements in 89 s: 1.2 s catalog, 0.2 s Company-Initialize and platform rows, 82 s demo tool.
- Peak RSS 4.67 GB. Most of it is the catalog (an empty store with the same catalog uses 1.42 GB) and the creation transaction. The finished company takes about 140 MB in memory (1.56 GB RSS when loaded from the WAL).
- Inside al-test, with the test apps in the catalog, creation takes 75 s and peaks at 4.9 GB.

P1 measured about 100 s on PostgreSQL. I have not re-measured that.

All 16 Contoso modules run all four levels (Setup, Master, Transactional, Historical data). Posting the sales and purchase history takes 80% of the time:

| Module | Rows | Time |
|---|---:|---:|
| Company-Initialize | 2,137 | 0.2 s |
| Foundation | 1,507 | 0.0 s |
| Finance | 1,166 | 0.1 s |
| CRM | 522 | 0.1 s |
| Bank | 64 | 0.0 s |
| Inventory | 496 | 0.2 s |
| Purchase | 6,902 | 14.9 s (Historical 13.1 s) |
| Sales | 11,791 | 60.6 s (Historical 58.0 s) |
| Common | 10 | 0.0 s |
| Manufacturing | 22,456 | 0.8 s |
| Warehouse | 199 | 0.1 s |
| Service | 115 | 0.0 s |
| Fixed Asset | 194 | 0.3 s |
| Human Resources | 157 | 0.0 s |
| Job | 66 | 0.0 s |
| EService | 5 | 0.0 s |
| Analytics | 876 | 5.2 s |

Largest tables: Calendar Entry 21,924 (Manufacturing), G/L Entry 4,171, G/L - Item Ledger Relation 1,560, G/L Transaction 1,558, Value Entry 1,282, Detailed Cust. Ledg. Entry 1,145, G/L Register 1,045, Detailed Vendor Ledg. Entry 853, Item Application Entry 834, Item Register 803, Item Ledger Entry 797, Cust. Ledger Entry 579.

Platform calls bc-rust shims during creation, none of which stops it: DotNet ApiServicesEnabled (17,350 calls), Object.Update (3,514), Notification Id/AddAction/Message/Send/Scope/SetData/Recall (about 2,400), IsInTestMode (39), Object.Open/Close, TryGetStringTenantSetting, the IsAzure/IsSaaS/IsSandbox checks.

## Test workers

Every al-test worker needs its own copy of the company, because the embedded store has one writer per store and no isolation between connections. Until now each worker got a full copy (`copy_mem_backend`), so a run held 1 + jobs copies.

`cow_backend` replaces the copy with a row-level copy-on-write view. The shared master store is never written. The worker's own store holds only the rows the running test inserted or modified, under their own table, and a tombstone for each master row it deleted (a copy of the row in a shadow table with the table id negated). A read takes the worker's row if there is one, otherwise the master row unless it has a tombstone. Tables the worker has not changed are read straight from the master. Transactions, savepoints and rollbacks are the worker store's own, so rolling back a test empties it again. The number sequences stay per worker, as before.

Peak RSS on ERM-Sales + SMB (2,461 tests, `--data contoso`):

| | 1 job | 8 jobs | Per worker |
|---|---:|---:|---:|
| Full copy | 2.58 GB | 3.90 GB | 189 MB |
| Copy-on-write | 2.47 GB | 3.01 GB | 77 MB |

The results are the same. With Random() seeded per test and one job (fixed test order), all 2,461 tests had the same outcome in both modes. The only message differences were CreateGuid values. Wall time did not change (80 s and 78 s on one job, 19 s and 17 s on eight).

A first version copied a whole table into the worker on its first write. That saved only 50 MB per worker, because nearly every ERM test posts and so writes G/L Entry, Value Entry and the other ledger tables, about 20,000 of the company's widest rows.

The remaining 77 MB per worker is not table data, since the overlay is empty between tests. Each worker builds its own `Runtime`, with a `TableSchema` for every table and its own cache of virtual system tables (Field, AllObj). Sharing those across workers is the next saving.

al-test (`crates/bc-server/src/bin/al-test.rs`) gives every worker a `cow_backend` view since 2026-11-07. Checked once more after the switch, with copy and copy-on-write binaries built from one snapshot. With Random() seeded per test and one job, all 2,461 ERM-Sales + SMB tests had the same outcome (1,727 passed, 691 failed, 43 errors). The only message differences were GUIDs and clock times. At 12 jobs, peak RSS was 4.86 GB with full copies and 3.49 GB copy-on-write.

These per-worker numbers only hold for a focused suite. A worker grows with the tables and objects its tests touch (the schemas and caches in its own `Runtime`), so the full suite (`system,foundation,baseapp`, 44,430 tests) needs much more. RSS rises from 4.7 GB after loading to a plateau of 18.3 GB at 24 jobs, which is 0.57 GB per worker. At 48 jobs it peaks at 25.3 GB (0.43 GB per worker). Both take the same wall time (449 s and 470 s). `WORKER_MB_CONTOSO`, al-test's per-worker estimate for the default `--jobs`, is therefore 600 MB instead of 400. Sharing the schemas and catalog caches between workers would be the next saving.

## Known gaps

- Microsoft runs its W1 Base App tests (test type Legacy) on the company made by the older demo tool (`Layers/W1/DemoTool`), not on Contoso. bc-rust now builds that company too (`--data w1demo`, [demo-data-w1.md](demo-data-w1.md)). It passes 425 more of 19,574 tests in 11 areas than Contoso.
- Responsibility Centers only come from the Sustainability Contoso app, which needs the Sustainability, Statistical Accounts and ESG apps. bc-rust doesn't load them, and 18 ERM-Sales tests fail on the missing centers.
- General Posting Setup: 385 test errors on the small seed, 1 on Contoso (Sales Line Disc. Account is blank for DOMESTIC / MANUFACT).
- Random() takes every value from a fresh UUID and Randomize() does nothing, so test data changes between runs. Number sequences are not rolled back with a test either, so the document numbers a test gets depend on which tests ran before it in the same worker. Together they flip 5 to 10 tests between two runs of the same build. CreateGuid is random too.
- The Contoso data has 579 customer ledger entries, 13 of them open, all consistent (Open matches Remaining Amount). A sales order posted on top of it gets an open entry with Remaining Amount equal to Amount, also on the small seed. The "Closed = Yes with the full amount remaining" report from earlier no longer reproduces.

## Fixed

Closing a Sales Order asked "The document has been saved but is not yet posted" in 17 Contoso tests. BC limits that warning to invoices with a "My Notifications" row whose filter is `Document Type = Invoice`. The AL creates the row on the first check (Instruction Mgt. subscribes to OnAfterIsNotificationEnabled). It stores `SalesHeader.GetView(false)` inside report-parameter XML and later applies it with SetView in filter group 0, while the checked record sits in filter group 2. bc-rust created the row, but its SetView wiped every filter group, so the order passed the invoice filter. GetView, SetView, GetFilter, GetFilters and HasFilter now follow the BC service tier [src:TODO]. They work on the current filter group only. SetView reads `FILTERGROUP(n: ...)` sections and sets the key from SORTING. GetView writes `VERSION(1) SORTING(Field1,Field3) WHERE(Field1=1(2))` without captions and `VERSION(1) SORTING(Document Type,No.) WHERE(Document Type=FILTER(Invoice))` with them.

The full Contoso baseline (44,430 tests, `data/parity/full-baseapp`) showed more runtime gaps that only appear once the setup data exists. Fixed the same day, with the effect measured per test area against that baseline:

- CopyFilter only accepted `Record.Field` as its target. AL also passes a field of the implicit Rec or data item (`Item2.CopyFilter("Date Filter", "Due Date")` in table and report code). The target now resolves like any field reference. 1,861 tests stopped there, most in SCM-Assembly (+561 passing) and SCM-Warehouse.
- ClearAll replaced every global with a new value, so a temporary record variable lost its rows. BC's ClearAll clears each record like Clear does and keeps a temporary table's rows. Purch.-Post depends on that: ClearAllVariables keeps TempWhseRcptHeader, so "Warehouse handling is required" went away for 460 SCM-Warehouse tests (442 to 1,117 passing with the CopyFilter fix).
- Validate checked TableRelation even for fields with `ValidateTableRelation = false` (Company Information SWIFT Code, 144 tests in Misc).
- RecordRef.Get(RecordId) on an unopened RecordRef used its placeholder Integer table instead of opening the RecordId's table ("The Integer does not exist", 62 Workflow and 59 Misc tests).
- StrCheckSum always returned 0, so every GLN check-digit validation failed. The 144 tests that the SWIFT fix let through stopped there next.
