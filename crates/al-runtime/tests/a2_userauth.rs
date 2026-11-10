//! Session identity and credentials from AL (docs/authentication.md): UserSecurityId() per session,
//! SetUserPassword only for the own user or by SUPER / SECURITY, ChangeUserPassword and
//! User."Change Password", NavUserAccountHelper's web service key helpers and credential type.

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
        field(4; State; Option) { OptionMembers = Enabled,Disabled; }
        field(5; "Expiry Date"; DateTime) { }
        field(8; "Change Password"; Boolean) { }
        field(10; "License Type"; Option) { OptionMembers = "Full User","Limited User","Device Only User","Windows Group","External User"; }
    }
    keys { key(PK; "User Security ID") { } }
}

table 2000000121 "User Property"
{
    fields
    {
        field(1; "User Security ID"; Guid) { }
        field(2; Password; Text[80]) { }
        field(5; "WebServices Key"; Text[80]) { }
        field(6; "WebServices Key Expiry Date"; DateTime) { }
    }
    keys { key(PK; "User Security ID") { } }
}

table 2000000053 "Access Control"
{
    fields
    {
        field(1; "User Security ID"; Guid) { }
        field(2; "Role ID"; Code[20]) { }
        field(3; "Company Name"; Text[30]) { }
        field(8; Scope; Option) { OptionMembers = System,Tenant; }
        field(9; "App ID"; Guid) { }
    }
    keys { key(PK; "User Security ID", "Role ID", "Company Name", Scope, "App ID") { } }
}

dotnet
{
    assembly("Microsoft.Dynamics.Nav.NavUserAccount")
    {
        type("Microsoft.Dynamics.Nav.NavUserAccount.NavUserAccountHelper"; "NavUserAccountHelper") { }
    }
}

