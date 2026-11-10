//! Named app sets: which first-party apps bc-server loads (`bc serve --apps standard-prod`, or
//! `BC_APPS=standard-prod`), and their lifecycle in the database. Without the option the server
//! loads `al_model::default_app_dirs` (platform, System Application, Business Foundation, Base App,
//! APIV2) and runs no install code, as before.
//!
//! A set is a manifest of BCApps source folders (relative to vendor/bcapps/src, one per line,
//! `#` comments; `@<set>` includes another set, `- <folder>` removes one). Built-in sets:
//! - `standard` (app-sets/standard.txt): the apps a BC 29.1 W1 sandbox tenant has installed
//!   (docs/standard-apps.md);
//! - `standard-prod` (app-sets/standard-prod.txt): `standard` without the test-framework apps.
//!
//! A set is loaded on top of the default apps, plus the dependency closure of its apps
//! (al_model::missing_dependency_dirs). `--apps <file>` reads a manifest file instead.
//!
//! Lifecycle ([`sync_apps`]): the installed apps and their versions are stored in BC's system table
//! "NAV App Installed App" (2000000153). On boot, apps that are not in it are installed (their
//! Install codeunits run, OnInstallAppPerDatabase then OnInstallAppPerCompany, DataVersion()
//! 0.0.0.0), apps with an older stored version are upgraded (their Upgrade codeunits run,
//! DataVersion() the stored version), and the table is updated. AL code still reads that table as
//! computed from the loaded apps (al-runtime virt.rs), which matches the stored rows once the sync
//! has run.

use al_runtime::install::{AppAction, InstallReport};
use al_runtime::{Session, Val};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The built-in sets: (name, manifest).
const SETS: &[(&str, &str)] = &[
    ("standard", include_str!("../app-sets/standard.txt")),
    ("standard-prod", include_str!("../app-sets/standard-prod.txt")),
];

/// BC's "NAV App Installed App" system table.
const NAV_APP_INSTALLED_APP: i64 = 2000000153;

fn set_text(spec: &str) -> anyhow::Result<String> {
    match SETS.iter().find(|(n, _)| n.eq_ignore_ascii_case(spec)) {
        Some((_, t)) => Ok(t.to_string()),
        None => std::fs::read_to_string(spec)
            .map_err(|e| anyhow::anyhow!("app set {spec:?}: not a built-in set ({}) and not a readable manifest file: {e}", SETS.iter().map(|s| s.0).collect::<Vec<_>>().join(", "))),
    }
}

/// The source folders a set lists (relative to vendor/bcapps/src), after its `@` includes and
/// `-` removals; and the removed folders.
fn set_entries(spec: &str, depth: usize) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    anyhow::ensure!(depth < 8, "app set {spec:?}: @ includes nest too deep");
    let text = set_text(spec)?;
    let (mut out, mut removed): (Vec<String>, Vec<String>) = (vec![], vec![]);
    for l in text.lines().map(|l| l.split('#').next().unwrap_or("").trim()).filter(|l| !l.is_empty()) {
        if let Some(inc) = l.strip_prefix('@') {
            let (e, r) = set_entries(inc.trim(), depth + 1)?;
            out.extend(e.into_iter().filter(|x| !out.contains(x)).collect::<Vec<_>>());
            removed.extend(r);
        } else if let Some(rm) = l.strip_prefix('-') {
            let rm = rm.trim().to_string();
            anyhow::ensure!(out.contains(&rm), "app set {spec:?}: removes {rm:?}, which it doesn't contain");
            out.retain(|x| *x != rm);
            removed.push(rm);
        } else if !out.iter().any(|x| x == l) {
            out.push(l.to_string());
        }
    }
    Ok((out, removed))
}

