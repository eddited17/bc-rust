//! Global system functions whose BC behaviour depends on the deployment. bc-rust is a single
//! service instance without Windows accounts, database encryption or external table
//! connections, so these follow what BC does in that setup (Ncl ALDatabase, ALSession,
//! ALSystemEncryption, TableConnectionManager):
//!
//! - `Sid([UserName])`: the Windows security id; '' as no name maps to a Windows account
//!   (BC returns '' on IdentityNotMappedException) and session users are not Windows users.
//! - `RegisterTableConnection` / `UnregisterTableConnection` / `HasTableConnection` /
//!   `SetDefaultTableConnection` / `GetDefaultTableConnection`: a per-session registry of names;
//!   unregistering an unknown connection is not an error, and clears the default if it was it.
//! - Test table connections (Ncl CrmTableConnection): a CRM connection registered with the
//!   connection string `@@test@@` (or containing `Url=@@test@@`) while a test runs keeps the rows
//!   of every CRM/CDS table (TableType CRM/CDS) in memory, one table per table id, owned by the
//!   connection: not transactional (rollback, asserterror and test isolation keep them) and gone
//!   when the connection is unregistered. Insert assigns a new Guid to the first primary-key
//!   field when it is empty, as Dataverse does (CrmTestDataProvider.InsertAsync).
//! - Errors as TableConnectionManager raises them: registering a name that is registered already,
//!   and reading or writing a CRM/CDS table while its current connection is not registered.
//! - `EncryptionEnabled()` / `EncryptionKeyExists()` and the other encryption-key functions:
//!   crate::enckey (the key is kept in [`SysState`]).
//! - `IsSessionActive(Id)`: true only for the current session; StartSession runs its codeunit
//!   inline, so a started session has ended by the time its id is returned.

use crate::interp::*;
use crate::value::*;
use al_syntax::Expr;
use std::cell::RefCell;
use std::rc::Rc;

