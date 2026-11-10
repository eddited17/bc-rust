//! Installing the loaded apps in a database, as BC does when extensions are installed in a
//! tenant: the Install codeunits (Subtype = Install) of each app run, apps after the apps they
//! depend on. Per app, OnInstallAppPerDatabase runs once, then OnInstallAppPerCompany in the
//! company. While an app installs, NavApp.IsInstalling() is true, the execution context is
//! Install and isolated events run like normal events (BC needs the installation in one
//! transaction).
//!
//! Callers: al-test runs it once per test database before the tests; a server can run it when it
//! creates a tenant's first company (`per_database` = true) or adds a company (false).

use crate::interp::*;
use crate::value::*;
use al_model::ProcRef;
use al_syntax::{Expr, ObjectKind};

/// What [`Session::install_apps`] did.
#[derive(Debug, Default, Clone)]
pub struct InstallReport {
    /// Install triggers that ran: (app, codeunit, trigger).
    pub ran: Vec<(String, String, &'static str)>,
    /// Apps whose Install codeunits ran, in install order.
    pub apps: Vec<String>,
    /// Install triggers that failed (rolled back): (app, "codeunit ... trigger: error").
    pub failed: Vec<(String, String)>,
    /// Apps whose Upgrade codeunits ran (also in `apps`).
    pub upgraded: Vec<String>,
    /// Ids (lower case) of the apps whose install or upgrade failed as a whole, or that depend on
    /// such an app ([`Session::run_app_lifecycle`]; their changes are rolled back).
    pub failed_apps: Vec<String>,
}

/// What [`Session::run_app_lifecycle`] does with an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    /// A new app: its Install codeunits run; DataVersion() is 0.0.0.0.
    Install,
    /// A newer version of an installed app: its Upgrade codeunits run; DataVersion() is `from`
    /// (the installed version), AppVersion() the new one.
    Upgrade { from: [i32; 4] },
}

const PER_DATABASE: &str = "oninstallappperdatabase";
const PER_COMPANY: &str = "oninstallapppercompany";
/// Upgrade triggers in BC's order (preconditions, upgrade, validation; database before company).
const UPGRADE_ALL: &[&str] = &[
    "oncheckpreconditionsperdatabase",
    "oncheckpreconditionspercompany",
    "onupgradeperdatabase",
    "onupgradepercompany",
    "onvalidateupgradeperdatabase",
    "onvalidateupgradepercompany",
];
const UPGRADE_COMPANY: &[&str] = &["oncheckpreconditionspercompany", "onupgradepercompany", "onvalidateupgradepercompany"];

