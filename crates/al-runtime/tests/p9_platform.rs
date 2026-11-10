//! Platform semantics closed by P9: RecordRef/Variant handling the workflow code relies on
//! (Variant := RecordRef replacing a Variant's record), FieldRefs across RecordRef.Open, field 0
//! (timestamp), table relations to a table without a field, and CRM test table connections.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
// unopened RecordRefs are placeholder Integer records, as with the platform tables loaded
table 2000000026 Integer
{
    fields { field(1; Number; Integer) { } }
    keys { key(PK; Number) { } }
}

table 2000000007 Date
{
    fields
    {
        field(1; "Period Type"; Option) { OptionMembers = Date,Week,Month,Quarter,Year; }
        field(2; "Period Start"; Date) { }
        field(3; "Period End"; Date) { }
        field(4; "Period No."; Integer) { }
        field(5; "Period Name"; Text[30]) { }
    }
    keys { key(PK; "Period Type", "Period Start") { } }
}

table 50900 "P9 Doc"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Description; Text[50]) { }
    }
    keys { key(PK; "No.") { } }
}

table 50901 "P9 Approval"
{
    fields
    {
        field(1; "Entry No."; Integer) { }
        field(2; "Record ID to Approve"; RecordId) { }
    }
    keys { key(PK; "Entry No.") { } }
}

table 50902 "P9 Employee"
{
    fields
    {
        field(1; "User ID"; Code[50]) { }
        field(2; "Location Code"; Code[10]) { }
    }
    keys { key(PK; "User ID", "Location Code") { } }
}

table 50903 "P9 Activity"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Location Code"; Code[10]) { }
        field(3; "Assigned User ID"; Code[50]) { TableRelation = "P9 Employee" where("Location Code" = field("Location Code")); }
    }
    keys { key(PK; "No.") { } }
}

table 50904 "P9 Line"
{
    fields
    {
        field(1; "Line No."; Integer) { }
        field(2; "Bal. Type"; Option)
        {
            OptionMembers = "G/L Account","Bank Account","Mat. & Requisition";
            trigger OnValidate()
            begin
                Validate("Bal. No.", '');
            end;
        }
        field(3; "Bal. No."; Code[20])
        {
            trigger OnValidate()
            begin
                SeenXType := xRec."Bal. Type";
            end;
        }
        field(4; SeenXType; Integer) { }
        field(5; Kind; Option) { OptionMembers = " ",A; }
    }
    keys { key(PK; "Line No.") { } }
}

table 50905 "P9 CRM Account"
{
    TableType = CRM;
    fields { field(1; AccountId; Guid) { } field(2; Name; Text[100]) { } }
    keys { key(PK; AccountId) { } }
}

query 50901 "P9 Lines"
{
    elements
    {
        dataitem(Line; "P9 Line")
        {
            column(Line_No; "Line No.") { }
            column(Bal_Type; "Bal. Type") { }
        }
    }
}

query 50900 "P9 Emp Docs"
{
    elements
    {
        dataitem(Emp; "P9 Employee")
        {
            column(User_ID; "User ID") { }
            dataitem(Doc; "P9 Doc")
            {
                DataItemLink = "No." = Emp."User ID";
                column(Description; Description) { }
            }
        }
    }
}

table 50906 "P9 Mod"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Val; Text[20]) { }
        field(3; SeenX; Text[20]) { }
    }
    keys { key(PK; "No.") { } }
    trigger OnModify()
    begin
        SeenX := xRec.Val;
    end;
}

table 2000000004 "Permission Set"
{
    fields { field(1; "Role ID"; Code[20]) { } field(2; Name; Text[30]) { } }
    keys { key(Key1; "Role ID") { } }
}

table 2000000250 "Metadata Permission Set"
{
    fields { field(1; "App ID"; Guid) { } field(2; "Role ID"; Code[30]) { } field(3; Name; Text[30]) { } field(4; Assignable; Boolean) { } }
    keys { key(Key1; "App ID", "Role ID") { } }
}

permissionset 50900 "P9 PERMS"
{
    Assignable = false;
    Caption = 'P9 test permissions';
}

table 50907 "P9 Blob"
{
    fields { field(1; "No."; Code[20]) { } field(2; Data; Blob) { } }
    keys { key(PK; "No.") { } }
}

report 50900 "P9 Rep"
{
    ProcessingOnly = false;
    dataset
    {
        dataitem(Doc; "P9 Doc")
        {
            column(No; "No.") { }
        }
    }
}

