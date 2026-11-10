//! I15 T: NavUserAccountHelper members answered from the User, User Property and Company tables
//! (Ncl UserManagement / NavUser, NavUserAccount.dll) instead of returning nothing
//! (DotNet.Neutral): IsWindowsAuthentication, IsDelegatedUser, TryGetAuthenticationKey,
//! GetAuthenticationStatus, IsPasswordSet, GetAllowedCompanies; and the native "Agent Utilities"
//! codeunit's BlockPageFromBeingOpenedByAgent (no agent sessions).

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP_JSON: &str = r#"{ "id": "3f6b9e21-0d4c-4b7a-a2e5-91c8d7f0b364", "name": "T15 User Account Test", "publisher": "bc-rust", "version": "1.0.0.0" }"#;

const APP: &str = r#"
table 2000000120 User
{
    DataPerCompany = false;
    fields
    {
        field(1; "User Security ID"; Guid) { }
        field(2; "User Name"; Code[50]) { }
        field(7; "Windows Security ID"; Text[119]) { }
        field(11; "Authentication Email"; Text[250]) { }
    }
    keys { key(PK; "User Security ID") { } }
}

table 2000000121 "User Property"
{
    DataPerCompany = false;
    fields
    {
        field(1; "User Security ID"; Guid) { }
        field(2; Password; Text[80]) { }
        field(3; "Name Identifier"; Text[250]) { }
        field(4; "Authentication Key"; Text[80]) { }
        field(5; "WebServices Key"; Text[80]) { }
        field(6; "WebServices Key Expiry Date"; DateTime) { }
        field(7; "Authentication Object ID"; Text[80]) { }
    }
    keys { key(PK; "User Security ID") { } }
}

table 2000000006 Company
{
    DataPerCompany = false;
    fields
    {
        field(1; Name; Text[30]) { }
    }
    keys { key(PK; Name) { } }
}

dotnet
{
    assembly("Microsoft.Dynamics.Nav.NavUserAccount")
    {
        type("Microsoft.Dynamics.Nav.NavUserAccount.NavUserAccountHelper"; "T15UserAccountHelper") { }
    }
}

