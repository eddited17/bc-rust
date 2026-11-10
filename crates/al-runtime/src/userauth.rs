//! NavUserPassword credentials as Business Central stores them (docs/authentication.md).
//!
//! BC 29.1 (Nav.Core `CryptographyHelper`, Ncl `NavUser.TryAuthenticate`, `ALDatabase.ALSetUserPassword`):
//! - `GeneratePasswordHash(pw)` = base64(PBKDF2-HMAC-SHA256(UTF-8 pw, salt = 16 zero bytes, 1 iteration,
//!   16 bytes)) + "-V3". The web server sends this "pre-hash" to the service tier instead of the password.
//! - `GenerateSaltedPasswordHash(prehash, sid)` = base64(PBKDF2-HMAC-SHA256(UTF-8 prehash, salt =
//!   sid.ToByteArray(), 100,000 iterations, 16 bytes)) + "-V3". This is what "User Property".Password holds.
//! - The web service access key ("User Property"."WebServices Key") is compared as plain text, and the user
//!   must be enabled (State, Expiry Date) and not a Windows / Microsoft Entra group.
//!
//! The functions here are the format; who may sign in, lockout and caching are bc-server's `auth` module.

use crate::interp::Session;
use bc_store::Value;
use sha2::{Digest, Sha256};

/// PBKDF2 iterations of the stored hash (Nav.Core `RsaEncryptionProviderBase.Iterations`).
pub const ITERATIONS: u32 = 100_000;
/// Field numbers of table 2000000120 User.
pub const USER_TABLE: i64 = 2_000_000_120;
/// Field numbers of table 2000000121 "User Property".
pub const USER_PROPERTY_TABLE: i64 = 2_000_000_121;

/// .NET `Guid.ToByteArray()` of a GUID that bc-store keeps in text order (the first three groups
/// are little-endian in .NET).
pub fn dotnet_guid_bytes(g: &[u8; 16]) -> [u8; 16] {
    let mut b = *g;
    b[0..4].reverse();
    b[4..6].reverse();
    b[6..8].reverse();
    b
}

/// PBKDF2 (RFC 8018) with HMAC-SHA256; the HMAC key pads are hashed once, not per iteration.
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32, len: usize) -> Vec<u8> {
    let mut key = [0u8; 64];
    if password.len() > 64 {
        key[..32].copy_from_slice(&Sha256::digest(password));
    } else {
        key[..password.len()].copy_from_slice(password);
    }
    let pad = |x: u8| {
        let mut h = Sha256::new();
        h.update(key.map(|b| b ^ x));
        h
    };
    let (inner, outer) = (pad(0x36), pad(0x5c));
    let mac = |data: &[u8]| -> [u8; 32] {
        let mut i = inner.clone();
        i.update(data);
        let mut o = outer.clone();
        o.update(i.finalize());
        o.finalize().into()
    };
    let mut out = Vec::with_capacity(len + 32);
    let mut block = 1u32;
    while out.len() < len {
        let mut s = salt.to_vec();
        s.extend_from_slice(&block.to_be_bytes());
        let mut u = mac(&s);
        let mut t = u;
        for _ in 1..iterations.max(1) {
            u = mac(&u);
            for (x, y) in t.iter_mut().zip(u.iter()) {
                *x ^= y;
            }
        }
        out.extend_from_slice(&t);
        block += 1;
    }
    out.truncate(len);
    out
}

