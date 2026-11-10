//! Business Central permissions (design: docs/permissions.md).
//!
//! - The permission model of a catalog (`model`): the apps' PermissionSet objects with their
//!   PermissionSetExtensions, expanded through IncludedPermissionSets / ExcludedPermissionSets
//!   into one mask per object (`RoleMasks`), the indirect permissions every object grants through
//!   its `Permissions` property (an extension object's go to its base object, as in BC), and the
//!   tables' `InherentPermissions`.
//! - `TestPermissions` of a test method, as Microsoft's test runner sees it.
//! - The session's permission state (`PermState`): the test's effective permission sets that the
//!   test framework's Permissions Mock sets through the DotNet `PermissionTestHelper` (BC:
//!   NavTestExecution.EffectivePermissionSets), and a per-session cache of the allowed operations
//!   per (calling object, table), so a check costs one bool test when nothing is enforced and
//!   one hash lookup when it is.
//! - Enforcement (`Session::check_record_op`) on the record operations AL code calls, behind a
//!   switch (`set_enforce`; al-test `--enforce-permissions`, `BC_PERMISSIONS=enforce`).
//!
//! Rules (Ncl PermissionSetBase.InternalHasPermissionsDoNotCheckIncludedExcludedPermissions,
//! VerifyIndirectPermissions, PermissionSet.VerifyInherentPermissions, RecordImplementation
//! .VerifyPermissions / IsPermissionCheckNeeded): an operation on table data is allowed when the
//! roles grant it directly; or the roles grant it at least indirectly and the object whose code
//! runs (the current method scope; for an extension object its base object) has it in its
//! `Permissions`; or the table's `InherentPermissions` grant it; or, for Read, ModifyAll and
//! DeleteAll, the table is empty. Temporary records are never checked.

use crate::interp::*;
use crate::record::RecHandle;
use crate::value::Val;
use std::rc::Rc;
use al_model::Catalog;
use al_syntax::{ObjectKind, Tk, Token};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// PermissionMask bits (Ncl PermissionMask): direct Read..Execute, indirect ones shifted by 5.
pub const READ: u16 = 1;
pub const INSERT: u16 = 2;
pub const MODIFY: u16 = 4;
pub const DELETE: u16 = 8;
pub const EXECUTE: u16 = 16;
const DIRECT: u16 = 0x1f;

/// Permission tables computed from the model (virt::catalog_table, Session::expanded_permission_rows).
pub const METADATA_PERMISSION: i64 = 2000000251;
pub const METADATA_PERMISSION_SET_REL: i64 = 2000000252;
pub const EXPANDED_PERMISSION: i64 = 2000000254;

/// "Object Type" of a permission (Permission / Metadata Permission option ordinals).
pub const TABLE_DATA: u8 = 0;
pub const TABLE: u8 = 1;

fn object_type(word: &str) -> Option<(u8, Option<ObjectKind>)> {
    Some(match word.to_ascii_lowercase().as_str() {
        "tabledata" => (TABLE_DATA, Some(ObjectKind::Table)),
        "table" => (TABLE, Some(ObjectKind::Table)),
        "report" => (3, Some(ObjectKind::Report)),
        "codeunit" => (5, Some(ObjectKind::Codeunit)),
        "xmlport" => (6, Some(ObjectKind::XmlPort)),
        "page" => (8, Some(ObjectKind::Page)),
        "query" => (9, Some(ObjectKind::Query)),
        "system" => (10, None),
        _ => return None,
    })
}

/// Mask letters: R I M D X direct (with their indirect bit), r i m d x indirect only.
fn mask_of(letters: &str) -> u16 {
    let mut m = 0;
    for c in letters.chars() {
        let b = match c.to_ascii_uppercase() {
            'R' => READ,
            'I' => INSERT,
            'M' => MODIFY,
            'D' => DELETE,
            'X' => EXECUTE,
            _ => continue,
        };
        m |= b << 5;
        if c.is_ascii_uppercase() {
            m |= b;
        }
    }
    m
}

