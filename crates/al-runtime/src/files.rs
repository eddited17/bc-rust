//! Server files: AL's `File` data type, `FILE.Exists/Erase/Copy/Rename/IsPathTemporary`, the
//! `Exists`/`Erase` functions and the `DotNet File` (System.IO.File) statics that Microsoft's AL
//! uses on them.
//!
//! BC writes these to the service tier's disk. In practice AL only uses server temp files
//! (FileManagement.ServerTempFileName, report datasets from Report.SaveAsXml and
//! TestRequestPage.SaveAsXml, ...), so bc-rust keeps them in memory per session ([`Vfs`]): nothing
//! touches the host file system and everything disappears with the session.
//!
//! A `File` variable holds `Val::Opaque("File")` while closed and `Val::Opaque("File#<n>")` while
//! open; `n` indexes the session's open handles.

use crate::interp::*;
use crate::value::*;
use al_syntax::{Expr, TypeRef};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Mutex;

/// The session's user folder on the server: BC resolves relative file names against it
/// (NavFile.ExpandFileName: Path.Combine(Session.UserFolder, name), then Path.GetFullPath).
pub const USER_DIR: &str = "C:\\bc-rust\\";

/// The server temp folder: TemporaryPath(), FILE.CreateTempFile,
/// FileManagement.ServerTempFileName. BC's is the TEMP subfolder of the user folder
/// (NavFile.TempPath), so `TemporaryPath() + '..\x'` is the user folder's `x`.
pub const TEMP_DIR: &str = "C:\\bc-rust\\Temp\\";

/// A server file name as BC expands it: '/' read as '\\', relative to [`USER_DIR`], with `.`,
/// `..` and empty segments resolved (Path.GetFullPath). Blank stays blank.
pub fn full_path(path: &str) -> String {
    let p = path.trim().replace('/', "\\");
    if p.is_empty() || p.starts_with("\\\\") {
        return p;
    }
    let drive = p.as_bytes().get(1) == Some(&b':');
    let abs = if drive || p.starts_with('\\') { p } else { format!("{USER_DIR}{p}") };
    let (prefix, rest) = if abs.as_bytes().get(1) == Some(&b':') { abs.split_at(2) } else { ("", abs.as_str()) };
    let mut segs: Vec<&str> = vec![];
    for s in rest.split('\\') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    let mut out = format!("{prefix}\\{}", segs.join("\\"));
    if rest.ends_with('\\') && !segs.is_empty() {
        out.push('\\');
    }
    out
}

type Data = Rc<RefCell<Vec<u8>>>;

/// In-memory server files of one session.
#[derive(Default)]
pub struct Vfs {
    /// lower-cased path → (path, content)
    files: HashMap<String, (String, Data)>,
    /// Folders made with Directory.CreateDirectory (lower-cased full paths without the trailing
    /// separator); a folder that holds files exists too.
    dirs: std::collections::HashSet<String>,
    handles: Vec<Option<Handle>>,
    seq: u64,
}

struct Handle {
    path: String,
    data: Data,
    pos: usize,
    text_mode: bool,
    /// CreateTempFile: the file goes away on Close.
    temp: bool,
    /// Code page of the TextEncoding given to Open / Create (None: not given; BC's default is
    /// MSDos, read here as UTF-8 when the bytes are valid UTF-8, else as the OEM code page).
    cp: Option<i32>,
}

/// A closed File variable with TextMode set (BC keeps the property on the variable).
const FILE_TEXT: &str = "File:text";

/// Text of file bytes in the handle's encoding.
fn file_text(cp: Option<i32>, b: &[u8]) -> String {
    match cp {
        Some(cp) => crate::dotnet::text::decode(cp, b),
        None => match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(_) => crate::dotnet::text::decode(437, b),
        },
    }
}

impl Vfs {
    fn key(path: &str) -> String {
        full_path(path).to_lowercase()
    }

    pub fn exists(&self, path: &str) -> bool {
        self.files.contains_key(&Self::key(path))
    }