codeunit 50140 "A2 Tests"
{
    var
        UserAccountHelper: DotNet NavUserAccountHelper;

    procedure Setup(): Text
    begin
        AddUser('{AAAAAAAA-0000-0000-0000-000000000001}', 'ADMIN', 'SUPER');
        AddUser('{CCCCCCCC-0000-0000-0000-000000000002}', 'CLERK', '');
        AddUser('{5EC00000-0000-0000-0000-000000000003}', 'SEC', 'SECURITY');
        exit('ok');
    end;

    local procedure AddUser(Sid: Guid; Name: Code[50]; Role: Code[20])
    var
        U: Record User;
        AC: Record "Access Control";
    begin
        U."User Security ID" := Sid;
        U."User Name" := Name;
        U.Insert();
        if Role <> '' then begin
            AC."User Security ID" := Sid;
            AC."Role ID" := Role;
            AC.Insert();
        end;
    end;

    procedure Sid(): Text
    begin
        exit(Format(UserSecurityId()));
    end;

    procedure SetPw(Target: Guid; Pw: Text): Text
    begin
        if SetUserPassword(Target, Pw) then
            exit('ok');
        exit('denied');
    end;

    procedure ChangePw(Old: Text; New: Text): Text
    begin
        if ChangeUserPassword(Old, New) then
            exit('ok');
        exit('wrong');
    end;

    procedure MustChange(Target: Guid): Text
    var
        U: Record User;
    begin
        U.Get(Target);
        exit(Format(U."Change Password"));
    end;

    procedure CreateKey(Target: Guid): Text
    var
        "Key": Text[80];
    begin
        if not UserAccountHelper.TryCreateWebServicesKey(Target, CreateDateTime(20991231D, 0T), "Key") then
            exit('denied:' + GetLastErrorText());
        exit("Key");
    end;

    procedure GetKey(Target: Guid): Text
    var
        "Key": Text[80];
        Expiry: DateTime;
    begin
        if not UserAccountHelper.TryGetWebServicesKey(Target, "Key", Expiry) then
            exit('denied');
        if Expiry = 0DT then
            exit("Key" + '|0');
        exit("Key" + '|' + Format(Date2DMY(DT2Date(Expiry), 3)));
    end;

    procedure ClearKey(Target: Guid): Text
    begin
        if not UserAccountHelper.TryClearWebServicesKey(Target) then
            exit('denied');
        exit(GetKey(Target));
    end;

    procedure IsNup(): Text
    begin
        exit(Format(UserAccountHelper.IsUserNamePasswordAuthentication()) + '|' + Format(UserAccountHelper.IsAccessControlServiceAuthentication()));
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("a2-userauth-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str, args: &[&str]) -> String {
    let ci = cat.code.iter().position(|c| c.name == "A2 Tests").unwrap() as u32;
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

const ADMIN: &str = "{AAAAAAAA-0000-0000-0000-000000000001}";
const CLERK: &str = "{CCCCCCCC-0000-0000-0000-000000000002}";
const SEC: &str = "{5EC00000-0000-0000-0000-000000000003}";

fn as_user(s: &mut Session, name: &str) {
    al_runtime::userauth::set_session_user(s, name);
}

#[test]
fn identity_and_credentials() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    // without a signed-in user: the platform user's id
    assert_eq!(call(&mut s, cat, "Sid", &[]), "{11111111-1111-1111-1111-111111111111}");
    assert_eq!(call(&mut s, cat, "Setup", &[]), "ok");
    as_user(&mut s, "clerk");
    assert_eq!(call(&mut s, cat, "Sid", &[]), CLERK);
    as_user(&mut s, "nobody");
    assert_eq!(s.user_sid, al_runtime::platform::USER_SID);

    // the permission system off: everyone is SUPER
    as_user(&mut s, "CLERK");
    assert_eq!(call(&mut s, cat, "SetPw", &[ADMIN, "Admin-1"]), "ok");
    al_runtime::permissions::set_enforce(true);
    // CLERK: own password yes, another user's no
    assert_eq!(call(&mut s, cat, "SetPw", &[ADMIN, "x"]), "denied");
    assert_eq!(call(&mut s, cat, "SetPw", &[CLERK, "Own-1"]), "ok");
    assert_eq!(call(&mut s, cat, "MustChange", &[CLERK]), "Yes");
    let users = al_runtime::userauth::users(&mut s, 0);
    let clerk = users.iter().find(|u| u.name == "CLERK").unwrap();
    assert_eq!(clerk.password_hash, al_runtime::userauth::stored_hash("Own-1", &clerk.sid));
    // ChangeUserPassword: the old password must match; then "Change Password" is cleared
    assert_eq!(call(&mut s, cat, "ChangePw", &["wrong", "Own-2"]), "wrong");
    assert_eq!(call(&mut s, cat, "ChangePw", &["Own-1", "Own-2"]), "ok");
    assert_eq!(call(&mut s, cat, "MustChange", &[CLERK]), "No");
    // SECURITY: other users, but not a SUPER user; SUPER: anyone
    as_user(&mut s, "SEC");
    assert_eq!(call(&mut s, cat, "SetPw", &[CLERK, "Reset-1"]), "ok");
    assert_eq!(call(&mut s, cat, "MustChange", &[CLERK]), "Yes");
    assert_eq!(call(&mut s, cat, "SetPw", &[ADMIN, "x"]), "denied");
    as_user(&mut s, "ADMIN");
    assert_eq!(call(&mut s, cat, "SetPw", &[SEC, "Sec-1"]), "ok");

    // web service access keys: own key, or SUPER / SECURITY
    as_user(&mut s, "CLERK");
    assert!(call(&mut s, cat, "CreateKey", &[ADMIN]).starts_with("denied:You need the SUPER or SECURITY permission set"));
    let key = call(&mut s, cat, "CreateKey", &[CLERK]);
    assert_eq!(key.len(), 44, "{key}");
    assert_eq!(call(&mut s, cat, "GetKey", &[CLERK]), format!("{key}|2099"));
    let u = al_runtime::userauth::users(&mut s, 0).into_iter().find(|u| u.name == "CLERK").unwrap();
    assert!(al_runtime::userauth::verify_key(&u, &key, 0));
    assert_eq!(call(&mut s, cat, "GetKey", &[ADMIN]), "denied");
    assert_eq!(call(&mut s, cat, "ClearKey", &[CLERK]), "|0");
    as_user(&mut s, "ADMIN");
    assert_eq!(call(&mut s, cat, "CreateKey", &[CLERK]).len(), 44);
    al_runtime::permissions::set_enforce(false);

    // the credential type comes from bc-server's BC_AUTH
    assert_eq!(call(&mut s, cat, "IsNup", &[]), "No|No");
    al_runtime::userauth::set_credential_type(al_runtime::userauth::CredentialType::NavUserPassword);
    assert_eq!(call(&mut s, cat, "IsNup", &[]), "Yes|No");
    al_runtime::userauth::set_credential_type(al_runtime::userauth::CredentialType::Windows);
}