/// Split property tokens at top-level commas.
fn segments(toks: &[Token]) -> Vec<&[Token]> {
    let mut out = vec![];
    let (mut depth, mut start) = (0i32, 0);
    for (i, t) in toks.iter().enumerate() {
        match t.kind {
            Tk::LParen | Tk::LBrack => depth += 1,
            Tk::RParen | Tk::RBrack => depth -= 1,
            Tk::Comma if depth == 0 => {
                out.push(&toks[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < toks.len() {
        out.push(&toks[start..]);
    }
    out
}

fn is_name(t: &Token) -> bool {
    matches!(t.kind, Tk::Ident | Tk::QIdent | Tk::Str)
}

/// `Permissions = tabledata "G/L Entry" = rimd, table * = X, codeunit 80 = X, ...` as
/// (object type, object id (0 = every object of the type), mask). Unknown objects are left out.
pub fn parse_permissions(cat: &Catalog, toks: &[Token]) -> Vec<(u8, i64, u16)> {
    let mut out = vec![];
    for seg in segments(toks) {
        let Some(eq) = seg.iter().rposition(|t| t.kind == Tk::Eq) else { continue };
        let (Some(first), Some(mask)) = (seg.first(), seg.get(eq + 1)) else { continue };
        let Some((ty, kind)) = object_type(&first.text) else { continue };
        let name = &seg[1..eq];
        let id = if name.iter().any(|t| t.kind == Tk::Star || &*t.text == "*") {
            Some(0)
        } else if let Some(t) = name.iter().rev().find(|t| t.kind == Tk::Int) {
            t.text.parse().ok()
        } else {
            let n = name.iter().rev().find(|t| is_name(t)).map(|t| t.text.to_string()).unwrap_or_default();
            kind.and_then(|k| cat.id_of(k, &n))
        };
        if let Some(id) = id {
            out.push((ty, id, mask_of(&mask.text)));
        }
    }
    out
}

/// `IncludedPermissionSets = "D365 BUS PREMIUM", SECURITY, ...` as lower-case names.
pub fn parse_names(toks: &[Token]) -> Vec<String> {
    segments(toks).into_iter().filter_map(|s| s.iter().rev().find(|t| is_name(t)).map(|t| t.text.to_ascii_lowercase())).collect()
}

/// The expanded permissions of a role: mask by (object type, object id), id 0 = wildcard.
pub type RoleMasks = HashMap<(u8, i64), u16>;

/// The permission model of a catalog.
pub struct Model {
    cat: &'static Catalog,
    /// Lower-case role id (permission set name) → its PermissionSet objects.
    sets: HashMap<String, Vec<usize>>,
    /// Lower-case extended set name → its PermissionSetExtension objects.
    exts: HashMap<String, Vec<usize>>,
    roles: Mutex<HashMap<String, Arc<RoleMasks>>>,
    /// Indirect permissions an object grants (its own and its extensions' `Permissions`): table
    /// → direct bits, by the base object's (kind, id).
    indirect: HashMap<(ObjectKind, i64), HashMap<i64, u16>>,
    /// Tables' `InherentPermissions` (direct bits).
    inherent: HashMap<i64, u16>,
    /// `[InherentPermissions(PermissionObjectType::TableData, Database::X, 'rm')]` on procedures:
    /// (table, mask) by (code object, procedure).
    method_inherent: HashMap<(u32, u32), Vec<(i64, u16)>>,
    /// App id by app directory (set_app).
    apps_by_root: Mutex<HashMap<PathBuf, [u8; 16]>>,
}

/// The permission model of `cat` (built once per catalog).
pub fn model(cat: &'static Catalog) -> &'static Model {
    static ALL: OnceLock<Mutex<HashMap<usize, &'static Model>>> = OnceLock::new();
    let key = cat as *const Catalog as usize;
    let mut m = ALL.get_or_init(Default::default).lock().unwrap();
    m.entry(key).or_insert_with(|| Box::leak(Box::new(Model::new(cat))))
}

impl Model {
    fn new(cat: &'static Catalog) -> Model {
        let mut sets: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, p) in cat.permission_sets.iter().enumerate() {
            sets.entry(p.name.to_ascii_lowercase()).or_default().push(i);
        }
        let mut exts: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, e) in cat.permission_set_exts.iter().enumerate() {
            exts.entry(e.extends.to_ascii_lowercase()).or_default().push(i);
        }
        let mut indirect: HashMap<(ObjectKind, i64), HashMap<i64, u16>> = HashMap::new();
        for (&(kind, id), toks) in &cat.object_permissions {
            // an extension object's permissions are its base object's (NavApplicationObjectBase
            // .GetIndirectPermissionMask asks the parent)
            let base = cat.code_by_key.get(&(kind, id)).and_then(|&ci| cat.code[ci as usize].extends).unwrap_or((kind, id));
            let m = indirect.entry(base).or_default();
            for (ty, table, mask) in parse_permissions(cat, toks) {
                if ty == TABLE_DATA {
                    // both cases grant: the object's code may use the user's indirect permission
                    *m.entry(table).or_default() |= (mask | mask >> 5) & DIRECT;
                }
            }
        }
        let mut inherent = HashMap::new();
        for (&(kind, id), toks) in &cat.inherent_permissions {
            if kind == ObjectKind::Table {
                let letters: String = toks.iter().map(|t| t.text.to_string()).collect();
                let m = mask_of(&letters);
                inherent.insert(id, (m | m >> 5) & (READ | INSERT | MODIFY | DELETE));
            }
        }
        let mut method_inherent: HashMap<(u32, u32), Vec<(i64, u16)>> = HashMap::new();
        for (ci, co) in cat.code.iter().enumerate() {
            for (pi, p) in co.procs.iter().enumerate() {
                for a in p.attributes.iter().filter(|a| a.name.eq_ignore_ascii_case("inherentpermissions")) {
                    if let Some(g) = method_inherent_arg(cat, &a.args) {
                        method_inherent.entry((ci as u32, pi as u32)).or_default().push(g);
                    }
                }
            }
        }
        Model { cat, sets, exts, roles: Mutex::new(HashMap::new()), indirect, inherent, method_inherent, apps_by_root: Mutex::new(HashMap::new()) }
    }

    /// Whether a permission set with this role id exists.
    pub fn has_set(&self, role: &str) -> bool {
        self.sets.contains_key(&role.to_ascii_lowercase())
    }

    /// The expanded permissions of a role id (metadata permission sets).
    pub fn role(&self, role: &str) -> Arc<RoleMasks> {
        let key = role.to_ascii_lowercase();
        if let Some(r) = self.roles.lock().unwrap().get(&key) {
            return r.clone();
        }
        let r = Arc::new(self.expand(&key, &mut HashSet::new()));
        self.roles.lock().unwrap().insert(key, r.clone());
        r
    }

    fn expand(&self, key: &str, seen: &mut HashSet<String>) -> RoleMasks {
        let mut m = RoleMasks::new();
        if !seen.insert(key.to_string()) {
            return m;
        }
        let (mut inc, mut exc) = (vec![], vec![]);
        let mut add = |perms: &[Token], i: &[Token], e: &[Token], m: &mut RoleMasks| {
            for (ty, id, mask) in parse_permissions(self.cat, perms) {
                *m.entry((ty, id)).or_default() |= mask;
            }
            inc.extend(parse_names(i));
            exc.extend(parse_names(e));
        };
        for &i in self.sets.get(key).into_iter().flatten() {
            let p = &self.cat.permission_sets[i];
            add(&p.permissions, &p.included, &p.excluded, &mut m);
        }
        for &i in self.exts.get(key).into_iter().flatten() {
            let p = &self.cat.permission_set_exts[i];
            add(&p.permissions, &p.included, &p.excluded, &mut m);
        }
        for n in inc {
            for (k, v) in self.expand(&n, seen) {
                *m.entry(k).or_default() |= v;
            }
        }
        for n in exc {
            for (k, v) in self.expand(&n, &mut HashSet::new()) {
                if let Some(x) = m.get_mut(&k) {
                    *x = exclude(*x, v);
                }
            }
        }
        m
    }

    /// The app (id) that defines permission set `i`: the app of a code object under the same
    /// (longest) app directory, as platform.rs fills "Metadata Permission Set".
    fn set_app(&self, i: usize) -> [u8; 16] {
        let file = &self.cat.permission_sets[i].file;
        let Some(root) = self.cat.app_dirs.iter().filter(|d| file.starts_with(d)).max_by_key(|d| d.as_os_str().len()) else { return [0; 16] };
        let mut memo = self.apps_by_root.lock().unwrap();
        *memo.entry(root.clone()).or_insert_with(|| {
            let mods = crate::modules::modules(self.cat);
            let ci = self.cat.code.iter().position(|co| co.file.starts_with(root));
            ci.and_then(|c| mods.app_of_code(c as u32)).map(|a| a.id).unwrap_or_default()
        })
    }
}

/// `x` without what an excluded set (or an Exclude permission) `v` names: an excluded direct
/// permission removes the operation, an excluded indirect one reduces it to indirect.
fn exclude(x: u16, v: u16) -> u16 {
    let mut r = x;
    for b in [READ, INSERT, MODIFY, DELETE, EXECUTE] {
        if v & b != 0 {
            r &= !(b | b << 5);
        } else if v & (b << 5) != 0 {
            r &= !b;
        }
    }
    r
}

/// (table, mask) of `[InherentPermissions(PermissionObjectType::TableData, Database::"X", 'rm')]`.
fn method_inherent_arg(cat: &Catalog, args: &[al_syntax::Expr]) -> Option<(i64, u16)> {
    use al_syntax::Expr;
    let [Expr::Scope(_, ty), Expr::Scope(_, table), Expr::Str(letters), ..] = args else { return None };
    if !ty.eq_ignore_ascii_case("tabledata") {
        return None;
    }
    Some((cat.id_of(ObjectKind::Table, table)?, mask_of(letters)))
}

/// 0 = not decided yet (BC_PERMISSIONS), 1 = off, 2 = enforce.
static ENFORCE: AtomicU8 = AtomicU8::new(0);

/// Turn permission enforcement on or off for every session of the process.
pub fn set_enforce(on: bool) {
    ENFORCE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
}

/// Whether permissions are enforced: `set_enforce`, else env `BC_PERMISSIONS=enforce` (default off).
#[inline]
pub fn enforce() -> bool {
    match ENFORCE.load(Ordering::Relaxed) {
        0 => {
            let on = std::env::var("BC_PERMISSIONS").is_ok_and(|v| v.eq_ignore_ascii_case("enforce"));
            set_enforce(on);
            on
        }
        v => v == 2,
    }
}

/// The user name of an HTTP `Authorization: Basic <base64(user:password)>` header. The
/// password is not checked: bc-rust has no user authentication yet, so this only names the
/// user whose permissions apply (BC_PERMISSIONS=enforce).
pub fn basic_auth_user(authorization: &str) -> Option<String> {
    use base64::Engine;
    let b = authorization.trim().strip_prefix("Basic ")?.trim();
    let raw = base64::engine::general_purpose::STANDARD.decode(b).ok()?;
    let s = String::from_utf8(raw).ok()?;
    let user = s.split(':').next()?.trim();
    (!user.is_empty()).then(|| user.to_string())
}

/// NavTestPermissions without InheritFromTestCodeunit (resolved here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestPermissions {
    Restrictive,
    NonRestrictive,
    Disabled,
}

impl TestPermissions {
    fn from_word(w: &str) -> Option<TestPermissions> {
        Some(match w.to_ascii_lowercase().as_str() {
            "restrictive" => TestPermissions::Restrictive,
            "nonrestrictive" => TestPermissions::NonRestrictive,
            "disabled" => TestPermissions::Disabled,
            _ => return None,
        })
    }
}

/// The `TestPermissions` of test method `proc` of codeunit `code`: its attribute
/// `[TestPermissions(TestPermissions::X)]` unless that is InheritFromTestCodeunit, else the
/// codeunit's `TestPermissions = X;` property, else Restrictive (NavTestExecution's default).
pub fn test_permissions(cat: &Catalog, code: u32, proc: u32) -> TestPermissions {
    let co = &cat.code[code as usize];
    if let Some(p) = co.procs.get(proc as usize) {
        for a in p.attributes.iter().filter(|a| a.name.eq_ignore_ascii_case("testpermissions")) {
            let s = format!("{:?}", a.args).to_ascii_lowercase();
            for w in ["nonrestrictive", "restrictive", "disabled"] {
                if s.contains(&format!("\"{w}\"")) {
                    return TestPermissions::from_word(w).unwrap();
                }
            }
        }
    }
    codeunit_test_permissions(&co.file, co.id).unwrap_or(TestPermissions::Restrictive)
}

/// The `TestPermissions` property of codeunit `id` in `file` (cached per file).
fn codeunit_test_permissions(file: &Arc<PathBuf>, id: i64) -> Option<TestPermissions> {
    static CACHE: OnceLock<Mutex<HashMap<(PathBuf, i64), Option<TestPermissions>>>> = OnceLock::new();
    let key = ((**file).clone(), id);
    if let Some(v) = CACHE.get_or_init(Default::default).lock().unwrap().get(&key) {
        return *v;
    }
    let v = std::fs::read_to_string(&**file).ok().and_then(|src| scan_test_permissions(&src, id));
    CACHE.get_or_init(Default::default).lock().unwrap().insert(key, v);
    v
}

/// The property lines between `codeunit <id>` and its first member.
fn scan_test_permissions(src: &str, id: i64) -> Option<TestPermissions> {
    let mut inside = false;
    for line in src.lines() {
        let lower = line.trim_start_matches('\u{feff}').trim().to_ascii_lowercase();
        if lower.starts_with("codeunit ") {
            inside = lower.split_whitespace().nth(1).and_then(|n| n.parse::<i64>().ok()) == Some(id);
            continue;
        }
        if !inside {
            continue;
        }
        if lower.starts_with("procedure ") || lower.starts_with("local procedure") || lower.starts_with("internal procedure") || lower.starts_with("trigger ") || lower.starts_with('[') || lower == "var" {
            return None;
        }
        if let Some(rest) = lower.strip_prefix("testpermissions") {
            let v = rest.trim_start().strip_prefix('=')?.trim().trim_end_matches(';').trim();
            return TestPermissions::from_word(v);
        }
    }
    None
}

/// Permission state of a session (crate::sysfuncs::SysState::perm).
#[derive(Default)]
pub struct PermState {
    /// The test's effective permission sets (role ids), as `PermissionTestHelper`
    /// AddEffectivePermissionSet added them since its last Clear/Dispose. Empty: not in use (BC's
    /// EffectivePermissionsInUse); the session user's own permissions (SUPER) apply.
    pub effective: Vec<String>,
    /// Checks run: enforcement is on, and the effective sets are in use or the session user is
    /// not SUPER (`Session::login_permissions`).
    pub active: bool,
    /// The session user's roles (Access Control), once `login_permissions` ran: None = not
    /// loaded (every session is SUPER then), Some(None) = SUPER, Some(Some(masks)) = the union
    /// of the user's permission sets.
    user: Option<Option<RoleMasks>>,
    /// Union of the effective sets' expanded permissions (computed on first use).
    roles: Option<RoleMasks>,
    /// Allowed direct operations by (calling code object, table).
    cache: HashMap<(u32, i64), u16>,
    /// Writes in progress whose check waits for the database operation, innermost last:
    /// (record handle address, or 0 for any record of the table; table; calling code object; op).
    writes: Vec<(usize, i64, u32, u16)>,
    /// The next record_method call is the deferred write's own run (its hook check is skipped).
    skip_next: bool,
}

impl std::fmt::Debug for PermState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermState").field("effective", &self.effective).field("active", &self.active).finish()
    }
}

impl PermState {
    fn changed(&mut self) {
        self.roles = None;
        self.cache.clear();
        self.active = enforce() && (!self.effective.is_empty() || matches!(self.user, Some(Some(_))));
    }

    /// PermissionTestHelper.Clear / Dispose / constructor.
    pub fn clear_effective(&mut self) {
        self.effective.clear();
        self.changed();
    }

    /// PermissionTestHelper.AddEffectivePermissionSet(RoleID).
    pub fn add_effective(&mut self, role: &str) {
        if !self.effective.iter().any(|r| r.eq_ignore_ascii_case(role)) {
            self.effective.push(role.to_string());
        }
        self.changed();
    }

    /// BC's EffectivePermissionsInUse (while a test runs).
    pub fn effective_in_use(&self) -> bool {
        !self.effective.is_empty()
    }
}

/// Virtual system tables without permission checks (Types SystemTables
/// .BuildVirtualTableWithoutPermissionChecksList, plus Permission Set / Permission with
/// UsePermissionSetsFromExtensions and Active Session with TrackSessionsInMemory, both on by
/// default). Every other system table is checked like an application table; the virtual tables
/// that BC checks anyway (2000000247, 2000000260-264, 2000000274-278, 2000000282, 2000000299) are
/// simply not in this list.
const VIRTUAL_UNCHECKED: &[i64] = &[
    2000000007, 2000000026, 2000000049, 2000000101, 2000000102, 2000000103, 2000000020, 2000000055, 2000000009, 2000000029, 2000000038, 2000000058,
    2000000041, 2000000028, 2000000063, 2000000022, 2000000048, 2000000040, 2000000043, 2000000044, 2000000251, 2000000250, 2000000252, 2000000039,
    2000000045, 2000000050, 2000000135, 2000000136, 2000000137, 2000000138, 2000000139, 2000000280, 2000000140, 2000000164, 2000000141, 2000000254,
    2000000167, 2000000171, 2000000172, 2000000154, 2000000235, 2000000236, 2000000193, 2000000097, 2000000178, 2000000085, 2000000246, 2000000192,
    2000000142, 2000000143, 2000000157, 2000000202, 2000000203, 2000000204, 2000000211, 2000000223, 2000000225, 2000000227, 2000000229, 2000000234,
    2000000243, 2000000244, 2000000288, 2000000289, 2000000300, 2000000174, 2000000004, 2000000005, 2000000110,
];

/// The direct operation a record method needs, and whether an empty table excuses it.
fn record_op(lname: &str) -> Option<(u16, bool)> {
    Some(match lname {
        "get" | "find" | "findfirst" | "findlast" | "findset" | "isempty" | "count" | "countapprox" | "calcsums" => (READ, true),
        "insert" => (INSERT, false),
        "modify" | "rename" => (MODIFY, false),
        "modifyall" => (MODIFY, true),
        "delete" => (DELETE, false),
        "deleteall" => (DELETE, true),
        _ => return None,
    })
}

fn op_name(op: u16) -> &'static str {
    match op {
        READ => "Read",
        INSERT => "Insert",
        MODIFY => "Modify",
        DELETE => "Delete",
        _ => "Execute",
    }
}