    pub fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.files.get(&Self::key(path)).map(|(_, d)| d.borrow().clone())
    }

    /// Create or replace a file.
    pub fn write(&mut self, path: &str, bytes: Vec<u8>) {
        let d = self.data(path, true);
        *d.borrow_mut() = bytes;
    }

    pub fn erase(&mut self, path: &str) -> bool {
        self.files.remove(&Self::key(path)).is_some()
    }

    /// The file's content buffer (created empty when `create`).
    fn data(&mut self, path: &str, create: bool) -> Data {
        let k = Self::key(path);
        if create {
            self.files.entry(k).or_insert_with(|| (full_path(path), Rc::new(RefCell::new(vec![])))).1.clone()
        } else {
            self.files.get(&k).map(|(_, d)| d.clone()).unwrap_or_default()
        }
    }

    /// A new unique file name in the temp folder.
    pub fn temp_name(&mut self, ext: &str) -> String {
        self.seq += 1;
        let ext = ext.trim_start_matches('.');
        format!("{TEMP_DIR}__TEMP__{:032x}.{}", self.seq, if ext.is_empty() { "tmp" } else { ext })
    }

    pub fn is_temp_path(path: &str) -> bool {
        Self::key(path).starts_with(&TEMP_DIR.to_lowercase())
    }

    fn dir_key(dir: &str) -> String {
        Self::key(dir).trim_end_matches('\\').to_string()
    }

    /// Whether `dir` is a folder: the user and temp folders, a created one, or one holding files.
    pub fn dir_exists(&self, dir: &str) -> bool {
        let k = Self::dir_key(dir);
        if k.is_empty() {
            return false;
        }
        let prefix = format!("{k}\\");
        [USER_DIR, TEMP_DIR].iter().any(|d| Self::dir_key(d) == k)
            || self.dirs.contains(&k)
            || self.dirs.iter().any(|d| d.starts_with(&prefix))
            || self.files.keys().any(|f| f.starts_with(&prefix))
    }

    pub fn create_dir(&mut self, dir: &str) {
        self.dirs.insert(Self::dir_key(dir));
    }

    /// Delete a folder: its files and subfolders too with `recursive`, else only when empty.
    pub fn delete_dir(&mut self, dir: &str, recursive: bool) -> Result<(), String> {
        if !self.dir_exists(dir) {
            return Err(format!("Could not find a part of the path '{}'.", full_path(dir)));
        }
        let k = Self::dir_key(dir);
        let prefix = format!("{k}\\");
        if !recursive && (self.files.keys().any(|f| f.starts_with(&prefix)) || self.dirs.iter().any(|d| d.starts_with(&prefix))) {
            return Err(format!("The directory is not empty. : '{}'", full_path(dir)));
        }
        self.files.retain(|f, _| !f.starts_with(&prefix));
        self.dirs.retain(|d| *d != k && !d.starts_with(&prefix));
        Ok(())
    }

    /// Full paths of the entries of `dir` whose name matches `pattern` (`*` and `?` wildcards,
    /// case-insensitive), sorted: its files and/or its subfolders; `recursive` includes those of
    /// all subfolders.
    pub fn list(&self, dir: &str, pattern: &str, files: bool, dirs: bool, recursive: bool) -> Vec<String> {
        let base = full_path(dir).trim_end_matches('\\').to_string();
        let prefix = format!("{}\\", base.to_lowercase());
        let mut out: Vec<String> = vec![];
        let add = |rel: &str, is_file: bool, out: &mut Vec<String>| {
            let segs: Vec<&str> = rel.split('\\').collect();
            for (i, seg) in segs.iter().enumerate() {
                let last = i + 1 == segs.len();
                if !recursive && i > 0 {
                    break;
                }
                let entry_is_file = last && is_file;
                if (entry_is_file && files || !entry_is_file && dirs) && wildcard_match(pattern, seg) {
                    let p = format!("{base}\\{}", segs[..=i].join("\\"));
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
        };
        for (k, (display, _)) in &self.files {
            // the display path has the key's layout; get() guards a non-ASCII case change
            if let Some(rel) = display.get(prefix.len()..).filter(|r| k.starts_with(&prefix) && !r.is_empty()) {
                add(rel, true, &mut out);
            }
        }
        for d in &self.dirs {
            if d.starts_with(&prefix) {
                add(&d[prefix.len()..], false, &mut out);
            }
        }
        out.sort_by_key(|p| p.to_lowercase());
        out
    }

    fn open(&mut self, h: Handle) -> usize {
        match self.handles.iter().position(|x| x.is_none()) {
            Some(i) => {
                self.handles[i] = Some(h);
                i
            }
            None => {
                self.handles.push(Some(h));
                self.handles.len() - 1
            }
        }
    }
}

/// A file name pattern of Directory.GetFiles: `*` any run of characters, `?` one character,
/// case-insensitive; blank and `*.*` match everything.
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    fn m(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| m(&p[1..], &n[i..])),
            Some('?') => !n.is_empty() && m(&p[1..], &n[1..]),
            Some(c) => n.first().is_some_and(|x| x.to_lowercase().eq(c.to_lowercase())) && m(&p[1..], &n[1..]),
        }
    }
    let p = pattern.trim();
    if p.is_empty() || p == "*.*" || p == "*" {
        return true;
    }
    let p: Vec<char> = p.chars().collect();
    let n: Vec<char> = name.chars().collect();
    m(&p, &n)
}