/// Session state of the system functions.
#[derive(Default)]
pub struct SysState {
    /// Registered table connections.
    conns: Vec<Conn>,
    /// Default table connection per TableConnectionType ordinal.
    defaults: Vec<(i64, String)>,
    /// Notifications sent and not recalled outside tests (crate::platobj), for a client to show.
    pub notifications: Vec<crate::platobj::Notif>,
    /// Collectible errors of ErrorBehavior::Collect scopes (crate::errinfo).
    pub errors: crate::errinfo::Collection,
    /// The session's application area, as ApplicationArea(Text) set it ('#Basic,#Suite', '' = all
    /// controls). None: never set (live sessions without the login code), ApplicationArea()
    /// reports every area and TestPages show every control.
    pub app_area: Option<String>,
    /// The xRec of every Validate in progress, innermost last, with its record (BC's OldRecord
    /// while a Validate chain runs on that record).
    pub validate_x: Vec<(crate::record::RecHandle, crate::record::RecHandle)>,
    /// Error traps open now (Session::trapped: TryFunction, `ok := Codeunit.Run`, asserterror, isolated
    /// event subscribers, FilterPageBuilder RunModal handlers).
    pub traps_open: u32,
    /// For each rec_validate_x in progress (innermost last): the record (Rc address) and traps_open when it
    /// began. A nested Validate keeps its own restore snapshot only if a trap opened since its record's
    /// outer Validate began (recops rec_validate_x).
    pub validate_traps: Vec<(usize, u32)>,
    /// The tenant's data encryption key (crate::enckey).
    pub enc: crate::enckey::EncState,
    /// The deployment the session presents itself as (crate::environment).
    pub env: crate::environment::EnvState,
    /// Pages with SaveValues = true: the page variables their controls show, as the page last
    /// closed (not with Cancel or No), by page id (crate::testpage, tp_save_values).
    pub page_saved_values: std::collections::HashMap<i64, Vec<(&'static str, crate::value::Val)>>,
    /// Reports whose request page has SaveValues: the request page variables as the page last
    /// closed (not Cancel or No), by save token. The report's Object Options row (table 2000000196,
    /// where BC's NavObjectOptions keeps them, in the session's transaction) holds the token, so a
    /// save rolls back with the database, as in BC (crate::testpage tp_save_values, crate::report).
    pub report_saved_values: std::collections::HashMap<u64, Vec<(&'static str, crate::value::Val)>>,
    /// Permission state: the test's effective permission sets (crate::permissions).
    pub perm: crate::permissions::PermState,
    /// Page background tasks (crate::pbt).
    pub pbt: crate::pbt::PbtState,
    /// The session's transaction outside tests (a test has its own: TestCtx::txn).
    pub txn: TxnState,
}

/// A transaction's TransactionType (CurrentTransactionType) and whether a statement ran in it
/// since its last Commit(). BC: Ncl TransactionManager, one LogicalTransaction per
/// BeginTransaction (a client call, a test method), starting as UpdateNoLocks; a Commit keeps the
/// type and clears BeginTransactionIssued.
#[derive(Clone, Copy, Default)]
pub struct TxnState {
    /// TransactionType ordinal (BC's enum order: UpdateNoLocks, Update, Snapshot, Browse, Report).
    pub ty: i32,
    /// A database write ran since the last Commit(). BC counts any statement (reads too); bc-rust
    /// counts the writes of non-temporary records (recops note_table_write).
    pub wrote: bool,
}

/// NavUserAccountHelper members answered from the User / User Property / Company tables
/// (Session::user_account_member).
pub const USER_ACCOUNT_MEMBERS: &[&str] = &[
    "iswindowsauthentication",
    "isdelegateduser",
    "isuserdelegatedadmin",
    "isuserdelegatedhelpdesk",
    "trygetauthenticationkey",
    "getauthenticationstatus",
    "ispasswordset",
    "getallowedcompanies",
];

/// A name in a call stack frame: quoted unless it is a plain identifier [src:A29.1].
fn quote_name(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c == '_' || c.is_alphabetic()) && name.chars().all(|c| c == '_' || c.is_alphanumeric());
    if plain {
        name.to_string()
    } else {
        format!("\"{name}\"")
    }
}

/// TransactionType names by ordinal (BC's TransactionType enum).
pub const TRANSACTION_TYPES: [&str; 5] = ["UpdateNoLocks", "Update", "Snapshot", "Browse", "Report"];

/// A registered table connection.
struct Conn {
    /// TableConnectionType ordinal.
    ty: i64,
    name: String,
    /// The rows of the CRM tables of a test connection (`@@test@@`), by table id; None otherwise.
    test_tables: Option<std::collections::HashMap<i64, Rc<RefCell<bc_store::TableData>>>>,
}

/// TableConnectionType::CRM.
fn crm_conn_type() -> i64 {
    crate::interp::system_option_ord("tableconnectiontype", "crm") as i64
}

/// A table stored in Dataverse (TableType CRM or CDS).
pub fn is_crm_table(t: &al_model::TableDef) -> bool {
    t.table_type.eq_ignore_ascii_case("crm") || t.table_type.eq_ignore_ascii_case("cds")
}

impl SysState {
    /// The in-memory rows of a CRM/CDS table when the current CRM connection is a test connection.
    pub fn crm_test_table(&mut self, schema: &std::sync::Arc<bc_store::TableSchema>) -> Option<Rc<RefCell<bc_store::TableData>>> {
        let ty = crm_conn_type();
        let cur = self.defaults.iter().find(|(t, _)| *t == ty).map(|(_, n)| n.as_str()).unwrap_or("");
        let c = self.conns.iter_mut().find(|c| c.ty == ty && c.name.eq_ignore_ascii_case(cur))?;
        let tables = c.test_tables.as_mut()?;
        Some(tables.entry(schema.id).or_insert_with(|| Rc::new(RefCell::new(bc_store::TableData::new(schema.clone())))).clone())
    }
}

impl<'r> Session<'r> {
    /// A CRM/CDS table is read or written through the current CRM connection, which must be
    /// registered (TableConnectionManager.GetCurrentTableConnection). Not called yet: BC runs all
    /// tests of a codeunit in one session, so a connection a previous test registered is still
    /// there; al-test starts a new session per test (enable once it keeps one per codeunit).
    pub fn crm_conn_check(&mut self, h: &crate::record::RecHandle) -> R<()> {
        let r = h.borrow();
        if r.temp.is_some() || !is_crm_table(r.tdef) {
            return Ok(());
        }
        let ty = crm_conn_type();
        let cur = self.sys.defaults.iter().find(|(t, _)| *t == ty).map(|(_, n)| n.clone()).unwrap_or_default();
        if self.sys.conns.iter().any(|c| c.ty == ty && c.name.eq_ignore_ascii_case(&cur)) {
            return Ok(());
        }
        err(format!(
            // own wording [src:X13]
            "No table connection {} of type CRM is registered; register one with RegisterTableConnection first.",
            cur.to_uppercase()
        ))
    }

