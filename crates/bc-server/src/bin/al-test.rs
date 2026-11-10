//! al-test: run Microsoft's AL test codeunits against bc-rust and measure the pass rate.
//!
//! Loads the default app set plus the Test Framework and the selected test apps, then runs every
//! [Test] procedure of every selected test codeunit in its own fresh session on an in-memory
//! database (OnRun first, UI handlers from [HandlerFunctions], everything rolled back afterwards).
//!
//! ```text
//! al-test [--suite system,foundation,baseapp[:Area,...]] [--app NAME] [--codeunit NAME|ID,...]
//!         [--test NAME] [--jobs N] [--label X] [--out DIR] [--step-limit N] [--test-timeout SECS]
//!         [--limit N] [--list] [-v]
//!         [--data contoso|w1demo|seed|none] [--demo-cache FILE|none] [--work-date demo|today] [--no-install]
//! ```
//! Every test database starts like a tenant with the loaded apps installed: their Install
//! codeunits run first (per app, dependencies first: OnInstallAppPerDatabase, then
//! OnInstallAppPerCompany; Session::install_apps), then the company data is created.
//! `--no-install` skips them.
//! Each suite runs on the company Microsoft's CI gives its tests (bc_server::demo): `baseapp` on
//! the company of Microsoft's legacy W1 demo tool (DemoTool app, data type Extended,
//! bc_server::demo::w1; BCApps runs the Base App tests as test type Legacy on it), `system` and
//! `foundation` on the full CRONUS company of the Contoso demo tool. `--data contoso|w1demo|seed|none`
//! puts every selected suite on one dataset instead. Suites with different datasets run one after
//! the other, each with its own catalog (the two demo tools share 105 object names) and company,
//! into one report (the Base App phase first). A demo company is created once per run, each
//! worker gets a copy-on-write view of it, and every test is rolled back. The company is cached
//! in a WAL file (plus FILE.seq.json for number sequences): by default
//! data/parity/cache/<dataset>-<key>.wal, where the key covers this executable and the loaded AL,
//! so a rebuilt runtime or changed AL creates a fresh one (~90 s) and later runs load it in under
//! a second. `--demo-cache FILE` picks the file (with several datasets, FILE gets a `-<dataset>`
//! suffix; delete it after AL or runtime changes), `--demo-cache none` disables caching.
//! `--data seed` is the small hand-made company of crates/bc-server/src/seed.rs; `--data none`
//! (or `--no-seed`) only has the platform rows.
//! With a demo company, sessions start on the work date BC gives a session that opens a demo
//! company (LogInManagement.GetDefaultWorkDate: the last G/L entry's date); `--work-date today`
//! keeps today's date instead.
//! Tests that Microsoft's CI disables (vendor/bcapps/src/DisabledTests/<app>/*.json, read per
//! loaded app as BCApps' Get-DisabledTestsForApp does; method "*" = the whole codeunit) are not
//! run: they are recorded with outcome "disabled", and the summary's headline counts the tests CI
//! runs (CI-comparable). `--include-disabled` runs them too, flags their results ("disabled":
//! true) and reports the all-tests figure of runs before 2026-11-08 08:00 UTC. About 12,100 of
//! the 42,146 Base App tests are disabled.
//! `--test-mem-limit MB` (default the dataset's per-worker figure plus 3 GB, 0 = off) is a memory
//! limit per test: a test whose worker thread grows by more than that (live bytes it allocated,
//! counted by the global allocator) fails with "memory limit exceeded: <codeunit> :: <test>",
//! its memory is freed with the test and the worker goes on. `<stem>-memory.txt` lists the tests
//! that grew their worker most (P9's and P11's runs were OOM-killed by single tests, 2026-11-08).
//! `--test-timeout SECS` (default 600, 0 = off) is a wall-clock limit per test, for hangs the step
//! limit does not see (native loops, regex backtracking, blocking waits): the test is recorded as
//! an error "timeout after N s", its worker thread is abandoned and a new worker takes the rest of
//! that codeunit. The slowest real tests take about 190 s (SCM Performance Tests,
//! TenThousandSerialNos).
//! `--isolation codeunit` runs a test codeunit the way Microsoft's CI does (runner 130450 "Test
//! Runner - Isol. Codeunit", TestIsolation = Codeunit; docs/test-isolation.md): one session and
//! one codeunit instance, OnRun once, then the [Test] methods in order sharing globals and single
//! instances; a method's changes stay for the next ones unless it fails (back to its last
//! Commit) or is [TransactionModel(AutoRollback)]; the codeunit end rolls everything back. A
//! timeout there ends the codeunit: its remaining tests are recorded as errors (BC's session would
//! be gone). This is the default since 2026-11-08 (full suites: w1demo +14, contoso +140 tests
//! against per-test isolation). `--isolation test` runs every method alone. `--seed-random` seeds Random()
//! per test from its codeunit and name.
//! Runs are deterministic by default (`--no-bc-runner` turns it off): before every test method al-test
//! does what Microsoft's test runner does (ClearLastError, Library - Random.SetSeed(1) with
//! Randomize(Seed) seeding Random(), the temporary notification context; `runner_reset`) and puts
//! the demo company's NumberSequences back to the snapshot (every codeunit with `--isolation
//! codeunit`). Two runs of one build then give the same outcome for every test whatever the job
//! count. `--seed-random` only matters with `--no-bc-runner`.
//! `--clock virtual` (al_runtime::clock) also makes timestamps and Guids repeat: a virtual clock
//! (every reading at least 1 ms later, 10 µs per AL statement, Sleep moves it on without waiting)
//! and a seeded version 4 Guid stream, both started per isolation unit (`start_unit_clock`); the
//! demo company is built on its own virtual clock. See docs/test-isolation.md.
//! Writes `<out>/<date>[-label]-summary.txt` and `<out>/<date>[-label]-results.jsonl`.
//!
//! Memory admission (bc_server::admission): runs that fit in memory run at the same time, in
//! arrival order. Each run has a ticket in /tmp/bc-rust-al-test.d (Windows:
//! %TEMP%\bc-rust-al-test.d) and reserves memory before it loads a catalog (CATALOG_MB) and
//! before it starts its workers (the demo company, MASTER_LOAD_MB from a cache or MASTER_BUILD_MB
//! to create it, plus WORKER_MB_<dataset> per worker). A reservation waits ("waiting ...: N GB
//! reserved by PIDs ...") until every earlier run is admitted and it fits in MemAvailable minus a
//! 6 GB margin, plus 1 GB per memory-holding run beyond two, at most 4 GB more
//! (`AL_TEST_MEM_MARGIN_MB` raises the base), minus what admitted runs reserved but do not use yet. Tickets of dead processes are
//! removed. Admitted runs hold /tmp/bc-rust-al-test.lock shared, so older binaries that lock it
//! exclusively still run alone. The default `--jobs` is the CPU count, capped at
//! DEFAULT_JOBS_MAX (16), by the number of test codeunits and by the memory this run could have
//! once the other runs are done. While it runs, the grant follows the RSS (every 5 s); a run that
//! outgrows its grant while MemAvailable is within the margin stops taking new codeunits ("memory
//! guard: pausing") until there is room again; the oldest run never pauses.

// mimalloc, as for the bc binary (Linux and Windows only, see Cargo.toml). On ERM-Sales + SMB
// (contoso, 7 jobs, 2026-11-08): same outcome for every test, peak RSS 5.88 -> 3.97 GB, CPU
// 215 -> 164 s, test wall time 31 -> 18 s.
// mimalloc only with the cargo feature `mimalloc` (off by default since 2026-11-08 05:25 UTC:
// both v3 and v2 crashed in _mi_page_malloc_zero, a heap corruption still being looked for)
#[cfg(all(feature = "mimalloc", any(target_os = "linux", target_os = "windows")))]
static INNER: mimalloc::MiMalloc = mimalloc::MiMalloc;
#[cfg(not(all(feature = "mimalloc", any(target_os = "linux", target_os = "windows"))))]
static INNER: std::alloc::System = std::alloc::System;

// Each worker thread's live bytes, for the per-test memory cap (run_workers): the allocator adds
// what a thread allocates to that thread's counter (a leaked AtomicI64 per worker, so the pointer
// is never dangling), and nothing for threads without one.
thread_local! {
    static WORKER_BYTES: std::cell::Cell<*const std::sync::atomic::AtomicI64> = const { std::cell::Cell::new(std::ptr::null()) };
}

#[inline]
fn count_alloc(d: i64) {
    let _ = WORKER_BYTES.try_with(|c| {
        let p = c.get();
        if !p.is_null() {
            // SAFETY: the counters are leaked ('static) and only ever set to such a pointer
            unsafe { (*p).fetch_add(d, std::sync::atomic::Ordering::Relaxed) };
        }
    });
}

struct CountingAlloc;

unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        let p = std::alloc::GlobalAlloc::alloc(&INNER, l);
        if !p.is_null() {
            count_alloc(l.size() as i64);
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: std::alloc::Layout) -> *mut u8 {
        let p = std::alloc::GlobalAlloc::alloc_zeroed(&INNER, l);
        if !p.is_null() {
            count_alloc(l.size() as i64);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        std::alloc::GlobalAlloc::dealloc(&INNER, p, l);
        count_alloc(-(l.size() as i64));
    }
    unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, new: usize) -> *mut u8 {
        let q = std::alloc::GlobalAlloc::realloc(&INNER, p, l, new);
        if !q.is_null() {
            count_alloc(new as i64 - l.size() as i64);
        }
        q
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session, TestCtx};
use al_syntax::ObjectKind;
use bc_server::demo::{self, SeqState};
use bc_store::{Backend, MemBackend, Store};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Data {
    /// platform rows only
    None,
    /// crates/bc-server/src/seed.rs
    Seed,
    /// Microsoft's Contoso demo tool (full CRONUS)
    Contoso,
    /// Microsoft's legacy W1 demo tool (DemoTool app, Extended): BCApps' CI company for the Base
    /// App tests
    W1Demo,
}

impl Data {
    fn name(self) -> &'static str {
        match self {
            Data::Contoso => "contoso",
            Data::W1Demo => "w1demo",
            Data::Seed => "seed",
            Data::None => "none",
        }
    }

    /// The dataset a suite runs on unless `--data` is given, as in Microsoft's CI (vendor/bcapps
    /// build/scripts/ImportTestDataInBcContainer.ps1, TestConfiguration.json): the Base App tests
    /// (test type Legacy) on the legacy W1 demo tool's company; the System Application and
    /// Business Foundation tests on Contoso (CI runs the apps it does not class as Legacy on
    /// Contoso data, or on none).
    fn for_suite(suite: &str) -> Data {
        match suite.split(':').next().unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "baseapp" => Data::W1Demo,
            _ => Data::Contoso,
        }
    }

    /// Memory estimate per worker (see BASE_MB).
    fn worker_mb(self) -> u64 {
        match self {
            Data::Contoso => WORKER_MB_CONTOSO,
            Data::W1Demo => WORKER_MB_W1DEMO,
            Data::Seed | Data::None => WORKER_MB_SEED,
        }
    }
}

#[derive(Clone)]
struct Opts {
    suites: Vec<String>,
    app: Option<String>,
    codeunits: Vec<String>,
    test: Option<String>,
    jobs: usize,
    label: Option<String>,
    out: PathBuf,
    step_limit: u64,
    /// `--test-timeout SECS`: wall-clock limit per test (0: none); see `run_workers`.
    test_timeout: u64,
    /// `--test-mem-limit MB`: how much one test may grow its worker (None: the dataset's per-worker
    /// figure plus 3 GB; 0: no limit); see `run_workers`.
    test_mem_mb: Option<u64>,
    /// `--include-disabled`: also run the tests Microsoft's CI disables (src/DisabledTests).
    include_disabled: bool,
    /// The run's start date: output files are `<date>[-label]-...`, fixed once so that a run
    /// across midnight keeps one set of files (see `out_stem`).
    run_date: String,
    /// `--isolation codeunit` (default): run a test codeunit like Microsoft's test runner 130450
    /// (see `run_codeunit`); `--isolation test`: every test method on its own.
    isolation_codeunit: bool,
    /// `--seed-random`: seed Random() per test from its codeunit and name (repeatable data).
    seed_random: bool,
    /// Deterministic runs (default; `--no-bc-runner` turns it off): what Microsoft's test runner does
    /// before every test method (`runner_reset`: Random() seeded to 1 through Library - Random,
    /// Randomize(Seed) seeding as in BC) and the demo company's NumberSequences put back to the
    /// snapshot before every test (`--isolation codeunit`: every codeunit), so a test's outcome
    /// does not depend on the tests that ran before it in its worker.
    bc_runner: bool,
    /// `--clock virtual` (al_runtime::clock): a virtual clock and seeded Guids per isolation unit, so
    /// timestamps, Guids and their sort order repeat between runs; `--clock wall`: the wall clock.
    virtual_clock: bool,
    limit: Option<usize>,
    /// The dataset of the phase being run (`Data::for_suite`, or `--data`).
    data: Data,
    /// `--data` / `--no-seed` was given: every suite runs on `data`.
    data_forced: bool,
    /// `--demo-cache FILE`; default: data/parity/cache/<dataset>-<build key>.wal
    demo_cache: Option<PathBuf>,
    /// `--demo-cache none`: create the company in memory on every run.
    demo_cache_off: bool,
    /// `--work-date today`: keep today's date instead of the demo company's default work date.
    work_date_today: bool,
    /// Internal: this run has one phase, so unknown --codeunit names fail before any test runs.
    codeunits_strict: bool,
    list: bool,
    verbose: bool,
    /// Run the apps' Install codeunits in every new test database (default; `--no-install`).
    install: bool,
    /// Apps to install (lower-case ids): the test apps and the apps they depend on.
    install_ids: Vec<String>,
    /// `--pg CONN` (docs/pg-vs-memory.md): the workers run on PostgreSQL scratch databases named
    /// after CONN's dbname (`bc_server::pgtest`) instead of in-memory copies of the company.
    pg: Option<String>,
    /// `--pg-keep`: leave the workers' databases for a look after the run.
    pg_keep: bool,
    /// `--ext-app DIR,...`: app directories outside vendor/bcapps loaded with suite `ext` (their
    /// app.json dependencies on BCApps apps are resolved as for the built-in suites).
    ext_apps: Vec<PathBuf>,
    /// `--ext-test DIR,...`: test app directories outside vendor/bcapps: suite `ext` runs their
    /// test codeunits (`--ext-test` alone selects `--suite ext`).
    ext_tests: Vec<PathBuf>,
}

/// The session company of every test session: AL_TEST_COMPANY, default "CRONUS".
fn company() -> String {
    std::env::var("AL_TEST_COMPANY").ok().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "CRONUS".into())
}

/// Further companies of the test tenant (AL_TEST_EXTRA_COMPANIES, comma-separated): Company rows
/// only, no data, as the second company of a fresh BC database.
fn extra_companies() -> Vec<String> {
    std::env::var("AL_TEST_EXTRA_COMPANIES").map(|v| v.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect()).unwrap_or_default()
}

