//! The session socket of the client services endpoint: JSON-RPC 2.0 over a
//! WebSocket at `/{instance}/client/ws/connect`, one BC session per socket.
//!
//! Each socket gets a worker thread that owns the AL [`Session`] and the open
//! page instances (al-runtime values are `Rc`-based and stay on that thread).
//! The async side only moves text frames: incoming frames go to the worker,
//! the worker's replies go back out. This also leaves room for live callbacks
//! (Confirm, RunModal): the worker can send a request to the web server and
//! keep reading frames until the matching `EndClientCall*` arrives.
//!
//! Wire format: docs/nst-protocol-transport.md. Method signatures are the
//! positional parameters of `Microsoft.Dynamics.Nav.Types.IClientApi`.
//!
//! Form calls run Microsoft's AL through `crate::form::PageForm` (one per
//! `ServerFormHandle`): OpenForm (SourceTableView, OnInit, OnOpenPage, positioning on
//! `CurrentRecord`), GetPage (OnAfterGetRecord per row, expression columns),
//! InvokeApplicationMethod (`AfterGetCurrRecord`, `NewRecord`), ValidateField
//! (`Rec.Validate` + the control's OnValidate), ModifyRecord / InsertRecord
//! (OnModifyRecord / OnInsertRecord, `Modify(true)` / `Insert(true)`, AutoSplitKey),
//! ActionField (the action's OnAction), CloseForm (OnClosePage). AL errors become
//! `NavNCLDialogException` faults; a failed ValidateField is wrapped in
//! `NavNSValidateFieldException` so the client shows it on the field.
//!
//! Confirmed against the BC 29.1 service tier (data/capture/nst-real, samples/):
//! `tenantId ""` (a non-empty one breaks the session id after a page navigation),
//! OpenForm's one-row DataSet table "0", `RecordState` 96 / 256, `PageCaption` and
//! `FormVariables` (`<table>_NonRow`) on returned states, `IsAbleToOverwriteAllowedProperties
//! true`, `VersionNumber 0`, the six envelope tokens, the Evaluate bag, the
//! GetAllowedCompanies / GetUsersAndAgents column positions, WorkDateChangedRequest in
//! OpenCompany, the NavNSValidateFieldException chain.
//! Guessed or stubbed (each logged where it applies): Confirm / StrMenu inside AL get
//! their default answer (no server-to-client dialog callback yet); Message() texts go
//! back as DialogMessageRequests after the call; `RecordState` flags after ActionField;
//! CaptionClass captions are `<n>,<text>` → `<text>` only; DrillDownField / LookupField /
//! callbacks (CreateDotNetHandle, ...) are not implemented.

use crate::wire::{self, Fault, NIL_GUID};
use al_runtime::{Flow, Instance, Runtime, Session};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Instant;

// NavDataSets (OpenForm's `DataSet`, GetPage rows) come from `crate::dataset`, in the
// non-compact format: OpenConnection sends `enableCompactSerialization: false`.

// ---------------------------------------------------------------------------
// Configuration and shared state

pub struct NstConfig {
    /// The only company bc-rust serves for now.
    pub company: String,
    pub company_display_name: String,
    /// Accepted user name (compared case-insensitively; NavUserPassword sends it upper-cased).
    pub user: String,
    /// Metadata token: sent as top-level `MetadataToken` on every response.
    /// Must equal the token the metadata endpoints use for `VersionNumber`.
    pub metadata_token: i64,
    pub lcid: i32,
    pub tenant_id: String,
    /// `UserSettings.enabledFeatures`.
    pub enabled_features: Vec<String>,
    /// Sent as `serverAssemblyFileVersion` when the client doesn't send its own version.
    pub server_version: String,
    pub application_version: String,
    pub application_build: String,
    pub profile_id: String,
    pub profile_caption: String,
    pub profile_app_id: String,
}

/// Base Application app id (Microsoft).
pub const BASE_APP_ID: &str = "437dbf0e-84ff-417a-965d-ed2bb9650972";

impl Default for NstConfig {
    fn default() -> Self {
        NstConfig {
            company: "CRONUS".into(),
            company_display_name: "CRONUS".into(),
            user: "admin".into(),
            metadata_token: 1,
            lcid: 1033,
            tenant_id: "default".into(),
            // SaveValueToDatabasePromptly: the web server sends ModifyRecord right after
            // each validated field, as BC 29.1 does (always on there)
            enabled_features: vec!["SaveValueToDatabasePromptly".into()],
            server_version: "29.0.57431.0".into(),
            application_version: "29.0.0.0".into(),
            application_build: "0".into(),
            profile_id: "BUSINESS MANAGER".into(),
            profile_caption: "Business Manager".into(),
            profile_app_id: BASE_APP_ID.into(),
        }
    }
}

/// Table "All Profile" (platform/SystemTables.al).
pub const ALL_PROFILE_TABLE: i64 = 2000000178;

/// Fill "All Profile" from the catalog's `profile` objects, the way the NST lists the
/// profiles of the installed apps. The web server reads this table without a form
/// while it starts a session (`ProfileAttributes`) and ends the session when it is
/// empty. The profile named `cfg.profile_id` is the default role center. Existing
/// rows are updated, so this can run at every start. Returns the number of profiles.
pub fn fill_all_profile(s: &mut Session, cfg: &NstConfig) -> anyhow::Result<usize> {
    use al_runtime::record::to_store;
    use al_runtime::Val;
    let cat = s.cat;
    let tdef = cat.tables.get(&ALL_PROFILE_TABLE).ok_or_else(|| anyhow::anyhow!("table {ALL_PROFILE_TABLE} \"All Profile\" is not in the catalog"))?;
    let app_id = al_runtime::value::parse_guid(BASE_APP_ID).expect("valid app id");
    let clip = |t: &str, n: usize| t.chars().take(n).collect::<String>();
    let flow = |e: al_runtime::Flow| match e {
        al_runtime::Flow::Err(e) => anyhow::anyhow!("{}", e.msg),
        _ => anyhow::anyhow!("unexpected control flow"),
    };
    let mut n = 0;
    for p in &cat.profiles {
        let r = s.new_record(ALL_PROFILE_TABLE, false);
        let fields: [(&str, Val); 10] = [
            ("Scope", Val::Opt { ord: 0, enum_id: None }), // System
            ("App ID", Val::Guid(app_id)),
            ("Profile ID", Val::Text(clip(&p.name, 30))),
            ("Description", Val::Text(clip(&p.description, 250))),
            ("Caption", Val::Text(clip(&p.caption, 100))),
            ("Role Center ID", Val::Int(p.role_center.unwrap_or(0) as i32)),
            ("Default Role Center", Val::Bool(p.name.eq_ignore_ascii_case(&cfg.profile_id))),
            ("Enabled", Val::Bool(p.enabled)),
            ("Promoted", Val::Bool(p.promoted)),
            ("App Name", Val::text("Base Application")),
        ];
        for (name, v) in &fields {
            let fi = tdef.field(name).ok_or_else(|| anyhow::anyhow!("All Profile has no field {name}"))?;
            let sv = to_store(&tdef.fields[fi], v).map_err(|e| anyhow::anyhow!("All Profile.{name}: {e}"))?;
            r.borrow_mut().row[fi] = sv;
        }
        let res = if s.rec_get_pk(&r).is_some() { s.rec_modify(&r, false) } else { s.rec_insert(&r, false) };
        res.map_err(flow)?.map_err(|e| anyhow::anyhow!("All Profile {}: {e}", p.name))?;
        n += 1;
    }
    s.commit().map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(n)
}

pub struct NstState {
    pub rt: &'static Runtime,
    pub cfg: NstConfig,
    next_session: AtomicI32,
    warned_auth: AtomicBool,
    /// PageUsageStates per (user, object type, object id): which tours and teaching tips the
    /// user has seen or dismissed. Kept in memory, so a restart shows them again.
    page_usage: std::sync::Mutex<HashMap<(String, i64, i64), i64>>,
}

impl NstState {
    pub fn new(rt: &'static Runtime, cfg: NstConfig) -> Arc<NstState> {
        Arc::new(NstState { rt, cfg, next_session: AtomicI32::new(1), warned_auth: AtomicBool::new(false), page_usage: Default::default() })
    }

    fn warn_auth(&self, user: &str) {
        if !self.warned_auth.swap(true, Ordering::Relaxed) {
            eprintln!("bc-nst: WARNING: authentication is not enforced; accepting user '{user}' with any password");
        }
    }
}

/// Routes for the session socket. `/{instance}/client/ws/connect` is the path
/// `ConnectionEstablisher.OpenWebSocket` dials. `/{instance}/Service` is
/// `ServiceUri.NavServerAddress` (used in the web server's messages); it is
/// accepted too and logged, in case some client dials it.
pub fn router(state: Arc<NstState>) -> Router {
    Router::new()
        .route("/{instance}/client/ws/connect", get(ws_connect))
        .route("/{instance}/Service", get(ws_service))
        .with_state(state)
}

async fn ws_connect(State(st): State<Arc<NstState>>, Path(instance): Path<String>, mut headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    // BC_AUTH: the credentials must validate before the upgrade (crate::auth)
    if let Err(r) = crate::auth::check_upgrade(&st, &mut headers).await {
        return r;
    }
    upgrade(st, instance, "client/ws/connect", headers, ws)
}

async fn ws_service(State(st): State<Arc<NstState>>, Path(instance): Path<String>, mut headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if let Err(r) = crate::auth::check_upgrade(&st, &mut headers).await {
        return r;
    }
    upgrade(st, instance, "Service", headers, ws)
}

fn upgrade(st: Arc<NstState>, instance: String, path: &'static str, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let user = match basic_auth_user(&headers) {
        // with BC_AUTH on, ws_connect has validated the credentials already
        Some(u) if bc_server::auth::enabled() => Some(u),
        Some(u) if user_matches(&st.cfg.user, &u) => {
            st.warn_auth(&u);
            Some(u)
        }
        Some(u) => {
            eprintln!("bc-nst: rejecting WebSocket upgrade on /{instance}/{path}: unknown user '{u}'");
            return (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Basic")]).into_response();
        }
        None => {
            eprintln!("bc-nst: WebSocket upgrade on /{instance}/{path} without Basic credentials (user taken from OpenConnection)");
            None
        }
    };
    eprintln!("bc-nst: WebSocket session on path /{instance}/{path} (user {})", user.as_deref().unwrap_or("?"));
    ws.on_upgrade(move |socket| run_socket(st, socket, user))
}

/// User name from `Authorization: Basic base64(user:password)`.
fn basic_auth_user(h: &HeaderMap) -> Option<String> {
    let v = h.get(header::AUTHORIZATION)?.to_str().ok()?;
    let b64 = v.strip_prefix("Basic ").or_else(|| v.strip_prefix("basic "))?;
    let raw = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    // ISO-8859-1 on the .NET side: map bytes to chars one to one
    let s: String = raw.iter().map(|&b| b as char).collect();
    Some(s.split_once(':').map(|(u, _)| u).unwrap_or(&s).to_string())
}

/// `ADMIN`, `admin` and `DOMAIN\admin` all match `admin`.
fn user_matches(expected: &str, given: &str) -> bool {
    let g = given.rsplit('\\').next().unwrap_or(given);
    g.eq_ignore_ascii_case(expected)
}

// ---------------------------------------------------------------------------
// Socket <-> worker

pub enum Job {
    Frame(String),
    /// A page background task's child session ended (crate::pbt): the worker tells the client.
    Wake,
}

async fn run_socket(st: Arc<NstState>, socket: WebSocket, user: Option<String>) {
    let (mut tx, mut rx) = socket.split();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    // page background tasks wake the worker through the socket loop (crate::pbt), so the job
    // channel still closes with the socket
    let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let sid = st.next_session.fetch_add(1, Ordering::Relaxed);
    let st2 = st.clone();
    let spawned = std::thread::Builder::new().name(format!("nst-session-{sid}")).spawn(move || {
        let mut w = Worker::new(st2, sid, user, out_tx);
        w.wake = Some(wake_tx);
        w.run(job_rx)
    });
    if let Err(e) = spawned {
        eprintln!("bc-nst: cannot start session thread: {e}");
        return;
    }
    loop {
        tokio::select! {
            m = rx.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    if job_tx.send(Job::Frame(t.to_string())).is_err() { break; }
                }
                Some(Ok(Message::Binary(b))) => match String::from_utf8(b.to_vec()) {
                    Ok(t) => { if job_tx.send(Job::Frame(t)).is_err() { break; } }
                    Err(_) => eprintln!("bc-nst[{sid}]: ignoring non-UTF-8 binary frame"),
                },
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // ping/pong are answered by the WebSocket layer
                Some(Err(e)) => { eprintln!("bc-nst[{sid}]: socket error: {e}"); break; }
            },
            o = out_rx.recv() => match o {
                Some(text) => { if tx.send(Message::Text(text.into())).await.is_err() { break; } }
                None => break,
            },
            Some(()) = wake_rx.recv() => {
                if job_tx.send(Job::Wake).is_err() { break; }
            }
        }
    }
    // dropping job_tx ends the worker, which rolls back and drops the AL session
    drop(job_tx);
    eprintln!("bc-nst[{sid}]: socket closed");
}