fn b64(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// BC's `GenerateSaltedPasswordHash(text, salt)`: the empty GUID means one iteration.
fn salted(text: &str, salt: &[u8; 16]) -> String {
    let iters = if salt == &[0u8; 16] { 1 } else { ITERATIONS };
    format!("{}-V3", b64(&pbkdf2_sha256(text.as_bytes(), &dotnet_guid_bytes(salt), iters, 16)))
}

/// `GeneratePasswordHash(password)`: what Microsoft's web server sends in place of the password.
pub fn prehash(password: &str) -> String {
    salted(password, &[0u8; 16])
}

/// The stored hash for a pre-hash (`GenerateSaltedPasswordHash(prehash, userSecurityId)`).
pub fn stored_hash_from_prehash(prehash: &str, user_sid: &[u8; 16]) -> String {
    salted(prehash, user_sid)
}

/// The stored hash of a password (`GenerateSaltedPasswordHashFromPassword`), "User Property".Password.
pub fn stored_hash(password: &str, user_sid: &[u8; 16]) -> String {
    stored_hash_from_prehash(&prehash(password), user_sid)
}

/// Equality that takes the same time wherever the first difference is.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A user's sign-in data from User and "User Property".
#[derive(Clone, Debug, Default)]
pub struct UserCreds {
    pub sid: [u8; 16],
    /// "User Name" as stored (Code, upper case).
    pub name: String,
    /// State Enabled, not expired, not a group.
    pub enabled: bool,
    pub password_hash: String,
    pub ws_key: String,
    /// "WebServices Key Expiry Date" (Unix ms), None when it doesn't expire.
    pub ws_key_expiry: Option<i64>,
    pub authentication_object_id: String,
    pub authentication_email: String,
    /// User."Application ID" (Microsoft Entra application users), lower case, "" when empty.
    pub application_id: String,
}

fn field<'a>(t: &al_model::TableDef, r: &'a [Value], name: &str) -> Option<&'a Value> {
    t.field(name).and_then(|i| r.get(i))
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

/// A DateTime value: None for 0DT.
fn datetime(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::DateTime(ms)) if *ms != bc_store::DATETIME_UNDEFINED && *ms != 0 => Some(*ms),
        _ => None,
    }
}

fn scan(s: &mut Session, table: i64) -> Vec<Vec<Value>> {
    let Some(schema) = s.rt.schemas.get(&table).cloned() else { return vec![] };
    let req = bc_store::ScanReq { key: 0, reverse: false, filters: &[], after: None, limit: usize::MAX, lock: false };
    let company = s.company.clone();
    s.db.scan(&company, &schema, &req).unwrap_or_default().into_iter().map(|r| r.to_vec()).collect()
}

/// Every user with their sign-in data (the User table is small; callers cache).
pub fn users(s: &mut Session, now_ms: i64) -> Vec<UserCreds> {
    let cat = s.cat;
    let (Some(ut), Some(pt)) = (cat.tables.get(&USER_TABLE), cat.tables.get(&USER_PROPERTY_TABLE)) else { return vec![] };
    let props = scan(s, USER_PROPERTY_TABLE);
    let mut out = vec![];
    for r in scan(s, USER_TABLE) {
        let Some(Value::Guid(sid)) = field(ut, &r, "User Security ID").cloned() else { continue };
        let state = matches!(field(ut, &r, "State"), Some(Value::Option(1) | Value::Int(1)));
        let expired = datetime(field(ut, &r, "Expiry Date")).is_some_and(|e| e <= now_ms);
        // License Type: Windows Group (3) and AAD Group (8) are not users
        let group = matches!(field(ut, &r, "License Type"), Some(Value::Option(3 | 8) | Value::Int(3 | 8)));
        let mut c = UserCreds { sid, name: text(field(ut, &r, "User Name")), enabled: !state && !expired && !group, authentication_email: text(field(ut, &r, "Authentication Email")), ..Default::default() };
        if let Some(Value::Guid(a)) = field(ut, &r, "Application ID") {
            if a != &[0u8; 16] {
                c.application_id = crate::value::format_guid(a).trim_matches(['{', '}']).to_lowercase();
            }
        }
        if let Some(p) = props.iter().find(|p| matches!(field(pt, p, "User Security ID"), Some(Value::Guid(g)) if *g == sid)) {
            c.password_hash = text(field(pt, p, "Password"));
            c.ws_key = text(field(pt, p, "WebServices Key"));
            c.ws_key_expiry = datetime(field(pt, p, "WebServices Key Expiry Date"));
            c.authentication_object_id = text(field(pt, p, "Authentication Object ID"));
        }
        out.push(c);
    }
    out
}