fn usage() -> ! {
    eprintln!(
        "usage: al-test [--suite system,foundation,baseapp[:Area,...]] [--app NAME] [--codeunit NAME|ID,...] [--test NAME]\n\
         \x20              [--jobs N] [--label X] [--out DIR] [--step-limit N] [--test-timeout SECS] [--test-mem-limit MB] [--limit N] [--no-seed] [--list] [-v]\n\
         \x20              [--isolation codeunit|test] [--seed-random] [--no-bc-runner] [--include-disabled] [--clock virtual|wall]\n\
         \x20              [--runner-permissions] [--enforce-permissions]   (docs/permissions.md)\n\
         \x20              [--pg 'host=... user=... dbname=PREFIX' [--pg-keep]]   (PostgreSQL scratch databases\n\
         \x20              PREFIX_tpl and PREFIX_w<N> instead of memory, docs/pg-vs-memory.md)\n\
         \x20              [--ext-test DIR,... [--ext-app DIR,...]]   (suite ext: test apps outside vendor/bcapps;\n\
         \x20              BC_AL_SYMBOLS=A,B defines preprocessor symbols, AL_TEST_COMPANY names the session company,\n\
         \x20              AL_TEST_EXTRA_COMPANIES adds Company rows)\n\
         \x20      al-test --serve DIR | al-test --client DIR [arguments]   (opt-in run server, docs/al-test-serve.md)\n\
         Runs are deterministic by default: before every test Random() is seeded to 1 as Microsoft's\n\
         test runner does (codeunit 130301 Library - Random.SetSeed(1)) and the demo company's number\n\
         sequences are put back. --no-bc-runner gives the old behaviour (Random() from UUIDs,\n\
         sequences carried over) for comparisons with runs before 2026-11-08.\n\
         \x20              [--data contoso|w1demo|seed|none] [--demo-cache FILE|none] [--work-date demo|today] [--no-install]\n\n\
         Datasets: baseapp runs on w1demo, system and foundation on contoso, unless --data is given.\n\
         Runs share the machine by memory: each reserves catalog + company + jobs x per-worker MB\n\
         in /tmp/bc-rust-al-test.d and waits, first come first served, until that fits in\n\
         MemAvailable minus a 6 GB margin, plus 1 GB per running run beyond two, at most 10 GB (AL_TEST_MEM_MARGIN_MB\n\
         raises the base; AL_TEST_ADMISSION_DIR gives a private queue). The default --jobs is at most\n\
         16. A run that outgrows its estimate while memory is tight stops taking new codeunits until\n\
         there is room (\"memory guard: pausing\"). Admitted runs hold /tmp/bc-rust-al-test.lock\n\
         shared, so older binaries, which lock it exclusively, still run alone."
    );
    std::process::exit(2)
}

fn parse_opts(root: &Path, args: Vec<String>) -> Opts {
    let mut o = Opts {
        suites: vec!["system".into(), "foundation".into()],
        app: None,
        codeunits: vec![],
        test: None,
        jobs: 0, // default_jobs once the data mode is known
        label: None,
        out: root.join("data/parity"),
        step_limit: 30_000_000,
        test_timeout: 600,
        test_mem_mb: None,
        include_disabled: false,
        run_date: chrono::Local::now().format("%Y-%m-%d").to_string(),
        isolation_codeunit: true,
        seed_random: false,
        bc_runner: true,
        virtual_clock: false,
        limit: None,
        data: Data::Contoso,
        data_forced: false,
        demo_cache: None,
        demo_cache_off: false,
        work_date_today: false,
        codeunits_strict: false,
        list: false,
        verbose: false,
        install: true,
        install_ids: vec![],
        pg: None,
        pg_keep: false,
        ext_apps: vec![],
        ext_tests: vec![],
    };
    let mut suite_given = false;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--suite" | "-s" => {
                o.suites = split_list(&val());
                suite_given = true;
            }
            "--ext-app" => o.ext_apps.extend(val().split(',').map(|d| PathBuf::from(d.trim())).filter(|d| !d.as_os_str().is_empty())),
            "--ext-test" => o.ext_tests.extend(val().split(',').map(|d| PathBuf::from(d.trim())).filter(|d| !d.as_os_str().is_empty())),
            "--app" | "-a" => o.app = Some(val()),
            "--codeunit" | "-c" => o.codeunits.extend(split_names(&val())),
            "--test" | "-t" => o.test = Some(val()),
            "--jobs" | "-j" => o.jobs = val().parse().unwrap_or_else(|_| usage()),
            "--label" => o.label = Some(val()),
            "--out" => o.out = PathBuf::from(val()),
            "--step-limit" => o.step_limit = val().parse().unwrap_or_else(|_| usage()),
            "--test-timeout" => o.test_timeout = val().parse().unwrap_or_else(|_| usage()),
            "--test-mem-limit" => o.test_mem_mb = Some(val().parse().unwrap_or_else(|_| usage())),
            "--isolation" => {
                o.isolation_codeunit = match val().to_ascii_lowercase().as_str() {
                    "codeunit" => true,
                    "test" => false,
                    _ => usage(),
                }
            }
            "--seed-random" => o.seed_random = true,
            "--clock" => {
                o.virtual_clock = match val().to_ascii_lowercase().as_str() {
                    "virtual" => true,
                    "wall" => false,
                    _ => usage(),
                }
            }
            "--bc-runner" => o.bc_runner = true,
            "--no-bc-runner" => o.bc_runner = false,
            "--runner-permissions" => RUNNER_PERMISSIONS.store(true, Ordering::Relaxed),
            "--enforce-permissions" => {
                RUNNER_PERMISSIONS.store(true, Ordering::Relaxed);
                al_runtime::permissions::set_enforce(true);
            }
            "--include-disabled" => o.include_disabled = true,
            "--limit" => o.limit = Some(val().parse().unwrap_or_else(|_| usage())),
            "--no-seed" => {
                o.data = Data::None;
                o.data_forced = true;
            }
            "--data" => {
                o.data_forced = true;
                o.data = match val().to_ascii_lowercase().as_str() {
                    "seed" => Data::Seed,
                    "contoso" | "demo" => Data::Contoso,
                    "w1demo" | "w1" | "legacy" => Data::W1Demo,
                    "none" => Data::None,
                    _ => usage(),
                }
            }
            "--demo-cache" => {
                let v = val();
                if v.eq_ignore_ascii_case("none") {
                    o.demo_cache_off = true;
                } else {
                    o.demo_cache = Some(PathBuf::from(v));
                }
            }
            "--work-date" => {
                o.work_date_today = match val().to_ascii_lowercase().as_str() {
                    "today" => true,
                    "demo" => false,
                    _ => usage(),
                }
            }
            "--pg" => o.pg = Some(val()),
            "--pg-keep" => o.pg_keep = true,
            "--list" => o.list = true,
            "--no-install" => o.install = false,
            "-v" | "--verbose" => o.verbose = true,
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    if !o.ext_tests.is_empty() && !suite_given {
        o.suites = vec!["ext".into()];
    }
    o
}

/// Memory of an al-test run: the catalog and shared state, plus each worker's database and its
/// sessions (`--data contoso` gives every worker a copy-on-write view of the one CRONUS company,
/// `demo::cow_backend`). Peak RSS measured 2026-11-07 with /usr/bin/time at 4 and 12 jobs: base
/// 1.9-4.6 GB, per worker 0.07 GB (seed) and 0.08 GB (contoso, 0.18 GB with a full copy per
/// worker), for both the System App + Business Foundation and the ERM-Sales + SMB suites.
/// A worker grows with the number of tables and objects its tests touch (its Runtime's schemas
/// and caches), so the full system,foundation,baseapp suite costs far more per worker: base
/// 4.7 GB, then a plateau of 18.3 GB at 24 jobs (0.57 GB per worker) and a peak of 25.3 GB at 48
/// jobs (0.43 GB), same wall time (449 s and 470 s). WORKER_MB_CONTOSO covers the full suite.
/// The full baseapp suite on w1demo (42,146 tests, 2026-11-07) peaked at 20.2 GB with 12 jobs:
/// (20.2 - 5) / 12 = 1.27 GB per worker, so WORKER_MB_W1DEMO is 1,300 MB. On 11 Base App areas
/// (19,574 tests, 10 jobs) w1demo peaked at 9.6 GB and contoso at 12.1 GB: the full suite's
/// heavier areas (SCM-Service, Planning, CRM) and single tests that spike dominate.
/// The old single figure BASE_MB (5 GB) is split for admission: a loaded catalog (1.4 GB for the
/// default apps, about 2 GB with all test apps), the demo company loaded from its cache, or the
/// creation peak when it has to be built (4.7 GB with the catalog).
const CATALOG_MB: u64 = 2_500;
const MASTER_LOAD_MB: u64 = 1_000;
const MASTER_BUILD_MB: u64 = 3_500;
const WORKER_MB_SEED: u64 = 150;
/// Recalibrated 2026-11-08 from measured peaks: a worker's caches (its Runtime's schemas and
/// virtual tables) grow with the codeunits it runs and saturate at about 1.2-1.3 GB, so a run's
/// workers take about min(workers x WORKER_MB, codeunits x MB_PER_CODEUNIT), at least
/// MIN_WORKER_MB each. Measured: full contoso (1,606 codeunits) at 16 jobs 21.3 GB (mimalloc,
/// I6; model 22.1), full w1demo baseapp (1,418) at 12 jobs 20.2 GB (glibc; model 20.2), six areas
/// (345) at 8 jobs 6.2 GB (model 9.5), ERM-Sales + SMB (70) at 7 jobs 3.97 GB (mimalloc; model
/// 6.7). The old flat 600 MB per contoso worker gave a full run a 15.6 GB first grant for a
/// 21 GB peak.
/// Raised again after mimalloc v2 replaced v3 (02:27 UTC), which holds about 15% more (4.59 vs
/// 3.97 GB on ERM-Sales + SMB): two runs were OOM-killed at 02:56 and 03:13 UTC, P9's 6-job
/// runs at 1.6 GB per worker and still growing, P12's run at 48 implicit jobs at 21.5 GB on a
/// 20.2 GB estimate. Every worker caches the common objects again, so the per-codeunit cap and
/// the per-worker floor went up too, and the default --jobs is capped at DEFAULT_JOBS_MAX.
/// Recalibrated again 2026-11-08 06:40 UTC after the Rc-cycle leak fix (Session drop clears its
/// globals): the earlier figures measured the leak (about 0.3 MB per test per worker, never
/// freed). Full w1demo baseapp (1,418 codeunits, 42,146 tests) at 6 jobs, system allocator: peak
/// RSS 4.11 GB, worker live memory 112-223 MB each. 400 MB per worker covers that twice over;
/// the memory guard and the per-test cap handle the rest.
/// Recalibrated 2026-11-08 13:00 UTC for `--isolation codeunit` (the default since 09:00): a
/// codeunit's session keeps the rows its methods wrote until the codeunit ends, so a worker holds
/// more than with per-test isolation. Fitted on full runs, peak RSS = fixed + workers x per-worker:
/// w1demo 5.3 GB at 6 jobs and 7.3 GB at 10 (I10), Contoso 5.1 GB at 6 jobs (P8, ab67) and
/// 9.9 GB at 12 (I10, which includes one 2.9 GB test): about 2.3 GB fixed and 0.5 GB per worker.
const WORKER_MB_CONTOSO: u64 = 512;
const WORKER_MB_W1DEMO: u64 = 512;
/// The grant above the measured peak RSS, for single tests that spike: a base and a little per
/// worker, raised to 1.25 x the largest growth of one test seen so far in the run (at most
/// HEADROOM_MAX_MB). It was max(jobs x 400 MB, 2 GB): 4.8 GB above the peak for a 12-job run,
/// while only one of Contoso's 44,430 tests grows its worker by more than 256 MB (Media Cleanup
/// Test EnsureLargeDetachedMediaIsDetected, 2.9 GB). Other runs' unused grants count against a
/// waiting run (admission `reserve`), so the slack queued runs (I10's Contoso run waited 182 s).
/// The memory guard and the host margin stay as they were.
/// Reverted 2026-11-08 14:50 UTC after the host OOM at 14:41 (P8): the base is the old
/// max(jobs x 400 MB, 2 GB) again until admission is re-fitted; the spike rule only raises it.
const HEADROOM_MIN_MB: u64 = 2_048;
const HEADROOM_PER_WORKER_MB: u64 = 400;
const HEADROOM_MAX_MB: u64 = 4_096;

/// The run's memory figures for the summary: peak RSS, the largest grant, time waiting for
/// admission.
static PEAK_RSS_MB: AtomicU64 = AtomicU64::new(0);
static MAX_GRANT_MB: AtomicU64 = AtomicU64::new(0);
static ADMIT_WAIT_MS: AtomicU64 = AtomicU64::new(0);
const MB_PER_CODEUNIT: u64 = 14;
const MIN_WORKER_MB: u64 = 200;
/// Workers when --jobs is not given, whatever the memory: per-worker overhead isn't linear, and an
/// implicit 48 jobs (P12's run) is the largest single reservation on the box.
const DEFAULT_JOBS_MAX: usize = 16;

/// Memory for a run's workers (see WORKER_MB_CONTOSO).
fn workers_mb(data: Data, workers: usize, codeunits: usize) -> u64 {
    (workers as u64 * data.worker_mb()).min(codeunits as u64 * MB_PER_CODEUNIT).max(workers as u64 * MIN_WORKER_MB)
}

/// Split `a,b:c,d` at commas, keeping `baseapp:X,Y` area lists together.
/// `--suite` values: comma-separated suites, where `suite:area,area` keeps its areas together.
/// A part continues the previous one's areas only when that one starts with a known suite and a
/// colon and this part doesn't start with a suite name.
fn split_list(s: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let is_suite = |x: &str| al_model::TEST_SUITES.iter().any(|t| x.eq_ignore_ascii_case(t));
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let last_has_areas = out.last().and_then(|l| l.split_once(':')).is_some_and(|(suite, _)| is_suite(suite.trim()));
        let continues_areas = last_has_areas && !al_model::TEST_SUITES.iter().any(|t| part.to_ascii_lowercase().starts_with(t));
        if continues_areas {
            let l = out.last_mut().unwrap();
            l.push(',');
            l.push_str(part);
        } else {
            out.push(part.to_string());
        }
    }
    out
}

