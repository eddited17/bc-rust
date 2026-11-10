//! Error codes of the record errors (GetLastErrorCode), as BC raises them and Library Assert's
//! ExpectedErrorCode / ExpectedErrorCannotFind / ExpectedTestFieldError checks them (a substring
//! match on GetLastErrorCode): DB:RecordNotFound for Get / Modify / Delete / Rename of a record
//! that does not exist, DB:RecordExists for a duplicate Insert, DB:NothingInsideFilter for a Find
//! with nothing in the filter, NCLCSRTS:TableErrorStr for Record.FieldError and FieldRef.FieldError.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 50790 "P11 Code Item"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Amount; Decimal) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}

table 50791 "P11 Code Doc"
{
    fields
    {
        field(1; "Document Type"; Option) { OptionMembers = Quote,Order,"Credit Memo"; }
        field(2; "No."; Code[20]) { Caption = 'Doc No.'; }
    }
    keys
    {
        key(PK; "Document Type", "No.") { Clustered = true; }
    }
}

table 50792 "P11 Del Line"
{
    fields
    {
        field(1; "No."; Integer) { }
        field(2; Partner; Integer) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }

    trigger OnDelete()
    var
        L: Record "P11 Del Line";
    begin
        if Partner <> 0 then
            if L.Get(Partner) then
                L.Delete();
    end;
}

enum 50798 "P11 TType" { value(0; Put) { } value(1; Pick) { } value(2; Move) { } }

table 50798 "P11 Tmpl"
{
    fields { field(1; Name; Code[10]) { } field(2; Type; Enum "P11 TType") { } }
    keys { key(PK; Name) { Clustered = true; } }
}

table 50799 "P11 WName"
{
    fields
    {
        field(1; "Worksheet Template Name"; Code[10]) { }
        field(2; Name; Code[10]) { }
        field(21; "Template Type"; Enum "P11 TType")
        {
            CalcFormula = lookup("P11 Tmpl".Type where(Name = field("Worksheet Template Name")));
            FieldClass = FlowField;
        }
    }
    keys { key(PK; "Worksheet Template Name", Name) { Clustered = true; } }
}

