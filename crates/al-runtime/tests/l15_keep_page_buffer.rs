//! I15 L7: a TestPage field change that leaves the record unchanged writes nothing and keeps the
//! page's record as it was: the page does not re-read it from the database (BC keeps the form's
//! buffer; only a save re-reads). Incom. Doc. Attach. FactBox ::
//! MainAttachmentEnabledAfterPostingGeneralJournal: the test links an incoming document to the
//! journal line the page shows (in the database), then sets "Account Type" to the value it already
//! has; the page's record must keep its own "Incoming Document Entry No." (0), which the FactBox
//! reads, until a real change is saved.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 51540 "L15 Jnl Line"
{
    fields
    {
        field(1; "Line No."; Integer) { }
        field(2; "Account Type"; Option) { OptionMembers = "G/L Account",Customer; }
        field(3; "Account No."; Code[20]) { }
        field(4; "Incoming Doc"; Integer) { }
    }
    keys { key(PK; "Line No.") { } }
}

page 51540 "L15 Journal"
{
    PageType = Worksheet;
    SourceTable = "L15 Jnl Line";

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field("Account Type"; Rec."Account Type") { }
                field("Account No."; Rec."Account No.") { }
                field("Incoming Doc"; Rec."Incoming Doc") { }
            }
        }
    }
}

codeunit 51541 "L15 Journal Tests"
{
    procedure UnchangedThenChanged(): Text
    var
        Line: Record "L15 Jnl Line";
        Other: Record "L15 Jnl Line";
        Jnl: TestPage "L15 Journal";
        Seen: Text;
    begin
        Line."Line No." := 10000;
        Line.Insert();
        Jnl.OpenEdit();
        Jnl.GoToRecord(Line);
        // the line gets an incoming document through another variable (as Import Attachment does)
        Other.Get(10000);
        Other."Incoming Doc" := 2;
        Other.Modify();
        // the same value: nothing to save, the page keeps its record
        Jnl."Account Type".SetValue(Line."Account Type"::"G/L Account");
        Seen := Jnl."Incoming Doc".Value;
        // a real change: the page saves its record
        Jnl."Account No.".SetValue('1000');
        Seen += ' ' + Jnl."Incoming Doc".Value;
        Other.Get(10000);
        exit(Seen + ' ' + Format(Other."Incoming Doc") + ' ' + Other."Account No.");
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("l15-keep-buffer-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

#[test]
fn unchanged_field_keeps_page_record() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let ci = cat.code.iter().position(|c| c.name == "L15 Journal Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find("UnchangedThenChanged")[0];
    let got = match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{} {:?}", e.msg, e.stack),
        Err(_) => panic!("unexpected flow"),
    };
    // the page shows 0 after the unchanged value and after its save, which writes its record
    assert_eq!(got, "0 0 0 1000");
}
