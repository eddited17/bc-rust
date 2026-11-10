//! I15 L5: saved request page values (SaveValues) of reports, as BC 29.1 handles them ([src:TODO],
//! stream O's answer (c)):
//! - saved when a request page that was shown closes with OK (NavForm.CloseFormAsync ->
//!   StoreSaveValues), in Object Options;
//! - restored when the report object is created (NCLMetaReport.CreateObjectInstance ->
//!   RequestPageBase.InitializeRequestPageWithCustomValues), for Report.Run/RunModal with or
//!   without the request page and for a report variable's first use; Report.Execute/SaveAs/Print/
//!   RunRequestPage restore only when their Parameters are not empty.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session, TestCtx};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP: &str = r#"
// the platform table BC keeps saved report options in (platform/system, not loaded by this test)
table 2000000196 "Object Options"
{
    DataPerCompany = false;
    fields
    {
        field(1; "Parameter Name"; Text[50]) { }
        field(2; "Object ID"; Integer) { }
        field(3; "Object Type"; Option) { OptionMembers = ,,,"Report",,,"XMLport",,"Page",,,,,,,,,,,; }
        field(4; "Company Name"; Text[30]) { }
        field(5; "User Name"; Code[50]) { }
        field(6; "Option Data"; BLOB) { }
    }
    keys { key(PK; "Parameter Name", "Object ID", "Object Type", "User Name", "Company Name") { } }
}

report 51520 "L15 Opts"
{
    ProcessingOnly = true;

    requestpage
    {
        SaveValues = true;

        layout
        {
            area(Content)
            {
                field(Qty; Qty) { }
            }
        }
    }

    var
        Qty: Integer;
}

codeunit 51521 "L15 Opts Store"
{
    SingleInstance = true;

    var
        Mode: Text;
        Seen: Text;

    procedure SetMode(NewMode: Text)
    begin
        Mode := NewMode;
    end;

    procedure GetMode(): Text
    begin
        exit(Mode);
    end;

    procedure Add(Value: Text)
    begin
        Seen += Value + ' ';
    end;

    procedure GetSeen(): Text
    begin
        exit(Seen.Trim());
    end;
}

codeunit 51522 "L15 Opts Tests"
{
    Subtype = Test;

    [Test]
    [HandlerFunctions('OptsHandler')]
    procedure SavedValuesPerCallSite()
    var
        Store: Codeunit "L15 Opts Store";
    begin
        // shown and closed with OK: saved
        Store.SetMode('set');
        Report.Run(Report::"L15 Opts");
        // RunRequestPage with empty parameters: not restored
        Store.SetMode('read');
        Report.RunRequestPage(Report::"L15 Opts", '');
        // Report.Run: restored when the report object is created
        Report.Run(Report::"L15 Opts");
        if Store.GetSeen() <> '0 7' then
            Error('request page values seen: %1', Store.GetSeen());
    end;

    [RequestPageHandler]
    procedure OptsHandler(var Opts: TestRequestPage "L15 Opts")
    var
        Store: Codeunit "L15 Opts Store";
    begin
        if Store.GetMode() = 'set' then begin
            Opts.Qty.SetValue(7);
            Opts.OK().Invoke();
        end else begin
            Store.Add(Opts.Qty.Value);
            Opts.Cancel().Invoke();
        end;
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("l15-saved-values-test-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

#[test]
fn report_saved_values_per_call_site() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let ci = cat.code.iter().position(|c| c.name == "L15 Opts Tests").unwrap() as u32;
    let pi = cat.code[ci as usize].find("SavedValuesPerCallSite")[0];
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