// ---------------------------------------------------------------------------
// Session worker

use crate::form::{FormRows, PageForm};

struct Worker {
    st: Arc<NstState>,
    sid: i32,
    unique_id: String,
    out: tokio::sync::mpsc::UnboundedSender<String>,
    upgrade_user: Option<String>,
    user: Option<String>,
    opened: bool,
    lcid: i32,
    time_zone_id: String,
    client_settings: Option<Value>,
    company: Option<String>,
    al: Option<Session<'static>>,
    forms: HashMap<String, PageForm>,
    /// writes of the current call, for `State.Changes`
    writes: crate::track::SharedLog,
    /// shared with the session's UI host (Confirm / StrMenu round trips, Page.Run)
    link: Option<crate::ui::UiLink>,
    /// nested worker of a RunModal: the modal form's handle (kept on CloseForm)
    modal_keep: Option<String>,
    /// wakes the worker when a page background task ends (crate::pbt)
    wake: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    /// ClientActivityId of the call being handled
    activity: String,
}

static NULL: Value = Value::Null;

/// Positional JSON-RPC params, with a by-name fallback for object params.
struct Params<'a>(&'a Value);

impl<'a> Params<'a> {
    fn get(&self, i: usize, name: &str) -> &'a Value {
        match self.0 {
            Value::Array(a) => a.get(i).unwrap_or(&NULL),
            Value::Object(m) => m.get(name).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
}

type R = Result<Value, Fault>;

impl Worker {
    fn new(st: Arc<NstState>, sid: i32, upgrade_user: Option<String>, out: tokio::sync::mpsc::UnboundedSender<String>) -> Worker {
        let lcid = st.cfg.lcid;
        Worker {
            st,
            sid,
            unique_id: uuid::Uuid::new_v4().to_string(),
            out,
            upgrade_user,
            user: None,
            opened: false,
            lcid,
            time_zone_id: "UTC".into(),
            client_settings: None,
            company: None,
            al: None,
            forms: HashMap::new(),
            writes: Default::default(),
            link: None,
            modal_keep: None,
            wake: None,
            activity: NIL_GUID.to_string(),
        }
    }

    /// The user page usage states are kept for.
    fn usage_user(&self) -> String {
        self.user.clone().unwrap_or_else(|| self.st.cfg.user.clone()).to_uppercase()
    }

    /// `NavRecordState.Changes` for the writes this call made (real shape: data/capture/nst-real
    /// sample 14): one entry per table with the forms that show it; more than 100 writes to
    /// a table are reported as `ManyChanges`.
    fn take_changes(&mut self) -> Option<Value> {
        let writes = std::mem::take(&mut self.writes.borrow_mut().writes);
        if writes.is_empty() {
            return None;
        }
        let mut order: Vec<i64> = vec![];
        let mut by_table: HashMap<i64, Vec<&crate::track::Write>> = HashMap::new();
        for w in &writes {
            let t = w.schema.id;
            if !by_table.contains_key(&t) {
                order.push(t);
            }
            by_table.entry(t).or_default().push(w);
        }
        let out: Vec<Value> = order
            .iter()
            .map(|t| {
                let ws = &by_table[t];
                let handles: Vec<&String> = self.forms.iter().filter(|(_, f)| f.rec.as_ref().is_some_and(|r| r.borrow().tdef.id == *t)).map(|(h, _)| h).collect();
                let many = ws.len() > 100;
                let changes: Vec<Value> = if many {
                    vec![]
                } else {
                    ws.iter()
                        .map(|w| json!({"RecordId": base64::engine::general_purpose::STANDARD.encode(crate::dataset::encode_pk(w.schema.id, &w.key)), "Operation": w.op}))
                        .collect()
                };
                json!({"TableId": t, "IsTemporary": false, "FormHandles": handles, "ManyChanges": many, "Changes": changes})
            })
            .collect();
        Some(Value::Array(out))
    }

    fn run(mut self, jobs: mpsc::Receiver<Job>) {
        let link = crate::ui::UiLink {
            sid: self.sid,
            token: self.st.cfg.metadata_token,
            out: self.out.clone(),
            jobs: std::rc::Rc::new(jobs),
            pending: Default::default(),
            next_id: std::rc::Rc::new(std::cell::Cell::new(2)),
            opened: Default::default(),
            requests: Default::default(),
            st: self.st.clone(),
            writes: self.writes.clone(),
            pbt: Default::default(),
            wake: self.wake.clone(),
        };
        self.link = Some(link.clone());
        loop {
            // frames that arrived during a UI round trip come first, in order
            let next = link.pending.borrow_mut().pop_front();
            let text = match next {
                Some(t) => t,
                None => match link.jobs.recv() {
                    Ok(Job::Frame(t)) => t,
                    Ok(Job::Wake) => {
                        self.pbt_notify();
                        continue;
                    }
                    Err(_) => break,
                },
            };
            if let Some(reply) = self.handle_frame(&text) {
                if self.out.send(reply).is_err() {
                    break;
                }
            }
            // tasks that ended during the call: the client hears of them after its reply
            self.pbt_notify();
        }
        self.close_company();
    }

