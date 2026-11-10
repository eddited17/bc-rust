//! Permissions (P13, docs/permissions.md): permission sets with includes and extensions, direct
//! and indirect permissions through an object's `Permissions`, a table's `InherentPermissions`,
//! the empty-table rule, ReadPermission, and BC's error text and code.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 50130 "P13 Data"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { } }
}

table 50131 "P13 Inherent"
{
    InherentPermissions = RIMD;
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { } }
}

table 50132 "P13 Other"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { } }
}

table 2000000251 "Metadata Permission"
{
    fields
    {
        field(1; "App ID"; Guid) { }
        field(2; "Role ID"; Code[30]) { }
        field(3; "Role Name"; Text[30]) { }
        field(4; "Object Type"; Option) { OptionMembers = "Table Data","Table",,"Report",,"Codeunit","XMLport","MenuSuite","Page","Query","System"; }
        field(5; "Object ID"; Integer) { }
        field(6; "Object Name"; Text[249]) { }
        field(7; "Read Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(8; "Insert Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(9; "Modify Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(10; "Delete Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(11; "Execute Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
    }
    keys { key(Key1; "App ID", "Role ID", "Object Type", "Object ID") { } }
}

table 2000000254 "Expanded Permission"
{
    fields
    {
        field(1; "App ID"; Guid) { }
        field(2; "Role ID"; Code[30]) { }
        field(3; "Role Name"; Text[30]) { }
        field(4; "Object Type"; Option) { OptionMembers = "Table Data","Table",,"Report",,"Codeunit","XMLport","MenuSuite","Page","Query","System"; }
        field(5; "Object ID"; Integer) { }
        field(6; "Object Name"; Text[249]) { }
        field(7; "Read Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(8; "Insert Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(9; "Modify Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(10; "Delete Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(11; "Execute Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(14; Scope; Option) { OptionMembers = System,Tenant; }
    }
    keys { key(Key1; "App ID", "Role ID", "Object Type", "Object ID") { } }
}

table 2000000166 "Tenant Permission"
{
    fields
    {
        field(1; "App ID"; Guid) { }
        field(2; "Role ID"; Code[20]) { }
        field(3; "Role Name"; Text[30]) { }
        field(4; "Object Type"; Option) { OptionMembers = "Table Data","Table",,"Report",,"Codeunit","XMLport","MenuSuite","Page","Query","System"; }
        field(5; "Object ID"; Integer) { }
        field(6; "Object Name"; Text[249]) { }
        field(7; "Read Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(8; "Insert Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(9; "Modify Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(10; "Delete Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(11; "Execute Permission"; Option) { OptionMembers = " ",Yes,Indirect; }
        field(13; Type; Option) { OptionMembers = Include,Exclude; }
    }
    keys { key(Key1; "App ID", "Role ID", "Object Type", "Object ID") { } }
}

table 2000000165 "Tenant Permission Set"
{
    fields
    {
        field(1; "App ID"; Guid) { }
        field(2; "Role ID"; Code[20]) { }
        field(3; Name; Text[30]) { }
    }
    keys { key(Key1; "App ID", "Role ID") { } }
}

table 2000000253 "Tenant Permission Set Rel."
{
    fields
    {
        field(1; "App ID"; Guid) { }
        field(2; "Role ID"; Code[30]) { }
        field(3; "Related App ID"; Guid) { }
        field(4; "Related Role ID"; Code[30]) { }
        field(5; Type; Option) { OptionMembers = Include,Exclude; }
        field(6; "Related Scope"; Option) { OptionMembers = System,Tenant; }
    }
    keys { key(Key1; "App ID", "Role ID", "Related App ID", "Related Role ID") { } }
}

table 2000000120 User
{
    fields
    {
        field(1; "User Security ID"; Guid) { }
        field(2; "User Name"; Code[50]) { }
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
        field(4; Scope; Option) { OptionMembers = System,Tenant; }
        field(5; "App ID"; Guid) { }
    }
    keys { key(PK; "User Security ID", "Role ID", "Company Name", Scope, "App ID") { } }
}

table 50135 "P13 Guarded"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { } }

    trigger OnDelete()
    begin
        Error('Guarded');
    end;
}

table 50136 "P13 Bulk"
{
    fields { field(1; "No."; Code[20]) { } }
    keys { key(PK; "No.") { } }
}

table 50138 "P13 Logged"
{
    fields { field(1; "No."; Code[20]) { } field(2; Name; Text[30]) { } }
    keys { key(PK; "No.") { } }
}

codeunit 2000000002 "Global Triggers"
{
    [BusinessEvent(false)]
    procedure GetDatabaseTableTriggerSetup(TableId: Integer; var OnDatabaseInsert: Boolean; var OnDatabaseModify: Boolean; var OnDatabaseDelete: Boolean; var OnDatabaseRename: Boolean)
    begin
    end;
}

codeunit 50139 "P13 Trigger Setup"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Global Triggers", 'GetDatabaseTableTriggerSetup', '', false, false)]
    local procedure GetSetup(TableId: Integer; var OnDatabaseInsert: Boolean; var OnDatabaseModify: Boolean; var OnDatabaseDelete: Boolean; var OnDatabaseRename: Boolean)
    begin
        if TableId = Database::"P13 Logged" then begin
            OnDatabaseModify := true;
            OnDatabaseDelete := true;
        end;
    end;
}

codeunit 50137 "P13 Subscriber"
{
    [EventSubscriber(ObjectType::Table, Database::"P13 Guarded", 'OnAfterDeleteEvent', '', false, false)]
    local procedure OnAfterDeleteGuarded(var Rec: Record "P13 Guarded"; RunTrigger: Boolean)
    begin
    end;
}

permissionset 50130 "P13 READ"
{
    Assignable = true;
    Permissions = tabledata "P13 Data" = R;
}

permissionset 50131 "P13 WRITE"
{
    Assignable = true;
    IncludedPermissionSets = "P13 READ";
    Permissions = tabledata "P13 Data" = imd;
}

permissionsetextension 50132 "P13 READ EXT" extends "P13 READ"
{
    Permissions = tabledata "P13 Other" = R;
}

codeunit 50130 "P13 Poster"
{
    Permissions = tabledata "P13 Data" = rimd;

    procedure InsertRow(No: Code[20])
    var
        D: Record "P13 Data";
    begin
        D."No." := No;
        D.Insert();
    end;
}

codeunit 50131 "P13 Plain"
{
    procedure InsertRow(No: Code[20])
    var
        D: Record "P13 Data";
    begin
        D."No." := No;
        D.Insert();
    end;

    procedure InsertInherent(No: Code[20])
    var
        D: Record "P13 Inherent";
    begin
        D."No." := No;
        D.Insert();
    end;
}

codeunit 50134 "P13 Method Inherent"
{
    Permissions = tabledata "P13 Data" = i;

    [InherentPermissions(PermissionObjectType::TableData, Database::"P13 Data", 'I')]
    procedure InsertDirect(No: Code[20])
    var
        D: Record "P13 Data";
    begin
        D."No." := No;
        D.Insert();
    end;

    [InherentPermissions(PermissionObjectType::TableData, Database::"P13 Data", 'i')]
    procedure InsertIndirect(No: Code[20])
    var
        D: Record "P13 Data";
    begin
        D."No." := No;
        D.Insert();
    end;

    procedure InsertWithout(No: Code[20])
    var
        D: Record "P13 Data";
    begin
        D."No." := No;
        D.Insert();
    end;
}

codeunit 50133 "P13 Tests"
{
    procedure SeedWrites(): Text
    var
        G: Record "P13 Guarded";
        B: Record "P13 Bulk";
        L: Record "P13 Logged";
    begin
        G."No." := 'G1';
        G.Insert();
        B."No." := 'B1';
        B.Insert();
        L."No." := 'L1';
        L.Insert();
        exit('ok');
    end;

    procedure WritesAfterTriggers(): Text
    var
        G: Record "P13 Guarded";
        B: Record "P13 Bulk";
        L: Record "P13 Logged";
        Out: Text;
    begin
        // the OnDelete trigger's error comes before the permission check
        G."No." := 'G1';
        asserterror G.Delete(true);
        Out := GetLastErrorText();
        // without the trigger: denied
        asserterror G.Delete(false);
        Out += '|' + GetLastErrorCode();
        // bulk DeleteAll (no trigger, no subscriber): checked up front, the table isn't empty
        B.SetRange("No.", 'ZZZ');
        asserterror B.DeleteAll();
        Out += '|' + GetLastErrorCode();
        // P13 Guarded has a delete subscriber: record by record, nothing matches, nothing checked
        G.SetRange("No.", 'ZZZ');
        G.DeleteAll();
        // P13 Logged has global database triggers (Global Triggers.GetDatabaseTableTriggerSetup):
        // record by record too, nothing matches, nothing checked
        L.SetRange("No.", 'ZZZ');
        L.DeleteAll();
        L.ModifyAll(Name, 'x');
        // without a filter it reaches L1: denied there
        L.Reset();
        asserterror L.ModifyAll(Name, 'x');
        Out += '|' + GetLastErrorCode();
        exit(Out + '|ok');
    end;

    procedure AddUser(Role: Code[20]): Text
    var
        U: Record User;
        AC: Record "Access Control";
    begin
        if not U.Get('{00000000-0000-0000-0000-0000000000A1}') then begin
            U."User Security ID" := '{00000000-0000-0000-0000-0000000000A1}';
            U."User Name" := 'LIMITED';
            U.Insert();
        end;
        AC."User Security ID" := U."User Security ID";
        AC."Role ID" := Role;
        AC.Insert();
        exit('ok');
    end;

    procedure AddUserSuper(): Text
    begin
        exit(AddUser('SUPER'));
    end;

    procedure AddUserRead(): Text
    begin
        exit(AddUser('P13 READ'));
    end;

    procedure PermTables(): Text
    var
        MP: Record "Metadata Permission";
        EP: Record "Expanded Permission";
        TPS: Record "Tenant Permission Set";
        TP: Record "Tenant Permission";
        Rel: Record "Tenant Permission Set Rel.";
        Out: Text;
    begin
        MP.SetRange("Role ID", 'P13 WRITE');
        Out := Format(MP.Count());
        EP.SetRange("Role ID", 'P13 WRITE');
        EP.SetRange("Object ID", Database::"P13 Data");
        EP.FindFirst();
        Out += '|' + Format(EP."Read Permission") + ',' + Format(EP."Insert Permission");
        EP.SetRange("Role ID", 'P13 READ');
        EP.SetRange("Object ID");
        Out += '|' + Format(EP.Count());
        // a tenant set: its own permission, and P13 WRITE included minus P13 READ excluded
        TPS."Role ID" := 'T1';
        TPS.Name := 'Tenant one';
        TPS.Insert();
        TP."Role ID" := 'T1';
        TP."Object Type" := TP."Object Type"::"Table Data";
        TP."Object ID" := Database::"P13 Other";
        TP."Modify Permission" := TP."Modify Permission"::Yes;
        TP.Insert();
        Rel."Role ID" := 'T1';
        Rel."Related Role ID" := 'P13 WRITE';
        Rel.Insert();
        Rel."Related Role ID" := 'P13 READ';
        Rel.Type := Rel.Type::Exclude;
        Rel.Insert();
        EP.Reset();
        EP.SetRange("Role ID", 'T1');
        EP.SetRange(Scope, EP.Scope::Tenant);
        EP.SetRange("Object ID", Database::"P13 Data");
        EP.FindFirst();
        Out += '|' + Format(EP."Read Permission") + ',' + Format(EP."Insert Permission");
        EP.SetRange("Object ID", Database::"P13 Other");
        EP.FindFirst();
        Out += '|' + Format(EP."Read Permission") + ',' + Format(EP."Modify Permission");
        exit(Out);
    end;

    procedure MethodInherent(): Text
    var
        M: Codeunit "P13 Method Inherent";
    begin
        M.InsertDirect('M1');
        M.InsertIndirect('M2');
        asserterror M.InsertWithout('M3');
        exit(GetLastErrorCode());
    end;

    procedure PlainInsert(): Text
    var
        Plain: Codeunit "P13 Plain";
    begin
        asserterror Plain.InsertRow('A');
        exit(GetLastErrorCode() + '|' + GetLastErrorText());
    end;

    procedure PosterInsert(): Text
    var
        Poster: Codeunit "P13 Poster";
        D: Record "P13 Data";
    begin
        Poster.InsertRow('B');
        exit(Format(D.Count()));
    end;

    procedure PosterInsert2(): Text
    var
        Poster: Codeunit "P13 Poster";
        D: Record "P13 Data";
    begin
        Poster.InsertRow('D');
        exit(Format(D.Count()));
    end;

    procedure PosterInsertFails(): Text
    var
        Poster: Codeunit "P13 Poster";
    begin
        asserterror Poster.InsertRow('C');
        exit(GetLastErrorCode());
    end;

    procedure Inherent(): Text
    var
        Plain: Codeunit "P13 Plain";
    begin
        Plain.InsertInherent('I');
        exit('ok');
    end;

    procedure EmptyRead(): Text
    var
        O: Record "P13 Other";
    begin
        exit(Format(O.FindFirst()));
    end;

    procedure Perms(): Text
    var
        D: Record "P13 Data";
        O: Record "P13 Other";
        T: Record "P13 Other" temporary;
    begin
        exit(Format(D.ReadPermission()) + '|' + Format(D.WritePermission()) + '|' + Format(O.ReadPermission()) + '|' + Format(T.WritePermission()));
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("p13-permissions-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == "P13 Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn permission_checks() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    // nothing is enforced without the switch, even with effective sets
    s.sys.perm.add_effective("P13 READ");
    assert!(!s.sys.perm.active);
    assert_eq!(call(&mut s, cat, "PosterInsert"), "1");
    al_runtime::permissions::set_enforce(true);
    s.sys.perm.clear_effective();
    s.sys.perm.add_effective("P13 READ");
    assert!(s.sys.perm.active);
    // READ: direct Read only; the posting codeunit's rimd needs an indirect grant in the role
    assert_eq!(call(&mut s, cat, "PlainInsert"), "Permission|Sorry, the current permissions prevented the action. (TableData 50130 P13 Data Insert: )");
    assert_eq!(call(&mut s, cat, "PosterInsertFails"), "Permission");
    // inherent permissions and the empty-table rule need no role
    assert_eq!(call(&mut s, cat, "Inherent"), "ok");
    // the extension of P13 READ grants P13 Other; Data has no write permission; temporary records always
    assert_eq!(call(&mut s, cat, "Perms"), "Yes|No|Yes|Yes");
    s.sys.perm.clear_effective();
    s.sys.perm.add_effective("P13 NOTHING");
    assert_eq!(call(&mut s, cat, "EmptyRead"), "No");
    // WRITE: includes READ, and imd indirect: allowed through the posting codeunit only
    s.sys.perm.clear_effective();
    s.sys.perm.add_effective("P13 WRITE");
    assert_eq!(call(&mut s, cat, "PosterInsert2"), "2");
    assert!(call(&mut s, cat, "PlainInsert").starts_with("Permission|"));
    // a procedure's [InherentPermissions]: upper case directly, lower case with the object's Permissions
    s.sys.perm.clear_effective();
    s.sys.perm.add_effective("P13 NOTHING");
    assert_eq!(call(&mut s, cat, "MethodInherent"), "Permission");
    al_runtime::permissions::set_enforce(false);
    s.rollback();
    // production: a user's Access Control roles (P13 READ): checks run; with SUPER: none
    al_runtime::permissions::set_enforce(true);
    s.sys.perm.clear_effective();
    assert_eq!(call(&mut s, cat, "AddUserRead"), "ok");
    s.user_id = "limited".into();
    s.login_permissions();
    assert!(s.sys.perm.active);
    assert!(call(&mut s, cat, "PlainInsert").starts_with("Permission|"));
    assert_eq!(call(&mut s, cat, "Perms"), "Yes|No|Yes|Yes");
    // the restricted user cannot even read User: add the SUPER row with enforcement off
    let ci = cat.code.iter().position(|c| c.name == "P13 Tests").unwrap() as u32;
    let denied = s.invoke(ProcRef { code: ci, proc: cat.code[ci as usize].find("AddUserSuper")[0] }, vec![], Some(Instance::new(ci)), None, None);
    assert!(matches!(&denied, Err(Flow::Err(e)) if e.msg.contains("(TableData 2000000120 User Read")), "{:?}", denied.err().map(|f| matches!(f, Flow::Err(_))));
    al_runtime::permissions::set_enforce(false);
    s.sys.perm.clear_effective();
    assert_eq!(call(&mut s, cat, "AddUserSuper"), "ok");
    al_runtime::permissions::set_enforce(true);
    s.login_permissions();
    assert!(!s.sys.perm.active);
    al_runtime::permissions::set_enforce(false);
    s.rollback();
    // writes are checked at the database operation, after the triggers; DeleteAll in a loop only per record
    al_runtime::permissions::set_enforce(false);
    s.sys.perm.clear_effective();
    assert_eq!(call(&mut s, cat, "SeedWrites"), "ok");
    al_runtime::permissions::set_enforce(true);
    s.sys.perm.add_effective("P13 NOTHING");
    assert_eq!(call(&mut s, cat, "WritesAfterTriggers"), "Guarded|Permission|Permission|Permission|ok");
    al_runtime::permissions::set_enforce(false);
    s.sys.perm.clear_effective();
    s.rollback();
    // the permission tables (Metadata Permission: P13 WRITE's own entry; Expanded Permission:
    // P13 WRITE = R plus imd indirect, P13 READ = P13 Data and its extension's P13 Other; the
    // tenant set: P13 WRITE minus P13 READ (Read Yes excluded) plus its own Modify on P13 Other,
    // where the excluded P13 READ EXT removes Read)
    assert_eq!(call(&mut s, cat, "PermTables"), "1|Yes,Indirect|2| ,Indirect| ,Yes");
    s.rollback();
}
