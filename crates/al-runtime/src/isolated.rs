//! IsolatedStorage with BC's semantics (Ncl ALIsolatedStorage over IsolatedStorageRepository): one
//! row per value in system table 2000000107 "Isolated Storage" (platform/system/System.Security.al),
//! keyed by (App Id, Scope, Company Name, User Id, Key).
//!
//! - App Id: the app that owns the code object running the call (crate::modules), so every app sees
//!   only its own values. Company Name only for the Company and CompanyAndUser scopes, User Id
//!   (the session's UserSecurityId()) only for User and CompanyAndUser; both are blank otherwise.
//! - The rows are written through the session's record operations: they belong to the transaction
//!   and roll back with it. No AL triggers and no permission checks (BC opens a
//!   MaximizedPermissionScope on the table).
//! - A call whose return value is not used fails on what would otherwise return false (BC's
//!   DataError.ThrowError): Get or Delete of a missing key, a failed insert.
//! - Keys longer than 200 characters are an error.
//! - `Value Type`: a value set from a SecretText is SecretText; Get into a Text variable is refused.
//! - `SetEncrypted`: BC encrypts with the tenant's RSA key (ALSystemEncryption). bc-rust encrypts at
//!   rest with AES-256-CBC + HMAC-SHA256 (encrypt-then-MAC, random IV per value) under a server key
//!   from `BC_ISOLATED_STORAGE_KEY` (64 hex characters) or the file named by
//!   `BC_ISOLATED_STORAGE_KEY_FILE` (one such line). Without a key, SetEncrypted and Get of an
//!   encrypted value fail. al-test, demo-data and tests call [`use_test_key`] instead, a fixed key
//!   that protects nothing. BC's 215-byte limit of SetEncrypted (RSA-2048 OAEP) is kept, because AL
//!   code relies on it.

use crate::interp::*;
use crate::value::*;
use al_syntax::{Expr, TypeRef};
use base64::Engine;
use bc_store::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// Table "Isolated Storage".
pub const TABLE: i64 = 2000000107;
const KEY_LENGTH: usize = 200;
const F_APP: i64 = 1;
const F_SCOPE: i64 = 2;
const F_COMPANY: i64 = 3;
const F_USER: i64 = 4;
const F_KEY: i64 = 5;
const F_VALUE: i64 = 6;
const F_ENCRYPTION: i64 = 7;
const F_VALUE_TYPE: i64 = 8;

/// DataScope (Ncl DataScope) = the table's Scope option.
const MODULE: i32 = 0;
const COMPANY: i32 = 1;
const USER: i32 = 2;
const COMPANY_AND_USER: i32 = 3;
/// "Encryption Status": PlainText, Encrypted, PendingForEncryption.
const PLAIN: i32 = 0;
const ENCRYPTED: i32 = 1;
/// "Target Value Type": Text, SecretText.
const TEXT: i32 = 0;
const SECRET: i32 = 1;

// own wording [src:X14] [src:X16]
const NO_KEY: &str = "This operation needs an encryption key, and none has been created.";
const TOO_LONG: &str = "The text is too long to be encrypted.";
const KEY_TOO_LONG: &str = "The key exceeds the maximum length of 200 characters.";
const FORBIDDEN_SECRET: &str = "The value is stored as a SecretText and cannot be retrieved into a Text variable.";
const BAD_DATA: &str = "The data cannot be decrypted with the current encryption key.";
/// Marks bc-rust's at-rest format in the Value BLOB: "bcis1:" + base64(iv | ciphertext | tag).
const SEALED: &str = "bcis1:";
const ENC_INFO: &[u8] = b"bc-rust isolated storage v1: AES-256-CBC";
const MAC_INFO: &[u8] = b"bc-rust isolated storage v1: HMAC-SHA256";

/// A fixed key for al-test, demo-data and cargo tests only. It is public in the source, so it
/// protects nothing; servers refuse SetEncrypted without a configured key instead.
const TEST_ONLY_KEY: [u8; 32] = *b"TEST-ONLY bc-rust isolated key!!";
static TEST_KEY: AtomicBool = AtomicBool::new(false);