    /// The in-memory table a record's rows live in instead of the database: its temporary table,
    /// or the table of a CRM/CDS table on a test table connection.
    pub fn rec_mem_table(&mut self, r: &crate::record::RecState) -> Option<Rc<RefCell<bc_store::TableData>>> {
        if r.temp.is_some() {
            return r.temp.clone();
        }
        if !is_crm_table(r.tdef) {
            return None;
        }
        self.sys.crm_test_table(&r.schema)
    }

    /// Before inserting a record of a CRM/CDS table on a test connection: give an empty Guid
    /// primary key a new value, as Dataverse does. Returns the field index it set.
    pub fn crm_test_new_id(&mut self, h: &crate::record::RecHandle) -> Option<usize> {
        let mut r = h.borrow_mut();
        if r.temp.is_some() || !is_crm_table(r.tdef) || self.sys.crm_test_table(&r.schema).is_none() {
            return None;
        }
        let i = *r.tdef.pk().fields.first()?;
        if r.row[i] != bc_store::Value::Guid([0; 16]) {
            return None;
        }
        r.row[i] = bc_store::Value::Guid(crate::clock::new_guid());
        Some(i)
    }
}

impl<'r> Session<'r> {
    /// Every row of a table (small system tables: User, User Property, Company).
    fn sys_rows(&mut self, table: i64) -> Vec<Vec<bc_store::Value>> {
        let Some(schema) = self.rt.schemas.get(&table).cloned() else { return vec![] };
        let req = bc_store::ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: usize::MAX, lock: false };
        let company = self.company.clone();
        self.db.scan(&company, &schema, &req).unwrap_or_default().into_iter().map(|r| r.to_vec()).collect()
    }

    /// The User row (2000000120) of a user, by User Security ID.
    fn user_row(&mut self, sid: &[u8; 16]) -> Option<Vec<bc_store::Value>> {
        self.sys_rows(crate::userauth::USER_TABLE).into_iter().find(|r| matches!(r.first(), Some(bc_store::Value::Guid(g)) if g == sid))
    }

    /// The User Property row (2000000121) of a user, by User Security ID. BC creates it when the
    /// User row is inserted (SystemTableTriggers.OnBeforeInsert); bc-rust does not, so an existing
    /// user without one has an empty row here. None: no such user.
    fn user_property_row(&mut self, sid: &[u8; 16]) -> Option<Vec<bc_store::Value>> {
        let t = crate::userauth::USER_PROPERTY_TABLE;
        if let Some(r) = self.sys_rows(t).into_iter().find(|r| matches!(r.first(), Some(bc_store::Value::Guid(g)) if g == sid)) {
            return Some(r);
        }
        self.user_row(sid)?;
        let mut blank = self.rt.schemas.get(&t)?.new_row().to_vec();
        if let Some(k) = blank.first_mut() {
            *k = bc_store::Value::Guid(*sid);
        }
        Some(blank)
    }

