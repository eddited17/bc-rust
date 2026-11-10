//! Apps (modules) of the loaded catalog: which app every object belongs to, with the id, name,
//! publisher and version from its app.json. Feeds the "Published Application" system table, the
//! package ids on AllObj rows, and ModuleInfo values (NavApp.GetCurrentModuleInfo /
//! GetCallerModuleInfo / GetModuleInfo). Platform objects (bc-rust's platform/ folder) belong to
//! no app, like BC's system objects.

use crate::interp::*;
use crate::value::*;
use al_model::Catalog;
use al_syntax::Expr;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone)]
pub struct AppInfo {
    pub id: [u8; 16],
    pub name: String,
    pub publisher: String,
    pub version: [i32; 4],
    pub package_id: [u8; 16],
    pub runtime_package_id: [u8; 16],
}

pub struct Modules {
    pub apps: Vec<AppInfo>,
    /// App of each code object (index into `apps`), by catalog code index.
    pub of_code: Vec<Option<u32>>,
}

impl Modules {
    pub fn app_of_code(&self, code: u32) -> Option<&AppInfo> {
        self.of_code.get(code as usize).copied().flatten().map(|i| &self.apps[i as usize])
    }
    /// App owning table `id` (the app of its table object).
    pub fn app_of_table(&self, cat: &Catalog, id: i64) -> Option<&AppInfo> {
        cat.code_by_key.get(&(al_syntax::ObjectKind::Table, id)).and_then(|&c| self.app_of_code(c))
    }
}

/// A package id derived from the app id and version (stable across runs).
fn derive_id(id: &[u8; 16], version: &[i32; 4], salt: u8) -> [u8; 16] {
    let mut out = *id;
    for (i, v) in version.iter().enumerate() {
        out[i * 4..i * 4 + 4].iter_mut().zip(v.to_le_bytes()).for_each(|(o, b)| *o ^= b);
    }
    for (i, o) in out.iter_mut().enumerate() {
        *o ^= salt.wrapping_mul(i as u8 + 1).wrapping_add(0x5d);
    }
    out[6] = (out[6] & 0x0f) | 0x40; // a well-formed v4-style guid
    out[8] = (out[8] & 0x3f) | 0x80;
    out
}

fn read_app(root: &std::path::Path) -> Option<AppInfo> {
    let text = std::fs::read_to_string(root.join("app.json")).ok()?;
    let j: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()?;
    let id = uuid::Uuid::parse_str(j.get("id")?.as_str()?).ok()?.into_bytes();
    let mut version = [0i32; 4];
    if let Some(v) = j.get("version").and_then(|v| v.as_str()) {
        for (i, p) in v.split('.').take(4).enumerate() {
            version[i] = p.parse().unwrap_or(0);
        }
    }
    Some(AppInfo {
        id,
        name: j.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        publisher: j.get("publisher").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        package_id: derive_id(&id, &version, 1),
        runtime_package_id: derive_id(&id, &version, 2),
        version,
    })
}

/// The module map of a catalog (computed once per catalog).
pub fn modules(cat: &'static Catalog) -> &'static Modules {
    static ALL: OnceLock<Mutex<HashMap<usize, &'static Modules>>> = OnceLock::new();
    let key = cat as *const Catalog as usize;
    let mut m = ALL.get_or_init(Default::default).lock().unwrap();
    m.entry(key).or_insert_with(|| {
        let mut apps: Vec<AppInfo> = vec![];
        let mut by_root: HashMap<PathBuf, Option<u32>> = HashMap::new();
        let mut by_dir: HashMap<PathBuf, Option<u32>> = HashMap::new();
        let of_code = cat
            .code
            .iter()
            .map(|co| {
                let dir = co.file.parent().map(|d| d.to_path_buf()).unwrap_or_default();
                if let Some(i) = by_dir.get(&dir) {
                    return *i;
                }
                // the app is the loaded directory the file is in (the longest matching one), whose
                // app.json describes it; nested app.json files are build modules, not apps
                let root = cat.app_dirs.iter().filter(|d| co.file.starts_with(d)).max_by_key(|d| d.as_os_str().len()).filter(|d| d.join("app.json").exists()).cloned();
                let i = match root {
                    Some(root) => *by_root.entry(root.clone()).or_insert_with(|| {
                        read_app(&root).map(|a| {
                            // the same app reached through two paths keeps one entry
                            match apps.iter().position(|x| x.id == a.id) {
                                Some(p) => p as u32,
                                None => {
                                    apps.push(a);
                                    apps.len() as u32 - 1
                                }
                            }
                        })
                    }),
                    None => None,
                };
                by_dir.insert(dir, i);
                i
            })
            .collect();
        Box::leak(Box::new(Modules { apps, of_code }))
    })
}

/// ModuleInfo values are `Opaque("ModuleInfo#<app>")`; a ModuleInfo variable that was never
/// assigned is `Opaque("ModuleInfo")`. Versions are `Opaque("Version#<app>")`.
fn tag(kind: &str, app: u32) -> &'static str {
    static TAGS: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let s = format!("{kind}#{app}");
    let mut m = TAGS.get_or_init(Default::default).lock().unwrap();
    m.entry(s.clone()).or_insert_with(|| Box::leak(s.into_boxed_str()))
}