report 50901 "P9 Init Rep"
{
    ProcessingOnly = true;
    dataset
    {
        dataitem(Doc; "P9 Doc") { }
    }
    trigger OnInitReport()
    begin
        Direction := Direction::Backward;
        Inits += 1;
    end;
    trigger OnPreReport()
    begin
        Result := Format(Direction, 0, 2) + ':' + Format(Inits);
    end;
    var
        Direction: Option Forward,Backward;
        Inits: Integer;
        Result: Text;
    procedure InitializeRequest(NewDirection: Option Forward,Backward)
    begin
        Direction := NewDirection;
    end;
    procedure GetResult(): Text
    begin
        exit(Result);
    end;
}

codeunit 50900 "P9 Tests"
{
    // CompressArray moves the non-empty strings to the front and returns their number
    procedure CompressArrayTest(): Text
    var
        A: array[5] of Text[10];
        N: Integer;
    begin
        A[2] := 'b';
        A[4] := 'd';
        A[5] := ' ';
        N := CompressArray(A);
        exit(Format(N) + '|' + A[1] + A[2] + A[3] + '|' + A[4] + A[5] + '|');
    end;
    // OnInitReport runs when the report instance is created: before InitializeRequest, once per
    // instance; Clear gives a new instance
    procedure InitReportOnce(): Text
    var
        R: Report "P9 Init Rep";
        T: Text;
    begin
        R.InitializeRequest(0);
        R.UseRequestPage(false);
        R.RunModal();
        T := R.GetResult();
        R.RunModal();
        T += '|' + R.GetResult();
        Clear(R);
        R.UseRequestPage(false);
        R.RunModal();
        exit(T + '|' + R.GetResult());
    end;
    // the Date table's first Week starts 0001-01-08 (BC); Find('=<') from 0D finds no period
    procedure DateTableBounds(): Text
    var
        D: Record Date;
        T: Text;
    begin
        D.SetRange("Period Type", D."Period Type"::Week);
        D.FindFirst();
        T := Format(D."Period Start", 0, 9);
        D.SetRange("Period Type", D."Period Type"::Year);
        D.FindLast();
        T += '|' + Format(D."Period Start", 0, 9);
        D.SetRange("Period Type", D."Period Type"::Week);
        D."Period Type" := D."Period Type"::Week;
        D."Period Start" := 0D;
        exit(T + '|' + Format(D.Find('=<')));
    end;
    // Arr::Member on an array of options is the element type's member
    procedure ArrayOptionScope(): Text
    var
        A: array[3] of Option "None",Released,FirmPlanned;
        T: Text;
    begin
        A[2] := A::FirmPlanned;
        case A[2] of
            A::Released:
                T := 'R';
            A::FirmPlanned:
                T := 'F';
        end;
        exit(Format(A::Released, 0, 2) + '|' + Format(A[2], 0, 2) + '|' + T);
    end;
    // A[2] := A[1] copies the record into element 2; the elements stay two records
    procedure ElemRecordAssign(): Text
    var
        A: array[3] of Record "P9 Doc";
        R: Record "P9 Doc";
    begin
        A[1]."No." := 'X';
        A[2] := A[1];
        A[2]."No." := 'Y';
        R."No." := 'Z';
        A[3] := R;
        R."No." := 'W';
        exit(A[1]."No." + '|' + A[2]."No." + '|' + A[3]."No.");
    end;
    // the elements of an array of temporary records share one temporary table
    procedure TempRecordArray(): Text
    var
        P: array[2] of Record "P9 Doc" temporary;
    begin
        P[1]."No." := 'T1';
        P[1].Insert();
        P[2]."No." := 'T2';
        P[2].Insert();
        exit(Format(P[1].Count()) + '|' + Format(P[2].Count()));
    end;
    // SaveAs(ReportFormat::Pdf): the report runs and writes a PDF; an empty dataset is BC's error
    procedure ReportPdf(): Text
    var
        Doc: Record "P9 Doc";
        B: Record "P9 Blob";
        OutS: OutStream;
        InS: InStream;
        Head: Text;
        Res: Text;
    begin
        Doc."No." := 'RPT1';
        Doc.Insert();
        B."No." := 'B1';
        B.Data.CreateOutStream(OutS);
        Report.SaveAs(Report::"P9 Rep", '', ReportFormat::Pdf, OutS);
        B.Data.CreateInStream(InS);
        InS.ReadText(Head, 5);
        Res := Head;
        Doc.DeleteAll();
        Clear(B);
        B.Data.CreateOutStream(OutS);
        asserterror Report.SaveAs(Report::"P9 Rep", '', ReportFormat::Pdf, OutS);
        exit(Res + '|' + GetLastErrorText());
    end;
    // BC writes a blank option value as '' in GetFilter; SetFilter takes it back
    procedure OptionFilterBlank(): Text
    var
        Line: Record "P9 Line";
        Line2: Record "P9 Line";
        F: Text;
    begin
        Line.SetRange(Kind, Line.Kind::" ");
        F := Line.GetFilter(Kind);
        Line2.SetFilter(Kind, F);
        exit(F + '|' + Line2.GetFilter(Kind));
    end;
    // BC quotes option captions with filter characters in GetFilter/GetFilters; SetFilter takes them back
    procedure OptionFilterQuoted(): Text
    var
        Line: Record "P9 Line";
        Line2: Record "P9 Line";
        F: Text;
    begin
        Line.SetRange("Bal. Type", Line."Bal. Type"::"Mat. & Requisition");
        F := Line.GetFilter("Bal. Type");
        Line2.SetFilter("Bal. Type", F);
        exit(F + '|' + Line2.GetFilter("Bal. Type") + '|' + Line.GetFilters());
    end;
    // the platform lists the apps' permission sets (PermissionsMock.Assign does PermissionSet.Get)
    procedure PermissionSets(): Text
    var
        PermissionSet: Record "Permission Set";
        MetadataPermissionSet: Record "Metadata Permission Set";
    begin
        PermissionSet.Get('P9 PERMS');
        MetadataPermissionSet.SetRange("Role ID", 'P9 PERMS');
        MetadataPermissionSet.FindFirst();
        exit(PermissionSet.Name + '|' + Format(MetadataPermissionSet.Assignable));
    end;
    // BC: Modify(true) from code gives OnModify xRec = the record as it is (OldRecord.ALAssign)
    procedure ModifyXRec(): Text
    var
        M: Record "P9 Mod";
    begin
        M."No." := 'M1';
        M.Val := 'A';
        M.Insert();
        M.Val := 'B';
        M.Modify(true);
        exit(M.SeenX);
    end;
    // GetFilter / GetFilters write Option values as captions (BC)
    procedure OptionGetFilter(): Text
    var
        Line: Record "P9 Line";
        RecRef: RecordRef;
    begin
        Line.SetRange("Bal. Type", Line."Bal. Type"::"Bank Account");
        RecRef.GetTable(Line);
        exit(Line.GetFilter("Bal. Type") + '|' + Line.GetFilters() + '|' + RecRef.Field(2).GetFilter());
    end;
    // BC NavSqlWhereClauseVisitor: a filter on a joined data item's column tests an unmatched
    // row's column as its default value (ISNULL(column, default))
    procedure QueryOuterDefault(): Text
    var
        Emp: Record "P9 Employee";
        Q1: Query "P9 Emp Docs";
        Q2: Query "P9 Emp Docs";
        Res: Text;
    begin
        Emp."User ID" := 'U1';
        Emp.Insert();
        Q1.SetRange(Description, '');
        Q1.Open();
        Res := Format(Q1.Read());
        Q2.SetRange(Description, 'X');
        Q2.Open();
        exit(Res + '|' + Format(Q2.Read()));
    end;
    // Query.Column::Member is the option value of the column's source field
    procedure QueryMember(): Text
    var
        Line: Record "P9 Line";
        Q: Query "P9 Lines";
    begin
        Line."Line No." := 7;
        Line."Bal. Type" := Line."Bal. Type"::"Bank Account";
        Line.Insert();
        Q.SetRange(Bal_Type, Q.Bal_Type::"Bank Account");
        Q.Open();
        exit(Format(Q.Read()) + '|' + Format(Q.Line_No));
    end;
    // BC TableConnectionManager: a name registered twice
    procedure ConnErrors(): Text
    var
        Res: Text;
    begin
        RegisterTableConnection(TableConnectionType::CRM, 'P9', '@@test@@');
        asserterror RegisterTableConnection(TableConnectionType::CRM, 'P9', '@@test@@');
        Res := GetLastErrorText();
        UnregisterTableConnection(TableConnectionType::CRM, 'P9');
        exit(Res);
    end;

    // BC NavRecordRef.ALClose: only the closed variable is detached; a RecordRef that shares its
    // record (RecRef2 := RecRef1) and FieldRefs taken from it keep working
    procedure CloseShared(): Text
    var
        Doc: Record "P9 Doc";
        RecRef1: RecordRef;
        RecRef2: RecordRef;
        FldRef: FieldRef;
    begin
        Doc."No." := 'C1';
        Doc.Insert();
        RecRef1.Open(Database::"P9 Doc");
        RecRef1.FindFirst();
        RecRef2 := RecRef1;
        FldRef := RecRef2.Field(1);
        RecRef1.Close();
        exit(Format(FldRef.Value) + '|' + Format(RecRef2.Number) + '|' + Format(RecRef1.Number));
    end;

    // BC NavRecord.ValidateAsync: a Validate from the record's own trigger keeps the outer xRec
    procedure NestedXRec(): Text
    var
        Line: Record "P9 Line";
    begin
        Line."Bal. Type" := Line."Bal. Type"::"G/L Account";
        Line."Bal. No." := 'GL1';
        Line.Validate("Bal. Type", Line."Bal. Type"::"Bank Account");
        exit(Format(Line.SeenXType));
    end;

    // Workflow Response Handling.ReleaseDocument: an Approval Entry in the Variant is replaced by
    // the RecordRef of the record it approves, then the procedure recurses on that Variant
    procedure VariantRecRef(): Text
    var
        Doc: Record "P9 Doc";
        Appr: Record "P9 Approval";
        V: Variant;
    begin
        Doc."No." := 'D1';
        Doc.Insert();
        Appr."Entry No." := 1;
        Appr."Record ID to Approve" := Doc.RecordId;
        Appr.Insert();
        V := Appr;
        exit(Release(V, 0));
    end;

    local procedure Release(var V: Variant; Depth: Integer): Text
    var
        Appr: Record "P9 Approval";
        RecRef: RecordRef;
        TargetRecRef: RecordRef;
    begin
        if Depth > 3 then
            exit('recursion');
        RecRef.GetTable(V);
        case RecRef.Number of
            Database::"P9 Approval":
                begin
                    Appr := V;
                    TargetRecRef.Get(Appr."Record ID to Approve");
                    V := TargetRecRef;
                    exit(Release(V, Depth + 1));
                end;
            Database::"P9 Doc":
                exit(Format(RecRef.Number) + ':' + Format(RecRef.Field(1).Value));
        end;
        exit('other ' + Format(RecRef.Number));
    end;

    procedure Field0(): Text
    var
        Doc: Record "P9 Doc";
        RecRef: RecordRef;
    begin
        Doc."No." := 'D2';
        Doc.Insert();
        RecRef.GetTable(Doc);
        exit(Format(RecRef.FieldExist(0)) + '|' + Format(Format(RecRef.Field(0).Value) <> ''));
    end;

    procedure RelationFirstPk(): Text
    var
        Emp: Record "P9 Employee";
        Act: Record "P9 Activity";
    begin
        Emp."User ID" := 'ADMIN';
        Emp."Location Code" := 'WHITE';
        Emp.Insert();
        Act."Location Code" := 'WHITE';
        Act.Validate("Assigned User ID", 'ADMIN');
        Act."Location Code" := 'BLUE';
        asserterror Act.Validate("Assigned User ID", 'ADMIN');
        exit(Act."Assigned User ID" + '|' + Format(GetLastErrorText() <> ''));
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("p9-platform-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == "P9 Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn platform_semantics() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    assert_eq!(call(&mut s, cat, "VariantRecRef"), "50900:D1");
    assert_eq!(call(&mut s, cat, "TempRecordArray"), "2|2");
    assert_eq!(call(&mut s, cat, "ElemRecordAssign"), "X|Y|Z");
    assert_eq!(call(&mut s, cat, "CompressArrayTest"), "3|bd ||");
    assert_eq!(call(&mut s, cat, "InitReportOnce"), "0:1|0:1|1:1");
    assert_eq!(call(&mut s, cat, "DateTableBounds"), "0001-01-08|9999-01-01|No");
    assert_eq!(call(&mut s, cat, "ArrayOptionScope"), "1|2|F");
    assert_eq!(call(&mut s, cat, "Field0"), "Yes|Yes");
    assert_eq!(call(&mut s, cat, "RelationFirstPk"), "ADMIN|Yes");
    assert_eq!(call(&mut s, cat, "CloseShared"), "C1|50900|0");
    assert_eq!(call(&mut s, cat, "NestedXRec"), "0");
    assert_eq!(call(&mut s, cat, "ModifyXRec"), "B");
    assert_eq!(call(&mut s, cat, "ReportPdf"), "%PDF-|The report couldn\u{2019}t be generated, because it was empty. Adjust your filters and try again.");
    assert_eq!(call(&mut s, cat, "OptionFilterBlank"), "''|''");
    assert_eq!(call(&mut s, cat, "OptionFilterQuoted"), "'Mat. & Requisition'|'Mat. & Requisition'|Bal. Type: 'Mat. & Requisition'");
    s.init_platform_data().unwrap_or_else(|_| panic!("platform data"));
    assert_eq!(call(&mut s, cat, "PermissionSets"), "P9 test permissions|No");
    assert_eq!(call(&mut s, cat, "OptionGetFilter"), "Bank Account|Bal. Type: Bank Account|Bank Account");
    assert_eq!(call(&mut s, cat, "QueryOuterDefault"), "Yes|No");
    assert_eq!(call(&mut s, cat, "QueryMember"), "Yes|7");
    assert_eq!(
        call(&mut s, cat, "ConnErrors"),
        "A table connection P9 of type CRM is registered already."
    );
    s.rollback();
}