/// `--codeunit` values: names or ids separated by commas. Names may contain colons ("No
/// Accounting Periods: Basic"); a name with a comma goes in double quotes.
fn split_names(s: &str) -> Vec<String> {
    let mut out = vec![];
    let (mut cur, mut quoted) = (String::new(), false);
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

/// Per --codeunit filter: whether some phase of this run has a test codeunit it matches (reset by
/// run_args; a filter that matches nothing is an error, not a silently smaller run).
static CODEUNIT_HITS: Mutex<Vec<bool>> = Mutex::new(Vec::new());
/// Set by run_phase when a single-phase run has --codeunit filters that match nothing.
static CODEUNIT_FILTER_ERROR: AtomicBool = AtomicBool::new(false);

fn repo_root() -> PathBuf {
    let candidates = [std::env::current_dir().ok(), Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))];
    for c in candidates.into_iter().flatten() {
        if c.join("vendor/bcapps/src").exists() {
            return bc_server::app::canonical(c);
        }
    }
    eprintln!("vendor/bcapps not found (run from the repo root)");
    std::process::exit(1)
}

/// Name of the app (app.json) a source file belongs to.
fn app_name(file: &Path, cache: &mut HashMap<PathBuf, String>) -> String {
    let mut d = file.parent();
    while let Some(dir) = d {
        if let Some(n) = cache.get(dir) {
            return n.clone();
        }
        let aj = dir.join("app.json");
        if aj.exists() {
            let name = std::fs::read_to_string(&aj)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s.trim_start_matches('\u{feff}')).ok())
                .and_then(|j| j.get("name").and_then(|n| n.as_str()).map(String::from))
                .unwrap_or_else(|| dir.file_name().unwrap_or_default().to_string_lossy().into_owned());
            cache.insert(dir.to_path_buf(), name.clone());
            return name;
        }
        d = dir.parent();
    }
    "?".into()
}

#[derive(Clone)]
struct Case {
    code: u32,
    proc: u32,
    codeunit_id: i64,
    codeunit: String,
    test: String,
    app: String,
}

#[derive(serde::Serialize, Clone)]
struct TestResult {
    app: String,
    codeunit_id: i64,
    codeunit: String,
    test: String,
    /// pass | fail (the AL code ran and an error/assertion failed) | error (bc-rust could not run it)
    outcome: &'static str,
    message: String,
    code: String,
    stack: Vec<String>,
    ms: f64,
    steps: u64,
    /// The dataset the test ran on (contoso, w1demo, seed, none).
    data: &'static str,
    /// Microsoft's CI disables this test (src/DisabledTests): outcome "disabled" (not run), or with
    /// --include-disabled its real outcome, counted apart from the CI-comparable figure.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    disabled: bool,
    /// Outcome class (I14, docs/assumptions.md), next to `outcome`, which keeps its values:
    /// pass | flagged_pass (passed, but touched a guessed default of al_runtime::assume::REGISTRY
    /// other than the skipped checks of assume::NOT_FLAGGING)
    /// | not_trusted (passed, but a refusal was swallowed by asserterror, a TryFunction or
    /// `if Codeunit.Run()` on its path) | fail (outcome fail or error) | not_run (disabled and not
    /// run, or never started because its codeunit was abandoned).
    class: &'static str,
    /// The guessed defaults the test touched (assume::REGISTRY names).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    assumed: Vec<&'static str>,
    /// The refusals swallowed on the test's path (first lines, at most assume::MAX_SWALLOWED).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    swallowed: Vec<String>,
}

/// TestResult::class of a finished test from its outcome and what it touched (al_runtime::assume).
fn outcome_class(outcome: &str, t: &al_runtime::assume::Record) -> &'static str {
    match outcome {
        "pass" if t.swallowed_n > 0 => "not_trusted",
        "pass" if t.flagging() => "flagged_pass",
        "pass" => "pass",
        "disabled" => "not_run",
        _ => "fail",
    }
}

/// The tests Microsoft's CI does not run: BCApps reads `src/DisabledTests/<app name, spaces as
/// _>/*.json` for each test app it runs (build/scripts/ParallelTestExecution.psm1,
/// Get-DisabledTestsForApp) and passes them to the test runner. Each file holds one entry or an
/// array of `{"codeunitId", "codeunitName", "method"}`; method "*" is the whole codeunit.
/// Codeunit id -> lower-case methods.
fn disabled_tests(root: &Path, apps: impl Iterator<Item = String>) -> HashMap<i64, std::collections::HashSet<String>> {
    let base = root.join("vendor/bcapps/src/DisabledTests");
    let mut out: HashMap<i64, std::collections::HashSet<String>> = HashMap::new();
    for app in apps {
        let Ok(rd) = std::fs::read_dir(base.join(app.replace(' ', "_"))) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let v: serde_json::Value = match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
                Ok(v) => v,
                Err(err) => {
                    eprintln!("warning: {}: {err}", path.display());
                    continue;
                }
            };
            let items = match v {
                serde_json::Value::Array(a) => a,
                one => vec![one],
            };
            for it in items {
                if let (Some(id), Some(m)) = (it["codeunitId"].as_i64(), it["method"].as_str()) {
                    out.entry(id).or_default().insert(m.to_ascii_lowercase());
                }
            }
        }
    }
    out
}

fn is_disabled(d: &HashMap<i64, std::collections::HashSet<String>>, c: &Case) -> bool {
    d.get(&c.codeunit_id).is_some_and(|m| m.contains("*") || m.contains(&c.test.to_ascii_lowercase()))
}

/// Messages that mean bc-rust itself could not execute the code (as opposed to AL raising an error).
const RUNTIME_GAPS: &[&str] = &[
    "Not implemented in bc-rust",
    "Unknown variable",
    "Method '",
    "Cannot call expression",
    "Cannot index",
    "Cannot resolve",
    "Record method",
    "Range outside set",
    "Set outside",
    "'in' requires",
    "Expected a field reference",
    "Step limit exceeded",
    "Maximum call depth",
    "panic:",
    "Handler function",
    "The RecordRef is not open",
];

fn classify(msg: &str) -> &'static str {
    if RUNTIME_GAPS.iter().any(|p| msg.starts_with(p)) || (msg.starts_with("Field '") && msg.contains("does not exist in table")) {
        "error"
    } else {
        "fail"
    }
}

/// Group key for an error message: the first line with literal values blanked out.
fn cause(msg: &str) -> String {
    let line = msg.lines().next().unwrap_or("").trim();
    if line.starts_with("Not implemented in bc-rust") || line.starts_with("Method '") || line.starts_with("Unknown variable") {
        return line.chars().take(120).collect();
    }
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' | '<' => {
                let close = if c == '<' { '>' } else { c };
                out.push(c);
                out.push('…');
                for d in chars.by_ref() {
                    if d == close {
                        break;
                    }
                }
                out.push(close);
            }
            '0'..='9' => {
                while chars.peek().is_some_and(|d| d.is_ascii_digit() || *d == '.' || *d == ',') {
                    chars.next();
                }
                out.push('#');
            }
            _ => out.push(c),
        }
    }
    out.chars().take(110).collect()
}

thread_local! {
    static PANIC_MSG: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The test session logs in: LogInManagement.LogInStart sets the application area from the
/// company's Application Area Setup (Contoso: the Essential experience; W1 demo: none).
fn login(s: &mut Session, cat: &'static Catalog) {
    if let Some(ci) = cat.code.iter().position(|x| x.kind == ObjectKind::Codeunit && x.name == "Application Area Mgmt. Facade") {
        if let Some(&p) = cat.code[ci].find("SetupApplicationArea").first() {
            let ci = ci as u32;
            let _ = s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None);
        }
    }
}

fn run_case(s: &mut Session, cat: &'static Catalog, c: &Case) -> (Result<(), (String, &'static str, Vec<String>)>, u64) {
    let inst = Instance::new(c.code);
    match TestCtx::new(cat, inst.clone(), c.proc) {
        Ok(ctx) => s.test = Some(ctx),
        Err(m) => return (Err((m, "", vec![])), 0),
    }
    al_runtime::assume::reset();
    let steps0 = s.steps;
    let co = &cat.code[c.code as usize];
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), Flow> {
        if let Some(t) = co.trigger("onrun") {
            s.invoke(ProcRef { code: c.code, proc: t }, vec![], Some(inst.clone()), None, None)?;
        }
        s.invoke(ProcRef { code: c.code, proc: c.proc }, vec![], Some(inst.clone()), None, None)?;
        Ok(())
    }));
    let steps = s.steps - steps0;
    let out = match res {
        Ok(Ok(())) => {
            let missing = s.test.as_ref().map(|t| t.not_executed()).unwrap_or_default();
            if missing.is_empty() {
                Ok(())
            } else {
                Err((format!("UI handlers declared but not called: {}", missing.join(", ")), "", vec![])) // own wording [src:X23]
            }
        }
        Ok(Err(Flow::Err(e))) => Err((e.msg.clone(), e.code, e.stack.clone())),
        Ok(Err(_)) => Ok(()),
        Err(_) => {
            let m = PANIC_MSG.with(|p| p.borrow_mut().take()).unwrap_or_else(|| "unknown".into());
            Err((format!("panic: {m}"), "", vec![]))
        }
    };
    s.test = None;
    s.rollback();
    (out, steps)
}

/// `--clock virtual`: where the virtual clock starts, the run date at `hour`:00 local time (the
/// demo company is built from 01:00, the tests start at 10:00, so its rows are older than theirs).
fn clock_start(opts: &Opts, hour: u32) -> i64 {
    let d = chrono::NaiveDate::parse_from_str(&opts.run_date, "%Y-%m-%d").unwrap_or_default();
    al_runtime::clock::local_to_ms(d.and_hms_opt(hour, 0, 0).unwrap_or_default())
}

/// `--clock virtual`: start the clock and the Guid sequence of an isolation unit (a codeunit, or a
/// test with `--isolation test`), seeded by the codeunit and the test.
fn start_unit_clock(opts: &Opts, codeunit_id: i64, test: Option<&str>) {
    use std::hash::{Hash, Hasher};
    if opts.virtual_clock {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ("clock", codeunit_id, test).hash(&mut h);
        al_runtime::clock::set_virtual(clock_start(opts, 10), h.finish());
    } else {
        // a served run's worker thread may have run a --clock virtual request before
        al_runtime::clock::set_wall();
    }
}

/// The Random() seed of a test: its codeunit and name.
fn case_seed(c: &Case) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (c.codeunit_id, &c.test).hash(&mut h);
    h.finish() | 1
}

/// Call a procedure of a codeunit, on its single instance when it is SingleInstance (errors
/// ignored). Nothing happens when the codeunit or procedure is not loaded.
fn call_lib(s: &mut Session, id: i64, name: &str, args: Vec<al_runtime::Val>) {
    let Some(&ci) = s.cat.code_by_key.get(&(ObjectKind::Codeunit, id)) else { return };
    let Some(p) = s.find_proc(ci, name, args.len()) else { return };
    let inst = if s.cat.code[ci as usize].single_instance { s.single_instance(ci) } else { Instance::new(ci) };
    let _ = s.invoke(ProcRef { code: ci, proc: p }, args.into_iter().map(al_runtime::interp::Arg::Val).collect(), Some(inst), None, None);
}

/// `--runner-permissions` (opt-in): `runner_reset` also runs the permission part of Microsoft's
/// runner reset (`runner_permissions`, docs/permissions.md).
static RUNNER_PERMISSIONS: AtomicBool = AtomicBool::new(false);

/// The permission part of codeunit 130453's ClearLegacyLibraries: 130301 "Reset State Before Test
/// Run".RunLegacyPermissionSet (Permission Test Catalog.InitializePermissionSetForTest(Disabled):
/// Permissions Mock started, no effective permission sets), then, unless the method's
/// TestPermissions is Disabled, codeunit 130302 "Set State Test Run" (D365 Full Access, All
/// Objects, TEST TABLES, D365 BASIC and LOCAL as the effective permission sets).
fn runner_permissions(s: &mut Session, cat: &'static Catalog, c: &Case) {
    if !RUNNER_PERMISSIONS.load(Ordering::Relaxed) {
        return;
    }
    call_lib(s, 130301, "RunLegacyPermissionSet", vec![]);
    if al_runtime::permissions::test_permissions(cat, c.code, c.proc) != al_runtime::permissions::TestPermissions::Disabled
        && cat.code_by_key.contains_key(&(ObjectKind::Codeunit, 130302))
    {
        let _ = s.run_codeunit(130302, None);
    }
}

/// What Microsoft's test runner does before every test method (codeunit 130453 "ALTestRunner Reset
/// Environment", subscriber of "Test Runner - Mgt".OnBeforeTestMethodRun), without the permission
/// part: ClearLastError; ApplicationArea('') (nothing to do in bc-rust, which does not filter by
/// application area); from codeunit 130301 "Reset State Before Test Run" `Library - Random`
/// .SetSeed(1) (Randomize(1): Random() starts from seed 1, as in BC) and `Library - Notification
/// Mgt.`.ClearTemporaryNotificationContext; and codeunit 132553 "Block Changes to System Tables"
/// when it is loaded. 130301's RunLegacyPermissionSet and codeunit 130302 need permission sets as
/// data, which bc-rust does not have (docs/test-isolation.md).
fn runner_reset(s: &mut Session) {
    al_runtime::builtins::seed_random(0);
    s.last_error.clear();
    s.last_error_code.clear();
    s.last_error_stack.clear();
    call_lib(s, 130440, "SetSeed", vec![al_runtime::Val::Int(1)]);
    call_lib(s, 132222, "ClearTemporaryNotificationContext", vec![]);
    if s.cat.code_by_key.contains_key(&(ObjectKind::Codeunit, 132553)) {
        let _ = s.run_codeunit(132553, None);
    }
}

/// Whether a test method is [TransactionModel(AutoRollback)].
fn auto_rollback(cat: &'static Catalog, c: &Case) -> bool {
    cat.code[c.code as usize].procs[c.proc as usize]
        .attributes
        .iter()
        .any(|a| a.name.eq_ignore_ascii_case("transactionmodel") && format!("{:?}", a.args).to_ascii_lowercase().contains("autorollback"))
}

