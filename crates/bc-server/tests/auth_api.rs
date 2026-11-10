//! `BC_AUTH=NavUserPassword` over HTTP (docs/authentication.md): Basic auth with a password or a web service
//! access key against User / "User Property" in BC's format, the lockout, and the signed-in user's permission
//! sets in the request's session (`BC_PERMISSIONS=enforce`), and Entra bearer tokens signed with a key made
//! at test time (`BC_AUTH_ENTRA_JWKS` = a file). Skipped when vendor/bcapps is not present.
//! The switches are process-wide, so this file holds a single test.

use al_runtime::{Session, Val};
use bc_server::app::*;
use serde_json::{json, Value as J};
use std::io::{Read, Write};

fn root() -> Option<std::path::PathBuf> {
    let r = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().ok()?;
    r.join("vendor/bcapps/src/Layers/W1/BaseApp").exists().then_some(r)
}

/// One HTTP/1.1 request with `Connection: close`: (status, response head, JSON body or Null).
fn http(port: u16, method: &str, path: &str, auth: Option<(&str, &str)>, body: Option<&J>) -> (u16, String, J) {
    use base64::Engine;
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    let b = body.map(|b| b.to_string()).unwrap_or_default();
    let a = match auth {
        Some(("Bearer", t)) => format!("Authorization: Bearer {t}\r\n"),
        Some((u, p)) => format!("Authorization: Basic {}\r\n", base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"))),
        None => String::new(),
    };
    write!(c, "{method} {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n{a}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}", b.len()).unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    let (head, rest) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let json = rest.find(['{', '[']).and_then(|i| serde_json::Deserializer::from_str(&rest[i..]).into_iter::<J>().next()).and_then(|r| r.ok()).unwrap_or(J::Null);
    (status, head.to_string(), json)
}

const TENANT: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";

fn b64url(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// An RS256 token with `claims`, signed by `key` under kid "test".
fn token(key: &rsa::RsaPrivateKey, claims: J) -> String {
    use rsa::signature::{SignatureEncoding, Signer};
    let input = format!("{}.{}", b64url(json!({"alg": "RS256", "kid": "test"}).to_string().as_bytes()), b64url(claims.to_string().as_bytes()));
    let sk = rsa::pkcs1v15::SigningKey::<sha2_010::Sha256>::new(key.clone());
    format!("{input}.{}", b64url(&sk.sign(input.as_bytes()).to_bytes()))
}

fn claims(oid: &str, upn: &str, aud: &str) -> J {
    let now = chrono::Utc::now().timestamp();
    json!({"iss": format!("https://login.microsoftonline.com/{TENANT}/v2.0"), "tid": TENANT, "aud": aud, "exp": now + 600, "nbf": now - 10, "oid": oid, "upn": upn, "scp": "Financials.ReadWrite.All"})
}

/// A user (enabled unless `disabled`) with Access Control rows for `roles`.
fn add_user(s: &mut Session, name: &str, sid: u8, roles: &[&str], disabled: bool) -> [u8; 16] {
    let sid = [sid; 16];
    let user = s.cat.table_by_name("User").unwrap().id;
    let u = s.new_record(user, false);
    set(s, &u, "User Security ID", Val::Guid(sid)).unwrap();
    set(s, &u, "User Name", Val::text(name)).unwrap();
    if disabled {
        let fi = u.borrow().tdef.field("State").unwrap();
        u.borrow_mut().row[fi] = bc_store::Value::Option(1);
    }
    s.rec_insert(&u, false).map_err(flow_err).unwrap().unwrap();
    let ac = s.cat.table_by_name("Access Control").unwrap().id;
    for r in roles {
        let a = s.new_record(ac, false);
        set(s, &a, "User Security ID", Val::Guid(sid)).unwrap();
        set(s, &a, "Role ID", Val::text(*r)).unwrap();
        s.rec_insert(&a, false).map_err(flow_err).unwrap().unwrap();
    }
    sid
}

#[test]
fn navuserpassword_api() {
    let Some(root) = root() else { return };
    // the process-wide switches, read on first use
    std::env::set_var("BC_AUTH", "NavUserPassword,Entra");
    std::env::set_var("BC_AUTH_LOCKOUT", "3/60/60");
    std::env::set_var("BC_PERMISSIONS", "enforce");
    // Entra: a key pair made now, its JWKS in a file
    use rsa::traits::PublicKeyParts;
    let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let jwks = json!({"keys": [{"kty": "RSA", "use": "sig", "kid": "test", "n": b64url(&key.n().to_bytes_be()), "e": b64url(&key.e().to_bytes_be())}]});
    let jwks_file = std::env::temp_dir().join(format!("a2-auth-api-jwks-{}.json", std::process::id()));
    std::fs::write(&jwks_file, jwks.to_string()).unwrap();
    std::env::set_var("BC_AUTH_ENTRA_TENANT", TENANT);
    std::env::set_var("BC_AUTH_ENTRA_AUDIENCE", "api://bc-rust-test");
    std::env::set_var("BC_AUTH_ENTRA_JWKS", &jwks_file);
    let app: &'static App = Box::leak(Box::new(boot(&root, Db::Memory).unwrap()));
    const KEY: &str = "test-access-key-not-a-secret";
    {
        let mut s = Session::new(&app.rt, "CRONUS");
        bc_server::seed::seed(&mut s).unwrap();
        s.init_platform_data().map_err(flow_err).unwrap();
        // ADMIN (SUPER, from the platform data) gets a password the way AL's SetUserPassword stores it
        let admin = al_runtime::userauth::users(&mut s, 0).into_iter().find(|u| u.name == "ADMIN").expect("platform user").sid;
        al_runtime::userauth::set_user_password(&mut s, &admin, "Admin-Passw0rd!", false).unwrap();
        let clerk = add_user(&mut s, "CLERK", 0x21, &["D365 CUSTOMER, EDIT"], false);
        let u = s.new_record(s.cat.table_by_name("User").unwrap().id, false);
        set(&mut s, &u, "User Security ID", Val::Guid(clerk)).unwrap();
        let row = s.rec_get_pk(&u).unwrap();
        u.borrow_mut().row = row.into();
        set(&mut s, &u, "Authentication Email", Val::text("clerk@contoso.example")).unwrap();
        s.rec_modify(&u, false).map_err(flow_err).unwrap().unwrap();
        al_runtime::userauth::set_user_password(&mut s, &clerk, "Clerk-Passw0rd!", true).unwrap();
        let reader = add_user(&mut s, "READER", 0x22, &["D365 READ"], false);
        let up = s.new_record(al_runtime::userauth::USER_PROPERTY_TABLE, false);
        set(&mut s, &up, "User Security ID", Val::Guid(reader)).unwrap();
        set(&mut s, &up, "WebServices Key", Val::text(KEY)).unwrap();
        s.rec_insert(&up, false).map_err(flow_err).unwrap().unwrap();
        let off = add_user(&mut s, "DISABLED", 0x23, &["SUPER"], true);
        al_runtime::userauth::set_user_password(&mut s, &off, "Off-Passw0rd!", false).unwrap();
        // the stored value is BC's V3 hash (CLERK must change it at the next web client sign-in, which the API ignores)
        let u = al_runtime::userauth::users(&mut s, 0).into_iter().find(|u| u.name == "CLERK").unwrap();
        assert_eq!(u.password_hash, al_runtime::userauth::stored_hash("Clerk-Passw0rd!", &clerk));
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
            axum::serve(l, bc_server::api::router(ctx).into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap();
        });
    });
    let co = format!("/api/v2.0/companies({})", bc_server::odata::COMPANY_ID);
    let customers = format!("{co}/customers");

    // no credentials: 401 with the Basic challenge, also for the own web client's endpoints
    let (st, head, _) = http(port, "GET", "/api/v2.0/companies", None, None);
    assert_eq!(st, 401);
    assert!(head.to_ascii_lowercase().contains("www-authenticate: basic realm="), "{head}");
    assert_eq!(http(port, "GET", "/api/home", None, None).0, 401);
    // wrong and right password
    assert_eq!(http(port, "GET", &customers, Some(("CLERK", "clerk-passw0rd!")), None).0, 401);
    let (st, _, body) = http(port, "GET", "/api/v2.0/companies", Some(("clerk", "Clerk-Passw0rd!")), None);
    assert_eq!(st, 200, "{body}");
    assert!(body["value"].as_array().is_some_and(|v| !v.is_empty()));
    // reading customers runs the API page's OnAfterGetRecord, as in BC; it reads General Ledger Setup
    // (TranslateNAVCurrencyCodeToCurrencyCode), which D365 CUSTOMER, EDIT doesn't grant
    // a permission error is 403 Internal_ServerError in BC 29.1 (measured by I15 O)
    let (st, _, body) = http(port, "GET", &customers, Some(("CLERK", "Clerk-Passw0rd!")), None);
    assert_eq!(st, 403, "{body}");
    assert_eq!(body["error"]["code"], json!("Internal_ServerError"), "{body}");
    assert!(body.to_string().contains("TableData 98 General Ledger Setup Read"), "{body}");
    // the session runs as CLERK: Customer.OnInsert reads Sales & Receivables Setup, which D365 CUSTOMER, EDIT
    // doesn't grant, while ADMIN (SUPER) inserts
    let (st, _, body) = http(port, "POST", &customers, Some(("CLERK", "Clerk-Passw0rd!")), Some(&json!({"displayName": "Auth Test Clerk"})));
    assert_eq!(st, 403, "{body}");
    assert!(body.to_string().contains("Sorry, the current permissions prevented the action"), "{body}");
    let (st, _, body) = http(port, "POST", &customers, Some(("ADMIN", "Admin-Passw0rd!")), Some(&json!({"displayName": "Auth Test Admin"})));
    assert_eq!(st, 201, "{body}");
    // the API page's source table is checked for the signed-in user (A1's hook in odata.rs): D365 CUSTOMER, EDIT
    // doesn't read vendors
    let (st, _, body) = http(port, "GET", &format!("{co}/vendors"), Some(("CLERK", "Clerk-Passw0rd!")), None);
    assert_eq!(st, 403, "{body}");
    assert!(body.to_string().contains("TableData 23 Vendor Read"), "{body}");
    assert_eq!(http(port, "GET", &format!("{co}/vendors"), Some(("ADMIN", "Admin-Passw0rd!")), None).0, 200);
    // Entra: the first token by email binds the user's object id; then a second account with the same email
    // is refused, and so is a wrong audience
    let t = token(&key, claims("0b5c2ce0-5a43-4f2b-9b6c-1d2e3f405162", "Clerk@Contoso.example", "api://bc-rust-test"));
    let (st, _, body) = http(port, "GET", "/api/v2.0/companies", Some(("Bearer", &t)), None);
    assert_eq!(st, 200, "{body}");
    {
        let mut s = Session::new(&app.rt, "CRONUS");
        let u = al_runtime::userauth::users(&mut s, 0).into_iter().find(|u| u.name == "CLERK").unwrap();
        assert_eq!(u.authentication_object_id, "0b5c2ce0-5a43-4f2b-9b6c-1d2e3f405162");
    }
    // the session runs as CLERK here too (A1's hook: no vendors)
    assert_eq!(http(port, "GET", &format!("{co}/vendors"), Some(("Bearer", &t)), None).0, 403);
    let other = token(&key, claims("99999999-0000-0000-0000-000000000000", "clerk@contoso.example", "api://bc-rust-test"));
    assert_eq!(http(port, "GET", "/api/v2.0/companies", Some(("Bearer", &other)), None).0, 401);
    let wrong_aud = token(&key, claims("0b5c2ce0-5a43-4f2b-9b6c-1d2e3f405162", "clerk@contoso.example", "https://graph.microsoft.com"));
    assert_eq!(http(port, "GET", "/api/v2.0/companies", Some(("Bearer", &wrong_aud)), None).0, 401);
    // the keys are cached now; the test leaves no file behind
    let _ = std::fs::remove_file(&jwks_file);
    // web service access key; it is not a password
    assert_eq!(http(port, "GET", "/api/v2.0/companies", Some(("READER", KEY)), None).0, 200);
    assert_eq!(http(port, "GET", "/api/v2.0/companies", Some(("READER", "x")), None).0, 401);
    // a disabled user can't sign in with the right password
    assert_eq!(http(port, "GET", "/api/v2.0/companies", Some(("DISABLED", "Off-Passw0rd!")), None).0, 401);
    // lockout: 3 failures for READER from this address, then even the key is refused for a while
    for _ in 0..2 {
        assert_eq!(http(port, "GET", "/api/v2.0/companies", Some(("READER", "nope")), None).0, 401);
    }
    let (st, head, _) = http(port, "GET", "/api/v2.0/companies", Some(("READER", KEY)), None);
    assert_eq!(st, 429, "{head}");
    assert!(head.to_ascii_lowercase().contains("retry-after:"), "{head}");
    // other users are not affected
    assert_eq!(http(port, "GET", "/api/v2.0/companies", Some(("ADMIN", "Admin-Passw0rd!")), None).0, 200);
}
