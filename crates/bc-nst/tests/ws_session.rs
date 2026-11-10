//! End-to-end check of the session socket: a JSON-RPC client talks to the
//! bc-nst router over a real WebSocket, the way Microsoft's web server does,
//! and the responses are checked against the invariants in
//! docs/nst-protocol-transport.md. In-memory database; skipped when
//! vendor/bcapps is not present.

use bc_nst::dataset::{Cell, NavDataSet};
use bc_nst::rpc::{fill_all_profile, NstConfig, NstState};
use bc_nst::wire;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn root() -> Option<std::path::PathBuf> {
    let r = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().ok()?;
    r.join("vendor/bcapps/src/Layers/W1/BaseApp").exists().then_some(r)
}

/// One catalog + seeded in-memory company for all tests in this file.
fn runtime() -> Option<&'static al_runtime::Runtime> {
    static RT: OnceLock<Option<&'static al_runtime::Runtime>> = OnceLock::new();
    *RT.get_or_init(|| {
        let root = root()?;
        let app = bc_server::app::boot(&root, bc_server::app::Db::Memory).unwrap();
        let rt: &'static al_runtime::Runtime = Box::leak(Box::new(app.rt));
        let mut s = al_runtime::Session::new(rt, "CRONUS");
        bc_server::seed::seed(&mut s).unwrap();
        fill_all_profile(&mut s, &NstConfig::default()).unwrap();
        // the first company open of the database (System Initialization Impl. inserts Signup
        // Context Values), before the tests below open sessions in parallel and race for it
        assert!(bc_nst::platform::company_open(&mut s).is_ok(), "OnCompanyOpen");
        s.commit().unwrap();
        Some(rt)
    })
}

async fn serve(rt: &'static al_runtime::Runtime) -> std::net::SocketAddr {
    let state = NstState::new(rt, NstConfig::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, bc_nst::rpc::router(state)).await.unwrap() });
    addr
}

fn basic(user: &str, pw: &str) -> String {
    use base64::Engine as _;
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pw}")))
}

async fn connect(addr: std::net::SocketAddr, path: &str, auth: Option<&str>) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
    if let Some(a) = auth {
        req.headers_mut().insert("Authorization", a.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws)
}

struct Client {
    ws: Ws,
    next: i64,
}

impl Client {
    /// Send one request with positional params and wait for the response with the same id.
    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.ws.send(Message::Text(req.to_string().into())).await.unwrap();
        loop {
            let m = tokio::time::timeout(std::time::Duration::from_secs(120), self.ws.next()).await.expect("response timeout").unwrap().unwrap();
            if let Message::Text(t) = m {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["id"] == json!(id) {
                    assert_eq!(v["jsonrpc"], "2.0");
                    return v;
                }
            }
        }
    }
}

fn ok<'a>(r: &'a Value, method: &str) -> &'a Value {
    assert!(r.get("error").is_none(), "{method} failed: {r}");
    assert!(r.get("result").is_some(), "{method}: no result: {r}");
    assert!(r["MetadataToken"].is_i64(), "{method}: no MetadataToken");
    &r["result"]
}

/// `requests` may be empty or absent, never null.
fn requests_ok(v: &Value, method: &str) {
    if let Some(q) = v.get("requests") {
        assert!(q.is_array(), "{method}: requests is {q}");
    }
}

/// The fault rule: code -32000, data is a ServerFault whose exceptionData is
/// base64 of a JSON array of typed .NET exceptions.
fn fault_ok(r: &Value) -> Value {
    let e = &r["error"];
    assert_eq!(e["code"], -32000, "{r}");
    assert!(e["message"].is_string());
    let sf = &e["data"];
    assert!(sf["responseQueue"]["requests"].is_array());
    let arr = wire::decode_exception_data(sf["exceptionData"].as_str().unwrap()).unwrap();
    let x = arr[0].clone();
    let ty = x["$type"].as_str().unwrap();
    assert!(ty.starts_with("Microsoft.Dynamics.Nav.Types.Exceptions.NavNCL") && ty.contains(", Microsoft.Dynamics.Nav.Types, Version="), "{ty}");
    assert_eq!(x.as_object().unwrap().keys().next().unwrap(), "$type");
    x
}

fn is_guid(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok() && s != wire::NIL_GUID
}