/// The parts of a path as .NET's System.IO.Path splits them (Windows rules: '\\' and '/' separate,
/// a drive's ':' ends the directory part).
fn split_file_name(p: &str) -> (&str, &str) {
    match p.rfind(['\\', '/', ':']) {
        Some(i) => (&p[..i + 1], &p[i + 1..]),
        None => ("", p),
    }
}

fn path_extension(p: &str) -> &str {
    let (_, name) = split_file_name(p);
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => &name[i..],
        _ => "",
    }
}

/// System.IO.Path.Combine of two parts.
fn path_combine(a: &str, b: &str) -> String {
    let rooted = b.starts_with(['\\', '/']) || b.as_bytes().get(1) == Some(&b':');
    if a.is_empty() || rooted {
        return b.to_string();
    }
    if b.is_empty() || a.ends_with(['\\', '/', ':']) {
        return format!("{a}{b}");
    }
    format!("{a}\\{b}")
}

/// System.IO.Path.GetDirectoryName: the path without its last part ('\\' for '/'), blank for a
/// root or a bare name.
fn path_directory_name(p: &str) -> String {
    let p = p.replace('/', "\\");
    let b = p.as_bytes();
    let root_len = match (b.get(1), b.get(2)) {
        (Some(b':'), Some(b'\\')) => 3,
        (Some(b':'), _) => 2,
        _ if p.starts_with('\\') => 1,
        _ => 0,
    };
    if p.len() <= root_len {
        return String::new();
    }
    let mut end = match p[root_len..].rfind('\\') {
        Some(i) => root_len + i,
        None => root_len,
    };
    while end > root_len && b[end - 1] == b'\\' {
        end -= 1;
    }
    p[..end].to_string()
}

/// `File#<n>`: one leaked name per handle index, shared by all sessions.
fn handle_name(i: usize) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap();
    while names.len() <= i {
        let n = names.len();
        names.push(Box::leak(format!("File#{n}").into_boxed_str()));
    }
    names[i]
}

fn handle_index(t: &str) -> Option<usize> {
    t.strip_prefix("File#").and_then(|n| n.parse().ok())
}

/// A value of AL's File type.
pub fn is_file_val(v: &Val) -> bool {
    matches!(v, Val::Opaque(t) if *t == "File" || *t == FILE_TEXT || t.starts_with("File#"))
}

fn not_exist<T>(path: &str) -> R<T> {
    err(format!("The file '{path}' does not exist."))
}

impl<'r> Session<'r> {
    /// Whether `base` is a `DotNet File` (System.IO.File) variable.
    pub fn is_dotnet_file(&self, f: &Frame, base: &Expr) -> bool {
        let Expr::Ident(n) = base else { return false };
        let var = f.locals.iter().rev().find(|v| v.name.eq_ignore_ascii_case(n)).or_else(|| f.globals.vars.iter().find(|v| v.name.eq_ignore_ascii_case(n)));
        match var.and_then(|v| v.ty) {
            Some(TypeRef::Object { kind, target, .. }) if kind == "dotnet" => crate::dotnet::resolve_type(self.cat, target).eq_ignore_ascii_case("System.IO.File"),
            _ => false,
        }
    }

