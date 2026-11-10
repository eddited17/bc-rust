//! API v2.0 bound actions over HTTP (`POST .../salesOrders(<id>)/Microsoft.NAV.shipAndInvoice`),
//! on the seed company in memory. Skipped when vendor/bcapps is not present.

use bc_server::app::*;
use serde_json::{json, Value as J};
use std::io::{Read, Write};

fn root() -> Option<std::path::PathBuf> {
    let r = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().ok()?;
    r.join("vendor/bcapps/src/Layers/W1/BaseApp").exists().then_some(r)
}

/// One HTTP/1.1 request with `Connection: close`: (status, Location header, JSON body or Null).
fn http(port: u16, method: &str, path: &str, body: Option<&J>) -> (u16, Option<String>, J) {
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let b = body.map(|b| b.to_string()).unwrap_or_default();
    // PATCH needs If-Match as in BC (api_if_match.rs); `*` takes whatever version the entity has
    let if_match = if matches!(method, "PATCH" | "DELETE") { "If-Match: *\r\n" } else { "" };
    write!(c, "{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n{if_match}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}", b.len()).unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    let (head, rest) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let location = head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("location")).map(|(_, v)| v.trim().to_string()));
    // chunked or plain body: the JSON starts at the first brace
    let json = rest.find(['{', '[']).and_then(|i| serde_json::Deserializer::from_str(&rest[i..]).into_iter::<J>().next()).and_then(|r| r.ok()).unwrap_or(J::Null);
    (status, location, json)
}