    /// NavUserAccountHelper members (NavUserAccount.dll, Ncl UserManagement / NavUser):
    /// - IsWindowsAuthentication: false, a fixed fact of this tier: no sign-in authenticates with a
    ///   Windows account or group, whatever the nominal credential type (crate::userauth calls it
    ///   Windows while BC_AUTH is unset); docs/assumptions.md "Fixed environment facts";
    /// - IsDelegatedUser / IsUserDelegatedAdmin / IsUserDelegatedHelpdesk: from the user's Entra
    ///   directory roles, which no bc-rust sign-in carries: false;
    /// - TryGetAuthenticationKey(UserSecurityId, out Key): needs SUPER or SECURITY for another user
    ///   (else false), then the User Property row's "Authentication Key" (no row: false);
    /// - GetAuthenticationStatus(UserSecurityId): Active (2) with an "Authentication Object ID",
    ///   else Inactive (1) with an "Authentication Email", else Disabled (0); an error without a
    ///   User or User Property row;
    /// - IsPasswordSet(UserSecurityId): the User Property row has a Password;
    /// - GetAllowedCompanies(): the companies the session can open (every company: permissions
    ///   on companies are not modelled).
    pub(crate) fn user_account_member(&mut self, f: &mut Frame, m: &str, a: &crate::dotnet::Args) -> R<Val> {
        use bc_store::Value as V;
        let sid = |i: usize| match a.get(i) {
            Val::Guid(g) => g,
            _ => [0; 16],
        };
        let text = |r: &[V], i: usize| match r.get(i) {
            Some(V::Text(s)) => s.clone(),
            _ => String::new(),
        };
        Ok(match m {
            "iswindowsauthentication" | "isdelegateduser" | "isuserdelegatedadmin" | "isuserdelegatedhelpdesk" => Val::Bool(false),
            "trygetauthenticationkey" => {
                let target = sid(0);
                let key = if crate::userauth::may_manage_user(self, &target) { self.user_property_row(&target).map(|r| text(&r, 3)) } else { None };
                self.net_out(f, a, 1, Val::Text(key.clone().unwrap_or_default()))?;
                Val::Bool(key.is_some())
            }
            "getauthenticationstatus" => {
                let target = sid(0);
                let user = self.user_row(&target);
                let (Some(user), Some(prop)) = (user, self.user_property_row(&target)) else {
                    return err(format!("There is no User within the filter.\n\nFilters: User Security ID: {}", Val::Guid(target).to_text()));
                };
                // User Property field 7 "Authentication Object ID", User field 11 "Authentication Email"
                let email_i = self.cat.tables.get(&crate::userauth::USER_TABLE).and_then(|t| t.field("Authentication Email"));
                Val::Int(if !text(&prop, 6).is_empty() {
                    2
                } else if email_i.is_some_and(|i| !text(&user, i).is_empty()) {
                    1
                } else {
                    0
                })
            }
            "ispasswordset" => {
                let target = sid(0);
                Val::Bool(self.user_property_row(&target).is_some_and(|r| !text(&r, 1).is_empty()))
            }
            "getallowedcompanies" => {
                let names: Vec<Val> = match self.cat.table_by_name("Company").map(|t| (t.id, t.field("Name"))) {
                    Some((id, Some(fi))) => self.sys_rows(id).iter().map(|r| Val::Text(text(r, fi))).collect(),
                    _ => vec![],
                };
                Val::List(Rc::new(RefCell::new(names)))
            }
            _ => return err(format!("Not implemented in bc-rust: NavUserAccountHelper.{m}")),
        })
    }

    /// SessionInformation.Callstack [src:A29.1]: the running AL frames, innermost first, each
    /// followed by '\':
    /// `<Object>(<ObjectType> <Id>).<Method>[(Trigger)] line <N> - <App> by <Publisher> version
    /// <Version>`. Names that are not plain identifiers are quoted; N is the frame's current
    /// statement line minus its procedure's signature line;
    /// a field or control trigger is named `<Field> - <Trigger>`.
    pub(crate) fn al_callstack(&self) -> String {
        let n = self.call_stack.len().min(self.frame_lines.len());
        let mods = crate::modules::modules(self.cat);
        let mut out = String::new();
        for i in (0..n).rev() {
            let code = self.call_stack[i];
            let (proc, _) = self.frame_lines[i];
            let line = if i + 1 == n { self.cur_line } else { self.frame_lines[i + 1].1 };
            let co = self.code(code);
            let Some(p) = co.procs.get(proc as usize) else { continue };
            // object type words of a call stack frame [src:A29.1]; Report, ReportExtension and
            // PageExtension are an open question [src:A29.2]
            let ty = match co.kind {
                al_syntax::ObjectKind::Table => "Table",
                al_syntax::ObjectKind::TableExtension => "TableExtension",
                al_syntax::ObjectKind::Page => "Page",
                al_syntax::ObjectKind::PageExtension => "PageExtension",
                al_syntax::ObjectKind::Codeunit => "CodeUnit",
                al_syntax::ObjectKind::Report => "Report",
                al_syntax::ObjectKind::ReportExtension => "ReportExtension",
                al_syntax::ObjectKind::Query => "Query",
                al_syntax::ObjectKind::XmlPort => "XmlPort",
                k => k.as_str(),
            };
            let trigger = p.kind == al_syntax::ProcKind::Trigger;
            let scope = match self.trigger_owner(code, proc).filter(|_| trigger) {
                Some(owner) => format!("{owner} - {}", p.name),
                None => p.name.clone(),
            };
            out.push_str(&quote_name(&co.name));
            out.push('(');
            out.push_str(ty);
            out.push(' ');
            out.push_str(&co.id.to_string());
            out.push_str(").");
            out.push_str(&quote_name(&scope));
            if trigger {
                out.push_str("(Trigger)");
            }
            out.push_str(&format!(" line {}", line.saturating_sub(p.line)));
            if let Some(app) = mods.app_of_code(code) {
                let v = app.version;
                out.push_str(&format!(" - {} by {} version {}.{}.{}.{}", app.name, app.publisher, v[0], v[1], v[2], v[3]));
            }
            out.push('\\');
        }
        out
    }

