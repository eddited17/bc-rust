//! demo-data: create the full CRONUS demo company by running Microsoft's Contoso demo tool
//! (codeunit 5193 "Contoso Demo Tool".CreateAllDemoData, Contoso Coffee Demo Dataset app) through
//! bc-rust, on the in-memory engine, a WAL file or PostgreSQL.
//!
//! ```text
//! demo-data [--memory | --db FILE.wal | --pg CONN | --pg-default] [--company CRONUS]
//!           [--setup-only] [--json FILE] [--tables N] [--trace]
//!           [--extra APPDIR]... [--call CODEUNIT:PROCEDURE]
//! ```
//! `--pg CONN` is a libpq connection string ("host=127.0.0.1 port=5442 user=bc dbname=bc_demo
//! password=..."); `--pg-default` uses the connection `bc serve --pg` uses (BC_PG, else
//! data/pg/password with database `bc`, the live demo). The target company must be empty: drop
//! its schema (c_<company>) and `shared` first.
//!
//! `--extra` loads more app directories; `--call` runs one parameterless procedure (after the
//! platform rows) instead of the demo tool, for probing the interpreter with scratch AL.
//!
//! `--tool w1demo` creates the company with Microsoft's legacy W1 demo tool instead (DemoTool app,
//! codeunit 101899, data type Extended; bc_server::demo::w1): the company BCApps' CI runs the Base
//! App tests on. `--tool contoso` (default) is the Contoso demo tool.

use al_runtime::Session;
use bc_server::app::Db;
use bc_server::demo::{self, Level};
use std::path::PathBuf;
use std::time::Instant;

fn usage() -> ! {
    eprintln!(
        "usage: demo-data [--memory | --db FILE.wal | --pg CONN | --pg-default] [--company NAME] [--setup-only]\n\
         \x20                [--json FILE] [--tables N] [--trace] [--extra APPDIR]... [--call CODEUNIT:PROCEDURE]\n\
         \x20                [--tool contoso|w1demo]"
    );
    std::process::exit(2)
}

fn repo_root() -> PathBuf {
    let candidates = [std::env::current_dir().ok(), Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))];
    for c in candidates.into_iter().flatten() {
        if c.join("vendor/bcapps/src").exists() {
            return bc_server::app::canonical(c);
        }
    }
    eprintln!("vendor/bcapps not found (run from the repo root)");
    std::process::exit(1)
}

