//! API v2.0 never drops input (I14 S6): properties are applied in page order like BC's OData
//! layer (NavFormODataExtension.SetValuesAsync), page-variable fields are served, record
//! triggers return true unless they exit(false), and records are read through the page
//! (OnOpenPage, OnFindRecord, OnAfterGetRecord). On the seed company in memory; skipped when
//! vendor/bcapps is not present.

use bc_server::app::*;
use serde_json::{json, Value as J};
use std::io::{Read, Write};

fn root() -> Option<std::path::PathBuf> {
    let r = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().ok()?;
    r.join("vendor/bcapps/src/Layers/W1/BaseApp").exists().then_some(r)
}

/// One HTTP/1.1 request with `Connection: close`: (status, JSON body or Null).
fn http(port: u16, method: &str, path: &str, body: Option<&J>) -> (u16, J) {
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let b = body.map(|b| b.to_string()).unwrap_or_default();
    // PATCH needs If-Match as in BC (api_if_match.rs); `*` takes whatever version the entity has
    let if_match = if matches!(method, "PATCH" | "DELETE") { "If-Match: *\r\n" } else { "" };
    write!(c, "{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n{if_match}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}", b.len()).unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    let (head, rest) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let json = rest.find(['{', '[']).and_then(|i| serde_json::Deserializer::from_str(&rest[i..]).into_iter::<J>().next()).and_then(|r| r.ok()).unwrap_or(J::Null);
    (status, json)
}

fn ins(s: &mut al_runtime::Session, table: &str, fields: &[(&str, al_runtime::Val)]) {
    let r = s.new_record(s.cat.table_by_name(table).unwrap().id, false);
    for (f, v) in fields {
        set(s, &r, f, v.clone()).unwrap();
    }
    s.rec_insert(&r, false).map_err(flow_err).unwrap().unwrap();
}

fn modify_first(s: &mut al_runtime::Session, table: &str, fields: &[(&str, al_runtime::Val)]) {
    let r = s.new_record(s.cat.table_by_name(table).unwrap().id, false);
    assert!(s.find_first(&r, false).map_err(flow_err).unwrap(), "{table} has no row");
    for (f, v) in fields {
        set(s, &r, f, v.clone()).unwrap();
    }
    s.rec_modify(&r, false).map_err(flow_err).unwrap().unwrap();
}

