//! IsolatedStorage (crate::isolated): values in table 2000000107 "Isolated Storage", separate per app
//! (module), company and user as DataScope says; transactional; SetEncrypted encrypted at rest;
//! SecretText values stay secret; BC's key length and missing-key rules.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const TABLE: &str = r#"
table 2000000107 "Isolated Storage"
{
    DataPerCompany = false;
    fields
    {
        field(1; "App Id"; Guid) { }
        field(2; Scope; Option) { OptionMembers = Module,Company,User,CompanyAndUser; }
        field(3; "Company Name"; Text[30]) { }
        field(4; "User Id"; Guid) { }
        field(5; "Key"; Text[200]) { }
        field(6; Value; BLOB) { }
        field(7; "Encryption Status"; Option) { OptionMembers = PlainText,Encrypted,PendingForEncryption; }
        field(8; "Target Value Type"; Option) { OptionMembers = Text,SecretText; }
    }
    keys { key(Key1; "App Id", Scope, "Company Name", "User Id", "Key") { Clustered = true; } }
}
"#;

const APP_A: &str = r#"
codeunit 50150 "Iso A"
{
    procedure SetV(K: Text; V: Text; S: Integer): Text
    var
        Ok: Boolean;
    begin
        case S of
            0: Ok := IsolatedStorage.Set(K, V, DataScope::Module);
            1: Ok := IsolatedStorage.Set(K, V, DataScope::Company);
            2: Ok := IsolatedStorage.Set(K, V, DataScope::User);
            3: Ok := IsolatedStorage.Set(K, V, DataScope::CompanyAndUser);
        end;
        if Ok then
            exit('ok');
        exit('fail');
    end;

    procedure GetV(K: Text; S: Integer): Text
    var
        V: Text;
        Ok: Boolean;
    begin
        V := 'stale';
        case S of
            0: Ok := IsolatedStorage.Get(K, DataScope::Module, V);
            1: Ok := IsolatedStorage.Get(K, DataScope::Company, V);
            2: Ok := IsolatedStorage.Get(K, DataScope::User, V);
            3: Ok := IsolatedStorage.Get(K, DataScope::CompanyAndUser, V);
        end;
        if Ok then
            exit(V);
        exit('<none:' + V + '>');
    end;

    procedure Has(K: Text; S: Integer): Text
    var
        Ok: Boolean;
    begin
        case S of
            0: Ok := IsolatedStorage.Contains(K, DataScope::Module);
            1: Ok := IsolatedStorage.Contains(K, DataScope::Company);
            2: Ok := IsolatedStorage.Contains(K, DataScope::User);
            3: Ok := IsolatedStorage.Contains(K, DataScope::CompanyAndUser);
        end;
        exit(Format(Ok));
    end;

    procedure Del(K: Text; S: Integer): Text
    var
        Ok: Boolean;
    begin
        case S of
            0: Ok := IsolatedStorage.Delete(K, DataScope::Module);
            1: Ok := IsolatedStorage.Delete(K, DataScope::Company);
            2: Ok := IsolatedStorage.Delete(K, DataScope::User);
            3: Ok := IsolatedStorage.Delete(K, DataScope::CompanyAndUser);
        end;
        exit(Format(Ok));
    end;

    // default scope (Module) through the short overloads and a DataScope variable
    procedure DefaultForms(K: Text; V: Text): Text
    var
        Scope: DataScope;
        Got: Text;
        Got2: Text;
    begin
        IsolatedStorage.Set(K, V);
        IsolatedStorage.Get(K, Scope, Got);
        IsolatedStorage.Get(K, IsolationLevel::ReadCommitted, Got2);
        exit(Got + '|' + Got2 + '|' + Format(IsolatedStorage.Contains(K)) + '|' + Format(IsolatedStorage.Contains(K, DataScope::Company)));
    end;

    // return value not used: BC's DataError.ThrowError
    procedure GetThrow(K: Text): Text
    var
        V: Text;
    begin
        IsolatedStorage.Get(K, V);
        exit(V);
    end;

    procedure DelThrow(K: Text): Text
    begin
        IsolatedStorage.Delete(K);
        exit('deleted');
    end;

    procedure SetEnc(K: Text; V: Text): Text
    begin
        if IsolatedStorage.SetEncrypted(K, V, DataScope::Module) then
            exit('ok');
        exit('fail');
    end;

    procedure Raw(K: Text): Text
    var
        IsoRow: Record "Isolated Storage";
        InS: InStream;
        T: Text;
    begin
        IsoRow.SetRange("Key", K);
        if not IsoRow.FindFirst() then
            exit('<no row>');
        IsoRow.CalcFields(Value);
        IsoRow.Value.CreateInStream(InS);
        InS.ReadText(T);
        exit(Format(IsoRow."Encryption Status") + '|' + Format(IsoRow."Target Value Type") + '|' + T);
    end;

    procedure SetSecret(K: Text; V: Text): Text
    var
        S: SecretText;
    begin
        S := V;
        if IsolatedStorage.Set(K, S, DataScope::Module) then
            exit('ok');
        exit('fail');
    end;

    procedure GetSecret(K: Text): Text
    var
        S: SecretText;
        IsSecret: Boolean;
    begin
        if not IsolatedStorage.Get(K, DataScope::Module, S) then
            exit('missing');
        IsolatedStorage.Contains(K, DataScope::Module, IsSecret);
        exit('got|' + Format(IsSecret));
    end;

    procedure GetSecretAsText(K: Text): Text
    var
        T: Text;
    begin
        IsolatedStorage.Get(K, DataScope::Module, T);
        exit(T);
    end;

    procedure SessionIds(): Text
    begin
        exit(Format(Session.CurrentSessionId() = SessionId()));
    end;
}
"#;