impl<'r> Session<'r> {
    /// The direct operations allowed on `table` for code of code object `code`.
    fn allowed_ops(&mut self, code: u32, table: i64) -> u16 {
        if let Some(&a) = self.sys.perm.cache.get(&(code, table)) {
            return a;
        }
        let m = model(self.cat);
        if self.sys.perm.roles.is_none() {
            let mut all = RoleMasks::new();
            if self.sys.perm.effective.is_empty() {
                // the session user's roles (login_permissions)
                if let Some(Some(u)) = &self.sys.perm.user {
                    all = u.clone();
                }
            } else {
                // a test's effective sets: each set id's System set, plus every tenant set
                // (PermissionManager.GetRolePermissions adds all Tenant Permission Sets in tests)
                for r in self.sys.perm.effective.clone() {
                    for (k, v) in m.role(&r).iter() {
                        *all.entry(*k).or_default() |= *v;
                    }
                }
                for (_, _, t) in self.tenant_roles() {
                    for (k, v) in t {
                        *all.entry(k).or_default() |= v;
                    }
                }
            }
            self.sys.perm.roles = Some(all);
        }
        let roles = self.sys.perm.roles.as_ref().unwrap();
        let rm = roles.get(&(TABLE_DATA, table)).copied().unwrap_or(0) | roles.get(&(TABLE_DATA, 0)).copied().unwrap_or(0);
        let direct = rm & DIRECT;
        let any = (rm | rm >> 5) & DIRECT;
        let co = self.cat.code.get(code as usize);
        let base = co.map(|c| c.extends.unwrap_or((c.kind, c.id)));
        let ind = base.and_then(|b| m.indirect.get(&b)).and_then(|t| t.get(&table)).copied().unwrap_or(0);
        let inh = m.inherent.get(&table).copied().unwrap_or(0);
        let a = direct | (ind & any) | inh;
        self.sys.perm.cache.insert((code, table), a);
        a
    }

