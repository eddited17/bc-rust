//! Platform data a BC tenant has before any AL runs: the company, the signed-in user (SUPER) and
//! the profiles of the installed apps (table "All Profile").

use crate::interp::*;
use crate::record::*;
use crate::value::*;

/// Security id of the session user (UserSecurityId()).
pub const USER_SID: [u8; 16] = [0x11; 16];

impl<'r> Session<'r> {
    fn platform_row(&mut self, table: &str, fields: &[(&str, Val)]) -> R<()> {
        let Some(t) = self.cat.table_by_name(table) else { return Ok(()) };
        let r = self.new_record(t.id, false);
        for (name, v) in fields {
            if let Some(fi) = t.field(name) {
                let sv = to_store(&t.fields[fi], v).map_err(|m| Flow::Err(Box::new(AlError::new(m))))?;
                r.borrow_mut().row[fi] = sv;
            }
        }
        if self.rec_get_pk_r(&r)?.is_none() {
            if let Err(m) = self.rec_insert(&r, false)? {
                return err(m);
            }
        }
        Ok(())
    }

    /// Insert a Company row for another company of the tenant (no data), keyed like the session
    /// company's row (al-test AL_TEST_EXTRA_COMPANIES).
    pub fn add_company_row(&mut self, company: &str) -> R<()> {
        let mut gid = [0x43u8; 16];
        for (i, b) in company.bytes().enumerate() {
            gid[i % 16] ^= b;
        }
        self.platform_row("Company", &[("Name", Val::text(company)), ("Display Name", Val::text(company)), ("Id", Val::Guid(gid))])
    }

    /// Insert the Company row for the session company, the session user with SUPER, and one
    /// "All Profile" row per profile object (Business Manager is the default role center).
    /// Rows that already exist are kept.
    pub fn init_platform_data(&mut self) -> R<()> {
        let company = self.company.clone();
        let mut gid = [0x43u8; 16];
        for (i, b) in company.bytes().enumerate() {
            gid[i % 16] ^= b;
        }
        let company_id = Val::Guid(gid);
        self.platform_row("Company", &[("Name", Val::text(&company)), ("Display Name", Val::text(&company)), ("Id", company_id)])?;
        let user = self.user_id.clone();
        self.platform_row(
            "User",
            &[("User Security ID", Val::Guid(USER_SID)), ("User Name", Val::text(&user)), ("Full Name", Val::text(&user)), ("Authentication Email", Val::text("admin@cronus.example"))],
        )?;
        self.platform_row("Access Control", &[("User Security ID", Val::Guid(USER_SID)), ("Role ID", Val::text("SUPER")), ("User Name", Val::text(&user))])?;
        for p in &self.cat.profiles {
            let rc = p.role_center.unwrap_or(0);
            self.platform_row(
                "All Profile",
                &[
                    ("Profile ID", Val::text(p.name.chars().take(30).collect::<String>())),
                    ("Description", Val::text(&p.description)),
                    ("Caption", Val::text(&p.caption)),
                    ("Role Center ID", Val::Int(rc as i32)),
                    ("Enabled", Val::Bool(p.enabled)),
                    ("Promoted", Val::Bool(p.promoted)),
                    ("Default Role Center", Val::Bool(rc == 9022)),
                ],
            )?;
        }
        self.init_permission_sets()
    }

    /// One row per PermissionSet object of the loaded apps: "Metadata Permission Set" (App ID, Role
    /// ID = the object name, Name = its Caption, Assignable), "Aggregate Permission Set" (Scope
    /// System) and the legacy "Permission Set" (Role ID, Name) that the test framework's
    /// Permissions Mock reads. Role IDs longer than those tables' Code[20] are left out there.
    fn init_permission_sets(&mut self) -> R<()> {
        let mods = crate::modules::modules(self.cat);
        // the app of a file: the app of a code object under the same (longest) app directory
        let app_of = |file: &std::path::Path| {
            let root = self.cat.app_dirs.iter().filter(|d| file.starts_with(d)).max_by_key(|d| d.as_os_str().len())?;
            let ci = self.cat.code.iter().position(|co| co.file.starts_with(root))?;
            mods.app_of_code(ci as u32)
        };
        let sets: Vec<_> = self.cat.permission_sets.iter().map(|p| (p.name.clone(), p.caption.chars().take(30).collect::<String>(), p.assignable, app_of(&p.file).map(|a| (a.id, a.name.clone())))).collect();
        for (role, name, assignable, app) in sets {
            let (app_id, app_name) = app.unwrap_or_default();
            if role.chars().count() <= 30 {
                self.platform_row(
                    "Metadata Permission Set",
                    &[("App ID", Val::Guid(app_id)), ("Role ID", Val::text(&role)), ("Name", Val::text(&name)), ("Assignable", Val::Bool(assignable))],
                )?;
            }
            if role.chars().count() <= 20 {
                self.platform_row(
                    "Aggregate Permission Set",
                    &[("Scope", Val::Int(0)), ("App ID", Val::Guid(app_id)), ("Role ID", Val::text(&role)), ("Name", Val::text(&name)), ("App Name", Val::text(&app_name))],
                )?;
                self.platform_row("Permission Set", &[("Role ID", Val::text(&role)), ("Name", Val::text(&name))])?;
            }
        }
        Ok(())
    }
}

/// Public base URL of the web client (`BC_PUBLIC_URL`, default http://localhost:8080/BC), without
/// a trailing slash.
pub fn public_base_url() -> String {
    std::env::var("BC_PUBLIC_URL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "http://localhost:8080/BC".into()).trim_end_matches('/').to_string()
}

fn url_encode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

impl<'r> Session<'r> {
    /// GetUrl(ClientType [, Company [, ObjectType, ObjectId [, Record [, UseFilters]]]]): the web
    /// client URL `<base>/?company=X&page=21`, or the base URL of the SOAP / OData / API services.
    pub fn get_url(&self, args: &[Val]) -> String {
        let base = public_base_url();
        let client = args.first().map(|v| v.as_i64() as i32).unwrap_or(crate::interp::system_option_ord("clienttype", "web"));
        let company = args.get(1).map(|v| v.to_text()).filter(|c| !c.is_empty()).unwrap_or_else(|| self.company.clone());
        let ord = |m: &str| crate::interp::system_option_ord("clienttype", m);
        if client == ord("soap") {
            return format!("{base}/WS/{}/", url_encode(&company));
        }
        if client == ord("odatav4") {
            return format!("{base}/ODataV4/");
        }
        if client == ord("api") {
            return format!("{base}/api/");
        }
        let mut url = format!("{base}/?company={}", url_encode(&company));
        if let (Some(ty), Some(id)) = (args.get(2), args.get(3)) {
            let kind = match ty.as_i64() {
                0 | 1 => Some("table"),
                3 => Some("report"),
                5 => Some("codeunit"),
                6 => Some("xmlport"),
                8 => Some("page"),
                9 => Some("query"),
                _ => None,
            };
            if let Some(k) = kind {
                url.push_str(&format!("&{k}={}", id.as_i64()));
            }
        }
        url
    }
}