codeunit 50774 "T15 User Account"
{
    procedure Setup()
    var
        User: Record User;
        UserProperty: Record "User Property";
        Company: Record Company;
    begin
        // A: password and authentication key, no Entra object id, no email
        User."User Security ID" := '{0000000A-0000-0000-0000-000000000000}';
        User."User Name" := 'A';
        User.Insert();
        UserProperty."User Security ID" := User."User Security ID";
        UserProperty.Password := 'hash';
        UserProperty."Authentication Key" := 'KEY-A';
        UserProperty.Insert();
        // B: an authentication email, no password
        User."User Security ID" := '{0000000B-0000-0000-0000-000000000000}';
        User."User Name" := 'B';
        User."Authentication Email" := 'b@contoso.com';
        User.Insert();
        UserProperty.Init();
        UserProperty."User Security ID" := User."User Security ID";
        UserProperty.Insert();
        // C: an Entra object id
        User."User Security ID" := '{0000000C-0000-0000-0000-000000000000}';
        User."User Name" := 'C';
        User."Authentication Email" := '';
        User.Insert();
        UserProperty.Init();
        UserProperty."User Security ID" := User."User Security ID";
        UserProperty."Authentication Object ID" := 'oid-c';
        UserProperty.Insert();
        // D: a User row only (BC creates the User Property row with it; bc-rust does not)
        User."User Security ID" := '{0000000D-0000-0000-0000-000000000000}';
        User."User Name" := 'D';
        User.Insert();
        Company.Name := 'CRONUS';
        Company.Insert();
        Company.Name := 'My Company';
        Company.Insert();
    end;

    procedure Flags(): Text
    var
        Helper: DotNet T15UserAccountHelper;
    begin
        exit(Format(Helper.IsDelegatedUser()) + '|' + Format(Helper.IsUserDelegatedAdmin()) + '|' + Format(Helper.IsUserDelegatedHelpdesk()));
    end;

    procedure WindowsAuth(): Text
    var
        Helper: DotNet T15UserAccountHelper;
    begin
        exit(Format(Helper.IsWindowsAuthentication()));
    end;

    procedure Keys(): Text
    var
        Helper: DotNet T15UserAccountHelper;
        A: Guid;
        D: Guid;
        F: Guid;
        Key: Text;
        Result: Text;
    begin
        Evaluate(A, '{0000000A-0000-0000-0000-000000000000}');
        Evaluate(D, '{0000000D-0000-0000-0000-000000000000}');
        Evaluate(F, '{0000000F-0000-0000-0000-000000000000}');
        Result := Format(Helper.TryGetAuthenticationKey(A, Key)) + ':' + Key;
        Result += '|' + Format(Helper.TryGetAuthenticationKey(F, Key)) + ':' + Key;
        Result += '|' + Format(Helper.TryGetAuthenticationKey(D, Key)) + ':' + Key;
        exit(Result);
    end;

    procedure Status(): Text
    var
        Helper: DotNet T15UserAccountHelper;
        A: Guid;
        B: Guid;
        C: Guid;
        D: Guid;
    begin
        Evaluate(A, '{0000000A-0000-0000-0000-000000000000}');
        Evaluate(B, '{0000000B-0000-0000-0000-000000000000}');
        Evaluate(C, '{0000000C-0000-0000-0000-000000000000}');
        Evaluate(D, '{0000000D-0000-0000-0000-000000000000}');
        exit(Format(Helper.GetAuthenticationStatus(A)) + '|' + Format(Helper.GetAuthenticationStatus(B)) + '|' + Format(Helper.GetAuthenticationStatus(C)) + '|' + Format(Helper.GetAuthenticationStatus(D)));
    end;

    procedure Passwords(): Text
    var
        Helper: DotNet T15UserAccountHelper;
        A: Guid;
        B: Guid;
        F: Guid;
        D: Guid;
    begin
        Evaluate(A, '{0000000A-0000-0000-0000-000000000000}');
        Evaluate(B, '{0000000B-0000-0000-0000-000000000000}');
        Evaluate(F, '{0000000F-0000-0000-0000-000000000000}');
        Evaluate(D, '{0000000D-0000-0000-0000-000000000000}');
        exit(Format(Helper.IsPasswordSet(A)) + '|' + Format(Helper.IsPasswordSet(B)) + '|' + Format(Helper.IsPasswordSet(F)) + '|' + Format(Helper.IsPasswordSet(D)));
    end;

    procedure Companies(): Text
    var
        Helper: DotNet T15UserAccountHelper;
        CompanyName: Text;
        Result: Text;
    begin
        foreach CompanyName in Helper.GetAllowedCompanies() do
            Result += CompanyName + ';';
        exit(Result);
    end;

    procedure AgentBlock(): Text
    var
        AgentUtilities: Codeunit "Agent Utilities";
    begin
        AgentUtilities.BlockPageFromBeingOpenedByAgent();
        exit('opened');
    end;

    procedure OtherPlatformMember(): Text
    var
        AgentUtilities: Codeunit "Agent Utilities";
    begin
        AgentUtilities.T15NoSuchMember();
        exit('done');
    end;

    procedure MissingUser(): Text
    var
        Helper: DotNet T15UserAccountHelper;
        F: Guid;
    begin
        Evaluate(F, '{0000000F-0000-0000-0000-000000000000}');
        asserterror Helper.GetAuthenticationStatus(F);
        exit(CopyStr(GetLastErrorText(), 1, 35));
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("t15-user-account-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.json"), APP_JSON).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == "T15 User Account").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn user_account_helper_members_read_the_user_tables() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    call(&mut s, cat, "Setup");
    al_runtime::assume::reset();

    // no Entra directory roles
    assert_eq!(call(&mut s, cat, "Flags"), "No|No|No");
    // the User Property row's Authentication Key; no row: false and ''
    // a user without a User Property row reads as an empty one (BC creates it with the user)
    assert_eq!(call(&mut s, cat, "Keys"), "Yes:KEY-A|No:|Yes:");
    // Disabled (0) without object id and email, Inactive (1) with an email, Active (2) with an object id
    assert_eq!(call(&mut s, cat, "Status"), "0|1|2|0");
    assert_eq!(call(&mut s, cat, "Passwords"), "Yes|No|No|No");
    assert_eq!(call(&mut s, cat, "Companies"), "CRONUS;My Company;");
    assert_eq!(call(&mut s, cat, "MissingUser"), "There is no User within the filter.");
    let r = al_runtime::assume::take();
    assert!(r.assumed.is_empty(), "{r:?}");

    // IsWindowsAuthentication: false, a fixed fact (no Windows accounts or groups on this tier)
    assert_eq!(call(&mut s, cat, "WindowsAuth"), "No");
    assert!(al_runtime::assume::take().assumed.is_empty());

    // "Agent Utilities" (native codeunit 2000000022): no agent sessions, the page opens
    assert_eq!(call(&mut s, cat, "AgentBlock"), "opened");
    assert!(al_runtime::assume::take().assumed.is_empty());
    // its other members are still a guessed default, recorded per member
    assert_eq!(call(&mut s, cat, "OtherPlatformMember"), "done");
    let r = al_runtime::assume::take();
    assert_eq!(r.assumed, vec![("PlatformObject.Neutral", 1), ("PlatformObject.Neutral:agent utilities.T15NoSuchMember", 1)]);
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("t15-user-account-test-{}", std::process::id())));
}