    /// Operations the running procedure's `[InherentPermissions]` grant on `table`
    /// (PermissionSet.VerifyInherentPermissions): upper case directly, lower case when the
    /// object's `Permissions` grant them too. Only asked after a denial.
    fn method_inherent_ops(&self, code: u32, table: i64) -> u16 {
        let Some(&proc) = self.proc_stack.last() else { return 0 };
        let m = model(self.cat);
        let Some(grants) = m.method_inherent.get(&(code, proc)) else { return 0 };
        let co = &self.cat.code[code as usize];
        let base = co.extends.unwrap_or((co.kind, co.id));
        let ind = m.indirect.get(&base).and_then(|t| t.get(&table)).copied().unwrap_or(0);
        grants.iter().filter(|(t, _)| *t == table).fold(0, |a, (_, mask)| a | (mask & DIRECT) | ((mask >> 5) & ind))
    }

    /// The table of a record that needs checks (RecordImplementation.IsPermissionCheckNeeded):
    /// not temporary, not a virtual table without permission checks, not query-backed.
    fn perm_checked(h: &RecHandle) -> Option<i64> {
        let b = h.borrow();
        let t = b.tdef;
        (b.temp.is_none() && !t.table_type.eq_ignore_ascii_case("temporary") && !t.table_type.eq_ignore_ascii_case("query") && !VIRTUAL_UNCHECKED.contains(&t.id)).then_some(t.id)
    }

    /// The permission check of record method `lname` called from code object `code`
    /// (RecordImplementation.VerifyPermissions), when `sys.perm.active`.
    pub fn check_record_op(&mut self, code: u32, h: &RecHandle, lname: &str) -> R<()> {
        let Some((op, empty_ok)) = record_op(lname) else { return Ok(()) };
        let Some(table) = Self::perm_checked(h) else { return Ok(()) };
        let company = h.borrow().company.clone();
        self.check_table(code, table, op, empty_ok, company)
    }

