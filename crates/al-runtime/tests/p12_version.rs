//! Version values (P12): Version.Create, comparison, parts and Format; and DataVersion() /
//! AppVersion() / the execution context while an app installs (0.0.0.0) and upgrades (the
//! installed version), through Session::run_app_lifecycle.

use al_model::{Catalog, ProcRef};
use al_runtime::install::AppAction;
use al_runtime::{Flow, Instance, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP_JSON: &str = r#"{ "id": "7c1f0d4e-12ab-4c5d-9e8f-0a1b2c3d4e5f", "name": "P12 Version Test", "publisher": "bc-rust", "version": "2.3.4.5" }"#;

const APP: &str = r#"
codeunit 50720 "P12 Log"
{
    SingleInstance = true;

    var
        Entries: Text;

    procedure Add(Entry: Text)
    begin
        Entries += Entry + ';';
    end;

    procedure Get(): Text
    begin
        exit(Entries);
    end;
}

codeunit 50721 "P12 Install"
{
    Subtype = Install;

    trigger OnInstallAppPerCompany()
    var
        Log: Codeunit "P12 Log";
        Info: ModuleInfo;
    begin
        NavApp.GetCurrentModuleInfo(Info);
        Log.Add('install ' + Format(Info.DataVersion()) + ' ' + Format(Info.AppVersion()) + ' ' + Format(Info.DataVersion() = Version.Create(0, 0, 0, 0)) + ' ' + Format(NavApp.IsInstalling()) + ' ' +
          Format(Session.GetCurrentModuleExecutionContext() = ExecutionContext::Install));
    end;
}

codeunit 50722 "P12 Upgrade"
{
    Subtype = Upgrade;

    trigger OnUpgradePerCompany()
    var
        Log: Codeunit "P12 Log";
        Info: ModuleInfo;
    begin
        NavApp.GetCurrentModuleInfo(Info);
        Log.Add('upgrade ' + Format(Info.DataVersion()) + ' ' + Format(Info.AppVersion()) + ' ' + Format(Info.DataVersion() < Info.AppVersion()) + ' ' + Format(NavApp.IsInstalling()) + ' ' +
          Format(Session.GetCurrentModuleExecutionContext() = ExecutionContext::Upgrade));
    end;
}

codeunit 50723 "P12 Version Ops"
{
    procedure Ops(): Text
    var
        V: Version;
    begin
        V := Version.Create('1.2.3.4');
        exit(Format(Version.Create(1, 2)) + '|' + Format(V) + '|' + Format(V.Major()) + Format(V.Minor()) + Format(V.Build()) + Format(V.Revision()) + '|' +
          Format(V = Version.Create(1, 2, 3, 4)) + Format(Version.Create(1, 10, 0, 0) > Version.Create(1, 9, 0, 0)) + Format(Version.Create(1, 2) < Version.Create(1, 2, 0, 0)) + Format(V <> Version.Create(1, 2, 3, 5)));
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("p12-version-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.json"), APP_JSON).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    // the directory stays until the end: modules() reads app.json lazily
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, cu: &str, proc: &str) -> String {
    let ci = cat.code.iter().position(|c| c.name == cu).unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    let inst = if cat.code[ci as usize].single_instance { s.single_instance(ci) } else { Instance::new(ci) };
    match s.invoke(ProcRef { code: ci, proc: p }, vec![], Some(inst), None, None) {
        Ok(v) => v.to_text(),
        Err(Flow::Err(e)) => panic!("{proc}: {} {:?}", e.msg, e.stack),
        Err(_) => panic!("{proc}: unexpected flow"),
    }
}

#[test]
fn version_values_and_lifecycle_data_version() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    assert_eq!(call(&mut s, cat, "P12 Version Ops", "Ops"), "1.2|1.2.3.4|1234|YesYesYesYes");
    let Ok(rep) = s.run_app_lifecycle(true, &|_| Some(AppAction::Install)) else { panic!("install") };
    assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    let Ok(rep) = s.run_app_lifecycle(true, &|_| Some(AppAction::Upgrade { from: [1, 0, 0, 0] })) else { panic!("upgrade") };
    assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    assert_eq!(rep.upgraded, ["P12 Version Test"]);
    assert_eq!(call(&mut s, cat, "P12 Log", "Get"), "install 0.0.0.0 2.3.4.5 Yes Yes Yes;upgrade 1.0.0.0 2.3.4.5 Yes No Yes;");
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("p12-version-test-{}", std::process::id())));
}
