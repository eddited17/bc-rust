//! API v2.0 PATCH and DELETE take `If-Match` like BC's OData layer (I15 W3; [src:TODO]):
//! - PATCH without If-Match, or with a malformed one: 400 `BadRequest_InvalidToken`;
//! - an ETag of an older version: 409 `Request_EntityChanged`, "Another user has already changed
//!   the record.", and nothing is written;
//! - the current ETag or `*`: the write goes through and the answer carries the new ETag;
//! - DELETE without If-Match is allowed; with one, the same checks.
//!
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

fn etag(v: &J) -> String {
    v["@odata.etag"].as_str().expect("an @odata.etag").to_string()
}

fn assert_error(st: u16, body: &J, status: u16, code: &str, message: &str) {
    assert_eq!(st, status, "{body}");
    assert_eq!(body["error"]["code"], json!(code), "{body}");
    assert_eq!(body["error"]["message"], json!(message), "{body}");
}

// BC's texts for these two errors [src:X03]
const INVALID_TOKEN: &str = "Could not validate the client concurrency token required by the service. Please provide a valid token in the client request.";
const CHANGED: &str = "Another user has already changed the record.";

#[test]
fn patch_and_delete_take_if_match_like_bc() {
    let Some(root) = root() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let port = serve(&root);
    let co = "/api/v2.0/companies(CRONUS)";
    let (st, list) = http(port, "GET", &format!("{co}/customers?$filter=number%20eq%20'C10000'"), None, None);
    assert_eq!(st, 200, "{list}");
    let cust = list["value"][0].clone();
    let id = cust["id"].as_str().unwrap().to_string();
    let url = format!("{co}/customers({id})");
    let name = |port| http(port, "GET", &url, None, None).1["displayName"].clone();
    let before = name(port);

    // PATCH without If-Match, or with a malformed one: BC's invalid-token error, nothing written
    let (st, b) = http(port, "PATCH", &url, Some(&json!({"displayName": "W3 no etag"})), None);
    assert_error(st, &b, 400, "BadRequest_InvalidToken", INVALID_TOKEN);
    for bad in ["12", "W/12", "\"abc\"", "W/\"\""] {
        let (st, b) = http(port, "PATCH", &url, Some(&json!({"displayName": "W3 bad etag"})), Some(bad));
        assert_error(st, &b, 400, "BadRequest_InvalidToken", INVALID_TOKEN);
    }
    assert_eq!(name(port), before);

    // the current ETag: written, with a new ETag
    let e0 = etag(&cust);
    let (st, b) = http(port, "PATCH", &url, Some(&json!({"displayName": "W3 first"})), Some(&e0));
    assert_eq!(st, 200, "{b}");
    let e1 = etag(&b);
    assert_ne!(e1, e0);

    // the old ETag again: 409 Request_EntityChanged, not written
    let (st, b) = http(port, "PATCH", &url, Some(&json!({"displayName": "W3 stale"})), Some(&e0));
    assert_error(st, &b, 409, "Request_EntityChanged", CHANGED);
    assert_eq!(name(port), json!("W3 first"));

    // `*`: whatever version it has; the ETag of the GET is the current one
    let (st, b) = http(port, "PATCH", &url, Some(&json!({"displayName": "W3 any"})), Some("*"));
    assert_eq!(st, 200, "{b}");
    let (_, got) = http(port, "GET", &url, None, None);
    assert_eq!(etag(&got), etag(&b));
    // the weak and the strong form of one ETag are the same version
    let strong = etag(&b).trim_start_matches("W/").to_string();
    let (st, b) = http(port, "PATCH", &url, Some(&json!({"displayName": "W3 strong"})), Some(&strong));
    assert_eq!(st, 200, "{b}");

    // a document line (an API page on a temporary source table, loaded by OnFindRecord): the ETag
    // of its GET is current for its PATCH, and the answer carries the next one
    let (st, o) = http(port, "POST", &format!("{co}/salesOrders"), Some(&json!({"customerNumber": "C10000"})), None);
    assert_eq!(st, 201, "{o}");
    let (st, l) = http(port, "POST", &format!("{co}/salesOrderLines"), Some(&json!({"documentId": o["id"], "lineType": "Item", "lineObjectNumber": "1000", "quantity": 2})), None);
    assert_eq!(st, 201, "{l}");
    let lurl = format!("{co}/salesOrderLines({})", l["id"].as_str().unwrap());
    let (st, g) = http(port, "GET", &lurl, None, None);
    assert_eq!(st, 200, "{g}");
    let (st, b) = http(port, "PATCH", &lurl, Some(&json!({"quantity": 3})), Some(&etag(&g)));
    assert_eq!(st, 200, "{b}");
    let (st, g2) = http(port, "GET", &lurl, None, None);
    assert_eq!(st, 200, "{g2}");
    let (st, b) = http(port, "PATCH", &lurl, Some(&json!({"quantity": 4})), Some(&etag(&g2)));
    assert_eq!(st, 200, "{b}");

    // a document (salesOrders: the Sales Order Entity Buffer, kept by subscribers): PATCHes chained on
    // the ETag each answer carries, and the GET agrees with the last one
    let ourl = format!("{co}/salesOrders({})", o["id"].as_str().unwrap());
    let (st, g) = http(port, "GET", &ourl, None, None);
    assert_eq!(st, 200, "{g}");
    let mut e = etag(&g);
    for n in 1..=3 {
        let (st, b) = http(port, "PATCH", &ourl, Some(&json!({"externalDocumentNumber": format!("W3-{n}")})), Some(&e));
        assert_eq!(st, 200, "PATCH {n} with the previous answer's ETag: {b}");
        e = etag(&b);
    }
    assert_eq!(etag(&http(port, "GET", &ourl, None, None).1), e);

    // DELETE: If-Match optional, checked when it is there
    let (st, c) = http(port, "POST", &format!("{co}/customers"), Some(&json!({"displayName": "W3 delete me"})), None);
    assert_eq!(st, 201, "{c}");
    let url2 = format!("{co}/customers({})", c["id"].as_str().unwrap());
    let ea = etag(&c);
    let (st, b) = http(port, "PATCH", &url2, Some(&json!({"phoneNumber": "0123"})), Some(&ea));
    assert_eq!(st, 200, "{b}");
    let eb = etag(&b);
    let (st, b) = http(port, "DELETE", &url2, None, Some(&ea));
    assert_error(st, &b, 409, "Request_EntityChanged", CHANGED);
    let (st, b) = http(port, "DELETE", &url2, None, Some("nonsense"));
    assert_error(st, &b, 400, "BadRequest_InvalidToken", INVALID_TOKEN);
    assert_eq!(http(port, "GET", &url2, None, None).0, 200);
    let (st, b) = http(port, "DELETE", &url2, None, Some(&eb));
    assert_eq!(st, 204, "{b}");
    assert_eq!(http(port, "GET", &url2, None, None).0, 404);
    let (st, c) = http(port, "POST", &format!("{co}/customers"), Some(&json!({"displayName": "W3 delete me too"})), None);
    assert_eq!(st, 201, "{c}");
    let (st, b) = http(port, "DELETE", &format!("{co}/customers({})", c["id"].as_str().unwrap()), None, None);
    assert_eq!(st, 204, "{b}");
}