/// The app directories of set `spec`: `default` (default_app_dirs), a built-in set name, or a
/// manifest file path. Errors when a listed folder has no app.json, or when the dependency
/// closure brings back a folder the set removes.
pub fn app_set_dirs(root: &Path, spec: &str) -> anyhow::Result<Vec<PathBuf>> {
    if spec.is_empty() || spec.eq_ignore_ascii_case("default") {
        return Ok(al_model::default_app_dirs(root));
    }
    let (entries, removed) = set_entries(spec, 0)?;
    let src = root.join("vendor/bcapps/src");
    let mut extra = vec![];
    for e in entries {
        let d = src.join(&e);
        anyhow::ensure!(d.join("app.json").is_file(), "app set {spec:?}: {} has no app.json", d.display());
        extra.push(d);
    }
    let mut dirs = al_model::app_dirs_with(root, &extra);
    for d in al_model::missing_dependency_dirs(root, &dirs) {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    for r in &removed {
        let d = src.join(r);
        anyhow::ensure!(!dirs.contains(&d), "app set {spec:?}: removes {r:?}, but an app of the set depends on it");
    }
    Ok(dirs)
}

/// The app set to load: `--apps <set>` or BC_APPS (None: the default apps).
pub fn app_set_arg() -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == "--apps").and_then(|i| a.get(i + 1).cloned()).or_else(|| std::env::var("BC_APPS").ok()).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("default"))
}

/// Boot with the app set `set` names (see [`app_set_dirs`]), else the default apps.
pub fn boot_app_set(root: &Path, db: crate::app::Db, set: &Option<String>) -> anyhow::Result<crate::app::App> {
    match set {
        Some(name) => {
            let dirs = app_set_dirs(root, name)?;
            eprintln!("app set {name}: {} app folders", dirs.len());
            crate::app::boot_dirs(db, &dirs)
        }
        None => crate::app::boot(root, db),
    }
}

/// With an app set loaded: [`sync_apps`] and print what it did (BC_INSTALL=all: the default apps'
/// install and upgrade code too; BC_INSTALL=none: skip). Errors are printed, not returned: the
/// server still starts.
pub fn sync_app_set(s: &mut Session, root: &Path, set: &Option<String>) {
    if set.is_none() || std::env::var("BC_INSTALL").is_ok_and(|v| v == "none") {
        return;
    }
    let t0 = std::time::Instant::now();
    let all = std::env::var("BC_INSTALL").is_ok_and(|v| v == "all");
    match sync_apps(s, root, all) {
        Ok(rep) => print_sync_report(&rep, t0.elapsed().as_millis()),
        Err(e) => eprintln!("installing the app set failed: {e}"),
    }
}

/// What [`sync_apps`] did.
#[derive(Debug, Default)]
pub struct SyncReport {
    /// Install and Upgrade triggers that ran or failed.
    pub code: InstallReport,
    /// Apps installed now (whether or not they have Install codeunits).
    pub installed: Vec<String>,
    /// Apps upgraded now: (name, from version, to version).
    pub upgraded: Vec<(String, String, String)>,
    /// Default apps recorded as installed without running their code (the seed stands in for it).
    pub recorded: Vec<String>,
    /// Apps already installed at the loaded version.
    pub unchanged: usize,
    /// Stored versions newer than the loaded app (BC refuses a downgrade; left as they are).
    pub downgrades: Vec<String>,
    /// Apps whose install or upgrade failed (or that depend on one that did): rolled back, not
    /// recorded at the new version, so the next boot retries them.
    pub failed: Vec<String>,
}

fn version_text(v: &[i32; 4]) -> String {
    format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3])
}