    /// The permission check of record method `name` at the top of `record_method`, when
    /// `sys.perm.active`. Writes are checked before they run. A read that the roles don't allow
    /// runs unchecked first, because BC checks reads after the query (RecordImplementation):
    /// Get when it found the row, IsEmpty when there are rows, Find* when it found one or (nothing
    /// found) when filters are set, Count when the count is above 0 or filters are set; the
    /// empty-table rule applies on top. Some(v): the read's result (the caller returns it).
    pub fn check_record_method(&mut self, f: &mut Frame, h: &RecHandle, name: &str, lname: &str, args: &[al_syntax::Expr], discard: bool) -> R<Option<Val>> {
        if std::mem::take(&mut self.sys.perm.skip_next) {
            return Ok(None);
        }
        let Some((op, empty_ok)) = record_op(lname) else { return Ok(None) };
        let Some(table) = Self::perm_checked(h) else { return Ok(None) };
        if op != READ {
            // allowed writes need no deferral and no bulk-path decision
            if self.allowed_ops(f.code, table) & op != 0 || self.method_inherent_ops(f.code, table) & op != 0 {
                return Ok(None);
            }
            let key = match lname {
                // BC checks at the database operation, after OnInsert/OnModify/OnDelete and the
                // OnBefore events, so an error those raise comes first (RecordImplementation)
                "insert" | "modify" | "delete" => Some(Rc::as_ptr(h) as *const () as usize),
                // the bulk statement is checked up front (empty-table rule); the record-by-record
                // loop BC uses when triggers or subscribers must run checks each record it touches
                "modifyall" | "deleteall" if !self.bulk_path(h, lname, args) => Some(0),
                _ => None,
            };
            let Some(key) = key else {
                let company = h.borrow().company.clone();
                self.check_table(f.code, table, op, empty_ok, company)?;
                return Ok(None);
            };
            self.sys.perm.writes.push((key, table, f.code, op));
            self.discard = discard;
            self.sys.perm.skip_next = true;
            let res = self.record_method(f, h, name, args);
            self.sys.perm.skip_next = false;
            self.sys.perm.writes.pop();
            return res.map(Some);
        }
        if (op == READ && matches!(table, 2_000_000_001 | 2_000_000_053 | 2_000_000_071)) || self.allowed_ops(f.code, table) & op != 0 || self.method_inherent_ops(f.code, table) & op != 0 {
            return Ok(None);
        }
        // run the read without checks (the hook sees `active` off), then apply BC's rule
        self.discard = discard;
        self.sys.perm.active = false;
        let res = self.record_method(f, h, name, args);
        self.sys.perm.active = true;
        let filtered = {
            let b = h.borrow();
            !b.filters.is_empty() || b.mark_only
        };
        let must_check = match (lname, &res) {
            ("get", Ok(Val::Bool(found))) => *found,
            ("get", _) => false,
            ("isempty", Ok(Val::Bool(empty))) => !*empty,
            ("count" | "countapprox", Ok(v)) => v.as_i64() > 0 || filtered,
            ("find" | "findfirst" | "findlast" | "findset", Ok(Val::Bool(found))) => *found || filtered,
            // Find* in statement form that found nothing: the permission error comes first when filtered
            ("find" | "findfirst" | "findlast" | "findset", Err(_)) => filtered,
            (_, Ok(_)) => true,
            (_, Err(_)) => false,
        };
        if must_check {
            let company = h.borrow().company.clone();
            self.check_table(f.code, table, READ, true, company)?;
        }
        res.map(Some)
    }

    /// The permission check of operation `op` (READ, INSERT, MODIFY, DELETE) on `table` for code of
    /// code object `code`, for platform callers that read or write rows themselves (the API
    /// pages' data provider). Nothing happens unless `sys.perm.active`.
    pub fn check_table_op(&mut self, code: u32, table: i64, op: u16) -> R<()> {
        if !self.sys.perm.active || VIRTUAL_UNCHECKED.contains(&table) {
            return Ok(());
        }
        self.check_table(code, table, op, op == READ, None)
    }

    fn check_table(&mut self, code: u32, table: i64, op: u16, empty_ok: bool, company: Option<String>) -> R<()> {
        // Object, Access Control and Object Metadata are always readable
        if op == READ && matches!(table, 2_000_000_001 | 2_000_000_053 | 2_000_000_071) {
            return Ok(());
        }
        if self.allowed_ops(code, table) & op != 0 || self.method_inherent_ops(code, table) & op != 0 {
            return Ok(());
        }
        if empty_ok {
            let probe = self.new_record(table, false);
            probe.borrow_mut().company = company;
            if self.fetch(&probe, false, None, 1)?.is_empty() {
                return Ok(());
            }
        }
        let caption = self.cat.tables.get(&table).map(|t| t.caption.clone()).unwrap_or_default();
        let app = crate::modules::modules(self.cat).app_of_code(code).map(|a| a.name.clone()).unwrap_or_default();
        Err(Flow::Err(Box::new(AlError {
            msg: format!("Sorry, the current permissions prevented the action. (TableData {table} {caption} {}: {app})", op_name(op)),
            stack: vec![],
            code: "Permission",
        })))
    }

    /// Load the session user's permissions from Access Control (BC: NavUserPermissions.GetRoles)
    /// when enforcement is on: the user (User."User Name" = user_id) gets the union of their
    /// permission sets for this company (Company Name '' or the session company); SUPER (System
    /// scope) means no checks. Call after the session's user and company are set.
    pub fn login_permissions(&mut self) {
        if !enforce() {
            return;
        }
        let cat = self.cat;
        let (Some(user_t), Some(ac)) = (cat.table_by_name("User"), cat.table_by_name("Access Control")) else { return };
        let get = |t: &al_model::TableDef, r: &bc_store::Row, n: &str| t.field(n).and_then(|i| r.get(i)).cloned();
        let me = self.user_id.clone();
        let users = self.perm_scan(user_t.id);
        if users.is_empty() {
            // no users: the permission system is off (NavDatabaseSecurityAndLicense
            // .IsPermissionSystemEnabled), everyone has all permissions
            self.sys.perm.user = Some(None);
            self.sys.perm.changed();
            return;
        }
        let sid = users.into_iter().find(|r| text_of(get(user_t, r, "User Name").as_ref()).eq_ignore_ascii_case(&me)).map(|r| guid_of(get(user_t, &r, "User Security ID").as_ref()));
        let company = self.company.clone();
        let mut rows = vec![];
        if let Some(sid) = sid {
            for r in self.perm_scan(ac.id) {
                let c = text_of(get(ac, &r, "Company Name").as_ref());
                if guid_of(get(ac, &r, "User Security ID").as_ref()) == sid && (c.is_empty() || c.eq_ignore_ascii_case(&company)) {
                    rows.push((text_of(get(ac, &r, "Role ID").as_ref()), guid_of(get(ac, &r, "App ID").as_ref()), opt_of(get(ac, &r, "Scope").as_ref()) == 1));
                }
            }
        }
        let user = if rows.iter().any(|(role, _, tenant)| !tenant && role.eq_ignore_ascii_case("SUPER")) {
            None
        } else {
            let m = model(cat);
            let tenant_sets = if rows.iter().any(|r| r.2) { self.tenant_roles() } else { vec![] };
            let mut all = RoleMasks::new();
            for (role, app, tenant) in &rows {
                let masks: Vec<RoleMasks> = if *tenant {
                    tenant_sets.iter().filter(|((a, r), _, _)| a == app && r.eq_ignore_ascii_case(role)).map(|(_, _, t)| t.clone()).collect()
                } else {
                    vec![(*m.role(role)).clone()]
                };
                for t in masks {
                    for (k, v) in t {
                        *all.entry(k).or_default() |= v;
                    }
                }
            }
            Some(all)
        };
        self.sys.perm.user = Some(user);
        self.sys.perm.changed();
    }