/// One method of a test codeunit run as a unit (`--isolation codeunit`) on the codeunit's session
/// and instance (`onrun`: run OnRun first). The method's changes stay unless it fails (rolled back
/// to its last Commit(), which is a savepoint in tests) or is AutoRollback.
fn run_method(s: &mut Session, cat: &'static Catalog, inst: &Rc<Instance>, c: &Case, onrun: bool, bc_runner: bool) -> (Result<(), (String, &'static str, Vec<String>)>, u64) {
    match TestCtx::new(cat, inst.clone(), c.proc) {
        Ok(ctx) => s.test = Some(ctx),
        Err(m) => return (Err((m, "", vec![])), 0),
    }
    let sp = s.db.savepoint().unwrap_or(0);
    if let Some(t) = s.test.as_mut() {
        t.commit_mark = sp;
    }
    if bc_runner {
        runner_reset(s);
        runner_permissions(s, cat, c);
    }
    al_runtime::assume::reset();
    let steps0 = s.steps;
    let co = &cat.code[c.code as usize];
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), Flow> {
        if onrun {
            if let Some(t) = co.trigger("onrun") {
                s.invoke(ProcRef { code: c.code, proc: t }, vec![], Some(inst.clone()), None, None)?;
            }
        }
        s.invoke(ProcRef { code: c.code, proc: c.proc }, vec![], Some(inst.clone()), None, None)?;
        Ok(())
    }));
    let steps = s.steps - steps0;
    let out = match res {
        Ok(Ok(())) => {
            let missing = s.test.as_ref().map(|t| t.not_executed()).unwrap_or_default();
            if missing.is_empty() {
                Ok(())
            } else {
                Err((format!("UI handlers declared but not called: {}", missing.join(", ")), "", vec![])) // own wording [src:X23]
            }
        }
        Ok(Err(Flow::Err(e))) => Err((e.msg.clone(), e.code, e.stack.clone())),
        Ok(Err(_)) => Ok(()),
        Err(_) => {
            let m = PANIC_MSG.with(|p| p.borrow_mut().take()).unwrap_or_else(|| "unknown".into());
            Err((format!("panic: {m}"), "", vec![]))
        }
    };
    if out.is_err() {
        s.rollback_to_commit();
    } else if auto_rollback(cat, c) {
        let _ = s.db.rollback_to(sp);
    }
    // no later method rolls back to this one's savepoints (its start, its Commit()s): release
    // them, or on PostgreSQL they nest deeper with every method (bc_store::Conn::release)
    let _ = s.db.release(sp);
    s.test = None;
    (out, steps)
}

struct Shared {
    /// What the Install codeunits did in the first test database (one line).
    install: Mutex<Option<String>>,
    /// Codeunits still to run (the rest of a timed-out codeunit goes back to the front).
    cases: Mutex<std::collections::VecDeque<Vec<Case>>>,
    results: Mutex<Vec<TestResult>>,
    missing: Mutex<BTreeMap<String, usize>>,
    done: AtomicUsize,
    total: usize,
    /// Per worker: the running test (start, test, the rest of its codeunit) and whether the
    /// watchdog abandoned the worker.
    slots: Mutex<Vec<Slot>>,
    /// Workers that have not finished (abandoned ones excluded).
    alive: AtomicUsize,
    /// `<out>/<stem>-results.jsonl`, appended as tests finish (rewritten sorted at the end), and
    /// the progress checkpoint `<out>/<stem>-progress.txt`.
    jsonl: Option<Mutex<std::fs::File>>,
    progress: PathBuf,
    /// Set by the memory guard in run_workers: workers finish their codeunit and take no new one.
    paused: AtomicBool,
    /// Tests that grew their worker by more than 256 MB: (bytes, "codeunit :: test").
    mem_top: Mutex<Vec<(i64, String)>>,
    /// Microsoft's CI's disabled tests (see `disabled_tests`), to flag their results.
    disabled: HashMap<i64, std::collections::HashSet<String>>,
    /// `<out>/<stem>-started.log`: a line per test as it starts, so that after a crash the tests
    /// started but not in the results file are the ones that were running (deleted when the run
    /// completes).
    started: Option<Mutex<std::fs::File>>,
}

/// Record a finished test: in memory and, at once, as a line of the results file, so a killed or
/// hung run keeps what it did.
fn record(sh: &Shared, r: TestResult) {
    if let Some(f) = &sh.jsonl {
        if let Ok(line) = serde_json::to_string(&r) {
            let mut f = f.lock().unwrap();
            let _ = writeln!(f, "{line}");
        }
    }
    sh.results.lock().unwrap().push(r);
}

/// `<date>[-label]`: the name stem of the run's output files.
fn out_stem(opts: &Opts) -> String {
    let date = opts.run_date.clone();
    match &opts.label {
        Some(l) => format!("{date}-{l}"),
        None => date,
    }
}

#[derive(Default)]
struct Slot {
    running: Option<(std::time::Instant, Case, Vec<Case>)>,
    abandoned: bool,
    /// The worker thread's live bytes (counted by the allocator) and their value when the
    /// running test started; the most the running test has grown them; the flag that stops it.
    mem: Option<&'static std::sync::atomic::AtomicI64>,
    base: i64,
    peak: i64,
    abort: Arc<AtomicBool>,
}

/// The demo company every worker copies (`--data contoso`).
struct Master {
    db: Arc<MemBackend>,
    seqs: Vec<SeqState>,
    /// WorkDate of a session that opens the demo company (LogInManagement.GetDefaultWorkDate).
    work_date: Option<i32>,
}

/// Install the loaded apps in a new database (their Install codeunits), as BC does when they are
/// installed in the tenant. Returns a one-line summary.
fn install_apps(s: &mut Session, opts: &Opts) -> String {
    // install code runs once per database: allow it more steps than a single test
    s.step_limit = s.steps.saturating_add(opts.step_limit.saturating_mul(10));
    let only = &opts.install_ids;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.install_apps_where(true, &|m| only.contains(&m.id.to_ascii_lowercase()))));
    s.step_limit = u64::MAX;
    match r {
        Ok(Ok(rep)) => {
            let mut line = format!("{} install triggers ran ({})", rep.ran.len(), rep.apps.join(", "));
            if !rep.failed.is_empty() {
                line.push_str(&format!(", {} apps failed (rolled back):", rep.failed.len()));
                for (app, e) in &rep.failed {
                    line.push_str(&format!(" [{app}: {}]", e.lines().next().unwrap_or("")));
                }
            }
            line
        }
        Ok(Err(Flow::Err(e))) => format!("install failed: {}", e.msg),
        Ok(Err(_)) => "install stopped".into(),
        Err(_) => format!("install panicked: {}", PANIC_MSG.with(|p| p.borrow_mut().take()).unwrap_or_default()),
    }
}

fn worker(cat: &'static Catalog, sh: Arc<Shared>, opts: Arc<Opts>, master: Option<Arc<Master>>, me: usize) {
    let (sh, opts, master) = (&*sh, &*opts, master.as_deref());
    // the per-test memory cap: this thread's live bytes and its abort flag
    let mem: &'static std::sync::atomic::AtomicI64 = Box::leak(Box::new(std::sync::atomic::AtomicI64::new(0)));
    WORKER_BYTES.with(|c| c.set(mem as *const _));
    let abort = {
        let mut slots = sh.slots.lock().unwrap();
        slots[me].mem = Some(mem);
        slots[me].abort.clone()
    };
    al_runtime::stackguard::register_abort(abort.clone());
    al_runtime::builtins::set_randomize_seeds(opts.bc_runner);
    // AutoIncrement keeps SQL identity semantics: not rolled back; reset with the sequences
    al_runtime::recops::set_identity_autoincrement(true);
    if opts.virtual_clock {
        al_runtime::clock::set_virtual(clock_start(opts, 1), 0x5EED);
    }
    let rt = match master {
        // a copy-on-write view of the demo company: the worker stores only the rows its running
        // test changed (rolled back after each test), the master is shared and never written
        Some(m) => match PG_SCRATCH.lock().unwrap().clone() {
            // --pg: the worker's own clone of the template database
            Some(pg) => match pg.worker_backend(me) {
                Ok(b) => Runtime::new(cat, Arc::new(b)),
                Err(e) => {
                    eprintln!("{e:#}");
                    std::process::exit(2)
                }
            },
            None => Runtime::new(cat, Arc::new(demo::cow_backend(&m.db, &m.seqs).expect("demo company view"))),
        },
        None => {
            let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
            {
                // platform rows of a new tenant (company, user, profiles), then the demo company data
                let mut s = Session::new(&rt, &company());
                if let Err(Flow::Err(e)) = s.init_platform_data() {
                    eprintln!("platform data failed: {}", e.msg);
                }
                if opts.install {
                    let line = install_apps(&mut s, opts);
                    sh.install.lock().unwrap().get_or_insert(line);
                }
                if opts.data == Data::Seed {
                    if let Err(e) = bc_server::seed::seed(&mut s) {
                        eprintln!("seed failed: {e:#}");
                    }
                }
                s.commit().expect("commit seed");
            }
            rt
        }
    };
    let mut missing: BTreeMap<String, usize> = BTreeMap::new();
    loop {
        // the memory guard (run_workers) holds new codeunits back while the host is tight
        while sh.paused.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        let Some(group) = sh.cases.lock().unwrap().pop_front() else { break };
        start_unit_clock(opts, group[0].codeunit_id, None);
        let work_date = master.and_then(|m| m.work_date).filter(|_| !opts.work_date_today);
        // --isolation codeunit: one session and instance for the whole codeunit
        let mut unit: Option<(Session, Rc<Instance>)> = opts.isolation_codeunit.then(|| {
            let mut s = Session::new(&rt, &company());
            if let Some(d) = work_date {
                s.work_date = d;
            }
            if let (true, Some(m)) = (opts.bc_runner, master) {
                let _ = demo::restore_sequences(s.db.as_mut(), &m.seqs);
            }
            login(&mut s, cat);
            (s, Instance::new(group[0].code))
        });
        for (k, c) in group.iter().enumerate() {
            {
                let mut slots = sh.slots.lock().unwrap();
                slots[me].running = Some((std::time::Instant::now(), c.clone(), group[k + 1..].to_vec()));
                slots[me].base = mem.load(Ordering::Relaxed);
                slots[me].peak = 0;
                abort.store(false, Ordering::SeqCst);
            }
            if let Some(f) = &sh.started {
                let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
                let _ = writeln!(f.lock().unwrap(), "{ms} worker {me}: {} :: {}", c.codeunit, c.test);
            }
            if opts.seed_random {
                al_runtime::builtins::seed_random(case_seed(c));
            }
            let t0 = std::time::Instant::now();
            let (r, steps, shimmed) = match unit.as_mut() {
                Some((us, inst)) => {
                    us.step_limit = us.steps.saturating_add(opts.step_limit);
                    let inst = inst.clone();
                    // BC's test runner restores the session's application area after each test
                    // method (NavTestCodeunit: applicationAreasBeforeTestRun)
                    let areas = us.sys.app_area.clone();
                    let (r, steps) = run_method(us, cat, &inst, c, k == 0, opts.bc_runner);
                    // values a method left in the codeunit's "Library - Variable Storage" queue
                    // reach the next methods (the queue is a global of the test codeunit, in BC
                    // too, where no passing test leaves one): name the method, it points at a
                    // handler or path that BC runs and bc-rust doesn't
                    if let Some(n) = queue_leftovers(us, &inst) {
                        let line = format!("{} :: {} left {n} value(s) in Library - Variable Storage ({})\n", c.codeunit, c.test, if r.is_ok() { "passed" } else { "failed" });
                        eprint!("queue leftover: {line}");
                        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(opts.out.join(format!("{}-queue-leftovers.txt", out_stem(opts)))) {
                            let _ = f.write_all(line.as_bytes());
                        }
                    }
                    us.sys.app_area = areas;
                    (r, steps, std::mem::take(&mut us.missing))
                }
                None => {
                    start_unit_clock(opts, c.codeunit_id, Some(&c.test));
                    let mut s = Session::new(&rt, &company());
                    s.step_limit = opts.step_limit;
                    if let Some(d) = work_date {
                        s.work_date = d;
                    }
                    if opts.bc_runner {
                        // NumberSequences are not transactional (as in BC): an earlier test's
                        // sequence changes would otherwise carry into this one
                        if let Some(m) = master {
                            let _ = demo::restore_sequences(s.db.as_mut(), &m.seqs);
                        }
                        runner_reset(&mut s);
                        runner_permissions(&mut s, cat, c);
                    }
                    login(&mut s, cat);
                    let (r, steps) = run_case(&mut s, cat, c);
                    (r, steps, std::mem::take(&mut s.missing))
                }
            };
            // the guessed defaults and swallowed refusals of this test (al_runtime::assume)
            let touched = al_runtime::assume::take();
            for (k, v) in shimmed {
                *missing.entry(k).or_default() += v;
            }
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            let peak = {
                // the watchdog gave this test up and requeued the rest: leave quietly
                let mut slots = sh.slots.lock().unwrap();
                if slots[me].abandoned {
                    return;
                }
                slots[me].running = None;
                slots[me].peak.max(mem.load(Ordering::Relaxed) - slots[me].base)
            };
            let stopped = abort.swap(false, Ordering::SeqCst);
            if peak > 256 << 20 {
                sh.mem_top.lock().unwrap().push((peak, format!("{} :: {}", c.codeunit, c.test)));
            }
            let (outcome, message, code, stack) = match r {
                _ if stopped => (
                    "error",
                    format!("memory limit exceeded: {} :: {} (grew its worker by {:.1} GB; --test-mem-limit {} MB)", c.codeunit, c.test, peak as f64 / (1u64 << 30) as f64, test_mem_limit_mb(opts)),
                    "MemoryLimit".to_string(),
                    vec![],
                ),
                Ok(()) => ("pass", String::new(), String::new(), vec![]),
                Err((m, code, stack)) => (classify(&m), m, code.to_string(), stack),
            };
            let n = sh.done.fetch_add(1, Ordering::SeqCst) + 1;
            if opts.verbose {
                eprintln!("[{n}/{}] {outcome:5} {} :: {} ({ms:.0} ms) {}", sh.total, c.codeunit, c.test, message.lines().next().unwrap_or(""));
            } else if n % 200 == 0 {
                eprintln!("  {n}/{} tests", sh.total);
            }
            let class = outcome_class(outcome, &touched);
            let (assumed, swallowed) = (touched.assumed.iter().map(|x| x.0).collect(), touched.swallowed);
            record(sh, TestResult { app: c.app.clone(), codeunit_id: c.codeunit_id, codeunit: c.codeunit.clone(), test: c.test.clone(), outcome, message, code, stack, ms, steps, data: opts.data.name(), disabled: is_disabled(&sh.disabled, c), class, assumed, swallowed });
            // AL_TEST_MEM_TRACE=1: the worker's live bytes and the process RSS after every test
            // (<stem>-memtrace.tsv), to find memory that outlives its test
            if std::env::var_os("AL_TEST_MEM_TRACE").is_some() {
                let line = format!("{n}\t{me}\t{:.1}\t{}\t{} :: {}\n", mem.load(Ordering::Relaxed) as f64 / (1u64 << 20) as f64, bc_server::admission::rss_mb(std::process::id()).unwrap_or(0), c.codeunit, c.test);
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(opts.out.join(format!("{}-memtrace.tsv", out_stem(opts)))) {
                    let _ = f.write_all(line.as_bytes());
                }
            }
        }
        // the codeunit's isolation ends: back to the company as it was
        if let Some((mut us, _)) = unit.take() {
            us.test = None;
            us.rollback();
        }
    }
    let mut m = sh.missing.lock().unwrap();
    for (k, v) in missing {
        *m.entry(k).or_default() += v;
    }
    drop(m);
    sh.alive.fetch_sub(1, Ordering::SeqCst);
}