const APP_B: &str = r#"
codeunit 50151 "Iso B"
{
    procedure GetV(K: Text): Text
    var
        V: Text;
    begin
        if IsolatedStorage.Get(K, DataScope::Module, V) then
            exit(V);
        exit('<none>');
    end;

    procedure SetV(K: Text; V: Text): Text
    begin
        IsolatedStorage.Set(K, V, DataScope::Module);
        exit('ok');
    end;
}
"#;

fn app_dir(base: &std::path::Path, name: &str, id: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let d = base.join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("app.json"), format!(r#"{{"id": "{id}", "name": "{name}", "publisher": "S2", "version": "1.0.0.0"}}"#)).unwrap();
    for (f, text) in files {
        std::fs::write(d.join(f), text).unwrap();
    }
    d
}

fn catalog() -> &'static Catalog {
    let base = std::env::temp_dir().join(format!("s2-isolated-test-{}", std::process::id()));
    let a = app_dir(&base, "AppA", "a0000000-0000-0000-0000-00000000000a", &[("Table.al", TABLE), ("A.al", APP_A)]);
    let b = app_dir(&base, "AppB", "b0000000-0000-0000-0000-00000000000b", &[("B.al", APP_B)]);
    let cat: &'static Catalog = Box::leak(Box::new(Catalog::load(&[a, b]).expect("catalog")));
    // the module map reads the app.json files: build it before they go
    assert_eq!(al_runtime::modules::modules(cat).apps.len(), 2);
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    cat
}

fn try_call(s: &mut Session, cat: &Catalog, cu: &str, proc: &str, args: &[&str]) -> Result<String, String> {
    let ci = cat.code.iter().position(|c| c.name == cu).unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    let args = args.iter().map(|a| al_runtime::interp::Arg::Val(match a.parse::<i32>() {
        Ok(i) if a.len() == 1 => al_runtime::Val::Int(i),
        _ => al_runtime::Val::text(*a),
    }));
    match s.invoke(ProcRef { code: ci, proc: p }, args.collect(), Some(Instance::new(ci)), None, None) {
        Ok(v) => Ok(v.to_text()),
        Err(Flow::Err(e)) => Err(e.msg),
        Err(_) => Err("unexpected flow".into()),
    }
}

