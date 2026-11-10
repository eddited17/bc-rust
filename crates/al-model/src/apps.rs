//! The apps (extensions) a catalog was loaded from: their app.json manifests, which app an
//! object belongs to, and the order BC installs them in (dependencies first).

use serde::Serialize;
use std::path::{Path, PathBuf};

/// The Microsoft apps an app.json `"application"` dependency stands for (the "Application" app
/// bundles them).
const APPLICATION_APPS: &[&str] = &["System Application", "Business Foundation", "Base Application"];

/// An app's app.json manifest.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AppManifest {
    /// App id (GUID text as written in app.json).
    pub id: String,
    pub name: String,
    pub publisher: String,
    /// Major, minor, build, revision.
    pub version: [i32; 4],
    /// Ids of the apps it depends on (`"dependencies"`).
    pub dependencies: Vec<String>,
    /// app.json has an `"application"` version: the app depends on the Application apps.
    pub uses_application: bool,
    /// Directory holding app.json; the app's objects are the .al files below it.
    pub dir: PathBuf,
}

/// Parse a version text "30.0.12345.0" (missing parts are 0).
pub fn parse_app_version(s: &str) -> [i32; 4] {
    let mut v = [0; 4];
    for (i, p) in s.trim().split('.').take(4).enumerate() {
        v[i] = p.trim().parse().unwrap_or(0);
    }
    v
}

/// Read `<dir>/app.json`.
pub fn read_app_json(dir: &Path) -> Option<AppManifest> {
    let text = std::fs::read_to_string(dir.join("app.json")).ok()?;
    let j: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let s = |k: &str| j.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let dependencies = j
        .get("dependencies")
        .and_then(|d| d.as_array())
        .map(|a| a.iter().filter_map(|d| d.get("id").or_else(|| d.get("appId")).and_then(|v| v.as_str()).map(String::from)).collect())
        .unwrap_or_default();
    Some(AppManifest {
        id: s("id"),
        name: s("name"),
        publisher: s("publisher"),
        version: parse_app_version(&s("version")),
        dependencies,
        uses_application: j.get("application").and_then(|v| v.as_str()).is_some_and(|v| !v.is_empty()),
        dir: dir.to_path_buf(),
    })
}

/// The app a source directory belongs to: app.json in it or in the nearest parent directory.
pub fn find_app(dir: &Path) -> Option<AppManifest> {
    let mut d = Some(dir);
    while let Some(p) = d {
        if p.join("app.json").is_file() {
            return read_app_json(p);
        }
        d = p.parent();
    }
    None
}

/// The apps of the directories a catalog was loaded from (`Catalog::app_dirs`): each directory
/// with an app.json is one app (each app once, in directory order). Directories without one
/// (bc-rust's platform/ objects) belong to no app.
pub fn apps_of_dirs(dirs: &[PathBuf]) -> Vec<AppManifest> {
    let mut out: Vec<AppManifest> = vec![];
    for d in dirs {
        if let Some(a) = read_app_json(d) {
            if !out.iter().any(|o| o.dir == a.dir || (!a.id.is_empty() && o.id.eq_ignore_ascii_case(&a.id))) {
                out.push(a);
            }
        }
    }
    out
}

/// Index of the app whose directory contains `file` (the innermost one).
pub fn app_of_file(apps: &[AppManifest], file: &Path) -> Option<usize> {
    apps.iter().enumerate().filter(|(_, a)| file.starts_with(&a.dir)).max_by_key(|(_, a)| a.dir.as_os_str().len()).map(|(i, _)| i)
}

/// For every app, the indexes of the loaded apps it depends on directly.
fn direct_dependencies(apps: &[AppManifest]) -> Vec<Vec<usize>> {
    apps.iter()
        .map(|a| {
            let mut d: Vec<usize> = a.dependencies.iter().filter_map(|id| apps.iter().position(|o| o.id.eq_ignore_ascii_case(id))).collect();
            if a.uses_application {
                d.extend(apps.iter().enumerate().filter(|(_, o)| APPLICATION_APPS.contains(&o.name.as_str())).map(|(i, _)| i));
            }
            d
        })
        .collect()
}

/// The apps `roots` need: themselves and, transitively, the loaded apps they depend on
/// (`in_closure[i]`).
pub fn dependency_closure(apps: &[AppManifest], roots: &[usize]) -> Vec<bool> {
    let deps = direct_dependencies(apps);
    let mut in_closure = vec![false; apps.len()];
    let mut todo: Vec<usize> = roots.to_vec();
    while let Some(i) = todo.pop() {
        if i < apps.len() && !in_closure[i] {
            in_closure[i] = true;
            todo.extend(deps[i].iter().copied());
        }
    }
    in_closure
}