/// Use the fixed test-only key when neither `BC_ISOLATED_STORAGE_KEY` nor
/// `BC_ISOLATED_STORAGE_KEY_FILE` is set. For al-test, demo-data and tests, never for a server.
pub fn use_test_key() {
    TEST_KEY.store(true, Ordering::Relaxed);
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// The configured key (read once): `Ok(None)` when none is configured.
fn configured_key() -> Result<Option<[u8; 32]>, String> {
    static KEY: OnceLock<Result<Option<[u8; 32]>, String>> = OnceLock::new();
    KEY.get_or_init(|| {
        if let Some(v) = std::env::var("BC_ISOLATED_STORAGE_KEY").ok().filter(|v| !v.trim().is_empty()) {
            return parse_hex32(&v).map(Some).ok_or_else(|| "BC_ISOLATED_STORAGE_KEY must be 64 hexadecimal characters (a 32-byte key).".to_string());
        }
        if let Some(p) = std::env::var("BC_ISOLATED_STORAGE_KEY_FILE").ok().filter(|v| !v.trim().is_empty()) {
            let text = std::fs::read_to_string(&p).map_err(|e| format!("BC_ISOLATED_STORAGE_KEY_FILE {p}: {e}"))?;
            return parse_hex32(&text).map(Some).ok_or_else(|| format!("BC_ISOLATED_STORAGE_KEY_FILE {p}: the file must hold 64 hexadecimal characters (a 32-byte key)."));
        }
        Ok(None)
    })
    .clone()
}

/// The key for encrypted values: the configured one, else the test-only key where allowed.
fn at_rest_key() -> Result<[u8; 32], String> {
    match configured_key()? {
        Some(k) => Ok(k),
        None if TEST_KEY.load(Ordering::Relaxed) => Ok(TEST_ONLY_KEY),
        None => Err(NO_KEY.into()),
    }
}

fn sym(key: Vec<u8>, iv: Vec<u8>) -> crate::dotnet::cipher::Sym {
    crate::dotnet::cipher::Sym { ty: "System.Security.Cryptography.Aes", key, iv, mode: crate::dotnet::cipher::CBC, padding: crate::dotnet::cipher::PKCS7, feedback: 16 }
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// Encrypt `plain` for the Value BLOB ('' stays '', as BC's Encrypt keeps it).
pub fn seal(master: &[u8; 32], plain: &str) -> Result<String, String> {
    use ::rsa::rand_core::RngCore;
    if plain.is_empty() {
        return Ok(String::new());
    }
    let enc = crate::dotnet::crypto::hmac("SHA256", master, ENC_INFO);
    let mac = crate::dotnet::crypto::hmac("SHA256", master, MAC_INFO);
    let mut iv = [0u8; 16];
    ::rsa::rand_core::OsRng.fill_bytes(&mut iv);
    let ct = crate::dotnet::cipher::transform(&sym(enc, iv.to_vec()), true, plain.as_bytes())?;
    let mut out = iv.to_vec();
    out.extend_from_slice(&ct);
    let tag = crate::dotnet::crypto::hmac("SHA256", &mac, &out);
    out.extend_from_slice(&tag);
    Ok(format!("{SEALED}{}", b64().encode(out)))
}

/// Decrypt what [`seal`] made; a wrong key or changed data is BC's "different key" error.
pub fn open(master: &[u8; 32], stored: &str) -> Result<String, String> {
    if stored.is_empty() {
        return Ok(String::new());
    }
    let data = stored.strip_prefix(SEALED).and_then(|b| b64().decode(b.trim()).ok()).ok_or(BAD_DATA)?;
    if data.len() < 16 + 16 + 32 {
        return Err(BAD_DATA.into());
    }
    let (body, tag) = data.split_at(data.len() - 32);
    let mac = crate::dotnet::crypto::hmac("SHA256", master, MAC_INFO);
    if !crate::userauth::ct_eq(&crate::dotnet::crypto::hmac("SHA256", &mac, body), tag) {
        return Err(BAD_DATA.into());
    }
    let enc = crate::dotnet::crypto::hmac("SHA256", master, ENC_INFO);
    let plain = crate::dotnet::cipher::transform(&sym(enc, body[..16].to_vec()), false, &body[16..]).map_err(|_| BAD_DATA.to_string())?;
    String::from_utf8(plain).map_err(|_| BAD_DATA.to_string())
}

/// A DataScope argument: the ordinal (interp::system_option_ord "datascope"), Module by default.
fn scope_of(v: &Val) -> i32 {
    let ord = match v {
        Val::Opt { ord, .. } => *ord,
        Val::Int(i) => *i,
        _ => return MODULE,
    };
    if (MODULE..=COMPANY_AND_USER).contains(&ord) {
        return ord;
    }
    ["module", "company", "user", "companyanduser"].iter().position(|m| system_option_ord("datascope", m) == ord).map(|i| i as i32).unwrap_or(MODULE)
}

impl<'r> Session<'r> {
    /// The declared type name of an argument (lower case) where the call site shows it: a variable
    /// (its declaration) or a `Type::Member` literal.
    fn iso_arg_type(&mut self, f: &mut Frame, e: &Expr) -> Option<String> {
        match e {
            Expr::Scope(b, _) => match &**b {
                Expr::Ident(n) => Some(n.to_ascii_lowercase()),
                _ => None,
            },
            Expr::Ident(_) => match self.lvalue(f, e).ok()? {
                LVal::Slot(_, Some(TypeRef::Simple { name, .. })) | LVal::Elem(_, _, Some(TypeRef::Simple { name, .. })) => Some(name.to_ascii_lowercase()),
                _ => None,
            },
            _ => None,
        }
    }

    /// The "Isolated Storage" record of `key` in `scope` for the running code's app.
    fn iso_record(&mut self, f: &Frame, scope: i32, key: &str) -> R<crate::record::RecHandle> {
        if key.chars().count() > KEY_LENGTH {
            return err(KEY_TOO_LONG);
        }
        if self.cat.tables.get(&TABLE).is_none() {
            return err("Not implemented in bc-rust: IsolatedStorage without the platform table 2000000107 \"Isolated Storage\"");
        }
        let mods = crate::modules::modules(self.cat);
        let app = mods.app_of_code(f.code).map(|a| a.id).unwrap_or([0; 16]);
        let company = if matches!(scope, COMPANY | COMPANY_AND_USER) { self.company.clone() } else { String::new() };
        let user = if matches!(scope, USER | COMPANY_AND_USER) { self.user_sid } else { [0; 16] };
        let h = self.new_record(TABLE, false);
        {
            let mut r = h.borrow_mut();
            let t = r.tdef;
            for (no, v) in [(F_APP, Value::Guid(app)), (F_SCOPE, Value::Option(scope)), (F_COMPANY, Value::Text(company)), (F_USER, Value::Guid(user)), (F_KEY, Value::Text(key.to_string()))] {
                if let Some(i) = t.field_no(no) {
                    r.row[i] = v;
                }
            }
        }
        Ok(h)
    }

    fn iso_set_field(h: &crate::record::RecHandle, no: i64, v: Value) {
        let mut r = h.borrow_mut();
        if let Some(i) = r.tdef.field_no(no) {
            r.row[i] = v;
        }
    }

    fn iso_field(h: &crate::record::RecHandle, no: i64) -> Value {
        let r = h.borrow();
        r.tdef.field_no(no).map(|i| r.row[i].clone()).unwrap_or(Value::Int(0))
    }

    /// `IsolatedStorage.Set / SetEncrypted / Get / Contains / Delete` (`m` lower-cased). `discard`:
    /// the return value is not used (BC's DataError.ThrowError).
    pub fn isolated_storage(&mut self, f: &mut Frame, m: &str, args: &[Expr], discard: bool) -> R<Val> {
        let fail = |discard: bool, msg: String| -> R<Val> {
            if discard {
                err(msg)
            } else {
                Ok(Val::Bool(false))
            }
        };
        let Some(key_e) = args.first() else { return err(format!("IsolatedStorage.{m} needs a key")) };
        let key = self.eval(f, key_e)?.to_text();
        match m {
            // Set(Key, Value [, DataScope]) / SetEncrypted(Key, Value [, DataScope])
            "set" | "setencrypted" => {
                let Some(val_e) = args.get(1) else { return err(format!("IsolatedStorage.{m} needs a value")) };
                let secret = self.iso_arg_type(f, val_e).as_deref() == Some("secrettext");
                let value = self.eval(f, val_e)?.to_text();
                let scope = match args.get(2) {
                    Some(e) => scope_of(&self.eval(f, e)?),
                    None => MODULE,
                };
                // BC encrypts first (the argument of its Set), then checks the key
                let (stored, status) = if m == "setencrypted" {
                    if value.len() > 215 {
                        return err(TOO_LONG);
                    }
                    let k = match at_rest_key() {
                        Ok(k) => k,
                        Err(e) => return err(e),
                    };
                    match seal(&k, &value) {
                        Ok(s) => (s, ENCRYPTED),
                        Err(e) => return err(e),
                    }
                } else {
                    (value, PLAIN)
                };
                let h = self.iso_record(f, scope, &key)?;
                let found = self.rec_get_pk_r(&h)?;
                let exists = found.is_some();
                if let Some(row) = found {
                    h.borrow_mut().row = row;
                }
                Self::iso_set_field(&h, F_VALUE, Value::Blob(stored.into_bytes()));
                Self::iso_set_field(&h, F_ENCRYPTION, Value::Option(status));
                Self::iso_set_field(&h, F_VALUE_TYPE, Value::Option(if secret { SECRET } else { TEXT }));
                let res = if exists { self.rec_modify(&h, false)? } else { self.rec_insert(&h, false)? };
                match res {
                    Ok(()) => Ok(Val::Bool(true)),
                    Err(msg) => fail(discard, msg),
                }
            }
            // Get(Key, [DataScope,] [IsolationLevel,] var Value)
            "get" => {
                let Some(target_e) = args.last().filter(|_| args.len() >= 2) else { return err("IsolatedStorage.Get needs a variable for the value") };
                let mut scope = MODULE;
                for e in &args[1..args.len() - 1] {
                    // the 3-argument form takes a DataScope or an IsolationLevel: tell them apart by type
                    let ty = self.iso_arg_type(f, e);
                    let v = self.eval(f, e)?;
                    if ty.as_deref() != Some("isolationlevel") {
                        scope = scope_of(&v);
                    }
                }
                let target = self.lvalue(f, target_e)?;
                let into_secret = matches!(&target, LVal::Slot(_, Some(TypeRef::Simple { name, .. })) | LVal::Elem(_, _, Some(TypeRef::Simple { name, .. })) if name.eq_ignore_ascii_case("secrettext"));
                let h = self.iso_record(f, scope, &key)?;
                let Some(row) = self.rec_get_pk_r(&h)? else {
                    if discard {
                        return err(self.iso_not_found(&h));
                    }
                    self.write_lval(&target, Val::Text(String::new()))?;
                    return Ok(Val::Bool(false));
                };
                h.borrow_mut().row = row;
                if matches!(Self::iso_field(&h, F_VALUE_TYPE), Value::Option(SECRET)) && !into_secret {
                    return err(FORBIDDEN_SECRET);
                }
                let raw = match Self::iso_field(&h, F_VALUE) {
                    Value::Blob(b) => String::from_utf8_lossy(&b).into_owned(),
                    _ => String::new(),
                };
                let text = if matches!(Self::iso_field(&h, F_ENCRYPTION), Value::Option(ENCRYPTED)) {
                    let opened = at_rest_key().and_then(|k| open(&k, &raw));
                    match opened {
                        Ok(t) => t,
                        Err(e) => {
                            self.write_lval(&target, Val::Text(String::new()))?;
                            return err(e);
                        }
                    }
                } else {
                    raw
                };
                self.write_lval(&target, Val::Text(text))?;
                Ok(Val::Bool(true))
            }
            // Contains(Key [, DataScope] [, var IsSecret])
            "contains" => {
                let scope = match args.get(1) {
                    Some(e) => scope_of(&self.eval(f, e)?),
                    None => MODULE,
                };
                let h = self.iso_record(f, scope, &key)?;
                let row = self.rec_get_pk_r(&h)?;
                let found = row.is_some();
                if let Some(e) = args.get(2) {
                    let secret = row.is_some_and(|r| {
                        h.borrow_mut().row = r;
                        matches!(Self::iso_field(&h, F_VALUE_TYPE), Value::Option(SECRET))
                    });
                    let lv = self.lvalue(f, e)?;
                    self.write_lval(&lv, Val::Bool(secret))?;
                }
                Ok(Val::Bool(found))
            }
            // Delete(Key [, DataScope])
            "delete" => {
                let scope = match args.get(1) {
                    Some(e) => scope_of(&self.eval(f, e)?),
                    None => MODULE,
                };
                let h = self.iso_record(f, scope, &key)?;
                match self.rec_delete(&h, false)? {
                    Ok(()) => Ok(Val::Bool(true)),
                    Err(msg) => fail(discard, msg),
                }
            }
            _ => err(format!("Not implemented in bc-rust: IsolatedStorage.{m}")),
        }
    }

    fn iso_not_found(&self, h: &crate::record::RecHandle) -> String {
        let r = h.borrow();
        format!("The {} does not exist. Identification fields and values: {}", r.tdef.caption, self.key_text(&r))
    }
}