    /// The field (table code) or control (page code) a trigger procedure belongs to.
    fn trigger_owner(&self, code: u32, proc: u32) -> Option<String> {
        let co = self.code(code);
        let base = match co.kind {
            al_syntax::ObjectKind::TableExtension | al_syntax::ObjectKind::PageExtension => co.extends.map(|e| e.1)?,
            _ => co.id,
        };
        let is = |r: &al_model::ProcRef| r.code == code && r.proc == proc;
        match co.kind {
            al_syntax::ObjectKind::Table | al_syntax::ObjectKind::TableExtension => {
                let t = self.cat.tables.get(&base)?;
                t.fields
                    .iter()
                    .find(|fd| fd.on_validate.as_ref().is_some_and(is) || fd.on_lookup.as_ref().is_some_and(is) || fd.before_validate.iter().any(is) || fd.after_validate.iter().any(is))
                    .map(|fd| fd.name.clone())
            }
            al_syntax::ObjectKind::Page | al_syntax::ObjectKind::PageExtension => {
                fn find(tree: &[al_model::Control], is: &dyn Fn(&al_model::ProcRef) -> bool) -> Option<String> {
                    for c in tree {
                        if c.triggers.values().any(is) {
                            return Some(c.name.clone());
                        }
                        if let Some(n) = find(&c.children, is) {
                            return Some(n);
                        }
                    }
                    None
                }
                let p = self.cat.pages.get(&base)?;
                find(&p.layout, &is).or_else(|| find(&p.actions, &is))
            }
            _ => None,
        }
    }

