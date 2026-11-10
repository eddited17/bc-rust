//! `bc user ...`: user accounts for `BC_AUTH` (docs/authentication.md), the counterpart of BC's
//! New-NAVServerUser / Set-NAVServerUser for operators. Writes User, "User Property" and Access Control in
//! BC's format, so the User Card and AL see the same data.
//!
//! ```text
//! bc user list
//! bc user add <NAME> [--role <ROLE ID>]...     enabled user; Access Control rows for all companies
//! bc user password <NAME> [--change-at-next-sign-in]   new password from stdin (first line)
//! bc user key <NAME> [--days <N>]             new web service access key, printed once
//! bc user disable <NAME> | enable <NAME>
//! ```
//! plus the database flags of `bc serve` (`--pg [conn]`, `--db <file>`, `--apps <set>`).

use crate::app::{flow_err, set};
use al_runtime::userauth::{self, USER_PROPERTY_TABLE, USER_TABLE};
use al_runtime::{Session, Val};
use anyhow::{anyhow, bail, Context};
use rsa::rand_core::{OsRng, RngCore};

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    OsRng.fill_bytes(&mut b);
    b
}

/// A new web service access key, as BC makes them (al_runtime::userauth::new_access_key).
pub fn new_access_key() -> String {
    userauth::new_access_key()
}

fn find(s: &mut Session, name: &str) -> anyhow::Result<userauth::UserCreds> {
    userauth::users(s, 0).into_iter().find(|u| u.name.eq_ignore_ascii_case(name)).ok_or_else(|| anyhow!("no user '{name}' (bc user add {name})"))
}

fn user_record(s: &mut Session, sid: &[u8; 16]) -> anyhow::Result<al_runtime::RecHandle> {
    let h = s.new_record(USER_TABLE, false);
    set(s, &h, "User Security ID", Val::Guid(*sid))?;
    let row = s.rec_get_pk(&h).ok_or_else(|| anyhow!("user row vanished"))?;
    h.borrow_mut().row = row.into();
    Ok(h)
}

fn property_record(s: &mut Session, sid: &[u8; 16]) -> anyhow::Result<al_runtime::RecHandle> {
    let h = s.new_record(USER_PROPERTY_TABLE, false);
    set(s, &h, "User Security ID", Val::Guid(*sid))?;
    match s.rec_get_pk(&h) {
        Some(row) => h.borrow_mut().row = row.into(),
        None => s.rec_insert(&h, false).map_err(flow_err)?.map_err(|m| anyhow!(m))?,
    }
    Ok(h)
}

/// Run one `bc user` command in session `s` and commit; returns what to print.
pub fn run(s: &mut Session, args: &[String], stdin: &mut dyn std::io::BufRead) -> anyhow::Result<String> {
    // positional words and --role / --days, without the database flags main.rs reads
    let (mut words, mut roles, mut days) = (vec![], vec![], None::<i64>);
    let change = args.iter().any(|a| a == "--change-at-next-sign-in");
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let next = args.get(i + 1).filter(|n| !n.starts_with("--")).cloned();
        match a {
            "--role" => roles.push(next.clone().ok_or_else(|| anyhow!("--role needs a role id"))?),
            "--days" => days = Some(next.clone().and_then(|d| d.parse().ok()).ok_or_else(|| anyhow!("--days needs a number"))?),
            "--pg" | "--db" | "--apps" => {}
            "--change-at-next-sign-in" => {
                i += 1;
                continue;
            }
            "--memory" => bail!("bc user needs a database that outlives the command (--pg or --db)"),
            _ => {
                words.push(a.to_string());
                i += 1;
                continue;
            }
        }
        i += if next.is_some() { 2 } else { 1 };
    }
    let name = || words.get(1).cloned().ok_or_else(|| anyhow!("missing user name"));
    let out = match words.first().map(String::as_str) {
        Some("list") => {
            let mut o = String::new();
            for u in userauth::users(s, chrono::Utc::now().timestamp_millis()) {
                o += &format!(
                    "{:<20} {} password:{} key:{} {}\n",
                    u.name,
                    al_runtime::value::format_guid(&u.sid),
                    if u.password_hash.is_empty() { "no" } else { "yes" },
                    if u.ws_key.is_empty() { "no" } else { "yes" },
                    if u.enabled { "enabled" } else { "disabled" }
                );
            }
            o
        }
        Some("add") => {
            let name = name()?.to_uppercase();
            if userauth::users(s, 0).iter().any(|u| u.name.eq_ignore_ascii_case(&name)) {
                bail!("user '{name}' exists");
            }
            let mut sid = random::<16>();
            sid[6] = (sid[6] & 0x0f) | 0x40; // a version 4 GUID (text byte order)
            sid[8] = (sid[8] & 0x3f) | 0x80;
            let h = s.new_record(USER_TABLE, false);
            set(s, &h, "User Security ID", Val::Guid(sid))?;
            set(s, &h, "User Name", Val::text(&name))?;
            set(s, &h, "Full Name", Val::text(&name))?;
            s.rec_insert(&h, false).map_err(flow_err)?.map_err(|m| anyhow!(m))?;
            property_record(s, &sid)?;
            let ac = s.cat.table_by_name("Access Control").ok_or_else(|| anyhow!("no Access Control table"))?.id;
            for r in &roles {
                let a = s.new_record(ac, false);
                set(s, &a, "User Security ID", Val::Guid(sid))?;
                set(s, &a, "Role ID", Val::text(r))?;
                s.rec_insert(&a, false).map_err(flow_err)?.map_err(|m| anyhow!(m))?;
            }
            format!("added {name} {} roles {roles:?}; set a password with: bc user password {name}\n", al_runtime::value::format_guid(&sid))
        }
        Some("password") => {
            let u = find(s, &name()?)?;
            let mut line = String::new();
            stdin.read_line(&mut line).context("reading the password from stdin")?;
            let pw = line.trim_end_matches(['\r', '\n']);
            if pw.is_empty() {
                bail!("empty password");
            }
            userauth::set_user_password(s, &u.sid, pw, change).map_err(|m| anyhow!(m))?;
            format!("password set for {}{}\n", u.name, if change { " (change at next sign-in)" } else { "" })
        }
        Some("key") => {
            let u = find(s, &name()?)?;
            let key = new_access_key();
            let expiry = days.map(|d| chrono::Utc::now().timestamp_millis() + d * 86_400_000);
            userauth::set_access_key(s, &u.sid, Some((&key, expiry))).map_err(|m| anyhow!(m))?;
            format!("web service access key for {}: {key}\n", u.name)
        }
        Some(w @ ("disable" | "enable")) => {
            let u = find(s, &name()?)?;
            let h = user_record(s, &u.sid)?;
            let fi = h.borrow().tdef.field("State").ok_or_else(|| anyhow!("no State field"))?;
            h.borrow_mut().row[fi] = bc_store::Value::Option(if w == "disable" { 1 } else { 0 });
            s.rec_modify(&h, false).map_err(flow_err)?.map_err(|m| anyhow!(m))?;
            format!("{w}d {}\n", u.name)
        }
        _ => bail!("usage: bc user list | add <NAME> [--role <ROLE>]... | password <NAME> [--change-at-next-sign-in] (stdin) | key <NAME> [--days N] | disable <NAME> | enable <NAME>"),
    };
    s.commit().map_err(|e| anyhow!("{e}"))?;
    Ok(out)
}