/// Indexes of the apps in the order BC installs them: every app after the apps it depends on
/// (dependencies that are not loaded are ignored), otherwise in load order.
pub fn install_order(apps: &[AppManifest]) -> Vec<usize> {
    let deps = direct_dependencies(apps);
    let mut order = Vec::with_capacity(apps.len());
    let mut state = vec![0u8; apps.len()]; // 0 new, 1 visiting, 2 done
    fn visit(i: usize, deps: &[Vec<usize>], state: &mut [u8], order: &mut Vec<usize>) {
        if state[i] != 0 {
            return; // done, or a dependency cycle (broken at this edge)
        }
        state[i] = 1;
        for &d in &deps[i] {
            if d != i {
                visit(d, deps, state, order);
            }
        }
        state[i] = 2;
        order.push(i);
    }
    for i in 0..apps.len() {
        visit(i, &deps, &mut state, &mut order);
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, id: &str, deps: &[&str], uses_application: bool) -> AppManifest {
        AppManifest { id: id.into(), name: name.into(), dependencies: deps.iter().map(|d| d.to_string()).collect(), uses_application, ..Default::default() }
    }

    #[test]
    fn install_order_puts_dependencies_first() {
        let apps = vec![
            app("Tests", "t", &["lib"], true),
            app("Base Application", "base", &["sys", "bf"], false),
            app("lib", "lib", &["sys"], false),
            app("Business Foundation", "bf", &["sys"], false),
            app("System Application", "sys", &[], false),
        ];
        let names: Vec<&str> = install_order(&apps).into_iter().map(|i| apps[i].name.as_str()).collect();
        assert_eq!(names, ["System Application", "lib", "Business Foundation", "Base Application", "Tests"]);
    }

    #[test]
    fn closure_follows_dependencies() {
        let apps = vec![
            app("Tests", "t", &["lib"], false),
            app("lib", "lib", &["sys"], false),
            app("Base Application", "base", &["sys"], false),
            app("System Application", "sys", &[], false),
        ];
        assert_eq!(dependency_closure(&apps, &[0]), [true, true, false, true]);
        let with_application = vec![app("Tests", "t", &[], true), app("Base Application", "base", &[], false)];
        assert_eq!(dependency_closure(&with_application, &[0]), [true, true]);
    }

    #[test]
    fn version_text() {
        assert_eq!(parse_app_version("30.1.2"), [30, 1, 2, 0]);
        assert_eq!(parse_app_version(""), [0, 0, 0, 0]);
    }
}

/// BCApps source folders searched for the apps that loaded apps depend on. Only W1: the
/// localization layers (Layers/DE, ...) repeat the W1 app ids.
const DEPENDENCY_SEARCH: &[&str] = &["Apps/W1", "Layers/W1", "System Application", "Business Foundation", "Tools"];

/// App directories (with app.json) below `dir`, by lower-case app id.
fn index_apps(dir: &Path, out: &mut std::collections::HashMap<String, PathBuf>) {
    if dir.join("app.json").is_file() {
        if let Some(a) = read_app_json(dir) {
            if !a.id.is_empty() {
                out.entry(a.id.to_ascii_lowercase()).or_insert(a.dir);
            }
        }
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            index_apps(&p, out);
        }
    }
}

/// The directories of the apps that the apps in `dirs` declare as dependencies in app.json,
/// transitively, found in BCApps' W1 source tree (`root`/vendor/bcapps/src) and not loaded yet:
/// e.g. PEPPOL for Tests-Misc. Microsoft's test environments install a test app's dependencies.
pub fn missing_dependency_dirs(root: &Path, dirs: &[PathBuf]) -> Vec<PathBuf> {
    let src = root.join("vendor/bcapps/src");
    let mut index = std::collections::HashMap::new();
    for s in DEPENDENCY_SEARCH {
        index_apps(&src.join(s), &mut index);
    }
    let loaded = apps_of_dirs(dirs);
    let mut have: std::collections::HashSet<String> = loaded.iter().map(|a| a.id.to_ascii_lowercase()).collect();
    let mut todo: Vec<String> = loaded.iter().flat_map(|a| a.dependencies.iter().map(|d| d.to_ascii_lowercase())).collect();
    let mut out = vec![];
    while let Some(id) = todo.pop() {
        if !have.insert(id.clone()) {
            continue;
        }
        if let Some(d) = index.get(&id) {
            if let Some(m) = read_app_json(d) {
                todo.extend(m.dependencies.iter().map(|x| x.to_ascii_lowercase()));
            }
            out.push(d.clone());
        }
    }
    out
}