    /// The session user's "Windows Security ID" (User table), '' when there is none.
    fn user_windows_sid(&mut self) -> String {
        let t = crate::userauth::USER_TABLE;
        let (Some(td), Some(schema)) = (self.cat.tables.get(&t), self.rt.schemas.get(&t).cloned()) else { return String::new() };
        let (Some(fs), Some(fw)) = (td.field("User Security ID"), td.field("Windows Security ID")) else { return String::new() };
        let req = bc_store::ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: usize::MAX, lock: false };
        let company = self.company.clone();
        let sid = self.user_sid;
        let rows = self.db.scan(&company, &schema, &req).unwrap_or_default();
        rows.iter()
            .find(|r| matches!(r.get(fs), Some(bc_store::Value::Guid(g)) if *g == sid))
            .and_then(|r| match r.get(fw) {
                Some(bc_store::Value::Text(s)) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// The running transaction: the test's, else the session's (TxnState).
    pub(crate) fn txn(&self) -> TxnState {
        match &self.test {
            Some(t) => t.txn,
            None => self.sys.txn,
        }
    }

    pub(crate) fn txn_mut(&mut self) -> &mut TxnState {
        match self.test.as_mut() {
            Some(t) => &mut t.txn,
            None => &mut self.sys.txn,
        }
    }

    /// CurrentTransactionType := `ty` (BC TransactionManager.CurrentTransactionType setter): before
    /// the transaction's first statement it is set; after it, a request for the same or a weaker
    /// type is ignored and a stronger one is an error. `force` (a report or XMLport run with its
    /// TransactionType property): set even after statements, as BC commits before running such a
    /// report outside AutoRollback tests (NavReport.RunReportCoreAsync BeginTransactionWorld).
    pub(crate) fn set_transaction_type(&mut self, ty: i32, force: bool) -> R<()> {
        let t = self.txn_mut();
        if !t.wrote || force {
            t.ty = ty;
            return Ok(());
        }
        let cur = t.ty;
        let ignore = match cur {
            0 => !(ty == 1 || ty == 2),
            1 => true,
            2 => ty > 1,
            _ => ty > 2,
        };
        if ignore {
            return Ok(());
        }
        let name = |i: i32| TRANSACTION_TYPES.get(i as usize).copied().unwrap_or("UpdateNoLocks");
        err(format!(
            // own wording [src:X12]
            "The transaction type can only be set before the transaction reads or writes the database; it was changed from {} to {}.",
            name(cur),
            name(ty)
        ))
    }
}

pub fn is_sys_builtin(l: &str) -> bool {
    matches!(
        l,
        "sid"
            | "registertableconnection"
            | "unregistertableconnection"
            | "hastableconnection"
            | "setdefaulttableconnection"
            | "getdefaulttableconnection"
            | "issessionactive"
            | "stopsession"
            | "hascollectederrors"
            | "getcollectederrors"
            | "clearcollectederrors"
            | "iscollectingerrors"
    )
}

/// SessionId() of every bc-rust session (crate::builtins "sessionid").
const CURRENT_SESSION_ID: i64 = 1;

impl<'r> Session<'r> {
    /// One of the functions of [`is_sys_builtin`] (`l` lower-cased).
    pub fn sys_builtin(&mut self, f: &mut Frame, l: &str, args: &[Expr]) -> R<Val> {
        let mut a = Vec::with_capacity(args.len());
        for e in args {
            a.push(self.eval(f, e)?);
        }
        if matches!(l, "hascollectederrors" | "getcollectederrors" | "clearcollectederrors" | "iscollectingerrors") {
            return Ok(self.collected_errors_fn(l, &a));
        }
        let ty = a.first().map(|v| v.as_i64()).unwrap_or(0);
        let name = a.get(1).map(|v| v.to_text().trim().to_string()).unwrap_or_default();
        let conn_str = a.get(2).map(|v| v.to_text()).unwrap_or_default();
        // Sid([UserName]): no name = the session user's Windows security id (the User row's
        // "Windows Security ID", '' for a user without Windows authentication); a name is looked up
        // among the host's Windows accounts, of which this tier has none, so ''. Real BC differs
        // [src:A35.2]
        if l == "sid" {
            let name = a.first().map(|v| v.to_text()).unwrap_or_default();
            return Ok(Val::Text(if name.is_empty() { self.user_windows_sid() } else { String::new() }));
        }
        let in_test = self.test.is_some();
        let st = &mut self.sys;
        Ok(match l {
            "registertableconnection" => {
                if st.conns.iter().any(|c| c.ty == ty && c.name.eq_ignore_ascii_case(&name)) {
                    let tyname = if ty == crm_conn_type() { "CRM".to_string() } else { ty.to_string() };
                    // own wording [src:X13]
                    return err(format!("A table connection {name} of type {tyname} is registered already."));
                }
                let test = ty == crm_conn_type() && in_test && (conn_str == "@@test@@" || conn_str.to_ascii_lowercase().contains("url=@@test@@"));
                st.conns.push(Conn { ty, name, test_tables: test.then(Default::default) });
                Val::Void
            }
            "unregistertableconnection" => {
                st.conns.retain(|c| !(c.ty == ty && c.name.eq_ignore_ascii_case(&name)));
                st.defaults.retain(|(t, n)| !(*t == ty && n.eq_ignore_ascii_case(&name)));
                Val::Void
            }
            "hastableconnection" => Val::Bool(st.conns.iter().any(|c| c.ty == ty && c.name.eq_ignore_ascii_case(&name))),
            "setdefaulttableconnection" => {
                st.defaults.retain(|(t, _)| *t != ty);
                if !name.is_empty() {
                    st.defaults.push((ty, name));
                }
                Val::Void
            }
            "getdefaulttableconnection" => Val::Text(st.defaults.iter().find(|(t, _)| *t == ty).map(|(_, n)| n.clone()).unwrap_or_default()),
            "issessionactive" => Val::Bool(ty == CURRENT_SESSION_ID.max(crate::workers::current_session_id() as i64) || crate::workers::is_active(ty)),
            // StartSession runs its codeunit to the end at once (builtins start_session): nothing to stop
            // with the background pool: ask that session to stop (crate::workers)
            "stopsession" => {
                crate::workers::stop(ty);
                Val::Void
            }
            _ => return err(format!("Not implemented in bc-rust: function {l}")),
        })
    }
}