/// Boot on `db`, seed company `company` with bc-rust's small seed (seed.rs) and serve the real router
/// on a local port.
fn serve(root: &std::path::Path, db: Db, company: &str) -> u16 {
    let app: &'static App = Box::leak(Box::new(boot(root, db).unwrap()));
    {
        let mut s = al_runtime::Session::new(&app.rt, company);
        bc_server::seed::seed(&mut s).unwrap();
        // deleting a sales document needs Source Code Setup."Deleted Document" (the seed has none)
        let sc = s.new_record(s.cat.table_by_name("Source Code").unwrap().id, false);
        set(&mut s, &sc, "Code", al_runtime::Val::text("DELETED")).unwrap();
        s.rec_insert(&sc, false).map_err(flow_err).unwrap().unwrap();
        let scs = s.new_record(s.cat.table_by_name("Source Code Setup").unwrap().id, false);
        assert!(s.find_first(&scs, false).map_err(flow_err).unwrap());
        set(&mut s, &scs, "Deleted Document", al_runtime::Val::text("DELETED")).unwrap();
        s.rec_modify(&scs, false).map_err(flow_err).unwrap().unwrap();
        s.commit().unwrap();
    }
    let ctx: &'static bc_server::api::Ctx = Box::leak(Box::new(bc_server::api::Ctx { rt: &app.rt, company: company.into(), boot_ms: 0 }));
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

/// An order with one item line through API v2.0: (order JSON, id).
fn order_with_line(port: u16, co: &str) -> (J, String) {
    let (st, _, order) = http(port, "POST", &format!("{co}/salesOrders"), Some(&json!({ "customerNumber": "C10000" })));
    assert_eq!(st, 201, "{order}");
    let id = order["id"].as_str().unwrap().to_string();
    let (st, _, line) = http(port, "POST", &format!("{co}/salesOrderLines"), Some(&json!({ "documentId": id, "lineType": "Item", "lineObjectNumber": "1000", "quantity": 2 })));
    assert_eq!(st, 201, "{line}");
    (order, id)
}

/// The entity key of salesOrders is the page's ODataKeyFields field (Id of the Sales Order Entity
/// Buffer), not the buffer's SystemId: GET/PATCH/DELETE salesOrders(<id>) answered 404 before.
#[test]
fn sales_orders_by_id() {
    let Some(root) = root() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let port = serve(&root, Db::Memory, "CRONUS");
    let co = "/api/v2.0/companies(CRONUS)";
    let (order, id) = order_with_line(port, co);
    let (st, _, got) = http(port, "GET", &format!("{co}/salesOrders({id})"), None);
    assert_eq!((st, got["number"].clone()), (200, order["number"].clone()), "{got}");
    let (st, _, got) = http(port, "PATCH", &format!("{co}/salesOrders({id})"), Some(&json!({ "externalDocumentNumber": "PO-4711" })));
    assert_eq!((st, got["externalDocumentNumber"].clone()), (200, json!("PO-4711")), "{got}");
    assert_eq!(http(port, "GET", &format!("{co}/salesOrders({id})"), None).2["externalDocumentNumber"], "PO-4711");
    let (st, _, got) = http(port, "DELETE", &format!("{co}/salesOrders({id})"), None);
    assert_eq!(st, 204, "{got}");
    assert_eq!(http(port, "GET", &format!("{co}/salesOrders({id})"), None).0, 404);
}

/// `SendToPosting` commits and runs Sales-Post under `if Codeunit.Run`; Sales-Post commits again and then
/// fails. On PostgreSQL the rollback to the savepoint failed ("savepoint sp1 does not exist") and that
/// replaced the posting's own error. The seed company has no Sales Credit Memo Account, so posting a
/// credit memo fails that way. Needs BC_PG_TEST (a scratch database); creates and drops its own company.
#[test]
fn failing_post_after_commit_reports_the_posting_error_on_postgres() {
    let Some(root) = root() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
        return;
    };
    let company = format!("APIPG{}", std::process::id());
    let schema = format!("c_{}", company.to_lowercase());
    let mut admin = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    admin.batch_execute(&format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE")).unwrap();
    let port = serve(&root, Db::Postgres(url.clone()), &company);
    let co = format!("/api/v2.0/companies({company})");
    let (st, _, memo) = http(port, "POST", &format!("{co}/salesCreditMemos"), Some(&json!({ "customerNumber": "C10000" })));
    assert_eq!(st, 201, "{memo}");
    let id = memo["id"].as_str().unwrap().to_string();
    let (st, _, line) = http(port, "POST", &format!("{co}/salesCreditMemoLines"), Some(&json!({ "documentId": id, "lineType": "Item", "lineObjectNumber": "1000", "quantity": 1 })));
    assert_eq!(st, 201, "{line}");
    let (st, _, err) = http(port, "POST", &format!("{co}/salesCreditMemos({id})/Microsoft.NAV.post"), None);
    let msg = err["error"]["message"].as_str().unwrap_or_default().to_string();
    assert_eq!(st, 400, "{err}");
    assert!(msg.contains("Sales Credit Memo Account is missing in General Posting Setup"), "{msg}");
    assert!(!msg.contains("savepoint"), "{msg}");
    // the failed posting left the credit memo as it was
    let (st, _, got) = http(port, "GET", &format!("{co}/salesCreditMemos({id})"), None);
    assert_eq!((st, got["status"].clone()), (200, json!("Draft")), "{got}");
    admin.batch_execute(&format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE")).unwrap();
}

#[test]
fn ship_and_invoice_posts_the_order_and_points_at_the_invoice() {
    let Some(root) = root() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let port = serve(&root, Db::Memory, "CRONUS");
    let co = "/api/v2.0/companies(CRONUS)";

    let (order, id) = order_with_line(port, co);

    // Invoiced: result code Deleted with the posted invoice's key -> 204 and a Location header
    let (st, location, body) = http(port, "POST", &format!("{co}/salesOrders({id})/Microsoft.NAV.shipAndInvoice"), None);
    assert_eq!(st, 204, "{body}");
    let location = location.expect("Location header");
    let path = &location[location.find("/api/v2.0/").expect("absolute URL")..];
    assert!(path.starts_with("/api/v2.0/companies(") && path.contains(")/salesInvoices("), "{location}");
    let (st, _, inv) = http(port, "GET", path, None);
    assert_eq!(st, 200, "{inv}");
    assert_eq!(inv["orderNumber"], order["number"], "{inv}");
    assert_eq!(inv["status"], "Open", "{inv}");
    // the order is gone
    assert_eq!(http(port, "GET", &format!("{co}/salesOrders({id})"), None).0, 404);

    // an action the page does not have, and a GET on an action
    let (st, _, _) = http(port, "POST", &format!("{co}/salesInvoices({})/Microsoft.NAV.noSuchAction", inv["id"].as_str().unwrap()), None);
    assert_eq!(st, 404);
    let (st, _, _) = http(port, "GET", &format!("{co}/salesInvoices({})/Microsoft.NAV.post", inv["id"].as_str().unwrap()), None);
    assert_eq!(st, 405);
}