    fn file_args(&mut self, f: &mut Frame, args: &[Expr]) -> R<Vec<Val>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            out.push(crate::dotnet::unboxed(self.eval(f, a)?));
        }
        Ok(out)
    }

    /// `FILE.X(...)` and the `Exists` / `Erase` functions. None: not a file function.
    /// `discard`: the call's result is not used (failures raise errors).
    pub fn file_static(&mut self, f: &mut Frame, m: &str, args: &[Expr], discard: bool) -> R<Option<Val>> {
        let known = matches!(m, "exists" | "erase" | "copy" | "rename" | "ispathtemporary");
        if !known {
            return Ok(None);
        }
        let a = self.file_args(f, args)?;
        let p = |i: usize| a.get(i).map(|v| v.to_text()).unwrap_or_default();
        Ok(Some(match m {
            "exists" => Val::Bool(self.files.exists(&p(0))),
            "erase" => {
                let ok = self.files.erase(&p(0));
                if !ok && discard {
                    return not_exist(&p(0));
                }
                Val::Bool(ok)
            }
            "copy" => match self.files.read(&p(0)) {
                Some(d) => {
                    self.files.write(&p(1), d);
                    Val::Bool(true)
                }
                None => return not_exist(&p(0)),
            },
            "rename" => match self.files.read(&p(0)) {
                Some(d) => {
                    self.files.erase(&p(0));
                    self.files.write(&p(1), d);
                    Val::Bool(true)
                }
                None => return not_exist(&p(0)),
            },
            _ => Val::Bool(Vfs::is_temp_path(&p(0))),
        }))
    }

    /// `DotNet File` (System.IO.File) statics on the session's files. None: not handled here.
    pub fn dotnet_file_static(&mut self, f: &mut Frame, m: &str, args: &[Expr]) -> R<Option<Val>> {
        if !matches!(m, "exists" | "delete" | "readalltext" | "readalllines" | "writealltext" | "readallbytes" | "writeallbytes" | "appendalltext" | "copy" | "move") {
            return Ok(None);
        }
        let a = self.file_args(f, args)?;
        let p = |i: usize| a.get(i).map(|v| v.to_text()).unwrap_or_default();
        Ok(Some(match m {
            "exists" => Val::Bool(self.files.exists(&p(0))),
            "delete" => {
                self.files.erase(&p(0));
                Val::Void
            }
            "readalltext" => match self.files.read(&p(0)) {
                Some(d) => Val::Text(String::from_utf8_lossy(d.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(&d)).into_owned()),
                None => return not_exist(&p(0)),
            },
            // File.ReadAllLines: a String[] of the lines as StreamReader.ReadLine splits them (CR LF,
            // LF or CR; no empty entry after a final line break), UTF-8 with the BOM skipped
            "readalllines" => match self.files.read(&p(0)) {
                Some(d) => {
                    let text = String::from_utf8_lossy(d.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(&d)).into_owned();
                    let mut lines = vec![];
                    let mut cur = String::new();
                    let mut it = text.chars().peekable();
                    while let Some(c) = it.next() {
                        match c {
                            '\r' | '\n' => {
                                if c == '\r' && it.peek() == Some(&'\n') {
                                    it.next();
                                }
                                lines.push(Val::Text(std::mem::take(&mut cur)));
                            }
                            c => cur.push(c),
                        }
                    }
                    if !cur.is_empty() {
                        lines.push(Val::Text(cur));
                    }
                    crate::dotnet::net(crate::dotnet::NetObj::Array("System.String", lines))
                }
                None => return not_exist(&p(0)),
            },
            "readallbytes" => match self.files.read(&p(0)) {
                Some(d) => Val::Blob(d),
                None => return not_exist(&p(0)),
            },
            "writealltext" => {
                self.files.write(&p(0), p(1).into_bytes());
                Val::Void
            }
            "writeallbytes" => {
                let bytes = match a.get(1) {
                    Some(Val::Blob(b)) => b.clone(),
                    Some(v) => v.to_text().into_bytes(),
                    None => vec![],
                };
                self.files.write(&p(0), bytes);
                Val::Void
            }
            "appendalltext" => {
                let d = self.files.data(&p(0), true);
                d.borrow_mut().extend_from_slice(p(1).as_bytes());
                Val::Void
            }
            "copy" | "move" => match self.files.read(&p(0)) {
                Some(d) => {
                    if m == "move" {
                        self.files.erase(&p(0));
                    }
                    self.files.write(&p(1), d);
                    Val::Void
                }
                None => return not_exist(&p(0)),
            },
            _ => unreachable!(),
        }))
    }

    /// DotNet System.IO.Path, Directory, DirectoryInfo and FileInfo on the session's server
    /// files (Windows path rules, as on BC's service tier). DirectoryInfo and FileInfo objects
    /// are boxed full paths. Out::Unknown: a member that is not emulated.
    pub fn net_io_call(&mut self, n: &crate::dotnet::NetRef, ty: &'static str, m: &str, a: &crate::dotnet::Args) -> R<crate::dotnet::Out> {
        use crate::dotnet::{net, NetObj, Out};
        let text_array = |items: Vec<String>| net(NetObj::Array("System.String", items.into_iter().map(Val::Text).collect()));
        let info = |t: &'static str, p: String| net(NetObj::Prim(t, Val::Text(p)));
        let infos = |t: &'static str, items: Vec<String>| net(NetObj::Array(t, items.into_iter().map(|p| net(NetObj::Prim(t, Val::Text(p)))).collect()));
        let recursive = |i: usize| a.raw(i).is_some_and(|v| crate::dotnet::unboxed(v.clone()).as_i64() == 1);
        let this = match &*n.borrow() {
            NetObj::Prim(_, v) => Some(v.to_text()),
            _ => None,
        };
        let cur = this.clone().unwrap_or_default();
        let p0 = a.text(0);
        Ok(Out::Val(match (ty, m) {
            ("System.IO.Path", "combine") => Val::Text(a.v.iter().skip(1).fold(p0.clone(), |acc, v| path_combine(&acc, &v.to_text()))),
            ("System.IO.Path", "getfilename") => Val::Text(split_file_name(&p0).1.to_string()),
            ("System.IO.Path", "getfilenamewithoutextension") => {
                let name = split_file_name(&p0).1;
                Val::Text(name.rfind('.').map(|i| &name[..i]).unwrap_or(name).to_string())
            }
            ("System.IO.Path", "getextension") => Val::Text(path_extension(&p0).to_string()),
            ("System.IO.Path", "hasextension") => Val::Bool(!path_extension(&p0).is_empty()),
            ("System.IO.Path", "changeextension") => {
                let (dir, name) = split_file_name(&p0);
                let stem = name.rfind('.').map(|i| &name[..i]).unwrap_or(name);
                let ext = a.text(1);
                let ext = if ext.is_empty() || ext.starts_with('.') { ext } else { format!(".{ext}") };
                Val::Text(format!("{dir}{stem}{ext}"))
            }
            ("System.IO.Path", "getdirectoryname") => Val::Text(path_directory_name(&p0)),
            ("System.IO.Path", "getfullpath") => Val::Text(full_path(&p0)),
            ("System.IO.Path", "ispathrooted") => Val::Bool(p0.starts_with(['\\', '/']) || p0.as_bytes().get(1) == Some(&b':')),
            ("System.IO.Path", "gettemppath") => Val::Text(TEMP_DIR.into()),
            ("System.IO.Path", "gettempfilename") => {
                let p = self.files.temp_name("tmp");
                self.files.write(&p, vec![]);
                Val::Text(p)
            }
            ("System.IO.Path", "getrandomfilename") => Val::Text(format!("{}.tmp", &uuid::Uuid::from_bytes(crate::clock::new_guid()).simple().to_string()[..8])),
            ("System.IO.Path", "getinvalidfilenamechars" | "getinvalidpathchars") => {
                let mut cs: Vec<char> = (0u8..32).map(char::from).collect();
                cs.extend(['"', '<', '>', '|']);
                if m == "getinvalidfilenamechars" {
                    cs.extend([':', '*', '?', '\\', '/']);
                }
                net(NetObj::Array("System.Char", cs.into_iter().map(Val::Char).collect()))
            }
            ("System.IO.Path", "directoryseparatorchar") => Val::Char('\\'),
            ("System.IO.Path", "altdirectoryseparatorchar") => Val::Char('/'),
            ("System.IO.Directory", "exists") => Val::Bool(self.files.dir_exists(&p0)),
            ("System.IO.Directory", "createdirectory") => {
                self.files.create_dir(&p0);
                info("System.IO.DirectoryInfo", full_path(&p0))
            }
            ("System.IO.Directory", "delete") => {
                if let Err(e) = self.files.delete_dir(&p0, a.bool(1)) {
                    return err(e);
                }
                Val::Void
            }
            ("System.IO.Directory", "getfiles" | "enumeratefiles" | "getdirectories" | "enumeratedirectories" | "getfilesystementries" | "enumeratefilesystementries") => {
                if !self.files.dir_exists(&p0) {
                    return err(format!("Could not find a part of the path '{}'.", full_path(&p0)));
                }
                let files = !m.contains("directories");
                let dirs = !m.ends_with("files");
                text_array(self.files.list(&p0, &a.text(1), files, dirs, recursive(2)))
            }
            ("System.IO.Directory", "getcurrentdirectory") => Val::Text(USER_DIR.trim_end_matches('\\').into()),
            ("System.IO.DirectoryInfo" | "System.IO.FileInfo", _) if this.is_none() => return crate::dotnet::null_ref(crate::dotnet::short_name(ty), m),
            ("System.IO.DirectoryInfo" | "System.IO.FileInfo", "fullname" | "tostring") => Val::Text(cur.clone()),
            ("System.IO.DirectoryInfo" | "System.IO.FileInfo", "name") => Val::Text(split_file_name(cur.clone().trim_end_matches('\\')).1.to_string()),
            ("System.IO.DirectoryInfo", "exists") => Val::Bool(self.files.dir_exists(&cur)),
            ("System.IO.DirectoryInfo", "create") => {
                self.files.create_dir(&cur);
                Val::Void
            }
            ("System.IO.DirectoryInfo", "delete") => {
                if let Err(e) = self.files.delete_dir(&cur, a.bool(0)) {
                    return err(e);
                }
                Val::Void
            }
            ("System.IO.DirectoryInfo", "parent") => info("System.IO.DirectoryInfo", path_directory_name(&cur)),
            ("System.IO.DirectoryInfo", "getfiles" | "enumeratefiles") => infos("System.IO.FileInfo", self.files.list(&cur, &a.text(0), true, false, recursive(1))),
            ("System.IO.DirectoryInfo", "getdirectories" | "enumeratedirectories") => infos("System.IO.DirectoryInfo", self.files.list(&cur, &a.text(0), false, true, recursive(1))),
            ("System.IO.FileInfo", "exists") => Val::Bool(self.files.exists(&cur)),
            ("System.IO.FileInfo", "length") => match self.files.read(&cur) {
                Some(d) => Val::BigInt(d.len() as i64),
                None => return err(format!("Could not find file '{}'.", cur.clone())),
            },
            ("System.IO.FileInfo", "extension") => Val::Text(path_extension(&cur).to_string()),
            ("System.IO.FileInfo", "directoryname") => Val::Text(path_directory_name(&cur)),
            ("System.IO.FileInfo", "directory") => info("System.IO.DirectoryInfo", path_directory_name(&cur)),
            ("System.IO.FileInfo", "delete") => {
                self.files.erase(&cur);
                Val::Void
            }
            _ => return Ok(Out::Unknown),
        }))
    }

    /// `new System.IO.DirectoryInfo(path)` / `new System.IO.FileInfo(path)`: the boxed full path.
    pub fn net_io_new(ty: &'static str, a: &crate::dotnet::Args) -> Val {
        crate::dotnet::net(crate::dotnet::NetObj::Prim(ty, Val::Text(full_path(&a.text(0)))))
    }

    fn file_set_handle(&mut self, f: &mut Frame, base: &Expr, v: Val) -> R<()> {
        let lv = self.lvalue(f, base)?;
        self.write_lval(&lv, v)
    }

    fn file_close(&mut self, t: &str) {
        if let Some(h) = handle_index(t).and_then(|i| self.files.handles.get_mut(i)).and_then(|h| h.take()) {
            if h.temp {
                self.files.erase(&h.path);
            }
        }
    }

    /// Methods on a File variable (`t` is its value, `base` the variable).
    pub fn file_method(&mut self, f: &mut Frame, t: &'static str, base: &Expr, m: &str, args: &[Expr], discard: bool) -> R<Val> {
        match m {
            "createtempfile" | "create" | "open" => {
                let a = self.file_args(f, args)?;
                self.file_close(t);
                let (path, temp) = match m {
                    "createtempfile" => (self.files.temp_name("tmp"), true),
                    _ => (a.first().map(|v| v.to_text()).unwrap_or_default(), false),
                };
                if m == "open" && !self.files.exists(&path) {
                    if discard {
                        return not_exist(&path);
                    }
                    return Ok(Val::Bool(false));
                }
                let data = self.files.data(&path, true);
                if m != "open" {
                    data.borrow_mut().clear();
                }
                // Open(Name [, TextEncoding]) / Create(Name [, TextEncoding]) / CreateTempFile([TextEncoding])
                let enc = if m == "createtempfile" { a.first() } else { a.get(1) };
                let cp = enc.map(|e| crate::dotnet::text::text_encoding_cp(e, 0));
                let i = self.files.open(Handle { path, data, pos: 0, text_mode: t == FILE_TEXT, temp, cp });
                self.file_set_handle(f, base, Val::Opaque(handle_name(i)))?;
                Ok(Val::Bool(true))
            }
            "close" => {
                let text = handle_index(t).and_then(|i| self.files.handles.get(i)).and_then(|h| h.as_ref()).is_some_and(|h| h.text_mode);
                self.file_close(t);
                self.file_set_handle(f, base, Val::Opaque(if text { FILE_TEXT } else { "File" }))?;
                Ok(Val::Void)
            }
            _ => {
                let Some(i) = handle_index(t).filter(|&i| self.files.handles.get(i).is_some_and(|h| h.is_some())) else {
                    // TextMode / WriteMode set before Open: TextMode stays on the variable
                    return match m {
                        "textmode" if args.is_empty() => Ok(Val::Bool(t == FILE_TEXT)),
                        "textmode" => {
                            let on = self.eval(f, &args[0])?.as_bool();
                            self.file_set_handle(f, base, Val::Opaque(if on { FILE_TEXT } else { "File" }))?;
                            Ok(Val::Void)
                        }
                        "writemode" if args.is_empty() => Ok(Val::Bool(false)),
                        "writemode" => Ok(Val::Void),
                        // own wording [src:X21]
                        _ => err(format!("{m} needs an open file.")),
                    };
                };
                let a = self.file_args(f, args)?;
                // methods with a var argument: compute the value, then write the variable
                if matches!(m, "read" | "createinstream" | "createoutstream") {
                    let (out, ret) = {
                        let h = self.files.handles[i].as_mut().unwrap();
                        match m {
                            "createinstream" => (crate::streams::in_stream(h.data.borrow().clone()), Val::Void),
                            "createoutstream" => (Val::OutStream(Rc::new(crate::streams::OutTarget::Buffer(h.data.clone()))), Val::Void),
                            _ => {
                                let d = h.data.borrow().clone();
                                let rest = &d[h.pos.min(d.len())..];
                                // text mode (ALStream.ReadLine): to the line feed or a zero byte, CRs ignored
                                let (line, used): (Vec<u8>, usize) = if h.text_mode {
                                    let n = rest.iter().position(|&b| b == b'\n' || b == 0);
                                    let (l, u) = match n {
                                        Some(n) => (&rest[..n], n + 1),
                                        None => (rest, rest.len()),
                                    };
                                    (l.iter().copied().filter(|&b| b != b'\r').collect(), u)
                                } else {
                                    (rest.to_vec(), rest.len())
                                };
                                let text = file_text(h.cp, &line);
                                h.pos += used;
                                (Val::Text(text), Val::Int(used as i32))
                            }
                        }
                    };
                    if let Some(x) = args.first() {
                        let lv = self.lvalue(f, x)?;
                        self.write_lval(&lv, out)?;
                    }
                    return Ok(ret);
                }
                let h = self.files.handles[i].as_mut().unwrap();
                Ok(match m {
                    "name" => Val::Text(h.path.clone()),
                    "len" => Val::Int(h.data.borrow().len() as i32),
                    "pos" => Val::Int(h.pos as i32),
                    "seek" => {
                        h.pos = a.first().map(|v| v.as_i64().max(0) as usize).unwrap_or(0).min(h.data.borrow().len());
                        Val::Void
                    }
                    "trunc" => {
                        let p = h.pos;
                        h.data.borrow_mut().truncate(p);
                        Val::Void
                    }
                    "textmode" => match a.first() {
                        Some(v) => {
                            h.text_mode = v.as_bool();
                            Val::Void
                        }
                        None => Val::Bool(h.text_mode),
                    },
                    "writemode" => match a.first() {
                        Some(_) => Val::Void,
                        None => Val::Bool(true),
                    },
                    "write" => {
                        let text = a.first().map(|v| v.to_text()).unwrap_or_default();
                        let mut bytes = match h.cp {
                            Some(cp) => crate::dotnet::text::encode(cp, &text, false),
                            None => text.into_bytes(),
                        };
                        if h.text_mode {
                            bytes.extend_from_slice(b"\r\n");
                        }
                        let mut d = h.data.borrow_mut();
                        let end = (h.pos + bytes.len()).min(d.len());
                        d.splice(h.pos..end, bytes.iter().copied());
                        h.pos += bytes.len();
                        Val::Void
                    }
                    "getstamp" => Val::Bool(true),
                    _ => {
                        *self.missing.entry(format!("File.{m}")).or_default() += 1;
                        return err(format!("Not implemented in bc-rust: File.{m}"));
                    }
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vfs_paths() {
        let mut v = Vfs::default();
        let n = v.temp_name(".xml");
        assert!(n.ends_with(".xml") && Vfs::is_temp_path(&n));
        v.write(&n, b"<a/>".to_vec());
        assert!(v.exists(&n.to_uppercase()));
        assert_eq!(v.read(&n).unwrap(), b"<a/>");
        assert!(v.erase(&n) && !v.exists(&n));
        assert_eq!(handle_name(3), "File#3");
        // relative names live in the user folder; TemporaryPath() + '..\\x' is the user folder's x
        assert_eq!(full_path("CurrencyData.txt"), "C:\\bc-rust\\CurrencyData.txt");
        assert_eq!(full_path(&format!("{TEMP_DIR}..\\WalkMeTour")), "C:\\bc-rust\\WalkMeTour");
        assert_eq!(full_path(&format!("{TEMP_DIR}\\..\\Defs\\a.xml")), "C:\\bc-rust\\Defs\\a.xml");
        assert_eq!(full_path("C:/x/./y/"), "C:\\x\\y\\");
        assert_eq!(full_path(""), "");
        v.write("Pictures\\a.png", vec![1]);
        assert!(v.exists("C:\\bc-rust\\pictures\\A.PNG"));
    }

    #[test]
    fn system_io_paths() {
        assert_eq!(path_combine("C:\\a", "b.txt"), "C:\\a\\b.txt");
        assert_eq!(path_combine("C:\\a\\", "b"), "C:\\a\\b");
        assert_eq!(path_combine("C:\\a", "D:\\x"), "D:\\x");
        assert_eq!(path_combine("", "b"), "b");
        assert_eq!(path_directory_name("C:\\a\\b.txt"), "C:\\a");
        assert_eq!(path_directory_name("C:\\b.txt"), "C:\\");
        assert_eq!(path_directory_name("C:\\"), "");
        assert_eq!(path_directory_name("b.txt"), "");
        assert_eq!(path_directory_name("a/b"), "a");
        assert_eq!(split_file_name("C:\\a\\b.tar.gz"), ("C:\\a\\", "b.tar.gz"));
        assert_eq!(path_extension("C:\\a.b\\c"), "");
        assert_eq!(path_extension("x.XML"), ".XML");
        assert!(wildcard_match("*.xml", "Sepa Camt.XML") && !wildcard_match("*.xml", "a.xmlx") && wildcard_match("a?c", "abc"));
        let mut v = Vfs::default();
        v.write("Defs\\a.xml", vec![]);
        v.write("Defs\\b.txt", vec![]);
        v.write("Defs\\Sub\\c.xml", vec![]);
        assert!(v.dir_exists(&format!("{TEMP_DIR}\\..\\Defs")) && v.dir_exists("Defs\\Sub") && !v.dir_exists("Nope"));
        assert_eq!(v.list("Defs", "*.xml", true, false, false), vec!["C:\\bc-rust\\Defs\\a.xml"]);
        assert_eq!(v.list("Defs", "", true, false, true).len(), 3);
        assert_eq!(v.list("Defs", "", false, true, false), vec!["C:\\bc-rust\\Defs\\Sub"]);
        assert!(v.delete_dir("Defs", false).is_err());
        assert!(v.delete_dir("Defs", true).is_ok() && !v.exists("Defs\\a.xml"));
        assert_eq!(handle_index(handle_name(3)), Some(3));
    }
}
