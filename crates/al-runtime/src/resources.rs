//! App resources: NavApp.GetResource / GetResourceAsText / GetResourceAsJson / ListResources.
//! An app's resources are the files under the folders listed in its app.json "resourceFolders";
//! a resource name is the path relative to such a folder (BC packs them into the .app the same
//! way). The app is the one whose code is running (the current frame's object).

use crate::interp::*;
use crate::value::*;
use al_syntax::Expr;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Resource folders per app root (directory holding app.json), cached.
fn resource_dirs(app_root: &Path) -> Vec<PathBuf> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Vec<PathBuf>>>> = OnceLock::new();
    let mut c = CACHE.get_or_init(Default::default).lock().unwrap();
    c.entry(app_root.to_path_buf())
        .or_insert_with(|| {
            let j: serde_json::Value = std::fs::read_to_string(app_root.join("app.json"))
                .ok()
                .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
                .unwrap_or_default();
            j.get("resourceFolders")
                .and_then(|r| r.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str()).map(|d| app_root.join(d)).collect())
                .unwrap_or_default()
        })
        .clone()
}

/// The directory with the app.json of the app a source file belongs to.
pub fn app_root(file: &Path) -> Option<PathBuf> {
    let mut d = file.parent();
    while let Some(dir) = d {
        if dir.join("app.json").exists() {
            return Some(dir.to_path_buf());
        }
        d = dir.parent();
    }
    None
}

/// Find `name` under `dir`, matching path components case-insensitively (as on Windows).
fn find_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut cur = dir.to_path_buf();
    for part in name.split(['/', '\\']).filter(|p| !p.is_empty()) {
        let exact = cur.join(part);
        if exact.exists() {
            cur = exact;
            continue;
        }
        let found = std::fs::read_dir(&cur).ok()?.flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?;
        cur = found.path();
    }
    cur.is_file().then_some(cur)
}

/// Resource files of the app (relative names with '/'), sorted.
fn list(app_root: &Path) -> Vec<String> {
    fn walk(base: &Path, d: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else if let Ok(rel) = p.strip_prefix(base) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = vec![];
    for d in resource_dirs(app_root) {
        walk(&d, &d, &mut out);
    }
    out.sort();
    out
}

/// `*` and `?` wildcard match, case-insensitive.
fn wildcard(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    let t: Vec<char> = s.to_lowercase().chars().collect();
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

impl<'r> Session<'r> {
    /// Bytes of resource `name` of the app whose code object `code` is.
    pub fn app_resource(&self, code: u32, name: &str) -> Option<Vec<u8>> {
        let root = app_root(&self.code(code).file)?;
        resource_dirs(&root).iter().find_map(|d| find_ci(d, name)).and_then(|p| std::fs::read(p).ok())
    }

    /// NavApp.GetResource(Name, var InStream [, TextEncoding]), GetResourceAsText(Name [, enc]),
    /// GetResourceAsJson(Name [, enc]), ListResources([Filter]).
    pub fn navapp_resource(&mut self, f: &mut Frame, m: &str, args: &[Expr]) -> R<Val> {
        let code = f.code;
        match m {
            "listresources" => {
                let filter = match args.first() {
                    Some(a) => self.eval(f, a)?.to_text(),
                    None => String::new(),
                };
                let root = app_root(&self.code(code).file);
                let names = root.map(|r| list(&r)).unwrap_or_default();
                let items = names.into_iter().filter(|n| filter.is_empty() || wildcard(&filter, n)).map(Val::Text).collect();
                Ok(Val::List(std::rc::Rc::new(std::cell::RefCell::new(items))))
            }
            _ => {
                let Some(a0) = args.first() else { return err(format!("NavApp.{m} requires a resource name")) };
                let name = self.eval(f, a0)?.to_text();
                let Some(bytes) = self.app_resource(code, &name) else {
                    // own wording [src:X18]
                    return err(format!("This app has no resource named '{name}'."));
                };
                match m {
                    "getresource" => {
                        let Some(a1) = args.get(1) else { return err("NavApp.GetResource requires an InStream") };
                        let lv = self.lvalue(f, a1)?;
                        for a in &args[2..] {
                            let _ = self.eval(f, a)?;
                        }
                        self.write_lval(&lv, crate::streams::in_stream(bytes))?;
                        Ok(Val::Void)
                    }
                    "getresourceastext" => Ok(Val::Text(text_of(&bytes))),
                    "getresourceasjson" => match crate::json::parse(&text_of(&bytes)) {
                        Some(j) => Ok(Val::Json(crate::json::jref(j))),
                        None => err(format!("The resource '{name}' is not valid JSON.")),
                    },
                    _ => {
                        *self.missing.entry(format!("NavApp.{m}")).or_default() += 1;
                        err(format!("Not implemented in bc-rust: NavApp.{m}"))
                    }
                }
            }
        }
    }
}

/// UTF-8 text without a byte order mark.
fn text_of(b: &[u8]) -> String {
    String::from_utf8_lossy(b.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(b)).into_owned()
}