    /// Whether BC runs ModifyAll / DeleteAll as one statement (NavRecord.CanUseBulkModifyAll /
    /// CanUseBulkDeleteAll): not when RunTrigger is set and the table has the trigger, not when
    /// the table's OnBefore/OnAfter Modify/Delete events have subscribers, and not when the table
    /// has a global database trigger (`IsGlobalTriggerImplemented(DatabaseModify/DatabaseDelete)`,
    /// see `db_trigger_setup`). (BC also loops for audited tables and media fields, which bc-rust
    /// doesn't have.) A RunTrigger argument that isn't a literal counts as true.
    fn bulk_path(&mut self, h: &RecHandle, lname: &str, args: &[al_syntax::Expr]) -> bool {
        let tdef = h.borrow().tdef;
        let (rt_arg, trigger, events) = if lname == "deleteall" {
            (args.first(), tdef.triggers.on_delete.is_some(), ["onbeforedeleteevent", "onafterdeleteevent"])
        } else {
            (if args.len() % 2 == 1 { args.last() } else { None }, tdef.triggers.on_modify.is_some(), ["onbeforemodifyevent", "onaftermodifyevent"])
        };
        let run_trigger = match rt_arg {
            None => false,
            Some(al_syntax::Expr::Bool(b)) => *b,
            Some(_) => true,
        };
        if run_trigger && trigger {
            return false;
        }
        if events.iter().any(|e| self.cat.subscribers.contains_key(&(ObjectKind::Table, tdef.id, e.to_string(), String::new()))) {
            return false;
        }
        let (modify, delete) = self.db_trigger_setup(tdef.id);
        !(if lname == "deleteall" { delete } else { modify })
    }

    /// Global Triggers.GetDatabaseTableTriggerSetup for `table`: (OnDatabaseModify, OnDatabaseDelete).
    /// BC loops record by record when they are set; the Base App sets them for Change Log, API
    /// webhook and Dataverse tables, and the test libraries' Backup Management (codeunit 130011,
    /// OnAfterGetDatabaseTableTriggerSetup) sets them for every table 1..99999 of a company, so in
    /// Microsoft's tests ModifyAll/DeleteAll on application tables never take the bulk path. The
    /// subscribers run without permission checks: BC asks for its trigger cache, not as the user.
    /// A failing subscriber counts as no trigger. bc-rust doesn't raise the OnDatabase* triggers
    /// themselves yet (docs/platform-tables.md); only this decision uses the setup.
    fn db_trigger_setup(&mut self, table: i64) -> (bool, bool) {
        const GLOBAL_TRIGGERS: i64 = 2_000_000_002;
        let flags: Vec<_> = (0..4).map(|_| crate::value::slot(Val::Bool(false))).collect();
        let args = vec![
            ("TableId", Arg::Val(Val::Int(table as i32))),
            ("OnDatabaseInsert", Arg::Ref(LVal::Slot(flags[0].clone(), None))),
            ("OnDatabaseModify", Arg::Ref(LVal::Slot(flags[1].clone(), None))),
            ("OnDatabaseDelete", Arg::Ref(LVal::Slot(flags[2].clone(), None))),
            ("OnDatabaseRename", Arg::Ref(LVal::Slot(flags[3].clone(), None))),
        ];
        let active = std::mem::replace(&mut self.sys.perm.active, false);
        let res = self.raise_event(ObjectKind::Codeunit, GLOBAL_TRIGGERS, "GetDatabaseTableTriggerSetup", "", args, None, None);
        self.sys.perm.active = active;
        if res.is_err() {
            return (false, false);
        }
        let b = |i: usize| flags[i].borrow().as_bool();
        (b(1), b(2))
    }

    /// The deferred check of a write in progress, right before its database operation
    /// (recops rec_insert_x / rec_modify_x / rec_delete_x): the write `check_record_method`
    /// deferred for this record, or any record of its table for a ModifyAll/DeleteAll loop.
    pub fn check_pending_write(&mut self, h: &RecHandle) -> R<()> {
        let Some(&(key, table, code, op)) = self.sys.perm.writes.last() else { return Ok(()) };
        let me = Rc::as_ptr(h) as *const () as usize;
        let tid = h.borrow().tdef.id;
        if key != me && !(key == 0 && table == tid) {
            return Ok(());
        }
        let company = h.borrow().company.clone();
        self.check_table(code, table, op, false, company)
    }

    /// Record.ReadPermission / WritePermission (Insert, Modify and Delete).
    pub fn record_permission(&mut self, code: u32, h: &RecHandle, write: bool) -> bool {
        if !self.sys.perm.active {
            return true;
        }
        let Some(table) = Self::perm_checked(h) else { return true };
        let need = if write { INSERT | MODIFY | DELETE } else { READ };
        (self.allowed_ops(code, table) | self.method_inherent_ops(code, table)) & need == need
    }
}

