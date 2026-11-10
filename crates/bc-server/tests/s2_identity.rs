//! Session identity without authentication (BC_AUTH off) and what it stamps (I14 S2):
//! UserSecurityId() of the default user from the User table, SystemCreatedBy / SystemModifiedBy from the
//! session's user, and bc serve's company open (crate::companyopen) raising Company Triggers, whose
//! subscribers record the sign-in (the System Application's User Login Time Tracker does this in BC).

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 2000000120 User
{
    fields
    {
        field(1; "User Security ID"; Guid) { }
        field(2; "User Name"; Code[50]) { }
    }
    keys { key(PK; "User Security ID") { } }
}

table 2000000121 "User Property"
{
    fields
    {
        field(1; "User Security ID"; Guid) { }
        field(2; Password; Text[80]) { }
    }
    keys { key(PK; "User Security ID") { } }
}

table 50100 "S2 Login"
{
    fields
    {
        field(1; "User SID"; Guid) { }
        field(2; Logins; Integer) { }
    }
    keys { key(PK; "User SID") { } }
}

table 50101 "S2 Doc"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Amount; Integer) { }
    }
    keys { key(PK; "No.") { } }
}

codeunit 2000000003 "Company Triggers"
{
    SingleInstance = true;

    [BusinessEvent(false)]
    procedure OnCompanyOpen()
    begin
    end;

    [BusinessEvent(false, true)]
    procedure OnCompanyOpenCompleted()
    begin
    end;
}

codeunit 50100 "S2 Login Tracker"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Company Triggers", 'OnCompanyOpenCompleted', '', false, false)]
    local procedure TrackLogin()
    var
        L: Record "S2 Login";
    begin
        if L.Get(UserSecurityId()) then begin
            L.Logins += 1;
            L.Modify();
        end else begin
            L.Init();
            L."User SID" := UserSecurityId();
            L.Logins := 1;
            L.Insert();
        end;
    end;
}

codeunit 50101 "S2 Tests"
{
    procedure AddUser(Sid: Guid; Name: Code[50])
    var
        U: Record User;
    begin
        U."User Security ID" := Sid;
        U."User Name" := Name;
        U.Insert();
    end;

    procedure Sid(): Text
    begin
        exit(Format(UserSecurityId()));
    end;

    procedure Make(No: Code[20])
    var
        D: Record "S2 Doc";
    begin
        D."No." := No;
        D.Insert();
    end;

    procedure Touch(No: Code[20])
    var
        D: Record "S2 Doc";
    begin
        D.Get(No);
        D.Amount += 1;
        D.Modify();
    end;

    procedure Who(No: Code[20]): Text
    var
        D: Record "S2 Doc";
    begin
        D.Get(No);
        exit(Format(D.SystemCreatedBy) + '|' + Format(D.SystemModifiedBy));
    end;

    procedure LoginCount(Sid: Guid): Integer
    var
        L: Record "S2 Login";
    begin
        if L.Get(Sid) then
            exit(L.Logins);
        exit(0);
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("s2-identity-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str, args: &[&str]) -> String {
    let ci = cat.code.iter().position(|c| c.name == "S2 Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    let args = args.iter().map(|a| al_runtime::interp::Arg::Val(match al_runtime::value::parse_guid(a).filter(|_| a.starts_with('{')) {
        Some(g) => al_runtime::Val::Guid(g),
        None => al_runtime::Val::text(*a),
    }));
    match s.invoke(ProcRef { code: ci, proc: p }, args.collect(), Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

const PLATFORM: &str = "{11111111-1111-1111-1111-111111111111}";
const ADMIN: &str = "{AAAAAAAA-0000-0000-0000-0000000000A1}";
const BOB: &str = "{B0B00000-0000-0000-0000-0000000000B2}";

#[test]
fn unauthenticated_identity_stamps_and_company_open() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    // no User rows: the platform user's id
    al_runtime::userauth::resolve_session_user(&mut s);
    assert_eq!(call(&mut s, cat, "Sid", &[]), PLATFORM);
    // the default user (ADMIN) has a User row: its own id (a missing user was not cached)
    call(&mut s, cat, "AddUser", &[ADMIN, "ADMIN"]);
    call(&mut s, cat, "AddUser", &[BOB, "BOB"]);
    al_runtime::userauth::resolve_session_user(&mut s);
    assert_eq!(call(&mut s, cat, "Sid", &[]), ADMIN);
    // a new session of the same user takes the cached id
    let mut s2 = Session::new(&rt, "CRONUS");
    al_runtime::userauth::resolve_session_user(&mut s2);
    assert_eq!(s2.user_sid, s.user_sid);

    // SystemCreatedBy / SystemModifiedBy: the user who inserted, then the user who modified
    call(&mut s, cat, "Make", &["D1"]);
    assert_eq!(call(&mut s, cat, "Who", &["D1"]), format!("{ADMIN}|{ADMIN}"));
    al_runtime::userauth::set_session_user(&mut s, "bob");
    assert_eq!(call(&mut s, cat, "Sid", &[]), BOB);
    call(&mut s, cat, "Touch", &["D1"]);
    assert_eq!(call(&mut s, cat, "Who", &["D1"]), format!("{ADMIN}|{BOB}"));

    // company open (bc serve's web client): OnCompanyOpenCompleted subscribers see the session's user
    assert_eq!(bc_server::companyopen::open(&mut s), None);
    assert_eq!(call(&mut s, cat, "LoginCount", &[BOB]), "1");
    assert_eq!(bc_server::companyopen::open(&mut s), None);
    assert_eq!(call(&mut s, cat, "LoginCount", &[BOB]), "2");
    assert_eq!(call(&mut s, cat, "LoginCount", &[ADMIN]), "0");
}