fn call(s: &mut Session, cat: &Catalog, cu: &str, proc: &str, args: &[&str]) -> String {
    try_call(s, cat, cu, proc, args).unwrap_or_else(|e| panic!("{cu}.{proc}{args:?}: {e}"))
}

const U1: [u8; 16] = [0xA1; 16];
const U2: [u8; 16] = [0xB2; 16];

#[test]
fn isolated_storage_scopes_encryption_and_secrets() {
    assert!(std::env::var("BC_ISOLATED_STORAGE_KEY").is_err() && std::env::var("BC_ISOLATED_STORAGE_KEY_FILE").is_err(), "run without a configured key");
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let a = "Iso A";
    let mut s1 = Session::new(&rt, "CRONUS");
    s1.user_sid = U1;

    // no key on a server: SetEncrypted fails as BC's does without an encryption key
    assert_eq!(try_call(&mut s1, cat, a, "SetEnc", &["k-enc", "secret"]), Err("This operation needs an encryption key, and none has been created.".into()));
    al_runtime::isolated::use_test_key();

    // one key in all four scopes, from one session (CRONUS, U1)
    for (sc, v) in [("0", "module"), ("1", "company"), ("2", "user"), ("3", "company+user")] {
        assert_eq!(call(&mut s1, cat, a, "SetV", &["k", v, sc]), "ok");
    }
    for (sc, v) in [("0", "module"), ("1", "company"), ("2", "user"), ("3", "company+user")] {
        assert_eq!(call(&mut s1, cat, a, "GetV", &["k", sc]), v);
        assert_eq!(call(&mut s1, cat, a, "Has", &["k", sc]), "Yes");
    }
    s1.commit().unwrap();

    // another company, same user: Module and User are shared, Company and CompanyAndUser are not
    let mut s2 = Session::new(&rt, "OTHER");
    s2.user_sid = U1;
    assert_eq!(call(&mut s2, cat, a, "GetV", &["k", "0"]), "module");
    assert_eq!(call(&mut s2, cat, a, "GetV", &["k", "1"]), "<none:>");
    assert_eq!(call(&mut s2, cat, a, "GetV", &["k", "2"]), "user");
    assert_eq!(call(&mut s2, cat, a, "GetV", &["k", "3"]), "<none:>");
    // same company, another user: Module and Company are shared, User and CompanyAndUser are not
    let mut s3 = Session::new(&rt, "CRONUS");
    s3.user_sid = U2;
    assert_eq!(call(&mut s3, cat, a, "GetV", &["k", "0"]), "module");
    assert_eq!(call(&mut s3, cat, a, "GetV", &["k", "1"]), "company");
    assert_eq!(call(&mut s3, cat, a, "GetV", &["k", "2"]), "<none:>");
    assert_eq!(call(&mut s3, cat, a, "Has", &["k", "3"]), "No");
    // another app: its own Module values
    assert_eq!(call(&mut s3, cat, "Iso B", "GetV", &["k"]), "<none>");
    assert_eq!(call(&mut s3, cat, "Iso B", "SetV", &["k", "app b"]), "ok");
    assert_eq!(call(&mut s3, cat, "Iso B", "GetV", &["k"]), "app b");
    assert_eq!(call(&mut s3, cat, a, "GetV", &["k", "0"]), "module");
    // one writer at a time on the memory backend: end these sessions' transactions
    s3.commit().unwrap();
    s2.commit().unwrap();

    // overwrite, delete, then the value is gone; Delete of a missing key returns false
    assert_eq!(call(&mut s1, cat, a, "SetV", &["k", "module 2", "0"]), "ok");
    assert_eq!(call(&mut s1, cat, a, "GetV", &["k", "0"]), "module 2");
    assert_eq!(call(&mut s1, cat, a, "Del", &["k", "2"]), "Yes");
    assert_eq!(call(&mut s1, cat, a, "Has", &["k", "2"]), "No");
    assert_eq!(call(&mut s1, cat, a, "Del", &["k", "2"]), "No");
    assert_eq!(call(&mut s1, cat, a, "GetV", &["k", "3"]), "company+user");
    s1.commit().unwrap();

    // transactional: a rolled-back Set is gone
    assert_eq!(call(&mut s1, cat, a, "SetV", &["k-rb", "x", "0"]), "ok");
    s1.rollback();
    assert_eq!(call(&mut s1, cat, a, "Has", &["k-rb", "0"]), "No");

    // the short forms use Module; Get with an IsolationLevel too
    assert_eq!(call(&mut s1, cat, a, "DefaultForms", &["k-def", "v"]), "v|v|Yes|No");
    // return value not used: missing key is an error
    assert!(try_call(&mut s1, cat, a, "GetThrow", &["nope"]).unwrap_err().starts_with("The Isolated Storage does not exist."));
    assert!(try_call(&mut s1, cat, a, "DelThrow", &["nope"]).unwrap_err().starts_with("The Isolated Storage does not exist."));
    assert_eq!(call(&mut s1, cat, a, "GetThrow", &["k-def"]), "v");

    // key length: 200 is fine, 201 is not
    let k200 = "x".repeat(200);
    assert_eq!(call(&mut s1, cat, a, "SetV", &[&k200, "long", "0"]), "ok");
    assert!(try_call(&mut s1, cat, a, "SetV", &[&format!("{k200}y"), "long", "0"]).unwrap_err().contains("200"));

    // SetEncrypted: stored encrypted, read back in clear; 215 bytes at most
    assert_eq!(call(&mut s1, cat, a, "SetEnc", &["k-enc", "my api secret"]), "ok");
    let raw = call(&mut s1, cat, a, "Raw", &["k-enc"]);
    assert!(raw.starts_with("Encrypted|Text|bcis1:") && !raw.contains("my api secret"), "{raw}");
    assert_eq!(call(&mut s1, cat, a, "GetV", &["k-enc", "0"]), "my api secret");
    assert_eq!(call(&mut s1, cat, a, "Raw", &["k-def"]), "PlainText|Text|v");
    assert_eq!(try_call(&mut s1, cat, a, "SetEnc", &["k-enc2", &"z".repeat(216)]), Err("The text is too long to be encrypted.".into()));
    // the cipher: a wrong key or a changed byte is refused
    let k1 = [7u8; 32];
    let sealed = al_runtime::isolated::seal(&k1, "abc").unwrap();
    assert_eq!(al_runtime::isolated::open(&k1, &sealed).unwrap(), "abc");
    assert!(al_runtime::isolated::open(&[8u8; 32], &sealed).is_err());
    let mut bad = sealed.clone().into_bytes();
    let n = bad.len() - 3;
    bad[n] = if bad[n] == b'A' { b'B' } else { b'A' };
    assert!(al_runtime::isolated::open(&k1, &String::from_utf8(bad).unwrap()).is_err());
    assert_ne!(al_runtime::isolated::seal(&k1, "abc").unwrap(), sealed, "a random IV per value");

    // SecretText: stored as such, readable only into a SecretText
    assert_eq!(call(&mut s1, cat, a, "SetSecret", &["k-sec", "pw"]), "ok");
    assert!(call(&mut s1, cat, a, "Raw", &["k-sec"]).starts_with("PlainText|SecretText|"));
    assert_eq!(call(&mut s1, cat, a, "GetSecret", &["k-sec"]), "got|Yes");
    assert!(try_call(&mut s1, cat, a, "GetSecretAsText", &["k-sec"]).unwrap_err().contains("SecretText"));
    assert_eq!(call(&mut s1, cat, a, "GetSecretAsText", &["k-def"]), "v");

    // Session.CurrentSessionId (no such method in BC 29.1) answers like SessionId()
    assert_eq!(call(&mut s1, cat, a, "SessionIds", &[]), "Yes");
    drop((s1, s2, s3));

    postgres_part(cat);
}

