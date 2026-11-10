//! Boot: load the AL apps into a catalog and open the database.

use al_model::Catalog;
use al_runtime::{Runtime, Session, Val};
use bc_store::{Backend, MemBackend, PgBackend, Store};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct App {
    pub rt: Runtime,
    pub load_ms: u128,
}

/// Where the data lives.
pub enum Db {
    /// Embedded engine, nothing persisted (tests).
    Memory,
    /// Embedded engine with a write-ahead log file.
    Wal(PathBuf),
    /// PostgreSQL connection string (system of record).
    Postgres(String),
}

/// `canonicalize` (falling back to the path itself). On Windows it drops the `\\?\` prefix that
/// canonicalize puts on drive paths, so messages show `C:\...` (std adds the prefix back by itself
/// where a path is too long); unchanged elsewhere.
pub fn canonical(p: PathBuf) -> PathBuf {
    let c = p.canonicalize().unwrap_or(p);
    #[cfg(windows)]
    if let Some(s) = c.to_str().and_then(|s| s.strip_prefix(r"\\?\")).filter(|s| s.as_bytes().get(1) == Some(&b':')) {
        return PathBuf::from(s);
    }
    c
}

/// Default PostgreSQL connection for this machine (password in data/pg/password).
pub fn default_pg(root: &Path) -> Option<String> {
    if let Ok(url) = std::env::var("BC_PG") {
        return Some(url);
    }
    let pw = std::fs::read_to_string(root.join("data/pg/password")).ok()?;
    Some(format!("host=127.0.0.1 port=5442 user=bc dbname=bc password={}", pw.trim()))
}

pub fn boot(root: &Path, db: Db) -> anyhow::Result<App> {
    boot_dirs(db, &al_model::default_app_dirs(root))
}

/// [`boot`] with the apps in `dirs` (e.g. an app set, crate::appsets).
pub fn boot_dirs(db: Db, dirs: &[PathBuf]) -> anyhow::Result<App> {
    let t0 = std::time::Instant::now();
    let cat: &'static Catalog = Box::leak(Box::new(Catalog::load(dirs)?));
    let backend: Arc<dyn Backend> = match db {
        Db::Memory => Arc::new(MemBackend::new(Store::memory())),
        Db::Wal(p) => {
            let (s, n) = Store::open(&p, &Runtime::resolver(cat))?;
            eprintln!("database {} opened, {n} WAL records replayed", p.display());
            Arc::new(MemBackend::new(s))
        }
        Db::Postgres(url) => {
            let b = PgBackend::connect_url(&url).map_err(|e| anyhow::anyhow!("{e}"))?;
            eprintln!("database: {}", b.describe());
            Arc::new(b)
        }
    };
    let rt = Runtime::new(cat, backend);
    Ok(App { rt, load_ms: t0.elapsed().as_millis() })
}

/// Set a field by name (no validation) on a record handle.
pub fn set(s: &mut Session, r: &al_runtime::RecHandle, field: &str, v: Val) -> anyhow::Result<()> {
    let fi = r.borrow().tdef.field(field).ok_or_else(|| anyhow::anyhow!("no field {field}"))?;
    let fd = r.borrow().field(fi);
    let sv = al_runtime::record::to_store(fd, &v).map_err(|e| anyhow::anyhow!(e))?;
    r.borrow_mut().row[fi] = sv;
    let _ = s;
    Ok(())
}

pub fn get(r: &al_runtime::RecHandle, field: &str) -> Val {
    let b = r.borrow();
    match b.tdef.field(field) {
        Some(i) => b.get_val(i),
        None => Val::Void,
    }
}

pub fn flow_err(e: al_runtime::Flow) -> anyhow::Error {
    match e {
        al_runtime::Flow::Err(e) => anyhow::anyhow!("{}\n  at {}", e.msg, e.stack.join("\n  at ")),
        _ => anyhow::anyhow!("unexpected control flow"),
    }
}
