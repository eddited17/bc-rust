//! bc-nst: bc-rust answering as a Business Central service tier on the client
//! services port, so Microsoft's web server can open sessions against it.
//!
//! usage: bc-nst [--host 127.0.0.1] [--port 7185] [--memory | --pg <conninfo>] [--profile <profile id>]
//!               [--apps <set>] [--bg-workers N]
//!
//! `--bg-workers N` / BC_BG_WORKERS=N: run Scheduled Tasks (the job queue, "Post with Job Queue")
//! and StartSession sessions in background sessions of their own, as `bc serve` does
//! (al_runtime::workers, docs/job-queue.md). Off by default: without it, a job queue entry made
//! in the web client waits for a server that runs the pool on the same database.
//!
//! `--apps <set>` / BC_APPS=<set>: load an app set (e.g. `standard-prod`) and install or upgrade
//! its apps in the database at boot, as bc-server does (bc_server::appsets). Both servers can use
//! the same database: the installed apps are recorded there ("NAV App Installed App").
//!
//! Database: PostgreSQL database `bc_nst` on the local server by default
//! (password from data/pg/password, or the full connection string in BC_NST_PG).
//! It never falls back to database `bc`, which is the live demo.
//! Run from the repository root (the AL apps are loaded from there).

use al_runtime::Session;
use bc_nst::rpc::{fill_all_profile, NstConfig, NstState};
use bc_server::app::Db;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned())
}

fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

/// Like `bc_server::app::default_pg`, but for database `bc_nst`.
fn nst_pg(root: &Path) -> Option<String> {
    if let Ok(url) = std::env::var("BC_NST_PG") {
        return Some(url);
    }
    let pw = std::fs::read_to_string(root.join("data/pg/password")).ok()?;
    Some(format!("host=127.0.0.1 port=5442 user=bc dbname=bc_nst password={}", pw.trim()))
}

