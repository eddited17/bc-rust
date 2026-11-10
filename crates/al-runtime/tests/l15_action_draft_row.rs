//! I15 L6: an action invoked on the page's untouched draft row does not write the draft over a
//! stored record with the same key. ERM RS Wizard & Worksheet :: VerifyPromotedTablesFiltering:
//! after the count loop the Config. Worksheet sits on its draft row (Line No. 0, the key of the
//! first stored line); the second "Promoted Only" action abandoned the draft and then modified the
//! record, which overwrote line 0 with the draft's blank values.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 51530 "L15 Config Line"
{
    fields
    {
        field(1; "Line No."; Integer) { }
        field(2; "Table ID"; Integer) { }
        field(3; Promoted; Boolean) { }
    }
    keys { key(PK; "Line No.") { } }
}

page 51530 "L15 Worksheet"
{
    PageType = List;
    SourceTable = "L15 Config Line";
    DelayedInsert = true;

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field("Table ID"; Rec."Table ID") { BlankZero = true; }
                field(Promoted; Rec.Promoted) { }
            }
        }
    }

    actions
    {
        area(Processing)
        {
            action(PromotedOnly)
            {
                trigger OnAction()
                begin
                    if Rec.GetFilter(Promoted) = '' then
                        Rec.SetRange(Promoted, true)
                    else
                        Rec.SetRange(Promoted);
                end;
            }
        }
    }
}

codeunit 51531 "L15 Worksheet Tests"
{
    procedure ToggleTwice(): Text
    var
        Line: Record "L15 Config Line";
        WS: TestPage "L15 Worksheet";
        First: Integer;
        Second: Integer;
    begin
        Line."Line No." := 0;
        Line."Table ID" := 18;
        Line.Promoted := true;
        Line.Insert();
        Line."Line No." := 10000;
        Line."Table ID" := 27;
        Line.Promoted := false;
        Line.Insert();
        WS.OpenEdit();
        WS.PromotedOnly.Invoke();
        First := CountLines(WS);
        WS.PromotedOnly.Invoke();
        Second := CountLines(WS);
        Line.Get(0);
        exit(Format(First) + ' ' + Format(Second) + ' ' + Format(Line."Table ID"));
    end;

    local procedure CountLines(WS: TestPage "L15 Worksheet") Counter: Integer
    begin
        WS.First();
        repeat
            if WS."Table ID".Value <> '' then
                Counter += 1;
        until not WS.Next();
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("l15-draft-action-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

#[test]
fn action_on_draft_row_keeps_stored_record() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let ci = cat.code.iter().position(|c| c.name == "L15 Worksheet Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find("ToggleTwice")[0];
    let got = match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{} {:?}", e.msg, e.stack),
        Err(_) => panic!("unexpected flow"),
    };
    // promoted only: 1 line; all again: 2 lines; line 0 keeps its Table ID
    assert_eq!(got, "1 2 18");
}