    /// One incoming JSON-RPC message; returns the reply frame for requests.
    fn handle_frame(&mut self, text: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("bc-nst[{}]: unparsable frame ({e}): {}", self.sid, clip(text, 200));
                return None;
            }
        };
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // a response to a server->client request outside a round trip (the `null` answer to a
            // PageBackgroundTaskCompletedNotification): only errors are worth a line
            if msg.get("error").is_some() || !msg.get("result").is_some_and(Value::is_null) {
                eprintln!("bc-nst[{}]: ignoring message without method: {}", self.sid, clip(text, 200));
            }
            return None;
        };
        if let Some(a) = msg.get("ClientActivityId").and_then(Value::as_str) {
            if uuid::Uuid::parse_str(a).is_ok() {
                self.activity = a.to_string();
            }
        }
        let params = msg.get("params").unwrap_or(&NULL);
        let t0 = Instant::now();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.call(method, Params(params))));
        let r = r.unwrap_or_else(|p| {
            let what = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            if let Some(s) = self.al.as_mut() {
                s.rollback();
            }
            Err(Fault::al(format!("Internal error in bc-rust while running {method}: {what}")))
        });
        // A client call is one transaction in BC: it ends committed or rolled back. A call that
        // still has one open here (a path that neither committed nor rolled back, or a locked
        // read) would keep its row locks until the next call; it is rolled back and logged.
        // Not in the nested worker of a RunModal: the AL code waiting for the modal page owns
        // that transaction (`run_modal`).
        if self.modal_keep.is_none() {
            if let Some(s) = self.al.as_mut() {
                if s.db.in_write() {
                    s.rollback();
                    eprintln!("bc-nst[{}]: {method} ended inside a transaction; rolled back", self.sid);
                }
            }
        }
        // a call that wrote nothing ends its read caches as a commit would (bc-store pg.rs): the
        // session's next call reads what other sessions committed meanwhile (I14 S1)
        if let Some(s) = self.al.as_mut() {
            if !s.db.in_write() {
                s.db.flush_read_cache(None);
            }
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let mut r = r;
        // page background tasks the call enqueued start now (crate::pbt)
        self.pbt_start();
        // pages AL opened with Page.Run: register their forms and queue the FormRunRequests
        if let Some(link) = self.link.clone() {
            let opened = std::mem::take(&mut *link.opened.borrow_mut());
            let reqs = std::mem::take(&mut *link.requests.borrow_mut());
            if let Ok(Value::Object(o)) = &mut r {
                for (h, f) in opened {
                    self.forms.insert(h, f);
                }
                if !reqs.is_empty() {
                    match o.get_mut("requests") {
                        Some(Value::Array(a)) => a.extend(reqs),
                        _ => {
                            o.insert("requests".into(), Value::Array(reqs));
                        }
                    }
                }
            }
        }
        // the call's writes go to the client as State.Changes (DataSetState for codeunit
        // responses); a fault rolled them back
        let changes = self.take_changes();
        if let (Ok(Value::Object(o)), Some(ch)) = (&mut r, changes) {
            for k in ["State", "DataSetState"] {
                if let Some(Value::Object(st)) = o.get_mut(k) {
                    st.insert("Changes".into(), ch);
                    break;
                }
            }
        }
        match &r {
            Ok(_) => eprintln!("bc-nst[{}]: {method} ok ({ms:.1} ms)", self.sid),
            Err(f) => {
                eprintln!("bc-nst[{}]: {method} fault ({ms:.1} ms): {}", self.sid, f.message());
                if std::env::var_os("BC_NST_TRACE_FAULTS").is_some() {
                    eprintln!("bc-nst[{}]:   params={}", self.sid, clip(&params.to_string(), 6000));
                }
            }
        }
        // requests carry an id; notifications get no reply
        let id = msg.get("id")?;
        let token = Some(self.st.cfg.metadata_token);
        Some(match r {
            Ok(v) => wire::rpc_result(id, v, token),
            Err(f) => wire::rpc_fault(id, &f, token),
        }
        .to_string())
    }

    fn call(&mut self, method: &str, p: Params) -> R {
        match method {
            "OpenConnection" => self.open_connection(p.get(0, "connectionRequest")),
            "SetClientSettings" => self.set_client_settings(p.get(0, "settings"), p.get(1, "saveOnly")),
            "OpenCompany" => self.open_company(p.get(0, "companyName").as_str(), p.get(1, "overridePersonalization").as_bool().unwrap_or(false)),
            "OpenForm" => self.open_form(p.get(0, "request")),
            "CloseForm" => self.close_form(p.get(0, "form")),
            "GetPage" => self.get_page(p.get(0, "pageRequestDefinition"), p.get(1, "state")),
            "CloseConnection" => self.close_connection(),
            "GetWorkDate" => self.get_work_date(),
            "SetWorkDate" => self.set_work_date(p.get(0, "workDate")),
            // trivial answers the web server asks for while building the shell
            // System Action Triggers (crate::platform): the subscribers answer
            "GetAutoStartTours" | "GetIsLegacyActionBarEnabled" => {
                let sid = self.sid;
                let ev = method.to_string();
                Ok(json!(system_action(self.al.as_mut(), sid, method, false, |s| crate::platform::is_enabled(s, &ev, false))))
            }
            "GetRoleCenterBannerPartId" => {
                let sid = self.sid;
                Ok(json!(system_action(self.al.as_mut(), sid, method, 0, crate::platform::role_center_banner_part)))
            }
            "GetNotificationStatus" => {
                let sid = self.sid;
                let id = p.get(0, "notificationId").as_str().unwrap_or(NIL_GUID).to_string();
                Ok(json!(system_action(self.al.as_mut(), sid, method, true, |s| crate::platform::notification_status(s, &id))))
            }
            "SetNotificationStatus" => {
                let sid = self.sid;
                let id = p.get(0, "notificationId").as_str().unwrap_or(NIL_GUID).to_string();
                let enable = p.get(1, "enable").as_bool().unwrap_or(true);
                system_action(self.al.as_mut(), sid, method, (), |s| crate::platform::set_notification_status(s, &id, enable));
                Ok(Value::Null)
            }
            "VerboseTraceLevelEnabled" | "FullALFunctionTracingEnabled" => Ok(json!(false)),
            // tours and teaching tips: PageUsageStates flags per user and object, set when the
            // user dismisses a tip (real tier: GetPageUsageState(22, 0) = 1 after that). The
            // web server asks during session start, where a fault ends the session.
            "GetPageUsageState" => {
                let key = (self.usage_user(), p.get(1, "objectType").as_i64().unwrap_or(0), p.get(0, "objectId").as_i64().unwrap_or(0));
                Ok(json!(self.st.page_usage.lock().map(|m| m.get(&key).copied().unwrap_or(0)).unwrap_or(0)))
            }
            "SetPageUsageState" => {
                let key = (self.usage_user(), p.get(1, "objectType").as_i64().unwrap_or(0), p.get(0, "objectId").as_i64().unwrap_or(0));
                let mask = p.get(2, "maskOfPageStatesToSet").as_i64().unwrap_or(0);
                let states = p.get(3, "pageStates").as_i64().unwrap_or(0);
                if let Ok(mut m) = self.st.page_usage.lock() {
                    let cur = m.get(&key).copied().unwrap_or(0);
                    m.insert(key, (cur & !mask) | (states & mask));
                }
                Ok(Value::Null)
            }
            // cancels a server-side progress dialog (Dialog.Open); bc-nst runs every call
            // to completion before answering, so there is never one to cancel
            "CancelDialog" => {
                eprintln!("bc-nst[{}]: CancelDialog: no server-side dialog is open (stub answer)", self.sid);
                Ok(Value::Null)
            }
            "HasObjectWithPageUsageState" => {
                let (user, ty) = (self.usage_user(), p.get(0, "objectType").as_i64().unwrap_or(0));
                let mask = p.get(1, "pageStatesToMask").as_i64().unwrap_or(0);
                let states = p.get(2, "pageStates").as_i64().unwrap_or(0);
                let any = self.st.page_usage.lock().map(|m| m.iter().any(|((u, t, _), v)| *u == user && *t == ty && (v & mask) == (states & mask))).unwrap_or(false);
                Ok(json!(any))
            }
            // control add-in assemblies with custom translations: none
            "GetAddInRegistrations" => Ok(json!([])),
            "GetAllowedCompanies" => self.get_allowed_companies(),
            "Evaluate" => self.evaluate(p.get(0, "toEvaluate").as_str().unwrap_or(""), p.get(1, "navType").as_i64().unwrap_or(0)),
            "EvaluateFilter" => self.evaluate_filter(p.get(0, "toEvaluate").as_str().unwrap_or(""), p.get(2, "objectId").as_i64().unwrap_or(0), p.get(3, "fieldId").as_i64().unwrap_or(0)),
            "InvokeApplicationMethod" => self.invoke_application_method(p.get(0, "applicationMethodRequest"), p.get(1, "state")),
            "InvokePageBackgroundTaskCompletion" => self.invoke_pbt_completion(p.get(0, "pageBackgroundTaskCompletionRequest"), p.get(1, "dataSetState")),
            "ValidateField" => self.validate_field(p.get(0, "fieldRequest"), p.get(1, "dataSetState")),
            "ActionField" => self.action_field(p.get(0, "fieldRequest"), p.get(1, "dataSetState")),
            "ModifyRecord" => self.modify_record(p.get(0, "dataSetState"), p.get(1, "recordRequest")),
            "RenameRecord" => self.rename_record(p.get(0, "dataSetState"), p.get(1, "recordRequest")),
            "EvaluateRelation" => self.evaluate_relation(p.get(0, "fieldRequest"), p.get(1, "state")),
            "LookupField" => self.field_action(p.get(0, "fieldRequest"), p.get(1, "dataSetState"), "lookup"),
            "DrillDownField" => self.field_action(p.get(0, "fieldRequest"), p.get(1, "dataSetState"), "drilldown"),
            "AssistEdit" => self.field_action(p.get(0, "fieldRequest"), p.get(1, "dataSetState"), "assistedit"),
            "DeleteRecord" => self.delete_record(p.get(0, "dataSetState"), p.get(2, "deleteSelectedRecords").as_bool().unwrap_or(false)),
            "InsertRecord" => self.insert_record(p.get(0, "dataSetState"), p.get(1, "recordRequest"), p.get(2, "belowXRecord").as_bool().unwrap_or(false)),
            "GetUsersAndAgents" => {
                let user = self.user.clone().unwrap_or_else(|| self.st.cfg.user.to_uppercase());
                let ds = users_and_agents(p.get(0, "getUsers").as_bool().unwrap_or(false), p.get(1, "getAgents").as_bool().unwrap_or(false), &user)
                    .map_err(|e| Fault::al(format!("GetUsersAndAgents: {e}")))?;
                let mut m = self.base_response();
                m.insert("dataSet".into(), serde_json::to_value(&ds).map_err(|e| Fault::al(format!("GetUsersAndAgents: {e}")))?);
                Ok(Value::Object(m))
            }
            _ => {
                // the request shape is what's needed to implement it: log it
                eprintln!("bc-nst[{}]: not implemented: {method} params={}", self.sid, clip(&p.0.to_string(), 4000));
                Err(Fault::not_implemented(method))
            }
        }
    }

    /// `ServerResponse` members. `VersionNumber` is 0 on the real NST (the metadata
    /// token travels separately as the top-level `MetadataToken`).
    fn base_response(&self) -> Map<String, Value> {
        wire::server_response(0)
    }

    // -- connection ---------------------------------------------------------

    fn open_connection(&mut self, req: &Value) -> R {
        let cfg = &self.st.cfg;
        let cred_user = req.pointer("/Credentials/UserName").and_then(Value::as_str);
        let mut user = match (cred_user, self.upgrade_user.as_deref()) {
            (Some(u), _) | (None, Some(u)) => u.to_string(),
            (None, None) => cfg.user.to_uppercase(),
        };
        if bc_server::auth::enabled() {
            // the user to run as: the validated upgrade user, or the user the Credentials authenticate
            let pw = req.pointer("/Credentials/Password").and_then(Value::as_str);
            user = crate::auth::open_connection(&self.st, cred_user, pw, self.upgrade_user.as_deref()).map_err(Fault::al)?;
        } else if !user_matches(&cfg.user, &user) {
            // own wording [src:X26]
            return Err(Fault::al(format!("User '{user}' cannot open a session on this server.")));
        } else {
            self.st.warn_auth(&user);
        }
        if let Some(l) = req.get("LCID").and_then(Value::as_i64).filter(|&l| l > 0) {
            self.lcid = l as i32;
        }
        if let Some(tz) = req.get("TimeZoneId").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            self.time_zone_id = tz.to_string();
        }
        let user_name = user.to_uppercase();
        self.user = Some(user_name.clone());
        self.opened = true;
        let server_version = req.get("ClientAssemblyFileVersion").and_then(Value::as_str).unwrap_or(&cfg.server_version).to_string();

        let mut m = self.base_response();
        m.insert("timeout".into(), json!(wire::timespan(24 * 3600)));
        m.insert("lcid".into(), json!(self.lcid));
        m.insert("defaultLcid".into(), json!(cfg.lcid));
        m.insert("localId".into(), json!(self.lcid));
        // "" on a single-tenant service tier (the real NST's value); "default" here would
        // make the web server put a tenant segment into the session id after sign-in but
        // not after a page navigation, and the second page could not find its session
        m.insert("tenantId".into(), json!(""));
        m.insert("tenantEnvironmentType".into(), json!(0)); // Production
        m.insert("licenseState".into(), json!(2)); // Paid: no trial/evaluation banners
        m.insert("companyName".into(), json!(cfg.company));
        m.insert("profileId".into(), json!(cfg.profile_id));
        m.insert("profileCaption".into(), json!(cfg.profile_caption));
        m.insert("profileAppId".into(), json!(cfg.profile_app_id));
        m.insert("configurationModeEnabled".into(), json!(false));
        m.insert("m365CollaborationLicense".into(), json!(false));
        // no personalization store yet
        m.insert("personalizationDisabled".into(), json!(true));
        m.insert("designerEnabled".into(), json!(false));
        m.insert("copilotAgentsEnabled".into(), json!(false));
        m.insert("maxUploadFileSize".into(), json!(5242880));
        // The time zone the session runs in. Only UTC for now; a client in another
        // zone gets a TimeZoneDataMismatch notice from the web server, not an error.
        let tz_id = if is_utc_id(&self.time_zone_id) { self.time_zone_id.clone() } else { "UTC".to_string() };
        m.insert("serializedTimeZoneInfo".into(), json!(wire::utc_time_zone(&tz_id)));
        m.insert("userProfileTimeZoneChanged".into(), json!(false));
        m.insert("userName".into(), json!(user_name));
        m.insert("userDisplayName".into(), json!(user.to_lowercase()));
        m.insert("isTenantAdmin".into(), json!(true));
        m.insert("userGuid".into(), json!(user_guid(&user_name)));
        m.insert("userTelemetryGuid".into(), json!(user_guid(&user_name)));
        m.insert("serverAssemblyFileVersion".into(), json!(server_version));
        m.insert("serverConnectionWarnings".into(), json!([]));
        m.insert("enableSaveToWordForRdlcReports".into(), json!(true));
        m.insert("enableSaveToExcelForRdlcReports".into(), json!(true));
        m.insert("enableSaveFromReportPreview".into(), json!(true));
        m.insert("useSimplifiedFilters".into(), json!(false));
        // the dataset encoder writes the non-compact NavDataSet format
        m.insert("enableCompactSerialization".into(), json!(false));
        m.insert("enabledFeatures".into(), json!(cfg.enabled_features));
        m.insert("runtimeTenantId".into(), json!(cfg.tenant_id));
        m.insert("ApplicationBuild".into(), json!(cfg.application_build));
        m.insert("ApplicationVersion".into(), json!(cfg.application_version));
        m.insert("ServerSessionId".into(), json!(self.sid));
        m.insert("ServerExternalSessionId".into(), json!(self.unique_id));
        m.insert("ServerUniqueSessionId".into(), json!(self.unique_id));
        m.insert("CalendarTwoDigitYearMax".into(), json!(2049));
        m.insert("DataSearchPageId".into(), json!(0));
        Ok(Value::Object(m))
    }

    fn set_client_settings(&mut self, settings: &Value, save_only: &Value) -> R {
        let cs = settings.get("ClientSettings").cloned().unwrap_or(Value::Null);
        if let Some(l) = cs.get("LCID").and_then(Value::as_i64).filter(|&l| l > 0) {
            if !save_only.as_bool().unwrap_or(false) {
                self.lcid = l as i32;
            }
        }
        self.client_settings = Some(cs);
        Ok(Value::Null)
    }

    fn close_connection(&mut self) -> R {
        self.close_company();
        self.opened = false;
        Ok(Value::Object(self.base_response()))
    }

    // -- company ------------------------------------------------------------

    fn close_company(&mut self) {
        self.forms.clear();
        if let Some(mut s) = self.al.take() {
            // Company Triggers.OnCompanyClose; its subscribers' writes are committed before
            // the session's open work is rolled back
            s.rollback();
            match crate::platform::company_close(&mut s) {
                Ok(()) => {
                    if let Err(e) = s.commit() {
                        eprintln!("bc-nst[{}]: cannot commit after OnCompanyClose: {e}", self.sid);
                    }
                }
                Err(e) => {
                    eprintln!("bc-nst[{}]: OnCompanyClose failed: {}", self.sid, crate::platform::flow_message(&e));
                    s.rollback();
                }
            }
        }
        self.company = None;
    }

    /// `companyName` null closes the company (the web server does this before
    /// CloseConnection); empty selects the default company.
    fn open_company(&mut self, name: Option<&str>, _override_personalization: bool) -> R {
        let st = self.st.clone();
        let cfg = &st.cfg;
        let mut m = self.base_response();
        // `Permissions` is required even when closing: the web server wraps it in a ReadOnlyDictionary
        m.insert("Permissions".into(), permissions());
        m.insert("PrivacyNoticeStates".into(), json!({}));
        m.insert("PowerPlatformEnvironment".into(), json!({}));
        let Some(name) = name else {
            self.close_company();
            return Ok(Value::Object(m));
        };
        if !self.opened {
            return Err(Fault::al("The connection is not open. Call OpenConnection first."));
        }
        let name = if name.is_empty() { cfg.company.as_str() } else { name };
        if !name.eq_ignore_ascii_case(&cfg.company) {
            return Err(Fault::al(format!("The company \"{name}\" does not exist.")));
        }
        self.close_company();
        let s = Session::try_new(self.st.rt, &cfg.company).map_err(|e| Fault::al(format!("Cannot open company {}: {e}", cfg.company)))?;
        let mut s = s;
        if let Some(u) = &self.user {
            // UserId() and UserSecurityId(): the User row of that name (al_runtime::userauth)
            al_runtime::userauth::set_session_user(&mut s, u);
            // an authenticated user (BC_AUTH): their permission sets (enforced with BC_PERMISSIONS=enforce)
            if bc_server::auth::enabled() {
                s.login_permissions();
            }
        } else {
            al_runtime::userauth::resolve_session_user(&mut s);
        }
        // a web client session: GuiAllowed() is true, as on the service tier
        s.gui_allowed = true;
        // page background tasks run in child sessions of their own (crate::pbt)
        s.sys.pbt.host = self.wake.is_some();
        // log writes for NavRecordState.Changes (crate::track)
        let inner = std::mem::replace(&mut s.db, Box::new(crate::track::NoConn));
        s.db = Box::new(crate::track::TrackingConn::wrap(inner, self.writes.clone()));
        // Confirm / StrMenu / Page.Run go to the web client (crate::ui)
        if let Some(link) = &self.link {
            s.ui = Some(Box::new(crate::ui::NstUi { link: link.clone() }));
        }
        // Company Triggers (crate::platform): OnCompanyOpen fails the open on an error;
        // OnCompanyOpenCompleted is isolated, so its errors are only logged
        if let Err(e) = crate::platform::company_open(&mut s) {
            s.rollback();
            eprintln!("bc-nst[{}]: OnCompanyOpen failed: {}", self.sid, crate::platform::flow_message(&e));
            return Err(Fault::from(e));
        }
        s.commit().map_err(|e| Fault::al(format!("Cannot commit after OnCompanyOpen: {e}")))?;
        if let Some(err) = crate::platform::company_open_completed(&mut s) {
            eprintln!("bc-nst[{}]: OnCompanyOpenCompleted subscriber failed (isolated, rolled back): {err}", self.sid);
        }
        let _ = s.commit();
        // the company badge (UI Helper Triggers.GetSystemIndicator)
        let indicator = system_indicator(&mut s, self.sid);
        self.al = Some(s);
        self.company = Some(cfg.company.clone());
        // the work date goes to the web server as a queued WorkDateChangedRequest
        let wd = self.get_work_date()?;
        m.insert(
            "requests".into(),
            json!([{"$polyType": "Microsoft.Dynamics.Nav.Types.WorkDateChangedRequest", "obj": {"WorkDate": wd, "AutomationId": NIL_GUID}}]),
        );
        m.insert("Indicator".into(), json!({"text": indicator.0, "style": indicator.1}));
        m.insert("CompanyName".into(), json!(cfg.company));
        m.insert("CompanyDisplayName".into(), json!(cfg.company_display_name));
        m.insert("CompanyTableId".into(), json!(name_guid("company", &cfg.company)));
        m.insert("EvaluationCompany".into(), json!(false));
        Ok(Value::Object(m))
    }

    // -- forms --------------------------------------------------------------

    /// `OpenForm(OpenFormRequest)`. One `States` entry per requested form (main
    /// form first when `HasMainForm`, then parts). Each returned state starts from
    /// the request's state, since the web server replaces its copy with ours.
    /// `ControlIds[i]` is the part control (on the main form) that hosts form i.
    fn open_form(&mut self, req: &Value) -> R {
        let states = req.get("States").and_then(Value::as_array).cloned().unwrap_or_default();
        let control_ids: Vec<Option<i32>> = req
            .get("ControlIds")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_i64().map(|n| n as i32))).collect())
            .unwrap_or_default();
        let has_main = req.get("HasMainForm").and_then(Value::as_bool).unwrap_or(false);
        let main_handle = req.get("MainFormHandle").and_then(Value::as_str).filter(|h| *h != NIL_GUID).map(str::to_lowercase);
        if self.al.is_none() {
            return Err(Fault::al("No company is open. Call OpenCompany first."));
        }
        let mut out_states = Vec::with_capacity(states.len());
        let mut captions = Vec::with_capacity(states.len());
        let mut props = Vec::with_capacity(states.len());
        let mut apps = Vec::with_capacity(states.len());
        let mut opened: Vec<String> = vec![];
        let mut main_of_parts = main_handle.clone();
        for (i, state) in states.iter().enumerate() {
            let is_main = has_main && i == 0;
            let part_control = if is_main { None } else { control_ids.get(i).copied().flatten() };
            let r = self.open_one(state, if is_main { None } else { main_of_parts.clone() }, part_control);
            let (st, page_id) = match r {
                Ok(x) => x,
                Err(f) => {
                    // a failure here (unknown page) fails the whole call; undo what was opened
                    for h in opened {
                        self.forms.remove(&h);
                    }
                    return Err(f);
                }
            };
            if let Some(h) = st.get("ServerFormHandle").and_then(Value::as_str) {
                if self.forms.contains_key(h) {
                    opened.push(h.to_string());
                    if is_main {
                        main_of_parts = Some(h.to_string());
                    }
                }
            }
            let page = self.st.rt.cat.pages.get(&page_id);
            captions.push(json!({"Captions": []}));
            props.push(form_properties(page));
            apps.push(app_information(page_id));
            out_states.push(st);
        }
        // DataSet: the first form's record, in a table named "0" (as the real NST does)
        let first = out_states.first().and_then(|st| st.get("ServerFormHandle")).and_then(Value::as_str).map(str::to_string);
        let ds = match first.as_deref().and_then(|h| self.forms.get(h)).zip(self.al.as_mut()) {
            Some((f, s)) => match f.record_dataset(s) {
                Ok(mut ds) => {
                    if let Some(t) = ds.tables.first_mut() {
                        t.name = "0".into();
                    }
                    serde_json::to_value(&ds).unwrap_or_else(|_| crate::dataset::NavDataSet::empty_json())
                }
                Err(e) => {
                    eprintln!("bc-nst[{}]: OpenForm record data set: {}", self.sid, e.msg);
                    crate::dataset::NavDataSet::empty_json()
                }
            },
            None => crate::dataset::NavDataSet::empty_json(),
        };
        let mut m = self.base_response();
        m.insert("States".into(), Value::Array(out_states));
        m.insert("DataSet".into(), ds);
        m.insert("TableCaptions".into(), Value::Array(captions));
        m.insert("FormProperties".into(), Value::Array(props));
        m.insert("AppInformation".into(), Value::Array(apps));
        Ok(Value::Object(m))
    }

    /// Create the page instance for one requested state and run OnInit and
    /// OnOpenPage. A part reuses the instance the main page's code reaches through
    /// `CurrPage.<Part>.Page`. An AL error goes back in the state's
    /// `FormOpenExceptionData` (the web server rethrows it when it builds the page),
    /// not as a call fault.
    fn open_one(&mut self, state: &Value, main: Option<String>, part_control: Option<i32>) -> Result<(Value, i64), Fault> {
        let mut st = state.as_object().cloned().unwrap_or_default();
        let page_id = st.get("FormId").and_then(Value::as_i64).unwrap_or(0);
        let cat = self.st.rt.cat;
        // a form AL already opened (Page.Run's FormRunRequest, RunModal's FormRunModal): the
        // client attaches to it with its handle; OnOpenPage has run and the record is set
        let existing = st.get("ServerFormHandle").and_then(Value::as_str).map(str::to_lowercase).filter(|h| h != NIL_GUID && self.forms.get(h).is_some_and(|f| f.page_id == page_id));
        if let Some(h) = existing {
            let s = self.al.as_mut().ok_or_else(|| Fault::al("No company is open."))?;
            let f = &self.forms[&h];
            let in_db = f.rec.as_ref().is_some_and(|r| s.rec_get_pk(r).is_some());
            st.insert("RecordState".into(), json!(if in_db { 0x40 | 0x20 } else { 0x100 }));
            if let Some(rec) = &f.rec {
                if in_db {
                    let r = rec.borrow();
                    st.insert("CurrentRecord".into(), json!(base64::engine::general_purpose::STANDARD.encode(crate::dataset::encode_bookmark(&r.schema, &r.row))));
                }
            }
            f.finish_state(s, &mut st);
            eprintln!("bc-nst[{}]: OpenForm attaches to page {page_id} ({h})", self.sid);
            return Ok((Value::Object(st), page_id));
        }
        let Some(page) = cat.pages.get(&page_id) else {
            return Err(Fault::al(format!("The page {page_id} does not exist.")));
        };
        // the part's instance, shared with the main page's CurrPage.<Part>
        let shared = match (main.as_ref().and_then(|h| self.forms.get(h)), part_control) {
            (Some(mf), Some(cid)) => mf.control(cid).filter(|c| c.kind == "part").map(|c| {
                let existing = mf.inst.parts.borrow().iter().find(|(n, _)| n.eq_ignore_ascii_case(&c.name)).map(|(_, x)| x.clone());
                existing.filter(|x| x.code == page.code).unwrap_or_else(|| {
                    let x = Instance::new(page.code);
                    mf.inst.parts.borrow_mut().retain(|(n, _)| !n.eq_ignore_ascii_case(&c.name));
                    mf.inst.parts.borrow_mut().push((c.name.as_str().into(), x.clone()));
                    x
                })
            }),
            _ => None,
        };
        let s = self.al.as_mut().expect("company open");
        let form = PageForm::new(s, page, shared, main);
        let handle = uuid::Uuid::new_v4().to_string();
        st.insert("ServerFormHandle".into(), json!(handle));
        st.insert("FormId".into(), json!(page_id));
        if !page.caption.is_empty() && st.get("PageCaption").is_none_or(Value::is_null) {
            st.insert("PageCaption".into(), json!(page.caption));
        }
        if let Some(tid) = page.source_table {
            let tv = st.entry("TableView").or_insert_with(|| json!({}));
            if let Some(o) = tv.as_object_mut() {
                if o.get("TableId").and_then(Value::as_i64).unwrap_or(0) == 0 {
                    o.insert("TableId".into(), json!(tid));
                }
                if o.get("CurrentSortingFieldIds").is_none_or(|v| v.as_array().is_none_or(|a| a.is_empty())) {
                    if let Some(t) = cat.tables.get(&tid) {
                        let ids: Vec<i64> = t.pk().fields.iter().map(|&fi| t.fields[fi].id).collect();
                        o.insert("CurrentSortingFieldIds".into(), json!(ids));
                    }
                }
                o.entry("Ascending").or_insert(json!(true));
            }
        }
        if let Err(e) = form.open(s) {
            s.rollback();
            let f = Fault::from(&e);
            eprintln!("bc-nst[{}]: opening page {page_id} failed: {}", self.sid, f.message());
            st.insert("FormOpenExceptionData".into(), json!(f.exception_data()));
            return Ok((Value::Object(st), page_id));
        }
        // RunFormOnRec / CurrentRecord: the record the page opens on (a drill-down from a
        // list); otherwise an initialized record. Either goes back as OpenForm's DataSet.
        let bookmark = st.get("CurrentRecord").and_then(Value::as_str).and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok()).filter(|b| !b.is_empty());
        let in_db = match &bookmark {
            Some(b) => form.position_on_bookmark(s, b),
            None => {
                form.init_record(s);
                false
            }
        };
        st.insert("RunFormOnRec".into(), json!(false));
        st.insert("RecordState".into(), json!(if in_db { 0x40 | 0x20 } else { 0x100 }));
        if let Some(rec) = &form.rec {
            let r = rec.borrow();
            st.insert("CurrentRecord".into(), json!(base64::engine::general_purpose::STANDARD.encode(crate::dataset::encode_bookmark(&r.schema, &r.row))));
        }
        form.finish_state(s, &mut st);
        if let Err(e) = s.commit() {
            return Err(Fault::al(format!("Cannot commit after opening page {page_id}: {e}")));
        }
        self.forms.insert(handle, form);
        Ok((Value::Object(st), page_id))
    }

    /// `CloseForm(CloseFormRequest)`: run OnClosePage, drop the page instances, return
    /// one state per form.
    fn close_form(&mut self, req: &Value) -> R {
        let states = req.get("States").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut out = Vec::with_capacity(states.len());
        for state in states {
            if let Some(h) = state.get("ServerFormHandle").and_then(Value::as_str) {
                match self.forms.remove(&h.to_lowercase()) {
                    Some(f) => {
                        // a RunModal's form: the AL code that waits reads the selection from it
                        // after EndClientCall, so it stays, positioned on the client's record
                        if self.modal_keep.as_deref() == Some(h.to_lowercase().as_str()) {
                            if let (Some(s), Some(b)) = (self.al.as_mut(), state.get("CurrentRecord").and_then(Value::as_str).and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())) {
                                f.position_on_bookmark(s, &b);
                            }
                            eprintln!("bc-nst[{}]: modal page {} closed by the client ({h})", self.sid, f.page_id);
                            self.forms.insert(h.to_lowercase(), f);
                            out.push(state);
                            continue;
                        }
                        // its page background tasks are cancelled (crate::pbt)
                        if let Some(link) = &self.link {
                            let ids = link.pbt.borrow_mut().cancel_form(&h.to_lowercase());
                            if let Some(s) = self.al.as_mut() {
                                for id in ids {
                                    s.sys.pbt.live.remove(&id);
                                }
                            }
                        }
                        if let Some(s) = self.al.as_mut() {
                            if let Err(e) = f.trigger(s, "OnClosePage", &[]) {
                                s.rollback();
                                eprintln!("bc-nst[{}]: OnClosePage of page {} failed: {}", self.sid, f.page_id, Fault::from(e).message());
                            } else {
                                let _ = s.commit();
                            }
                        }
                        eprintln!("bc-nst[{}]: closed page {} ({h})", self.sid, f.page_id)
                    }
                    None => eprintln!("bc-nst[{}]: CloseForm for unknown form handle {h}", self.sid),
                }
            }
            out.push(state);
        }
        let mut m = self.base_response();
        m.insert("States".into(), Value::Array(out));
        Ok(Value::Object(m))
    }

    /// `GetPage(PageRequestDefinition, NavRecordState)` → `DataSetResponse`.
    /// Rows come from the open form's `Rec` (SourceTableView + OnOpenPage) when the
    /// state names a form this session opened: each row runs OnAfterGetRecord on the
    /// page and carries the page's expression columns. Without a form, rows come
    /// from the page in `FormId`, else from the table in `TableView.TableId`
    /// (form-less reads such as All Profile).
    fn get_page(&mut self, def: &Value, state: &Value) -> R {
        use crate::dataset::{self, GetPageRequest};
        let req = GetPageRequest::from_params(&json!([def, state])).map_err(|e| Fault::al(format!("GetPage: bad request: {e}")))?;
        let handle = state.get("ServerFormHandle").and_then(Value::as_str).map(str::to_lowercase).filter(|h| h != NIL_GUID);
        let Worker { forms, al, .. } = self;
        let s = al.as_mut().ok_or_else(|| Fault::al("No company is open. Call OpenCompany first."))?;
        let form = handle.as_ref().and_then(|h| forms.get(h));
        let mut def = req.page_request_definition.clone();
        let r = match form {
            Some(f) => match f.rec.clone() {
                Some(rec) => {
                    // FlowFields bound to the page's controls are always calculated
                    let tdef = rec.borrow().tdef;
                    let calc = def.calc_fields.get_or_insert_with(Vec::new);
                    for &fi in &f.statics.flow_fields {
                        let id = tdef.fields[fi].id as i32;
                        if !calc.contains(&id) {
                            calc.push(id);
                        }
                    }
                    dataset::read_page(s, &rec, &def, &req.state, &mut FormRows { form: f, after_get: true })
                }
                None => dataset::get_page(s, f.page_id, &req),
            },
            // IsResourceDefinedForm: FormId names a system form (Links 3, Notes 1 on table
            // 2000000068), not an AL page; read the table in TableView instead
            None if state.get("IsResourceDefinedForm").and_then(Value::as_bool) == Some(true) => dataset::get_page(s, 0, &req),
            None => dataset::get_page(s, req.state.form_id as i64, &req),
        };
        let res = match r {
            Ok(res) => res,
            Err(e) => {
                s.rollback();
                return Err(Fault::from(&e));
            }
        };
        let mut out_state = serde_json::to_value(&res.state).map_err(|e| Fault::al(format!("GetPage: {e}")))?;
        if let (Some(f), Some(o)) = (form, out_state.as_object_mut()) {
            f.finish_state(s, o);
        }
        let _ = s.commit();
        let mut m = self.base_response();
        m.insert("dataSet".into(), serde_json::to_value(&res.dataset).map_err(|e| Fault::al(format!("GetPage: {e}")))?);
        m.insert("State".into(), out_state);
        Ok(Value::Object(m))
    }

    /// The open form a request's state names (`ServerFormHandle`).
    fn form_of(&self, state: &Value) -> Result<String, Fault> {
        let h = state.get("ServerFormHandle").and_then(Value::as_str).map(str::to_lowercase).unwrap_or_default();
        if self.forms.contains_key(&h) {
            Ok(h)
        } else {
            Err(Fault::al(format!("The page is not open on the server (form handle {h}).")))
        }
    }

    /// `CodeUnitResponse` for a form call: the page's `Rec` as a one-row data set
    /// (with expression columns) and the state with `RecordState` flags.
    fn codeunit_response(&mut self, handle: &str, state: &Value, record_state: i64, args: &Value) -> R {
        let Worker { forms, al, .. } = self;
        let s = al.as_mut().ok_or_else(|| Fault::al("No company is open."))?;
        let f = forms.get(handle).ok_or_else(|| Fault::al("The page is not open on the server."))?;
        let ds = f.record_dataset(s).map_err(|e| Fault::from(&e))?;
        let mut st = state.as_object().cloned().unwrap_or_default();
        st.insert("RecordState".into(), json!(record_state));
        f.finish_state(s, &mut st);
        if let Some(rec) = &f.rec {
            let r = rec.borrow();
            st.insert("CurrentRecord".into(), json!(base64::engine::general_purpose::STANDARD.encode(crate::dataset::encode_bookmark(&r.schema, &r.row))));
        }
        let msgs = std::mem::take(&mut s.messages);
        let mut m = self.base_response();
        m.insert("requests".into(), dialog_messages(msgs));
        m.insert("Args".into(), args.clone());
        m.insert("DataSetState".into(), Value::Object(st));
        m.insert("DataSet".into(), serde_json::to_value(&ds).map_err(|e| Fault::al(format!("{e}")))?);
        m.insert("ReturnValue".into(), Value::Null);
        Ok(Value::Object(m))
    }

    /// Run `body` against the form, rolling back on an AL error and committing otherwise.
    fn with_form<T>(&mut self, handle: &str, body: impl FnOnce(&mut Session<'static>, &PageForm) -> Result<T, Flow>) -> Result<T, Fault> {
        let Worker { forms, al, .. } = self;
        let s = al.as_mut().ok_or_else(|| Fault::al("No company is open."))?;
        let f = forms.get(handle).ok_or_else(|| Fault::al("The page is not open on the server."))?;
        match body(s, f) {
            Ok(v) => {
                s.commit().map_err(|e| Fault::al(format!("Cannot commit: {e}")))?;
                Ok(v)
            }
            Err(e) => {
                s.rollback();
                Err(Fault::from(e))
            }
        }
    }

    /// `InvokeApplicationMethod(ApplicationMethodRequest, NavRecordState)`. The web
    /// client calls page triggers this way: `AfterGetCurrRecord` (positions the page
    /// on the record in `DataSet`, runs OnAfterGetRecord and OnAfterGetCurrRecord) and
    /// `NewRecord` (OnNewRecord on an initialized record).
    fn invoke_application_method(&mut self, req: &Value, state: &Value) -> R {
        let method = req.get("MethodName").and_then(Value::as_str).unwrap_or("");
        let code_type = req.get("ApplicationCodeType").and_then(Value::as_i64).unwrap_or(-1);
        let args = req.get("Args").cloned().unwrap_or(Value::Null);
        if code_type != 1 {
            eprintln!("bc-nst[{}]: not implemented: InvokeApplicationMethod type {code_type} {method}", self.sid);
            return Err(Fault::not_implemented(&format!("InvokeApplicationMethod({code_type}, {method})")));
        }
        let h = self.form_of(state)?;
        let ds = client_dataset(req.get("DataSet"))?;
        let st = state.clone();
        self.with_form(&h, |s, f| {
            if let Some(rec) = &f.rec {
                crate::form::apply_state_filter_groups(s, rec, &st);
            }
            Ok(())
        })?;
        match method {
            "AfterGetCurrRecord" => {
                self.with_form(&h, |s, f| {
                    if let Some(ds) = &ds {
                        f.load_client_record(s, ds, None)?;
                    }
                    f.calc_flow_fields(s)?;
                    f.trigger(s, "OnAfterGetRecord", &[])?;
                    f.trigger(s, "OnAfterGetCurrRecord", &[])?;
                    Ok(())
                })?;
                self.codeunit_response(&h, state, 0x40, &args)
            }
            "NewRecord" => {
                let below = args.as_array().and_then(|a| a.first()).and_then(|v| v.get("value")).and_then(Value::as_bool).unwrap_or(false);
                self.with_form(&h, |s, f| f.new_record(s, ds.as_ref(), below))?;
                self.codeunit_response(&h, state, 0, &args)
            }
            _ => {
                eprintln!("bc-nst[{}]: not implemented: InvokeApplicationMethod {method}", self.sid);
                Err(Fault::not_implemented(&format!("InvokeApplicationMethod({method})")))
            }
        }
    }

    /// Start the page background tasks the last call enqueued (crate::pbt).
    fn pbt_start(&mut self) {
        let (Some(link), Some(wake)) = (self.link.clone(), self.wake.clone()) else { return };
        let Worker { al, forms, st, activity, sid, .. } = self;
        let Some(s) = al.as_mut() else { return };
        link.pbt.borrow_mut().start_queued(st.rt, s, forms, activity, &wake, *sid);
    }

    /// Tell the client about the page background tasks that ended:
    /// `PageBackgroundTaskCompletedNotification`, a request it answers with `null`.
    fn pbt_notify(&mut self) {
        let Some(link) = self.link.clone() else { return };
        let ended = link.pbt.borrow_mut().collect(self.sid);
        for (handle, task, activity) in ended {
            let id = link.next_id.get();
            link.next_id.set(id + 1);
            let msg = wire::rpc_request(&json!(id), "PageBackgroundTaskCompletedNotification", json!([{"FormHandle": handle, "TaskId": task, "ClientActivityId": activity}]), self.st.cfg.metadata_token);
            let _ = self.out.send(msg.to_string());
        }
    }

    /// `InvokePageBackgroundTaskCompletion(PageBackgroundTaskCompletionRequest, NavRecordState)`:
    /// the completion trigger of a finished task (OnPageBackgroundTaskCompleted, or
    /// OnPageBackgroundTaskError and an unhandled error as a fault), then the page's record as
    /// `{requests, DataSetState, DataSet, VersionNumber}`. A task that is unknown, cancelled or
    /// not finished runs nothing (NavForm.InvokePageBackgroundTaskCompletionAsync).
    fn invoke_pbt_completion(&mut self, req: &Value, state: &Value) -> R {
        let h = self.form_of(state)?;
        let id = req.get("TaskId").and_then(Value::as_i64).unwrap_or(0) as i32;
        let task = self.link.as_ref().and_then(|l| l.pbt.borrow_mut().take(&h, id));
        if let Some(t) = task {
            if let Some(outcome) = t.outcome {
                self.with_form(&h, |s, f| {
                    let insts: Vec<std::rc::Rc<Instance>> = std::iter::once(f.inst.clone()).chain(f.ext.iter().cloned()).collect();
                    s.pbt_complete(&insts, f.rec.as_ref(), t.bookmark.as_deref(), id, outcome, t.level).map(|_| ())
                })?;
            }
        }
        let pending = self.link.as_ref().map(|l| l.pbt.borrow().pending(&h)).unwrap_or_default();
        let mut r = self.codeunit_response(&h, state, 0x40, &Value::Null)?;
        if let Value::Object(o) = &mut r {
            o.remove("Args");
            o.remove("ReturnValue");
            if let Some(Value::Object(st)) = o.get_mut("DataSetState") {
                st.insert("PendingBackgroundTasks".into(), json!(pending));
            }
        }
        Ok(r)
    }

    /// `ValidateField(FieldRequest, NavRecordState)`: the page's `Rec` takes the
    /// client's record, then `Rec.Validate(<field>, <new value>)` (table OnValidate,
    /// table relation check, events) and the control's OnValidate run. Nothing is
    /// saved here; the web client sends ModifyRecord / InsertRecord afterwards.
    fn validate_field(&mut self, freq: &Value, state: &Value) -> R {
        let h = self.form_of(state)?;
        let control_id = freq.get("ControlId").and_then(Value::as_i64).unwrap_or(0) as i32;
        let ds = client_dataset(freq.get("RecDataSet"))?.ok_or_else(|| Fault::al("ValidateField without a record."))?;
        let st = state.clone();
        let in_db = self
            .with_form(&h, |s, f| {
                let c = f.control(control_id).ok_or_else(|| Flow::Err(Box::new(al_runtime::AlError::new(format!("Page {} has no control {control_id}.", f.page_id)))))?;
                let defaults = f.rec.as_ref().map(|r| crate::form::state_defaults(s, r, &st)).unwrap_or_default();
                let in_db = f.validate_control_with(s, &ds, c, &defaults)?;
                f.calc_flow_fields(s)?;
                Ok(in_db)
            })
            .map_err(Fault::validate_field)?;
        let form_id = self.forms.get(&h).map(|f| f.page_id).unwrap_or(0);
        let mut r = self.codeunit_response(&h, state, if in_db { 0x40 } else { 0 }, &Value::Null)?;
        if let Some(o) = r.as_object_mut() {
            o.insert("ClientContext".into(), json!({"FormId": form_id, "ControlId": control_id}));
        }
        Ok(r)
    }

    /// `ModifyRecord(NavRecordState, RecordRequest)`: OnModifyRecord, then
    /// `Rec.Modify(true)` (table OnModify).
    fn modify_record(&mut self, state: &Value, rreq: &Value) -> R {
        let h = self.form_of(state)?;
        let ds = client_dataset(rreq.get("RecDataSet"))?.ok_or_else(|| Fault::al("ModifyRecord without a record."))?;
        self.with_form(&h, |s, f| f.modify(s, &ds))?;
        self.dataset_response(&h, state, 0x2 | 0x40)
    }

    /// `RenameRecord(NavRecordState, RecordRequest)`: the client sends it when the
    /// primary key in its row differs from its bookmark. bc-rust handles the case it
    /// causes itself (the record already exists under the key in the row: the bookmark
    /// predates the insert that numbered it) as a modify and reports Rename, so the
    /// client takes the new bookmark. A real key change (AL `Rename`) is not
    /// implemented yet.
    fn rename_record(&mut self, state: &Value, rreq: &Value) -> R {
        let h = self.form_of(state)?;
        let ds = client_dataset(rreq.get("RecDataSet"))?.ok_or_else(|| Fault::al("RenameRecord without a record."))?;
        self.with_form(&h, |s, f| f.modify(s, &ds))?;
        self.dataset_response(&h, state, 0x8 | 0x40)
    }

    /// `EvaluateRelation(FieldRequest, NavRecordState)` → `EvaluateRelationResponse`:
    /// which branch of a conditional table relation (`CDD_1` order) applies to the
    /// client's record, e.g. Sales Line "No." with Type = Item.
    fn evaluate_relation(&mut self, freq: &Value, state: &Value) -> R {
        let h = self.form_of(state)?;
        let field_id = freq.get("FieldId").and_then(Value::as_i64).unwrap_or(0);
        let ds = client_dataset(freq.get("RecDataSet"))?;
        let st = state.clone();
        let idx = self.with_form(&h, |s, f| {
            let Some(rec) = f.rec.clone() else { return Ok(-1) };
            let defaults = crate::form::state_defaults(s, &rec, &st);
            if let Some(ds) = &ds {
                f.load_client_record_with(s, ds, None, &defaults)?;
            }
            let fi = rec.borrow().tdef.field_no(field_id);
            match fi {
                Some(fi) => f.relation_index(s, fi),
                None => Ok(-1),
            }
        })?;
        let mut m = self.base_response();
        m.insert("RelationIndex".into(), json!(idx));
        Ok(Value::Object(m))
    }

    /// `InsertRecord(NavRecordState, RecordRequest, belowXRec, isEmptyMultipleLine)`:
    /// OnInsertRecord, then `Rec.Insert(true)` (table OnInsert).
    fn insert_record(&mut self, state: &Value, rreq: &Value, below: bool) -> R {
        let h = self.form_of(state)?;
        let ds = client_dataset(rreq.get("RecDataSet"))?.ok_or_else(|| Fault::al("InsertRecord without a record."))?;
        let st = state.clone();
        self.with_form(&h, |s, f| {
            let defaults = f.rec.as_ref().map(|r| crate::form::state_defaults(s, r, &st)).unwrap_or_default();
            f.insert(s, &ds, below, &defaults)
        })?;
        self.dataset_response(&h, state, 0x1 | 0x40)
    }

    /// `DeleteRecord(NavRecordState, RecordRequest, deleteSelectedRecords)` →
    /// `RecordDeletedResponse {DeletedRecords: [bookmark…], State}`. The records are the
    /// state's `SelectedRecords` (when asked to delete the selection) or its `CurrentRecord`.
    fn delete_record(&mut self, state: &Value, selected: bool) -> R {
        let h = self.form_of(state)?;
        let b64 = |v: &Value| v.as_str().and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok());
        let mut marks: Vec<Vec<u8>> = vec![];
        if selected || state.get("CurrentRecord").is_none_or(Value::is_null) {
            marks.extend(state.get("SelectedRecords").and_then(Value::as_array).into_iter().flatten().filter_map(b64));
        }
        if marks.is_empty() {
            marks.extend(state.get("CurrentRecord").and_then(b64));
        }
        if marks.is_empty() {
            return Err(Fault::al("There is no record to delete."));
        }
        let deleted = self.with_form(&h, |s, f| {
            let mut out = vec![];
            for m in &marks {
                if f.delete(s, m)? {
                    out.push(m.clone());
                }
            }
            Ok(out)
        })?;
        eprintln!("bc-nst[{}]: DeleteRecord: {} of {} record(s) deleted", self.sid, deleted.len(), marks.len());
        let mut st = state.as_object().cloned().unwrap_or_default();
        st.insert("RecordState".into(), json!(0x04));
        st.insert("SelectedRecords".into(), json!([]));
        let msgs = self.al.as_mut().map(|s| std::mem::take(&mut s.messages)).unwrap_or_default();
        let mut m = self.base_response();
        m.insert("requests".into(), dialog_messages(msgs));
        m.insert("DeletedRecords".into(), Value::Array(deleted.iter().map(|d| json!(base64::engine::general_purpose::STANDARD.encode(d))).collect()));
        m.insert("State".into(), Value::Object(st));
        Ok(Value::Object(m))
    }

    /// `DataSetResponse` with the page's `Rec` and the state with `RecordState` flags.
    fn dataset_response(&mut self, handle: &str, state: &Value, record_state: i64) -> R {
        let Worker { forms, al, .. } = self;
        let s = al.as_mut().ok_or_else(|| Fault::al("No company is open."))?;
        let f = forms.get(handle).ok_or_else(|| Fault::al("The page is not open on the server."))?;
        let ds = f.record_dataset(s).map_err(|e| Fault::from(&e))?;
        let mut st = state.as_object().cloned().unwrap_or_default();
        st.insert("RecordState".into(), json!(record_state));
        f.finish_state(s, &mut st);
        if let Some(rec) = &f.rec {
            let r = rec.borrow();
            st.insert("CurrentRecord".into(), json!(base64::engine::general_purpose::STANDARD.encode(crate::dataset::encode_bookmark(&r.schema, &r.row))));
        }
        let msgs = std::mem::take(&mut s.messages);
        let mut m = self.base_response();
        m.insert("requests".into(), dialog_messages(msgs));
        m.insert("dataSet".into(), serde_json::to_value(&ds).map_err(|e| Fault::al(format!("{e}")))?);
        m.insert("State".into(), Value::Object(st));
        Ok(Value::Object(m))
    }

    /// `ActionField(FieldRequest, NavRecordState)`: an action on the page. The page's
    /// `Rec` takes the client's current record, then the action's OnAction trigger runs
    /// with the page instance (page or page extension code). The reply carries the
    /// record as it is afterwards (re-read; gone if the action deleted it, as posting
    /// does) and the Message() texts as DialogMessageRequests.
    fn action_field(&mut self, freq: &Value, state: &Value) -> R {
        let h = self.form_of(state)?;
        let control_id = freq.get("ControlId").and_then(Value::as_i64).unwrap_or(0) as i32;
        let ds = client_dataset(freq.get("RecDataSet"))?;
        let sid = self.sid;
        let in_db = self.with_form(&h, |s, f| {
            let c = f.control(control_id).ok_or_else(|| Flow::Err(Box::new(al_runtime::AlError::new(format!("Page {} has no action {control_id}.", f.page_id)))))?;
            if let Some(ds) = &ds {
                f.load_client_record(s, ds, None)?;
            }
            let Some(pr) = c.triggers.get("onaction").copied() else {
                return Err(Flow::Err(Box::new(al_runtime::AlError::new(format!("The action {} has no OnAction trigger.", c.name)))));
            };
            eprintln!("bc-nst[{sid}]: page {} action {} (OnAction)", f.page_id, c.name);
            f.run_proc(s, pr, vec![])?;
            // the record after the action: re-read, or gone (posted, deleted)
            let in_db = match &f.rec {
                Some(rec) => match s.rec_get_pk(rec) {
                    Some(row) => {
                        f.load_row(&row);
                        f.calc_flow_fields(s)?;
                        f.trigger(s, "OnAfterGetRecord", &[])?;
                        f.trigger(s, "OnAfterGetCurrRecord", &[])?;
                        true
                    }
                    None => false,
                },
                None => false,
            };
            Ok(in_db)
        })?;
        // Navigation: the client re-reads the page; Delete when the record is gone
        let flags = if in_db { 0x40 | 0x10 } else { 0x04 | 0x10 };
        self.codeunit_response(&h, state, flags, &freq.get("Args").cloned().unwrap_or(Value::Null))
    }

    /// `LookupField` / `DrillDownField` / `AssistEdit(FieldRequest, NavRecordState)` →
    /// `CodeUnitResponse`. The web client calls them for fields with an AL trigger. The
    /// control's OnLookup / OnDrillDown / OnAssistEdit runs on the client's record; without
    /// one, the field's own behaviour: the table field's OnLookup, else the table relation's
    /// lookup page (modal, lookup mode); for a FlowField the calculated table's drill-down
    /// page on the formula's filters. LookupField's text goes in and out as `Args[0]`;
    /// `ValidateRequired` makes the client validate a looked-up value.
    fn field_action(&mut self, freq: &Value, state: &Value, kind: &'static str) -> R {
        use al_runtime::interp::{Arg, LVal};
        use al_runtime::Val;
        let h = self.form_of(state)?;
        let control_id = freq.get("ControlId").and_then(Value::as_i64).unwrap_or(0) as i32;
        let ds = client_dataset(freq.get("RecDataSet"))?;
        let text_in = freq.pointer("/Args/0/value").and_then(Value::as_str).unwrap_or("").to_string();
        let sid = self.sid;
        let lookup_ok = al_runtime::interp::system_option_ord("action", "lookupok");
        let (text_out, validate, in_db) = self.with_form(&h, |s, f| {
            let c = f.control(control_id).ok_or_else(|| Flow::Err(Box::new(al_runtime::AlError::new(format!("Page {} has no control {control_id}.", f.page_id)))))?;
            if let Some(ds) = &ds {
                f.load_client_record(s, ds, None)?;
            }
            let start_row = f.rec.as_ref().map(|r| r.borrow().row.clone());
            // RecordState: in the database (0x40), and Dirty (0x20, RecordMadeDirtyByServer)
            // when the trigger changed the record, so the client saves it
            let in_db = |s: &mut Session<'static>| {
                let db = f.rec.as_ref().is_some_and(|r| s.rec_get_pk(r).is_some());
                let dirty = f.rec.as_ref().zip(start_row.as_ref()).is_some_and(|(r, s0)| r.borrow().row != *s0);
                (if db { 0x40 } else { 0 }) | (if dirty { 0x20 } else { 0 })
            };
            let trig = match kind {
                "lookup" => "onlookup",
                "drilldown" => "ondrilldown",
                _ => "onassistedit",
            };
            if let Some(pr) = c.triggers.get(trig).copied() {
                eprintln!("bc-nst[{sid}]: page {} control {} {trig}", f.page_id, c.name);
                if s.code(pr.code).procs[pr.proc as usize].params.is_empty() {
                    f.run_proc(s, pr, vec![])?;
                    return Ok((None, false, in_db(s)));
                }
                // OnLookup(var Text: Text): Boolean
                let text = al_runtime::value::slot(Val::Text(text_in.clone()));
                let ok = f.run_proc(s, pr, vec![Arg::Ref(LVal::Slot(text.clone(), None))])?;
                let out = ok.as_bool().then(|| text.borrow().to_text());
                let v = out.is_some();
                return Ok((out, v, in_db(s)));
            }
            let (Some(fi), Some(rec)) = (c.field, f.rec.clone()) else { return Ok((None, false, in_db(s))) };
            let fd = rec.borrow().field(fi);
            match kind {
                "lookup" => {
                    if let Some(pr) = fd.on_lookup {
                        eprintln!("bc-nst[{sid}]: table {} field {} OnLookup", rec.borrow().tdef.name, fd.name);
                        let before = rec.borrow().row.clone();
                        s.invoke(pr, vec![], None, Some(rec.clone()), None)?;
                        let r = rec.borrow();
                        let changed: Vec<String> = (0..r.row.len()).filter(|&i| r.row[i] != before[i]).map(|i| format!("{} = {:?}", r.tdef.fields[i].name, r.row[i])).collect();
                        eprintln!("bc-nst[{sid}]:   OnLookup changed: {}", changed.join(", "));
                        return Ok((None, false, in_db(s)));
                    }
                    let Some(rel) = fd.relation.first() else { return Ok((None, false, in_db(s))) };
                    let Some(rt) = s.cat.table_by_name(&rel.table) else { return Ok((None, false, in_db(s))) };
                    let Some(page) = s.lookup_page_of(rt) else { return Ok((None, false, in_db(s))) };
                    let inst = Instance::new(s.cat.pages[&page].code);
                    inst.lookup.set(true);
                    let target = s.new_record(rt.id, false);
                    *inst.rec.borrow_mut() = Some(target);
                    let act = match s.host_run_page(page, Some(inst.clone()), None, true) {
                        Some(r) => r?,
                        None => return Ok((None, false, in_db(s))),
                    };
                    if matches!(act, Val::Opt { ord, .. } if ord == lookup_ok) {
                        let sel = inst.rec.borrow().clone();
                        let tf = rel.field.as_ref().and_then(|x| rt.field(x)).or(rt.pk().fields.last().copied());
                        if let (Some(sel), Some(tf)) = (sel, tf) {
                            let v = sel.borrow().get_val(tf).to_text();
                            return Ok((Some(v), true, in_db(s)));
                        }
                    }
                }
                "drilldown" if fd.class == al_model::FieldClass::FlowField => {
                    let Some(cf) = &fd.calc else { return Ok((None, false, in_db(s))) };
                    let Some(tt) = s.cat.table_by_name(&cf.table) else { return Ok((None, false, in_db(s))) };
                    let page = flowfield_drilldown_page(s.cat, c, tt);
                    let Some(page) = page else { return Ok((None, false, in_db(s))) };
                    let target = s.new_record(tt.id, false);
                    for cd in &cf.conds {
                        let Some(tfi) = tt.field(&cd.field) else { continue };
                        let tfd = &tt.fields[tfi];
                        let node = match cd.kind {
                            al_model::CondKind::Field => {
                                let Some(sfi) = rec.borrow().tdef.field(&cd.value) else { continue };
                                if rec.borrow().tdef.fields[sfi].class == al_model::FieldClass::FlowFilter {
                                    // a FlowFilter field: its current filter (none: no condition)
                                    let ff = rec.borrow().filters.iter().rev().find(|x| x.field == sfi).map(|x| (x.node.clone(), x.text.clone()));
                                    if let Some((node, text)) = ff {
                                        target.borrow_mut().set_filter(tfi, Some(node), text);
                                    }
                                    continue;
                                }
                                let v = rec.borrow().row[sfi].clone();
                                al_runtime::filter::FNode::Eq(v)
                            }
                            al_model::CondKind::Const => {
                                let ev = |n: &str| s.enum_ordinal_for_field(tfd, n);
                                match al_runtime::record::text_to_store(tfd, &cd.value, &ev, s.work_date) {
                                    Some(v) => al_runtime::filter::FNode::Eq(v),
                                    None => continue,
                                }
                            }
                            al_model::CondKind::Filter => match s.parse_field_filter(tfd, &cd.value) {
                                Ok(n) => n,
                                Err(_) => continue,
                            },
                        };
                        target.borrow_mut().set_filter(tfi, Some(node), cd.value.clone());
                    }
                    eprintln!("bc-nst[{sid}]: FlowField {} drill-down: page {page}", fd.name);
                    if let Some(r) = s.host_run_page(page, None, Some(target), false) {
                        r?;
                    }
                }
                _ => {}
            }
            Ok((None, false, in_db(s)))
        })?;
        let args = json!([{"typeHint": 21, "value": text_out.clone().unwrap_or(text_in)}]);
        let mut r = self.codeunit_response(&h, state, in_db, &args)?;
        if let Some(st) = r.get_mut("DataSetState").and_then(Value::as_object_mut) {
            st.insert("ValidateRequired".into(), json!(validate));
        }
        Ok(r)
    }

    /// `Evaluate(text, navType)` → `EvaluateBag`: the client checks typed input this
    /// way before ValidateField. Not a ServerResponse (no `requests`). An input that
    /// does not evaluate is an AL error, as `Evaluate()` without a return value.
    fn evaluate(&mut self, text: &str, meta_type_code: i64) -> R {
        use al_runtime::record::{parse_date_text, parse_datetime_text, parse_num, parse_time_text};
        let wd = self.al.as_ref().map(|s| s.work_date).unwrap_or(0);
        let mut bag = json!({"dateTimeValue": "0001-01-01T00:00:00", "millisecondsDurationValue": 0, "boolean": false, "bigInt": 0, "integer": 0, "code": "", "dec": 0});
        let t = text.trim();
        let bad = |ty: &str| Fault::al(format!("The value \"{text}\" can't be evaluated into type {ty}."));
        match meta_type_code {
            31490 => bag["code"] = json!(t.to_uppercase()),             // Code
            31489 | 31488 => bag["code"] = json!(text),                 // Text, String
            34560 | 35584 | 77569 => bag["integer"] = json!(t.parse::<i32>().map_err(|_| bad("Integer"))?), // Integer, Option, Enum
            36096 => bag["bigInt"] = json!(t.parse::<i64>().map_err(|_| bad("BigInteger"))?),
            12800 => bag["dec"] = json!(parse_num(t).ok_or_else(|| bad("Decimal"))?),
            34048 => bag["boolean"] = json!(match t.to_ascii_lowercase().as_str() {
                "yes" | "true" | "1" => true,
                "no" | "false" | "0" | "" => false,
                _ => return Err(bad("Boolean")),
            }),
            11776 => {
                let d = parse_date_text(t, wd).ok_or_else(|| bad("Date"))?;
                let nd = al_runtime::value::date_to_naive(d).map(|n| n.format("%Y-%m-%dT00:00:00").to_string()).unwrap_or_else(|| "0001-01-01T00:00:00".into());
                bag["dateTimeValue"] = json!(nd);
            }
            11777 => {
                let ms = parse_time_text(t).ok_or_else(|| bad("Time"))?.max(1) - 1;
                bag["dateTimeValue"] = json!(format!("0001-01-02T{:02}:{:02}:{:02}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60));
            }
            37376 => {
                let ms = parse_datetime_text(t).ok_or_else(|| bad("DateTime"))?;
                let dt = chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default();
                bag["dateTimeValue"] = json!(dt);
            }
            36864 => bag["millisecondsDurationValue"] = json!(parse_num(t).and_then(|d| rust_decimal::prelude::ToPrimitive::to_i64(&d)).ok_or_else(|| bad("Duration"))?),
            _ => bag["code"] = json!(text),
        }
        Ok(bag)
    }

    /// `EvaluateFilter(text, objectType, objectId, fieldId)`: the filter normalized for
    /// the field. bc-rust checks that it parses and returns it as typed.
    fn evaluate_filter(&mut self, text: &str, table_id: i64, field_id: i64) -> R {
        let cat = self.st.rt.cat;
        let sid = self.sid;
        if let (Some(s), Some(fd)) = (self.al.as_mut(), cat.tables.get(&table_id).and_then(|t| t.field_no(field_id).map(|fi| &t.fields[fi]))) {
            // Filter Helper Triggers.Make<Type>Filter first: Filter Tokens Impl. turns TODAY,
            // WORKDATE, USER, COMPANY, ... into values. The client uses the returned text.
            let text = match crate::platform::make_filter(s, fd, text) {
                Ok(t) => {
                    let _ = s.commit();
                    t
                }
                Err(e) => {
                    s.rollback();
                    eprintln!("bc-nst[{sid}]: Make filter event for {} failed: {}", fd.name, crate::platform::flow_message(&e));
                    return Err(Fault::from(e));
                }
            };
            if let Err(e) = s.parse_field_filter(fd, &text) {
                return Err(Fault::from(e));
            }
            return Ok(json!(text));
        }
        Ok(json!(text))
    }

    /// `GetAllowedCompanies(includeSystemIndicator)` → `DataSetResponse` with the
    /// companies the user may open. bc-rust serves one company.
    fn get_allowed_companies(&mut self) -> R {
        use crate::dataset::{Cell, Column, DataTable, NavDataSet, CellTypeCode as T};
        // the badge of the session's company (UI Helper Triggers.GetSystemIndicator)
        let sid = self.sid;
        let indicator = match self.al.as_mut() {
            Some(s) => system_indicator(s, sid),
            None => (String::new(), 0),
        };
        let cfg = &self.st.cfg;
        // read by column name in ServiceAvailableCompaniesProvider: "1" Name, "2"
        // Evaluation Company, "3" Display Name, "8000" Id (as a string), plus the
        // system indicator text and style
        let cols = vec![
            Column::new("1", T::String),
            Column::new("2", T::Boolean),
            Column::new("3", T::String),
            Column::new("8000", T::String),
            Column::new("SystemIndicatorText", T::String),
            Column::new("SystemIndicatorStyle", T::Int32),
        ];
        let mut t = DataTable::new("2000000006", cols).map_err(|e| Fault::al(e.0))?;
        t.push_row(vec![
            Cell::String(cfg.company.clone()),
            Cell::Bool(false),
            Cell::String(cfg.company_display_name.clone()),
            Cell::String(name_guid("company", &cfg.company)),
            Cell::String(indicator.0),
            Cell::Int32(indicator.1 as i32),
        ])
        .map_err(|e| Fault::al(e.0))?;
        let mut ds = NavDataSet::new();
        ds.add_table(t).map_err(|e| Fault::al(e.0))?;
        let mut m = self.base_response();
        m.insert("dataSet".into(), serde_json::to_value(&ds).map_err(|e| Fault::al(format!("{e}")))?);
        m.insert("State".into(), Value::Null);
        Ok(Value::Object(m))
    }

    // -- work date ----------------------------------------------------------

    fn get_work_date(&mut self) -> R {
        let d = self.al.as_ref().map(|s| s.work_date).unwrap_or_else(al_runtime::value::today);
        let nd = al_runtime::value::date_to_naive(d).unwrap_or_else(|| chrono::Local::now().date_naive());
        Ok(json!(format!("{}T00:00:00", nd.format("%Y-%m-%d"))))
    }

    fn set_work_date(&mut self, v: &Value) -> R {
        let s = v.as_str().ok_or_else(|| Fault::al("SetWorkDate: expected a date"))?;
        let nd = chrono::NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d").map_err(|_| Fault::al(format!("SetWorkDate: invalid date {s}")))?;
        if let Some(sess) = self.al.as_mut() {
            if let Some(d) = al_runtime::value::date_from_ymd(chrono::Datelike::year(&nd), chrono::Datelike::month(&nd), chrono::Datelike::day(&nd)) {
                sess.work_date = d;
            }
        }
        self.get_work_date()
    }
}

/// UI Helper Triggers.GetSystemIndicator as (text, style); an error is logged and gives no badge.
fn system_indicator(s: &mut Session<'static>, sid: i32) -> (String, i64) {
    match crate::platform::system_indicator(s) {
        Ok(x) => x,
        Err(e) => {
            s.rollback();
            eprintln!("bc-nst[{sid}]: GetSystemIndicator failed: {}", crate::platform::flow_message(&e));
            (String::new(), 0)
        }
    }
}

/// A System Action Triggers query the web client makes while it builds the shell. Its
/// answer comes from the subscribers; an error is logged and gives the fixed answer bc-nst
/// used before, since a fault here ends the web session.
fn system_action<T: std::fmt::Debug>(s: Option<&mut Session<'static>>, sid: i32, what: &str, fallback: T, f: impl FnOnce(&mut Session<'static>) -> Result<T, Flow>) -> T {
    let Some(s) = s else { return fallback };
    match f(s) {
        Ok(v) => {
            let _ = s.commit();
            eprintln!("bc-nst[{sid}]: {what} (System Action Triggers) -> {v:?}");
            v
        }
        Err(e) => {
            s.rollback();
            eprintln!("bc-nst[{sid}]: {what} failed: {}", crate::platform::flow_message(&e));
            fallback
        }
    }
}