/// The progress checkpoint: counts so far and the tests running now (with how long).
fn write_progress(sh: &Shared) {
    use std::fmt::Write as _;
    let (mut pass, mut fail, mut error) = (0, 0, 0);
    for r in sh.results.lock().unwrap().iter() {
        match r.outcome {
            "pass" => pass += 1,
            "fail" => fail += 1,
            _ => error += 1,
        }
    }
    let mut s = String::new();
    let _ = writeln!(s, "{} UTC: {}/{} tests done, {pass} passed, {fail} failed, {error} errors", chrono::Utc::now().format("%H:%M:%S"), sh.done.load(Ordering::SeqCst), sh.total);
    // each worker's live memory (what its thread allocated and still holds: caches plus the
    // running test), to see where a run's RSS goes
    if let Some(rss) = bc_server::admission::rss_mb(std::process::id()) {
        let _ = writeln!(s, "process RSS {:.1} GB", rss as f64 / 1024.0);
    }
    for (i, slot) in sh.slots.lock().unwrap().iter().enumerate() {
        let live = slot.mem.map(|m| format!(", worker live {:.0} MB", m.load(Ordering::Relaxed) as f64 / (1u64 << 20) as f64)).unwrap_or_default();
        if let (false, Some((t0, c, _))) = (slot.abandoned, &slot.running) {
            let _ = writeln!(s, "  worker {i}: {} :: {} ({:.0} s{live}, test peak {:.0} MB)", c.codeunit, c.test, t0.elapsed().as_secs_f64(), slot.peak as f64 / (1u64 << 20) as f64);
        } else if !slot.abandoned {
            let _ = writeln!(s, "  worker {i}: idle{live}");
        }
    }
    let _ = std::fs::write(&sh.progress, s);
}

/// Run the cases on `jobs` detached worker threads and watch them: a test running longer than
/// `--test-timeout` is recorded as an error, its worker is abandoned (a Rust thread cannot be
/// stopped; it keeps whatever it holds until the process exits) and a new worker takes the rest
/// of its codeunit.
fn run_workers(cat: &'static Catalog, sh: &Arc<Shared>, opts: &Arc<Opts>, master: &Option<Arc<Master>>, jobs: usize, mut adm: Option<&mut bc_server::admission::Admission>) {
    let spawn = |me: usize| {
        let (sh2, opts2, master2) = (sh.clone(), opts.clone(), master.clone());
        sh.slots.lock().unwrap().push(Slot::default());
        sh.alive.fetch_add(1, Ordering::SeqCst);
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn(move || {
                al_runtime::stackguard::register(1 << 30);
                worker(cat, sh2, opts2, master2, me)
            })
            .expect("spawn worker");
    };
    for me in 0..jobs {
        spawn(me);
    }
    let limit = std::time::Duration::from_secs(opts.test_timeout);
    let mut checkpoint = std::time::Instant::now();
    // the grant follows the measured RSS once the workers have reached their working set (40% of
    // the tests and 90 s in): the peak so far plus headroom for single tests that spike
    let started = std::time::Instant::now();
    let base_headroom = (jobs as u64 * HEADROOM_PER_WORKER_MB).max(HEADROOM_MIN_MB);
    let (mut peak, mut measured) = (0u64, std::time::Instant::now());
    let mut paused_since: Option<std::time::Instant> = None;
    while sh.alive.load(Ordering::SeqCst) > 0 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if let Some(a) = adm.as_deref_mut() {
            if measured.elapsed().as_secs() >= 5 {
                measured = std::time::Instant::now();
                if let Some(rss) = bc_server::admission::rss_mb(std::process::id()) {
                    peak = peak.max(rss);
                    PEAK_RSS_MB.fetch_max(peak, Ordering::Relaxed);
                    let spike_mb = sh.mem_top.lock().unwrap().iter().map(|(b, _)| (*b >> 20).max(0) as u64).max().unwrap_or(0);
                    let headroom = base_headroom.max((spike_mb * 5 / 4).min(HEADROOM_MAX_MB));
                    let past_ramp = started.elapsed().as_secs() >= 90 && sh.done.load(Ordering::SeqCst) * 10 >= sh.total * 4;
                    let target = peak + headroom;
                    // over the grant: grow it if the host has room, else stop taking new codeunits
                    // until it has (the memory guard); shrink after the ramp
                    let go = if target > a.granted_mb() + 256 {
                        let g = a.grow_or_pause(target);
                        MAX_GRANT_MB.fetch_max(a.granted_mb(), Ordering::Relaxed);
                        g
                    } else {
                        if past_ramp && target + 256 < a.granted_mb() {
                            a.adjust(target);
                        }
                        true
                    };
                    match (go, paused_since) {
                        (false, None) => {
                            sh.paused.store(true, Ordering::SeqCst);
                            paused_since = Some(std::time::Instant::now());
                            let avail = bc_server::admission::mem_available_mb().unwrap_or(0);
                            eprintln!("memory guard: pausing new codeunits (RSS {:.1} GB over a {:.1} GB estimate, {:.1} GB available): other runs hold the rest", rss as f64 / 1024.0, (a.granted_mb().saturating_sub(headroom)) as f64 / 1024.0, avail as f64 / 1024.0);
                        }
                        (true, Some(t)) => {
                            sh.paused.store(false, Ordering::SeqCst);
                            paused_since = None;
                            eprintln!("memory guard: resuming after {:.0} s", t.elapsed().as_secs_f64());
                        }
                        _ => {}
                    }
                }
            }
        }
        let mem_limit = test_mem_limit_mb(opts) as i64 * (1 << 20);
        if mem_limit > 0 {
            let mut slots = sh.slots.lock().unwrap();
            for slot in slots.iter_mut().filter(|s| !s.abandoned) {
                let (Some(m), Some((_, c, _))) = (slot.mem, slot.running.as_ref()) else { continue };
                let grown = m.load(Ordering::Relaxed) - slot.base;
                slot.peak = slot.peak.max(grown);
                if grown > mem_limit && !slot.abort.swap(true, Ordering::SeqCst) {
                    eprintln!("memory limit: stopping {} :: {} (grew its worker by {:.1} GB, limit {} MB)", c.codeunit, c.test, grown as f64 / (1u64 << 30) as f64, mem_limit >> 20);
                }
            }
        }
        if checkpoint.elapsed().as_secs() >= 60 {
            checkpoint = std::time::Instant::now();
            write_progress(sh);
        }
        if opts.test_timeout == 0 {
            continue;
        }
        let mut hung = vec![];
        {
            let mut slots = sh.slots.lock().unwrap();
            for slot in slots.iter_mut() {
                if slot.abandoned {
                    continue;
                }
                if let Some((t0, c, rest)) = slot.running.as_ref().filter(|(t0, _, _)| t0.elapsed() > limit) {
                    hung.push((t0.elapsed().as_secs_f64(), c.clone(), rest.clone()));
                    slot.abandoned = true;
                    slot.running = None;
                }
            }
        }
        for (secs, c, rest) in hung {
            sh.alive.fetch_sub(1, Ordering::SeqCst);
            let n = sh.done.fetch_add(1, Ordering::SeqCst) + 1;
            let message = format!("timeout after {} s: {} :: {} did not finish (worker abandoned)", opts.test_timeout, c.codeunit, c.test);
            eprintln!("[{n}/{}] {message}", sh.total);
            record(sh, TestResult {
                app: c.app.clone(),
                codeunit_id: c.codeunit_id,
                codeunit: c.codeunit.clone(),
                test: c.test.clone(),
                outcome: "error",
                message,
                code: "Timeout".into(),
                stack: vec![],
                ms: secs * 1000.0,
                steps: 0,
                data: opts.data.name(),
                disabled: is_disabled(&sh.disabled, &c),
                class: "fail",
                assumed: vec![],
                swallowed: vec![],
            });
            if opts.isolation_codeunit {
                // BC's test session for the codeunit is gone: the rest of it never runs
                for r in rest {
                    sh.done.fetch_add(1, Ordering::SeqCst);
                    record(sh, TestResult {
                        app: r.app.clone(),
                        codeunit_id: r.codeunit_id,
                        codeunit: r.codeunit.clone(),
                        test: r.test.clone(),
                        outcome: "error",
                        message: format!("not run: the codeunit was abandoned after {} :: {} timed out", c.codeunit, c.test),
                        code: "Timeout".into(),
                        stack: vec![],
                        ms: 0.0,
                        steps: 0,
                        data: opts.data.name(),
                        disabled: is_disabled(&sh.disabled, &r),
                        class: "not_run",
                        assumed: vec![],
                        swallowed: vec![],
                    });
                }
            } else if !rest.is_empty() {
                sh.cases.lock().unwrap().push_front(rest);
            }
            let me = sh.slots.lock().unwrap().len();
            spawn(me);
        }
    }
}

/// data/parity/cache/contoso-<key>.wal, the key covering this executable (the runtime and the demo
/// code path) and the loaded AL. Keeps the four most recent caches.
/// `--clock virtual` (set in main): the demo cache key covers the clock mode.
static CLOCK_VIRTUAL: AtomicBool = AtomicBool::new(false);

fn default_demo_cache(root: &Path, cat: &Catalog, install: bool, data: Data) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Ok(exe) = std::env::current_exe().and_then(std::fs::read) {
        exe.hash(&mut h);
    }
    (cat.stats.files, cat.stats.lines, cat.stats.objects, install).hash(&mut h);
    // the session company and the extra companies are part of the cached company
    if company() != "CRONUS" || !extra_companies().is_empty() {
        (company(), extra_companies()).hash(&mut h);
    }
    if CLOCK_VIRTUAL.load(Ordering::Relaxed) {
        "clock virtual".hash(&mut h);
    }
    let dir = root.join("data/parity/cache");
    let path = dir.join(format!("{}-{:016x}.wal", data.name(), h.finish()));
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut caches: Vec<(std::time::SystemTime, PathBuf)> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "wal") && *p != path)
            .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
            .collect();
        caches.sort();
        while caches.len() > 3 {
            let (_, old) = caches.remove(0);
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::remove_file(format!("{}.seq.json", old.display()));
        }
    }
    path
}

/// `--pg`: the scratch databases of the running phase (workers clone the template).
static PG_SCRATCH: Mutex<Option<Arc<bc_server::pgtest::PgScratch>>> = Mutex::new(None);

/// `--pg CONN`: copy the demo company into the template database the workers clone (reused
/// while the demo cache key, i.e. the build and the company, is the same). Returns a line for
/// the summary's `data:`; exits when the database can't be set up.
fn pg_setup(cat: &'static Catalog, opts: &Opts, conn: &str, m: &Master) -> String {
    let key = opts.demo_cache.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| format!("uncached-{}", std::process::id()));
    let r = bc_server::pgtest::PgScratch::new(conn).and_then(|pg| {
        let line = pg.ensure_template(cat, &m.db, &m.seqs, &key)?;
        Ok((pg, line))
    });
    match r {
        Ok((pg, line)) => {
            eprintln!("{line}");
            *PG_SCRATCH.lock().unwrap() = Some(Arc::new(pg));
            line
        }
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(2)
        }
    }
}

/// Create the Contoso demo company once (or load it from `--demo-cache`).
fn build_master(cat: &'static Catalog, opts: &Opts) -> (Master, String) {
    let t0 = std::time::Instant::now();
    let seq_path = opts.demo_cache.as_ref().map(|p| PathBuf::from(format!("{}.seq.json", p.display())));
    if let (Some(p), Some(sp)) = (&opts.demo_cache, &seq_path) {
        if p.exists() && sp.exists() {
            let (store, n) = Store::open(p, &Runtime::resolver(cat)).expect("open demo cache");
            store.set_sync(false);
            let seqs: Vec<SeqState> = serde_json::from_slice(&std::fs::read(sp).expect("read sequences")).expect("parse sequences");
            let db = Arc::new(MemBackend::new(store));
            let rows = db.stats().rows;
            let work_date = master_work_date(cat, &db, &seqs);
            eprintln!("demo company loaded from {} ({n} WAL records) in {:.1} s", p.display(), t0.elapsed().as_secs_f64());
            return (Master { db, seqs, work_date }, format!("{} (cache {}, {rows} rows)", opts.data.name(), p.display()));
        }
    }
    // written under a temporary name and renamed when complete, so parallel runs never read a
    // half-written cache
    let tmp_path = opts.demo_cache.as_ref().map(|p| PathBuf::from(format!("{}.tmp{}", p.display(), std::process::id())));
    let store = match &tmp_path {
        Some(t) => {
            if let Some(dir) = t.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::remove_file(t);
            let (s, _) = Store::open(t, &Runtime::resolver(cat)).expect("create demo cache");
            s.set_sync(false);
            s
        }
        None => Store::memory(),
    };
    let mem = Arc::new(MemBackend::new(store));
    let rt = Runtime::new(cat, mem.clone());
    let (report, seqs, installed) = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(scope, || {
                al_runtime::stackguard::register(1 << 30);
                if opts.virtual_clock {
                    al_runtime::clock::set_virtual(clock_start(opts, 1), 0xDE30);
                }
                let mut s = Session::new(&rt, &company());
                // a tenant with the apps installed, then the demo company
                let installed = opts.install.then(|| {
                    if let Err(Flow::Err(e)) = s.init_platform_data() {
                        eprintln!("platform data failed: {}", e.msg);
                    }
                    let line = install_apps(&mut s, opts);
                    if let Err(e) = s.commit() {
                        eprintln!("commit after install failed: {e}");
                    }
                    line
                });
                let r = match opts.data {
                    Data::W1Demo => demo::w1::create_w1_demo_company(&mut s),
                    _ => demo::create_test_company(&mut s),
                };
                // AL_TEST_EXTRA_COMPANIES: Company rows of the tenant's other companies (no data)
                if r.is_ok() && !extra_companies().is_empty() {
                    for c in extra_companies() {
                        if let Err(Flow::Err(e)) = s.add_company_row(&c) {
                            eprintln!("company {c}: {}", e.msg);
                        }
                    }
                    if let Err(e) = s.commit() {
                        eprintln!("commit after the extra companies failed: {e}");
                    }
                }
                let seqs = demo::number_sequences(&rt, &company(), s.db.as_mut()).unwrap_or_default();
                (r, seqs, installed)
            })
            .expect("spawn demo thread")
            .join()
            .expect("demo thread panicked")
    });
    let summary = match &report {
        Ok(r) => format!(
            "{} ({} rows in {} tables, created in {:.1} s{})",
            opts.data.name(),
            r.total_rows,
            r.tables.len(),
            t0.elapsed().as_secs_f64(),
            installed.map(|l| format!("; apps installed first: {l}")).unwrap_or_default()
        ),
        Err(e) => {
            eprintln!("demo company failed, tests run on what was created: {e:#}");
            format!("{} (INCOMPLETE: {})", opts.data.name(), format!("{e}").lines().next().unwrap_or(""))
        }
    };
    eprintln!("demo company: {summary}");
    if let (Some(p), Some(t), Some(sp)) = (&opts.demo_cache, &tmp_path, &seq_path) {
        let written = report.is_ok()
            && rt.db.compact().is_ok()
            && std::fs::write(format!("{}.tmp{}", sp.display(), std::process::id()), serde_json::to_vec(&seqs).unwrap()).is_ok()
            && std::fs::rename(format!("{}.tmp{}", sp.display(), std::process::id()), sp).is_ok()
            && std::fs::rename(t, p).is_ok();
        if written {
            eprintln!("demo company cached in {}", p.display());
        } else {
            let _ = std::fs::remove_file(t);
            eprintln!("demo company not cached");
        }
    }
    // the master store is only read from now on
    let work_date = master_work_date(cat, &mem, &seqs);
    (Master { db: mem, seqs, work_date }, summary)
}