/// "Metadata Permission", "Metadata Permission Set Rel." and the System-scope rows of "Expanded
/// Permission", from the catalog (virt::catalog_table; Ncl MetadataPermissionDataProvider,
/// MetadataPermissionSetRelationDataProvider, ExpandedPermissionDataProvider).
pub fn catalog_rows(cat: &'static Catalog, tdef: &al_model::TableDef, schema: &bc_store::TableSchema, put: &mut dyn FnMut(bc_store::Row)) {
    let m = model(cat);
    let pb = PermRow { tdef, schema };
    for (i, p) in cat.permission_sets.iter().enumerate() {
        if p.name.chars().count() > 30 {
            continue;
        }
        let app = m.set_app(i);
        match tdef.id {
            METADATA_PERMISSION => {
                let mut own = RoleMasks::new();
                for (ty, id, mask) in parse_permissions(cat, &p.permissions) {
                    *own.entry((ty, id)).or_default() |= mask;
                }
                let mut own: Vec<_> = own.into_iter().collect();
                own.sort();
                for (k, v) in own {
                    put(pb.row(cat, app, &p.name, &p.caption, k, v, 0));
                }
            }
            METADATA_PERMISSION_SET_REL => {
                for (names, ty) in [(&p.included, 0), (&p.excluded, 1)] {
                    for n in parse_names(names) {
                        let Some(&j) = m.sets.get(&n).and_then(|v| v.first()) else { continue };
                        let mut r = schema.new_row();
                        pb.put(&mut r, "App ID", Val::Guid(app));
                        pb.put(&mut r, "Role ID", Val::text(&p.name));
                        pb.put(&mut r, "Related App ID", Val::Guid(m.set_app(j)));
                        pb.put(&mut r, "Related Role ID", Val::text(&cat.permission_sets[j].name));
                        pb.opt(&mut r, "Type", ty);
                        put(r);
                    }
                }
            }
            _ => {
                for (k, v) in with_wildcards(&m.role(&p.name)) {
                    put(pb.row(cat, app, &p.name, &p.caption, k, v, 0));
                }
            }
        }
    }
}

/// The entries of a role, each wildcard (object id 0) merged into the specific entries of its
/// object type (ExpandedPermission: AsEnumerableWithWildcardUnion).
fn with_wildcards(r: &RoleMasks) -> Vec<((u8, i64), u16)> {
    let mut v: Vec<_> = r.iter().map(|(k, m)| (*k, *m | if k.1 != 0 { r.get(&(k.0, 0)).copied().unwrap_or(0) } else { 0 })).collect();
    v.sort();
    v
}

struct PermRow<'a> {
    tdef: &'a al_model::TableDef,
    schema: &'a bc_store::TableSchema,
}

impl PermRow<'_> {
    fn put(&self, r: &mut bc_store::Row, field: &str, v: Val) {
        if let Some(i) = self.tdef.field(field) {
            let fd = &self.tdef.fields[i];
            let v = match (&fd.ty, v) {
                (al_model::FieldType::Text(n) | al_model::FieldType::Code(n), Val::Text(s)) => Val::Text(s.chars().take(*n as usize).collect()),
                (_, v) => v,
            };
            if let Ok(sv) = crate::record::to_store(fd, &v) {
                r[i] = sv;
            }
        }
    }
    fn opt(&self, r: &mut bc_store::Row, field: &str, ord: i32) {
        if let Some(i) = self.tdef.field(field) {
            r[i] = bc_store::Value::Option(ord);
        }
    }
    /// A permission row: App ID, Role ID, Role Name, Object Type, Object ID, Object Name,
    /// Read..Execute as ' ' / Yes / Indirect, AL Object Name, Scope (fields a table lacks are skipped).
    #[allow(clippy::too_many_arguments)]
    fn row(&self, cat: &Catalog, app: [u8; 16], role: &str, name: &str, (ty, id): (u8, i64), mask: u16, scope: i32) -> bc_store::Row {
        let mut r = self.schema.new_row();
        self.put(&mut r, "App ID", Val::Guid(app));
        self.put(&mut r, "Role ID", Val::text(role));
        self.put(&mut r, "Role Name", Val::text(name));
        self.opt(&mut r, "Object Type", ty as i32);
        self.put(&mut r, "Object ID", Val::Int(id as i32));
        let kind = match ty {
            TABLE_DATA | TABLE => Some(ObjectKind::Table),
            3 => Some(ObjectKind::Report),
            5 => Some(ObjectKind::Codeunit),
            6 => Some(ObjectKind::XmlPort),
            8 => Some(ObjectKind::Page),
            9 => Some(ObjectKind::Query),
            _ => None,
        };
        if let Some(n) = kind.filter(|_| id != 0).and_then(|k| cat.code_by_key.get(&(k, id))).map(|&c| cat.code[c as usize].name.clone()) {
            self.put(&mut r, "Object Name", Val::text(&n));
            self.put(&mut r, "AL Object Name", Val::text(&n));
        }
        for (f, b) in [("Read Permission", READ), ("Insert Permission", INSERT), ("Modify Permission", MODIFY), ("Delete Permission", DELETE), ("Execute Permission", EXECUTE)] {
            self.opt(&mut r, f, if mask & b != 0 { 1 } else if mask & (b << 5) != 0 { 2 } else { 0 });
        }
        self.opt(&mut r, "Scope", scope);
        r
    }
}

/// Mask bits for operation `b` from a permission option (' ' / Yes / Indirect).
fn mask_of_option(v: Option<&bc_store::Value>, b: u16) -> u16 {
    match v {
        Some(bc_store::Value::Option(1)) => b | b << 5,
        Some(bc_store::Value::Option(2)) => b << 5,
        _ => 0,
    }
}