/// Install the loaded apps that "NAV App Installed App" doesn't list, upgrade the ones with an
/// older stored version (both dependencies first, al_runtime install.rs), and record the loaded
/// versions. The default apps' install and upgrade code runs only with `all` (BC_INSTALL=all):
/// bc-rust's seed creates their data instead. When the table is empty (a new database, or one
/// created before the table was kept), the default apps count as installed. As in BC, an app
/// whose install (upgrade) code fails is not installed (upgraded): its changes are rolled back,
/// apps that depend on it are skipped too, and the next boot tries again.
pub fn sync_apps(s: &mut Session, root: &Path, all: bool) -> anyhow::Result<SyncReport> {
    let cat = s.cat;
    let company = s.company.clone();
    let tdef = cat.tables.get(&NAV_APP_INSTALLED_APP).ok_or_else(|| anyhow::anyhow!("platform table NAV App Installed App missing"))?;
    let schema = s.rt.schemas.get(&NAV_APP_INSTALLED_APP).cloned().ok_or_else(|| anyhow::anyhow!("no schema for NAV App Installed App"))?;
    let col = |n: &str| tdef.field(n).ok_or_else(|| anyhow::anyhow!("NAV App Installed App has no field {n}"));
    let (c_id, c_pkg, c_name, c_pub) = (col("App ID")?, col("Package ID")?, col("Name")?, col("Publisher")?);
    let c_ver = [col("Version Major")?, col("Version Minor")?, col("Version Build")?, col("Version Revision")?];
    let store_err = |e: bc_store::StoreError| anyhow::anyhow!("NAV App Installed App: {e}");

    let req = bc_store::ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: usize::MAX, lock: false };
    let rows = s.db.scan(&company, &schema, &req).map_err(store_err)?;
    let int = |v: &bc_store::Value| if let bc_store::Value::Int(i) = v { *i } else { 0 };
    let mut stored: HashMap<[u8; 16], ([i32; 4], bc_store::Row)> = HashMap::new();
    for r in rows {
        if let bc_store::Value::Guid(g) = r[c_id] {
            stored.insert(g, ([int(&r[c_ver[0]]), int(&r[c_ver[1]]), int(&r[c_ver[2]]), int(&r[c_ver[3]])], r));
        }
    }
    let baseline = stored.is_empty();
    let default_ids: HashSet<String> = al_model::apps_of_dirs(&al_model::default_app_dirs(root)).into_iter().map(|m| m.id.to_ascii_lowercase()).collect();
    let loaded = al_model::apps_of_dirs(&cat.app_dirs);
    let mut rep = SyncReport::default();
    let mut plan: HashMap<String, AppAction> = HashMap::new();
    let mut write: Vec<&al_model::AppManifest> = vec![];
    for m in &loaded {
        let Some(id) = al_runtime::value::parse_guid(&m.id) else { continue };
        let key = m.id.to_ascii_lowercase();
        let run_code = all || !default_ids.contains(&key);
        match stored.get(&id) {
            None if baseline && !run_code => rep.recorded.push(m.name.clone()),
            None => {
                if run_code {
                    plan.insert(key, AppAction::Install);
                }
                rep.installed.push(m.name.clone());
            }
            Some((v, _)) if *v < m.version => {
                if run_code {
                    plan.insert(key, AppAction::Upgrade { from: *v });
                }
                rep.upgraded.push((m.name.clone(), version_text(v), version_text(&m.version)));
            }
            Some((v, _)) if *v > m.version => {
                rep.downgrades.push(format!("{}: installed {}, loaded {}", m.name, version_text(v), version_text(&m.version)));
                continue;
            }
            Some(_) => {
                rep.unchanged += 1;
                continue;
            }
        }
        write.push(m);
    }
    if !plan.is_empty() {
        // the platform rows a tenant has before any AL runs (Company, User, All Profile): install
        // code reads them (e.g. Company.Get(CompanyName()) for "Evaluation Company"), as in al-test
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            s.init_platform_data()?;
            s.run_app_lifecycle(true, &|m| plan.get(&m.id.to_ascii_lowercase()).copied())
        }));
        match r {
            Ok(Ok(code)) => {
                s.commit()?;
                rep.code = code;
            }
            Ok(Err(e)) => {
                s.rollback();
                return Err(crate::app::flow_err(e));
            }
            Err(p) => {
                s.rollback();
                let msg = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                anyhow::bail!("install code panicked: {msg}")
            }
        }
    }
    let mods = al_runtime::modules::modules(cat);
    let failed: HashSet<String> = rep.code.failed_apps.iter().cloned().collect();
    for m in write {
        let Some(id) = al_runtime::value::parse_guid(&m.id) else { continue };
        if failed.contains(&m.id.to_ascii_lowercase()) {
            rep.installed.retain(|n| *n != m.name);
            rep.upgraded.retain(|u| u.0 != m.name);
            rep.failed.push(m.name.clone());
            continue;
        }
        let mut row = match stored.get(&id) {
            Some((_, r)) => r.clone(),
            None => schema.new_row(),
        };
        let pkg = mods.apps.iter().find(|a| a.id == id).map(|a| a.package_id).unwrap_or([0; 16]);
        let mut put = |fi: usize, v: Val| -> anyhow::Result<()> {
            row[fi] = al_runtime::record::to_store(&tdef.fields[fi], &v).map_err(|e| anyhow::anyhow!(e))?;
            Ok(())
        };
        put(c_id, Val::Guid(id))?;
        put(c_pkg, Val::Guid(pkg))?;
        put(c_name, Val::text(m.name.chars().take(250).collect::<String>()))?;
        put(c_pub, Val::text(m.publisher.chars().take(250).collect::<String>()))?;
        for (k, &fi) in c_ver.iter().enumerate() {
            put(fi, Val::Int(m.version[k]))?;
        }
        if stored.contains_key(&id) {
            s.db.modify(&company, &schema, &row).map_err(store_err)?;
        } else {
            s.db.insert(&company, &schema, &row).map_err(store_err)?;
        }
    }
    s.commit()?;
    Ok(rep)
}

