//! How the service tier identifies its deployment to AL code: one source of truth for
//! ApplicationIdentifier(), the NavTenantSettingsHelper and ALConfigSettings DotNet helpers
//! (behind the System Application's Environment Information / Tenant Information / Server
//! Setting codeunits) and NavUserAccountHelper.IsAzure.
//!
//! bc-rust runs self-hosted, so the default is an on-premises Business Central 29.1 single-tenant
//! server, which is also how Microsoft's CI test containers run (BcContainerHelper). Values as in
//! BC 29.1 [src:TODO]:
//! - ApplicationIdentifier() = `NavSession.AppId` = the `DefaultApplicationId` server setting,
//!   default "NAV" (Types `ServerUserSettings`); SaaS servers use "FIN".
//! - NavTenantSettingsHelper (NavUserAccount.dll) reads the tenant's `NavTenantSettings`. The
//!   single-tenant "default" tenant (Ncl `NavTenantCollection`) has Id "default", no DisplayName
//!   or EnvironmentName (null), EnvironmentType = the `TenantEnvironmentType` setting (Production)
//!   and AadTenantId = `ServerUserSettings.AadTenantId` ("common" without AAD configuration).
//!   IsSandbox() is also true while tests run after `SetTestTenantEnvironmentType(true)`.
//!   GetRingName() comes from the `ApplicationServiceProperties` setting, empty on-premises.
//!   GetApplicationFamily() is the application database's family, "W1" for the W1 app.
//! - ALConfigSettings (Types) holds server settings; CustomSettings.config of the on-prem
//!   install: EnableMembershipEntitlement "" (so IsSaaS() is false), TestAutomationEnabled,
//!   UsePermissionSetsFromExtensions, ApiServicesEnabled, ApiSubscriptionsEnabled true,
//!   EnableEntraGroupsOnPrem and EnableSaasExtensionInstall false.
//! - NavUserAccountHelper.IsAzure() = !`IsServiceRunningInLocalEnvironment` (false on-prem).
//!
//! The AL-level testability switches (Environment Information Impl.
//! SetTestabilitySoftwareAsAService / SetTestabilitySandbox) are AL code and keep working on top.

use crate::dotnet::{net, ok, Args, NetObj, Out};
use crate::interp::{Frame, Session, R};
use crate::value::Val;

pub const TENANT_SETTINGS_HELPER: &str = "Microsoft.Dynamics.Nav.NavUserAccount.NavTenantSettingsHelper";
pub const CONFIG_SETTINGS: &str = "Microsoft.Dynamics.Nav.Types.ALConfigSettings";

/// The platform's file version (VersionCompatibility.AssemblyFileVersion of the 29.1 NST).
pub const PLATFORM_VERSION: [i32; 4] = [29, 0, 57431, 0];

/// What kind of Business Central deployment bc-rust presents itself as.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeploymentKind {
    /// A self-hosted (on-premises) single-tenant server: ApplicationIdentifier() 'NAV'.
    #[default]
    OnPrem,
    /// Business Central online (a production environment): ApplicationIdentifier() 'FIN'.
    SaaS,
}

impl DeploymentKind {
    /// `onprem` / `saas` (case-insensitive).
    pub fn parse(s: &str) -> Option<DeploymentKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "onprem" | "on-prem" | "nav" => Some(DeploymentKind::OnPrem),
            "saas" | "online" | "fin" => Some(DeploymentKind::SaaS),
            _ => None,
        }
    }

    /// ApplicationIdentifier().
    pub fn application_identifier(self) -> &'static str {
        match self {
            DeploymentKind::OnPrem => "NAV",
            DeploymentKind::SaaS => "FIN",
        }
    }

    /// ALConfigSettings.IsSaaS(): the EnableMembershipEntitlement server setting.
    pub fn membership_entitlement(self) -> bool {
        self == DeploymentKind::SaaS
    }

    /// NavUserAccountHelper.IsAzure(): the service does not run in a local environment.
    pub fn is_azure(self) -> bool {
        self == DeploymentKind::SaaS
    }

    /// The tenant's EnvironmentName setting (null on-prem; AL then reports 'Production').
    pub fn environment_name(self) -> &'static str {
        match self {
            DeploymentKind::OnPrem => "",
            DeploymentKind::SaaS => "Production",
        }
    }

    /// The tenant's AadTenantId setting.
    pub fn aad_tenant_id(self) -> &'static str {
        match self {
            DeploymentKind::OnPrem => "common",
            DeploymentKind::SaaS => "00000000-0000-0000-0000-000000000000",
        }
    }
}

/// The session's view of its environment (crate::sysfuncs::SysState::env).
#[derive(Clone, Debug, Default)]
pub struct EnvState {
    pub kind: DeploymentKind,
    /// NavTenantSettingsHelper.SetTestTenantEnvironmentType(true) (Library - Permissions):
    /// IsSandbox() answers true while tests run. BC keeps it in a static field; here it lives as
    /// long as the session (a test, or a codeunit under `--isolation codeunit`).
    pub test_sandbox: bool,
}

impl EnvState {
    /// NavTenantSettingsHelper.IsSandbox(): the tenant's EnvironmentType is Sandbox, or the test
    /// override is set and a test is running.
    pub fn is_sandbox(&self, in_test: bool) -> bool {
        (in_test && self.test_sandbox) || self.tenant_sandbox()
    }

