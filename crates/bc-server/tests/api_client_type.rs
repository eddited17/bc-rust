//! API v2.0 requests run as API client sessions (I15 W, coordinator's batch-2 item): `CurrentClientType()` is
//! `ClientType::Api` there, as in BC, and the Base App branches on it. Item's "Base Unit of Measure" OnValidate
//! inserts the new item first for ODataV4 and API clients (Item.Table.al), so a POST of an item with
//! `baseUnitOfMeasureCode` creates the Item Unit of Measure; as a Web client session it failed. On the seed company
//! in memory; skipped when vendor/bcapps is not present.

use bc_server::app::*;
use serde_json::{json, Value as J};
use std::io::{Read, Write};

fn root() -> Option<std::path::PathBuf> {
    let r = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().ok()?;
    r.join("vendor/bcapps/src/Layers/W1/BaseApp").exists().then_some(r)
}

/// One HTTP/1.1 request with `Connection: close` and an optional If-Match: (status, JSON body or Null).
fn http(port: u16, method: &str, path: &str, body: Option<&J>, if_match: Option<&str>) -> (u16, J) {
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let b = body.map(|b| b.to_string()).unwrap_or_default();
    let im = if_match.map(|v| format!("If-Match: {v}\r\n")).unwrap_or_default();
    write!(c, "{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n{im}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}", b.len()).unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    let (head, rest) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let json = rest.find(['{', '[']).and_then(|i| serde_json::Deserializer::from_str(&rest[i..]).into_iter::<J>().next()).and_then(|r| r.ok()).unwrap_or(J::Null);
    (status, json)
}

fn serve(root: &std::path::Path) -> u16 {
    let app: &'static App = Box::leak(Box::new(boot(root, Db::Memory).unwrap()));
    {
        let mut s = al_runtime::Session::new(&app.rt, "CRONUS");
        bc_server::seed::seed(&mut s).unwrap();
        s.commit().unwrap();
    }
    let ctx: &'static bc_server::api::Ctx = Box::leak(Box::new(bc_server::api::Ctx { rt: &app.rt, company: "CRONUS".into(), boot_ms: 0 }));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(l, bc_server::api::router(ctx)).await.unwrap();
        });
    });
    port
}

#[test]
fn api_requests_are_api_client_sessions() {
    let Some(root) = root() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let port = serve(&root);
    let co = "/api/v2.0/companies(CRONUS)";
    let (st, item) = http(port, "POST", &format!("{co}/items"), Some(&json!({"displayName": "W15 client type", "baseUnitOfMeasureCode": "PCS"})), None);
    assert_eq!(st, 201, "{item}");
    assert_eq!(item["baseUnitOfMeasureCode"], json!("PCS"), "{item}");
    let (st, got) = http(port, "GET", &format!("{co}/items({})", item["id"].as_str().unwrap()), None, None);
    assert_eq!(st, 200, "{got}");
    assert_eq!(got["baseUnitOfMeasureCode"], json!("PCS"), "{got}");
}