/// Seed company plus what these tests need: purchase invoice numbers, posting groups on the
/// vendors, a EUR exchange rate, and two journal templates (General, Cash Receipts) with a batch each.
fn serve(root: &std::path::Path, db: Db, company: &str) -> u16 {
    use al_runtime::Val;
    let t = Val::text;
    let app: &'static App = Box::leak(Box::new(boot(root, db).unwrap()));
    {
        let mut s = al_runtime::Session::new(&app.rt, company);
        bc_server::seed::seed(&mut s).unwrap();
        for (code, start) in [("P-INV", "P-INV1001"), ("P-INV+", "108001")] {
            ins(&mut s, "No. Series", &[("Code", t(code)), ("Description", t(code)), ("Default Nos.", Val::Bool(true)), ("Manual Nos.", Val::Bool(true))]);
            ins(&mut s, "No. Series Line", &[("Series Code", t(code)), ("Line No.", Val::Int(10000)), ("Starting No.", t(start)), ("Increment-by No.", Val::Int(10))]);
        }
        modify_first(&mut s, "Purchases & Payables Setup", &[("Invoice Nos.", t("P-INV")), ("Posted Invoice Nos.", t("P-INV+"))]);
        let v = s.new_record(s.cat.table_by_name("Vendor").unwrap().id, false);
        let mut ok = s.find_first(&v, false).map_err(flow_err).unwrap();
        while ok {
            for (f, x) in [("Vendor Posting Group", "DOMESTIC"), ("Gen. Bus. Posting Group", "DOMESTIC"), ("VAT Bus. Posting Group", "DOMESTIC")] {
                set(&mut s, &v, f, t(x)).unwrap();
            }
            s.rec_modify(&v, false).map_err(flow_err).unwrap().unwrap();
            ok = s.next(&v, 1).map_err(flow_err).unwrap() != 0;
        }
        let d2020 = Val::Date(al_runtime::value::date_from_ymd(2020, 1, 1).unwrap());
        let dec = |x: i64| Val::Dec(x.into());
        ins(
            &mut s,
            "Currency Exchange Rate",
            &[("Currency Code", t("EUR")), ("Starting Date", d2020), ("Exchange Rate Amount", dec(100)), ("Adjustment Exch. Rate Amount", dec(100)), ("Relational Exch. Rate Amount", dec(110)), ("Relational Adjmt Exch Rate Amt", dec(110))],
        );
        // General (page 39, type General) and Cash Receipts (page 255, type Cash Receipts)
        ins(&mut s, "Gen. Journal Template", &[("Name", t("GENERAL")), ("Description", t("General")), ("Page ID", Val::Int(39)), ("Type", Val::Opt { ord: 0, enum_id: None }), ("Source Code", t("GENJNL"))]);
        ins(&mut s, "Gen. Journal Template", &[("Name", t("CASHRCPT")), ("Description", t("Cash receipts")), ("Page ID", Val::Int(255)), ("Type", Val::Opt { ord: 3, enum_id: None }), ("Source Code", t("GENJNL"))]);
        ins(&mut s, "Gen. Journal Batch", &[("Journal Template Name", t("GENERAL")), ("Name", t("DEFAULT")), ("Description", t("Default"))]);
        ins(&mut s, "Gen. Journal Batch", &[("Journal Template Name", t("CASHRCPT")), ("Name", t("CASH")), ("Description", t("Cash"))]);
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

fn post(port: u16, co: &str, set: &str, body: J) -> J {
    let (st, got) = http(port, "POST", &format!("{co}/{set}"), Some(&body));
    assert_eq!(st, 201, "POST {set} {body}: {got}");
    got
}

fn get(port: u16, co: &str, path: &str) -> J {
    let (st, got) = http(port, "GET", &format!("{co}/{path}"), None);
    assert_eq!(st, 200, "GET {path}: {got}");
    got
}

fn num(v: &J) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

const ZERO_ID: &str = "00000000-0000-0000-0000-000000000000";

#[test]
fn api_v2_applies_every_property() {
    let Some(root) = root() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let port = serve(&root, Db::Memory, "CRONUS");
    check_properties(port, "/api/v2.0/companies(CRONUS)");
}

#[test]
fn api_v2_journal_lines_are_stored_and_journals_filtered_by_template() {
    let Some(root) = root() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let port = serve(&root, Db::Memory, "CRONUS");
    check_journals(port, "/api/v2.0/companies(CRONUS)");
}

/// The same on PostgreSQL (BC_PG_TEST = a scratch database connection string), in a company of its own.
#[test]
fn api_v2_input_on_postgres() {
    let Some(root) = root() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("skipped: set BC_PG_TEST to a scratch database connection string");
        return;
    };
    let company = format!("APIIN{}", std::process::id());
    let schema = format!("c_{}", company.to_lowercase());
    let mut admin = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    admin.batch_execute(&format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE")).unwrap();
    let port = serve(&root, Db::Postgres(url.clone()), &company);
    let co = format!("/api/v2.0/companies({company})");
    check_properties(port, &co);
    check_journals(port, &co);
    admin.batch_execute(&format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE")).unwrap();
}

fn check_properties(port: u16, co: &str) {

    // gap 3: a sales line's discountPercent is applied after lineObjectNumber (page order), so
    // validating "No." no longer resets it; the JSON object lists discountPercent first
    let inv = post(port, co, "salesInvoices", json!({ "customerNumber": "C10000" }));
    let id = inv["id"].as_str().unwrap().to_string();
    let line = post(port, co, "salesInvoiceLines", json!({ "discountPercent": 10, "documentId": id, "lineObjectNumber": "1000", "lineType": "Item", "quantity": 2 }));
    assert_eq!(num(&line["discountPercent"]), 10.0, "{line}");
    let lid = line["id"].as_str().unwrap();
    let got = get(port, co, &format!("salesInvoiceLines({lid})"));
    assert_eq!(num(&got["discountPercent"]), 10.0, "{got}");
    assert_eq!(num(&got["amountExcludingTax"]), 1800.0, "{got}");
    let hdr = get(port, co, &format!("salesInvoices({id})"));
    assert_eq!(num(&hdr["totalAmountExcludingTax"]), 1800.0, "{hdr}");

    // gaps 1 and 2: currencyCode (a page variable) is served and applied; currencyId too
    let eur = post(port, co, "salesInvoices", json!({ "customerNumber": "C10000", "currencyCode": "EUR" }));
    assert_eq!(eur["currencyCode"], "EUR", "{eur}");
    let eur_id = eur["currencyId"].as_str().unwrap().to_string();
    assert_ne!(eur_id, ZERO_ID, "{eur}");
    let got = get(port, co, &format!("salesInvoices({})", eur["id"].as_str().unwrap()));
    assert_eq!(got["currencyCode"], "EUR", "{got}");
    assert_eq!(got["currencyId"], eur_id.as_str(), "{got}");
    let by_id = post(port, co, "salesInvoices", json!({ "currencyId": eur_id, "customerNumber": "C10000" }));
    let got = get(port, co, &format!("salesInvoices({})", by_id["id"].as_str().unwrap()));
    assert_eq!(got["currencyId"], eur_id.as_str(), "{got}");
    assert_eq!(got["currencyCode"], "EUR", "{got}");
    // an unknown property is still refused
    let (st, e) = http(port, "POST", &format!("{co}/salesInvoices"), Some(&json!({ "customerNumber": "C10000", "noSuchField": 1 })));
    assert_eq!(st, 400, "{e}");

    // gap 4: a purchase line with discountPercent and unitCost before lineObjectNumber
    let pinv = post(port, co, "purchaseInvoices", json!({ "vendorNumber": "V10000" }));
    let pid = pinv["id"].as_str().unwrap().to_string();
    let pl = post(port, co, "purchaseInvoiceLines", json!({ "discountPercent": 5, "documentId": pid, "lineObjectNumber": "1000", "lineType": "Item", "quantity": 4, "unitCost": 50 }));
    assert_eq!(num(&pl["unitCost"]), 50.0, "{pl}");
    assert_eq!(num(&pl["discountPercent"]), 5.0, "{pl}");
    assert_eq!(num(&pl["netAmount"]), 190.0, "{pl}");
    // gap 5: PATCH a purchase line (the page loads its temporary lines in OnFindRecord)
    let plid = pl["id"].as_str().unwrap();
    let (st, patched) = http(port, "PATCH", &format!("{co}/purchaseInvoiceLines({plid})"), Some(&json!({ "discountPercent": 10 })));
    assert_eq!(st, 200, "{patched}");
    let got = get(port, co, &format!("purchaseInvoiceLines({plid})"));
    assert_eq!(num(&got["discountPercent"]), 10.0, "{got}");
    assert_eq!(num(&got["netAmount"]), 180.0, "{got}");
}

fn check_journals(port: u16, co: &str) {
    // gap 7: journals lists the General template's batches only (OnFindRecord), payment journals
    // their own template's (OnOpenPage)
    let names = |set: &str| -> Vec<String> { get(port, co, set)["value"].as_array().unwrap().iter().map(|b| b["code"].as_str().unwrap().to_string()).collect() };
    assert_eq!(names("journals"), vec!["DEFAULT"]);
    assert_eq!(names("customerPaymentJournals"), vec!["CASH"]);
    let batch = get(port, co, "journals")["value"][0].clone();
    let bid = batch["id"].as_str().unwrap();
    // gap 6: OnInsertRecord has no exit, so the line is inserted (it answered 201 and stored nothing)
    let jl = post(port, co, "journalLines", json!({ "journalId": bid, "accountNumber": "60100", "amount": 125.5, "description": "s6" }));
    let jid = jl["id"].as_str().unwrap().to_string();
    assert_ne!(jid, ZERO_ID, "{jl}");
    assert_eq!(jl["accountNumber"], "60100", "{jl}");
    let got = get(port, co, &format!("journalLines({jid})"));
    assert_eq!(got["accountNumber"], "60100", "{got}");
    assert_eq!(num(&got["amount"]), 125.5, "{got}");
    assert_eq!(got["journalDisplayName"], "DEFAULT", "{got}");
}
