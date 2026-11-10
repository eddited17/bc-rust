# The legacy W1 demo company (`--data w1demo`)

Microsoft's CI does not run the Base App tests on the Contoso company. bc-rust can now create the company those tests are written for, with Microsoft's legacy W1 demo tool running unmodified, and `al-test --data w1demo` runs the tests on it.

Status as of 2026-11-07.

## What Microsoft's CI uses

BCApps' AL-Go build (vendor/bcapps) sorts test apps into test types, and each type gets its own company and data:

- `build/scripts/TestConfiguration.json` lists every Base App test app (`Tests-ERM`, `Tests-SCM-Warehouse`, `Tests-Prepayment`, `Tests-Misc`, ... all of `src/Layers/W1/Tests`) in the buckets `LegacyTests-Bucket1` and `LegacyTests-Bucket2`.
- `build/projects/Test Apps W1/.AL-Go/settings.json` maps the build modes `LegacyTestsBucket1/2` to `testType: Legacy` and `companyName: CRONUS International Ltd.`. `UncategorizedTests` uses `testType: Uncategorized`, `IntegrationTests` uses `IntegrationTest` with "My Company", and the default is `UnitTest` with "Empty Company".
- `build/scripts/ImportTestDataInBcContainer.ps1`, `Invoke-DemoDataGeneration`, builds each company:
  - Legacy: install the Base, TestFramework and LocalBaseExtensions groups plus the `DemoTool` app, create "CRONUS International Ltd." (not an evaluation company), run codeunit 2 "Company-Initialize", stage the tool's resources and run page 101900's action "Create Demo Data from Config" (codeunit 101899). Only then are the remaining apps installed. `Initialize-DemoToolResources` copies `src/DemoTool/Pictures/*`, the `o365` folder and `DemoDataConfig.xml` with `DataType` set to `Extended` into the session's user folder. The comment there says: "BCApps legacy tests only generate Extended demo data".
  - Uncategorized: an evaluation company with Contoso (codeunit 5193 CreateAllDemoData, then 5140 DeleteWarehouseEmployee and 5691).
  - IntegrationTest: Contoso setup data only (CreateSetupDemoData).
  - UnitTest: no demo data.
- `.github/AL-Go-Settings.json` adds `src/Layers/W1/DemoTool` to the app folders. For `LegacyTestsBucket1` it also sets `additionalDemoDataTypes: ["Standard", "Evaluation"]`. Those runs only check that the tool works on throwaway companies, which are deleted before the real test company is built.

So the W1 Base App tests run on the legacy tool's Extended company. Contoso is for the newer apps' Uncategorized and Integration tests. bc-rust's `--data contoso` had been running the Base App tests on the wrong company.

The test libraries carry the same assumptions: `LibraryWarehouse` says "expected to be present as Distribution demo data has been called" and "same as the one in the demo data". Neither of those is Contoso data.

## How bc-rust builds it

`bc_server::demo::w1` (`crates/bc-server/src/demo/w1.rs`) follows the Legacy branch above:

1. `al_model::w1_demo_tool_dirs` adds the DemoTool app (`Layers/W1/DemoTool`, 513 files) to the catalog. `demo::w1::w1_demo_app_dirs` gives the default apps plus that app.
2. `create_w1_demo_company` writes the platform rows and runs codeunit 2. It then puts the resources from `al_model::w1_demo_tool_resources` (`vendor/bcapps/src/DemoTool`) into the session's in-memory server files, in the user folder where CI copies them, and runs codeunit 101899. `DemoDataConfig.xml` gets `DataType` Extended and `DataLanguageID` 1033, as in CI.
3. The tool sets the starting year to the current year + 1 and posts about 14 months of history. The posting starts in December of the current year. The work date of a test session is the last G/L entry's date (LogInManagement.GetDefaultWorkDate, because the tool sets "Demo Company"), 2028-01-27 when run in 2026.

Entry points:

- `demo-data --memory --tool w1demo` (or `--db FILE`, `--pg CONN`). `--tool contoso` is the default.
- al-test runs the `baseapp` suite on this company by default (since 2026-11-07); `system` and `foundation` stay on Contoso, as in Microsoft's CI. `--data contoso|w1demo|seed|none` overrides that for every suite. A run with both, such as `--suite system,foundation,baseapp`, runs the Base App phase first and then the Contoso phase. Each phase has its own catalog, because the two demo tools share 105 object names (for example codeunit "Create Bank Account") and al-model resolves names in one global map. The phases write one report, with a `data:` line per phase in the summary and a `data` field on every JSONL result.
- The company is created once per run and cached in `data/parity/cache/w1demo-<key>.wal`. Workers get copy-on-write views of it (`demo::cow_backend`).
- Memory: `WORKER_MB_W1DEMO` = 1,300 MB per worker, from the full Base App run, which peaked at 20.2 GB on 12 jobs: (20.2 − 5 GB base) / 12. The default `--jobs` is the CPU count, capped by the number of codeunits and by `(MemAvailable − 6 GB − catalog 2.5 GB − company 1 GB from the cache, or 3.5 GB to build it) / 1.3 GB`. Contoso stays at I1's 600 MB, measured on the full suite at 24 jobs. On the 11 areas below, w1demo used less than Contoso (9.6 against 12.1 GB at 10 jobs). The full Base App suite's heavier areas set the w1demo figure.
- `demo.rs` still copies the legacy tool's four prepayment number series and its three warehouse worksheet templates into the Contoso company (`legacy_demo_tool_setup`), so `--data contoso` Base App runs stay comparable. This company has them from the tool itself.