fn tag_app(t: &str) -> Option<u32> {
    t.split_once('#').and_then(|(_, n)| n.parse().ok())
}

pub fn module_info_val(app: Option<u32>) -> Val {
    match app {
        Some(i) => Val::Opaque(tag("ModuleInfo", i)),
        None => Val::Opaque("ModuleInfo"),
    }
}

impl<'r> Session<'r> {
    /// NavApp.GetCurrentModuleInfo(var Info), GetCallerModuleInfo(var Info),
    /// GetModuleInfo(AppId, var Info): Boolean.
    pub fn navapp_module_info(&mut self, f: &mut Frame, m: &str, args: &[Expr]) -> R<Val> {
        let mods = modules(self.cat);
        let (target, app) = match m {
            "getmoduleinfo" => {
                let id = match args.first() {
                    Some(a) => self.eval(f, a)?,
                    None => Val::Void,
                };
                let app = match id {
                    Val::Guid(g) => mods.apps.iter().position(|a| a.id == g).map(|i| i as u32),
                    _ => None,
                };
                (args.get(1), app)
            }
            // the method that called the one running now (BC: the caller's module)
            "getcallermoduleinfo" => {
                let n = self.call_stack.len();
                let caller = if n >= 2 { self.call_stack[n - 2] } else { f.code };
                (args.first(), mods.of_code.get(caller as usize).copied().flatten())
            }
            _ => (args.first(), mods.of_code.get(f.code as usize).copied().flatten()),
        };
        if let Some(t) = target {
            let lv = self.lvalue(f, t)?;
            self.write_lval(&lv, module_info_val(app))?;
        }
        Ok(Val::Bool(app.is_some() || m != "getmoduleinfo"))
    }

    /// Methods of ModuleInfo / Version values.
    pub fn module_value_method(&mut self, t: &'static str, m: &str) -> Option<Val> {
        let mods = modules(self.cat);
        let app = tag_app(t).and_then(|i| mods.apps.get(i as usize));
        if t.starts_with("ModuleInfo") {
            return Some(match (m, app) {
                ("name", Some(a)) => Val::Text(a.name.clone()),
                ("publisher", Some(a)) => Val::Text(a.publisher.clone()),
                ("id", Some(a)) => Val::Guid(a.id),
                ("packageid", Some(a)) => Val::Guid(a.package_id),
                ("appversion", Some(a)) => version_val(a.version),
                // DataVersion: 0.0.0.0 while the app installs, the old version while it upgrades
                ("dataversion", Some(a)) => version_val(data_version_override(tag_app(t).unwrap_or(0)).unwrap_or(a.version)),
                ("name" | "publisher", None) => Val::Text(String::new()),
                ("id" | "packageid", None) => Val::Guid([0; 16]),
                ("appversion" | "dataversion", None) => Val::Opaque("Version"),
                ("dependencies", _) => Val::List(std::rc::Rc::new(std::cell::RefCell::new(vec![]))),
                _ => Val::Void,
            });
        }
        if t.starts_with("Version") {
            let v = version_parts(t);
            return Some(match m {
                "major" => Val::Int(v[0]),
                "minor" => Val::Int(v[1]),
                "build" => Val::Int(v[2]),
                "revision" => Val::Int(v[3]),
                "totext" => Val::Text(version_text(t)),
                _ => Val::Void,
            });
        }
        None
    }
}