table 50793 "P11 VRest"
{
    fields
    {
        field(1; "No."; Code[10]) { }
        field(2; Amount; Decimal)
        {
            trigger OnValidate()
            begin
                if Amount > 10 then
                    Error('too big');
            end;
        }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}

codeunit 50790 "P11 Code Tests"
{
    procedure TypedCollections(): Text
    var
        Ids: List of [Guid];
        Codes: List of [Code[10]];
        ByCode: Dictionary of [Code[10], Integer];
        G: Guid;
        T: Text;
    begin
        // the arguments convert to the element types at the call
        Ids.Add('5a0b41e9-7a42-4123-d521-2265186cfb31');
        Evaluate(G, '{5A0B41E9-7A42-4123-D521-2265186CFB31}');
        Codes.Add('abc');
        ByCode.Add('xy', 1);
        // a Guid compared with a Text in an expression is a text comparison (StringEqual)
        T := '5a0b41e9-7a42-4123-d521-2265186cfb31';
        exit(Format(Ids.Contains(G)) + '|' + Codes.Get(1) + '|' + Format(ByCode.ContainsKey('XY')) + '|' + Format(G = T));
    end;

    procedure SubstOption(): Text
    var
        D: Record "P11 Code Doc";
    begin
        // an Option field argument formats with its caption, as Format(D."Document Type") does
        D."Document Type" := D."Document Type"::"Credit Memo";
        asserterror Error('%1', D."Document Type");
        exit(StrSubstNo('%1|%2|%3', D."Document Type", Format(D."Document Type"), D."Document Type"::Order) + '|' + GetLastErrorText());
    end;

    procedure ValidateRestore(): Text
    var
        R: Record "P11 VRest";
    begin
        // a Validate that raises restores the record [src:A11b.3]
        R.Amount := 5;
        asserterror R.Validate(Amount, 20);
        exit(Format(R.Amount));
    end;

    procedure ByValueFilters(): Text
    var
        L: Record "P11 Del Line";
    begin
        // a Record passed by value gets the values, not the caller's filters (the callee counts
        // all records): open question [src:A11b.4]
        L."No." := 11; L.Insert();
        L."No." := 12; L.Insert();
        L.SetRange("No.", 11);
        exit(Format(CountAll(L) = 2) + '|' + Format(L.Count()) + '|' + Format(NoOf(L)));
    end;

    local procedure CountAll(L: Record "P11 Del Line"): Integer
    begin
        exit(L.Count());
    end;

    local procedure NoOf(L: Record "P11 Del Line"): Integer
    begin
        exit(L."No.");
    end;

    procedure FlowFieldFilter(): Text
    var
        T: Record "P11 Tmpl";
        N: Record "P11 WName";
    begin
        // a filter on a FlowField restricts the records (BC calculates it per record), as in
        // SCM Pick Worksheet's Whse. Worksheet Name.SetRange("Template Type", Pick)
        T.Name := 'MOVEMENT'; T.Type := T.Type::Move; T.Insert();
        T.Name := 'PICK'; T.Type := T.Type::Pick; T.Insert();
        N."Worksheet Template Name" := 'MOVEMENT'; N.Name := 'DEFAULT'; N.Insert();
        N."Worksheet Template Name" := 'PICK'; N.Name := 'DEFAULT'; N.Insert();
        N.Reset();
        N.SetRange("Template Type", N."Template Type"::Pick);
        if not N.FindFirst() then
            exit('none');
        exit(N."Worksheet Template Name" + '|' + Format(N.Count()) + '|' + Format(N.IsEmpty()));
    end;

    procedure DeleteAllCursor(): Text
    var
        L: Record "P11 Del Line";
    begin
        // DeleteAll(true): a record an earlier record's OnDelete removed (2, by 1) is skipped, not
        // an error: open question [src:A11b.5]
        L."No." := 1; L.Partner := 2; L.Insert();
        L."No." := 2; L.Partner := 0; L.Insert();
        L."No." := 3; L.Partner := 0; L.Insert();
        L.DeleteAll(true);
        exit(Format(L.Count()));
    end;

    procedure KeyTexts(): Text
    var
        Doc: Record "P11 Code Doc";
    begin
        // the key in error texts: field captions, formatted values (an Option by its caption) [src:A10.1]
        asserterror Doc.Get(Doc."Document Type"::"Credit Memo", 'X1');
        exit(GetLastErrorText());
    end;

    procedure RecordCodes(): Text
    var
        Item: Record "P11 Code Item";
        Item2: Record "P11 Code Item";
        R: Text;
    begin
        // nothing in the table, no filters: DB:TableIsEmpty
        asserterror Item.FindFirst();
        R := GetLastErrorCode() + '|' + GetLastErrorText();
        // a filter on a blank value is a filter
        Item.SetRange("No.", '');
        asserterror Item.FindFirst();
        R += '|' + GetLastErrorCode();
        Item.Reset();
        // a positioned Find: the record does not exist
        Item."No." := 'NONE';
        asserterror Item.Find();
        R += '|' + GetLastErrorCode() + '|' + GetLastErrorText();
        asserterror Item.Find('<>');
        R += '|' + GetLastErrorText() + '|';
        asserterror Item.Get('NONE');
        R += GetLastErrorCode();
        Item."No." := 'NONE';
        asserterror Item.Modify();
        R += '|' + GetLastErrorCode();
        asserterror Item.Delete();
        R += '|' + GetLastErrorCode();
        asserterror Item.Rename('NEW');
        R += '|' + GetLastErrorCode();
        Item."No." := 'A';
        Item.Insert();
        Item2."No." := 'A';
        asserterror Item2.Insert();
        R += '|' + GetLastErrorCode();
        Item.SetRange("No.", 'Z');
        asserterror Item.FindFirst();
        R += '|' + GetLastErrorCode();
        asserterror Item.FindSet();
        R += '|' + GetLastErrorCode();
        // the non-throwing forms raise nothing
        R += '|' + Format(Item.FindFirst()) + Format(Item2.Insert());
        exit(R);
    end;

    procedure FieldErrors(): Text
    var
        Item: Record "P11 Code Item";
        RecRef: RecordRef;
        FldRef: FieldRef;
        R: Text;
    begin
        Item."No." := 'A';
        Item.Amount := 5;
        asserterror Item.FieldError(Amount);
        R := GetLastErrorCode() + '|' + GetLastErrorText();
        RecRef.GetTable(Item);
        FldRef := RecRef.Field(2);
        asserterror FldRef.FieldError();
        R += '|' + GetLastErrorCode() + '|' + GetLastErrorText();
        asserterror FldRef.FieldError('is wrong');
        R += '|' + GetLastErrorText();
        // an empty code value: "You must specify" [src:A10.2]
        Item."No." := '';
        asserterror Item.FieldError("No.");
        R += '|' + GetLastErrorText();
        exit(R);
    end;
}
"#;

fn catalog() -> &'static Catalog {
    // one fixed directory, the file replaced atomically: repeated and concurrent runs leave one small
    // directory behind instead of one per run (nothing is deleted)
    let dir = std::env::temp_dir().join("p11-codes-test");
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = dir.join(format!("App.al.{}.{:?}.part", std::process::id(), std::thread::current().id()));
    std::fs::write(&tmp, APP).unwrap();
    std::fs::rename(&tmp, dir.join("App.al")).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == "P11 Code Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn record_error_codes() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    assert_eq!(
        call(&mut s, cat, "RecordCodes"),
        "DB:TableIsEmpty|The P11 Code Item table is empty.|DB:NothingInsideFilter\
         |DB:RecordNotFound|The P11 Code Item does not exist. \n\nIdentification fields and values:\n\nNo.='NONE'\
         |The P11 Code Item does not exist. <>\n\nIdentification fields and values:\n\nNo.='NONE'\
         |DB:RecordNotFound|DB:RecordNotFound|DB:RecordNotFound|DB:RecordNotFound|DB:RecordExists|DB:NothingInsideFilter|DB:NothingInsideFilter|NoNo"
    );
    // FieldRef.FieldError raises what Record.FieldError raises, with BC's code
    let got = call(&mut s, cat, "FieldErrors");
    let parts: Vec<&str> = got.split('|').collect();
    assert_eq!(parts.len(), 6, "{got}");
    assert_eq!(parts[0], "NCLCSRTS:TableErrorStr");
    assert_eq!(parts[1], "Amount must not be 5 in P11 Code Item No.='A'.");
    assert_eq!(parts[2], "NCLCSRTS:TableErrorStr");
    assert_eq!(parts[3], parts[1]);
    // FieldError with a text: '<field> <text> in <table> <key>.', no colon after the table caption [src:A10.2]
    assert_eq!(parts[4], "Amount is wrong in P11 Code Item No.='A'.");
    assert_eq!(parts[5], "You must specify No. in P11 Code Item No.=''.");
    assert_eq!(
        call(&mut s, cat, "KeyTexts"),
        "The P11 Code Doc does not exist. Identification fields and values: Document Type='Credit Memo',Doc No.='X1'"
    );
    assert_eq!(call(&mut s, cat, "DeleteAllCursor"), "0");
    assert_eq!(call(&mut s, cat, "FlowFieldFilter"), "PICK|1|No");
    assert_eq!(call(&mut s, cat, "ByValueFilters"), "Yes|1|12");
    assert_eq!(call(&mut s, cat, "ValidateRestore"), "5");
    assert_eq!(call(&mut s, cat, "TypedCollections"), "Yes|ABC|Yes|No");
    assert_eq!(call(&mut s, cat, "SubstOption"), "Credit Memo|Credit Memo|Order|Credit Memo");
    s.rollback();
}