Measured on a release build with `demo-data --memory --tool w1demo`:

- 45,044 rows in 567 tables, in 82 s wall time (78 s for the tool, 162 million AL statements). Peak RSS 3.95 GB.
- Inside al-test, with the test apps installed first: 45,265 rows in 577 tables, 74.7 s.
- Largest tables: Calendar Entry 9,378, G/L Entry 2,814, Contact Profile Answer 2,015, Config. Package Field 2,003, Cost Entry 1,279, Analysis View Entry 1,151.
- The only shims left are the platform check IsInTestMode and the skipped PDF rendering of the VAT settlement report (below). Since 2026-11-08 the environment checks (IsSaaS, IsSandbox, TryGetStringTenantSetting, ApplicationIdentifier) answer as an on-premises BC 29.1 server does (crates/al-runtime/src/environment.rs).

## Runtime gaps the tool hit, fixed generally

- **XMLports with `Format = VariableText`** could not import (`xmlport.rs`). They now read the file into records and fields using FieldDelimiter, FieldSeparator and RecordSeparator, with BC's `<None>`, `<NewLine>`, `<CR/LF>`, `<CR>`, `<LF>` and `<TAB>` tokens. Delimited fields can contain separators. Each line becomes one record of a table element, its fields go to the element's field and text elements in schema order, and the usual triggers run. The tool reads `CurrencyData.txt` through XMLport 101904 this way.
- **`DefaultFieldsValidation`** on an XMLport now sets the default of FieldValidate for its fields (al-model). Before, it was always yes.
- **Server file paths** (`files.rs`) now resolve like BC's `NavFile.ExpandFileName`: a relative name is relative to the session's user folder (`C:\bc-rust\`), and `.`, `..` and doubled separators are resolved. **`TemporaryPath()`** returned `''`. It now returns the user folder's TEMP subfolder (`C:\bc-rust\Temp\`), as BC's `NavFile.TempPath` does, so `TemporaryPath() + '..\WalkMeTour'` finds the user folder's `WalkMeTour`. [src:TODO]
- **DotNet `System.IO.Path`, `Directory`, `DirectoryInfo` and `FileInfo`** did nothing: every call returned an empty value, so all of FileManagement's path functions (GetFileName, CombinePath, GetDirectoryName, ServerDirectoryExists, GetServerDirectoryFilesList, ...) returned `''`/false. They now work on the session's server files (`Session::net_io_call` in files.rs) with Windows path rules. The Vfs also knows folders now: exists, create, delete, and listing with `*`/`?` patterns.
- **`Blob.Import(FileName)` / `Blob.Export(FileName)`** were not implemented (`streams.rs`). **`Media.ImportFile`** was a no-op (`media.rs`). All three now read and write server files.
- **"Application Object Metadata"** (2000000207) had no rows. It is now a catalog-computed table with one row per object, like AllObj. The Metadata blob, BC's compiled object metadata, stays empty, so AL that reads it sees `HasValue = false`. ODataUtility.GetTenantWebServiceMetadata then returns no QueryMetadataReader, and the web service columns get no field names.
- **`for D := A to B` over Date values** ran once with `0D`, because `as_i64()` of a Date is 0 (`interp.rs`). Date counters now step one day at a time. Dates are stored as days × 2, and closing dates are odd. The tool's posting loop over 14 months depends on it.
- **DotNet `System.IO.Compression.GZipStream`**, with CompressionMode and CompressionLevel, is new (`gzip.rs`). It is pure Rust: compression writes stored deflate blocks, which is valid gzip without the size reduction, and decompression inflates any deflate stream and checks the CRC. "Config. Pckg. Compression Mgt." uses it for RapidStart packages. The tool exports its packages with it, and RapidStart imports need it too.
- **Report rendering** is still not implemented. The tool posts its VAT settlements with `Report 20.SaveAsPdf` and never reads the PDF. `al_runtime::report::set_skip_rendering(true)` makes the report run its processing and skip the rendering. `create_w1_demo_company` turns it on only while the tool runs. Tests still get the "Not implemented in bc-rust: report rendering" error.

## Contoso and w1demo compared

One al-test binary, the same 11 test apps (646 codeunits, 19,574 tests), 10 jobs, once with each dataset. "Gained" and "lost" count single tests that changed outcome. The I1 baseline (`data/parity/full-baseapp-2/2026-11-07-full-1724`, a slightly older build on Contoso) is there for reference; it matches the same-build Contoso run within a few tests per area.

| Area | Tests | Contoso (I1 baseline) | Contoso (same build) | w1demo | Delta | Gained | Lost |
|---|---:|---:|---:|---:|---:|---:|---:|
| Prepayment + Workflow | 1,664 | 856 | 855 | 953 | +98 | 99 | 1 |
| SCM-Assembly | 1,779 | 1,005 | 1,005 | 1,028 | +23 | 30 | 7 |
| SCM-Warehouse | 2,206 | 1,148 | 1,148 | 1,280 | +132 | 135 | 3 |
| ERM-Sales + SMB | 2,461 | 1,729 | 1,722 | 1,769 | +47 | 53 | 6 |
| Misc | 3,305 | 2,073 | 2,087 | 2,098 | +11 | 53 | 42 |
| ERM | 4,079 | 2,964 | 2,971 | 2,998 | +27 | 32 | 5 |
| ERM-Purchase | 1,739 | 1,237 | 1,238 | 1,258 | +20 | 25 | 5 |
| ERM-Finance | 1,542 | 940 | 940 | 982 | +42 | 50 | 8 |
| ERM-Application | 799 | 542 | 539 | 564 | +25 | 29 | 4 |
| **Total** | 19,574 | 12,494 | 12,505 | 12,930 | +425 | 506 | 81 |

The whole Base App suite (`--suite baseapp`, 1,418 codeunits, 42,146 tests, 12 jobs, 608 s) passes 27,249 on w1demo (64.7%). The I1 Contoso baseline passes 25,854 of the same tests, so w1demo is ahead by 1,395. Of the 48 apps, 45 are the same or better, and three lose 1–2 tests (Rapid Start, SMB, Data Exchange). The biggest gains are SCM-Service +309, SCM-Warehouse +136, Cost Accounting +130, SCM-Planning +98, SCM +88, SCM-Costing +87, Prepayment +87, SCM-Manufacturing +61, ERM-Sales +41, ERM-Finance +39, Resource +34, ERM +33 and Job +31. That run peaked at 20.2 GB RSS.

Within the 11 areas: Prepayment 425 → 518, Workflow 430 → 435, ERM-Sales 963 → 1,009, SMB 759 → 760.

Wall time was the same (349 s with w1demo, 357 s with Contoso). Peak RSS was 9.6 GB with w1demo and 12.1 GB with Contoso at 10 jobs.

What the gained tests had failed on with Contoso is mostly missing setup: VAT Posting Setup (76), Whse. Worksheet Template (55), General Posting Setup (37), Warehouse Entry (35), "Enter the document no." (21), Responsibility Center (16), Cost Journal Template (14), Inventory Posting Setup (10), Purch. Line Disc. Account (9), Service Item (9), Internal Movement Nos. (9), Service Quote Nos. (7).

The data-shaped causes in the Contoso baseline's top 30, counted in these 11 areas:

| Cause | Contoso | w1demo |
|---|---:|---:|
| There is no Whse. Worksheet Template within the filter | 155 | 0 |
| The VAT Posting Setup does not exist | 94 | 10 |
| The Bank Account does not exist | 41 | 67 |
| Assigned User ID 'ADMIN' not in Warehouse Employee | 87 | 115 |

The remaining Bank Account errors name accounts that the tests create themselves (`1GU0000…`, blank), so they are runtime gaps, not demo data. The Assigned User ID failures aren't data either. `check_relation` (`recops.rs`) picks the last primary key field when a TableRelation names no field, and it ignores FIELD conditions. BC picks the first one [src:TODO]. For `"Warehouse Employee" where("Location Code" = field("Location Code"))` (key "User ID", "Location Code"), bc-rust therefore compares 'ADMIN' with Location Code where BC uses User ID. The Field table's RelationFieldNo (`virt.rs`) has the same last-field rule. The legacy tool creates no Warehouse Employee rows (CI's Contoso flow even deletes them, codeunit 5140), and the tests create their own.

The 81 lost tests are bc-rust gaps exposed by the other data, not wrong data, because CI runs these tests on this company. The largest groups are 16 Payroll Extension Support tests, where the General Journal TestPage doesn't compute ImportPayrollTransactions' visibility on the page-open path the legacy journal templates take, and 4 "Income/Balance must be equal" in G/L Account Category.