    /// The tenant's own EnvironmentType (Production for both kinds so far).
    fn tenant_sandbox(&self) -> bool {
        false
    }

    /// The tenant id (TenantId(), Database.TenantId(), TryGetStringTenantSetting('TENANTID')).
    pub fn tenant_id(&self) -> &'static str {
        "default"
    }

    /// GetApplicationFamily().
    pub fn application_family(&self) -> &'static str {
        "W1"
    }

    /// A boolean or integer ALConfigSettings member (lower-case name), None when unknown.
    pub fn config_setting(&self, m: &str) -> Option<Val> {
        Some(match m {
            "issaas" => Val::Bool(self.kind.membership_entitlement()),
            "testautomationenabled" | "usepermissionsfromextensions" | "apiservicesenabled" | "apisubscriptionsenabled" => Val::Bool(true),
            "enablesaasextensioninstallconfigsetting" | "issaasexceladdinenabled" | "enableentragroupsonprem" => Val::Bool(false),
            "apisubscriptionsendingnotificationtimeout" | "apisubscriptiondelaytime" => Val::Int(30000),
            "apisubscriptionmaxnumberofnotifications" => Val::Int(1000),
            _ => return None,
        })
    }
}

impl<'r> Session<'r> {
    /// A NavTenantSettingsHelper (static) member.
    pub fn tenant_settings_call(&mut self, f: &mut Frame, m: &str, a: &Args) -> R<Out> {
        let in_test = self.test.is_some();
        let env = &self.sys.env;
        ok(match m {
            "issandbox" => Val::Bool(env.is_sandbox(in_test)),
            "isproduction" => Val::Bool(!env.is_sandbox(in_test)),
            "settesttenantenvironmenttype" => {
                if !in_test {
                    // own wording [src:X25]
                    return crate::dotnet::net_err(TENANT_SETTINGS_HELPER, m, "SetTestTenantEnvironmentType is only available while tests run.");
                }
                self.sys.env.test_sandbox = a.bool(0);
                Val::Void
            }
            "getenvironmentname" => Val::Text(env.kind.environment_name().into()),
            "getapplicationfamily" => Val::Text(env.application_family().into()),
            "getplatformversion" => net(NetObj::Version(PLATFORM_VERSION.to_vec())),
            "getringname" | "getappserviceid" | "getappservicelocation" | "getappservicegeo" | "getappservicefamily" | "getappserviceplatformmajorminor"
            | "getappservicecountrycodes" | "getlinkedpowerplatformenvironmentid" | "getenvironmentsetting" | "getenvironmentapplicationsetting" => Val::Text(String::new()),
            "getappserviceineudb" => Val::Bool(false),
            "iswskeyallowed" => Val::Bool(true),
            "enablem365collaboration" => Val::Void,
            "trygetstringtenantsetting" => {
                let value = match a.text(0).to_ascii_uppercase().as_str() {
                    "TENANTID" => Some(env.tenant_id().to_string()),
                    "DISPLAYNAME" => Some(String::new()),
                    "AADTENANTID" => Some(env.kind.aad_tenant_id().to_string()),
                    _ => None,
                };
                let found = value.is_some();
                self.net_out(f, a, 1, Val::Text(value.unwrap_or_default()))?;
                Val::Bool(found)
            }
            _ => return Ok(Out::Unknown),
        })
    }

    /// An ALConfigSettings member: `Instance()` gives the settings object, the rest are settings.
    pub fn config_settings_call(&mut self, m: &str) -> R<Out> {
        if m == "instance" {
            return ok(net(NetObj::Prim(CONFIG_SETTINGS, Val::Void)));
        }
        Ok(match self.sys.env.config_setting(m) {
            Some(v) => Out::Val(v),
            None => Out::Unknown,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_prem_is_the_default() {
        let env = EnvState::default();
        assert_eq!(env.kind, DeploymentKind::OnPrem);
        assert_eq!(env.kind.application_identifier(), "NAV");
        assert!(!env.kind.membership_entitlement());
        assert!(!env.kind.is_azure());
        assert_eq!(env.kind.environment_name(), "");
        assert_eq!(env.tenant_id(), "default");
        assert_eq!(env.application_family(), "W1");
        assert!(!env.is_sandbox(false) && !env.is_sandbox(true));
        assert!(matches!(env.config_setting("issaas"), Some(Val::Bool(false))));
        assert!(matches!(env.config_setting("testautomationenabled"), Some(Val::Bool(true))));
        assert!(env.config_setting("nosuchsetting").is_none());
    }

    #[test]
    fn saas_kind() {
        let k = DeploymentKind::parse("SaaS").unwrap();
        assert_eq!(k.application_identifier(), "FIN");
        assert!(k.membership_entitlement() && k.is_azure());
        assert_eq!(DeploymentKind::parse("onprem"), Some(DeploymentKind::OnPrem));
        assert_eq!(DeploymentKind::parse("x"), None);
    }

    #[test]
    fn test_sandbox_override_only_in_tests() {
        let env = EnvState { test_sandbox: true, ..Default::default() };
        assert!(env.is_sandbox(true));
        assert!(!env.is_sandbox(false));
    }
}