/// AL's Page.RunModal in the web client. bc-nst sends `FormRunModal([FormRunModalRequest])`
/// (a `ServerResponse` with `Result` and `Data` = `FormRunData`) and waits, inside the AL
/// call, until the web server ends it with `EndClientCall(FormResult)`
/// (`ServiceClientCallback.OnFormRunModal`). Meanwhile the web server reads and changes the
/// modal page through ordinary calls (OpenForm for its parts, GetPage, AfterGetCurrRecord,
/// ValidateField, CloseForm, ...). A nested worker answers them: it takes the live session
/// (swapped out of `*s`, so the AL code that waits keeps its transaction) and owns the
/// modal form. Returns the FormResult and the modal form (for the selected record).
pub(crate) fn run_modal(link: &crate::ui::UiLink, s: &mut Session<'static>, handle: String, form: PageForm, data: Value) -> (Value, Option<PageForm>) {
    let st = link.st.clone();
    let company = s.company.clone();
    let spare = match Session::try_new(st.rt, &company) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("bc-nst[{}]: RunModal: no spare session: {e}", link.sid);
            return (Value::Null, Some(form));
        }
    };
    let mut real = std::mem::replace(s, spare);
    // Message() texts so far show before the modal page
    let msgs = dialog_messages(std::mem::take(&mut real.messages));
    // dialogs and pages that AL raises inside the modal page go to the client too (the
    // waiting frame's host is taken out of the session while it runs)
    let outer_ui = real.ui.take();
    real.ui = Some(Box::new(crate::ui::NstUi { link: link.clone() }));
    let mut w = Worker::new(st, link.sid, None, link.out.clone());
    w.user = Some(real.user_id.clone());
    w.al = Some(real);
    w.opened = true;
    w.company = Some(company);
    w.link = Some(link.clone());
    w.writes = link.writes.clone();
    w.forms.insert(handle.clone(), form);
    w.modal_keep = Some(handle.clone());
    let id = link.next_id.get();
    link.next_id.set(id + 1);
    let mut req = wire::server_response(0);
    req.insert("requests".into(), msgs);
    req.insert("Result".into(), json!(0));
    req.insert("Data".into(), data);
    let msg = wire::rpc_request(&json!(id), "FormRunModal", json!([Value::Object(req)]), link.token);
    eprintln!("bc-nst[{}]: -> web server FormRunModal (id {id})", link.sid);
    let mut result = Value::Null;
    if link.out.send(msg.to_string()).is_ok() {
        loop {
            let text = match link.jobs.recv() {
                Ok(Job::Frame(t)) => t,
                // page background tasks are reported once the modal page is done
                Ok(Job::Wake) => continue,
                Err(_) => break,
            };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            match v.get("method").and_then(Value::as_str) {
                None => {
                    if v.get("id") == Some(&json!(id)) && v.get("error").is_some() {
                        eprintln!("bc-nst[{}]: web server refused FormRunModal: {}", link.sid, clip(&v["error"].to_string(), 300));
                        break;
                    }
                }
                Some(m @ ("EndClientCall" | "EndClientFormRunModalCall")) => {
                    if let Some(rid) = v.get("id") {
                        let _ = link.out.send(wire::rpc_result(rid, Value::Null, Some(link.token)).to_string());
                    }
                    let p = v.pointer("/params/0").cloned().unwrap_or(Value::Null);
                    eprintln!("bc-nst[{}]: <- web server {m} {}", link.sid, clip(&p.to_string(), 200));
                    result = match p {
                        Value::Object(ref o) if m == "EndClientCall" => o.get("Result").map(|r| r.get("value").cloned().unwrap_or_else(|| r.clone())).unwrap_or(Value::Null),
                        other => other,
                    };
                    break;
                }
                Some(_) => {
                    if let Some(reply) = w.handle_frame(&text) {
                        if link.out.send(reply).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
    let form = w.forms.remove(&handle);
    if let Some(mut real) = w.al.take() {
        real.ui = outer_ui;
        let spare = std::mem::replace(s, real);
        drop(spare);
    }
    (result, form)
}

/// Message() texts as `DialogMessageRequest`s for a response's `requests` queue. The
/// runtime's own notes for UI it cannot show yet ("(UI) page X opened" for Page.Run)
/// are logged instead of shown.
pub(crate) fn dialog_messages(msgs: Vec<String>) -> Value {
    Value::Array(
        msgs.into_iter()
            .filter(|m| {
                let ui_note = m.starts_with("(UI) ");
                if ui_note {
                    eprintln!("bc-nst: not shown in the client (no FormRunRequest yet): {m}");
                }
                !ui_note
            })
            .map(|m| json!({"$polyType": "Microsoft.Dynamics.Nav.Types.DialogMessageRequest", "obj": {"Message": crate::ui::dialog_text(&m), "AutomationId": NIL_GUID}}))
            .collect(),
    )
}

/// A NavDataSet the client sent (`{"compressed","count","buffers"}`), `None` for null.
fn client_dataset(v: Option<&Value>) -> Result<Option<crate::dataset::NavDataSet>, Fault> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => crate::dataset::NavDataSet::from_json_value(v).map(Some).map_err(|e| Fault::al(format!("Cannot read the record data set: {e}"))),
    }
}

/// `OpenCompanyResponse.Permissions`: the capability names the web client checks.
/// Copilot and Power Automate are off (bc-rust has neither).
fn permissions() -> Value {
    json!({
        "AllowActionAutomate": false,
        "AllowAddRelatedFieldsinDataAnalysis": true,
        "AllowCopilotAnalysisAssist": false,
        "AllowCopilotAutofill": false,
        "AllowCopilotChat": false,
        "AllowCopilotSummary": false,
        "AllowPageScriptingPlayback": true,
    })
}

fn prop_false(p: &al_model::PageDef, name: &str) -> bool {
    p.props.get(name).is_some_and(|v| v.eq_ignore_ascii_case("false"))
}

/// `FormProperties` mirrors the page's InsertAllowed/ModifyAllowed/DeleteAllowed.
/// `Editable = false` is separate (page metadata `PP_18`): a non-editable list
/// such as page 22 still allows New through its card page.
fn form_properties(p: Option<&al_model::PageDef>) -> Value {
    let (ins, modi, del) = match p {
        Some(p) => (!prop_false(p, "insertallowed"), !prop_false(p, "modifyallowed"), !prop_false(p, "deleteallowed")),
        None => (false, false, false),
    };
    // IsAbleToOverwriteAllowedProperties: the web server only takes these three flags
    // when it is true (NavFormDescriptionBuilder.UpdateProperties); the real NST sends true
    json!({"InsertAllowed": ins, "ModifyAllowed": modi, "DeleteAllowed": del, "IsAbleToOverwriteAllowedProperties": true})
}

/// `AppInformation` of the app that defines the page. bc-rust doesn't track app
/// identity per object yet, so every page reports the Base Application.
/// `ApplicationInsightsConnectionString` must be a string: the web server calls
/// `.ToString()` on it in `NstDataAccess.UpdateFormAppInfo` (the real NST sends "").
/// `PageHelpInfo` (a list of 5-tuples) may be null.
fn app_information(page_id: i64) -> Value {
    json!({"ObjectId": page_id, "AppId": BASE_APP_ID, "Name": "Base Application", "Publisher": "Microsoft", "Version": "29.0.0.0",
        "ApplicationInsightsConnectionString": "", "PageHelpInfo": null})
}

/// `GetUsersAndAgents(getUsers, getAgents)` → `DataSetResponse`. The web server
/// (`UserAndAgentService.GetUsersAndAgents`) reads the tables by name and the cells by
/// index: `Users` = [user id, caption, is agent, user name, enabled];
/// `ConfiguredAgents` (24 cells) and `NotConfiguredAgents` (12 cells) list Copilot
/// agents, which bc-rust has none of. The users list must not be null when asked for
/// (the caller builds a dictionary from it). Column names are bc-rust's; only the
/// positions are read.
fn users_and_agents(get_users: bool, get_agents: bool, user_name: &str) -> Result<crate::dataset::NavDataSet, crate::dataset::DatasetError> {
    use crate::dataset::{Cell, Column, DataTable, NavDataSet, CellTypeCode as T};
    let mut ds = NavDataSet::new();
    if get_users {
        let cols = [("UserSecurityId", T::String), ("Caption", T::String), ("IsAgent", T::Boolean), ("UserName", T::String), ("Enabled", T::Boolean)];
        let mut t = DataTable::new("Users", cols.iter().map(|(n, ty)| Column::new(*n, *ty)).collect())?;
        t.push_row(vec![Cell::String(user_guid(user_name)), Cell::String(user_name.to_string()), Cell::Bool(false), Cell::String(user_name.to_string()), Cell::Bool(true)])?;
        ds.add_table(t)?;
    }
    if get_agents {
        let configured: Vec<Column> = (0..24).map(|i| Column::new(format!("c{i}"), T::String)).collect();
        ds.add_table(DataTable::new("ConfiguredAgents", configured)?)?;
        let not_configured: Vec<Column> = (0..12).map(|i| Column::new(format!("c{i}"), T::String)).collect();
        ds.add_table(DataTable::new("NotConfiguredAgents", not_configured)?)?;
    }
    Ok(ds)
}

fn is_utc_id(id: &str) -> bool {
    matches!(id.to_ascii_uppercase().as_str(), "UTC" | "ETC/UTC" | "ETC/UCT" | "UCT" | "ZULU" | "ETC/ZULU" | "UNIVERSAL" | "ETC/UNIVERSAL" | "COORDINATED UNIVERSAL TIME" | "GMT" | "ETC/GMT")
}

/// Stable GUID for a name (user ids, company SystemId) so reconnects see the same values.
fn name_guid(kind: &str, name: &str) -> String {
    // FNV-1a over kind/name, stretched to 128 bits; formatted as a v4-shaped GUID
    let mut h: u128 = 0x6c62272e07bb014262b821756295c58d;
    for b in kind.bytes().chain([0u8]).chain(name.to_ascii_uppercase().bytes()) {
        h ^= b as u128;
        h = h.wrapping_mul(0x0000000001000000000000000000013B);
    }
    let mut bytes = h.to_be_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes).to_string()
}

fn user_guid(user: &str) -> String {
    name_guid("user", user)
}

pub(crate) fn clip(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// The page a FlowField drill-down opens: the control's DrillDownPageID (Approvals Activities'
/// "Requests to Approve" cue sets one; its target table "Approval Entry" has none), else the
/// target table's DrillDownPageId, else its LookupPageId.
fn flowfield_drilldown_page(cat: &al_model::Catalog, c: &al_model::Control, target: &al_model::TableDef) -> Option<i64> {
    c.prop_src
        .get("drilldownpageid")
        .or_else(|| c.props.get("drilldownpageid"))
        .map(String::as_str)
        .or(target.drilldown_page.as_deref())
        .or(target.lookup_page.as_deref())
        .and_then(|p| cat.id_of(al_syntax::ObjectKind::Page, p.trim().trim_matches('"')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flowfield_drilldown_uses_the_controls_page() {
        let Some(rt) = crate::dataset::tests::runtime() else { return };
        fn find<'a>(cs: &'a [al_model::Control], name: &str) -> Option<&'a al_model::Control> {
            cs.iter().find_map(|c| if c.name == name { Some(c) } else { find(&c.children, name) })
        }
        let page = &rt.cat.pages[&9144];
        let target = rt.cat.table_by_name("Approval Entry").unwrap();
        let c = find(&page.layout, "Requests to Approve").unwrap();
        assert_eq!(flowfield_drilldown_page(rt.cat, c, target), Some(654));
        let c = find(&page.layout, "Requests Sent for Approval").unwrap();
        assert_eq!(flowfield_drilldown_page(rt.cat, c, target), Some(658));
    }

    #[test]
    fn empty_dataset_shape() {
        let v = crate::dataset::NavDataSet::empty_json();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["compressed", "count", "buffers"]);
        let b = base64::engine::general_purpose::STANDARD.decode(v["buffers"][0].as_str().unwrap()).unwrap();
        assert_eq!(b.len(), 19);
        assert_eq!(&b[1..11], b"NewDataSet");
    }

    #[test]
    fn users_and_agents_layout() {
        // UserAndAgentService reads Users by index: id, caption, is agent, user name, enabled
        let ds = users_and_agents(true, true, "ADMIN").unwrap();
        let names: Vec<&str> = ds.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Users", "ConfiguredAgents", "NotConfiguredAgents"]);
        let u = &ds.tables[0];
        assert_eq!(u.rows.len(), 1);
        use crate::dataset::Cell;
        assert_eq!(u.rows[0][1], Cell::String("ADMIN".into()));
        assert_eq!(u.rows[0][2], Cell::Bool(false));
        assert_eq!(u.rows[0][4], Cell::Bool(true));
        assert_eq!(ds.tables[1].columns.len(), 24);
        assert_eq!(ds.tables[2].columns.len(), 12);
        assert!(ds.tables[1].rows.is_empty());
        // only what was asked for
        assert_eq!(users_and_agents(true, false, "ADMIN").unwrap().tables.len(), 1);
        // the dataset reader round-trips it
        let v = serde_json::to_value(&ds).unwrap();
        let back = crate::dataset::NavDataSet::from_json_value(&v).unwrap();
        assert_eq!(back.tables[0].rows[0][3], Cell::String("ADMIN".into()));
    }

    /// A client call is one transaction: one that ends with a transaction still open (a path
    /// that neither committed nor rolled back) is rolled back when the call ends, not left
    /// holding its row locks until the session's next call (I15 W5).
    #[test]
    fn a_call_that_ends_inside_a_transaction_is_rolled_back() {
        let Some(rt) = crate::dataset::tests::runtime() else { return };
        let (out, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut w = Worker::new(NstState::new(rt, NstConfig::default()), 9_001, None, out);
        let mut s = Session::try_new(rt, "CRONUS").unwrap();
        let tid = rt.cat.table_by_name("Standard Text").unwrap().id;
        let h = s.new_record(tid, false);
        {
            let mut r = h.borrow_mut();
            let fi = r.tdef.field("Code").unwrap();
            r.row[fi] = bc_store::Value::Text("W5OPEN".into());
        }
        assert!(matches!(s.rec_insert(&h, false), Ok(Ok(()))));
        assert!(s.db.in_write());
        w.al = Some(s);
        w.handle_frame(r#"{"jsonrpc":"2.0","id":1,"method":"VerboseTraceLevelEnabled","params":[]}"#).expect("a reply");
        let s = w.al.as_mut().unwrap();
        assert!(!s.db.in_write(), "the call ended with its transaction open");
        assert!(s.rec_get_pk(&h).is_none(), "the uncommitted row survived the call");
    }

    #[test]
    fn app_information_has_a_string_connection_string() {
        // NstDataAccess.UpdateFormAppInfo calls ApplicationInsightsConnectionString.ToString()
        assert_eq!(app_information(21)["ApplicationInsightsConnectionString"], json!(""));
        assert_eq!(form_properties(None)["IsAbleToOverwriteAllowedProperties"], json!(true));
    }

    #[test]
    fn auth_helpers() {
        let mut h = HeaderMap::new();
        let v = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode("ADMIN:app\\host\\default\\hash"));
        h.insert(header::AUTHORIZATION, v.parse().unwrap());
        assert_eq!(basic_auth_user(&h).as_deref(), Some("ADMIN"));
        assert!(user_matches("admin", "ADMIN"));
        assert!(user_matches("admin", "CRONUS\\admin"));
        assert!(!user_matches("admin", "bob"));
        assert_eq!(name_guid("user", "ADMIN"), name_guid("user", "admin"));
    }
}
