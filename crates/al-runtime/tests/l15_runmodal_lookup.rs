//! I15 L3: Page.RunModal opens a page in lookup mode only when its return value is used. BC's AL
//! compiler emits `NavForm.RunModal(isInLookupTrigger, isLookup, ...)` with isLookup = the call is
//! not a statement (Microsoft.Dynamics.Nav.CodeAnalysis PageClassTypeSymbol.
//! RunModalStaticBuiltInMethodTypeSymbol.EmitRunModalArguments: `call.IsStatement ? false : true`),
//! and NavForm.RunModalAsync sets the page's LookupMode from it. `Page.RunModal(0, NoSeriesLine);`
//! (No. Series drill-down) is an editable list with a draft row; `if Page.RunModal(Page::"Item
//! Reference List", ItemReference) = Action::LookupOK` is a lookup.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session, TestCtx};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
table 51510 "L15 Line"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
    keys { key(PK; "No.") { } }
}

codeunit 51511 "L15 Mode Store"
{
    SingleInstance = true;

    var
        Modes: Text;

    procedure Add(DraftRow: Boolean)
    begin
        Modes += Format(DraftRow) + ' ';
    end;

    procedure Get(): Text
    begin
        exit(Modes.Trim());
    end;
}

page 51510 "L15 Lines"
{
    PageType = List;
    SourceTable = "L15 Line";

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field("No."; Rec."No.") { }
            }
        }
    }

}

codeunit 51512 "L15 RunModal Tests"
{
    Subtype = Test;

    [Test]
    [HandlerFunctions('LinesHandler')]
    procedure StatementIsNotLookup()
    var
        Line: Record "L15 Line";
        Store: Codeunit "L15 Mode Store";
    begin
        Line."No." := 'A';
        Line.Insert();
        Page.RunModal(Page::"L15 Lines", Line);
        if Page.RunModal(Page::"L15 Lines", Line) = Action::LookupOK then;
        // the statement opens an editable list (a draft row after the last line), the expression a lookup
        if Store.Get() <> 'Yes No' then
            Error('draft row after the last line: %1', Store.Get());
    end;

    [ModalPageHandler]
    procedure LinesHandler(var Lines: TestPage "L15 Lines")
    var
        Store: Codeunit "L15 Mode Store";
    begin
        Lines.Last();
        Store.Add(Lines.Next());
        Lines.OK().Invoke();
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("l15-runmodal-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

#[test]
fn runmodal_statement_is_not_a_lookup() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let ci = cat.code.iter().position(|c| c.name == "L15 RunModal Tests").unwrap() as u32;
    let pi = cat.code[ci as usize].find("StatementIsNotLookup")[0];
    let mut s = Session::new(&rt, "CRONUS");
    let inst = Instance::new(ci);
    s.test = Some(TestCtx::new(cat, inst.clone(), pi).unwrap());
    match s.invoke(ProcRef { code: ci, proc: pi }, vec![], Some(inst), None, None) {
        Ok(_) => {}
        Err(Flow::Err(e)) => panic!("{}", e.msg),
        Err(_) => panic!("unexpected flow"),
    }
    let missing = s.test.as_ref().unwrap().not_executed();
    assert!(missing.is_empty(), "handlers not executed {missing:?}");
}
