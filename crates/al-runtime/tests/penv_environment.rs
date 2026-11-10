//! Environment identity (crate::environment): bc-rust answers like an on-premises BC 29.1
//! single-tenant server: ApplicationIdentifier() 'NAV', NavTenantSettingsHelper (production
//! tenant 'default', family W1, platform 29.0.57431.0), ALConfigSettings (IsSaaS false,
//! TestAutomationEnabled true), NavUserAccountHelper.IsAzure false. The System Application's
//! testability switch (SetTestabilitySoftwareAsAService) is AL and still flips IsSaaS.

use al_model::{Catalog, ProcRef};
use al_runtime::environment::DeploymentKind;
use al_runtime::{Flow, Runtime, Session};
use bc_store::{MemBackend, Store};
use std::sync::Arc;

const APP_JSON: &str = r#"{ "id": "3f1e2d4c-5b6a-4789-9abc-def012345678", "name": "Env Test", "publisher": "bc-rust", "version": "1.0.0.0" }"#;

const APP: &str = r#"
dotnet
{
    assembly("Microsoft.Dynamics.Nav.NavUserAccount")
    {
        type("Microsoft.Dynamics.Nav.NavUserAccount.NavTenantSettingsHelper"; "NavTenantSettingsHelper") { }
        type("Microsoft.Dynamics.Nav.NavUserAccount.NavUserAccountHelper"; "NavUserAccountHelper") { }
    }
    assembly("Microsoft.Dynamics.Nav.Types")
    {
        type("Microsoft.Dynamics.Nav.Types.ALConfigSettings"; "ALConfigSettings") { }
    }
}

codeunit 50790 "Env Impl"
{
    SingleInstance = true;

    var
        NavTenantSettingsHelper: DotNet NavTenantSettingsHelper;
        TestabilitySoftwareAsAService: Boolean;

    procedure SetTestabilitySoftwareAsAService(Enable: Boolean)
    begin
        TestabilitySoftwareAsAService := Enable;
    end;

    // as System Application's Environment Information Impl. / Server Setting Impl.
    procedure IsSaaS(): Boolean
    var
        ALConfigSettings: DotNet ALConfigSettings;
    begin
        if TestabilitySoftwareAsAService then
            exit(true);
        ALConfigSettings := ALConfigSettings.Instance();
        exit(NavTenantSettingsHelper.IsSandbox() or ALConfigSettings.IsSaaS());
    end;

    procedure IsOnPrem(): Boolean
    begin
        exit(ApplicationIdentifier() = 'NAV');
    end;

    procedure Identity(): Text
    var
        UserAccountHelper: DotNet NavUserAccountHelper;
        ALConfigSettings: DotNet ALConfigSettings;
        TenantIdValue: Text;
        DisplayName: Text;
        Other: Text;
    begin
        ALConfigSettings := ALConfigSettings.Instance();
        exit(ApplicationIdentifier() + '|' + Format(IsOnPrem()) + Format(IsSaaS()) + Format(NavTenantSettingsHelper.IsSandbox()) + Format(NavTenantSettingsHelper.IsProduction()) + '|' +
          Format(NavTenantSettingsHelper.TryGetStringTenantSetting('TENANTID', TenantIdValue)) + TenantIdValue + Format(NavTenantSettingsHelper.TryGetStringTenantSetting('displayname', DisplayName)) + DisplayName +
          Format(NavTenantSettingsHelper.TryGetStringTenantSetting('Nope', Other)) + '|' + NavTenantSettingsHelper.GetEnvironmentName() + '|' + NavTenantSettingsHelper.GetApplicationFamily() + '|' +
          Format(NavTenantSettingsHelper.GetPlatformVersion()) + '|' + NavTenantSettingsHelper.GetRingName() + '|' + Format(ALConfigSettings.TestAutomationEnabled()) + Format(UserAccountHelper.IsAzure()) + '|' +
          TenantId() + Database.TenantId());
    end;

    procedure SetTestSandbox()
    begin
        NavTenantSettingsHelper.SetTestTenantEnvironmentType(true);
    end;
}
"#;

fn catalog() -> &'static Catalog {
    let dir = std::env::temp_dir().join(format!("penv-environment-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("app.json"), APP_JSON).unwrap();
    std::fs::write(dir.join("App.al"), APP).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    Box::leak(Box::new(cat))
}

fn call(s: &mut Session, cat: &Catalog, proc: &str, args: Vec<al_runtime::interp::Arg>) -> Result<String, String> {
    let ci = cat.code.iter().position(|c| c.name == "Env Impl").unwrap() as u32;
    let p = cat.code[ci as usize].find(proc)[0];
    let inst = s.single_instance(ci);
    match s.invoke(ProcRef { code: ci, proc: p }, args, Some(inst), None, None) {
        Ok(v) => Ok(v.to_text()),
        Err(Flow::Err(e)) => Err(e.msg),
        Err(_) => Err("unexpected flow".into()),
    }
}

#[test]
fn on_prem_identity_and_testability_switch() {
    let cat = catalog();
    let rt = Runtime::new(cat, Arc::new(MemBackend::new(Store::memory())));
    let mut s = Session::new(&rt, "CRONUS");
    assert_eq!(s.sys.env.kind, DeploymentKind::OnPrem);
    assert_eq!(call(&mut s, cat, "Identity", vec![]).unwrap(), "NAV|YesNoNoYes|YesdefaultYesNo||W1|29.0.57431.0||YesNo|defaultdefault");
    // the AL testability switch still makes IsSaaS() true; the platform answer stays on-prem
    call(&mut s, cat, "SetTestabilitySoftwareAsAService", vec![al_runtime::interp::Arg::Val(al_runtime::Val::Bool(true))]).unwrap();
    assert_eq!(call(&mut s, cat, "IsSaaS", vec![]).unwrap(), "Yes");
    assert_eq!(call(&mut s, cat, "IsOnPrem", vec![]).unwrap(), "Yes");
    // SetTestTenantEnvironmentType only works while tests run (as BC's InvalidOperationException)
    let e = call(&mut s, cat, "SetTestSandbox", vec![]).unwrap_err();
    assert!(e.contains("only available while tests run"), "{e}");
    assert!(!s.sys.env.test_sandbox);
    // a SaaS session answers 'FIN' (the option for later)
    let mut s2 = Session::new(&rt, "CRONUS");
    s2.sys.env.kind = DeploymentKind::SaaS;
    assert!(call(&mut s2, cat, "Identity", vec![]).unwrap().starts_with("FIN|NoYesNoYes|"));
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("penv-environment-test-{}", std::process::id())));
}
