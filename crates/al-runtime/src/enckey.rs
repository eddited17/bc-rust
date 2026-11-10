//! Data encryption system functions with BC's semantics (Ncl ALSystemEncryption over
//! Nav.Core RsaEncryptionProviderBase): one RSA-2048 key per tenant, held in the session state.
//!
//! - `CreateEncryptionKey()`: generates the key; error if one exists already.
//! - `DeleteEncryptionKey()`: forgets the key (and the stored key hash, so another key can be
//!   imported).
//! - `EncryptionEnabled()` / `EncryptionKeyExists()`: whether the key exists (bc-rust has no key
//!   file that can go missing, so both agree).
//! - `Encrypt(Text)` / `Decrypt(Text)`: '' stays ''; otherwise RSA OAEP-SHA1 over the UTF-8 text,
//!   as Base64. More than 215 bytes is "too long"; 215 itself passes the check but is beyond what
//!   OAEP-SHA1 fits in 2048 bits, which BC reports as "Unknown error".
//! - `ExportEncryptionKey(Password)`: writes the key to a new server temp file and returns its
//!   name: the private-key XML, AES-256-CBC encrypted with a PBKDF2-SHA256 (100,000 iterations)
//!   key when a password is given. The fixed salt is bc-rust's own, so key files only round-trip
//!   within bc-rust.
//! - `ImportEncryptionKey(FileName, Password)`: reads such a file. A key different from the one
//!   created or imported before (and not deleted since) is refused.

use crate::dotnet::asym::{self, Key, RsaState};
use crate::interp::*;
use crate::value::*;
use al_syntax::Expr;
use base64::Engine;

// own wording [src:X14] [src:X15]
const NOT_CREATED: &str = "This operation needs an encryption key, and none has been created.";
const ALREADY_CREATED: &str = "An encryption key exists already; delete it before creating a new one.";
const TOO_LONG: &str = "The text is too long to be encrypted.";
const BAD_DATA: &str = "The data cannot be decrypted with the current encryption key.";
const BAD_FILE_OR_PASSWORD: &str = "The key file could not be decrypted: check the password and that the file is unchanged.";
const BAD_FILE: &str = "The key file holds no valid encryption key.";
const OTHER_KEY: &str = "Another encryption key is registered already; delete it before importing a different one.";

const SALT: [u8; 64] = *b"bc-rust tenant encryption key export salt (PBKDF2, AES-256-CBC).";
const ITERATIONS: u32 = 100_000;

/// The tenant's encryption key.
#[derive(Default)]
pub struct EncState {
    key: Option<Box<::rsa::RsaPrivateKey>>,
    /// Public-key XML of the key created or imported (BC's StoredEncryptionKeyHash).
    stored: Option<String>,
}