fn text_of(v: Option<&bc_store::Value>) -> String {
    match v {
        Some(bc_store::Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

fn guid_of(v: Option<&bc_store::Value>) -> [u8; 16] {
    match v {
        Some(bc_store::Value::Guid(g)) => *g,
        _ => [0; 16],
    }
}

fn opt_of(v: Option<&bc_store::Value>) -> i32 {
    match v {
        Some(bc_store::Value::Option(o)) => *o,
        Some(bc_store::Value::Int(o)) => *o,
        _ => 0,
    }
}

/// A tenant permission set: (App ID, upper-case Role ID).
type SetKey = ([u8; 16], String);

/// Own permissions of tenant sets (Include, Exclude) and their relations (related set, related
/// scope is Tenant, Exclude).
struct TenantGraph {
    own: HashMap<SetKey, (RoleMasks, RoleMasks)>,
    related: HashMap<SetKey, Vec<(SetKey, bool, bool)>>,
}

impl TenantGraph {
    fn walk(&self, k: &SetKey, m: &Model, seen: &mut HashSet<SetKey>) -> RoleMasks {
        let mut out = RoleMasks::new();
        if !seen.insert(k.clone()) {
            return out;
        }
        let mut excluded = vec![];
        if let Some((inc, exc)) = self.own.get(k) {
            for (o, v) in inc {
                *out.entry(*o).or_default() |= *v;
            }
            excluded.push(exc.clone());
        }
        for (rk, tenant, excl) in self.related.get(k).into_iter().flatten() {
            let sub = if *tenant { self.walk(rk, m, seen) } else { (*m.role(&rk.1)).clone() };
            if *excl {
                excluded.push(sub);
            } else {
                for (o, v) in sub {
                    *out.entry(o).or_default() |= v;
                }
            }
        }
        for e in excluded {
            for (o, v) in e {
                if let Some(x) = out.get_mut(&o) {
                    *x = exclude(*x, v);
                }
            }
        }
        out
    }
}

impl<'r> Session<'r> {
    /// Every row of a stored table, unfiltered.
    fn perm_scan(&mut self, table: i64) -> Vec<bc_store::Row> {
        let Some(schema) = self.rt.schemas.get(&table).cloned() else { return vec![] };
        let req = bc_store::ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: usize::MAX, lock: false };
        let company = self.company.clone();
        self.db.scan(&company, &schema, &req).unwrap_or_default().into_iter().map(|r| r.to_vec()).collect()
    }

    /// The tenant permission sets with their expanded permissions: (set, Name, permissions).
    /// Own permissions from "Tenant Permission", included and excluded sets from "Tenant
    /// Permission Set Rel." (System sets from the model).
    pub fn tenant_roles(&mut self) -> Vec<(SetKey, String, RoleMasks)> {
        let cat = self.cat;
        let (Some(tps), Some(tp), Some(rel)) = (cat.table_by_name("Tenant Permission Set"), cat.table_by_name("Tenant Permission"), cat.table_by_name("Tenant Permission Set Rel.")) else { return vec![] };
        let sets = self.perm_scan(tps.id);
        if sets.is_empty() {
            return vec![];
        }
        let (perms, rels) = (self.perm_scan(tp.id), self.perm_scan(rel.id));
        let get = |t: &al_model::TableDef, r: &'_ bc_store::Row, n: &str| t.field(n).and_then(|i| r.get(i)).cloned();
        let key = |t: &al_model::TableDef, r: &bc_store::Row, a: &str, b: &str| -> SetKey { (guid_of(get(t, r, a).as_ref()), text_of(get(t, r, b).as_ref()).to_uppercase()) };
        let mut g = TenantGraph { own: HashMap::new(), related: HashMap::new() };
        for r in &perms {
            let ty = opt_of(get(tp, r, "Object Type").as_ref()) as u8;
            let id = opt_of(get(tp, r, "Object ID").as_ref()) as i64;
            let mut mask = 0;
            for (f, b) in [("Read Permission", READ), ("Insert Permission", INSERT), ("Modify Permission", MODIFY), ("Delete Permission", DELETE), ("Execute Permission", EXECUTE)] {
                mask |= mask_of_option(get(tp, r, f).as_ref(), b);
            }
            let e = g.own.entry(key(tp, r, "App ID", "Role ID")).or_default();
            let side = if opt_of(get(tp, r, "Type").as_ref()) == 1 { &mut e.1 } else { &mut e.0 };
            *side.entry((ty, id)).or_default() |= mask;
        }
        for r in &rels {
            let tenant = opt_of(get(rel, r, "Related Scope").as_ref()) == 1;
            let excl = opt_of(get(rel, r, "Type").as_ref()) == 1;
            g.related.entry(key(rel, r, "App ID", "Role ID")).or_default().push((key(rel, r, "Related App ID", "Related Role ID"), tenant, excl));
        }
        let m = model(cat);
        sets.iter()
            .map(|r| {
                let k = key(tps, r, "App ID", "Role ID");
                let masks = g.walk(&k, m, &mut HashSet::new());
                (k, text_of(get(tps, r, "Name").as_ref()), masks)
            })
            .collect()
    }

    /// Expanded Permission as read: the catalog's System rows plus the expansion of the tenant
    /// permission sets (Scope Tenant).
    pub fn expanded_permission_rows(&mut self, base: Arc<bc_store::TableData>) -> Arc<bc_store::TableData> {
        let tenant = self.tenant_roles();
        if tenant.is_empty() {
            return base;
        }
        let Some(tdef) = self.cat.tables.get(&EXPANDED_PERMISSION) else { return base };
        let mut t = (*base).clone();
        let schema = t.schema.clone();
        let pb = PermRow { tdef, schema: &schema };
        for ((app, role), name, masks) in tenant {
            for (k, v) in with_wildcards(&masks) {
                let _ = t.insert(pb.row(self.cat, app, &role, &name, k, v, 1));
            }
        }
        Arc::new(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codeunit_property() {
        let src = "\u{feff}codeunit 134000 \"ERM X\"\n{\n    Subtype = Test;\n    TestPermissions = NonRestrictive;\n\n    var\n        X: Integer;\n}\n";
        assert_eq!(scan_test_permissions(src, 134000), Some(TestPermissions::NonRestrictive));
        assert_eq!(scan_test_permissions(src, 134001), None);
        let none = "codeunit 1 A\n{\n    Subtype = Test;\n\n    [Test]\n    procedure T()\n    begin\n    end;\n}\n";
        assert_eq!(scan_test_permissions(none, 1), None);
    }

    #[test]
    fn basic_auth() {
        assert_eq!(basic_auth_user("Basic Q0xFUks6eA=="), Some("CLERK".into()));
        assert_eq!(basic_auth_user("Bearer abc"), None);
        assert_eq!(basic_auth_user("Basic !!"), None);
    }

    #[test]
    fn masks() {
        assert_eq!(mask_of("R"), READ | READ << 5);
        assert_eq!(mask_of("rimd"), (READ | INSERT | MODIFY | DELETE) << 5);
        assert_eq!(mask_of("Rimd") & DIRECT, READ);
        assert_eq!(mask_of("X"), EXECUTE | EXECUTE << 5);
    }

    #[test]
    fn names_and_entries() {
        let toks = al_syntax::lex("\"D365 BUS PREMIUM\", \"System App - Admin\", SECURITY", &Default::default()).unwrap();
        assert_eq!(parse_names(&toks), vec!["d365 bus premium", "system app - admin", "security"]);
        let cat = Catalog::default();
        let toks = al_syntax::lex("tabledata 18 = RIMD, table * = X, codeunit 80 = x", &Default::default()).unwrap();
        assert_eq!(parse_permissions(&cat, &toks), vec![(TABLE_DATA, 18, mask_of("RIMD")), (TABLE, 0, mask_of("X")), (5, 80, mask_of("x"))]);
    }

    #[test]
    fn effective_sets() {
        let mut p = PermState::default();
        assert!(!p.effective_in_use());
        p.add_effective("D365 BASIC");
        p.add_effective("d365 basic");
        assert_eq!(p.effective.len(), 1);
        p.clear_effective();
        assert!(!p.effective_in_use() && !p.active);
    }
}