/// What a client presented.
pub enum Secret<'a> {
    /// The password itself (API Basic auth): matches the access key or the password.
    Password(&'a str),
    /// BC's pre-hash (Microsoft's web server, NavUserPassword).
    PreHash(&'a str),
}

/// The web service access key check of NavUser.TryAuthenticate (Basic auth only): an enabled user with an
/// unexpired, non-empty key equal to the presented password.
pub fn verify_key(u: &UserCreds, password: &str, now_ms: i64) -> bool {
    u.enabled && !password.is_empty() && !u.ws_key.is_empty() && u.ws_key_expiry.is_none_or(|e| e > now_ms) && ct_eq(u.ws_key.as_bytes(), password.as_bytes())
}

/// The password check of NavUser.TryAuthenticate on a pre-hash (the slow part: 100,000 PBKDF2 rounds).
/// Users without a password never match.
pub fn verify_prehash(u: &UserCreds, prehash: &str) -> bool {
    u.enabled && !u.password_hash.is_empty() && ct_eq(stored_hash_from_prehash(prehash, &u.sid).as_bytes(), u.password_hash.as_bytes())
}

/// NavUser.TryAuthenticate for an enabled user: the web service access key (Basic auth only, unexpired),
/// else the salted password hash.
pub fn verify(u: &UserCreds, secret: &Secret, now_ms: i64) -> bool {
    match secret {
        Secret::Password(p) => verify_key(u, p, now_ms) || verify_prehash(u, &prehash(p)),
        Secret::PreHash(h) => verify_prehash(u, h),
    }
}

/// The "User Property" record of a user (inserted when missing).
fn user_property(s: &mut Session, sid: &[u8; 16]) -> Result<crate::record::RecHandle, String> {
    let h = s.new_record(USER_PROPERTY_TABLE, false);
    let fi = h.borrow().tdef.field("User Security ID").ok_or("User Property has no User Security ID")?;
    h.borrow_mut().row[fi] = Value::Guid(*sid);
    match s.rec_get_pk(&h) {
        Some(row) => h.borrow_mut().row = row.into(),
        None => {
            if let Err(m) = s.rec_insert(&h, false).map_err(|e| flow_msg(e))? {
                return Err(m);
            }
        }
    }
    Ok(h)
}

fn flow_msg(e: crate::interp::Flow) -> String {
    match e {
        crate::interp::Flow::Err(e) => e.msg,
        _ => "unexpected control flow".into(),
    }
}

fn set_field(h: &crate::record::RecHandle, name: &str, v: Value) {
    let fi = h.borrow().tdef.field(name);
    if let Some(fi) = fi {
        h.borrow_mut().row[fi] = v;
    }
}

/// The session user's security id: User."User Name" = UserId().
pub fn session_user_sid(s: &mut Session) -> Option<[u8; 16]> {
    let me = s.user_id.clone();
    users(s, 0).into_iter().find(|u| u.name.eq_ignore_ascii_case(&me)).map(|u| u.sid)
}

/// Run session `s` as user `name`: UserId() and UserSecurityId() (the platform user's id when the User
/// table has no such user). Background sessions take their user this way.
pub fn set_session_user(s: &mut Session, name: &str) {
    s.user_id = name.to_string();
    s.user_sid = session_user_sid(s).unwrap_or(crate::platform::USER_SID);
}

/// How long `resolve_session_user` trusts a user's id it read before.
const USER_SID_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// UserSecurityId() of a session nobody authenticated (BC_AUTH off; background and child sessions): the
/// User row named UserId(), else the platform user's id. Found ids are cached on the Runtime for a minute,
/// since every API request is a session; a missing user is looked up again next time.
pub fn resolve_session_user(s: &mut Session) {
    let key = s.user_id.to_uppercase();
    if let Some((sid, at)) = s.rt.user_sids.lock().unwrap().get(&key) {
        if at.elapsed() < USER_SID_TTL {
            s.user_sid = *sid;
            return;
        }
    }
    let cat = s.cat;
    let found = cat.tables.get(&USER_TABLE).and_then(|ut| {
        scan(s, USER_TABLE).iter().find(|r| text(field(ut, r, "User Name")).eq_ignore_ascii_case(&key)).and_then(|r| match field(ut, r, "User Security ID") {
            Some(Value::Guid(g)) => Some(*g),
            _ => None,
        })
    });
    match found {
        Some(sid) => {
            s.rt.user_sids.lock().unwrap().insert(key, (sid, std::time::Instant::now()));
            s.user_sid = sid;
        }
        None => s.user_sid = crate::platform::USER_SID,
    }
}

/// The credential type the service authenticates with (BC's ClientServicesCredentialType), for
/// NavUserAccountHelper.IsUserNamePasswordAuthentication / IsAccessControlServiceAuthentication.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CredentialType {
    /// BC's default; also bc-rust without BC_AUTH (no password checks).
    Windows = 0,
    NavUserPassword = 1,
    AccessControlService = 2,
}

