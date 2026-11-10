//! Objects of a kind that share a name (al_model::names): code in each app runs its own app's
//! object, in both engines. Two unrelated apps declare codeunit "Search" and enum "Search Style"
//! (as Create Product Information With Copilot and Sales Lines Suggestions do in the
//! standard-prod app set); a base app has a table its dependents reach while a Removed table of
//! the same name sits in another app (Dimension Set Entry 480 vs PowerBIReports' 36950).
//!
//! The engine is chosen once per process from BC_ENGINE, so the test runs itself in one child
//! process per engine.

use al_model::{Catalog, ProcRef};
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const TEST: &str = "each_app_runs_its_own_object_in_both_engines";
const CHILD: &str = "SHARED_NAMES_CHILD";

const BASE: &str = "00000000-0000-0000-0000-00000000b001";

/// (folder, app id, depends on BASE, source)
const APPS: &[(&str, &str, bool, &str)] = &[
    (
        "base",
        BASE,
        false,
        r#"namespace Microsoft.Finance.Dimension;
table 480 "Dimension Set Entry"
{
    fields
    {
        field(1; "Dimension Set ID"; Integer) { }
        field(2; "Dimension Code"; Code[20]) { }
    }
    keys { key(PK; "Dimension Set ID", "Dimension Code") { } }
}
codeunit 50480 "Base Dims"
{
    procedure Run(): Text
    var
        DimSetEntry: Record "Dimension Set Entry";
    begin
        DimSetEntry."Dimension Set ID" := 7;
        DimSetEntry."Dimension Code" := 'AREA';
        DimSetEntry.Insert();
        exit(Format(Database::"Dimension Set Entry") + ':' + DimSetEntry."Dimension Code");
    end;
}
"#,
    ),
    (
        "pbi",
        "00000000-0000-0000-0000-00000000b002",
        true,
        r#"namespace Microsoft.PowerBIReports;
table 36950 "Dimension Set Entry"
{
    ObsoleteState = Removed;
    fields { field(1; "Dimension Set ID"; Integer) { } }
}
"#,
    ),
    (
        "one",
        "00000000-0000-0000-0000-00000000b003",
        true,
        r#"namespace Microsoft.Inventory.Item.Substitution;
codeunit 7333 "Search"
{
    procedure Who(): Text
    begin
        exit('item-substitution');
    end;
}
enum 7332 "Search Style"
{
    value(0; Permissive) { Caption = 'Permissive (one)'; }
    value(1; Balanced) { Caption = 'Balanced (one)'; }
    value(2; Precise) { Caption = 'Precise (one)'; }
}
codeunit 7330 "Run One"
{
    procedure Run(): Text
    var
        SearchUtility: Codeunit "Search";
        Style: Enum "Search Style";
        Out: Text;
    begin
        Style := Enum::"Search Style"::Balanced;
        case Style of
            "Search Style"::Balanced:
                Out := 'case-ok';
            else
                Out := 'case-miss';
        end;
        exit(SearchUtility.Who() + '|' + Format(Style) + '|' + Out + '|' + Format(Codeunit::"Search"));
    end;
}
"#,
    ),
    (
        "two",
        "00000000-0000-0000-0000-00000000b004",
        true,
        r#"namespace Microsoft.Sales.Document;
codeunit 7282 "Search"
{
    procedure Who(): Text
    begin
        exit('sales-lines');
    end;
}
enum 7277 "Search Style"
{
    value(0; Permissive) { Caption = 'Permissive (two)'; }
    value(1; Balanced) { Caption = 'Balanced (two)'; }
    value(2; Precise) { Caption = 'Precise (two)'; }
}
codeunit 7275 "Run Two"
{
    procedure Run(): Text
    var
        SearchUtility: Codeunit "Search";
        Style: Enum "Search Style";
        Out: Text;
    begin
        Style := "Search Style"::Precise;
        case Style of
            "Search Style"::Precise:
                Out := 'case-ok';
            else
                Out := 'case-miss';
        end;
        exit(SearchUtility.Who() + '|' + Format(Style) + '|' + Out + '|' + Format(Codeunit::"Search"));
    end;
}
"#,
    ),
];

fn root() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("shared-object-names-{}", std::process::id()))
}

fn catalog() -> &'static Catalog {
    let mut dirs = vec![];
    // the Removed table and the unrelated apps load last, where last-wins picked them
    for (folder, id, dep, src) in APPS {
        let d = root().join(folder);
        std::fs::create_dir_all(&d).unwrap();
        let deps = if *dep { format!(r#"{{ "id": "{BASE}", "name": "Base", "publisher": "Microsoft", "version": "1.0.0.0" }}"#) } else { String::new() };
        std::fs::write(d.join("app.json"), format!(r#"{{ "id": "{id}", "name": "{folder}", "publisher": "Microsoft", "version": "1.0.0.0", "dependencies": [{deps}] }}"#)).unwrap();
        std::fs::write(d.join("Objects.al"), src).unwrap();
        dirs.push(d);
    }
    let cat = Catalog::load(&dirs).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn run(rt: &Runtime, cat: &'static Catalog, codeunit: &str) -> String {
    let mut s = Session::new(rt, "CRONUS");
    let ci = cat.code.iter().position(|c| c.name == codeunit).unwrap() as u32;
    let p = cat.code[ci as usize].find("Run")[0];
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(Instance::new(ci)), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{codeunit}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{codeunit}: unexpected flow"),
    }
}

#[test]
fn each_app_runs_its_own_object_in_both_engines() {
    if let Ok(engine) = std::env::var(CHILD) {
        assert_eq!(al_runtime::compiled::enabled(), engine == "compiled", "BC_ENGINE={engine} did not select that engine");
        let cat = catalog();
        let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
        assert_eq!(run(&rt, cat, "Base Dims"), "480:AREA", "BC_ENGINE={engine}");
        assert_eq!(run(&rt, cat, "Run One"), "item-substitution|Balanced (one)|case-ok|7333", "BC_ENGINE={engine}");
        assert_eq!(run(&rt, cat, "Run Two"), "sales-lines|Precise (two)|case-ok|7282", "BC_ENGINE={engine}");
        let _ = std::fs::remove_dir_all(root());
        println!("SHARED-NAMES-OK {engine}");
        return;
    }
    for engine in ["tree", "compiled"] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([TEST, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD, engine)
            .env("BC_ENGINE", engine)
            .output()
            .expect("run the test binary");
        let (o, e) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success() && o.contains(&format!("SHARED-NAMES-OK {engine}")), "BC_ENGINE={engine}:\n{o}\n{e}");
    }
}