/// Print a sync report (stderr, one line per failure).
pub fn print_sync_report(rep: &SyncReport, ms: u128) {
    let c = &rep.code;
    eprintln!(
        "apps: {} installed, {} upgraded, {} recorded as installed (seeded), {} unchanged, {} failed; {} install/upgrade triggers ran, {} failed, in {ms} ms",
        rep.installed.len(),
        rep.upgraded.len(),
        rep.recorded.len(),
        rep.unchanged,
        rep.failed.len(),
        c.ran.len(),
        c.failed.len()
    );
    for (app, from, to) in &rep.upgraded {
        eprintln!("  upgraded: {app} {from} -> {to}");
    }
    for d in &rep.downgrades {
        eprintln!("  not downgraded: {d}");
    }
    for (app, f) in &c.failed {
        eprintln!("  install failed: {app}: {f}");
    }
    if !rep.failed.is_empty() {
        eprintln!("  NOT INSTALLED (rolled back; the next boot retries them): {}", rep.failed.join(", "));
    }
    if std::env::var("BC_INSTALL_VERBOSE").is_ok() {
        for a in &rep.installed {
            eprintln!("  installed: {a}");
        }
        for (app, cu, t) in &c.ran {
            eprintln!("  ran: {app}: {cu} {t}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifests_list_existing_apps() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let (std_entries, _) = set_entries("standard", 0).unwrap();
        assert_eq!(std_entries.len(), 100, "100 apps installed in the BC 29.1 tenant");
        let (prod_entries, removed) = set_entries("standard-prod", 0).unwrap();
        assert_eq!(prod_entries.len() + removed.len(), 100);
        // the folder checks need the whole BCApps tree (a partial vendor copy, as on the Windows
        // test VM, skips them)
        let src = root.join("vendor/bcapps/src");
        if !std_entries.iter().all(|e| src.join(e).join("app.json").is_file()) {
            eprintln!("skipped: {} lacks some of the standard apps", src.display());
            return;
        }
        let def = al_model::default_app_dirs(&root);
        let dirs = app_set_dirs(&root, "standard").unwrap();
        assert_eq!(&dirs[..def.len()], &def[..], "default apps first, unchanged");
        assert!(dirs.len() >= def.len() + 95);
        // app_set_dirs checks that the closure doesn't bring removed folders back
        let prod = app_set_dirs(&root, "standard-prod").unwrap();
        let names: Vec<String> = al_model::apps_of_dirs(&prod).into_iter().map(|m| m.name).collect();
        for test_app in ["Test Runner", "Library Assert", "Any", "Application Test Library", "AI Test Toolkit"] {
            assert!(!names.iter().any(|n| n == test_app), "{test_app} in standard-prod");
        }
        assert_eq!(app_set_dirs(&root, "default").unwrap(), def);
    }
}