fn connection_request() -> Value {
    json!({
        "LCID": 1033, "DefaultLCID": 1033, "LocalId": 1033, "TenantId": "default",
        "ClientHostName": "localhost", "TimeZoneId": "UTC",
        "Credentials": {"UserName": "ADMIN", "Password": "app\\localhost\\default\\hash"},
        "ClientConnectionType": 0, "ClientCompatibilityVersion": 15041, "ClientAssemblyFileVersion": "29.1.57326.57497",
        "ProfileAppId": wire::NIL_GUID, "ApplicationExtension": wire::NIL_GUID
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_lifecycle() {
    let Some(rt) = runtime() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let addr = serve(rt).await;
    let ws = connect(addr, "/BC/client/ws/connect", Some(&basic("ADMIN", "x"))).await.unwrap();
    let mut c = Client { ws, next: 0 };

    // OpenConnection: valid time zone, LCIDs, enabledFeatures
    let r = c.call("OpenConnection", json!([connection_request()])).await;
    let us = ok(&r, "OpenConnection").clone();
    requests_ok(&us, "OpenConnection");
    assert_eq!(us["lcid"], 1033);
    assert_eq!(us["defaultLcid"], 1033);
    assert!(us["enabledFeatures"].is_array());
    let tz = us["serializedTimeZoneInfo"].as_str().unwrap();
    assert_eq!(tz.split(';').count(), 7, "{tz}");
    assert_eq!(tz.split(';').nth(1), Some("0"));
    assert_eq!(us["companyName"], "CRONUS");
    assert_eq!(us["userName"], "ADMIN");
    assert!(is_guid(us["ServerUniqueSessionId"].as_str().unwrap()));
    assert_eq!(us["enableCompactSerialization"], false);
    println!("OpenConnection response: {}", r);

    // SetClientSettings(SessionSettings, saveOnly) returns nothing
    let r = c.call("SetClientSettings", json!([{"ClientSettings": {"LCID": 1033, "WindowsLCID": 1033, "DecimalSeparator": "."}}, false])).await;
    assert!(ok(&r, "SetClientSettings").is_null());

    // OpenCompany: Permissions required
    let r = c.call("OpenCompany", json!(["CRONUS", false])).await;
    let oc = ok(&r, "OpenCompany");
    requests_ok(oc, "OpenCompany");
    assert!(oc["Permissions"].is_object());
    assert_eq!(oc["CompanyName"], "CRONUS");

    // unknown company: a recoverable fault, session stays usable
    let r = c.call("OpenCompany", json!(["NOPE", false])).await;
    let x = fault_ok(&r);
    assert!(x["Message"].as_str().unwrap().contains("NOPE"));

    // the default company via "" (what the web server sends at startup)
    let r = c.call("OpenCompany", json!(["", false])).await;
    assert_eq!(ok(&r, "OpenCompany")["CompanyName"], "CRONUS");

    // OpenForm(22): one state per form, non-null DataSet, per-form arrays
    let state = json!({"FormId": 22, "TableView": {"TableId": 0, "Ascending": true}, "NavFormEditable": true, "ValidateRequired": true, "PromptMode": 0});
    let req = json!({"States": [state], "HasMainForm": true, "VersionNumber": 1, "ControlIds": [null], "MainFormHandle": wire::NIL_GUID});
    let r = c.call("OpenForm", json!([req])).await;
    let fo = ok(&r, "OpenForm").clone();
    requests_ok(&fo, "OpenForm");
    let states = fo["States"].as_array().unwrap();
    assert_eq!(states.len(), 1);
    let s0 = &states[0];
    assert_eq!(s0["FormId"], 22);
    assert!(is_guid(s0["ServerFormHandle"].as_str().unwrap()));
    assert_eq!(s0["TableView"]["TableId"], 18);
    assert_eq!(s0["TableView"]["CurrentSortingFieldIds"], json!([1]));
    assert!(s0.get("FormOpenExceptionData").is_none(), "OnOpenPage of page 22 failed: {}", s0["FormOpenExceptionData"]);
    let ds: Vec<&str> = fo["DataSet"].as_object().unwrap().keys().map(|k| k.as_str()).collect();
    assert_eq!(ds, ["compressed", "count", "buffers"]);
    for k in ["TableCaptions", "FormProperties", "AppInformation"] {
        assert_eq!(fo[k].as_array().map(|a| a.len()), Some(1), "{k}");
    }
    assert_eq!(fo["FormProperties"][0]["InsertAllowed"], true);

    // a page that doesn't exist: fault
    let bad = json!({"States": [{"FormId": 1999999999}], "HasMainForm": true, "ControlIds": [null]});
    fault_ok(&c.call("OpenForm", json!([bad])).await);

    // GetPage on the open Customer List: first three customers, more to come
    let r = c.call("GetPage", json!([{"PageSize": 3, "StartFromPage": 1, "IncludeMoreDataInformation": true}, s0])).await;
    let gp = ok(&r, "GetPage");
    requests_ok(gp, "GetPage");
    let keys: Vec<&str> = gp["dataSet"].as_object().unwrap().keys().map(|k| k.as_str()).collect();
    assert_eq!(keys, ["compressed", "count", "buffers"]);
    let ds = NavDataSet::from_json_value(&gp["dataSet"]).unwrap();
    let t = &ds.tables[0];
    let nos: Vec<&Cell> = (0..t.rows.len()).map(|i| t.get(i, "1").unwrap()).collect();
    assert_eq!(nos, [&Cell::String("C10000".into()), &Cell::String("C10010".into()), &Cell::String("C10020".into())]);
    assert_eq!(gp["State"]["MoreDataInReadDirection"], true);
    assert_eq!(gp["State"]["ServerFormHandle"], s0["ServerFormHandle"]);
    assert_eq!(gp["State"]["TableView"]["Ascending"], true);

    // GetPage without a form, as the web server reads All Profile while starting up
    let pr = json!({"IsSubFormUpdateRequest": false, "PageSize": 2147483647, "PageSizeInOppositeDirection": 0, "ReadDirection": 0, "StartFromPage": 0,
                    "IncludeMoreDataInformation": true, "IncludeNonRowData": false, "ExcludeStartingRecord": false, "RequestRecentEntries": false});
    let st = json!({"TableView": {"TableId": 2000000178, "Ascending": true, "CurrentFilters": []}, "ServerFormHandle": wire::NIL_GUID, "FormId": 0, "SelectedRecords": []});
    let r = c.call("GetPage", json!([pr, st])).await;
    let ds = NavDataSet::from_json_value(&ok(&r, "GetPage(All Profile)")["dataSet"]).unwrap();
    let t = &ds.tables[0];
    assert!(t.rows.len() > 1, "All Profile rows: {}", t.rows.len());
    // ProfileAttributes reads cells by field number: 3 Profile ID, 6 Default Role Center, 5 Role Center ID, 13 Disable Personalization
    let bm = (0..t.rows.len()).find(|&i| t.get(i, "3") == Some(&Cell::String("BUSINESS MANAGER".into()))).expect("BUSINESS MANAGER profile");
    assert_eq!(t.get(bm, "6"), Some(&Cell::Bool(true)));
    assert!(matches!(t.get(bm, "5"), Some(Cell::Int32(id)) if *id > 0));
    assert_eq!(t.get(bm, "13"), Some(&Cell::Bool(false)));
    assert_eq!(t.get(bm, "1"), Some(&Cell::Int32(0)));
    let defaults = (0..t.rows.len()).filter(|&i| t.get(i, "6") == Some(&Cell::Bool(true))).count();
    assert_eq!(defaults, 1);

    // User Personalization by user GUID (ProfileAttributes.UserPersonalizationProfileName): no row, one empty table
    let st = json!({"TableView": {"TableId": 2000000073, "Ascending": true, "CurrentFilters": [{"FilterGroupNo": 0, "Filters": [{"FilterField": 3, "FilterValue": "6f2a3b1c-0d4e-4f5a-8b6c-7d8e9f0a1b2c"}]}]}, "FormId": 0});
    let r = c.call("GetPage", json!([{"PageSize": 2147483647}, st])).await;
    let ds = NavDataSet::from_json_value(&ok(&r, "GetPage(User Personalization)")["dataSet"]).unwrap();
    assert_eq!(ds.tables.len(), 1);
    assert_eq!(ds.tables[0].rows.len(), 0);

    // a method bc-nst doesn't implement: ServerFault, never "method not found"
    let r = c.call("NoSuchMethodForTesting", json!([])).await;
    let x = fault_ok(&r);
    assert_eq!(x["$type"].as_str().unwrap().split(',').next(), Some("Microsoft.Dynamics.Nav.Types.Exceptions.NavNCLNotSupportedOperationException"));

    // CloseForm: one state per form
    let req = json!({"States": [s0], "HasMainForm": true, "ExitActions": [0], "Force": false, "DataSet": null});
    let r = c.call("CloseForm", json!([req])).await;
    let fc = ok(&r, "CloseForm");
    requests_ok(fc, "CloseForm");
    assert_eq!(fc["States"].as_array().map(|a| a.len()), Some(1));

    // the web server closes the company (null) before CloseConnection
    let r = c.call("OpenCompany", json!([null, false])).await;
    assert!(ok(&r, "OpenCompany(null)")["Permissions"].is_object());
    let r = c.call("CloseConnection", json!([])).await;
    requests_ok(ok(&r, "CloseConnection"), "CloseConnection");
    assert_eq!(ok(&r, "CloseConnection")["requests"], json!([]));
    c.ws.close(None).await.ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgrade_auth_and_paths() {
    let Some(rt) = runtime() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let addr = serve(rt).await;
    // wrong user: 401 on the upgrade (the web server shows "invalid credentials")
    match connect(addr, "/BC/client/ws/connect", Some(&basic("BOB", "x"))).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => assert_eq!(resp.status(), 401),
        other => panic!("expected 401, got {:?}", other.map(|_| ())),
    }
    // /{instance}/Service is accepted too
    let ws = connect(addr, "/BC/Service", Some(&basic("admin", "x"))).await.unwrap();
    let mut c = Client { ws, next: 100 };
    let r = c.call("OpenConnection", json!([connection_request()])).await;
    ok(&r, "OpenConnection");
    // notifications (no id) get no reply; the next request still answers
    c.ws.send(Message::Text(json!({"jsonrpc": "2.0", "method": "CancelDialog", "params": []}).to_string().into())).await.unwrap();
    let r = c.call("CloseConnection", json!([])).await;
    ok(&r, "CloseConnection");
}

/// Platform trigger events (crate::platform): the subscribers of the System.app trigger
/// codeunits answer, not fixed values. Company Information's badge comes through
/// UI Helper Triggers.GetSystemIndicator (LogInManagement), filter tokens through Filter
/// Helper Triggers.Make*Filter (Filter Tokens Impl.), and the shell queries through System
/// Action Triggers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn platform_trigger_events() {
    use al_runtime::Val;
    let Some(rt) = runtime() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    {
        let mut s = al_runtime::Session::new(rt, "CRONUS");
        let t = s.cat.table_by_name("Company Information").expect("Company Information");
        let r = s.new_record(t.id, false);
        assert!(matches!(s.find_first(&r, false), Ok(true)), "no Company Information record");
        for (name, v) in [("System Indicator", Val::Opt { ord: 1, enum_id: None }), ("Custom System Indicator Text", Val::text("bc-nst test")), ("System Indicator Style", Val::Opt { ord: 2, enum_id: None })] {
            let fi = t.field(name).unwrap();
            let sv = al_runtime::record::to_store(&t.fields[fi], &v).unwrap();
            r.borrow_mut().row[fi] = sv;
        }
        assert!(matches!(s.rec_modify(&r, false), Ok(Ok(()))), "cannot modify Company Information");
        s.commit().unwrap();
    }
    let addr = serve(rt).await;
    let ws = connect(addr, "/BC/client/ws/connect", Some(&basic("ADMIN", "x"))).await.unwrap();
    let mut c = Client { ws, next: 0 };
    ok(&c.call("OpenConnection", json!([connection_request()])).await, "OpenConnection");
    let r = c.call("OpenCompany", json!(["CRONUS", false])).await;
    let oc = ok(&r, "OpenCompany");
    assert_eq!(oc["Indicator"]["text"], "bc-nst test", "{oc}");
    assert_eq!(oc["Indicator"]["style"], 2, "{oc}");
    // Customer 5 Address (Text), 54 Last Date Modified (Date), 2 Name (Text)
    let r = c.call("EvaluateFilter", json!(["USER", 1, 18, 5])).await;
    assert_eq!(ok(&r, "EvaluateFilter"), "ADMIN");
    let r = c.call("EvaluateFilter", json!(["COMPANY", 1, 18, 2])).await;
    assert_eq!(ok(&r, "EvaluateFilter"), "CRONUS");
    let r = c.call("EvaluateFilter", json!(["YESTERDAY..TODAY", 1, 18, 54])).await;
    let f = ok(&r, "EvaluateFilter").as_str().unwrap().to_string();
    assert!(f.contains("..") && !f.contains("TODAY"), "{f}");
    let r = c.call("GetAutoStartTours", json!([])).await;
    assert!(ok(&r, "GetAutoStartTours").is_boolean());
    let r = c.call("GetRoleCenterBannerPartId", json!([])).await;
    assert!(ok(&r, "GetRoleCenterBannerPartId").is_i64());
    let r = c.call("GetNotificationStatus", json!([wire::NIL_GUID])).await;
    assert!(ok(&r, "GetNotificationStatus").is_boolean());
}