thread_local! {
    static UPGRADING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Upgrade code is running in this thread (ExecutionContext::Upgrade, NavApp.IsInstalling false).
pub fn upgrading() -> bool {
    UPGRADING.with(|u| u.get())
}

impl<'r> Session<'r> {
    /// Install the loaded apps in the session's database and company: for every app (dependencies
    /// first) run its Install codeunits' OnInstallAppPerDatabase triggers (when `per_database`),
    /// then their OnInstallAppPerCompany triggers. A trigger that fails is rolled back and
    /// reported, and the others still run (BC would fail the app's installation; here a failure
    /// is mostly a runtime gap, which should not undo the rest of the install code). The changes
    /// stay in the session's transaction: commit them.
    pub fn install_apps(&mut self, per_database: bool) -> R<InstallReport> {
        self.install_apps_where(per_database, &|_| true)
    }

    /// [`Session::install_apps`] for the loaded apps `include` selects (e.g. only the apps a set
    /// of test apps depends on, see al_model::dependency_closure).
    pub fn install_apps_where(&mut self, per_database: bool, include: &dyn Fn(&al_model::AppManifest) -> bool) -> R<InstallReport> {
        self.lifecycle(per_database, false, &|m| include(m).then_some(AppAction::Install))
    }

    /// Install or upgrade the loaded apps `plan` selects, dependencies first, as BC does: Install
    /// codeunits for [`AppAction::Install`] (DataVersion() 0.0.0.0), Upgrade codeunits for
    /// [`AppAction::Upgrade`] (DataVersion() the installed version, ExecutionContext Upgrade).
    /// Without `per_database` only the per-company triggers run (a company added later). The
    /// first trigger of an app that fails rolls back all of that app's install or upgrade code and
    /// its remaining triggers don't run; apps that depend on it are skipped too. Those apps are in
    /// `failed_apps` (and `failed`, with the reason). Each app runs in its own transaction (the
    /// work before it is committed first); commit the last one.
    pub fn run_app_lifecycle(&mut self, per_database: bool, plan: &dyn Fn(&al_model::AppManifest) -> Option<AppAction>) -> R<InstallReport> {
        self.lifecycle(per_database, true, plan)
    }

    /// The lifecycle runner; `atomic`: an app's install or upgrade fails as a whole.
    fn lifecycle(&mut self, per_database: bool, atomic: bool, plan: &dyn Fn(&al_model::AppManifest) -> Option<AppAction>) -> R<InstallReport> {
        let cat = self.cat;
        let manifests = al_model::apps_of_dirs(&cat.app_dirs);
        let mut install_cus: Vec<Vec<u32>> = vec![vec![]; manifests.len()];
        let mut upgrade_cus: Vec<Vec<u32>> = vec![vec![]; manifests.len()];
        for (ci, co) in cat.code.iter().enumerate() {
            if co.kind != ObjectKind::Codeunit {
                continue;
            }
            let list = match co.subtype.as_deref() {
                Some(s) if s.eq_ignore_ascii_case("install") => &mut install_cus,
                Some(s) if s.eq_ignore_ascii_case("upgrade") => &mut upgrade_cus,
                _ => continue,
            };
            if let Some(a) = al_model::app_of_file(&manifests, &co.file) {
                list[a].push(ci as u32);
            }
        }
        let mods = crate::modules::modules(cat);
        let mut report = InstallReport::default();
        let mut failed_ids: std::collections::HashSet<String> = Default::default();
        for ai in al_model::install_order(&manifests) {
            let Some(action) = plan(&manifests[ai]) else { continue };
            let app = &manifests[ai];
            if atomic {
                if let Some(dep) = app.dependencies.iter().find(|d| failed_ids.contains(&d.to_ascii_lowercase())) {
                    let dep_name = manifests.iter().find(|m| m.id.eq_ignore_ascii_case(dep)).map_or(dep.clone(), |m| m.name.clone());
                    let what = if matches!(action, AppAction::Install) { "installed" } else { "upgraded" };
                    report.failed.push((app.name.clone(), format!("not {what}: it depends on {dep_name}, which failed")));
                    failed_ids.insert(app.id.to_ascii_lowercase());
                    report.failed_apps.push(app.id.to_ascii_lowercase());
                    continue;
                }
            }
            let (mut cus, triggers, data_version): (Vec<u32>, &[&'static str], [i32; 4]) = match action {
                AppAction::Install => (std::mem::take(&mut install_cus[ai]), if per_database { &[PER_DATABASE, PER_COMPANY] } else { &[PER_COMPANY] }, [0; 4]),
                AppAction::Upgrade { from } => (std::mem::take(&mut upgrade_cus[ai]), if per_database { UPGRADE_ALL } else { UPGRADE_COMPANY }, from),
            };
            if cus.is_empty() {
                continue;
            }
            cus.sort_by_key(|&c| cat.code[c as usize].id);
            let id = parse_guid(&app.id);
            let idx = mods.apps.iter().position(|m| Some(m.id) == id).map(|i| i as u32).unwrap_or(u32::MAX);
            let saved = self.installing.replace(idx);
            crate::modules::set_data_version(idx, Some(data_version));
            let up = matches!(action, AppAction::Upgrade { .. });
            UPGRADING.with(|u| u.set(up));
            report.apps.push(app.name.clone());
            if up {
                report.upgraded.push(app.name.clone());
            }
            // BC installs each app in its own transaction: commit what came before, so a failure
            // rolls back this app only (a savepoint would not survive a Commit() in its code)
            if atomic {
                self.db.commit().map_err(Self::store_err)?;
            }
            let ran_before = report.ran.len();
            let res = self.run_install_triggers(app, &cus, triggers, &mut report, atomic);
            UPGRADING.with(|u| u.set(false));
            crate::modules::set_data_version(idx, None);
            self.installing = saved;
            if !res? {
                // BC fails the app's install or upgrade: undo what its other triggers did
                self.rollback();
                report.ran.truncate(ran_before);
                failed_ids.insert(app.id.to_ascii_lowercase());
                report.failed_apps.push(app.id.to_ascii_lowercase());
            }
        }
        Ok(report)
    }

    /// Run `triggers` of `cus`; Ok(false) when one failed and `stop_on_failure` (the rest is then
    /// skipped). A failed trigger is rolled back and listed in `report.failed`.
    fn run_install_triggers(&mut self, app: &al_model::AppManifest, cus: &[u32], triggers: &[&'static str], report: &mut InstallReport, stop_on_failure: bool) -> R<bool> {
        for &trigger in triggers {
            for &cu in cus {
                let co = &self.cat.code[cu as usize];
                let Some(t) = co.trigger(trigger) else { continue };
                // (stop_on_failure: the caller rolls the whole app back instead)
                let mark = if stop_on_failure { None } else { Some(self.db.savepoint().map_err(Self::store_err)?) };
                match self.invoke(ProcRef { code: cu, proc: t }, vec![], Some(Instance::new(cu)), None, None) {
                    Err(Flow::Err(e)) => {
                        if let Some(mark) = mark {
                            self.db.rollback_to(mark).map_err(Self::store_err)?;
                        }
                        let at = e.stack.iter().take(3).cloned().collect::<Vec<_>>().join(" < ");
                        report.failed.push((app.name.clone(), format!("codeunit {} \"{}\" {trigger}: {} (at {at})", co.id, co.name, e.msg)));
                        if stop_on_failure {
                            return Ok(false);
                        }
                    }
                    _ => report.ran.push((app.name.clone(), co.name.clone(), trigger)),
                }
            }
        }
        Ok(true)
    }

    /// Session.GetExecutionContext(), GetCurrentModuleExecutionContext() and
    /// GetModuleExecutionContext(AppId): Install while that app (any app for the session's
    /// context) installs, otherwise Normal.
    pub fn execution_context(&mut self, f: &mut Frame, m: &str, args: &[Expr]) -> R<Val> {
        let mods = crate::modules::modules(self.cat);
        let installing = match (m, self.installing) {
            (_, None) => false,
            ("getexecutioncontext", Some(_)) => true,
            ("getcurrentmoduleexecutioncontext", Some(a)) => mods.of_code.get(f.code as usize).copied().flatten() == Some(a),
            (_, Some(a)) => match args.first() {
                Some(e) => matches!(self.eval(f, e)?, Val::Guid(g) if mods.apps.get(a as usize).is_some_and(|x| x.id == g)),
                None => false,
            },
        };
        let ctx = match installing {
            true if upgrading() => "upgrade",
            true => "install",
            false => "normal",
        };
        // with its members, so `Session.GetExecutionContext()::Normal` resolves (crate::optmeta)
        static MEMBERS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        let members = MEMBERS.get_or_init(|| ["Normal", "Install", "Upgrade"].map(String::from).to_vec());
        Ok(Val::Opt { ord: system_option_ord("executioncontext", ctx), enum_id: Some(crate::optmeta::for_members(members)) })
    }
}