fn main() -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let host = arg("--host").unwrap_or_else(|| "127.0.0.1".into());
    // not 7085: on this host that port forwards to the real service tier in a VM
    let port: u16 = arg("--port").and_then(|p| p.parse().ok()).unwrap_or(7185);
    let db = if flag("--memory") {
        Db::Memory
    } else {
        let url = match arg("--pg") {
            Some(u) => u,
            None => nst_pg(&root).ok_or_else(|| anyhow::anyhow!("no PostgreSQL connection: set BC_NST_PG, create data/pg/password, or use --memory"))?,
        };
        if url.split_whitespace().any(|kv| kv == "dbname=bc") {
            anyhow::bail!("refusing to use database `bc` (the live demo); use dbname=bc_nst");
        }
        Db::Postgres(url)
    };
    let app_set = bc_server::appsets::app_set_arg();
    let app = bc_server::appsets::boot_app_set(&root, db, &app_set)?;
    let rt: &'static al_runtime::Runtime = Box::leak(Box::new(app.rt));
    let mut cfg = NstConfig::default();
    // A new metadata token per start: the web server keeps its metadata caches while the
    // token is unchanged, and the catalog may have changed since the last run. JSON-RPC
    // (cfg) and the metadata endpoints (MP_15 / VersionNumber / MetadataToken header) share it.
    cfg.metadata_token = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(1);
    {
        let mut s = Session::try_new(rt, &cfg.company).map_err(|e| anyhow::anyhow!("{e}"))?;
        let ci = s.cat.table_by_name("Company Information").map(|t| t.id).ok_or_else(|| anyhow::anyhow!("no Company Information table"))?;
        if rt.db.table_count(&cfg.company, &rt.schemas[&ci]) == 0 {
            let t0 = Instant::now();
            let n = bc_server::seed::seed(&mut s)?;
            rt.db.compact()?;
            eprintln!("seeded demo company {} with {n} master records in {:?}", cfg.company, t0.elapsed());
        }
        // the set's apps that this database doesn't have installed yet (or at an older version)
        if app_set.is_some() {
            bc_server::appsets::sync_app_set(&mut s, &root, &app_set);
            rt.db.compact()?;
        }
        // the default role center: `--profile <id>`, else the page System Action
        // Triggers.GetDefaultRoleCenterID names (Default Role Center Impl.), else Business Manager
        match arg("--profile") {
            Some(p) => cfg.profile_id = p,
            None => match bc_nst::platform::default_role_center(&mut s) {
                Ok(rc) if rc > 0 => match s.cat.profiles.iter().find(|p| p.role_center == Some(rc)) {
                    Some(p) => {
                        cfg.profile_id = p.name.clone();
                        cfg.profile_caption = p.caption.clone();
                        eprintln!("default role center {rc} (GetDefaultRoleCenterID): profile {}", p.name);
                    }
                    None => eprintln!("GetDefaultRoleCenterID named page {rc}, which no profile uses; keeping {}", cfg.profile_id),
                },
                Ok(_) => {}
                Err(e) => {
                    s.rollback();
                    eprintln!("GetDefaultRoleCenterID failed: {}", bc_nst::platform::flow_message(&e));
                }
            },
        }
        // platform data the web server reads while starting a session
        let n = fill_all_profile(&mut s, &cfg)?;
        eprintln!("All Profile: {n} profiles (default {})", cfg.profile_id);
    }
    // the background pool (job queue, StartSession), the same as bc serve's; BC_BG_* settings
    // (docs/job-queue.md, "Configuration")
    let bg_workers = arg("--bg-workers").or_else(|| std::env::var("BC_BG_WORKERS").ok()).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
    if bg_workers > 0 {
        // the pool's AutoIncrement mode (workers::start) from the first request on, not only
        // once the pool runs
        al_runtime::recops::set_identity_autoincrement(true);
    }
    {
        // create missing tables/columns ahead of first use, as bc-server does; the pool starts
        // when they are ready, so its first tasks don't set up tables beside the sync
        let schemas: Vec<Arc<bc_store::TableSchema>> = rt.schemas.values().cloned().collect();
        let company = cfg.company.clone();
        std::thread::spawn(move || {
            match rt.db.sync_schema(&company, &schemas) {
                Ok(n) if n > 0 => eprintln!("schema sync: {n} tables ready"),
                Ok(_) => {}
                Err(e) => eprintln!("schema sync failed: {e}"),
            }
            if bg_workers > 0 {
                al_runtime::workers::start(rt, al_runtime::workers::Config::with_workers(bg_workers, &company));
            }
        });
    }
    // BC_AUTH (bc_server::auth): read the configuration now, so a bad one stops the start
    bc_server::auth::authenticator();
    let state = NstState::new(rt, cfg);
    let router = bc_nst::rpc::router(state.clone()).merge(metadata_routes(&state)).fallback(unknown_route);
    let tokio = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    tokio.block_on(async move {
        let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
        eprintln!("bc-nst listening on ws://{host}:{port}/{{instance}}/client/ws/connect (catalog+db ready in {} ms)", app.load_ms);
        axum::serve(listener, router).await?;
        anyhow::Ok(())
    })
}

/// HTTP side endpoints under `/{instance}/client/metadata` (`bc_nst::metadata`), using the
/// same metadata token and default profile as the JSON-RPC endpoint.
fn metadata_routes(state: &Arc<NstState>) -> axum::Router {
    let ms = bc_nst::metadata::MetadataState::new(state.rt.cat, state.cfg.metadata_token).with_profile(state.cfg.profile_id.clone());
    bc_nst::metadata::mounted_router(ms)
}

/// Log what the web server asks for that bc-nst doesn't serve yet.
async fn unknown_route(method: axum::http::Method, uri: axum::http::Uri) -> axum::http::StatusCode {
    eprintln!("bc-nst: no route for {method} {uri}");
    axum::http::StatusCode::NOT_FOUND
}