static CREDENTIAL_TYPE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Set by bc-server's authentication from `BC_AUTH`.
pub fn set_credential_type(t: CredentialType) {
    CREDENTIAL_TYPE.store(t as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn credential_type() -> CredentialType {
    match CREDENTIAL_TYPE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => CredentialType::NavUserPassword,
        2 => CredentialType::AccessControlService,
        _ => CredentialType::Windows,
    }
}

/// Role IDs of a user's System-scope Access Control rows (any company).
fn system_roles(s: &mut Session, sid: &[u8; 16]) -> Vec<String> {
    let Some(ac) = s.cat.table_by_name("Access Control") else { return vec![] };
    let rows = scan(s, ac.id);
    rows.iter()
        .filter(|r| matches!(field(ac, r, "User Security ID"), Some(Value::Guid(g)) if g == sid))
        .filter(|r| !matches!(field(ac, r, "Scope"), Some(Value::Option(1) | Value::Int(1))))
        .map(|r| text(field(ac, r, "Role ID")).to_uppercase())
        .collect()
}

/// PermissionManagement.SessionHasSuperOrSecurityPermissionsForUser, plus "it is my own user": the
/// session's own user, a session user with SUPER, or one with SECURITY when the target is not SUPER.
/// While the permission system is off (BC_PERMISSIONS not enforce, or no users) everyone is SUPER.
pub fn may_manage_user(s: &mut Session, target: &[u8; 16]) -> bool {
    if !crate::permissions::enforce() {
        return true;
    }
    let all = users(s, 0);
    if all.is_empty() {
        return true;
    }
    let me = s.user_id.clone();
    let Some(my) = all.iter().find(|u| u.name.eq_ignore_ascii_case(&me)).map(|u| u.sid) else { return false };
    if &my == target {
        return true;
    }
    let mine = system_roles(s, &my);
    if mine.iter().any(|r| r == "SUPER") {
        return true;
    }
    mine.iter().any(|r| r == "SECURITY") && !system_roles(s, target).iter().any(|r| r == "SUPER")
}

/// Store a user's password in BC's format; `change_at_next_sign_in` is User."Change Password", which
/// the Base App's LogInManagement turns into the Change Password dialog at the next web client sign-in.
/// No permission check: AL goes through `al_set_user_password`, operators through `bc user`.
pub fn set_user_password(s: &mut Session, sid: &[u8; 16], password: &str, change_at_next_sign_in: bool) -> Result<(), String> {
    let h = user_property(s, sid)?;
    set_field(&h, "Password", Value::Text(stored_hash(password, sid)));
    s.rec_modify(&h, false).map_err(flow_msg)??;
    let u = s.new_record(USER_TABLE, false);
    set_field(&u, "User Security ID", Value::Guid(*sid));
    if let Some(row) = s.rec_get_pk(&u) {
        u.borrow_mut().row = row.into();
        set_field(&u, "Change Password", Value::Bool(change_at_next_sign_in));
        s.rec_modify(&u, false).map_err(flow_msg)??;
    }
    Ok(())
}

/// AL `SetUserPassword(UserSecurityId, Password)` (ALDatabase.ALSetUserPassword): only for the session's
/// own user or by SUPER / SECURITY (`may_manage_user`); stores the hash and sets User."Change Password",
/// as BC does.
pub fn al_set_user_password(s: &mut Session, sid: &[u8; 16], password: &str) -> Result<(), String> {
    if !may_manage_user(s, sid) {
        return Err(NO_SECURITY_PERMISSION.into());
    }
    set_user_password(s, sid, password, true)
}

/// bc-rust's message when a user without SUPER or SECURITY manages another user's credentials.
pub const NO_SECURITY_PERMISSION: &str = "You need the SUPER or SECURITY permission set to manage the credentials of another user.";

/// AL `ChangeUserPassword(OldPassword, NewPassword)` for the session user: Err(None) = the old password
/// is wrong (NavNCLWrongPasswordException). Clears User."Change Password".
pub fn change_user_password(s: &mut Session, old: &str, new: &str) -> Result<(), Option<String>> {
    let sid = session_user_sid(s).ok_or_else(|| Some(format!("The user {} does not exist.", s.user_id)))?;
    let h = user_property(s, &sid).map_err(Some)?;
    let fi = h.borrow().tdef.field("Password");
    let cur = fi.map(|i| text(h.borrow().row.get(i))).unwrap_or_default();
    if !ct_eq(cur.as_bytes(), stored_hash(old, &sid).as_bytes()) {
        return Err(None);
    }
    set_user_password(s, &sid, new, false).map_err(Some)
}

/// A new web service access key as BC makes it (UserManagement.CreateWebServicesKey): base64 of
/// HMAC-SHA256 under a random key over a random GUID, 44 characters. Always from the OS RNG, also under
/// al-test's virtual clock.
pub fn new_access_key() -> String {
    let key = uuid::Uuid::new_v4();
    let msg = uuid::Uuid::new_v4();
    let mut k = [0u8; 64];
    k[..16].copy_from_slice(key.as_bytes());
    let mut i = Sha256::new();
    i.update(k.map(|b| b ^ 0x36));
    i.update(msg.as_bytes());
    let mut o = Sha256::new();
    o.update(k.map(|b| b ^ 0x5c));
    o.update(i.finalize());
    b64(&o.finalize())
}

/// Store the Entra object id of a user ("User Property"."Authentication Object ID", first sign-in).
pub fn set_object_id(s: &mut Session, sid: &[u8; 16], oid: &str) -> Result<(), String> {
    let h = user_property(s, sid)?;
    set_field(&h, "Authentication Object ID", Value::Text(oid.to_string()));
    s.rec_modify(&h, false).map_err(flow_msg)?
}

/// Store (or clear, with None) a user's web service access key and its expiry (None = no expiry).
pub fn set_access_key(s: &mut Session, sid: &[u8; 16], key: Option<(&str, Option<i64>)>) -> Result<(), String> {
    let h = user_property(s, sid)?;
    set_field(&h, "WebServices Key", Value::Text(key.map(|k| k.0.to_string()).unwrap_or_default()));
    set_field(&h, "WebServices Key Expiry Date", Value::DateTime(key.and_then(|k| k.1).unwrap_or(bc_store::DATETIME_UNDEFINED)));
    s.rec_modify(&h, false).map_err(flow_msg)?
}

impl<'r> Session<'r> {
    /// NavUserAccountHelper members that need the session (crate::dotnet dispatch): the credential type,
    /// and the web service access key helpers (UserManagement.Create/Get/ClearWebServicesKey: the
    /// session's own user or SUPER / SECURITY; Try* return false and set the last error otherwise).
    /// None: not a member handled here.
    pub fn user_account_call(&mut self, f: &mut crate::interp::Frame, m: &str, a: &crate::dotnet::Args) -> crate::interp::R<Option<crate::value::Val>> {
        use crate::value::Val;
        let sid = |i: usize| match a.get(i) {
            Val::Guid(g) => g,
            _ => [0; 16],
        };
        let fail = |s: &mut Session, msg: &str| -> crate::interp::R<Option<Val>> {
            s.last_error = msg.to_string();
            s.last_error_code.clear();
            Ok(Some(Val::Bool(false)))
        };
        Ok(Some(match m {
            "isusernamepasswordauthentication" => Val::Bool(credential_type() == CredentialType::NavUserPassword),
            "isaccesscontrolserviceauthentication" => Val::Bool(credential_type() == CredentialType::AccessControlService),
            "trycreatewebserviceskey" => {
                let target = sid(0);
                if !may_manage_user(self, &target) {
                    return fail(self, NO_SECURITY_PERMISSION);
                }
                let expiry = match a.get(1) {
                    Val::DateTime(ms) if ms != bc_store::DATETIME_UNDEFINED && ms != 0 => Some(ms),
                    _ => None,
                };
                let key = new_access_key();
                if let Err(e) = set_access_key(self, &target, Some((&key, expiry))) {
                    return fail(self, &e);
                }
                self.net_out(f, a, 2, Val::Text(key))?;
                Val::Bool(true)
            }
            "tryclearwebserviceskey" => {
                let target = sid(0);
                if !may_manage_user(self, &target) {
                    return fail(self, NO_SECURITY_PERMISSION);
                }
                if let Err(e) = set_access_key(self, &target, None) {
                    return fail(self, &e);
                }
                Val::Bool(true)
            }
            "trygetwebserviceskey" => {
                let target = sid(0);
                if !may_manage_user(self, &target) {
                    return fail(self, NO_SECURITY_PERMISSION);
                }
                let Some(u) = users(self, 0).into_iter().find(|u| u.sid == target) else { return fail(self, "The user does not exist.") };
                self.net_out(f, a, 1, Val::Text(u.ws_key))?;
                self.net_out(f, a, 2, Val::DateTime(u.ws_key_expiry.unwrap_or(bc_store::DATETIME_UNDEFINED)))?;
                Val::Bool(true)
            }
            _ => return Ok(None),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guid(s: &str) -> [u8; 16] {
        crate::value::parse_guid(s).unwrap()
    }

    // Vectors from BC 29.1's own Microsoft.Dynamics.Nav.Core.dll CryptographyHelper (run under .NET 10).
    #[test]
    fn bc_v3_vectors() {
        let sid = guid("01234567-89ab-cdef-0123-456789abcdef");
        assert_eq!(dotnet_guid_bytes(&sid), [0x67, 0x45, 0x23, 0x01, 0xAB, 0x89, 0xEF, 0xCD, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]);
        assert_eq!(prehash("Passw0rd!"), "z9Ecv6HC8K9I51R+uOrvRQ==-V3");
        assert_eq!(stored_hash("Passw0rd!", &sid), "onx5VWbixN7uvRbD6KSyIg==-V3");
        assert_eq!(prehash(""), "yMLvk2JPU/dGrVZkRB+eBA==-V3");
        assert_eq!(stored_hash("", &sid), "45/HdFCJkY6M7CA8/gUudg==-V3");
        let sid2 = guid("a1b2c3d4-e5f6-4711-8899-aabbccddeeff");
        assert_eq!(prehash("Grüße – ünïcödé €"), "2eXvbi9IjFEYa+R4D2+9gA==-V3");
        assert_eq!(stored_hash("Grüße – ünïcödé €", &sid2), "9yzR8Dx5UYbKGLCeQ6DeXw==-V3");
    }

    #[test]
    fn pbkdf2_rfc7914_vector() {
        // RFC 7914 section 11: PBKDF2-HMAC-SHA256 ("passwd", "salt", 1, 64)
        let h: String = pbkdf2_sha256(b"passwd", b"salt", 1, 64).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(&h[..32], "55ac046e56e3089fec1691c22544b605");
        // a key longer than the block is hashed first
        assert_eq!(pbkdf2_sha256(&[7u8; 100], b"s", 2, 16), pbkdf2_sha256(&Sha256::digest([7u8; 100]), b"s", 2, 16));
    }

    #[test]
    fn access_keys_and_credential_type() {
        let k = new_access_key();
        assert_eq!(k.len(), 44);
        assert_ne!(k, new_access_key());
        assert_eq!(credential_type(), CredentialType::Windows);
    }

    #[test]
    fn verify_rules() {
        let sid = guid("01234567-89ab-cdef-0123-456789abcdef");
        let mut u = UserCreds { sid, name: "CLERK".into(), enabled: true, password_hash: stored_hash("Passw0rd!", &sid), ws_key: "K3y+/==".into(), ..Default::default() };
        assert!(verify(&u, &Secret::Password("Passw0rd!"), 0));
        assert!(verify(&u, &Secret::PreHash("z9Ecv6HC8K9I51R+uOrvRQ==-V3"), 0));
        assert!(!verify(&u, &Secret::Password("passw0rd!"), 0));
        assert!(!verify(&u, &Secret::PreHash("Passw0rd!"), 0));
        assert!(verify(&u, &Secret::Password("K3y+/=="), 0));
        // the access key is not accepted as a pre-hash (web client), and expires
        assert!(!verify(&u, &Secret::PreHash("K3y+/=="), 0));
        u.ws_key_expiry = Some(1000);
        assert!(verify(&u, &Secret::Password("K3y+/=="), 999));
        assert!(!verify(&u, &Secret::Password("K3y+/=="), 1000));
        u.enabled = false;
        assert!(!verify(&u, &Secret::Password("Passw0rd!"), 0));
        // no password set: nothing matches, not even the empty password
        let v = UserCreds { sid, enabled: true, ..Default::default() };
        assert!(!verify(&v, &Secret::Password(""), 0));
        assert!(!verify(&v, &Secret::PreHash(&prehash("")), 0));
    }
}