/// Ask the AL for the work date of a session opening the demo company, on a throwaway copy.
fn master_work_date(cat: &'static Catalog, db: &MemBackend, seqs: &[SeqState]) -> Option<i32> {
    let tmp = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let company_name = company();
    let copy = demo::copy_mem_backend(&tmp, db, &[company_name.as_str()], seqs).ok()?;
    let rt = Runtime::new(cat, Arc::new(copy));
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(scope, || {
                al_runtime::stackguard::register(1 << 30);
                let mut s = Session::new(&rt, &company());
                let d = demo::default_work_date(&mut s);
                s.rollback();
                d
            })
            .ok()?
            .join()
            .ok()?
    })
}

fn pct(a: usize, b: usize) -> f64 {
    if b == 0 {
        0.0
    } else {
        100.0 * a as f64 / b as f64
    }
}

/// One run over the suites that share a dataset: its own catalog, company and workers.
struct Phase {
    suites: Vec<String>,
    jobs: usize,
    data_desc: String,
    installed: Option<String>,
    install: bool,
    catalog: String,
    apps: Vec<String>,
    codeunits: usize,
    total: usize,
    run_s: f64,
    results: Vec<TestResult>,
    missing: BTreeMap<String, usize>,
}

/// The selected suites grouped by dataset (one group with `--data`), the Base App group first:
/// every phase's catalog stays loaded (leaked for the runtime's 'static references), so the
/// biggest phase runs before the others add theirs.
fn phases(o: &Opts) -> Vec<(Data, Vec<String>)> {
    let mut out: Vec<(Data, Vec<String>)> = vec![];
    for s in &o.suites {
        let d = if o.data_forced { o.data } else { Data::for_suite(s) };
        match out.iter_mut().find(|p| p.0 == d) {
            Some(p) => p.1.push(s.clone()),
            None => out.push((d, vec![s.clone()])),
        }
    }
    out.sort_by_key(|p| !p.1.iter().any(|s| s.to_ascii_lowercase().starts_with("baseapp")));
    out
}

fn main() {
    let root = repo_root();
    // IsolatedStorage.SetEncrypted: the fixed test-only key unless BC_ISOLATED_STORAGE_KEY(_FILE) is set
    al_runtime::isolated::use_test_key();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--serve") => serve(&root, Path::new(args.get(1).unwrap_or_else(|| usage()))),
        Some("--client") => {
            let dir = PathBuf::from(args.get(1).unwrap_or_else(|| usage()));
            client(&root, &dir, args[2..].to_vec())
        }
        _ => std::process::exit(run_args(&root, args)),
    }
}

/// One al-test run with these arguments (what `main` does standalone, and what the server does
/// for each request). Returns the exit code.
fn run_args(root: &Path, args: Vec<String>) -> i32 {
    let root = root.to_path_buf();
    let opts = parse_opts(&root, args.clone());
    let mut adm = if opts.list { None } else { bc_server::admission::Admission::join(&format!("al-test {}", args.join(" "))) };
    std::panic::set_hook(Box::new(|info| {
        let msg = match info.payload().downcast_ref::<&str>() {
            Some(s) => s.to_string(),
            None => info.payload().downcast_ref::<String>().cloned().unwrap_or_default(),
        };
        let loc = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
        PANIC_MSG.with(|p| *p.borrow_mut() = Some(format!("{msg}{loc}")));
    }));
    let plan = phases(&opts);
    let several = plan.len() > 1;
    *CODEUNIT_HITS.lock().unwrap() = vec![false; opts.codeunits.len()];
    CODEUNIT_FILTER_ERROR.store(false, Ordering::SeqCst);
    if !opts.list {
        // results are appended as tests finish (report() rewrites the file sorted at the end)
        let _ = std::fs::create_dir_all(&opts.out);
        let _ = std::fs::File::create(opts.out.join(format!("{}-results.jsonl", out_stem(&opts))));
        let _ = std::fs::File::create(opts.out.join(format!("{}-started.log", out_stem(&opts))));
    }
    let mut done = vec![];
    let mut leaked_mb = 0;
    for (data, suites) in plan {
        let mut po = opts.clone();
        po.data = data;
        po.suites = suites;
        if several {
            // one cache file per dataset
            po.demo_cache = opts.demo_cache.as_ref().map(|p| p.with_file_name(format!("{}-{}.wal", p.file_stem().unwrap_or_default().to_string_lossy(), data.name())));
            eprintln!("phase: {} on --data {}", po.suites.join(","), data.name());
        }
        po.codeunits_strict = !several;
        done.extend(run_phase(&root, po, adm.as_mut(), leaked_mb));
        if CODEUNIT_FILTER_ERROR.load(Ordering::SeqCst) {
            return 2;
        }
        // the phase's company and workers are gone; its catalog stays loaded
        leaked_mb += CATALOG_MB;
        if let Some(a) = adm.as_mut() {
            // shrink only: the next phase reserves what it needs itself (while serving, the grant is
            // the server's real RSS, which can be below CATALOG_MB, and growing here only queued)
            a.reserve(leaked_mb.min(a.granted_mb()), "the next phase");
        }
    }
    let unknown: Vec<String> = opts.codeunits.iter().zip(CODEUNIT_HITS.lock().unwrap().iter()).filter(|(_, hit)| !**hit).map(|(f, _)| f.clone()).collect();
    if opts.list {
        return if unknown.is_empty() { 0 } else { unknown_codeunits(&unknown, &opts) };
    }
    report(&opts, done);
    // completed: the start log is only needed after a crash
    let _ = std::fs::remove_file(opts.out.join(format!("{}-started.log", out_stem(&opts))));
    if !unknown.is_empty() {
        return unknown_codeunits(&unknown, &opts);
    }
    0
}

/// Error for --codeunit filters that match no test codeunit of the run's suites (exit code 2).
fn unknown_codeunits(unknown: &[String], opts: &Opts) -> i32 {
    eprintln!(
        "error: --codeunit {} matches no test codeunit in --suite {} (names match case-insensitively as substrings, or give the codeunit id; separate them with commas, quote a name that has one)",
        unknown.iter().map(|u| format!("\"{u}\"")).collect::<Vec<_>>().join(", "),
        opts.suites.join(",")
    );
    2
}

/// Values left in the test codeunit instance's "Library - Variable Storage" (codeunit 131004)
/// queues: the TotalCount of every global of that type, or None when all are empty.
fn queue_leftovers(s: &mut Session, inst: &Rc<Instance>) -> Option<i64> {
    let globals = s.instance_globals(inst);
    let mut left = 0i64;
    for v in &globals.vars {
        let is_queue = matches!(v.ty, Some(al_syntax::ast::TypeRef::Object { kind, target, .. }) if kind.eq_ignore_ascii_case("codeunit") && target.trim_matches('"').eq_ignore_ascii_case("Library - Variable Storage"));
        if !is_queue {
            continue;
        }
        let al_runtime::Val::Object(q) = v.slot.borrow().clone() else { continue };
        let qg = s.instance_globals(&q);
        if let Some(tc) = qg.vars.iter().find(|x| x.name.eq_ignore_ascii_case("TotalCount")) {
            left += tc.slot.borrow().as_i64();
        }
    }
    (left > 0).then_some(left)
}

/// How much one test may grow its worker (MB; 0 = no limit): --test-mem-limit, else the
/// dataset's per-worker figure plus 3 GB.
fn test_mem_limit_mb(opts: &Opts) -> u64 {
    opts.test_mem_mb.unwrap_or(opts.data.worker_mb() + 3072)
}

/// Where a run's standard output goes: stdout, or while `--serve` runs a request, that request's
/// log (`DIR/done/<id>.log`, which the client prints).
static OUT: Mutex<Option<std::fs::File>> = Mutex::new(None);

fn emit(text: &str) {
    match OUT.lock().unwrap().as_mut() {
        Some(f) => {
            let _ = f.write_all(text.as_bytes());
        }
        None => print!("{text}"),
    }
}

/// Set while `--serve` runs: demo companies are then kept for later requests.
static SERVING: AtomicBool = AtomicBool::new(false);

/// The catalog for these app directories, loaded once per process (catalogs are leaked for the
/// runtime's 'static references; `--serve` reuses them across requests).
fn cached_catalog(dirs: &[PathBuf]) -> &'static Catalog {
    if let Some((_, c)) = CATALOGS.lock().unwrap().iter().find(|(d, _)| d.as_slice() == dirs) {
        eprintln!("catalog: reused (loaded earlier by this process)");
        return c;
    }
    let c: &'static Catalog = Box::leak(Box::new(Catalog::load(dirs).expect("load catalog")));
    CATALOG_LOADS.fetch_add(1, Ordering::SeqCst);
    CATALOGS.lock().unwrap().push((dirs.to_vec(), c));
    c
}

/// Catalogs loaded by this process, by app directory list (see `cached_catalog`).
static CATALOGS: Mutex<Vec<(Vec<PathBuf>, &'static Catalog)>> = Mutex::new(Vec::new());

/// Demo companies kept while serving, by (catalog, dataset, install, --clock virtual) (see `cached_master`).
type MasterKey = (usize, &'static str, bool, bool);
static MASTERS: Mutex<Vec<(MasterKey, Arc<Master>, String)>> = Mutex::new(Vec::new());

fn master_key(cat: &'static Catalog, opts: &Opts) -> MasterKey {
    (cat as *const Catalog as usize, opts.data.name(), opts.install, opts.virtual_clock)
}

/// The demo company of a catalog and dataset: built (or loaded from its cache) once; kept for
/// later requests while serving.
fn cached_master(cat: &'static Catalog, opts: &Opts) -> (Arc<Master>, String) {
    let key = master_key(cat, opts);
    if let Some((_, m, d)) = MASTERS.lock().unwrap().iter().find(|(k, _, _)| *k == key) {
        eprintln!("demo company: reused (kept in memory by the server)");
        return (m.clone(), format!("{d}; kept in memory by the server"));
    }
    let (m, d) = build_master(cat, opts);
    let m = Arc::new(m);
    if SERVING.load(Ordering::SeqCst) {
        MASTERS.lock().unwrap().push((key, m.clone(), d.clone()));
    }
    (m, d)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Request {
    argv: Vec<String>,
    cwd: PathBuf,
    client_pid: u32,
}

/// `--serve DIR`: run queued requests (docs/al-test-serve.md) one at a time in this process,
/// keeping catalogs and demo companies loaded between them. Restarts itself by exiting after
/// SERVE_MAX_REQUESTS requests or when more than SERVE_MAX_CATALOGS catalogs are cached; a
/// supervisor (or the next client, which falls back to a standalone run) carries on.
fn serve(root: &Path, dir: &Path) {
    const SERVE_MAX_REQUESTS: usize = 50;
    const SERVE_MAX_CATALOGS: usize = 3;
    // an idle server holds its caches (about 3-6 GB) for nothing: exit, the next client falls
    // back to a standalone run (P8's three idle servers held 18 GB at the 04:30 UTC OOM kill)
    const SERVE_IDLE_EXIT_S: u64 = 300;
    for d in ["requests", "done"] {
        std::fs::create_dir_all(dir.join(d)).expect("create spool dir");
    }
    let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join("server.lock")).expect("server lock");
    if lock.try_lock().is_err() {
        eprintln!("al-test --serve: another server holds {}", dir.join("server.lock").display());
        std::process::exit(1);
    }
    let _ = std::fs::write(dir.join("server.pid"), format!("{}", std::process::id()));
    SERVING.store(true, Ordering::SeqCst);
    eprintln!("al-test --serve {}: waiting for requests (exits after {} s without one)", dir.display(), SERVE_IDLE_EXIT_S);
    let mut served = 0usize;
    // between requests the cached catalogs and companies are resident: a ticket of their real RSS
    // makes other runs see them in the queue (counted as a holder, listed in "reserved by");
    // while a request runs, its own ticket is granted the server's RSS instead (run_phase)
    let idle_ticket = |served: usize| -> Option<bc_server::admission::Admission> {
        let mut t = bc_server::admission::Admission::join(&format!("al-test --serve {} (cached catalogs and companies, {served} requests served)", dir.display()))?;
        t.adjust(bc_server::admission::rss_mb(std::process::id()).unwrap_or(0));
        Some(t)
    };
    let mut idle = idle_ticket(served);
    let (mut idle_since, mut refreshed) = (std::time::Instant::now(), std::time::Instant::now());
    loop {
        let mut reqs: Vec<PathBuf> = std::fs::read_dir(dir.join("requests")).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect()).unwrap_or_default();
        reqs.sort();
        let Some(path) = reqs.into_iter().next() else {
            if idle_since.elapsed().as_secs() >= SERVE_IDLE_EXIT_S {
                eprintln!("al-test --serve: no request for {SERVE_IDLE_EXIT_S} s; exiting (the next client runs standalone or starts a server)");
                return;
            }
            if refreshed.elapsed().as_secs() >= 15 {
                refreshed = std::time::Instant::now();
                if let (Some(t), Some(rss)) = (idle.as_mut(), bc_server::admission::rss_mb(std::process::id())) {
                    t.adjust(rss);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
            continue;
        };
        drop(idle.take());
        let id = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let req: Option<Request> = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok());
        let _ = std::fs::remove_file(&path);
        let Some(req) = req else { continue };
        eprintln!("al-test --serve: request {id} from pid {}: al-test {}", req.client_pid, req.argv.join(" "));
        let _ = std::env::set_current_dir(&req.cwd);
        let opts = parse_opts(root, req.argv.clone());
        let summary = opts.out.join(format!("{}-summary.txt", out_stem(&opts)));
        let results = opts.out.join(format!("{}-results.jsonl", out_stem(&opts)));
        let log = dir.join("done").join(format!("{id}.log"));
        *OUT.lock().unwrap() = std::fs::File::create(&log).ok();
        let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_args(root, req.argv.clone()))).unwrap_or(101);
        *OUT.lock().unwrap() = None;
        let done = serde_json::json!({ "exit": code, "summary": summary, "results": results, "log": log });
        let _ = std::fs::write(dir.join("done").join(format!("{id}.json")), done.to_string());
        served += 1;
        idle = idle_ticket(served);
        idle_since = std::time::Instant::now();
        // distinct catalogs loaded so far (the catalog cache never shrinks)
        let catalogs = CATALOG_LOADS.load(Ordering::SeqCst);
        if served >= SERVE_MAX_REQUESTS || catalogs > SERVE_MAX_CATALOGS {
            eprintln!("al-test --serve: restarting after {served} requests and {catalogs} catalogs");
            return;
        }
    }
}