/// The same table on PostgreSQL (BC_PG_TEST = a scratch database): round trip through a commit, in
/// the four scopes, and an encrypted value stored as ciphertext. Part of the test above, which owns
/// the process-wide key switch.
fn postgres_part(cat: &'static Catalog) {
    let Ok(url) = std::env::var("BC_PG_TEST") else {
        eprintln!("PostgreSQL part skipped: set BC_PG_TEST to a scratch database connection string");
        return;
    };
    let pg: &'static Runtime = Box::leak(Box::new(Runtime::new(cat, Arc::new(bc_store::PgBackend::connect_url(&url).expect("connect")))));
    pg.db.sync_schema("CRONUS", &[pg.schemas[&al_runtime::isolated::TABLE].clone()]).expect("schema");
    let a = "Iso A";
    let tag = format!("pg{}", std::process::id());
    let k = |s: &str| format!("{tag}-{s}");
    let mut s1 = Session::new(pg, "CRONUS");
    s1.user_sid = U1;
    for (sc, v) in [("0", "module"), ("1", "company"), ("2", "user"), ("3", "company+user")] {
        assert_eq!(call(&mut s1, cat, a, "SetV", &[&k("k"), v, sc]), "ok");
    }
    assert_eq!(call(&mut s1, cat, a, "SetEnc", &[&k("enc"), "pg secret"]), "ok");
    s1.commit().unwrap();
    drop(s1);
    let mut s2 = Session::new(pg, "CRONUS");
    s2.user_sid = U1;
    for (sc, v) in [("0", "module"), ("1", "company"), ("2", "user"), ("3", "company+user")] {
        assert_eq!(call(&mut s2, cat, a, "GetV", &[&k("k"), sc]), v);
    }
    let raw = call(&mut s2, cat, a, "Raw", &[&k("enc")]);
    assert!(raw.starts_with("Encrypted|Text|bcis1:") && !raw.contains("pg secret"), "{raw}");
    assert_eq!(call(&mut s2, cat, a, "GetV", &[&k("enc"), "0"]), "pg secret");
    s2.commit().unwrap();
    let mut s3 = Session::new(pg, "OTHER");
    s3.user_sid = U2;
    assert_eq!(call(&mut s3, cat, a, "GetV", &[&k("k"), "0"]), "module");
    assert_eq!(call(&mut s3, cat, a, "Has", &[&k("k"), "1"]), "No");
    assert_eq!(call(&mut s3, cat, a, "Has", &[&k("k"), "2"]), "No");
    for sc in ["0", "1", "2", "3"] {
        let _ = call(&mut s3, cat, a, "Del", &[&k("k"), sc]);
    }
    s3.commit().unwrap();
    let mut s4 = Session::new(pg, "CRONUS");
    s4.user_sid = U1;
    assert_eq!(call(&mut s4, cat, a, "Has", &[&k("k"), "0"]), "No");
    assert_eq!(call(&mut s4, cat, a, "GetV", &[&k("k"), "1"]), "company");
    assert_eq!(call(&mut s4, cat, a, "Del", &[&k("k"), "1"]), "Yes");
    assert_eq!(call(&mut s4, cat, a, "Del", &[&k("k"), "2"]), "Yes");
    assert_eq!(call(&mut s4, cat, a, "Del", &[&k("k"), "3"]), "Yes");
    assert_eq!(call(&mut s4, cat, a, "Del", &[&k("enc"), "0"]), "Yes");
    s4.commit().unwrap();
}