fn main() -> anyhow::Result<()> {
    let root = repo_root();
    // IsolatedStorage.SetEncrypted: the fixed test-only key unless BC_ISOLATED_STORAGE_KEY(_FILE) is set
    al_runtime::isolated::use_test_key();
    let mut db = Db::Memory;
    let mut company = "CRONUS".to_string();
    let mut level = Level::All;
    let mut json: Option<PathBuf> = None;
    let mut show_tables = 40usize;
    let mut trace = false;
    let mut extra: Vec<PathBuf> = vec![];
    let mut call: Option<String> = None;
    let mut w1demo = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--memory" => db = Db::Memory,
            "--pg" => db = Db::Postgres(val()),
            "--pg-default" => db = Db::Postgres(bc_server::app::default_pg(&root).ok_or_else(|| anyhow::anyhow!("no PostgreSQL connection (set BC_PG or data/pg/password)"))?),
            "--db" => db = Db::Wal(PathBuf::from(val())),
            "--company" => company = val(),
            "--setup-only" => level = Level::Setup,
            "--json" => json = Some(PathBuf::from(val())),
            "--tables" => show_tables = val().parse().unwrap_or_else(|_| usage()),
            "--trace" => trace = true,
            "--extra" => extra.push(PathBuf::from(val())),
            "--call" => call = Some(val()),
            "--tool" => {
                w1demo = match val().to_ascii_lowercase().as_str() {
                    "contoso" => false,
                    "w1demo" | "w1" | "legacy" => true,
                    _ => usage(),
                }
            }
            _ => usage(),
        }
    }
    let t0 = Instant::now();
    let rt = if w1demo {
        let mut dirs = demo::w1::w1_demo_app_dirs(&root);
        dirs.extend(extra.iter().cloned());
        demo::open_runtime_dirs(&dirs, db)?
    } else {
        demo::open_runtime(&root, db, &extra)?
    };
    let cat = rt.cat;
    for w in cat.warnings.iter().filter(|w| !w.contains("unknown publisher")).take(10) {
        eprintln!("catalog: {w}");
    }
    eprintln!("catalog: {} files, {} objects, {} parse errors, {} ms", cat.stats.files, cat.stats.objects, cat.stats.parse_errors, t0.elapsed().as_millis());
    eprintln!("database: {}", rt.db.describe());

    if let Some(c) = call {
        let (id, p) = c.split_once(':').unwrap_or_else(|| usage());
        let id: i64 = id.parse().unwrap_or_else(|_| usage());
        return std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(1 << 30)
                .spawn_scoped(scope, || -> anyhow::Result<()> {
                    let mut s = Session::new(&rt, &company);
                    s.trace = trace;
                    s.init_platform_data().map_err(|_| anyhow::anyhow!("platform data"))?;
                    let r = demo::call_codeunit(&mut s, id, p);
                    for m in &s.messages {
                        println!("message: {m}");
                    }
                    r
                })?
                .join()
                .map_err(|_| anyhow::anyhow!("panicked"))?
        });
    }

    let report = if w1demo { demo::seed_with(&rt, &company, trace, demo::w1::create_w1_demo_company)? } else { demo::seed(&rt, &company, level, trace)? };
    let wall = t0.elapsed();
    println!("demo company {} on {}", report.company, report.backend);
    println!("Company-Initialize + platform rows: {:.0} ms", report.init_ms);
    if w1demo {
        println!("legacy W1 DemoTool ({}): {:.0} ms, {} AL statements", demo::w1::DATA_TYPE, report.contoso_ms, report.steps);
    } else {
        println!("Contoso Demo Tool ({:?}): {:.0} ms, {} AL statements", level, report.contoso_ms, report.steps);
    }
    println!("total: {} rows in {} tables, wall time {:.1} s (including catalog load)", report.total_rows, report.tables.len(), wall.as_secs_f64());
    for w in &report.warnings {
        println!("warning: {w}");
    }
    println!();
    println!("rows written per module and level (net inserts):");
    let mut per_module: Vec<(String, i64, f64)> = vec![];
    for p in &report.phases {
        match per_module.iter_mut().find(|m| m.0 == p.module) {
            Some(m) => {
                m.1 += p.rows;
                m.2 += p.ms;
            }
            None => per_module.push((p.module.clone(), p.rows, p.ms)),
        }
        println!("  {:28} {:20} {:8} rows {:9.0} ms", p.module, p.level, p.rows, p.ms);
    }
    println!();
    println!("per module:");
    for (m, rows, ms) in &per_module {
        println!("  {m:28} {rows:8} rows {ms:9.0} ms");
    }
    if show_tables > 0 {
        println!();
        println!("largest tables:");
        for t in report.tables.iter().take(show_tables) {
            println!("  {:8} {:10} {}", t.rows, t.id, t.name);
        }
    }
    if !report.missing.is_empty() {
        let mut miss: Vec<_> = report.missing.iter().collect();
        miss.sort_by(|a, b| b.1.cmp(a.1));
        println!();
        println!("platform calls shimmed or missing (hits):");
        for (k, v) in miss.iter().take(30) {
            println!("  {v:8}  {k}");
        }
    }
    if let Some(p) = json {
        std::fs::write(&p, serde_json::to_vec_pretty(&report)?)?;
        eprintln!("wrote {}", p.display());
    }
    Ok(())
}