/// Catalogs this process loaded (see `cached_catalog`).
static CATALOG_LOADS: AtomicUsize = AtomicUsize::new(0);

/// `--client DIR ARGS`: hand the run to a server on DIR and wait for it; without a live server
/// (or if it dies meanwhile) run standalone.
fn client(root: &Path, dir: &Path, args: Vec<String>) -> ! {
    // bad arguments fail here, never in the server
    let _ = parse_opts(root, args.clone());
    let server_alive = || -> bool {
        match std::fs::OpenOptions::new().read(true).write(true).open(dir.join("server.lock")) {
            Ok(f) => match f.try_lock() {
                Ok(()) => {
                    let _ = f.unlock();
                    false
                }
                Err(_) => true,
            },
            Err(_) => false,
        }
    };
    if !server_alive() {
        eprintln!("al-test --client: no server on {}; running standalone", dir.display());
        std::process::exit(run_args(root, args));
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let id = format!("{nanos:024}-{}", std::process::id());
    let req = Request { argv: args.clone(), cwd: std::env::current_dir().unwrap_or_default(), client_pid: std::process::id() };
    let tmp = dir.join("requests").join(format!("{id}.tmp"));
    std::fs::write(&tmp, serde_json::to_string(&req).unwrap()).expect("write request");
    std::fs::rename(&tmp, dir.join("requests").join(format!("{id}.json"))).expect("submit request");
    eprintln!("al-test --client: submitted {id} to the server on {}", dir.display());
    let done = dir.join("done").join(format!("{id}.json"));
    loop {
        if let Some(d) = std::fs::read_to_string(&done).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) {
            let _ = std::fs::remove_file(&done);
            // the request's standard output (summary, --list lines), as a standalone run prints it
            if let Some(lp) = d["log"].as_str() {
                if let Ok(text) = std::fs::read_to_string(lp) {
                    print!("{text}");
                }
                let _ = std::fs::remove_file(lp);
            }
            std::process::exit(d["exit"].as_i64().unwrap_or(1) as i32);
        }
        if !server_alive() {
            let pending = dir.join("requests").join(format!("{id}.json"));
            let _ = std::fs::remove_file(&pending);
            eprintln!("al-test --client: the server is gone; running standalone");
            std::process::exit(run_args(root, args));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Load the catalog for `opts.suites` (plus `opts.data`'s demo tool), create the company and run
/// the tests. None with `--list` (the codeunits are printed instead).
fn run_phase(root: &Path, mut opts: Opts, mut adm: Option<&mut bc_server::admission::Admission>, leaked_mb: u64) -> Option<Phase> {
    let root = root.to_path_buf();
    // apps: default set + Test Framework + test libraries + test apps of the selected suites
    let mut extra = al_model::test_framework_dirs(&root);
    let mut test_dirs = vec![];
    for s in &opts.suites {
        let (libs, tests) = if s.eq_ignore_ascii_case("ext") { (opts.ext_apps.clone(), opts.ext_tests.clone()) } else { al_model::test_suite_dirs(&root, s) };
        if tests.is_empty() {
            eprintln!("unknown suite {s} (known: {})", al_model::TEST_SUITES.join(", "));
            std::process::exit(2);
        }
        extra.extend(libs);
        extra.extend(tests.iter().cloned());
        test_dirs.extend(tests);
    }
    if opts.data == Data::Contoso {
        extra.extend(al_model::demo_tool_dirs(&root));
    }
    if opts.data == Data::W1Demo {
        extra.extend(al_model::w1_demo_tool_dirs(&root));
    }
    let mut dirs = al_model::app_dirs_with(&root, &extra);
    // plus the apps they declare as dependencies (app.json), transitively: PEPPOL for Tests-Misc
    let deps = al_model::missing_dependency_dirs(&root, &dirs);
    dirs.extend(deps);
    // what the run holds already: the catalogs of its earlier phases. While serving, everything
    // the server holds (earlier requests' catalogs and companies): it is resident, so it is
    // granted at once (grant = RSS leaves nothing unused for other runs to count), and only what
    // is not cached yet waits in the queue.
    let serving = SERVING.load(Ordering::SeqCst);
    let held_mb = if serving { bc_server::admission::rss_mb(std::process::id()).unwrap_or(leaked_mb) } else { leaked_mb };
    let catalog_mb = if serving && CATALOGS.lock().unwrap().iter().any(|(d, _)| *d == dirs) { 0 } else { CATALOG_MB };
    if let Some(a) = adm.as_deref_mut() {
        if serving && held_mb > a.granted_mb() {
            a.adjust(held_mb);
        }
        let w0 = std::time::Instant::now();
        a.reserve(held_mb + catalog_mb, "the catalog");
        ADMIT_WAIT_MS.fetch_add(w0.elapsed().as_millis() as u64, Ordering::Relaxed);
        MAX_GRANT_MB.fetch_max(a.granted_mb(), Ordering::Relaxed);
    }
    let t0 = std::time::Instant::now();
    let cat: &'static Catalog = cached_catalog(&dirs);
    let mut app_cache = HashMap::new();
    let mut apps: BTreeMap<String, usize> = BTreeMap::new();
    for d in &dirs {
        *apps.entry(app_name(&d.join("x"), &mut app_cache)).or_default() += 0;
    }
    eprintln!(
        "catalog: {} files, {} lines, {} objects, {} parse errors, {} ms",
        cat.stats.files,
        cat.stats.lines,
        cat.stats.objects,
        cat.stats.parse_errors,
        t0.elapsed().as_millis()
    );

    // test codeunits and their [Test] procedures
    let mut groups: Vec<Vec<Case>> = vec![];
    let mut codeunit_count = 0;
    let mut order: Vec<(i64, u32)> = cat
        .code
        .iter()
        .enumerate()
        .filter(|(_, co)| co.kind == ObjectKind::Codeunit && co.subtype.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("test")))
        .filter(|(_, co)| test_dirs.iter().any(|d| co.file.starts_with(d)))
        .map(|(i, co)| (co.id, i as u32))
        .collect();
    order.sort();
    if !opts.codeunits.is_empty() {
        let mut hits = CODEUNIT_HITS.lock().unwrap();
        hits.resize(opts.codeunits.len(), false);
        for (i, f) in opts.codeunits.iter().enumerate() {
            let fl = f.to_ascii_lowercase();
            if order.iter().any(|&(id, ci)| f.parse::<i64>().ok() == Some(id) || cat.code[ci as usize].name.to_ascii_lowercase().contains(&fl)) {
                hits[i] = true;
            }
        }
        if opts.codeunits_strict && hits.iter().any(|h| !h) {
            let unknown: Vec<String> = opts.codeunits.iter().zip(hits.iter()).filter(|(_, h)| !**h).map(|(f, _)| f.clone()).collect();
            drop(hits);
            unknown_codeunits(&unknown, &opts);
            CODEUNIT_FILTER_ERROR.store(true, Ordering::SeqCst);
            return None;
        }
    }
    for (id, ci) in order {
        let co = &cat.code[ci as usize];
        let app = app_name(&co.file, &mut app_cache);
        if let Some(a) = &opts.app {
            if !app.to_ascii_lowercase().contains(&a.to_ascii_lowercase()) {
                continue;
            }
        }
        if !opts.codeunits.is_empty()
            && !opts.codeunits.iter().any(|f| f.parse::<i64>().ok() == Some(id) || co.name.to_ascii_lowercase().contains(&f.to_ascii_lowercase()))
        {
            continue;
        }
        let mut cases = vec![];
        for (pi, p) in co.procs.iter().enumerate() {
            if !p.attributes.iter().any(|a| a.name.eq_ignore_ascii_case("test")) {
                continue;
            }
            if let Some(t) = &opts.test {
                if !p.name.to_ascii_lowercase().contains(&t.to_ascii_lowercase()) {
                    continue;
                }
            }
            cases.push(Case { code: ci, proc: pi as u32, codeunit_id: id, codeunit: co.name.clone(), test: p.name.clone(), app: app.clone() });
        }
        if cases.is_empty() {
            continue;
        }
        codeunit_count += 1;
        groups.push(cases);
        if opts.limit.is_some_and(|l| groups.len() >= l) {
            break;
        }
    }
    // tests Microsoft's CI does not run: recorded as "disabled", not run (--include-disabled runs them)
    let mut disabled = disabled_tests(&root, apps.keys().cloned());
    let mut skipped: Vec<TestResult> = vec![];
    if !opts.include_disabled {
        for g in groups.iter_mut() {
            g.retain(|c| {
                if !is_disabled(&disabled, c) {
                    return true;
                }
                skipped.push(TestResult { app: c.app.clone(), codeunit_id: c.codeunit_id, codeunit: c.codeunit.clone(), test: c.test.clone(), outcome: "disabled", message: "disabled in Microsoft's CI (vendor/bcapps/src/DisabledTests); --include-disabled runs it".into(), code: String::new(), stack: vec![], ms: 0.0, steps: 0, data: opts.data.name(), disabled: true, class: "not_run", assumed: vec![], swallowed: vec![] });
                false
            });
        }
        groups.retain(|g| !g.is_empty());
    }
    if !skipped.is_empty() {
        eprintln!("{} tests are disabled in Microsoft's CI: not run (--include-disabled runs them)", skipped.len());
    }
    let total: usize = groups.iter().map(|g| g.len()).sum();
    // installed: the apps under test, the Test Framework and (contoso) the demo tool, with
    // everything they depend on -- like Microsoft's test environments, which have only those
    let manifests = al_model::apps_of_dirs(&cat.app_dirs);
    let mut roots_dirs: Vec<PathBuf> = test_dirs.clone();
    roots_dirs.extend(al_model::test_framework_dirs(&root));
    // suite ext: every --ext-app is installed too, as the CI that runs those apps publishes them
    roots_dirs.extend(opts.ext_apps.iter().cloned());
    if opts.data == Data::Contoso {
        roots_dirs.extend(al_model::demo_tool_dirs(&root));
    }
    if opts.data == Data::W1Demo {
        roots_dirs.extend(al_model::w1_demo_tool_dirs(&root));
    }
    let roots: Vec<usize> = manifests.iter().enumerate().filter(|(_, m)| roots_dirs.contains(&m.dir)).map(|(i, _)| i).collect();
    let closure = al_model::dependency_closure(&manifests, &roots);
    opts.install_ids = manifests.iter().zip(&closure).filter(|(_, c)| **c).map(|(m, _)| m.id.to_ascii_lowercase()).collect();
    // workers: --jobs, or one per CPU but no more than codeunits and than fit in the memory this
    // run could have once the others are done; then reserve the company and the workers
    let is_demo = matches!(opts.data, Data::Contoso | Data::W1Demo);
    if is_demo && opts.demo_cache.is_none() && !opts.demo_cache_off && !opts.list {
        CLOCK_VIRTUAL.store(opts.virtual_clock, Ordering::Relaxed);
        opts.demo_cache = Some(default_demo_cache(&root, cat, opts.install, opts.data));
    }
    let cached = opts.demo_cache.as_ref().is_some_and(|p| p.exists() && PathBuf::from(format!("{}.seq.json", p.display())).exists());
    let master_mb = match (is_demo, cached) {
        (false, _) => 0,
        (true, true) => MASTER_LOAD_MB,
        (true, false) => MASTER_BUILD_MB,
    };
    let master_mb = if serving && is_demo && MASTERS.lock().unwrap().iter().any(|(k, _, _)| *k == master_key(cat, &opts)) { 0 } else { master_mb };
    let fixed_mb = held_mb + catalog_mb + master_mb;
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let capacity = match adm.as_deref() {
        Some(a) => a.capacity_mb(),
        None => bc_server::admission::mem_available_mb().map(|m| m.saturating_sub(bc_server::admission::margin_mb())),
    };
    let codeunits = groups.len().max(1);
    // how many workers fit: when the codeunits' footprint fits, workers only need MIN_WORKER_MB
    let fit = capacity.map(|c| {
        let room = c.saturating_sub(fixed_mb);
        if codeunits as u64 * MB_PER_CODEUNIT <= room {
            (room / MIN_WORKER_MB).max(1) as usize
        } else {
            (room / opts.data.worker_mb()).max(1) as usize
        }
    });
    let requested = opts.jobs;
    opts.jobs = match requested {
        0 => cpus.min(codeunits).min(fit.unwrap_or(cpus)).min(DEFAULT_JOBS_MAX),
        n => {
            if fit.is_some_and(|f| n.min(codeunits) > f) {
                eprintln!("warning: --jobs {n} may not fit in memory (about {} workers fit)", fit.unwrap_or(0));
            }
            n
        }
    };
    let workers = opts.jobs.min(codeunits);
    let want_mb = fixed_mb + workers_mb(opts.data, workers, codeunits);
    eprintln!(
        "jobs: {} ({}; {cpus} CPUs, {codeunits} codeunits{}), reserving {:.1} GB (--data {}: {} MB per worker, company {} MB, catalogs {} MB)",
        opts.jobs,
        if requested == 0 { "default" } else { "--jobs" },
        fit.map(|f| format!(", memory for {f}")).unwrap_or_default(),
        want_mb as f64 / 1024.0,
        opts.data.name(),
        opts.data.worker_mb(),
        master_mb,
        held_mb + catalog_mb
    );
    eprintln!("{codeunit_count} test codeunits, {total} tests, {} jobs", opts.jobs);
    if opts.list {
        for g in &groups {
            emit(&format!("{} {} [{}] ({} tests)\n", g[0].codeunit_id, g[0].codeunit, g[0].app, g.len()));
        }
        return None;
    }
    if let Some(a) = adm.as_deref_mut() {
        if !groups.is_empty() {
            let w0 = std::time::Instant::now();
            a.reserve(want_mb, &format!("{workers} workers"));
            ADMIT_WAIT_MS.fetch_add(w0.elapsed().as_millis() as u64, Ordering::Relaxed);
            MAX_GRANT_MB.fetch_max(a.granted_mb(), Ordering::Relaxed);
        }
    }
    if groups.is_empty() {
        // the filters select nothing here: no company to create
        let catalog = format!("{} files, {} objects, {} parse errors", cat.stats.files, cat.stats.objects, cat.stats.parse_errors);
        let data_desc = format!("{} (not created: no tests selected)", opts.data.name());
        return Some(Phase { suites: opts.suites.clone(), jobs: opts.jobs, data_desc, installed: None, install: opts.install, catalog, apps: apps.into_keys().collect(), codeunits: 0, total: skipped.len(), run_s: 0.0, results: std::mem::take(&mut skipped), missing: BTreeMap::new() });
    }

    // run: longest codeunits first so the tail is short
    groups.sort_by_key(|g| std::cmp::Reverse(g.len()));
    let (master, data_desc) = match opts.data {
        Data::Contoso | Data::W1Demo => {
            if opts.demo_cache.is_none() && !opts.demo_cache_off {
                CLOCK_VIRTUAL.store(opts.virtual_clock, Ordering::Relaxed);
        opts.demo_cache = Some(default_demo_cache(&root, cat, opts.install, opts.data));
            }
            let (m, d) = cached_master(cat, &opts);
            let d = match &opts.pg {
                Some(conn) => format!("{d}; {}", pg_setup(cat, &opts, conn, &m)),
                None => d,
            };
            let wd = m.work_date.and_then(al_runtime::value::date_to_naive).map(|d| d.to_string()).unwrap_or_else(|| "today".into());
            (Some(m), format!("{d}, WorkDate {wd}"))
        }
        _ if opts.pg.is_some() => {
            eprintln!("--pg needs a demo company: --data contoso or w1demo");
            std::process::exit(2)
        }
        Data::Seed => (None, "seed (crates/bc-server/src/seed.rs)".to_string()),
        Data::None => (None, "none (platform rows only)".to_string()),
    };
    let jobs = opts.jobs.min(groups.len().max(1));
    // the company exists now: its creation peak is over (a loaded company costs MASTER_LOAD_MB)
    if master_mb == MASTER_BUILD_MB {
        if let Some(a) = adm.as_deref_mut() {
            a.adjust(want_mb - (MASTER_BUILD_MB - MASTER_LOAD_MB));
        }
    }
    let sh = Arc::new(Shared {
        install: Mutex::new(None),
        cases: Mutex::new(groups.into()),
        results: Mutex::new(vec![]),
        missing: Mutex::new(BTreeMap::new()),
        done: AtomicUsize::new(0),
        total,
        slots: Mutex::new(vec![]),
        alive: AtomicUsize::new(0),
        jsonl: std::fs::OpenOptions::new().create(true).append(true).open(opts.out.join(format!("{}-results.jsonl", out_stem(&opts)))).ok().map(Mutex::new),
        progress: opts.out.join(format!("{}-progress.txt", out_stem(&opts))),
        paused: AtomicBool::new(false),
        mem_top: Mutex::new(vec![]),
        disabled: std::mem::take(&mut disabled),
        started: std::fs::OpenOptions::new().create(true).append(true).open(opts.out.join(format!("{}-started.log", out_stem(&opts)))).ok().map(Mutex::new),
    });
    let t1 = std::time::Instant::now();
    let shared_opts = Arc::new(opts.clone());
    run_workers(cat, &sh, &shared_opts, &master, jobs, adm.as_deref_mut());
    let run_s = t1.elapsed().as_secs_f64();
    if let Some(pg) = PG_SCRATCH.lock().unwrap().take() {
        pg.finish(opts.pg_keep);
    }
    {
        let mut top = std::mem::take(&mut *sh.mem_top.lock().unwrap());
        top.sort_by(|a, b| b.0.cmp(&a.0));
        if !top.is_empty() {
            let text: String = top.iter().take(50).map(|(b, t)| format!("{:8.0} MB  {t}\n", *b as f64 / (1u64 << 20) as f64)).collect();
            let _ = std::fs::write(opts.out.join(format!("{}-memory.txt", out_stem(&opts))), format!("tests that grew their worker by more than 256 MB (largest first, top 50)\n{text}"));
            eprintln!("largest test memory: {}", top.iter().take(3).map(|(b, t)| format!("{t} {:.1} GB", *b as f64 / (1u64 << 30) as f64)).collect::<Vec<_>>().join("; "));
        }
    }
    if sh.paused.swap(false, Ordering::SeqCst) {
        eprintln!("memory guard: run ended while paused");
    }
    let (deep, at) = al_runtime::stackguard::deepest();
    eprintln!("native stack: deepest {:.1} MB of 1024 MB per worker (at AL call depth {at})", deep as f64 / 1048576.0);
    let installed = sh.install.lock().unwrap().take();
    let mut results = std::mem::take(&mut *sh.results.lock().unwrap());
    results.append(&mut skipped);
    results.sort_by(|a, b| (a.codeunit_id, &a.test).cmp(&(b.codeunit_id, &b.test)));
    let missing = std::mem::take(&mut *sh.missing.lock().unwrap());
    Some(Phase {
        suites: opts.suites.clone(),
        jobs: opts.jobs,
        data_desc,
        installed,
        install: opts.install,
        catalog: format!("{} files, {} objects, {} parse errors", cat.stats.files, cat.stats.objects, cat.stats.parse_errors),
        apps: apps.into_keys().collect(),
        codeunits: codeunit_count,
        total: total + results.iter().filter(|r| r.outcome == "disabled").count(),
        run_s,
        results,
        missing,
    })
}

/// Write `<out>/<stem>-results.jsonl` and the summary of all phases.
fn report(opts: &Opts, phases: Vec<Phase>) {

    let mut results: Vec<TestResult> = vec![];
    let mut missing: BTreeMap<String, usize> = BTreeMap::new();
    let mut apps: std::collections::BTreeSet<String> = Default::default();
    for p in &phases {
        results.extend(p.results.iter().cloned());
        for (k, v) in &p.missing {
            *missing.entry(k.clone()).or_default() += v;
        }
        apps.extend(p.apps.iter().cloned());
    }
    results.sort_by(|a, b| (a.codeunit_id, &a.test).cmp(&(b.codeunit_id, &b.test)));
    let total: usize = phases.iter().map(|p| p.total).sum();
    let codeunit_count: usize = phases.iter().map(|p| p.codeunits).sum();
    let run_s: f64 = phases.iter().map(|p| p.run_s).sum();
    let jobs = phases.iter().map(|p| p.jobs.to_string()).collect::<Vec<_>>().join("/");
    let label = |p: &Phase| if phases.len() > 1 { format!(" ({})", p.suites.join(", ")) } else { String::new() };

    // report
    let date = opts.run_date.clone();
    let stem = out_stem(opts);
    std::fs::create_dir_all(&opts.out).expect("create output dir");
    let jsonl = opts.out.join(format!("{stem}-results.jsonl"));
    let mut jf = std::io::BufWriter::new(std::fs::File::create(&jsonl).expect("results file"));
    for r in &results {
        serde_json::to_writer(&mut jf, r).unwrap();
        jf.write_all(b"\n").unwrap();
    }
    jf.flush().unwrap();

    // the headline counts the tests Microsoft's CI runs; disabled ones are apart (src/DisabledTests)
    let disabled_n = results.iter().filter(|r| r.disabled).count();
    let enabled_total = total - disabled_n;
    let passed = results.iter().filter(|r| !r.disabled && r.outcome == "pass").count();
    let failed = results.iter().filter(|r| !r.disabled && r.outcome == "fail").count();
    let errors = results.iter().filter(|r| !r.disabled && r.outcome == "error").count();
    let disabled_passed = results.iter().filter(|r| r.disabled && r.outcome == "pass").count();
    let mut s = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(s, "bc-rust AL test parity, {date}{}", opts.label.as_ref().map(|l| format!(" ({l})")).unwrap_or_default());
    let _ = writeln!(s, "suites: {}   filters: app={:?} codeunit={:?} test={:?}", opts.suites.join(" "), opts.app, opts.codeunits, opts.test);
    let _ = writeln!(s, "apps loaded ({}): {}", apps.len(), apps.iter().cloned().collect::<Vec<_>>().join(", "));
    for p in &phases {
        let _ = writeln!(s, "catalog{}: {}", label(p), p.catalog);
    }
    // the dataset per suite
    for p in &phases {
        let _ = writeln!(s, "data: {} -> {}", p.suites.join(", "), p.data_desc);
    }
    let _ = writeln!(
        s,
        "bc-runner: {}   isolation: {}   clock: {}",
        if opts.bc_runner { "on (Random seeded to 1 and number sequences restored before every test)" } else { "off (--no-bc-runner)" },
        if opts.isolation_codeunit { "codeunit" } else { "test" },
        if opts.virtual_clock { "virtual (deterministic timestamps and Guids per codeunit)" } else { "wall" }
    );
    let gb = |mb: &AtomicU64| mb.load(Ordering::Relaxed) as f64 / 1024.0;
    let _ = writeln!(
        s,
        "memory: peak RSS {:.1} GB, largest grant {:.1} GB, admission wait {:.0} s",
        gb(&PEAK_RSS_MB),
        gb(&MAX_GRANT_MB),
        ADMIT_WAIT_MS.load(Ordering::Relaxed) as f64 / 1000.0
    );
    for p in &phases {
        if let Some(l) = &p.installed {
            let _ = writeln!(s, "install codeunits{}: {l}", label(p));
        } else if !p.install {
            let _ = writeln!(s, "install codeunits{}: skipped (--no-install)", label(p));
        }
    }
    let _ = writeln!(s, "test codeunits: {codeunit_count}   tests: {total}   wall time: {run_s:.1} s on {jobs} jobs");
    let _ = writeln!(s);
    let _ = writeln!(s, "passed: {passed:6} ({:.1}%)   of the {enabled_total} tests Microsoft's CI runs (CI-comparable)", pct(passed, enabled_total));
    let _ = writeln!(s, "failed: {failed:6} ({:.1}%)   AL code ran, an error or assertion failed", pct(failed, enabled_total));
    let _ = writeln!(s, "errors: {errors:6} ({:.1}%)   bc-rust could not run the code (missing feature, panic, limit)", pct(errors, enabled_total));
    if disabled_n > 0 {
        if opts.include_disabled {
            let _ = writeln!(s, "disabled: {disabled_n:4}   disabled in Microsoft's CI (src/DisabledTests), run with --include-disabled: {disabled_passed} passed");
        } else {
            let _ = writeln!(s, "disabled: {disabled_n:4}   disabled in Microsoft's CI (src/DisabledTests): not run");
        }
    }
    if disabled_n == 0 || opts.include_disabled {
        let _ = writeln!(s, "all tests: {} of {total} passed ({:.1}%), comparable with runs before 2026-11-08 08:00 UTC", passed + disabled_passed, pct(passed + disabled_passed, total));
    } else {
        let _ = writeln!(s, "all tests: {total}, of which {disabled_n} not run; --include-disabled gives the all-tests figure of runs before 2026-11-08 08:00 UTC");
    }
    // I14 outcome classes (docs/assumptions.md) of the CI-comparable tests
    let n_class = |k: &str| results.iter().filter(|r| !r.disabled && r.class == k).count();
    let _ = writeln!(
        s,
        "classes: pass {}, flagged pass {} (touched a guessed default), not trusted {} (a refusal was swallowed), fail {}, not run {}   of the {enabled_total} CI-comparable tests",
        n_class("pass"),
        n_class("flagged_pass"),
        n_class("not_trusted"),
        n_class("fail"),
        n_class("not_run")
    );
    let mut by_default: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_refusal: BTreeMap<&str, usize> = BTreeMap::new();
    for r in results.iter().filter(|r| !r.disabled && r.outcome == "pass") {
        for a in &r.assumed {
            *by_default.entry(*a).or_default() += 1;
        }
        for x in &r.swallowed {
            *by_refusal.entry(x.as_str()).or_default() += 1;
        }
    }
    let top = |m: BTreeMap<&str, usize>| {
        let mut v: Vec<_> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        v.iter().take(12).map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ")
    };
    let _ = writeln!(s, "  registered defaults touched by passing tests (tests; {} do not flag a pass): {}", al_runtime::assume::NOT_FLAGGING.join(", "), top(by_default));
    let _ = writeln!(s, "  refusals swallowed in passing tests (tests): {}", top(by_refusal));
    let _ = writeln!(s);
    let mut per_app: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for r in results.iter().filter(|r| !r.disabled) {
        let e = per_app.entry(&r.app).or_default();
        e.1 += 1;
        if r.outcome == "pass" {
            e.0 += 1;
        }
    }
    let _ = writeln!(s, "per app:");
    for (a, (p, n)) in &per_app {
        let _ = writeln!(s, "  {a:45} {p:6} / {n:6}  {:5.1}%", pct(*p, *n));
    }
    let _ = writeln!(s);
    let mut causes: HashMap<String, (usize, &'static str, String)> = HashMap::new();
    for r in results.iter().filter(|r| r.outcome != "pass" && !r.disabled) {
        let e = causes.entry(cause(&r.message)).or_insert((0, r.outcome, format!("{} :: {}", r.codeunit, r.test)));
        e.0 += 1;
    }
    let mut causes: Vec<_> = causes.into_iter().collect();
    causes.sort_by(|a, b| b.1 .0.cmp(&a.1 .0).then(a.0.cmp(&b.0)));
    let _ = writeln!(s, "top 30 failure causes (first line of the error, literals blanked):");
    for (c, (n, kind, example)) in causes.iter().take(30) {
        let _ = writeln!(s, "  {n:6}  {kind:5}  {c}\n                 e.g. {example}");
    }
    let _ = writeln!(s);
    let mut miss: Vec<_> = missing.iter().filter(|(k, _)| !k.starts_with("table ")).collect();
    miss.sort_by(|a, b| b.1.cmp(a.1));
    let _ = writeln!(s, "platform calls shimmed or missing (Session::missing, hits across all tests):");
    for (k, v) in miss.iter().take(30) {
        let _ = writeln!(s, "  {v:8}  {k}");
    }
    let mut tables: Vec<_> = missing.iter().filter(|(k, _)| k.starts_with("table ")).collect();
    tables.sort_by(|a, b| b.1.cmp(a.1));
    let _ = writeln!(s);
    let _ = writeln!(s, "record variables of tables that are not defined (platform tables missing from platform/), hits:");
    for (k, v) in tables.iter().take(40) {
        let _ = writeln!(s, "  {v:8}  {}", &k[6..]);
    }
    let summary = opts.out.join(format!("{stem}-summary.txt"));
    std::fs::write(&summary, &s).expect("write summary");
    emit(&s);
    eprintln!("wrote {} and {}", summary.display(), jsonl.display());
}

#[cfg(test)]
mod arg_tests {
    use super::*;

    #[test]
    fn codeunit_names_with_colons_stay_apart() {
        // P9, 2026-11-08: "No Accounting Periods: Basic" swallowed every later name
        assert_eq!(split_names("No Accounting Periods: Basic,ERM Sales Documents,134000"), ["No Accounting Periods: Basic", "ERM Sales Documents", "134000"]);
        assert_eq!(split_names(" A , \"B, with comma\" ,,C"), ["A", "B, with comma", "C"]);
    }

    #[test]
    fn suite_areas_continue_only_after_a_suite() {
        assert_eq!(split_list("baseapp:ERM,SCM,system"), ["baseapp:ERM,SCM", "system"]);
        assert_eq!(split_list("system,foundation,baseapp"), ["system", "foundation", "baseapp"]);
        assert_eq!(split_list("baseapp:CRM integration,Workflow"), ["baseapp:CRM integration,Workflow"]);
    }
}
