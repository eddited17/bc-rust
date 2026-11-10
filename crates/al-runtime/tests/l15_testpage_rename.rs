//! I15 L2: a page variable's OnValidate that changes a key field of the page's stored record renames
//! the record (BC NavForm.SaveRecordAsync: RecordId <> OldRecord.RecordId -> RenameAsync). The
//! Customer Report Selections pattern (Test Custom Reports :: MultipleReportSelectionReportIdFirst
//! ValidateOnPage): a DelayedInsert list whose Report ID's OnValidate calls CurrPage.SaveRecord (the
//! new line is inserted with the default usage), then the page variable Usage2 sets Rec.Usage, part
//! of the key. The line must end up under the new usage, not stay under the old one.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 51500 "L15 Selection"
{
    fields
    {
        field(1; Usage; Option) { OptionMembers = Quote,Invoice,CrMemo; }
        field(2; Sequence; Integer) { }
        field(3; ReportId; Integer) { }
    }
    keys { key(PK; Usage, Sequence) { } }

    trigger OnInsert()
    var
        Last: Record "L15 Selection";
    begin
        if Last.FindLast() then
            Sequence := Last.Sequence + 1
        else
            Sequence := 1;
    end;
}

page 51500 "L15 Selections"
{
    PageType = List;
    SourceTable = "L15 Selection";
    DelayedInsert = true;

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field(Usage2; Usage2)
                {
                    trigger OnValidate()
                    begin
                        case Usage2 of
                            Usage2::Quote:
                                Rec.Usage := Rec.Usage::Quote;
                            Usage2::Invoice:
                                Rec.Usage := Rec.Usage::Invoice;
                            Usage2::CrMemo:
                                Rec.Usage := Rec.Usage::CrMemo;
                        end;
                    end;
                }
                field(Usage3; Usage2)
                {
                    trigger OnValidate()
                    begin
                        // assigns the key, then refreshes without saving: BC drops the change
                        Rec.Usage := Rec.Usage::CrMemo;
                        CurrPage.Update(false);
                    end;
                }
                field(ReportId; Rec.ReportId)
                {
                    trigger OnValidate()
                    begin
                        CurrPage.SaveRecord();
                    end;
                }
            }
        }
    }

    trigger OnAfterGetRecord()
    begin
        Usage2 := Rec.Usage;
    end;

    trigger OnNewRecord(BelowxRec: Boolean)
    begin
        Rec.Usage := Rec.Usage::Quote;
        Usage2 := Usage2::Quote;
    end;

    var
        Usage2: Option Quote,Invoice,CrMemo;
}

codeunit 51501 "L15 Rename Tests"
{
    procedure ReportIdFirst(): Text
    var
        TP: TestPage "L15 Selections";
        Sel: Record "L15 Selection";
        Out: Text;
    begin
        TP.OpenEdit();
        TP.New();
        TP.ReportId.SetValue(7);
        TP.Usage2.SetValue('Invoice');
        TP.New();
        TP.ReportId.SetValue(8);
        TP.Usage2.SetValue('CrMemo');
        TP.Close();
        if Sel.FindSet() then
            repeat
                Out += Format(Sel.Usage) + '/' + Format(Sel.Sequence) + '/' + Format(Sel.ReportId) + ' ';
            until Sel.Next() = 0;
        exit(Out.Trim());
    end;

    procedure KeyThenUpdateFalse(): Text
    var
        TP: TestPage "L15 Selections";
        Sel: Record "L15 Selection";
        Out: Text;
    begin
        TP.OpenEdit();
        TP.New();
        TP.ReportId.SetValue(9);
        TP.Usage3.SetValue('CrMemo');
        TP.Close();
        if Sel.FindSet() then
            repeat
                Out += Format(Sel.Usage) + '/' + Format(Sel.Sequence) + '/' + Format(Sel.ReportId) + ' ';
            until Sel.Next() = 0;
        exit(Out.Trim());
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("l15-tp-rename-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

#[test]
fn page_variable_trigger_renames_the_stored_line() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let ci = cat.code.iter().position(|c| c.name == "L15 Rename Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find("ReportIdFirst")[0];
    let got = match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{} {:?}", e.msg, e.stack),
        Err(_) => panic!("unexpected flow"),
    };
    // each line under the usage the page variable set (renamed), none left under Quote
    assert_eq!(got, "Invoice/1/7 CrMemo/2/8");
}

/// The same key assignment followed by CurrPage.Update(false) is not saved: BC re-reads the record
/// (NavForm.UpdateAsync with saveRecord = false), so the line stays under its stored key.
#[test]
fn update_false_drops_the_key_change() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    let ci = cat.code.iter().position(|c| c.name == "L15 Rename Tests").unwrap() as u32;
    let p = cat.code[ci as usize].find("KeyThenUpdateFalse")[0];
    let got = match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{} {:?}", e.msg, e.stack),
        Err(_) => panic!("unexpected flow"),
    };
    assert_eq!(got, "Quote/1/9");
}
