//! API v2.0 errors carry BC's OData error body `{"error": {"code", "message"}}` with BC's codes (I15 W,
//! coordinator's batch-2 item; codes [src:TODO], statuses and texts
//! measured on BC 29.1 by I15 O): a missing entity is 404
//! BadRequest_ResourceNotFound "Resource not found for the segment '<entity set>'." (a write names the entity), an
//! AL Error() is 400 Application_DialogException, an unknown property 400 BadRequest, an unknown entity set 404
//! BadRequest_NotFound, a wrong method 405 BadRequest_MethodNotAllowed. bc-rust answered `{"error": "<text>"}`.
//! On the seed company in memory; skipped when vendor/bcapps is not present.

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

fn assert_error(st: u16, body: &J, status: u16, code: &str) -> String {
    assert_eq!(st, status, "{body}");
    assert_eq!(body["error"]["code"], json!(code), "{body}");
    body["error"]["message"].as_str().expect("error.message").to_string()
}

#[test]
fn api_errors_carry_bcs_code_and_message() {
    let Some(root) = root() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let port = serve(&root);
    let co = "/api/v2.0/companies(CRONUS)";
    let nobody = "00000000-0000-0000-0000-00000000abcd";
    let (st, b) = http(port, "GET", &format!("{co}/customers({nobody})"), None, None);
    assert_eq!(assert_error(st, &b, 404, "BadRequest_ResourceNotFound"), "Resource not found for the segment 'customers'.");
    let (st, b) = http(port, "PATCH", &format!("{co}/customers({nobody})"), Some(&json!({"displayName": "x"})), Some("*"));
    assert_eq!(assert_error(st, &b, 404, "BadRequest_ResourceNotFound"), "Resource not found for the segment 'customer'.");
    let (st, b) = http(port, "DELETE", &format!("{co}/customers({nobody})"), None, None);
    assert_error(st, &b, 404, "BadRequest_ResourceNotFound");
    // the page's own Error(): NavNCLDialogException
    let (st, b) = http(port, "POST", &format!("{co}/customers"), Some(&json!({"displayName": ""})), None);
    assert_eq!(assert_error(st, &b, 400, "Application_DialogException"), "The blank \"displayName\" is not allowed.");
    let (st, b) = http(port, "POST", &format!("{co}/customers"), Some(&json!({"noSuchProperty": 1})), None);
    assert_error(st, &b, 400, "BadRequest");
    let (st, b) = http(port, "GET", &format!("{co}/noSuchEntities"), None, None);
    assert_error(st, &b, 404, "BadRequest_NotFound");
    let (st, b) = http(port, "PUT", &format!("{co}/customers({nobody})"), Some(&json!({})), None);
    assert_error(st, &b, 405, "BadRequest_MethodNotAllowed");
}