/// A Version value: `Opaque("Version#=1.2.3.4")` (-1: a part Version.Create left out).
pub fn version_val(v: [i32; 4]) -> Val {
    static TAGS: OnceLock<Mutex<HashMap<[i32; 4], &'static str>>> = OnceLock::new();
    let mut m = TAGS.get_or_init(Default::default).lock().unwrap();
    Val::Opaque(m.entry(v).or_insert_with(|| Box::leak(format!("Version#={}.{}.{}.{}", v[0], v[1], v[2], v[3]).into_boxed_str())))
}

/// Major, minor, build and revision of a Version value. `Version#<app>` (older values) is that
/// app's version; a Version variable that was never assigned is 30.0.0.0, as before.
pub fn version_parts(t: &str) -> [i32; 4] {
    if let Some(s) = t.strip_prefix("Version#=") {
        let mut v = [-1; 4];
        for (i, p) in s.split('.').take(4).enumerate() {
            v[i] = p.parse().unwrap_or(-1);
        }
        return v;
    }
    crate::record::CATALOG.get().and_then(|cat| tag_app(t).and_then(|i| modules(cat).apps.get(i as usize)).map(|a| a.version)).unwrap_or([30, 0, 0, 0])
}

/// Format(Version): "30.0.0.0"; parts Version.Create left out are not shown ("1.2").
pub fn version_text(t: &str) -> String {
    let v = version_parts(t);
    let n = v.iter().rposition(|&p| p >= 0).map_or(0, |i| i + 1).max(2);
    v[..n].iter().map(|p| p.to_string()).collect::<Vec<_>>().join(".")
}

thread_local! {
    /// DataVersion() of apps that are installing or upgrading in this thread (crate::install).
    static DATA_VERSIONS: std::cell::RefCell<HashMap<u32, [i32; 4]>> = Default::default();
}

/// Set (Some) or clear (None) the DataVersion() an app reports while it installs or upgrades.
pub fn set_data_version(app: u32, v: Option<[i32; 4]>) {
    DATA_VERSIONS.with(|m| match v {
        Some(v) => m.borrow_mut().insert(app, v),
        None => m.borrow_mut().remove(&app),
    });
}

fn data_version_override(app: u32) -> Option<[i32; 4]> {
    DATA_VERSIONS.with(|m| m.borrow().get(&app).copied())
}

impl<'r> Session<'r> {
    /// Version.Create(Major, Minor[, Build[, Revision]]) or Version.Create(Text).
    pub fn version_create(&mut self, f: &mut Frame, args: &[Expr]) -> R<Val> {
        let mut vals = vec![];
        for a in args {
            vals.push(self.eval(f, a)?);
        }
        if let [Val::Text(s)] = &vals[..] {
            let mut v = [-1; 4];
            for (i, p) in s.trim().split('.').take(4).enumerate() {
                v[i] = match p.trim().parse() {
                    Ok(n) => n,
                    Err(_) => return err(format!("The version '{s}' is not valid.")),
                };
            }
            return Ok(version_val(v));
        }
        let mut v = [-1; 4];
        for (i, x) in vals.iter().take(4).enumerate() {
            v[i] = x.as_i64() as i32;
        }
        Ok(version_val(v))
    }
}

impl<'r> Session<'r> {
    /// NavApp.GetCallerCallstackModuleInfos(): the apps of the code below the running method on
    /// the call stack, innermost first, each app once (platform objects belong to none).
    pub fn navapp_callstack_module_infos(&mut self, f: &Frame) -> Val {
        let mods = modules(self.cat);
        let n = self.call_stack.len();
        let below = if n > 0 && self.call_stack[n - 1] == f.code { &self.call_stack[..n - 1] } else { &self.call_stack[..] };
        let mut seen: Vec<u32> = vec![];
        for &c in below.iter().rev() {
            if let Some(a) = mods.of_code.get(c as usize).copied().flatten() {
                if !seen.contains(&a) {
                    seen.push(a);
                }
            }
        }
        Val::List(std::rc::Rc::new(std::cell::RefCell::new(seen.into_iter().map(|a| module_info_val(Some(a))).collect())))
    }
}
