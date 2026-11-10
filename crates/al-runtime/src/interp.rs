//! The AL interpreter: sessions, frames, name resolution, calls, events,
//! statements and expressions.

use crate::record::*;
use crate::value::*;
use al_model::{Catalog, CodeObject, ProcRef};
use al_syntax::*;
use bc_store::{Backend, Conn, TableSchema};
use rust_decimal::prelude::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

pub struct Runtime {
    pub cat: &'static Catalog,
    pub db: Arc<dyn Backend>,
    pub schemas: HashMap<i64, Arc<TableSchema>>,
    /// System tables computed from the catalog (Field, AllObj, ...), built on first use.
    pub catalog_tables: std::sync::Mutex<HashMap<i64, Arc<bc_store::TableData>>>,
    /// User name (upper case) -> User Security ID and when it was read (crate::userauth::resolve_session_user).
    pub user_sids: std::sync::Mutex<HashMap<String, ([u8; 16], std::time::Instant)>>,
}

impl Runtime {
    pub fn new(cat: &'static Catalog, db: Arc<dyn Backend>) -> Runtime {
        let _ = CATALOG.set(cat);
        let schemas = cat.tables.iter().map(|(id, t)| (*id, Arc::new(schema_for(t)))).collect();
        Runtime { cat, db, schemas, catalog_tables: Default::default(), user_sids: Default::default() }
    }

    /// Rows of a catalog-computed system table, or None for ordinary tables.
    pub fn catalog_table(&self, id: i64) -> Option<Arc<bc_store::TableData>> {
        if !crate::virt::is_catalog_table(id) {
            return None;
        }
        let mut m = self.catalog_tables.lock().unwrap();
        if let Some(t) = m.get(&id) {
            return Some(t.clone());
        }
        let tdef = self.cat.tables.get(&id)?;
        let t = Arc::new(crate::virt::catalog_table(self.cat, tdef, self.schemas[&id].clone()));
        m.insert(id, t.clone());
        Some(t)
    }

    pub fn resolver(cat: &'static Catalog) -> bc_store::Resolver {
        Box::new(move |id| cat.tables.get(&id).map(|t| Arc::new(schema_for(t))))
    }
}

#[derive(Clone)]
pub struct Var {
    /// From the catalog (lives for the whole run), so no copy per call.
    pub name: &'static str,
    pub slot: Slot,
    pub ty: Option<&'static TypeRef>,
}

pub struct Globals {
    pub code: u32,
    pub vars: Vec<Var>,
}

pub struct Instance {
    pub code: u32,
    globals: RefCell<Option<Rc<Globals>>>,
    /// Page `Rec` / codeunit TableNo record.
    pub rec: RefCell<Option<RecHandle>>,
    /// Page variables: LookupMode.
    pub lookup: std::cell::Cell<bool>,
    /// Pages: instances of the page's parts (CurrPage.<Part>.Page), by control name.
    pub parts: RefCell<Vec<(Box<str>, Rc<Instance>)>>,
    /// Queries: filters and the result set.
    pub query: RefCell<Option<Box<crate::query::QueryState>>>,
    /// Reports: request page options, UseRequestPage, extension instances (crate::report).
    pub report: RefCell<Option<Box<crate::report::ReportState>>>,
    /// Pages: CurrPage.Update was called; a TestPage re-reads its record (crate::testpage).
    pub update: std::cell::Cell<bool>,
    /// Pages: CurrPage.Update(false) was called: the record is re-read without saving it.
    pub update_nosave: std::cell::Cell<bool>,
    /// Pages: CurrPage.Close was called; the TestPage closes once the trigger that called it
    /// returns (crate::testpage).
    pub close: std::cell::Cell<bool>,
    /// Pages: OnInit ran for the page's current open (parts run it with their parent).
    pub inited: std::cell::Cell<bool>,
    /// Pages: the page's own xRec while it is on a new record it started (crate::testpage
    /// tp_new_record); a loaded record clears it (tp_after_get).
    pub page_xrec: RefCell<Option<RecHandle>>,
    /// Pages: the TestPage this instance belongs to (page code saves through it: CurrPage.SaveRecord).
    pub test_page: RefCell<Option<std::rc::Weak<RefCell<crate::testpage::TestPage>>>>,
    /// Pages: CurrPage.Editable / PageVar.Editable as AL set it (None: the page's Editable
    /// property; BC NavForm.Editable, initialised from the metadata, not from the open mode).
    pub editable: std::cell::Cell<Option<bool>>,
}

impl Instance {
    pub fn new(code: u32) -> Rc<Instance> {
        Rc::new(Instance { code, globals: RefCell::new(None), rec: RefCell::new(None), lookup: std::cell::Cell::new(false), parts: RefCell::new(vec![]), query: RefCell::new(None), report: RefCell::new(None), update: std::cell::Cell::new(false), update_nosave: std::cell::Cell::new(false), close: std::cell::Cell::new(false), inited: std::cell::Cell::new(false), page_xrec: RefCell::new(None), test_page: RefCell::new(None), editable: std::cell::Cell::new(None) })
    }
}

pub struct Frame {
    pub code: u32,
    pub locals: Vec<Var>,
    pub globals: Rc<Globals>,
    pub rec: Option<RecHandle>,
    pub xrec: Option<RecHandle>,
    pub with: Vec<RecHandle>,
    pub inst: Option<Rc<Instance>>,
    pub ret: Option<Slot>,
    pub ret_ty: Option<&'static TypeRef>,
    pub curr_field: i32,
}

#[derive(Debug, Clone)]
pub struct AlError {
    pub msg: String,
    pub stack: Vec<String>,
    /// BC error code as returned by GetLastErrorCode ("Dialog", "TestField", "DB:RecordNotFound", ...).
    pub code: &'static str,
}

impl AlError {
    pub fn new(msg: String) -> AlError {
        AlError { msg, stack: vec![], code: "" }
    }
}

pub enum Flow {
    Exit,
    Break,
    Continue,
    Err(Box<AlError>),
}

pub type R<T> = Result<T, Flow>;

pub fn err<T>(msg: impl Into<String>) -> R<T> {
    Err(Flow::Err(Box::new(AlError::new(msg.into()))))
}

/// An error with a BC error code (GetLastErrorCode).
pub fn err_code<T>(code: &'static str, msg: impl Into<String>) -> R<T> {
    Err(Flow::Err(Box::new(AlError { msg: msg.into(), stack: vec![], code })))
}

/// The error for a Decimal result outside the decimal range: the message of .NET's
/// OverflowException for decimals. Real BC's range error differs [src:A31.2].
pub const DEC_OVERFLOW: &str = "Value was either too large or too small for a Decimal.";

/// The result of a checked Decimal operation, or BC's overflow error instead of a panic.
pub fn dec_checked(v: Option<Decimal>) -> R<Decimal> {
    match v {
        Some(d) => Ok(dec18(d)),
        None => err(DEC_OVERFLOW),
    }
}

/// An AL Decimal result keeps at most 18 significant digits of its coefficient, rounded, and a rounded fraction
/// drops its trailing zeros [src:A31.1]: 1/3 is 0.333333333333333333 and 2/3 0.666666666666666667. So
/// 100 / 85.68024 is 1.16713025080228533 in BC (its Currency Factor columns hold that), not .NET's 28-digit
/// 1.1671302508022853344014909389 (ledger oracle, I15). Ties go away from zero (open question [src:A31.3]). A
/// result with more than 18 integer digits keeps no fraction.
pub fn dec18(d: Decimal) -> Decimal {
    const MAX18: u128 = 999_999_999_999_999_999;
    let m = d.mantissa().unsigned_abs();
    if m <= MAX18 {
        return d;
    }
    let mut digits = 19u32;
    while digits < 38 && m > 10u128.pow(digits) - 1 {
        digits += 1;
    }
    let excess = digits - 18;
    let scale = d.scale();
    if scale >= excess {
        return d.round_dp_with_strategy(scale - excess, RoundingStrategy::MidpointAwayFromZero).normalize();
    }
    let p = 10u128.pow(excess);
    let mut q = m / p;
    if (m % p) * 2 >= p {
        q += 1;
    }
    let v = (q * 10u128.pow(excess - scale)) as i128;
    Decimal::from_i128_with_scale(if d.is_sign_negative() { -v } else { v }, 0)
}