pub fn is_enc_builtin(l: &str) -> bool {
    matches!(
        l,
        "createencryptionkey" | "deleteencryptionkey" | "encryptionenabled" | "encryptionkeyexists" | "encrypt" | "decrypt" | "exportencryptionkey" | "importencryptionkey"
    )
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn xml_of(key: &::rsa::RsaPrivateKey, private: bool) -> String {
    let mut s = RsaState { ty: asym::RSA, key: Key::Private(Box::new(key.clone())) };
    asym::to_xml(&mut s, private).unwrap_or_default()
}

fn aes(password: &str) -> crate::dotnet::cipher::Sym {
    crate::dotnet::cipher::Sym {
        ty: "System.Security.Cryptography.Aes",
        key: crate::dotnet::crypto::pbkdf2("SHA256", password.as_bytes(), &SALT, ITERATIONS, 32),
        iv: SALT[..16].to_vec(),
        mode: crate::dotnet::cipher::CBC,
        padding: crate::dotnet::cipher::PKCS7,
        feedback: 16,
    }
}

impl<'r> Session<'r> {
    /// One of the functions of [`is_enc_builtin`] (`l` lower-cased).
    pub fn enc_builtin(&mut self, f: &mut Frame, l: &str, args: &[Expr]) -> R<Val> {
        let mut a = Vec::with_capacity(args.len());
        for e in args {
            a.push(self.eval(f, e)?);
        }
        let text = |i: usize| a.get(i).map(|v| v.to_text()).unwrap_or_default();
        match l {
            "encryptionenabled" | "encryptionkeyexists" => Ok(Val::Bool(self.sys.enc.key.is_some())),
            "createencryptionkey" => {
                if self.sys.enc.key.is_some() {
                    return err(ALREADY_CREATED);
                }
                let key = ::rsa::RsaPrivateKey::new(&mut ::rsa::rand_core::OsRng, 2048).map_err(|e| e.to_string());
                let key = match key {
                    Ok(k) => k,
                    Err(e) => return err(e),
                };
                self.sys.enc.stored = Some(xml_of(&key, false));
                self.sys.enc.key = Some(Box::new(key));
                Ok(Val::Bool(true))
            }
            "deleteencryptionkey" => {
                self.sys.enc = EncState::default();
                Ok(Val::Void)
            }
            "encrypt" => {
                let plain = text(0);
                if plain.is_empty() {
                    return Ok(Val::Text(String::new()));
                }
                let Some(key) = &self.sys.enc.key else { return err(NOT_CREATED) };
                if plain.len() > 215 {
                    return err(TOO_LONG);
                }
                let oaep = ::rsa::Oaep::new::<sha1::Sha1>();
                match key.to_public_key().encrypt(&mut ::rsa::rand_core::OsRng, oaep, plain.as_bytes()) {
                    Ok(c) => Ok(Val::Text(b64().encode(c))),
                    Err(_) => err("Unknown error"),
                }
            }
            "decrypt" => {
                let cipher = text(0);
                if cipher.is_empty() {
                    return Ok(Val::Text(String::new()));
                }
                let Some(key) = &self.sys.enc.key else { return err(NOT_CREATED) };
                let Ok(bytes) = b64().decode(cipher.trim()) else { return err(BAD_DATA) };
                match key.decrypt(::rsa::Oaep::new::<sha1::Sha1>(), &bytes) {
                    Ok(p) => Ok(Val::Text(String::from_utf8_lossy(&p).into_owned())),
                    Err(_) => err(BAD_DATA),
                }
            }
            "exportencryptionkey" => {
                let Some(key) = &self.sys.enc.key else { return err(NOT_CREATED) };
                let xml = xml_of(key, true);
                let password = text(0);
                let data = if password.is_empty() {
                    xml.into_bytes()
                } else {
                    match crate::dotnet::cipher::transform(&aes(&password), true, xml.as_bytes()) {
                        Ok(c) => b64().encode(c).into_bytes(),
                        Err(e) => return err(e),
                    }
                };
                let name = self.files.temp_name("key");
                self.files.write(&name, data);
                Ok(Val::Text(name))
            }
            "importencryptionkey" => {
                let (name, password) = (text(0), text(1));
                let Some(data) = self.files.read(&name).filter(|_| !name.trim().is_empty()) else {
                    return err(format!("The file '{name}' does not exist."));
                };
                let xml = if password.trim().is_empty() {
                    String::from_utf8_lossy(&data).into_owned()
                } else {
                    let raw = String::from_utf8_lossy(&data);
                    let plain = b64().decode(raw.trim()).ok().and_then(|c| crate::dotnet::cipher::transform(&aes(&password), false, &c).ok());
                    match plain.and_then(|p| String::from_utf8(p).ok()) {
                        Some(x) => x,
                        None => return err(BAD_FILE_OR_PASSWORD),
                    }
                };
                if !xml.trim_start().starts_with('<') {
                    return err(BAD_FILE_OR_PASSWORD);
                }
                let key = match asym::from_xml(&xml) {
                    Ok(Key::Private(k)) => k,
                    _ => return err(BAD_FILE),
                };
                let public = xml_of(&key, false);
                if self.sys.enc.stored.as_ref().is_some_and(|s| *s != public) {
                    return err(OTHER_KEY);
                }
                self.sys.enc.stored = Some(public);
                self.sys.enc.key = Some(key);
                Ok(Val::Bool(true))
            }
            _ => err(format!("Not implemented in bc-rust: function {l}")),
        }
    }
}