pub enum LVal {
    Slot(Slot, Option<&'static TypeRef>),
    Field(RecHandle, usize),
    Elem(Rc<RefCell<Vec<Val>>>, usize, Option<&'static TypeRef>),
    /// ErrorInfo property (only Message is kept)
    ErrorMsg(crate::errinfo::ErrRef, String),
    /// Character `idx` (0-based) of a text variable: `Txt[1] := 13`.
    TextChar(Box<LVal>, usize),
    /// Record property in assignment form: `Rec.FilterGroup := 2`, `Rec.ReadIsolation := ...`.
    RecProp(RecHandle, &'static str),
    /// Session property in assignment form: `GlobalLanguage := 1031`.
    SessionProp(&'static str),
    /// Property of a DotNet / Xml / Http value in assignment form: `UriBuilder.Query := X`.
    NetProp(crate::dotnet::NetRef, Box<str>),
    /// A row of a multi-dimensional array (`A[i]` of `array[m, n]`): the flat elements from
    /// `start`, an array of the remaining dimensions `dims`.
    Row(Rc<RefCell<Vec<Val>>>, usize, Vec<usize>),
}

pub enum Arg {
    Val(Val),
    Ref(LVal),
}

pub struct Session<'r> {
    /// Every global-variable block this session made (weak), cleared when the session ends: a
    /// record copy stored in its own table's globals (`SalesLine2 := Rec`, the copy shares the
    /// globals) or instances that hold each other form Rc cycles, which would otherwise outlive
    /// the session (al-test leaked about 40 instances and 70 records per test, 2026-11-08).
    made_globals: Vec<std::rc::Weak<Globals>>,
    /// A process-wide id (crate::frames::next_session_id).
    pub id: u64,
    pub rt: &'r Runtime,
    pub cat: &'static Catalog,
    pub db: Box<dyn Conn>,
    pub company: String,
    pub user_id: String,
    /// UserSecurityId(): the signed-in user's User Security ID (bc-server's auth, crate::userauth), else the
    /// platform user's (crate::platform::USER_SID).
    pub user_sid: [u8; 16],
    pub work_date: i32,
    pub single: HashMap<u32, Rc<Instance>>,
    pub messages: Vec<String>,
    /// GuiAllowed() outside tests: true for sessions behind a client UI (bc-nst's web
    /// client sessions), false for API/background sessions. Default false.
    pub gui_allowed: bool,
    /// CurrentClientType(): "web" (bc-nst, bc-rust's web client, tests), "api" (API v2.0 requests),
    /// "background" (the pool's task and StartSession sessions); a name of system option ClientType
    pub client_type: &'static str,
    pub depth: usize,
    pub last_error: String,
    pub missing: BTreeMap<String, usize>,
    pub steps: u64,
    pub trace: bool,
    /// Set while evaluating an expression statement: the call's result is discarded,
    /// so failing Get/Find/Insert/... must raise an error instead of returning false.
    pub discard: bool,
    /// discard flag captured by resolve_call for the procedure about to be invoked (TryFunction semantics)
    pub pending_discard: bool,
    /// Code and call stack of the last error (GetLastErrorCode / GetLastErrorCallStack).
    pub last_error_code: String,
    pub last_error_stack: String,
    /// Instances bound with BindSubscription (subscribers of EventSubscriberInstance = Manual
    /// codeunits). Weak: a binding ends when its instance is gone (no variable refers to it any
    /// more), not only with UnbindSubscription [src:A30.2].
    pub bound: Vec<std::rc::Weak<Instance>>,
    /// Running test procedure: UI handlers and test isolation (None outside tests).
    pub test: Option<crate::testing::TestCtx>,
    /// Abort with an error after this many AL statements (runaway loops in tests).
    pub step_limit: u64,
    /// > 0 while a StartSession codeunit runs inline (its Commit() calls become savepoints).
    pub bg_depth: usize,
    /// The savepoint of the last Commit() inside the running background task or session (its
    /// errors roll back to it, not to its start: BC keeps what the task committed).
    pub bg_commit_mark: Option<usize>,
    /// Read-ahead validity (recops `next`): a number per database write of this session, and the
    /// last one per table.
    pub write_seq: u64,
    pub table_writes: HashMap<i64, u64>,
    pub last_session_id: i32,
    /// TestPages waiting for the next Page.Run of their page (TestPage.Trap()).
    pub traps: Vec<crate::testpage::TpRef>,
    /// GlobalLanguage (Windows language id). Read and format calls of crate::datefml take it as
    /// an argument (no module state).
    pub global_language: i32,
    /// Parsed date formulas of this session (crate::datefml::Cache; CalcDate is on the posting path).
    pub df_cache: crate::datefml::Cache,
    /// The next invoked procedure's Boolean return value defaults to true (page triggers such as
    /// OnInsertRecord/OnQueryClosePage that end without exit(...)).
    pub ret_true: bool,
    /// Telemetry sink: Session.LogMessage(EventId, Message, ...) calls it with the event id and message.
    pub on_log: Option<Box<dyn FnMut(&str, &str)>>,
    /// Code objects of the procedures being executed, innermost last (NavApp.GetCallerModuleInfo).
    pub call_stack: Vec<u32>,
    /// In step with call_stack: each frame's procedure and the line its caller was on when it
    /// was called (SessionInformation.Callstack, crate::sysfuncs al_callstack).
    pub frame_lines: Vec<(u32, u32)>,
    /// The line of the statement running in the innermost frame.
    pub cur_line: u32,
    /// The procedures being executed (index in their code object), innermost last, in step with
    /// call_stack (crate::permissions: a method's [InherentPermissions]).
    pub proc_stack: Vec<u32>,
    /// Records that page frames being executed run on (a page's Rec): their xRec in table code is
    /// the page's xRec, BC's OldRecord of the page's source table (Session::xrec_of).
    pub page_recs: Vec<RecHandle>,
    /// Source records of the open TestPages (tp_open/tp_close): their xRec is the page's xRec too.
    pub page_open_recs: Vec<std::rc::Weak<crate::record::RecCell>>,
    /// The app being installed (index in crate::modules::modules().apps; u32::MAX for an app
    /// without a module entry) while Session::install_apps runs its Install codeunits.
    pub installing: Option<u32>,
    /// Client UI outside tests (Confirm, StrMenu, Page.Run); see [`UiHost`].
    pub ui: Option<Box<dyn UiHost>>,
    /// Server files of the session (AL File type, report datasets): crate::files.
    pub files: crate::files::Vfs,
    /// CurrFieldNo of record instances (crate::currfield).
    pub curr_fields: crate::currfield::CurrFields,
    /// Table connections and other system-function state (crate::sysfuncs).
    pub sys: crate::sysfuncs::SysState,
}

/// A client that shows AL's UI calls outside tests (bc-nst: Microsoft's web client).
/// A method returns `None` when the client does not take the call; the runtime's own
/// behaviour applies then (the default answer, a "(UI) page opened" note).
pub trait UiHost {
    /// `s` is the session (its pending `messages` show before the dialog).
    fn confirm(&mut self, s: &mut Session<'_>, question: &str, default: bool) -> Option<bool>;
    /// StrMenu: the chosen option, 1-based; 0 is Cancel.
    fn strmenu(&mut self, s: &mut Session<'_>, options: &str, default: i32, instruction: &str) -> Option<i32>;
    /// Page.Run / Page.RunModal of `page_id`: `inst` is the page variable's instance (its
    /// SetRecord / SetTableView state), `rec` the record passed to Page.Run(Id, Rec).
    /// Returns the page's Action.
    fn run_page(&mut self, s: &mut Session<'_>, page_id: i64, inst: Option<Rc<Instance>>, rec: Option<RecHandle>, modal: bool) -> Option<R<Val>>;
}

impl<'r> Session<'r> {
    pub fn new(rt: &'r Runtime, company: &str) -> Session<'r> {
        Self::try_new(rt, company).expect("database connection")
    }

    pub fn try_new(rt: &'r Runtime, company: &str) -> Result<Session<'r>, bc_store::StoreError> {
        Ok(Session {
            made_globals: vec![],
            id: crate::frames::next_session_id(),
            rt,
            cat: rt.cat,
            db: rt.db.connect()?,
            company: company.to_string(),
            user_id: "ADMIN".into(),
            user_sid: crate::platform::USER_SID,
            work_date: today(),
            single: HashMap::new(),
            messages: vec![],
            gui_allowed: false,
            client_type: "web",
            depth: 0,
            last_error: String::new(),
            missing: BTreeMap::new(),
            steps: 0,
            trace: false,
            discard: false,
            pending_discard: false,
            last_error_code: String::new(),
            last_error_stack: String::new(),
            bound: vec![],
            test: None,
            step_limit: u64::MAX,
            bg_depth: 0,
            bg_commit_mark: None,
            write_seq: 0,
            table_writes: HashMap::new(),
            last_session_id: 1,
            traps: vec![],
            global_language: 1033,
            df_cache: crate::datefml::Cache::new(),
            ret_true: false,
            on_log: None,
            call_stack: vec![],
            frame_lines: vec![],
            cur_line: 0,
            proc_stack: vec![],
            page_recs: vec![],
            page_open_recs: vec![],
            installing: None,
            ui: None,
            files: Default::default(),
            curr_fields: Default::default(),
            sys: Default::default(),
        })
    }

    /// Confirm through the client ([`UiHost`]); `None` without one, and in tests.
    pub fn host_confirm(&mut self, question: &str, default: bool) -> Option<bool> {
        if self.test.is_some() {
            return None;
        }
        let mut h = self.ui.take()?;
        let r = h.confirm(self, question, default);
        self.ui = Some(h);
        r
    }

    /// StrMenu through the client ([`UiHost`]); `None` without one, and in tests.
    pub fn host_strmenu(&mut self, options: &str, default: i32, instruction: &str) -> Option<i32> {
        if self.test.is_some() {
            return None;
        }
        let mut h = self.ui.take()?;
        let r = h.strmenu(self, options, default, instruction);
        self.ui = Some(h);
        r
    }

    /// Page.Run / RunModal through the client ([`UiHost`]); `None` without one, and in tests.
    pub fn host_run_page(&mut self, page_id: i64, inst: Option<Rc<Instance>>, rec: Option<RecHandle>, modal: bool) -> Option<R<Val>> {
        if self.test.is_some() {
            return None;
        }
        let mut h = self.ui.take()?;
        let r = h.run_page(self, page_id, inst, rec, modal);
        self.ui = Some(h);
        r
    }

    /// The host ends the unit of work (a client call, a request): the next one is a new
    /// transaction (UpdateNoLocks, crate::sysfuncs TxnState).
    pub fn commit(&mut self) -> Result<(), bc_store::StoreError> {
        self.sys.txn = Default::default();
        self.db.commit()
    }

    pub fn rollback(&mut self) {
        self.sys.txn = Default::default();
        self.db.rollback()
    }

    /// Remember an error that was caught (GetLastErrorText / GetLastErrorCode / GetLastErrorCallStack).
    pub fn note_error(&mut self, e: &AlError) {
        // AL_TRACE_CAUGHT=1: print errors that AL code catches (Codeunit.Run, TryFunction, asserterror)
        static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *TRACE.get_or_init(|| std::env::var_os("AL_TRACE_CAUGHT").is_some()) {
            eprintln!("  caught: {} {:?}", e.msg.lines().next().unwrap_or(""), e.stack);
        }
        // a refusal caught here was swallowed by AL code (crate::assume: the test is not trusted)
        crate::assume::note_caught(&e.msg);
        self.last_error = e.msg.clone();
        self.last_error_code = e.code.to_string();
        self.last_error_stack = if e.stack.is_empty() { "(AL)".to_string() } else { e.stack.join("\\") };
    }

    pub fn code(&self, i: u32) -> &'static CodeObject {
        &self.cat.code[i as usize]
    }

    // ------------------------------------------------------------------ values & types

    pub fn default_val(&mut self, ty: &'static TypeRef) -> Val {
        match ty {
            TypeRef::Simple { name, .. } => match crate::frames::simple_default(ty, name) {
                Some(v) => v,
                None => match crate::frames::Lower::new(name).as_str() {
                "integer" | "byte" => Val::Int(0),
                "biginteger" => Val::BigInt(0),
                "decimal" => Val::Dec(Decimal::ZERO),
                "boolean" => Val::Bool(false),
                "code" | "text" | "bigtext" | "secrettext" | "textbuilder" => Val::Text(String::new()),
                "char" => Val::Char('\0'),
                "date" => Val::Date(0),
                "time" => Val::Time(0),
                "datetime" => Val::DateTime(bc_store::DATETIME_UNDEFINED),
                "duration" => Val::Duration(0),
                "guid" => Val::Guid([0; 16]),
                "dateformula" => Val::DateFormula(String::new()),
                "recordid" => Val::RecordId(0, vec![]),
                "variant" => Val::Void,
                "errorinfo" => crate::errinfo::new_val(String::new()),
                "jsonobject" => Val::Json(crate::json::jref(crate::json::J::Obj(vec![]))),
                "jsonarray" => Val::Json(crate::json::jref(crate::json::J::Arr(vec![]))),
                "jsontoken" | "jsonvalue" => Val::Json(crate::json::jref(crate::json::J::Null)),
                "notification" => crate::platobj::new_notification(),
                "recordref" => self.new_recordref(),
                "fieldref" => Val::Opaque("FieldRef"),
                "instream" => Val::Opaque("InStream"),
                "outstream" => Val::Opaque("OutStream"),
                "moduleinfo" => Val::Opaque("ModuleInfo"),
                "version" => Val::Opaque("Version"),
                "sessionsettings" => crate::platobj::new_session_settings(),
                "filterpagebuilder" => crate::platobj::new_filter_page_builder(),
                "webserviceactioncontext" => crate::platobj::new_action_context(),
                "file" => Val::Opaque("File"),
                "dialog" => Val::Opaque("Dialog"),
                "textbuilder" => Val::Text(String::new()),
                other => crate::dotnet::default_simple(other).unwrap_or(Val::Opaque("Object")),
            }},
            TypeRef::Object { kind, target, temporary } => match kind.as_str() {
                "record" => match crate::frames::table_of(self.cat, ty, target) {
                    Some(id) if self.cat.tables.contains_key(&id) => Val::Record(self.new_record(id, *temporary)),
                    _ => {
                        *self.missing.entry(format!("table {target}")).or_default() += 1;
                        Val::Opaque("Record")
                    }
                },
                "testpage" => match self.cat.id_of(ObjectKind::Page, target).and_then(|id| self.new_test_page(id, None)) {
                    Some(tp) => Val::TestPage(tp),
                    None => Val::Opaque("TestPage"),
                },
                "codeunit" | "page" | "report" | "query" | "xmlport" | "testrequestpage" => {
                    let k = match kind.as_str() {
                        "codeunit" => ObjectKind::Codeunit,
                        "page" | "testpage" => ObjectKind::Page,
                        "report" | "testrequestpage" => ObjectKind::Report,
                        "query" => ObjectKind::Query,
                        _ => ObjectKind::XmlPort,
                    };
                    match self.cat.id_of(k, target).and_then(|id| self.cat.code_by_key.get(&(k, id))) {
                        Some(&ci) => {
                            if k == ObjectKind::Codeunit && self.code(ci).single_instance {
                                Val::Object(self.single_instance(ci))
                            } else {
                                Val::Object(Instance::new(ci))
                            }
                        }
                        // platform codeunits without AL source (Base64Convert, ...)
                        None => crate::dotnet::platform_codeunit(target).unwrap_or(Val::Opaque("Object")),
                    }
                }
                "enum" => Val::Opt { ord: 0, enum_id: self.cat.id_of(ObjectKind::Enum, target).or(Some(-1)) },
                "interface" => Val::Iface(None),
                "dotnet" => crate::dotnet::default_dotnet(self.cat, target),
                "controladdin" => Val::Opaque("ControlAddIn"),
                _ => Val::Opaque("Object"),
            },
            TypeRef::Array { dims, elem } => {
                let n: usize = dims.iter().map(|d| *d as usize).product();
                let v: Vec<Val> = (0..n).map(|_| self.default_val(elem)).collect();
                // the elements of an array of temporary records share one temporary table
                if let Some(Val::Record(first)) = v.first() {
                    let shared = first.borrow().temp.clone();
                    if let Some(t) = shared {
                        for x in v.iter().skip(1) {
                            if let Val::Record(h) = x {
                                h.borrow_mut().temp = Some(t.clone());
                            }
                        }
                    }
                }
                Val::Array(Rc::new(RefCell::new(v)), dims.iter().map(|d| *d as usize).collect())
            }
            TypeRef::List(_) => Val::List(Rc::new(RefCell::new(vec![]))),
            TypeRef::Dictionary(..) => Val::Dict(Rc::new(RefCell::new(vec![]))),
            // Option values keep their declared members (crate::optmeta)
            TypeRef::Option(m) => Val::Opt { ord: 0, enum_id: Some(crate::optmeta::for_members(m)) },
            TypeRef::Label { text, .. } => Val::Text(text.clone()),
            TypeRef::TextConst(t) => Val::Text(t.clone()),
        }
    }

    pub fn single_instance(&mut self, ci: u32) -> Rc<Instance> {
        self.single.entry(ci).or_insert_with(|| Instance::new(ci)).clone()
    }

    pub fn new_record(&mut self, table: i64, temporary: bool) -> RecHandle {
        let tdef = &self.cat.tables[&table];
        let schema = self.rt.schemas[&table].clone();
        let init = crate::frames::initial_row(self, table, tdef, &schema);
        let temp = (temporary || tdef.temporary_only).then(|| Rc::new(RefCell::new(bc_store::TableData::new(schema.clone()))));
        Rc::new(crate::record::RecCell::new(RecState {
            tdef,
            schema,
            // the table's initial row, shared until the record writes it (CowRow)
            row: init,
            xrow: None,
            filters: vec![],
            filter_group: 0,
            key: 0,
            custom_key: None,
            ascending: true,
            temp,
            company: None,
            globals: None,
            ext_globals: vec![],
            marks: Default::default(),
            mark_only: false,
            auto_calc: vec![],
            prefetch: Default::default(),
            is_ref: false,
            unopened: false,
        }))
    }

    /// A RecordRef that is not open yet. RecordRefs are reference values: the handle is shared by
    /// copies of the variable, and Open/GetTable/Close replace its contents in place.
    pub fn new_recordref(&mut self) -> Val {
        if !self.cat.tables.contains_key(&crate::virt::INTEGER) {
            return Val::Opaque("RecordRef");
        }
        let h = self.new_record(crate::virt::INTEGER, true);
        {
            let mut b = h.borrow_mut();
            b.is_ref = true;
            b.unopened = true;
        }
        Val::Record(h)
    }

    /// The RecordRef for a value assigned or passed to a RecordRef: RecordRefs are shared, a
    /// Record becomes a new RecordRef on a copy of it (GetTable).
    pub fn as_recordref(&mut self, v: Val) -> Val {
        match v {
            Val::Record(h) if h.borrow().is_ref => Val::Record(h),
            Val::Record(h) => {
                let c = copy_record(&h);
                c.borrow_mut().is_ref = true;
                Val::Record(c)
            }
            o => o,
        }
    }

    /// Apply InitValue properties.
    pub fn init_row(&self, tdef: &'static al_model::TableDef, row: &mut bc_store::Row) {
        for (i, f) in tdef.fields.iter().enumerate() {
            if let Some(iv) = &f.init_value {
                let enum_lookup = |n: &str| self.enum_ordinal_for_field(f, n);
                if let Some(v) = text_to_store(f, iv, &enum_lookup, self.work_date) {
                    row[i] = v;
                }
            }
        }
    }

    pub fn enum_ordinal_for_field(&self, f: &al_model::FieldDef, n: &str) -> Option<i32> {
        if let al_model::FieldType::Enum(en) = &f.ty {
            let e = self.cat.enum_by_name(en)?;
            return e.values.iter().find(|v| crate::filter::text_eq(&v.name, n) || crate::filter::text_eq(&v.caption, n)).map(|v| v.ordinal as i32);
        }
        None
    }

    /// Coerce a value for assignment to a variable of type `ty`.
    pub fn coerce(&mut self, v: Val, ty: Option<&'static TypeRef>) -> R<Val> {
        let Some(ty) = ty else { return Ok(v) };
        // DotNet values: box AL values assigned to DotNet variables, unbox .NET primitives into AL types
        let v = match (&v, ty) {
            (_, TypeRef::Object { kind, target, .. }) if kind == "dotnet" => return Ok(crate::dotnet::coerce_dotnet(self.cat, target, v)),
            (Val::Net(_), TypeRef::Simple { name, .. }) if !name.eq_ignore_ascii_case("variant") => crate::dotnet::unboxed(v),
            _ => v,
        };
        Ok(match ty {
            // the common kinds first (the same as the string arms below), decided once per type node
            TypeRef::Simple { name, len } => match crate::frames::simple_kind(ty, name) {
                crate::frames::SimpleKind::Int => Val::Int(v.as_i64() as i32),
                crate::frames::SimpleKind::BigInt => Val::BigInt(v.as_i64()),
                crate::frames::SimpleKind::Dec => Val::Dec(v.as_dec()),
                crate::frames::SimpleKind::Bool => Val::Bool(v.as_bool()),
                crate::frames::SimpleKind::Code => {
                    let s = v.to_text().trim().to_uppercase();
                    check_len(&s, *len)?;
                    Val::Text(s)
                }
                crate::frames::SimpleKind::Text => {
                    let s = v.to_text();
                    check_len(&s, *len)?;
                    Val::Text(s)
                }
                _ => match crate::frames::Lower::new(name).as_str() {
                "integer" | "byte" => Val::Int(v.as_i64() as i32),
                "biginteger" => Val::BigInt(v.as_i64()),
                "decimal" => Val::Dec(v.as_dec()),
                "boolean" => Val::Bool(v.as_bool()),
                "code" => {
                    let s = v.to_text().trim().to_uppercase();
                    check_len(&s, *len)?;
                    Val::Text(s)
                }
                "text" => {
                    let s = v.to_text();
                    check_len(&s, *len)?;
                    Val::Text(s)
                }
                // SecretText holds text; a Void (e.g. from an unemulated DotNet call) becomes ''
                "secrettext" => Val::Text(v.to_text()),
                "char" => match v {
                    Val::Text(s) => Val::Char(s.chars().next().unwrap_or('\0')),
                    Val::Int(i) => Val::Char(char::from_u32(i as u32).unwrap_or('\0')),
                    o => o,
                },
                "guid" => match v {
                    Val::Text(s) => Val::Guid(parse_guid(&s).unwrap_or([0; 16])),
                    o => o,
                },
                "duration" => match v {
                    Val::Int(_) | Val::BigInt(_) | Val::Dec(_) => Val::Duration(v.as_dec().round().to_i64().unwrap_or(0)),
                    o => o,
                },
                // a DateTime (a Variant's) into a Date: its date, as DT2Date gives it
                "date" => match v {
                    Val::DateTime(t) if t != bc_store::DATETIME_UNDEFINED => {
                        Val::Date(chrono::DateTime::from_timestamp_millis(t).map(|d| chrono::Datelike::num_days_from_ce(&d.date_naive()) * 2).unwrap_or(0))
                    }
                    Val::DateTime(_) => Val::Date(0),
                    o => o,
                },
                "dateformula" => match v {
                    Val::Text(s) => Val::DateFormula(s),
                    o => o,
                },
                // value types: the variable gets its own copy (crate::platobj)
                "notification" | "sessionsettings" | "webserviceactioncontext" => match v {
                    Val::Plat(p) => crate::platobj::copy(&p),
                    o => o,
                },
                "errorinfo" => match v {
                    Val::ErrorInfo(e) => crate::errinfo::copy(&e),
                    o => o,
                },
                _ => v,
            }},
            TypeRef::Option(m) => Val::Opt { ord: v.as_i64() as i32, enum_id: Some(crate::optmeta::for_members(m)) },
            TypeRef::Object { kind, target, .. } if kind == "enum" => match v {
                Val::Opt { ord, enum_id: Some(e) } if e >= -1 => Val::Opt { ord, enum_id: Some(e) },
                o => Val::Opt { ord: o.as_i64() as i32, enum_id: self.cat.id_of(ObjectKind::Enum, target) },
            },
            TypeRef::Label { .. } | TypeRef::TextConst(_) => Val::Text(v.to_text()),
            TypeRef::Object { kind, target, .. } if kind == "interface" => match v {
                Val::Opt { ord, enum_id: Some(eid) } if eid >= -1 => self.enum_implementation(eid, ord, target)?,
                Val::Object(i) => Val::Iface(Some(i)),
                o => o,
            },
            _ => v,
        })
    }

    /// Resolve the codeunit implementing `iface` for enum value `ord` of enum `eid`.
    pub fn enum_implementation(&mut self, eid: i64, ord: i32, iface: &str) -> R<Val> {
        let Some(e) = self.cat.enums.get(&eid) else { return Ok(Val::Iface(None)) };
        let pick = |pairs: &[(String, String)]| -> Option<String> {
            pairs
                .iter()
                .find(|(i, _)| i.eq_ignore_ascii_case(iface) || i.rsplit('.').next().is_some_and(|x| x.eq_ignore_ascii_case(iface)))
                .or_else(|| if pairs.len() == 1 { pairs.first() } else { None })
                .map(|(_, c)| c.clone())
        };
        let cu = e.by_ordinal(ord as i64).and_then(|v| pick(&v.implementations)).or_else(|| pick(&e.default_implementations));
        let Some(cu) = cu else {
            return err(format!("No implementation of interface '{iface}' for value {ord} of enum '{}'", e.name));
        };
        let id = self.cat.id_of(ObjectKind::Codeunit, &cu).or_else(|| self.cat.id_of(ObjectKind::Codeunit, cu.rsplit('.').next().unwrap_or(&cu)));
        match id.and_then(|id| self.cat.code_by_key.get(&(ObjectKind::Codeunit, id))) {
            Some(&ci) => {
                let inst = if self.code(ci).single_instance { self.single_instance(ci) } else { Instance::new(ci) };
                Ok(Val::Iface(Some(inst)))
            }
            None => err(format!("Implementation codeunit '{cu}' not found")),
        }
    }

    // ------------------------------------------------------------------ globals & frames

    pub fn globals_for(&mut self, code: u32) -> Rc<Globals> {
        let co = self.code(code);
        let mut vars = Vec::with_capacity(co.globals.len());
        for g in &co.globals {
            // labels are constants: one slot per declaration within the session (crate::frames::label_slot)
            let s = match &g.ty {
                TypeRef::Label { text, .. } | TypeRef::TextConst(text) => crate::frames::label_slot(self.id, &g.ty, text),
                _ => slot(self.default_val(&g.ty)),
            };
            vars.push(Var { name: g.name.as_str(), slot: s, ty: Some(&g.ty) });
        }
        let g = Rc::new(Globals { code, vars });
        self.made_globals.push(Rc::downgrade(&g));
        // drop the entries of globals that are gone now and then (the list stays small)
        if self.made_globals.len() >= 4096 && self.made_globals.len().is_power_of_two() {
            self.made_globals.retain(|w| w.strong_count() > 0);
        }
        g
    }

    pub fn instance_globals(&mut self, inst: &Rc<Instance>) -> Rc<Globals> {
        if let Some(g) = inst.globals.borrow().as_ref() {
            return g.clone();
        }
        let g = self.globals_for(inst.code);
        *inst.globals.borrow_mut() = Some(g.clone());
        // pages and codeunits with TableNo get an implicit Rec
        let co = self.code(inst.code);
        if inst.rec.borrow().is_none() {
            if let Some(t) = co.table {
                if matches!(co.kind, ObjectKind::Page | ObjectKind::PageExtension) {
                    // SourceTableTemporary = true: the page works on its own temporary table
                    let temp = self.cat.pages.get(&co.id).is_some_and(|p| p.props.get("sourcetabletemporary").is_some_and(|v| v.eq_ignore_ascii_case("true")));
                    let r = self.new_record(t, temp);
                    *inst.rec.borrow_mut() = Some(r);
                }
            }
        }
        // a report whose request page has a SourceTable: the request page triggers' Rec
        if inst.rec.borrow().is_none() && co.kind == ObjectKind::Report {
            let rp = self.cat.reports.get(&co.id).map(|rd| &rd.request_page);
            let t = rp.and_then(|p| p.props.get("sourcetable")).and_then(|n| self.cat.id_of(ObjectKind::Table, n.trim().trim_matches('"')));
            if let Some(t) = t.filter(|t| self.cat.tables.contains_key(t)) {
                let temp = rp.is_some_and(|p| p.props.get("sourcetabletemporary").is_some_and(|v| v.eq_ignore_ascii_case("true")));
                let r = self.new_record(t, temp);
                *inst.rec.borrow_mut() = Some(r);
            }
        }
        g
    }

    /// Globals of an extension object instance: its own variables plus the base object's
    /// (shared slots), so extension code sees the base's protected variables.
    pub fn bind_ext_globals(&mut self, ext: &Rc<Instance>, base: &Rc<Instance>) {
        let own = self.globals_for(ext.code);
        let base_g = self.instance_globals(base);
        let mut vars = own.vars.clone();
        vars.extend(base_g.vars.iter().cloned());
        *ext.globals.borrow_mut() = Some(Rc::new(Globals { code: ext.code, vars }));
        *ext.rec.borrow_mut() = base.rec.borrow().clone();
    }

    /// The instance of page extension `ext` for a page variable's instance, created on first use
    /// with the extension's globals bound to the page's (kept in `parts` under a reserved key).
    pub fn page_ext_instance(&mut self, base: &Rc<Instance>, ext: u32) -> Rc<Instance> {
        let key = format!("\u{0}ext:{ext}");
        if let Some((_, i)) = base.parts.borrow().iter().find(|(k, _)| **k == *key) {
            return i.clone();
        }
        let i = Instance::new(ext);
        self.bind_ext_globals(&i, base);
        base.parts.borrow_mut().push((key.into(), i.clone()));
        i
    }

    pub(crate) fn record_globals(&mut self, rec: &RecHandle, code: u32) -> Rc<Globals> {
        let tcode = rec.borrow().tdef.code;
        if code == tcode {
            if let Some(g) = rec.borrow().globals.clone() {
                return g;
            }
            let g = self.globals_for(code);
            rec.borrow_mut().globals = Some(g.clone());
            g
        } else {
            if let Some((_, g)) = rec.borrow().ext_globals.iter().find(|(c, _)| *c == code) {
                return g.clone();
            }
            // table extension code also sees the base table's (protected) globals
            let own = self.globals_for(code);
            let base = self.record_globals(rec, tcode);
            let mut vars = own.vars.clone();
            vars.extend(base.vars.iter().cloned());
            let g = Rc::new(Globals { code, vars });
            rec.borrow_mut().ext_globals.push((code, g.clone()));
            g
        }
    }

    // ------------------------------------------------------------------ invocation

    pub fn find_proc(&self, code: u32, name: &str, argc: usize) -> Option<u32> {
        let co = self.code(code);
        let cands = co.find(name);
        let mut best = None;
        for &i in cands {
            let p = &co.procs[i as usize];
            if p.kind == ProcKind::Trigger {
                continue;
            }
            if p.params.len() == argc {
                return Some(i);
            }
            if best.is_none() {
                best = Some(i);
            }
        }
        best
    }

    /// Find the best overload of `name` in `code` for the given argument expressions,
    /// evaluating each argument exactly once. Returns None if no procedure has that name/arity.
    pub fn resolve_call(&mut self, f: &mut Frame, code: u32, name: &str, args: &[Expr]) -> R<Option<(ProcRef, Vec<Arg>)>> {
        let co = self.code(code);
        let discard = self.discard;
        let cands: Vec<u32> = co
            .find(name)
            .iter()
            .copied()
            .filter(|&i| {
                let p = &co.procs[i as usize];
                p.kind != ProcKind::Trigger && p.params.len() == args.len()
            })
            .collect();
        if cands.is_empty() {
            return Ok(None);
        }
        self.discard = false;
        let mut evald: Vec<(Option<LVal>, Val)> = Vec::with_capacity(args.len());
        for a in args {
            evald.push(self.eval_arg(f, a)?);
        }
        let mut best = cands[0];
        if cands.len() > 1 {
            let mut best_score = i32::MIN;
            for &c in &cands {
                let p = &co.procs[c as usize];
                let sc: i32 = p
                    .params
                    .iter()
                    .zip(evald.iter())
                    .map(|(prm, (lv, v))| {
                        let declared = match lv {
                            Some(LVal::Slot(_, Some(t))) => Some(t.base_name()),
                            _ => None,
                        };
                        match declared {
                            // Codeunit "A" vs Codeunit "B" (and Page / Report / Enum ...) overloads
                            Some(_) if matches!(lv, Some(LVal::Slot(_, Some(t))) if self.object_type_differs(t, &prm.ty)) => -10,
                            Some(d) if d == prm.ty.base_name() && d != "record" => 10,
                            Some(d) if (d == "recordref") != (prm.ty.base_name() == "recordref") && prm.ty.base_name() != "variant" && matches!(v, Val::Record(_)) => -30,
                            _ => self.type_score(&prm.ty, v),
                        }
                    })
                    .sum();
                if sc > best_score {
                    best_score = sc;
                    best = c;
                }
            }
        }
        let p = &co.procs[best as usize];
        let mut out = Vec::with_capacity(args.len());
        for (prm, (lv, v)) in p.params.iter().zip(evald) {
            out.push(match (prm.by_ref, lv) {
                (true, Some(lv)) => Arg::Ref(lv),
                (true, None) => Arg::Ref(LVal::Slot(slot(v), None)),
                // DotNet variables are wrappers shared with the callee (see invoke)
                (false, Some(lv @ LVal::Slot(..))) if crate::dotnet::is_dotnet_param(&prm.ty) => Arg::Ref(lv),
                (false, _) => Arg::Val(v),
            });
        }
        self.pending_discard = discard;
        Ok(Some((ProcRef { code, proc: best }, out)))
    }

    pub(crate) fn eval_arg(&mut self, f: &mut Frame, e: &Expr) -> R<(Option<LVal>, Val)> {
        if let Some(n) = this_member(e) {
            if let Some(v) = f.globals.vars.iter().find(|v| v.name.eq_ignore_ascii_case(n)) {
                let lv = LVal::Slot(v.slot.clone(), v.ty);
                let val = self.read_lval(&lv);
                return Ok((Some(lv), val));
            }
        }
        let lvalue_able = match e {
            Expr::Ident(n) => {
                Self::find_var(f, n).is_some()
                    || f.with.iter().any(|r| r.borrow().tdef.field(n).is_some())
                    || f.rec.as_ref().is_some_and(|r| r.borrow().tdef.field(n).is_some())
            }
            Expr::Index(..) => true,
            _ => false,
        };
        if lvalue_able {
            let lv = self.lvalue(f, e)?;
            let v = self.read_lval(&lv);
            return Ok((Some(lv), v));
        }
        // Database::Microsoft.Sales."Sales Line" and other namespace-qualified ids are values
        if let Expr::Member(base, _) = e {
            if scoped_object_path(base).is_some() {
                return Ok((None, self.eval(f, e)?));
            }
        }
        if let Expr::Member(base, name) = e {
            let b = self.eval(f, base)?;
            if let Val::Record(r) = &b {
                let fi = r.borrow().tdef.field(name);
                if let Some(i) = fi {
                    let lv = LVal::Field(r.clone(), i);
                    let v = self.read_lval(&lv);
                    return Ok((Some(lv), v));
                }
            }
            let v = match b {
                Val::Record(r) => self.record_method(f, &r, name, &[])?,
                Val::Object(i) | Val::Iface(Some(i)) => self.object_method(f, &i, name, &[])?,
                Val::TestPage(tp) => self.testpage_member(f, &tp, name, None)?,
                Val::TestCtl(tp, c) => self.testctl_method(f, &tp, &c, name, &[])?,
                other => self.value_method(f, other, base, name, &[])?,
            };
            return Ok((None, v));
        }
        Ok((None, self.eval(f, e)?))
    }

    /// Two object types of the same kind (not Record) with different objects:
    /// `Codeunit "Price Asset List"` vs `Codeunit "Price Source List"`.
    fn object_type_differs(&self, a: &TypeRef, b: &TypeRef) -> bool {
        // List of [Integer] vs List of [Text] overloads (Dim Correct Selection Criteria.GetDimensionSetIds)
        match (a, b) {
            (TypeRef::List(x), TypeRef::List(y)) => return x.base_name() != y.base_name() || self.object_type_differs(x, y),
            (TypeRef::Dictionary(k1, v1), TypeRef::Dictionary(k2, v2)) => {
                return k1.base_name() != k2.base_name() || v1.base_name() != v2.base_name() || self.object_type_differs(v1, v2)
            }
            _ => {}
        }
        let (TypeRef::Object { kind: k1, target: t1, .. }, TypeRef::Object { kind: k2, target: t2, .. }) = (a, b) else { return false };
        if !k1.eq_ignore_ascii_case(k2) || k1.eq_ignore_ascii_case("record") {
            return false;
        }
        let k = match k1.to_ascii_lowercase().as_str() {
            "codeunit" => Some(ObjectKind::Codeunit),
            "page" => Some(ObjectKind::Page),
            "report" => Some(ObjectKind::Report),
            "query" => Some(ObjectKind::Query),
            "xmlport" => Some(ObjectKind::XmlPort),
            "enum" => Some(ObjectKind::Enum),
            _ => None,
        };
        match k.map(|k| (self.cat.id_of(k, t1), self.cat.id_of(k, t2))) {
            Some((Some(x), Some(y))) => x != y,
            _ => !t1.eq_ignore_ascii_case(t2),
        }
    }

    fn type_score(&self, ty: &TypeRef, v: &Val) -> i32 {
        let base = ty.base_name();
        match (base.as_str(), v) {
            ("record", Val::Record(r)) => {
                if let TypeRef::Object { target, .. } = ty {
                    if self.cat.id_of(ObjectKind::Table, target) == Some(r.borrow().tdef.id) {
                        return 6;
                    }
                }
                3
            }
            ("recordref", Val::Record(_)) => 2,
            ("variant", _) => 1,
            ("record", _) | (_, Val::Record(_)) => -20,
            ("code" | "text" | "label" | "bigtext" | "secrettext", Val::Text(_) | Val::Char(_)) => 4,
            ("integer" | "biginteger" | "decimal" | "byte", v) if v.is_numeric() => 4,
            ("boolean", Val::Bool(_)) => 4,
            ("date", Val::Date(_)) | ("time", Val::Time(_)) | ("datetime", Val::DateTime(_)) | ("guid", Val::Guid(_)) => 4,
            ("enum", Val::Opt { enum_id: Some(e), .. }) if *e >= -1 => 4,
            ("option", Val::Opt { .. }) => 3,
            ("list", Val::List(_)) | ("dictionary", Val::Dict(_)) | ("array", Val::Array(..)) => 4,
            ("interface", Val::Iface(_) | Val::Object(_)) => 3,
            // the argument's object must be the parameter's (overloads per codeunit type)
            ("codeunit" | "page" | "report" | "query" | "xmlport", Val::Object(i)) => {
                let k = match base.as_str() {
                    "codeunit" => ObjectKind::Codeunit,
                    "page" => ObjectKind::Page,
                    "report" => ObjectKind::Report,
                    "query" => ObjectKind::Query,
                    _ => ObjectKind::XmlPort,
                };
                match ty {
                    TypeRef::Object { target, .. } => match self.cat.id_of(k, target) {
                        Some(id) if id == self.code(i.code).id => 6,
                        Some(_) => -10,
                        None => 4,
                    },
                    _ => 4,
                }
            }
            ("testpage", Val::TestPage(_)) => 4,
            ("variant", _) => 1,
            ("errorinfo", Val::ErrorInfo(_)) => 4,
            ("recordid", Val::RecordId(..)) => 4,
            ("integer" | "biginteger" | "decimal", Val::Opt { .. }) => 1,
            (b, Val::Net(n)) => crate::dotnet::type_score(b, n),
            ("instream", Val::InStream(_)) | ("outstream", Val::OutStream(_)) => 4,
            (p @ ("jsonobject" | "jsonarray" | "jsonvalue"), Val::Json(_)) => {
                if v.type_name().eq_ignore_ascii_case(p) {
                    3
                } else {
                    -10
                }
            }
            ("jsontoken", Val::Json(_)) => 4,
            _ => -5,
        }
    }

    /// Run `g` inside an error trap (SysState::traps_open; see recops rec_validate_x).
    pub fn trapped<T>(&mut self, g: impl FnOnce(&mut Self) -> T) -> T {
        self.sys.traps_open += 1;
        let r = g(self);
        self.sys.traps_open -= 1;
        r
    }

    /// Invoke a procedure. `rec`/`xrec` bind the implicit record for table code.
    pub fn invoke(&mut self, pr: ProcRef, args: Vec<Arg>, inst: Option<Rc<Instance>>, rec: Option<RecHandle>, xrec: Option<RecHandle>) -> R<Val> {
        let co = self.code(pr.code);
        let p: &'static Procedure = &co.procs[pr.proc as usize];
        let ret_true = std::mem::take(&mut self.ret_true);
        // events: dispatch to subscribers instead of running the (empty) body
        if p.attributes.iter().any(|a| {
            let n = &a.name;
            n.eq_ignore_ascii_case("integrationevent") || n.eq_ignore_ascii_case("businessevent") || n.eq_ignore_ascii_case("internalevent")
        }) {
            let (kind, id) = match co.kind {
                ObjectKind::TableExtension | ObjectKind::Table => (ObjectKind::Table, co.table.unwrap_or(co.id)),
                ObjectKind::PageExtension => (ObjectKind::Page, co.extends.map(|e| e.1).unwrap_or(co.id)),
                k => (k, co.id),
            };
            // a [TryFunction] publisher whose result is used: the caller's try (BC TryInvoke at the
            // call site) catches what the subscribers raise; true when none raised an error
            let try_used = !std::mem::take(&mut self.pending_discard) && p.attributes.iter().any(|a| a.name.eq_ignore_ascii_case("tryfunction"));
            // no subscribers (static per catalog): raise_event_ex would find none and return
            if !crate::frames::event_has_subscribers(self.cat, pr, kind, id, &p.name) {
                return Ok(if try_used { Val::Bool(true) } else { Val::Void });
            }
            let named: Vec<(&str, Arg)> = p.params.iter().map(|pp| pp.name.as_str()).zip(args).collect();
            // `sender` is the publishing object: the record for table events, otherwise the
            // instance (not a codeunit's TableNo record)
            let sender_rec = if kind == ObjectKind::Table { rec.clone() } else { None };
            if try_used {
                return match self.trapped(|s| s.raise_event_ex(kind, id, &p.name, "", named, inst.clone(), sender_rec, crate::events::is_isolated(p))) {
                    Ok(()) => Ok(Val::Bool(true)),
                    Err(Flow::Err(e)) => {
                        self.note_error(&e);
                        Ok(Val::Bool(false))
                    }
                    Err(o) => Err(o),
                };
            }
            self.raise_event_ex(kind, id, &p.name, "", named, inst.clone(), sender_rec, crate::events::is_isolated(p))?;
            return Ok(Val::Void);
        }
        let discard = std::mem::take(&mut self.pending_discard);
        self.discard = false;
        if !discard && p.attributes.iter().any(|a| a.name.eq_ignore_ascii_case("tryfunction")) {
            // [TryFunction] whose result is used: errors become `false`
            self.pending_discard = true; // run the body normally
            return match self.trapped(|s| s.invoke(pr, args, inst, rec, xrec)) {
                Ok(_) => Ok(Val::Bool(true)),
                Err(Flow::Err(e)) => {
                    self.note_error(&e);
                    Ok(Val::Bool(false))
                }
                Err(o) => Err(o),
            };
        }
        self.depth += 1;
        if self.depth > 200 || crate::stackguard::exhausted(self.depth) {
            // the native stack guard (crate::stackguard) fires first only if a call level costs
            // more native stack than expected; it turns a would-be overflow into this AL error
            let native = self.depth <= 200;
            self.depth -= 1;
            return err(if native { "Maximum call depth exceeded (native stack three-quarters full)" } else { "Maximum call depth exceeded (recursion?)" });
        }
        let globals = match (&inst, &rec) {
            (Some(i), _) => self.instance_globals(i),
            (None, Some(r)) if matches!(co.kind, ObjectKind::Table | ObjectKind::TableExtension) => self.record_globals(r, pr.code),
            _ => self.globals_for(pr.code),
        };
        let rec = match (&rec, &inst) {
            (Some(r), _) => Some(r.clone()),
            (None, Some(i)) => i.rec.borrow().clone(),
            _ => None,
        };
        let mut locals = Vec::with_capacity(p.params.len() + p.vars.len() + 1);
        let mut writebacks: Vec<(Slot, LVal)> = vec![];
        let mut args = args.into_iter();
        for prm in &p.params {
            let a = args.next();
            let s = match a {
                Some(Arg::Ref(LVal::Slot(s, _))) if prm.by_ref => s,
                // a DotNet parameter shares the caller's variable: assigning it in the procedure
                // changes the caller's variable, as BC's DotNet wrappers do
                Some(Arg::Ref(LVal::Slot(s, _))) if crate::dotnet::is_dotnet_param(&prm.ty) => s,
                Some(Arg::Ref(lv)) if prm.by_ref => {
                    let v = self.read_lval(&lv);
                    let s = slot(v);
                    writebacks.push((s.clone(), lv));
                    s
                }
                Some(Arg::Ref(lv)) => {
                    let v = self.read_lval(&lv);
                    if shares_by_value(&prm.ty) && matches!(v, Val::Plat(_)) {
                        slot(v)
                    } else {
                        let v = if is_reference_type(&prm.ty) { self.as_recordref(v) } else { by_value(v.deep_copy(), &prm.ty) };
                        slot(self.coerce_param(v, &prm.ty)?)
                    }
                }
                Some(Arg::Val(v)) if !prm.by_ref && shares_by_value(&prm.ty) && matches!(v, Val::Plat(_)) => slot(v),
                Some(Arg::Val(v)) => {
                    let v = if prm.by_ref {
                        v
                    } else if is_reference_type(&prm.ty) {
                        self.as_recordref(v)
                    } else {
                        by_value(v.deep_copy(), &prm.ty)
                    };
                    slot(self.coerce_param(v, &prm.ty)?)
                }
                None => slot(self.default_val(&prm.ty)),
            };
            locals.push(Var { name: prm.name.as_str(), slot: s, ty: Some(&prm.ty) });
        }
        for v in &p.vars {
            let d = self.default_val(&v.ty);
            locals.push(Var { name: v.name.as_str(), slot: slot(d), ty: Some(&v.ty) });
        }
        let ret = p.ret.as_ref().map(|t| slot(if ret_true { Val::Bool(true) } else { self.default_val(t) }));
        if let (Some(n), Some(r)) = (&p.ret_name, &ret) {
            locals.push(Var { name: n.as_str(), slot: r.clone(), ty: p.ret.as_ref() });
        }
        let mut f = Frame {
            code: pr.code,
            locals,
            globals,
            rec,
            xrec,
            with: vec![],
            inst,
            ret,
            ret_ty: p.ret.as_ref(),
            curr_field: 0,
        };
        self.call_stack.push(pr.code);
        self.frame_lines.push((pr.proc, self.cur_line));
        // the running procedure, only while permissions can be enforced (a method's
        // [InherentPermissions], crate::permissions); free otherwise
        let track_proc = crate::permissions::enforce();
        if track_proc {
            self.proc_stack.push(pr.proc);
        }
        let page_rec = match &f.rec {
            Some(r) if matches!(self.code(pr.code).kind, ObjectKind::Page | ObjectKind::PageExtension) => Some(r.clone()),
            _ => None,
        };
        if let Some(r) = &page_rec {
            self.page_recs.push(r.clone());
        }
        // [ErrorBehavior(ErrorBehavior::Collect)]: a scope for collectible errors (crate::errinfo)
        let collect = if crate::errinfo::collects(p) { Some(self.start_collecting()) } else { None };
        let res = match &p.body {
            // the compiled engine (crate::compiled), the default; BC_ENGINE=tree runs exec_block
            Some(b) if crate::compiled::enabled() => self.exec_body_compiled(&mut f, pr, b),
            Some(b) => self.exec_block(&mut f, b),
            None => Ok(()),
        };
        let res = match collect {
            Some(prev) => {
                let ok = !matches!(res, Err(Flow::Err(_)));
                match self.stop_collecting(ok, prev) {
                    Err(e) if ok => Err(e),
                    _ => res,
                }
            }
            None => res,
        };
        self.call_stack.pop();
        if let Some((_, l)) = self.frame_lines.pop() {
            self.cur_line = l;
        }
        if track_proc {
            self.proc_stack.pop();
        }
        if page_rec.is_some() {
            self.page_recs.pop();
        }
        self.depth -= 1;
        for (s, lv) in writebacks {
            let v = s.borrow().clone();
            self.write_lval(&lv, v)?;
        }
        // AL_TRACE_PROCS=Name1,Name2: print each call of those procedures (any object) with its result
        static TRACE_PROCS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        let traced = TRACE_PROCS.get_or_init(|| std::env::var("AL_TRACE_PROCS").map(|s| s.split(',').map(|x| x.trim().to_ascii_lowercase()).filter(|x| !x.is_empty()).collect()).unwrap_or_default());
        if !traced.is_empty() && traced.iter().any(|n| n.eq_ignore_ascii_case(&p.name)) {
            let out = match &res {
                Ok(()) | Err(Flow::Exit) => f.ret.as_ref().map(|r| r.borrow().to_text()).unwrap_or_default(),
                Err(Flow::Err(e)) => format!("error: {}", e.msg.lines().next().unwrap_or("")),
                _ => String::new(),
            };
            let on = f.rec.as_ref().map(|r| r.borrow().pk_text()).unwrap_or_default();
            eprintln!("  proc {}::{} [{on}] -> {out}", co.name, p.name);
            // AL_TRACE_ROW=1: also the implicit Rec's fields that differ from their initial value
            if std::env::var_os("AL_TRACE_ROW").is_some() {
                if let Some(r) = &f.rec {
                    let rb = r.borrow();
                    let blank = rb.schema.new_row();
                    let set: Vec<String> = rb.row.iter().zip(blank.iter()).enumerate().filter(|(_, (a, b))| a != b).map(|(i, _)| format!("{}={}", rb.tdef.fields[i].name, rb.get_val(i).to_text())).collect();
                    eprintln!("    row: {}", set.join(", "));
                }
            }
        }
        match res {
            Ok(()) | Err(Flow::Exit) => Ok(f.ret.map(|r| r.borrow().clone()).unwrap_or(Val::Void)),
            Err(Flow::Break) | Err(Flow::Continue) => Ok(Val::Void),
            Err(Flow::Err(mut e)) => {
                if e.stack.len() < 30 {
                    e.stack.push(format!("{} \"{}\"({})", co.kind.as_str(), co.name, p.name));
                }
                Err(Flow::Err(e))
            }
        }
    }

    fn coerce_param(&mut self, v: Val, ty: &'static TypeRef) -> R<Val> {
        match (ty, &v) {
            (TypeRef::Object { kind, .. }, Val::Record(_)) if kind == "record" => Ok(v),
            (TypeRef::Simple { name, .. }, _) if crate::frames::simple_kind(ty, name) == crate::frames::SimpleKind::Variant => Ok(v),
            (TypeRef::Simple { name, .. }, _) if matches!(crate::frames::simple_kind(ty, name), crate::frames::SimpleKind::Text | crate::frames::SimpleKind::Code) => {
                // parameters are not length checked at runtime in BC (truncation errors happen on assignment)
                let s = v.to_text();
                Ok(Val::Text(if crate::frames::simple_kind(ty, name) == crate::frames::SimpleKind::Code { s.trim().to_uppercase() } else { s }))
            }
            _ => self.coerce(v, Some(ty)),
        }
    }

    /// Raise an event to all subscribers. Parameters are matched by name.
    pub fn raise_event(
        &mut self,
        kind: ObjectKind,
        id: i64,
        event: &str,
        element: &str,
        args: Vec<(&str, Arg)>,
        sender: Option<Rc<Instance>>,
        sender_rec: Option<RecHandle>,
    ) -> R<()> {
        self.raise_event_ex(kind, id, event, element, args, sender, sender_rec, false)
    }

    /// [`Session::raise_event`]; `isolated`: an isolated event, each subscriber in its own
    /// transaction (crate::events).
    #[allow(clippy::too_many_arguments)]
    pub fn raise_event_ex(
        &mut self,
        kind: ObjectKind,
        id: i64,
        event: &str,
        element: &str,
        args: Vec<(&str, Arg)>,
        sender: Option<Rc<Instance>>,
        sender_rec: Option<RecHandle>,
        isolated: bool,
    ) -> R<()> {
        let key = (kind, id, event.to_ascii_lowercase(), element.to_ascii_lowercase());
        let Some(subs) = self.cat.subscribers.get(&key) else { return Ok(()) };
        let isolated = isolated && self.isolation_applies()?;
        // materialize args as slots so all subscribers share by-ref state
        let mut shared: Vec<(&str, Slot, Option<LVal>)> = vec![];
        for (n, a) in args {
            match a {
                Arg::Val(v) => shared.push((n, slot(v), None)),
                Arg::Ref(LVal::Slot(s, _)) => shared.push((n, s, None)),
                Arg::Ref(lv) => {
                    let v = self.read_lval(&lv);
                    shared.push((n, slot(v), Some(lv)));
                }
            }
        }
        for sub in subs {
            let co = self.code(sub.proc.code);
            let p = &co.procs[sub.proc.proc as usize];
            let mut call_args = vec![];
            for prm in &p.params {
                let found = shared.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(&prm.name));
                match found {
                    Some((_, s, _)) => call_args.push(Arg::Ref(LVal::Slot(s.clone(), None))),
                    None if prm.name.eq_ignore_ascii_case("sender") => match (&sender_rec, &sender) {
                        (Some(r), _) => call_args.push(Arg::Val(Val::Record(r.clone()))),
                        (None, Some(i)) => call_args.push(Arg::Val(Val::Object(i.clone()))),
                        _ => call_args.push(Arg::Val(Val::Void)),
                    },
                    None => call_args.push(Arg::Val(self.default_val(&prm.ty))),
                }
            }
            // Manual subscribers fire only for bound instances; subscribers in test codeunits only
            // while that test codeunit runs (on its instance); others get a new or single instance.
            let insts: Vec<Rc<Instance>> = if co.manual_subscriber {
                self.bound.iter().filter_map(|w| w.upgrade()).filter(|i| i.code == sub.proc.code).collect()
            } else if co.subtype.as_deref().is_some_and(|t| t.eq_ignore_ascii_case("test")) {
                match &self.test {
                    Some(t) if t.inst.code == sub.proc.code => vec![t.inst.clone()],
                    _ => vec![],
                }
            } else if co.single_instance {
                vec![self.single_instance(sub.proc.code)]
            } else {
                vec![Instance::new(sub.proc.code)]
            };
            if insts.is_empty() {
                continue;
            }
            if self.trace {
                eprintln!("  event {event} -> {}::{}", co.name, p.name);
            }
            let n = insts.len();
            for (k, inst) in insts.into_iter().enumerate() {
                let a = if k + 1 == n { std::mem::take(&mut call_args) } else { call_args.iter().map(clone_arg).collect() };
                // the dispatcher calls a subscriber as a statement: a [TryFunction] subscriber's
                // error is not turned into `false` (BC: no TryInvoke around subscriber calls)
                self.pending_discard = true;
                if isolated {
                    self.invoke_isolated(sub.proc, a, inst)?;
                } else {
                    self.invoke(sub.proc, a, Some(inst), None, None)?;
                }
            }
        }
        for (_, s, lv) in shared {
            if let Some(lv) = lv {
                let v = s.borrow().clone();
                self.write_lval(&lv, v)?;
            }
        }
        Ok(())
    }

    /// Run a codeunit's OnRun trigger.
    pub fn run_codeunit(&mut self, id: i64, rec: Option<RecHandle>) -> R<()> {
        let Some(&ci) = self.cat.code_by_key.get(&(ObjectKind::Codeunit, id)) else {
            return err(format!("Codeunit {id} does not exist"));
        };
        let co = self.code(ci);
        let inst = if co.single_instance { self.single_instance(ci) } else { Instance::new(ci) };
        // a TableNo codeunit run without a record gets an empty record of its table
        let rec = match (rec, co.table) {
            (None, Some(t)) if self.cat.tables.contains_key(&t) => Some(self.new_record(t, false)),
            (r, _) => r,
        };
        if let Some(r) = &rec {
            *inst.rec.borrow_mut() = Some(r.clone());
        }
        if let Some(t) = co.trigger("onrun") {
            self.invoke(ProcRef { code: ci, proc: t }, vec![], Some(inst), rec, None)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------ lvalues

    pub fn read_lval(&self, lv: &LVal) -> Val {
        match lv {
            LVal::Slot(s, _) => s.borrow().clone(),
            LVal::Field(r, i) => r.borrow().get_val(*i),
            LVal::Elem(a, i, _) => a.borrow().get(*i).cloned().unwrap_or(Val::Void),
            LVal::ErrorMsg(m, p) => crate::errinfo::get_prop(m, p).unwrap_or(Val::Void),
            LVal::TextChar(base, i) => Val::Char(self.read_lval(base).to_text().chars().nth(*i).unwrap_or('\0')),
            LVal::Row(a, start, dims) => array_row(a, *start, dims),
            LVal::RecProp(r, p) => match *p {
                "filtergroup" => Val::Int(r.borrow().filter_group),
                "currfieldno" => Val::Int(self.curr_field_no(r)),
                "ascending" => Val::Bool(r.borrow().ascending),
                _ => Val::Void,
            },
            LVal::SessionProp("workdate") => Val::Date(self.work_date),
            LVal::SessionProp("currenttransactiontype") => Val::Opt { ord: self.txn().ty, enum_id: None },
            LVal::SessionProp(_) => Val::Int(self.global_language),
            LVal::NetProp(..) => Val::Void,
        }
    }

    pub fn write_lval(&mut self, lv: &LVal, v: Val) -> R<()> {
        match lv {
            LVal::Slot(s, Some(t)) if is_reference_type(t) && matches!(v, Val::Record(_)) => {
                let v = self.as_recordref(v);
                *s.borrow_mut() = v;
                Ok(())
            }
            LVal::Slot(s, ty) => {
                // a Record variable takes the fields of an assigned record; a Variant that holds a
                // record is replaced by the assigned value (Variant := RecordRef in
                // Workflow Response Handling.ReleaseDocument)
                let is_variant = matches!(ty, Some(TypeRef::Simple { name, .. }) if name.eq_ignore_ascii_case("variant"));
                let cur_is_rec = !is_variant && matches!(&*s.borrow(), Val::Record(_));
                if cur_is_rec {
                    if let Val::Record(src) = &v {
                        let cur = match &*s.borrow() {
                            Val::Record(c) => c.clone(),
                            _ => unreachable!(),
                        };
                        if !Rc::ptr_eq(&cur, src) {
                            let row = src.borrow().row.clone();
                            let same = cur.borrow().tdef.id == src.borrow().tdef.id;
                            if same {
                                cur.borrow_mut().row = row;
                            }
                        }
                        return Ok(());
                    }
                }
                let v = self.coerce(v, *ty)?;
                *s.borrow_mut() = v;
                Ok(())
            }
            LVal::Field(r, i) => {
                let f = r.borrow().field(*i);
                let sv = to_store(f, &v).map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
                r.borrow_mut().row[*i] = sv;
                Ok(())
            }
            LVal::Elem(a, i, ty) => {
                // a Record element takes the fields of an assigned record, like a Record variable;
                // Variant and RecordRef elements are replaced
                let replace = ty.is_some_and(|t| is_reference_type(t) || matches!(t, TypeRef::Simple { name, .. } if name.eq_ignore_ascii_case("variant")));
                if let (false, Val::Record(src)) = (replace, &v) {
                    let cur = match a.borrow().get(*i) {
                        Some(Val::Record(c)) => Some(c.clone()),
                        _ => None,
                    };
                    if let Some(cur) = cur {
                        if !Rc::ptr_eq(&cur, src) && cur.borrow().tdef.id == src.borrow().tdef.id {
                            let row = src.borrow().row.clone();
                            cur.borrow_mut().row = row;
                        }
                        return Ok(());
                    }
                }
                let v = self.coerce(v, *ty)?;
                let mut a = a.borrow_mut();
                if *i >= a.len() {
                    return err(format!("Array index {} is outside the bounds of the array", i + 1));
                }
                a[*i] = v;
                Ok(())
            }
            LVal::ErrorMsg(m, p) => {
                crate::errinfo::set_prop(m, p, &v);
                Ok(())
            }
            LVal::RecProp(r, p) => {
                if *p == "filtergroup" {
                    r.borrow_mut().filter_group = v.as_i64() as i32;
                } else if *p == "currfieldno" {
                    self.set_curr_field_no(r, v.as_i64() as i32);
                } else if *p == "ascending" {
                    r.borrow_mut().ascending = v.as_bool();
                } else if *p == "readisolation" {
                    // Rec.ReadIsolation := IsolationLevel::UpdLock (crate::recops rec_read_isolation)
                    self.rec_read_isolation(r, &v)?;
                }
                Ok(())
            }
            LVal::SessionProp("workdate") => {
                // WorkDate := d (as WorkDate(d))
                if let Val::Date(d) = v {
                    self.work_date = d;
                }
                Ok(())
            }
            // CurrentTransactionType := t (as CurrentTransactionType(t))
            LVal::SessionProp("currenttransactiontype") => self.set_transaction_type(v.as_i64() as i32, false),
            LVal::SessionProp(_) => {
                self.global_language = v.as_i64() as i32;
                Ok(())
            }
            LVal::NetProp(n, name) => self.net_set_prop(n, name, v),
            LVal::Row(a, start, dims) => {
                if let Val::Array(src, _) = v {
                    let n: usize = dims.iter().product();
                    let src = src.borrow();
                    let mut dst = a.borrow_mut();
                    for (k, x) in src.iter().take(n).enumerate() {
                        if let Some(d) = dst.get_mut(start + k) {
                            *d = x.clone();
                        }
                    }
                }
                Ok(())
            }
            LVal::TextChar(base, i) => {
                let c = match v {
                    Val::Char(c) => c,
                    Val::Text(t) => t.chars().next().unwrap_or(' '),
                    n => char::from_u32(n.as_i64() as u32).unwrap_or('\0'),
                };
                let mut chars: Vec<char> = self.read_lval(base).to_text().chars().collect();
                while chars.len() <= *i {
                    chars.push(' ');
                }
                chars[*i] = c;
                self.write_lval(base, Val::Text(chars.into_iter().collect()))
            }
        }
    }

    fn find_var<'f>(f: &'f Frame, name: &str) -> Option<&'f Var> {
        f.locals.iter().rev().find(|v| v.name.eq_ignore_ascii_case(name)).or_else(|| f.globals.vars.iter().find(|v| v.name.eq_ignore_ascii_case(name)))
    }

    pub fn lvalue(&mut self, f: &mut Frame, e: &Expr) -> R<LVal> {
        match e {
            Expr::Ident(n) => {
                if let Some(v) = Self::find_var(f, n) {
                    return Ok(LVal::Slot(v.slot.clone(), v.ty));
                }
                for r in f.with.iter().rev() {
                    if let Some(i) = r.borrow().tdef.field(n) {
                        return Ok(LVal::Field(r.clone(), i));
                    }
                }
                if let Some(r) = &f.rec {
                    if let Some(i) = r.borrow().tdef.field(n) {
                        return Ok(LVal::Field(r.clone(), i));
                    }
                }
                if n.eq_ignore_ascii_case("rec") {
                    if let Some(r) = &f.rec {
                        return Ok(LVal::Slot(slot(Val::Record(r.clone())), None));
                    }
                }
                if n.eq_ignore_ascii_case("xrec") {
                    let x = self.xrec_of(f);
                    return Ok(LVal::Slot(slot(Val::Record(x)), None));
                }
                if n.eq_ignore_ascii_case("globallanguage") {
                    return Ok(LVal::SessionProp("globallanguage"));
                }
                if n.eq_ignore_ascii_case("workdate") {
                    return Ok(LVal::SessionProp("workdate"));
                }
                if n.eq_ignore_ascii_case("currenttransactiontype") {
                    return Ok(LVal::SessionProp("currenttransactiontype"));
                }
                // properties of the implicit Rec: `CurrFieldNo := FieldNo(...)` (crate::currfield),
                // `FilterGroup := 2`, `ReadIsolation := IsolationLevel::UpdLock`
                if let Some(r) = &f.rec {
                    match n.to_ascii_lowercase().as_str() {
                        "currfieldno" => return Ok(LVal::RecProp(r.clone(), "currfieldno")),
                        "filtergroup" => return Ok(LVal::RecProp(r.clone(), "filtergroup")),
                        "ascending" => return Ok(LVal::RecProp(r.clone(), "ascending")),
                        "readisolation" => return Ok(LVal::RecProp(r.clone(), "readisolation")),
                        "securityfiltering" => return Ok(LVal::RecProp(r.clone(), "other")),
                        _ => {}
                    }
                }
                err(format!("Unknown variable '{n}'"))
            }
            Expr::Member(base, name) if matches!(**base, Expr::This) && f.globals.vars.iter().any(|v| v.name.eq_ignore_ascii_case(name)) => {
                let v = f.globals.vars.iter().find(|v| v.name.eq_ignore_ascii_case(name)).unwrap();
                Ok(LVal::Slot(v.slot.clone(), v.ty))
            }
            Expr::Member(base, name) => {
                let b = self.eval(f, base)?;
                match b {
                    Val::Record(r) => {
                        let fi = r.borrow().tdef.field(name);
                        match fi {
                            Some(i) => Ok(LVal::Field(r, i)),
                            // record properties: Rec.FilterGroup := 2, Rec.ReadIsolation := ..., Rec.SecurityFiltering := ...
                            None if name.eq_ignore_ascii_case("filtergroup") => Ok(LVal::RecProp(r, "filtergroup")),
                            // Rec.Ascending := false (as Rec.Ascending(false))
                            None if name.eq_ignore_ascii_case("ascending") => Ok(LVal::RecProp(r, "ascending")),
                            None if name.eq_ignore_ascii_case("readisolation") => Ok(LVal::RecProp(r, "readisolation")),
                            None if ["readisolation", "securityfiltering"].contains(&name.to_ascii_lowercase().as_str()) => {
                                Ok(LVal::RecProp(r, "other"))
                            }
                            None => err(format!("Field '{name}' does not exist in table '{}'", r.borrow().tdef.name)),
                        }
                    }
                    Val::ErrorInfo(ei) => Ok(LVal::ErrorMsg(ei, name.to_ascii_lowercase())),
                    Val::FieldRef(h, fi) if name.eq_ignore_ascii_case("value") => match crate::record::stale_fieldref(&h, fi) {
                        Some(m) => err(m),
                        None => Ok(LVal::Field(h, fi)),
                    },
                    Val::Net(n) => Ok(LVal::NetProp(n, name.clone())),
                    // Report.UseRequestPage := false (property assignment on a report variable / CurrReport)
                    ref o if name.eq_ignore_ascii_case("userequestpage") && self.report_use_request_page_slot(f, o).is_some() => {
                        Ok(LVal::Slot(self.report_use_request_page_slot(f, o).unwrap(), None))
                    }
                    _ => {
                        let v = self.eval(f, e)?;
                        Ok(LVal::Slot(slot(v), None))
                    }
                }
            }
            Expr::Index(base, idx) => {
                // A[i] [j] := X: an element of a multi-dimensional array written as two index groups
                if let Expr::Index(b2, i2) = &**base {
                    if let Expr::Ident(n) = &**b2 {
                        let var = Self::find_var(f, n).map(|v| (v.slot.borrow().clone(), v.ty));
                        if let Some((Val::Array(a, dims), ty)) = var {
                            if dims.len() == i2.len() + idx.len() {
                                let all: Vec<Expr> = i2.iter().chain(idx.iter()).cloned().collect();
                                let i = self.array_index(f, &all, &dims)?;
                                let elem_ty = match ty {
                                    Some(TypeRef::Array { elem, .. }) => Some(&**elem),
                                    _ => None,
                                };
                                return Ok(LVal::Elem(a, i, elem_ty));
                            }
                        }
                    }
                }
                let b = self.eval(f, base)?;
                let ty = match &**base {
                    Expr::Ident(n) => Self::find_var(f, n).and_then(|v| v.ty),
                    _ => None,
                };
                let elem_ty = match ty {
                    Some(TypeRef::Array { elem, .. }) => Some(&**elem),
                    _ => None,
                };
                match b {
                    // fewer indices than dimensions: a row
                    Val::Array(a, dims) if idx.len() < dims.len() => {
                        let (start, rest) = self.array_row_start(f, idx, &dims)?;
                        Ok(LVal::Row(a, start, rest))
                    }
                    Val::Array(a, dims) => {
                        let i = self.array_index(f, idx, &dims)?;
                        Ok(LVal::Elem(a, i, elem_ty))
                    }
                    Val::Text(_) => {
                        let i = self.eval(f, &idx[0])?.as_i64();
                        if i < 1 {
                            return err(format!("Index {i} is outside the bounds of the text"));
                        }
                        let base_lv = self.lvalue(f, base)?;
                        Ok(LVal::TextChar(Box::new(base_lv), i as usize - 1))
                    }
                    _ => {
                        let v = self.eval(f, e)?;
                        Ok(LVal::Slot(slot(v), None))
                    }
                }
            }
            _ => {
                let v = self.eval(f, e)?;
                Ok(LVal::Slot(slot(v), None))
            }
        }
    }

    /// `A[i]` with fewer indices than `A` has dimensions: the row's first flat element and the
    /// remaining dimensions.
    fn array_row_start(&mut self, f: &mut Frame, idx: &[Expr], dims: &[usize]) -> R<(usize, Vec<usize>)> {
        let rest = dims[idx.len()..].to_vec();
        let n: usize = rest.iter().product();
        Ok((self.array_index(f, idx, &dims[..idx.len()])? * n, rest))
    }

    fn array_index(&mut self, f: &mut Frame, idx: &[Expr], dims: &[usize]) -> R<usize> {
        let mut flat = 0usize;
        for (k, ie) in idx.iter().enumerate() {
            let i = self.eval(f, ie)?.as_i64();
            let d = dims.get(k).copied().unwrap_or(1);
            if i < 1 || i as usize > d {
                return err(format!("Array index {i} is outside the bounds of the array"));
            }
            flat = flat * d + (i as usize - 1);
        }
        Ok(flat)
    }

    pub fn xrec_of(&mut self, f: &mut Frame) -> RecHandle {
        if let Some(x) = &f.xrec {
            return x.clone();
        }
        let r = f.rec.clone().expect("xRec outside record context");
        // BC: xRec is the record's OldRecord, which an operation running on it has assigned
        if let Some(x) = self.sys.validate_x.iter().rev().find(|(h, _)| Rc::ptr_eq(h, &r)).map(|(_, x)| x.clone()) {
            f.xrec = Some(x.clone());
            return x;
        }
        let x = copy_record(&r);
        let xr = r.borrow().xrow.clone();
        // a page on a new record it started: the page's own xRec
        let is_page = matches!(self.code(f.code).kind, ObjectKind::Page | ObjectKind::PageExtension);
        let page_x = match &f.inst {
            Some(i) if is_page => i.page_xrec.borrow().clone(),
            _ => None,
        };
        // a page's source record (a page frame runs on it): the page's xRec = the record as the
        // page last read or stored it; other records: BC's OldRecord outside a running operation
        let page_src = is_page
            || self.page_recs.iter().any(|h| Rc::ptr_eq(h, &r))
            || self.page_open_recs.iter().any(|w| w.upgrade().is_some_and(|h| Rc::ptr_eq(&h, &r)));
        match xr {
            Some(xr) if page_src => x.borrow_mut().row = xr,
            None if page_x.is_some() => x.borrow_mut().row = page_x.unwrap().borrow().row.clone(),
            _ => {
                // BC's OldRecord outside a running operation: a fresh record
                let tdef = x.borrow().tdef;
                let mut blank = x.borrow().schema.new_row();
                self.init_row(tdef, &mut blank);
                x.borrow_mut().row = (blank).into();
            }
        }
        f.xrec = Some(x.clone());
        x
    }

    // ------------------------------------------------------------------ statements

    pub fn exec_block(&mut self, f: &mut Frame, b: &[Stmt]) -> R<()> {
        for s in b {
            self.exec(f, s)?;
        }
        Ok(())
    }

    pub fn exec(&mut self, f: &mut Frame, s: &Stmt) -> R<()> {
        self.cur_line = s.line;
        self.steps += 1;
        if self.steps > self.step_limit {
            // the step limit, or a background session's checkpoint (crate::workers)
            return self.step_limit_reached();
        }
        if self.steps & 0x3ff == 0 && crate::stackguard::abort_requested() {
            return err("Memory limit exceeded: the test grew its worker past al-test's per-test memory limit");
        }
        match &s.kind {
            StmtKind::Empty => Ok(()),
            StmtKind::Block(b) => self.exec_block(f, b),
            StmtKind::Assign { target, op, value } => {
                let v = self.eval(f, value)?;
                // PageVar.LookupMode := true (a property assignment on a page instance);
                // Rec.Mark / Rec.MarkedOnly := true (properties, same as Rec.Mark(true))
                if let (Expr::Member(base, name), AssignOp::Set) = (target, op) {
                    // TestPage.Field.Value := X: the user enters X (as SetValue)
                    if name.eq_ignore_ascii_case("value") {
                        if let Val::TestCtl(tp, c) = self.eval(f, base)? {
                            if !c.starts_with('\u{0}') {
                                self.testctl_assign(f, &tp, &c, v)?;
                                return Ok(());
                            }
                        }
                    }
                    if name.eq_ignore_ascii_case("lookupmode") {
                        if let Val::Object(inst) = self.eval(f, base)? {
                            inst.lookup.set(v.as_bool());
                            return Ok(());
                        }
                    }
                    // CurrPage.Editable := X / PageVar.Editable := X (BC NavForm.Editable setter)
                    if name.eq_ignore_ascii_case("editable") {
                        if matches!(&**base, Expr::Ident(b) if b.eq_ignore_ascii_case("currpage")) {
                            if let Some(i) = f.inst.clone() {
                                i.editable.set(Some(v.as_bool()));
                                return Ok(());
                            }
                        }
                        if let Val::Object(inst) = self.eval(f, base)? {
                            if self.code(inst.code).kind == ObjectKind::Page {
                                inst.editable.set(Some(v.as_bool()));
                                return Ok(());
                            }
                        }
                    }
                    if name.eq_ignore_ascii_case("mark") || name.eq_ignore_ascii_case("markedonly") {
                        if let Val::Record(r) = self.eval(f, base)? {
                            if r.borrow().tdef.field(name).is_none() {
                                // the value is already evaluated: pass it as a literal, not the expression again
                                self.record_method(f, &r, &name.to_ascii_lowercase(), &[Expr::Bool(v.as_bool())])?;
                                return Ok(());
                            }
                        }
                    }
                }
                // Mark := true / MarkedOnly := true on the implicit Rec (table, page and report data item code)
                if let (Expr::Ident(name), AssignOp::Set) = (target, op) {
                    if (name.eq_ignore_ascii_case("mark") || name.eq_ignore_ascii_case("markedonly")) && Self::find_var(f, name).is_none() {
                        if let Some(r) = f.rec.clone() {
                            if r.borrow().tdef.field(name).is_none() {
                                self.record_method(f, &r, &name.to_ascii_lowercase(), &[Expr::Bool(v.as_bool())])?;
                                return Ok(());
                            }
                        }
                    }
                }
                // Obj.Proc := Value / Proc := Value: a one-parameter procedure called with
                // property-assignment syntax (crate::propcall)
                if *op == AssignOp::Set && matches!(target, Expr::Member(..)) && self.try_assign_call(f, target, &v)? {
                    return Ok(());
                }
                let lv = match self.lvalue(f, target) {
                    Ok(lv) => lv,
                    Err(e) if *op == AssignOp::Set && matches!(target, Expr::Ident(_)) => {
                        if self.try_assign_call(f, target, &v)? {
                            return Ok(());
                        }
                        return Err(e);
                    }
                    Err(e) => return Err(e),
                };
                let nv = match op {
                    AssignOp::Set => v,
                    o => {
                        let cur = self.read_lval(&lv);
                        let bop = match o {
                            AssignOp::Add => BinOp::Add,
                            AssignOp::Sub => BinOp::Sub,
                            AssignOp::Mul => BinOp::Mul,
                            _ => BinOp::Div,
                        };
                        self.binop(bop, cur, v)?
                    }
                };
                self.write_lval(&lv, nv).map_err(|e| self.at_line(e, s.line))
            }
            StmtKind::Expr(e) => {
                self.discard = matches!(e, Expr::Call(..) | Expr::Member(..) | Expr::Ident(_));
                let r = self.eval(f, e);
                self.discard = false;
                r.map_err(|e| self.at_line(e, s.line))?;
                Ok(())
            }
            StmtKind::If { cond, then, els } => {
                if self.eval(f, cond)?.as_bool() {
                    self.exec(f, then)
                } else if let Some(e) = els {
                    self.exec(f, e)
                } else {
                    Ok(())
                }
            }
            StmtKind::Case { subject, branches, els } => {
                let v = self.eval(f, subject)?;
                for b in branches {
                    for cv in &b.values {
                        if self.case_match(f, &v, cv)? {
                            return self.exec(f, &b.body);
                        }
                    }
                }
                if let Some(e) = els {
                    self.exec_block(f, e)?;
                }
                Ok(())
            }
            StmtKind::While { cond, body } => {
                while self.eval(f, cond)?.as_bool() {
                    match self.exec(f, body) {
                        Err(Flow::Break) => break,
                        Err(Flow::Continue) => continue,
                        r => r?,
                    }
                }
                Ok(())
            }
            StmtKind::Repeat { body, until } => {
                loop {
                    match self.exec_block(f, body) {
                        Err(Flow::Break) => break,
                        Err(Flow::Continue) => {}
                        r => r?,
                    }
                    if self.eval(f, until)?.as_bool() {
                        break;
                    }
                }
                Ok(())
            }
            StmtKind::For { var, from, to, down, body } => {
                let a = self.eval(f, from)?;
                let b = self.eval(f, to)?;
                let lv = self.lvalue(f, var)?;
                // a Date counter steps one day (dates are stored as days * 2, closing dates odd)
                let date = matches!(a, Val::Date(_));
                let num = |v: &Val| match v {
                    Val::Date(d) => *d as i64,
                    v => v.as_i64(),
                };
                let (lo, hi) = (num(&a), num(&b));
                let mk = |i: i64| match &a {
                    Val::Date(_) => Val::Date(i as i32),
                    Val::Opt { enum_id, .. } => Val::Opt { ord: i as i32, enum_id: *enum_id },
                    _ => Val::Int(i as i32),
                };
                let mut i = lo;
                loop {
                    if (!*down && i > hi) || (*down && i < hi) {
                        break;
                    }
                    self.write_lval(&lv, mk(i))?;
                    match self.exec(f, body) {
                        Err(Flow::Break) => break,
                        Err(Flow::Continue) => {}
                        r => r?,
                    }
                    let cur = num(&self.read_lval(&lv));
                    i = match (date, *down) {
                        (true, false) => (cur / 2 + 1) * 2,
                        (true, true) => (cur / 2 - 1) * 2,
                        (false, false) => cur + 1,
                        (false, true) => cur - 1,
                    };
                }
                Ok(())
            }
            StmtKind::ForEach { var, iter, body } => {
                let coll = self.eval(f, iter)?;
                let from_net = matches!(coll, Val::Net(_));
                let items: Vec<Val> = match coll {
                    Val::List(l) => l.borrow().clone(),
                    Val::Dict(d) => d.borrow().iter().map(|(k, _)| k.clone()).collect(),
                    Val::Text(s) => s.chars().map(Val::Char).collect(),
                    Val::Array(a, _) => a.borrow().clone(),
                    Val::Net(n) => crate::dotnet::enumerate(&n).unwrap_or_default(),
                    _ => vec![],
                };
                let lv = self.lvalue(f, var)?;
                // a .NET element becomes Code/Text without the declared length's check (BC
                // NavCode / NavText.CreateFromObject: max length 2048)
                let unsized_text = match (&lv, from_net) {
                    (LVal::Slot(s, Some(TypeRef::Simple { name, len: Some(_) })), true) if name.eq_ignore_ascii_case("code") || name.eq_ignore_ascii_case("text") => Some((s.clone(), name.eq_ignore_ascii_case("code"))),
                    _ => None,
                };
                for it in items {
                    match &unsized_text {
                        Some((s, code)) => {
                            let t = crate::dotnet::unboxed(it).to_text();
                            *s.borrow_mut() = Val::Text(if *code { t.trim().to_uppercase() } else { t });
                        }
                        None => self.write_lval(&lv, it)?,
                    }
                    match self.exec(f, body) {
                        Err(Flow::Break) => break,
                        Err(Flow::Continue) => {}
                        r => r?,
                    }
                }
                Ok(())
            }
            StmtKind::With { rec, body } => {
                let r = self.eval(f, rec)?;
                if let Val::Record(h) = r {
                    f.with.push(h);
                    let res = self.exec(f, body);
                    f.with.pop();
                    res
                } else {
                    self.exec(f, body)
                }
            }
            StmtKind::Exit(v) => {
                if let Some(e) = v {
                    let val = self.eval(f, e)?;
                    let val = match f.ret_ty {
                        Some(TypeRef::Object { kind, .. }) if kind == "record" => val,
                        t => self.coerce(val, t)?,
                    };
                    match &f.ret {
                        Some(r) => *r.borrow_mut() = val,
                        None => f.ret = Some(slot(val)),
                    }
                }
                Err(Flow::Exit)
            }
            StmtKind::Break => Err(Flow::Break),
            StmtKind::Continue => Err(Flow::Continue),
            StmtKind::AssertError(st) => match self.trapped(|s| s.exec(f, st)) {
                Err(Flow::Err(e)) => {
                    self.note_error(&e);
                    // the error ends the write transaction: changes since the last Commit() are undone
                    self.rollback_to_commit();
                    Ok(())
                }
                Err(other) => Err(other),
                // the text Learn documents [src:X23]
                Ok(()) => err("An error was expected inside an ASSERTERROR statement."),
            },
        }
    }

    fn at_line(&self, e: Flow, line: u32) -> Flow {
        match e {
            Flow::Err(mut x) => {
                if x.stack.is_empty() {
                    x.stack.push(format!("line {line}"));
                }
                Flow::Err(x)
            }
            o => o,
        }
    }

    pub(crate) fn case_match(&mut self, f: &mut Frame, v: &Val, cv: &Expr) -> R<bool> {
        match cv {
            Expr::Range(lo, hi) => {
                let lo_ok = match lo {
                    Some(l) => {
                        let l = self.eval(f, l)?;
                        compare(v, &l) != std::cmp::Ordering::Less
                    }
                    None => true,
                };
                let hi_ok = match hi {
                    Some(h) => {
                        let h = self.eval(f, h)?;
                        compare(v, &h) != std::cmp::Ordering::Greater
                    }
                    None => true,
                };
                Ok(lo_ok && hi_ok)
            }
            e => {
                let c = self.eval(f, e)?;
                Ok(equals(v, &c))
            }
        }
    }

    // ------------------------------------------------------------------ expressions

    pub fn eval(&mut self, f: &mut Frame, e: &Expr) -> R<Val> {
        Ok(match e {
            Expr::Int(i) => {
                if *i > i32::MAX as i64 {
                    Val::BigInt(*i)
                } else {
                    Val::Int(*i as i32)
                }
            }
            Expr::BigInt(i) => Val::BigInt(*i),
            Expr::Dec(s) => Val::Dec(Decimal::from_str(s).unwrap_or_default()),
            Expr::Str(s) => Val::Text(s.to_string()),
            Expr::Bool(b) => Val::Bool(*b),
            Expr::Date(s) => Val::Date(parse_date_literal(s)),
            Expr::Time(s) => Val::Time(parse_time_literal(s)),
            Expr::DateTime(_) => Val::DateTime(bc_store::DATETIME_UNDEFINED),
            Expr::This => match &f.inst {
                Some(i) => Val::Object(i.clone()),
                None => match &f.rec {
                    Some(r) => Val::Record(r.clone()),
                    None => Val::Void,
                },
            },
            Expr::Ident(n) => return self.eval_ident(f, n),
            Expr::Member(base, name) => return self.eval_member(f, base, name, None),
            Expr::Call(callee, args) => match &**callee {
                Expr::Ident(n) => return self.call_named(f, n, args),
                Expr::Member(base, name) => return self.eval_member(f, base, name, Some(args)),
                Expr::Scope(..) => return self.eval(f, callee),
                other => return err(format!("Cannot call expression {other:?}")),
            },
            Expr::Scope(base, name) => return self.eval_scope(f, base, name),
            Expr::Index(base, idx) => {
                // A[i] [j]: an element of a multi-dimensional array written as two index groups
                if let Expr::Index(b2, i2) = &**base {
                    if matches!(**b2, Expr::Ident(_)) {
                        if let Val::Array(a, dims) = self.eval(f, b2)? {
                            if dims.len() == i2.len() + idx.len() {
                                let all: Vec<Expr> = i2.iter().chain(idx.iter()).cloned().collect();
                                let i = self.array_index(f, &all, &dims)?;
                                return Ok(a.borrow()[i].clone());
                            }
                        }
                    }
                }
                let b = self.eval(f, base)?;
                match b {
                    // fewer indices than dimensions: a row, as an array of the remaining dimensions
                    Val::Array(a, dims) if idx.len() < dims.len() => {
                        let (start, rest) = self.array_row_start(f, idx, &dims)?;
                        return Ok(array_row(&a, start, &rest));
                    }
                    Val::Array(a, dims) => {
                        let i = self.array_index(f, idx, &dims)?;
                        return Ok(a.borrow()[i].clone());
                    }
                    Val::Text(s) => {
                        let i = self.eval(f, &idx[0])?.as_i64();
                        Val::Char(s.chars().nth((i - 1).max(0) as usize).unwrap_or('\0'))
                    }
                    other => return err(format!("Cannot index a {}", other.type_name())),
                }
            }
            Expr::Unary(op, x) => {
                let v = self.eval(f, x)?;
                match op {
                    UnOp::Not => Val::Bool(!v.as_bool()),
                    UnOp::Plus => v,
                    UnOp::Neg => match v {
                        Val::Int(i) => Val::Int(-i),
                        Val::BigInt(i) => Val::BigInt(-i),
                        Val::Duration(i) => Val::Duration(-i),
                        o => Val::Dec(-o.as_dec()),
                    },
                }
            }
            Expr::Binary(op, l, r) => match op {
                // AL evaluates both operands of `and` / `or` (no short-circuit); code relies on the
                // right side's side effects (Item Jnl.-Post Line.SetupSplitJnlLine)
                BinOp::And => {
                    let a = self.eval(f, l)?.as_bool();
                    let b = self.eval(f, r)?.as_bool();
                    Val::Bool(a && b)
                }
                BinOp::Or => {
                    let a = self.eval(f, l)?.as_bool();
                    let b = self.eval(f, r)?.as_bool();
                    Val::Bool(a || b)
                }
                BinOp::In => {
                    let items = match &**r {
                        Expr::Set(items) => items,
                        _ => return err("'in' requires a set [...]"),
                    };
                    // `A and B in [..]` parses as `(A and B) in [..]`; when B is not a Boolean the
                    // only well-typed reading (the one the AL compiler accepts) is `A and (B in [..])`
                    if let Expr::Binary(lop @ (BinOp::And | BinOp::Or), a, b) = &**l {
                        let av = self.eval(f, a)?;
                        let bv = self.eval(f, b)?;
                        if !matches!(bv, Val::Bool(_)) {
                            let mut hit = false;
                            for it in items {
                                if self.case_match(f, &bv, it)? {
                                    hit = true;
                                    break;
                                }
                            }
                            return Ok(Val::Bool(if *lop == BinOp::And { av.as_bool() && hit } else { av.as_bool() || hit }));
                        }
                        let v = Val::Bool(if *lop == BinOp::And { av.as_bool() && bv.as_bool() } else { av.as_bool() || bv.as_bool() });
                        for it in items {
                            if self.case_match(f, &v, it)? {
                                return Ok(Val::Bool(true));
                            }
                        }
                        return Ok(Val::Bool(false));
                    }
                    let v = self.eval(f, l)?;
                    for it in items {
                        if self.case_match(f, &v, it)? {
                            return Ok(Val::Bool(true));
                        }
                    }
                    Val::Bool(false)
                }
                // Code values of digits only compare right-aligned ('9' < '10'), see expr_is_code
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge if self.expr_is_code(f, l) || self.expr_is_code(f, r) => {
                    let a = self.eval(f, l)?;
                    let b = self.eval(f, r)?;
                    match (&a, &b) {
                        (Val::Text(x), Val::Text(y)) if !x.is_empty() && !y.is_empty() && x.bytes().all(|c| c.is_ascii_digit()) && y.bytes().all(|c| c.is_ascii_digit()) => {
                            let o = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
                            Val::Bool(match op {
                                BinOp::Lt => o.is_lt(),
                                BinOp::Le => o.is_le(),
                                BinOp::Gt => o.is_gt(),
                                _ => o.is_ge(),
                            })
                        }
                        _ => self.binop(*op, a, b)?,
                    }
                }
                _ => {
                    let a = self.eval(f, l)?;
                    let b = self.eval(f, r)?;
                    self.binop(*op, a, b)?
                }
            },
            Expr::Range(..) => return err("Range outside set"),
            Expr::Set(_) => return err("Set outside 'in'"),
            Expr::Ternary(c, a, b) => {
                if self.eval(f, c)?.as_bool() {
                    self.eval(f, a)?
                } else {
                    self.eval(f, b)?
                }
            }
            Expr::Is(x, iface) => {
                let v = self.eval(f, x)?;
                Val::Bool(match v {
                    Val::Iface(Some(i)) | Val::Object(i) => self.code(i.code).implements.iter().any(|n| n.eq_ignore_ascii_case(iface)),
                    _ => false,
                })
            }
            Expr::As(x, _) => self.eval(f, x)?,
        })
    }

    pub fn binop(&mut self, op: BinOp, a: Val, b: Val) -> R<Val> {
        use Val::*;
        // boxed .NET primitives (DateTime, TimeSpan, ...) take part in AL arithmetic as AL values
        let (a, b) = (crate::dotnet::unboxed(a), crate::dotnet::unboxed(b));
        // two DateFormula values: `=` and `<>` by stored text (R5, T4:P14eq); an order is OPEN:P14
        // (R6, R82)
        if let (DateFormula(x), DateFormula(y)) = (&a, &b) {
            let r = match op {
                BinOp::Eq | BinOp::Ne => crate::datefml::equal(x, y).map(|e| Bool(e == (op == BinOp::Eq))),
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => crate::datefml::order(x, y).map(|o| {
                    Bool(match op {
                        BinOp::Lt => o.is_lt(),
                        BinOp::Le => o.is_le(),
                        BinOp::Gt => o.is_gt(),
                        _ => o.is_ge(),
                    })
                }),
                _ => Ok(Void),
            };
            match r {
                Ok(Void) => {}
                Ok(v) => return Ok(v),
                Err(e) => return crate::builtins::df_error(e, self.global_language),
            }
        }
        Ok(match op {
            BinOp::Eq => Bool(equals(&a, &b)),
            BinOp::Ne => Bool(!equals(&a, &b)),
            BinOp::Lt => Bool(compare(&a, &b) == std::cmp::Ordering::Less),
            BinOp::Le => Bool(compare(&a, &b) != std::cmp::Ordering::Greater),
            BinOp::Gt => Bool(compare(&a, &b) == std::cmp::Ordering::Greater),
            BinOp::Ge => Bool(compare(&a, &b) != std::cmp::Ordering::Less),
            BinOp::Xor => Bool(a.as_bool() ^ b.as_bool()),
            BinOp::And => Bool(a.as_bool() && b.as_bool()),
            BinOp::Or => Bool(a.as_bool() || b.as_bool()),
            BinOp::Add => match (&a, &b) {
                (Text(_) | Char(_), _) | (_, Text(_)) => Text(a.to_text() + &b.to_text()),
                (Date(d), x) | (x, Date(d)) if x.is_numeric() => {
                    if *d == 0 {
                        return err("Undefined date cannot be used in date arithmetic");
                    }
                    Date((d / 2 + x.as_i64() as i32) * 2)
                }
                (DateTime(t), x) => DateTime(t + x.as_i64()),
                (Time(t), x) => Time(t + x.as_i64() as i32),
                (Int(x), Int(y)) => match x.checked_add(*y) {
                    Some(v) => Int(v),
                    None => BigInt(*x as i64 + *y as i64),
                },
                (Int(_) | BigInt(_), Int(_) | BigInt(_)) => BigInt(a.as_i64() + b.as_i64()),
                (Duration(x), y) => Duration(x + y.as_i64()),
                _ => Dec(dec_checked(a.as_dec().checked_add(b.as_dec()))?),
            },
            BinOp::Sub => match (&a, &b) {
                (Date(x), Date(y)) => Int(x / 2 - y / 2),
                (Date(d), n) => {
                    if *d == 0 {
                        return err("Undefined date cannot be used in date arithmetic");
                    }
                    Date((d / 2 - n.as_i64() as i32) * 2)
                }
                (DateTime(x), DateTime(y)) => Duration(x - y),
                (DateTime(t), n) => DateTime(t - n.as_i64()),
                (Time(x), Time(y)) => Duration((x - y) as i64),
                (Time(t), n) => Time(t - n.as_i64() as i32),
                (Int(x), Int(y)) => match x.checked_sub(*y) {
                    Some(v) => Int(v),
                    None => BigInt(*x as i64 - *y as i64),
                },
                (Int(_) | BigInt(_), Int(_) | BigInt(_)) => BigInt(a.as_i64() - b.as_i64()),
                (Duration(x), y) => Duration(x - y.as_i64()),
                _ => Dec(dec_checked(a.as_dec().checked_sub(b.as_dec()))?),
            },
            BinOp::Mul => match (&a, &b) {
                (Int(x), Int(y)) => match x.checked_mul(*y) {
                    Some(v) => Int(v),
                    None => BigInt(*x as i64 * *y as i64),
                },
                (Int(_) | BigInt(_), Int(_) | BigInt(_)) => BigInt(a.as_i64().wrapping_mul(b.as_i64())),
                _ => Dec(dec_checked(a.as_dec().checked_mul(b.as_dec()))?),
            },
            BinOp::Div => {
                let d = b.as_dec();
                if d.is_zero() {
                    return err_code("AL:DivideByZero", "Attempted to divide by zero.");
                }
                Dec(dec_checked(a.as_dec().checked_div(d))?)
            }
            BinOp::IntDiv => {
                // a decimal divisor such as 0.01 is not zero (as_i64 would truncate it)
                let d = b.as_i64();
                if if matches!((&a, &b), (Dec(_), _) | (_, Dec(_))) { b.as_dec().is_zero() } else { d == 0 } {
                    return err_code("AL:DivideByZero", "Attempted to divide by zero.");
                }
                match (&a, &b) {
                    (Int(_), Int(_)) => Int((a.as_i64() / d) as i32),
                    (Dec(_), _) | (_, Dec(_)) => Dec(dec_checked(a.as_dec().checked_div(b.as_dec()))?.trunc()),
                    _ => BigInt(a.as_i64() / d),
                }
            }
            BinOp::Mod => {
                let d = b.as_i64();
                if if matches!((&a, &b), (Dec(_), _) | (_, Dec(_))) { b.as_dec().is_zero() } else { d == 0 } {
                    return err_code("AL:DivideByZero", "Attempted to divide by zero.");
                }
                match (&a, &b) {
                    (Dec(_), _) | (_, Dec(_)) => Dec(dec_checked(a.as_dec().checked_rem(b.as_dec()))?),
                    (Int(_), Int(_)) => Int((a.as_i64() % d) as i32),
                    _ => BigInt(a.as_i64() % d),
                }
            }
            BinOp::In => Bool(false),
        })
    }

    fn eval_ident(&mut self, f: &mut Frame, n: &str) -> R<Val> {
        if let Some(v) = Self::find_var(f, n) {
            return Ok(v.slot.borrow().clone());
        }
        for r in f.with.iter().rev() {
            if let Some(i) = r.borrow().tdef.field(n) {
                return Ok(r.borrow().get_val(i));
            }
        }
        if let Some(r) = &f.rec {
            let fi = r.borrow().tdef.field(n);
            if let Some(i) = fi {
                return Ok(r.borrow().get_val(i));
            }
        }
        let lower = n.to_ascii_lowercase();
        match lower.as_str() {
            "rec" => {
                if let Some(r) = &f.rec {
                    return Ok(Val::Record(r.clone()));
                }
            }
            "xrec" => {
                if f.rec.is_some() {
                    return Ok(Val::Record(self.xrec_of(f)));
                }
            }
            "currfieldno" => return Ok(Val::Int(f.rec.as_ref().map(|r| self.curr_field_no(r)).unwrap_or(f.curr_field))),
            // CurrQuery in query code: the query instance (CurrQuery.SetFilter(...))
            "currquery" if self.code(f.code).kind == ObjectKind::Query && f.inst.is_some() => return Ok(Val::Object(f.inst.clone().unwrap())),
            "currpage" | "currreport" | "currxmlport" | "requestoptionspage" => return Ok(Val::Opaque("CurrPage")),
            _ => {}
        }
        // parameterless procedure / builtin called without parentheses
        self.call_named(f, n, &[])
    }

    /// Unqualified call: own procedures, base object procedures, implicit-Rec methods, builtins.
    fn call_named(&mut self, f: &mut Frame, n: &str, args: &[Expr]) -> R<Val> {
        let co = self.code(f.code);
        if let Some((pr, a)) = self.resolve_call(f, f.code, n, args)? {
            return self.invoke(pr, a, f.inst.clone(), f.rec.clone(), f.xrec.clone());
        }
        // extension → base object procedures
        if let Some(base) = co.extends {
            if let Some(&bc) = self.cat.code_by_key.get(&base) {
                if let Some((pr, a)) = self.resolve_call(f, bc, n, args)? {
                    return self.invoke(pr, a, f.inst.clone(), f.rec.clone(), f.xrec.clone());
                }
            }
        }
        // query code: SetRange / SetFilter / ... on the query itself
        if co.kind == ObjectKind::Query && matches!(n.to_ascii_lowercase().as_str(), "setrange" | "setfilter" | "getfilter" | "getfilters" | "topnumberofrows" | "open" | "read" | "close") {
            if let Some(inst) = f.inst.clone() {
                return self.query_method(f, &inst, n, args);
            }
        }
        // query code: a column name is the current row's value (Serial_No_ in a query procedure)
        if co.kind == ObjectKind::Query && args.is_empty() {
            if let Some(inst) = f.inst.clone() {
                let is_col = inst.query.borrow().as_ref().is_some_and(|st| st.cols.iter().any(|c| c.eq_ignore_ascii_case(n)));
                if is_col {
                    return self.query_method(f, &inst, n, args);
                }
            }
        }
        // implicit Rec methods (table code / pages)
        if let Some(r) = f.rec.clone() {
            if crate::recops::is_record_method(n) {
                return self.record_method(f, &r, n, args);
            }
            // table procedures callable from page code (Rec.Proc without Rec.)
            let tcode = r.borrow().tdef.code;
            if tcode != f.code {
                if let Some((pr, a)) = self.resolve_call(f, tcode, n, args)? {
                    // from page code the table code sees the page's xRec (as Rec.Proc in recops)
                    let x = if matches!(self.code(f.code).kind, ObjectKind::Page | ObjectKind::PageExtension) { f.xrec.clone() } else { None };
                    return self.invoke(pr, a, None, Some(r), x);
                }
            }
        }
        self.builtin(f, n, args)
    }

    pub fn eval_args(&mut self, f: &mut Frame, pr: ProcRef, args: &[Expr]) -> R<Vec<Arg>> {
        let p = &self.code(pr.code).procs[pr.proc as usize];
        let mut out = Vec::with_capacity(args.len());
        for (i, a) in args.iter().enumerate() {
            let by_ref = p.params.get(i).is_some_and(|x| x.by_ref);
            if by_ref {
                out.push(Arg::Ref(self.lvalue(f, a)?));
            } else {
                out.push(Arg::Val(self.eval(f, a)?));
            }
        }
        Ok(out)
    }

    fn eval_member(&mut self, f: &mut Frame, base: &Expr, name: &str, args: Option<&[Expr]>) -> R<Val> {
        // namespace-qualified object id: Database::Microsoft.Projects.Project.Journal."Job Journal Line"
        if args.is_none() && matches!(base, Expr::Member(..) | Expr::Scope(..)) {
            if let Some((k, q)) = scoped_object_path(base) {
                let kind = match k.as_str() {
                    "database" => Some(ObjectKind::Table),
                    "codeunit" => Some(ObjectKind::Codeunit),
                    "page" => Some(ObjectKind::Page),
                    "report" => Some(ObjectKind::Report),
                    "query" => Some(ObjectKind::Query),
                    "xmlport" => Some(ObjectKind::XmlPort),
                    _ => None,
                };
                if let Some(kind) = kind {
                    let q = format!("{q}.{name}");
                    if let Some(id) = self.cat.id_of(kind, &q) {
                        return Ok(Val::Int(id as i32));
                    }
                }
            }
        }
        // this.Global: the object's own global variable
        if args.is_none() && matches!(base, Expr::This) {
            if let Some(v) = f.globals.vars.iter().find(|v| v.name.eq_ignore_ascii_case(name)) {
                return Ok(v.slot.borrow().clone());
            }
        }
        // static "classes" that are not variables
        if let Expr::Ident(b) = base {
            if Self::find_var(f, b).is_none() && !f.rec.as_ref().is_some_and(|r| r.borrow().tdef.field(b).is_some()) {
                let bl = b.to_ascii_lowercase();
                // Dialog.Confirm / Dialog.StrMenu / ... (crate::dialog)
                if bl == "dialog" {
                    return self.dialog_static(f, name, args.unwrap_or(&[]));
                }
                // Version.Create(...) (crate::modules)
                if bl == "version" && name.eq_ignore_ascii_case("create") {
                    return self.version_create(f, args.unwrap_or(&[]));
                }
                // Text.UpperCase(...), Text.CopyStr(...): the Text type's static methods are the
                // global string functions
                if bl == "text" && !matches!(name.to_ascii_lowercase().as_str(), "contains" | "startswith" | "endswith") {
                    return self.builtin(f, name, args.unwrap_or(&[]));
                }
                if crate::builtins::is_static_class(&bl) {
                    return self.static_call(f, &bl, name, args.unwrap_or(&[]));
                }
                if crate::dotnet::is_static_type(&bl) {
                    return self.net_static(f, &bl, name, args.unwrap_or(&[]));
                }
                if self.cat.enum_by_name(b).is_some() {
                    return self.enum_static(f, b, name, args.unwrap_or(&[]));
                }
            }
        }
        // Enum::"Name".Ordinals() / .Names() / .FromInteger(n)
        if let Expr::Scope(k, enum_name) = base {
            if matches!(&**k, Expr::Ident(k) if k.eq_ignore_ascii_case("enum")) && self.cat.enum_by_name(enum_name).is_some() {
                return self.enum_static(f, enum_name, name, args.unwrap_or(&[]));
            }
        }
        // Enum::Microsoft.Manufacturing.Document."Planning Create Prod. Order".FromInteger(n)
        if let Some((k, q)) = scoped_object_path(base) {
            if k == "enum" && self.cat.enum_by_name(&q).is_some() {
                return self.enum_static(f, &q, name, args.unwrap_or(&[]));
            }
        }
        // TestPage.Next.Invoke(): an action or control named like a TestPage method
        // (wizard actions Next / Back, a part's New) is the control here, not the method
        if let (Expr::Member(tb, ctl), Some(a)) = (base, args) {
            if name.eq_ignore_ascii_case("invoke") && matches!(**tb, Expr::Ident(_) | Expr::Member(..)) {
                if let Val::TestPage(tp) = self.eval(f, tb)? {
                    if let Some(c) = self.testpage_control(&tp, ctl) {
                        return self.testctl_method(f, &tp, &c, name, a);
                    }
                }
            }
        }
        let b = self.eval(f, base)?;
        match b {
            Val::TestPage(tp) => return self.testpage_member(f, &tp, name, args),
            Val::TestCtl(tp, c) => return self.testctl_method(f, &tp, &c, name, args.unwrap_or(&[])),
            _ => {}
        }
        let args = args.unwrap_or(&[]);
        if args.is_empty() {
            if let Some(t) = variant_test(&b, name) {
                return Ok(Val::Bool(t));
            }
        }
        match b {
            Val::Record(r) => {
                // RecordRef.Close detaches this variable only (BC NavRecordRef.ALClose sets its
                // target to null): RecordRefs that share its record keep it open
                if args.is_empty() && name.eq_ignore_ascii_case("close") && r.borrow().is_ref {
                    if let Ok(LVal::Slot(sl, _)) = self.lvalue(f, base) {
                        let v = self.new_recordref();
                        *sl.borrow_mut() = v;
                        return Ok(Val::Void);
                    }
                }
                // a RecordRef variable has no fields by name: `RecRef.RecordId` is the member even
                // when its table has a field "RecordID" (Data Type Buffer). Decided by the declared
                // type: record values that went through a RecordRef or Variant are still records.
                let declared_recordref = matches!(base, Expr::Ident(n) if Self::find_var(f, n).and_then(|v| v.ty).is_some_and(|t| matches!(t, TypeRef::Simple { name, .. } if name.eq_ignore_ascii_case("recordref"))));
                if args.is_empty() && !declared_recordref {
                    let fi = r.borrow().tdef.field(name);
                    if let Some(i) = fi {
                        return Ok(r.borrow().get_val(i));
                    }
                }
                self.record_method(f, &r, name, args)
            }
            Val::Object(inst) => self.object_method(f, &inst, name, args),
            Val::Iface(Some(inst)) => self.object_method(f, &inst, name, args),
            Val::Iface(None) => err(format!("The interface variable is not initialized (calling {name})")),
            other => self.value_method(f, other, base, name, args),
        }
    }

    pub fn object_method(&mut self, f: &mut Frame, inst: &Rc<Instance>, name: &str, args: &[Expr]) -> R<Val> {
        // a report runs OnInitReport when BC constructs it, before calls like InitializeRequest
        if self.code(inst.code).kind == ObjectKind::Report {
            self.rep_init_once(inst, true)?;
        }
        let co = self.code(inst.code);
        // XMLport variables: SetSource / Import / Export / SetTableView ... (xmlport.rs)
        if co.kind == ObjectKind::XmlPort {
            if let Some(v) = self.xmlport_method(f, inst, name, args)? {
                return Ok(v);
            }
        }
        if let Some((pr, a)) = self.resolve_call(f, inst.code, name, args)? {
            return self.invoke(pr, a, Some(inst.clone()), None, None);
        }
        // page extension procedures are reachable through the page
        if co.kind == ObjectKind::Page {
            if let Some(pd) = self.cat.pages.get(&co.id) {
                for &ec in &pd.ext_code {
                    if let Some((pr, a)) = self.resolve_call(f, ec, name, args)? {
                        // on the extension's instance: its own globals plus the page's
                        let ext = self.page_ext_instance(inst, ec);
                        return self.invoke(pr, a, Some(ext), None, None);
                    }
                }
            }
        }
        // a method the object does not define but one of its interfaces implements by default
        // (`procedure P() begin ... end;` inside the interface)
        for iface in &co.implements {
            let key = iface.to_ascii_lowercase();
            let short = key.rsplit('.').next().unwrap_or(&key).trim_matches('"');
            let Some(&ic) = self.cat.interfaces.get(&key).or_else(|| self.cat.interfaces.get(short)) else { continue };
            if let Some((pr, a)) = self.resolve_call(f, ic, name, args)? {
                return self.invoke(pr, a, Some(inst.clone()), None, None);
            }
        }
        // a report extension reaches the base report's procedures (`this.GetLocation(...)`)
        if co.kind == ObjectKind::ReportExtension {
            let base = inst.report.borrow().as_ref().and_then(|st| st.base.as_ref()).and_then(|w| w.upgrade());
            if let Some(base) = base {
                return self.object_method(f, &base, name, args);
            }
        }
        let lname = name.to_ascii_lowercase();
        match (co.kind, lname.as_str()) {
            (ObjectKind::Codeunit, "run") => {
                let discard = std::mem::take(&mut self.discard);
                let rec = match args.first() {
                    Some(a) => match self.eval(f, a)? {
                        Val::Record(r) => Some(r),
                        _ => None,
                    },
                    None => None,
                };
                // OnRun runs on this variable's instance (globals set before Run() are kept);
                // `ok := CU.Run()` catches the error and rolls back the codeunit's writes
                let mark = if discard { None } else { Some(self.db.savepoint().map_err(Self::store_err)?) };
                if let Some(r) = &rec {
                    *inst.rec.borrow_mut() = Some(r.clone());
                }
                let res = match co.trigger("onrun") {
                    Some(t) if mark.is_some() => self.trapped(|s| s.invoke(ProcRef { code: inst.code, proc: t }, vec![], Some(inst.clone()), rec, None)).map(|_| ()),
                    Some(t) => self.invoke(ProcRef { code: inst.code, proc: t }, vec![], Some(inst.clone()), rec, None).map(|_| ()),
                    None => Ok(()),
                };
                match (res, mark) {
                    (Ok(()), _) => Ok(Val::Bool(true)),
                    (Err(Flow::Err(e)), Some(mark)) => {
                        self.note_error(&e);
                        self.rollback_trapped(mark)?;
                        Ok(Val::Bool(false))
                    }
                    (Err(e), _) => Err(e),
                }
            }
            (ObjectKind::Page, "run") | (ObjectKind::Page, "runmodal") => {
                for a in args {
                    let _ = self.eval(f, a)?;
                }
                if let Some(r) = self.host_run_page(co.id, Some(inst.clone()), None, lname == "runmodal") {
                    return r;
                }
                self.ui_run_page(co.id, Some(inst.clone()), None, lname == "runmodal")
            }
            (ObjectKind::Page, "lookupmode") => {
                if let Some(a) = args.first() {
                    let on = self.eval(f, a)?.as_bool();
                    inst.lookup.set(on);
                }
                Ok(Val::Bool(inst.lookup.get()))
            }
            (ObjectKind::Page, "settableview") | (ObjectKind::Page, "setrecord") | (ObjectKind::Page, "getrecord") | (ObjectKind::Page, "setselectionfilter") => {
                self.instance_globals(inst);
                let Some(a) = args.first() else { return Ok(Val::Void) };
                let lv = self.lvalue(f, a)?;
                let Val::Record(src) = self.read_lval(&lv) else { return Ok(Val::Void) };
                let cur = inst.rec.borrow().clone();
                let Some(cur) = cur else { return Ok(Val::Void) };
                if src.borrow().tdef.id != cur.borrow().tdef.id {
                    return Ok(Val::Void);
                }
                // a page with SourceTableTemporary = true keeps its own temporary table: SetTableView /
                // SetRecord pass only the view / current record (its data comes from page code such as
                // Item Tracking Summary.SetSources); other pages run on a temporary record passed in
                let own_temp = self.cat.pages.get(&co.id).is_some_and(|p| p.props.get("sourcetabletemporary").is_some_and(|v| v.eq_ignore_ascii_case("true")));
                match lname.as_str() {
                    "settableview" => {
                        let s = src.borrow();
                        let mut c = cur.borrow_mut();
                        c.filters = s.filters.clone();
                        c.key = s.key;
                        c.ascending = s.ascending;
                        if s.temp.is_some() && !own_temp {
                            c.temp = s.temp.clone();
                        }
                    }
                    "setrecord" => {
                        let s = src.borrow();
                        let mut c = cur.borrow_mut();
                        c.row = s.row.clone();
                        if s.temp.is_some() && !own_temp {
                            c.temp = s.temp.clone();
                        }
                    }
                    "getrecord" => {
                        let row = cur.borrow().row.clone();
                        src.borrow_mut().row = row;
                    }
                    _ => {
                        // the selection is the current record
                        let c = cur.borrow();
                        let mut s = src.borrow_mut();
                        s.filters = c.filters.clone();
                        s.row = c.row.clone();
                        let pkf = s.tdef.pk().fields.clone();
                        for fi in pkf {
                            let v = s.row[fi].clone();
                            let t = to_val(s.field(fi), &v).to_text();
                            s.set_filter(fi, Some(crate::filter::FNode::Eq(v)), t);
                        }
                    }
                }
                Ok(Val::Void)
            }
            (ObjectKind::Query, _) => self.query_method(f, inst, name, args),
            (ObjectKind::Report, _) => self.report_method(f, inst, name, args),
            (ObjectKind::Page, "page") => Ok(Val::Object(inst.clone())),
            (ObjectKind::Page, "visible") | (ObjectKind::Page, "enabled") => Ok(Val::Bool(true)),
            // PageVar.Editable([X]): the page's Editable (BC NavForm.Editable; builtins.rs page_editable)
            (ObjectKind::Page, "editable") => {
                if let Some(a) = args.first() {
                    let v = self.eval(f, a)?.as_bool();
                    inst.editable.set(Some(v));
                }
                Ok(Val::Bool(self.page_editable(inst, co.id)))
            }
            // PageVar.Caption: the page's caption (CurrPage.Caption: builtins.rs)
            (ObjectKind::Page, "caption") if args.is_empty() => Ok(Val::Text(self.cat.pages.get(&co.id).map(|p| p.caption.clone()).unwrap_or_default())),
            (_, "settableview") | (_, "setrecord") | (_, "settablefilter") | (_, "lookupmode") | (_, "editable") | (_, "caption")
            | (_, "update") | (_, "close") | (_, "activate") | (_, "usereqeuestpage") | (_, "userequestpage") | (_, "setselectionfilter") => {
                for a in args {
                    let _ = self.eval(f, a)?;
                }
                Ok(Val::Bool(false))
            }
            (_, "run") | (_, "runmodal") => {
                self.messages.push(format!("(UI) {} {} opened", co.kind.as_str(), co.name));
                Ok(Val::Opt { ord: 0, enum_id: None })
            }
            _ => err(format!("Method '{name}' not found on {} '{}'", co.kind.as_str(), co.name)),
        }
    }

    fn eval_scope(&mut self, f: &mut Frame, base: &Expr, name: &str) -> R<Val> {
        // Enum::"X"::Y
        if let Expr::Scope(b2, enum_name) = base {
            if matches!(&**b2, Expr::Ident(k) if k.eq_ignore_ascii_case("enum")) {
                return self.enum_value(enum_name, name);
            }
        }
        match base {
            Expr::Ident(b) => {
                let bl = b.to_ascii_lowercase();
                let kind = match bl.as_str() {
                    "database" => Some(ObjectKind::Table),
                    "codeunit" => Some(ObjectKind::Codeunit),
                    "page" => Some(ObjectKind::Page),
                    "report" => Some(ObjectKind::Report),
                    "query" => Some(ObjectKind::Query),
                    "xmlport" => Some(ObjectKind::XmlPort),
                    _ => None,
                };
                if let Some(k) = kind {
                    return Ok(Val::Int(self.cat.id_of(k, name).unwrap_or(0) as i32));
                }
                if bl == "objecttype" {
                    let o = match name.to_ascii_lowercase().as_str() {
                        "tabledata" => 0,
                        "table" => 1,
                        "report" => 3,
                        "codeunit" => 5,
                        "xmlport" => 6,
                        "menusuite" => 7,
                        "page" => 8,
                        "query" => 9,
                        "system" => 10,
                        "fieldnumber" => 11,
                        _ => 99,
                    };
                    return Ok(Val::Opt { ord: o, enum_id: None });
                }
                // variable with option/enum type (of an array: the element type's member)
                if let Some(v) = Self::find_var(f, b) {
                    let ty = match v.ty {
                        Some(TypeRef::Array { elem, .. }) => Some(&**elem),
                        t => t,
                    };
                    match ty {
                        Some(TypeRef::Option(members)) => {
                            let i = members.iter().position(|m| m.eq_ignore_ascii_case(name)).unwrap_or(0);
                            return Ok(Val::Opt { ord: i as i32, enum_id: Some(crate::optmeta::for_members(members)) });
                        }
                        Some(TypeRef::Object { kind, target, .. }) if kind == "enum" => return self.enum_value(target, name),
                        _ => {}
                    }
                }
                // field of implicit Rec
                if let Some(r) = f.rec.clone() {
                    let fi = r.borrow().tdef.field(b);
                    if let Some(i) = fi {
                        return self.field_option(r.borrow().field(i), name);
                    }
                }
                if self.cat.enum_by_name(b).is_some() {
                    return self.enum_value(b, name);
                }
                Ok(Val::Opt { ord: system_option_ord(&bl, name), enum_id: crate::optmeta::for_system(&bl) })
            }
            Expr::Member(rb, fname) => {
                // namespace-qualified type: Microsoft.Foundation.Enums."Inventory Order Type"::Service
                if let Some((k, q)) = scoped_object_path(base) {
                    // Enum::Microsoft.Inventory."Some Enum"::Value
                    if k == "enum" && self.cat.enum_by_name(&q).is_some() {
                        return self.enum_value(&q, name);
                    }
                }
                if let Some(q) = qualified_name(base) {
                    let root = q.split('.').next().unwrap_or("");
                    let implicit_rec = (root.eq_ignore_ascii_case("rec") && f.rec.is_some()) || (root.eq_ignore_ascii_case("xrec") && f.xrec.is_some());
                    if !implicit_rec && Self::find_var(f, root).is_none() && !f.rec.as_ref().is_some_and(|r| r.borrow().tdef.field(root).is_some()) {
                        if self.cat.enum_by_name(&q).is_some() {
                            return self.enum_value(&q, name);
                        }
                    }
                }
                let r = self.eval(f, rb)?;
                // Query.Column::Member (WorkflowDefinition.Type::"Event"): the option of the
                // column's source field
                if let Val::Object(qi) = &r {
                    if let Some(fd) = self.query_column_field(qi, fname) {
                        return self.field_option(fd, name);
                    }
                }
                if let Val::Record(h) = r {
                    let fi = h.borrow().tdef.field(fname);
                    if let Some(i) = fi {
                        let fd = h.borrow().field(i);
                        return self.field_option(fd, name);
                    }
                }
                // record property option values: Rec.ReadIsolation::ReadUncommitted
                Ok(Val::Opt { ord: system_option_ord(&fname.to_ascii_lowercase(), name), enum_id: None })
            }
            other => {
                // AmountField[I]::SchPrice: a member of an Option / Enum array's element type
                if let Expr::Index(b, _) = other {
                    if let Expr::Ident(n) = &**b {
                        if let Some(TypeRef::Array { elem, .. }) = Self::find_var(f, n).and_then(|v| v.ty) {
                            match &**elem {
                                TypeRef::Option(members) => {
                                    let i = members.iter().position(|m| m.eq_ignore_ascii_case(name)).unwrap_or(0);
                                    return Ok(Val::Opt { ord: i as i32, enum_id: Some(crate::optmeta::for_members(members)) });
                                }
                                TypeRef::Object { kind, target, .. } if kind == "enum" => return self.enum_value(target, name),
                                _ => {}
                            }
                        }
                    }
                }
                let v = self.eval(f, other)?;
                match v {
                    Val::Opt { enum_id: Some(eid), .. } if eid >= 0 => {
                        let en = self.cat.enums[&eid].name.clone();
                        self.enum_value(&en, name)
                    }
                    // an Option value with its declared members (crate::optmeta)
                    Val::Opt { enum_id: Some(e), .. } if crate::optmeta::is_meta(e) => match crate::optmeta::get(e).and_then(|m| m.names.iter().position(|x| x.trim().eq_ignore_ascii_case(name.trim()))) {
                        Some(i) => Ok(Val::Opt { ord: i as i32, enum_id: Some(e) }),
                        None => err(format!("Cannot resolve ::{name}")),
                    },
                    _ => err(format!("Cannot resolve ::{name}")),
                }
            }
        }
    }

    pub fn enum_value(&mut self, enum_name: &str, value: &str) -> R<Val> {
        match self.cat.enum_by_name(enum_name) {
            Some(e) => match e.by_name(value) {
                Some(v) => Ok(Val::Opt { ord: v.ordinal as i32, enum_id: Some(e.id) }),
                None => err(format!("Enum '{enum_name}' has no value '{value}'")),
            },
            None => Ok(Val::Opt { ord: system_option_ord(&enum_name.to_ascii_lowercase(), value), enum_id: None }),
        }
    }

    pub fn field_option(&mut self, fd: &al_model::FieldDef, name: &str) -> R<Val> {
        match &fd.ty {
            al_model::FieldType::Enum(en) => {
                let en = en.clone();
                self.enum_value(&en, name)
            }
            _ => {
                let i = fd.options.iter().position(|m| m.trim().eq_ignore_ascii_case(name.trim())).unwrap_or(0);
                Ok(Val::Opt { ord: i as i32, enum_id: None })
            }
        }
    }
}

/// A Record parameter passed by value: the record's values, without the caller's filters, sorting
/// and marks (open question [src:A11b.4]).
fn by_value(v: Val, ty: &TypeRef) -> Val {
    if let (Val::Record(h), TypeRef::Object { kind, .. }) = (&v, ty) {
        if kind == "record" && !h.borrow().is_ref {
            let mut r = h.borrow_mut();
            r.filters.clear();
            r.filter_group = 0;
            r.key = 0;
            r.custom_key = None;
            r.ascending = true;
            r.marks.clear();
            r.mark_only = false;
            r.prefetch = Default::default();
        }
    }
    v
}

/// RecordRef is a reference type: passing it by value still shares the RecordRef.
fn is_reference_type(t: &TypeRef) -> bool {
    matches!(t, TypeRef::Simple { name, .. } if name.eq_ignore_ascii_case("recordref"))
}

/// A Notification parameter passed by value shares the caller's notification (assignment still
/// copies): Check Credit Limit.PopulateDataOnNotification(Notification) passes it on to Credit
/// Limit Details' SetData calls, and the caller sends it with that data in BC.
fn shares_by_value(t: &TypeRef) -> bool {
    matches!(t, TypeRef::Simple { name, .. } if name.eq_ignore_ascii_case("notification") || name.eq_ignore_ascii_case("filterpagebuilder"))
}

fn clone_arg(a: &Arg) -> Arg {
    match a {
        Arg::Val(v) => Arg::Val(v.clone()),
        Arg::Ref(lv) => Arg::Ref(clone_lval(lv)),
    }
}

/// A copy of the row of `a` that starts at flat element `start`, as an array of `dims`.
fn array_row(a: &Rc<RefCell<Vec<Val>>>, start: usize, dims: &[usize]) -> Val {
    let n: usize = dims.iter().product();
    let row: Vec<Val> = a.borrow().iter().skip(start).take(n).cloned().collect();
    Val::Array(Rc::new(RefCell::new(row)), dims.to_vec())
}

fn clone_lval(lv: &LVal) -> LVal {
    match lv {
        LVal::Slot(s, t) => LVal::Slot(s.clone(), *t),
        LVal::Field(r, i) => LVal::Field(r.clone(), *i),
        LVal::Elem(a, i, t) => LVal::Elem(a.clone(), *i, *t),
        LVal::ErrorMsg(m, p) => LVal::ErrorMsg(m.clone(), p.clone()),
        LVal::TextChar(b, i) => LVal::TextChar(Box::new(clone_lval(b)), *i),
        LVal::Row(a, s, d) => LVal::Row(a.clone(), *s, d.clone()),
        LVal::RecProp(r, p) => LVal::RecProp(r.clone(), p),
        LVal::SessionProp(p) => LVal::SessionProp(p),
        LVal::NetProp(n, p) => LVal::NetProp(n.clone(), p.clone()),
    }
}

fn check_len(s: &str, len: Option<u32>) -> R<()> {
    if let Some(n) = len {
        let c = s.chars().count();
        if c > n as usize {
            return err(format!("The length of the string is {c}, but it must be less than or equal to {n} characters. Value: {s}"));
        }
    }
    Ok(())
}

pub fn parse_date_literal(s: &str) -> i32 {
    if s == "0" || s.len() != 8 {
        return 0;
    }
    let y = s[0..4].parse().unwrap_or(0);
    let m = s[4..6].parse().unwrap_or(0);
    let d = s[6..8].parse().unwrap_or(0);
    date_from_ymd(y, m, d).unwrap_or(0)
}

pub fn parse_time_literal(s: &str) -> i32 {
    if s == "0" {
        return 0;
    }
    let mut parts = s.split('.');
    let digits: String = parts.next().unwrap_or("").to_string();
    let n: i32 = digits.parse().unwrap_or(0);
    let (h, m, sec) = (n / 10000, (n / 100) % 100, n % 100);
    // hhmmss.fffT: the fraction is milliseconds (235959.999T, 103045.5T = 500 ms)
    let frac: String = parts.next().unwrap_or("").chars().filter(|c| c.is_ascii_digit()).take(3).collect();
    let ms = if frac.is_empty() { 0 } else { format!("{frac:0<3}").parse::<i32>().unwrap_or(0) };
    h * 3_600_000 + m * 60_000 + sec * 1000 + ms + 1
}

/// Ordinals for platform option types (ClientType::Web, ...). Unknown members get a
/// stable hashed ordinal so equality comparisons still behave.
pub fn system_option_ord(ty: &str, member: &str) -> i32 {
    let m = member.to_ascii_lowercase();
    let list: &[&str] = match ty {
        "clienttype" => &["windows", "soap", "odatav4", "nas", "background", "management", "web", "desktop", "tablet", "phone", "api", "childsession", "teams"],
        "verbosity" => &["critical", "error", "warning", "normal", "verbose"],
        "dataclassification" => &["customercontent", "endusertdentifiableinformation", "accountdata", "euii", "systemmetadata", "organizationidentifiableinformation", "enduserpseudonymousidentifiers"],
        "telemetryscope" => &["extensionpublisher", "all"],
        "isolationlevel" => &["default", "readuncommitted", "readcommitted", "repeatableread", "updlock"],
        "action" => &["ok", "cancel", "lookupok", "lookupcancel", "yes", "no", "runobject", "runsystem"],
        "executioncontext" => &["normal", "install", "upgrade"],
        "defaultlayout" => &["rdlc", "word", "excel", "external"],
        "reportformat" => &["pdf", "word", "excel", "xml", "html"],
        "notificationscope" => &["localscope", "globalscope"],
        // HTTP codes: platobj::action_result_status
        "webserviceactionresultcode" => &["none", "get", "created", "updated", "deleted"],
        "securityfilter" => &["ignored", "filtered", "validated", "disallowed"],
        // Ncl DataScope = table "Isolated Storage" Scope (crate::isolated); a DataScope variable starts as Module
        "datascope" => &["module", "company", "user", "companyanduser"],
        "fieldtype" => &["integer", "decimal", "text", "code", "boolean", "date", "time", "datetime", "option", "guid", "biginteger", "blob", "media", "mediaset", "dateformula", "duration", "recordid", "tablefilter"],
        "fieldclass" => &["normal", "flowfield", "flowfilter"],
        "pagebackgroundtaskerrorlevel" => &["error", "warning", "ignore"],
        // BC's TransactionType enum (crate::sysfuncs TRANSACTION_TYPES)
        "transactiontype" => &["updatenolocks", "update", "snapshot", "browse", "report"],
        _ => &[],
    };
    if let Some(i) = list.iter().position(|x| *x == m) {
        return i as i32;
    }
    let mut h: u32 = 2166136261;
    for b in ty.bytes().chain([b':']).chain(m.bytes()) {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    1000 + (h & 0x3fff_ffff) as i32
}

/// Variant type tests: v.IsRecord, v.IsText, ...
pub fn variant_test(v: &Val, name: &str) -> Option<bool> {
    let n = name.to_ascii_lowercase();
    if !n.starts_with("is") {
        return None;
    }
    Some(match n.as_str() {
        "isrecord" => matches!(v, Val::Record(r) if !r.borrow().is_ref),
        "isrecordref" => matches!(v, Val::Record(r) if r.borrow().is_ref) || matches!(v, Val::Opaque("RecordRef")),
        "isrecordid" => matches!(v, Val::RecordId(..)),
        "isfieldref" => matches!(v, Val::FieldRef(..)),
        "istext" | "iscode" => matches!(v, Val::Text(_)),
        "ischar" => matches!(v, Val::Char(_)),
        "isinteger" => matches!(v, Val::Int(_)),
        "isbiginteger" => matches!(v, Val::BigInt(_)),
        "isdecimal" => matches!(v, Val::Dec(_)),
        "isboolean" => matches!(v, Val::Bool(_)),
        "isdate" => matches!(v, Val::Date(_)),
        "istime" => matches!(v, Val::Time(_)),
        "isdatetime" => matches!(v, Val::DateTime(_)),
        "isduration" => matches!(v, Val::Duration(_)),
        "isguid" => matches!(v, Val::Guid(_)),
        "isoption" => matches!(v, Val::Opt { .. }),
        "isbyte" => false,
        "isdateformula" => matches!(v, Val::DateFormula(_)),
        "iscodeunit" => matches!(v, Val::Object(_)),
        "islist" => matches!(v, Val::List(_)),
        "isdictionary" => matches!(v, Val::Dict(_)),
        "isjsonobject" => matches!(v, Val::Json(j) if matches!(&*j.borrow(), crate::json::J::Obj(_))),
        "isjsonarray" => matches!(v, Val::Json(j) if matches!(&*j.borrow(), crate::json::J::Arr(_))),
        "isjsontoken" => matches!(v, Val::Json(_)),
        "isjsonvalue" => matches!(v, Val::Json(j) if !matches!(&*j.borrow(), crate::json::J::Obj(_) | crate::json::J::Arr(_))),
        "isnotification" => matches!(v, Val::Opaque("Notification")) || crate::platobj::is_notification(v),
        "isinstream" => matches!(v, Val::InStream(_)),
        "isoutstream" => matches!(v, Val::OutStream(_)),
        "isautomation" | "isdotnet" | "isfile" | "isaction" | "isbinary" | "istextconstant" | "isclienttype"
        | "isreportformat" | "isfilterpagebuilder" | "istestpermissions" | "isexecutionmode" | "isobjecttype" | "isdefaultlayout" | "issecuritfiltering"
        | "istransactiontype" | "istextencoding" | "isbytes" => false,
        _ => return None,
    })
}

/// `this.Name` → Name
fn this_member(e: &Expr) -> Option<&str> {
    match e {
        Expr::Member(b, n) if matches!(**b, Expr::This) => Some(n),
        _ => None,
    }
}

/// `A.B."C"` → "A.B.C" when the expression is a plain dotted name.
pub fn qualified_name(e: &Expr) -> Option<String> {
    match e {
        Expr::Ident(n) => Some(n.to_string()),
        Expr::Member(b, n) => Some(format!("{}.{}", qualified_name(b)?, n)),
        _ => None,
    }
}

/// `Database::Microsoft.Sales.Customer.Customer` parses as members on `Database::Microsoft`:
/// returns the keyword (lower-cased) and the namespace-qualified name.
pub fn scoped_object_path(e: &Expr) -> Option<(String, String)> {
    match e {
        Expr::Member(b, n) => scoped_object_path(b).map(|(k, q)| (k, format!("{q}.{n}"))),
        Expr::Scope(b, n) => match &**b {
            Expr::Ident(k) => Some((k.to_ascii_lowercase(), n.to_string())),
            _ => None,
        },
        _ => None,
    }
}

impl Drop for Session<'_> {
    /// Break the Rc cycles that go through global variables (see `made_globals`): every object
    /// the session made ends with it, as in BC, where they belong to the session.
    fn drop(&mut self) {
        crate::frames::forget_labels(self.id);
        crate::recops::forget_own_writes(self.id);
        for w in std::mem::take(&mut self.made_globals) {
            if let Some(g) = w.upgrade() {
                for v in &g.vars {
                    if let Ok(mut slot) = v.slot.try_borrow_mut() {
                        *slot = Val::Void;
                    }
                }
            }
        }
    }
}
