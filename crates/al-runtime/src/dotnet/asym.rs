//! RSA (System.Security.Cryptography.RSA, RSACryptoServiceProvider) on the RustCrypto `rsa` crate:
//! key generation (lazy, since FromXmlString usually replaces the key `RSA.Create` made), PKCS#1
//! v1.5 and PSS signatures, PKCS#1 v1.5 and OAEP encryption, .NET XML key strings, PEM import and
//! export, and the HashAlgorithmName / RSASignaturePadding / RSAEncryptionPadding values.
//!
//! Note: `rsa` 0.9 has a known timing side channel in private-key operations (RUSTSEC-2023-0071,
//! "Marvin attack"). That does not matter for test emulation with throwaway keys, but this module
//! is not meant to protect production secrets.

use super::*;
use ::rsa::pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey, EncodeRsaPrivateKey, EncodeRsaPublicKey, LineEnding};
use ::rsa::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey};
use ::rsa::pss::Pss;
use ::rsa::rand_core::OsRng;
use ::rsa::traits::{PrivateKeyParts, PublicKeyParts};
use ::rsa::{BigUint, Oaep, Pkcs1v15Encrypt, Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
use base64::Engine;

pub const RSA: &str = "System.Security.Cryptography.RSA";
pub const CSP: &str = "System.Security.Cryptography.RSACryptoServiceProvider";
const HASH_NAME: &str = "System.Security.Cryptography.HashAlgorithmName";
const SIG_PAD: &str = "System.Security.Cryptography.RSASignaturePadding";
const ENC_PAD: &str = "System.Security.Cryptography.RSAEncryptionPadding";
const HELPER: &str = "Microsoft.Dynamics.BusinessCentral.DotNet.Helpers.RSAEncryptionHelper";

pub const TYPES: &[&str] = &[RSA, CSP, "System.Security.Cryptography.AsymmetricAlgorithm", HASH_NAME, SIG_PAD, ENC_PAD, HELPER];

pub enum Key {
    /// generated on first use (bits)
    Pending(usize),
    Private(Box<RsaPrivateKey>),
    Public(Box<RsaPublicKey>),
}

pub struct RsaState {
    pub ty: &'static str,
    pub key: Key,
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

pub fn construct(ty: &'static str, a: &Args) -> R<Val> {
    Ok(match ty {
        RSA | CSP => {
            // new RSACryptoServiceProvider([keySize]): 1024 bits by default
            let bits = if a.is_empty() { 1024 } else { a.int(0).max(0) as usize };
            new_rsa(ty, bits)
        }
        _ => net(NetObj::Null(ty)),
    })
}

fn new_rsa(ty: &'static str, bits: usize) -> Val {
    net(NetObj::Rsa(Box::new(RsaState { ty, key: Key::Pending(bits) })))
}

fn named(ty: &'static str, name: &str) -> Val {
    net(NetObj::Prim(ty, Val::Text(name.into())))
}

/// Canonical hash name of a HashAlgorithmName / text argument.
fn hash_name(v: Option<&Val>) -> String {
    let t = match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Prim(_, v) => v.to_text(),
            NetObj::Hasher(alg, _) => alg.to_string(),
            _ => String::new(),
        },
        Some(v) => v.to_text(),
        None => String::new(),
    };
    let u = t.trim().to_ascii_uppercase().replace('-', "");
    match u.as_str() {
        "SHA" => "SHA1".into(),
        _ => u,
    }
}

fn pad_name(v: Option<&Val>) -> String {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Prim(_, v) => v.to_text(),
            _ => String::new(),
        },
        Some(v) => v.to_text(),
        None => String::new(),
    }
}

fn pkcs1_sig(h: &str) -> Option<Pkcs1v15Sign> {
    Some(match h {
        "MD5" => Pkcs1v15Sign::new::<md5_010::Md5>(),
        "SHA1" => Pkcs1v15Sign::new::<sha1::Sha1>(),
        "SHA256" => Pkcs1v15Sign::new::<sha2_010::Sha256>(),
        "SHA384" => Pkcs1v15Sign::new::<sha2_010::Sha384>(),
        "SHA512" => Pkcs1v15Sign::new::<sha2_010::Sha512>(),
        _ => return None,
    })
}

fn pss(h: &str) -> Option<Pss> {
    Some(match h {
        "MD5" => Pss::new::<md5_010::Md5>(),
        "SHA1" => Pss::new::<sha1::Sha1>(),
        "SHA256" => Pss::new::<sha2_010::Sha256>(),
        "SHA384" => Pss::new::<sha2_010::Sha384>(),
        "SHA512" => Pss::new::<sha2_010::Sha512>(),
        _ => return None,
    })
}

fn oaep(h: &str) -> Option<Oaep> {
    Some(match h {
        "MD5" => Oaep::new::<md5_010::Md5>(),
        "SHA1" => Oaep::new::<sha1::Sha1>(),
        "SHA256" => Oaep::new::<sha2_010::Sha256>(),
        "SHA384" => Oaep::new::<sha2_010::Sha384>(),
        "SHA512" => Oaep::new::<sha2_010::Sha512>(),
        _ => return None,
    })
}

impl RsaState {
    fn ensure(&mut self) -> Result<(), String> {
        if let Key::Pending(bits) = self.key {
            let k = RsaPrivateKey::new(&mut OsRng, bits.max(512)).map_err(|e| e.to_string())?;
            self.key = Key::Private(Box::new(k));
        }
        Ok(())
    }

    pub fn private(&mut self) -> Result<&RsaPrivateKey, String> {
        self.ensure()?;
        match &self.key {
            Key::Private(k) => Ok(k),
            _ => Err("Key does not exist.".into()),
        }
    }

    fn public(&mut self) -> Result<RsaPublicKey, String> {
        self.ensure()?;
        Ok(match &self.key {
            Key::Private(k) => k.to_public_key(),
            Key::Public(k) => (**k).clone(),
            Key::Pending(_) => unreachable!(),
        })
    }

    fn bits(&self) -> usize {
        match &self.key {
            Key::Pending(b) => *b,
            Key::Private(k) => k.size() * 8,
            Key::Public(k) => k.size() * 8,
        }
    }
}

fn field(n: &BigUint, len: usize) -> String {
    let mut v = n.to_bytes_be();
    if v.len() < len {
        let mut p = vec![0; len - v.len()];
        p.extend(v);
        v = p;
    }
    b64().encode(v)
}

/// RSA.ToXmlString: .NET's element order and fixed-width parameters.
pub fn to_xml(s: &mut RsaState, private: bool) -> Result<String, String> {
    let public = s.public()?;
    let k = public.size();
    let half = k.div_ceil(2);
    let mut x = format!("<RSAKeyValue><Modulus>{}</Modulus><Exponent>{}</Exponent>", field(public.n(), k), b64().encode(public.e().to_bytes_be()));
    if private {
        let key = s.private()?;
        let (p, q) = (&key.primes()[0], &key.primes()[1]);
        let one = BigUint::from(1u8);
        let dp = key.d() % (p - &one);
        let dq = key.d() % (q - &one);
        let qinv = key.crt_coefficient().unwrap_or_default();
        x.push_str(&format!(
            "<P>{}</P><Q>{}</Q><DP>{}</DP><DQ>{}</DQ><InverseQ>{}</InverseQ><D>{}</D>",
            field(p, half),
            field(q, half),
            field(&dp, half),
            field(&dq, half),
            field(&qinv, half),
            field(key.d(), k)
        ));
    }
    x.push_str("</RSAKeyValue>");
    Ok(x)
}

/// RSACryptoServiceProvider.ExportCspBlob: a CAPI PUBLICKEYBLOB (0x06) or PRIVATEKEYBLOB (0x07):
/// BLOBHEADER (type, version 2, CALG_RSA_KEYX), RSAPUBKEY ("RSA1" / "RSA2", bit length, public
/// exponent), then little-endian modulus and, for a private key, prime1, prime2, exponent1,
/// exponent2, coefficient and private exponent.
fn to_csp_blob(s: &mut RsaState, private: bool) -> Result<Vec<u8>, String> {
    let public = s.public()?;
    let bits = public.size() * 8;
    let (k, half) = (bits / 8, bits / 16);
    let le = |x: &BigUint, len: usize| {
        let mut v = x.to_bytes_le();
        v.resize(len, 0);
        v
    };
    let mut out = vec![if private { 0x07 } else { 0x06 }, 0x02, 0, 0];
    out.extend(0x0000_a400u32.to_le_bytes());
    out.extend(if private { b"RSA2" } else { b"RSA1" });
    out.extend((bits as u32).to_le_bytes());
    let mut e = le(public.e(), 4);
    e.truncate(4);
    out.extend(e);
    out.extend(le(public.n(), k));
    if private {
        let key = s.private()?;
        let (p, q) = (&key.primes()[0], &key.primes()[1]);
        let one = BigUint::from(1u8);
        for x in [p.clone(), q.clone(), key.d() % (p - &one), key.d() % (q - &one), key.crt_coefficient().unwrap_or_default()] {
            out.extend(le(&x, half));
        }
        out.extend(le(key.d(), k));
    }
    Ok(out)
}

/// RSACryptoServiceProvider.ImportCspBlob (the layout of to_csp_blob).
fn from_csp_blob(b: &[u8]) -> Result<Key, String> {
    if b.len() < 20 || b[1] != 0x02 {
        return Err("Bad Version of provider.".into());
    }
    let bits = u32::from_le_bytes([b[12], b[13], b[14], b[15]]) as usize;
    let e = BigUint::from(u32::from_le_bytes([b[16], b[17], b[18], b[19]]));
    let (k, half) = (bits / 8, bits / 16);
    let mut pos = 20;
    let mut take = |len: usize| -> Result<BigUint, String> {
        let s = b.get(pos..pos + len).ok_or_else(|| "Bad Data.".to_string())?;
        pos += len;
        Ok(BigUint::from_bytes_le(s))
    };
    let n = take(k)?;
    match b[0] {
        0x06 => Ok(Key::Public(Box::new(RsaPublicKey::new(n, e).map_err(|x| x.to_string())?))),
        0x07 => {
            let p = take(half)?;
            let q = take(half)?;
            for _ in 0..3 {
                take(half)?;
            }
            let d = take(k)?;
            Ok(Key::Private(Box::new(RsaPrivateKey::from_components(n, e, d, vec![p, q]).map_err(|x| x.to_string())?)))
        }
        _ => Err("Bad Data.".into()),
    }
}

/// SignedXml (signedxml.rs): a key's public half, and PKCS#1 v1.5 signing / verification of a hash.
pub fn public_key(s: &mut RsaState) -> Result<RsaPublicKey, String> {
    s.public()
}

pub fn pkcs1_sign(s: &mut RsaState, h: &str, hashed: &[u8]) -> Result<Vec<u8>, String> {
    let p = pkcs1_sig(h).ok_or_else(|| format!("'{h}' is not a known hash algorithm."))?;
    s.private().and_then(|k| k.sign(p, hashed).map_err(|e| e.to_string()))
}

pub fn pkcs1_verify(k: &RsaPublicKey, h: &str, hashed: &[u8], sig: &[u8]) -> bool {
    pkcs1_sig(h).is_some_and(|p| k.verify(p, hashed, sig).is_ok())
}

fn xml_part(x: &str, tag: &str) -> Option<BigUint> {
    let open = format!("<{tag}>");
    let start = x.find(&open)? + open.len();
    let end = start + x[start..].find(&format!("</{tag}>"))?;
    let text: String = x[start..end].chars().filter(|c| !c.is_whitespace()).collect();
    b64().decode(text).ok().map(|b| BigUint::from_bytes_be(&b))
}

/// RSA.FromXmlString.
pub fn from_xml(x: &str) -> Result<Key, String> {
    let bad = |p: &str| format!("Input string does not contain a valid encoding of the 'RSA' '{p}' parameter.");
    let n = xml_part(x, "Modulus").ok_or_else(|| bad("Modulus"))?;
    let e = xml_part(x, "Exponent").ok_or_else(|| bad("Exponent"))?;
    match xml_part(x, "D") {
        Some(d) => {
            let primes = match (xml_part(x, "P"), xml_part(x, "Q")) {
                (Some(p), Some(q)) => vec![p, q],
                _ => vec![],
            };
            let k = RsaPrivateKey::from_components(n, e, d, primes).map_err(|e| e.to_string())?;
            Ok(Key::Private(Box::new(k)))
        }
        None => RsaPublicKey::new(n, e).map(|k| Key::Public(Box::new(k))).map_err(|e| e.to_string()),
    }
}

/// ImportFromPem: PKCS#1 or PKCS#8 private keys, PKCS#1 or SubjectPublicKeyInfo public keys.
pub fn from_pem(pem: &str) -> Result<Key, String> {
    let bad = || "No supported key formats were found. Check that the input represents the contents of a PEM-encoded key file, not the path to such a file.".to_string();
    let begin = pem.find("-----BEGIN ").ok_or_else(bad)?;
    let rest = &pem[begin + 11..];
    let label_end = rest.find("-----").ok_or_else(bad)?;
    let label = rest[..label_end].trim().to_string();
    let body_start = label_end + 5;
    let body_end = rest[body_start..].find("-----END").map(|i| body_start + i).ok_or_else(bad)?;
    let body: String = rest[body_start..body_end].chars().filter(|c| !c.is_whitespace()).collect();
    let der = b64().decode(body).map_err(|_| bad())?;
    let r = match label.as_str() {
        "RSA PRIVATE KEY" => RsaPrivateKey::from_pkcs1_der(&der).map(|k| Key::Private(Box::new(k))).map_err(|e| e.to_string()),
        "PRIVATE KEY" => RsaPrivateKey::from_pkcs8_der(&der).map(|k| Key::Private(Box::new(k))).map_err(|e| e.to_string()),
        "RSA PUBLIC KEY" => RsaPublicKey::from_pkcs1_der(&der).map(|k| Key::Public(Box::new(k))).map_err(|e| e.to_string()),
        "PUBLIC KEY" => RsaPublicKey::from_public_key_der(&der).map(|k| Key::Public(Box::new(k))).map_err(|e| e.to_string()),
        _ => return Err(bad()),
    };
    r.map_err(|_| bad())
}

fn pem_text(r: Result<String, String>) -> Result<Val, String> {
    r.map(|s| Val::Text(s.trim_end().to_string()))
}

pub fn call(n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    // statics and named values
    match (ty, m) {
        (RSA | CSP | "System.Security.Cryptography.AsymmetricAlgorithm", "create") => {
            let bits = match a.raw(0) {
                Some(Val::Int(_) | Val::BigInt(_)) => a.int(0).max(0) as usize,
                Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Prim(_, Val::Int(_))) => a.int(0).max(0) as usize,
                _ => 2048,
            };
            return ok(new_rsa(RSA, bits));
        }
        (HASH_NAME, "md5" | "sha1" | "sha256" | "sha384" | "sha512") => return ok(named(HASH_NAME, &m.to_ascii_uppercase())),
        (HASH_NAME, "fromoid") => return ok(named(HASH_NAME, "SHA256")),
        (HASH_NAME, "name") => return ok(Val::Text(pad_name(Some(&Val::Net(n.clone()))))),
        (SIG_PAD, "pkcs1") => return ok(named(SIG_PAD, "Pkcs1")),
        (SIG_PAD, "pss") => return ok(named(SIG_PAD, "Pss")),
        (ENC_PAD, "pkcs1") => return ok(named(ENC_PAD, "Pkcs1")),
        (ENC_PAD, "oaepsha1" | "oaepsha256" | "oaepsha384" | "oaepsha512") => return ok(named(ENC_PAD, &format!("OaepSHA{}", &m[7..]))),
        (ENC_PAD, "createoaep") => return ok(named(ENC_PAD, &format!("Oaep{}", hash_name(a.raw(0))))),
        (SIG_PAD | ENC_PAD, "mode") => return ok(Val::Text(pad_name(Some(&Val::Net(n.clone()))))),
        (HELPER, "importfrompem") => {
            let key = match from_pem(&a.text(1)) {
                Ok(k) => k,
                Err(e) => return net_err("RSAEncryptionHelper", "ImportFromPem", e),
            };
            if let Some(t) = a.net(0) {
                if let NetObj::Rsa(s) = &mut *t.borrow_mut() {
                    s.key = key;
                }
            }
            return void();
        }
        _ => {}
    }
    let mut o = n.borrow_mut();
    let NetObj::Rsa(s) = &mut *o else { return Ok(Out::Unknown) };
    let short = super::short_name(s.ty);
    let fail = |member: &str, e: String| net_err::<Out>(short, member, e);
    let set = a.len() == 1;
    ok(match m {
        "keysize" if set => {
            s.key = Key::Pending(a.int(0).max(0) as usize);
            Val::Void
        }
        "keysize" => Val::Int(s.bits() as i32),
        "persistkeyincsp" if set => Val::Void,
        "persistkeyincsp" => Val::Bool(false),
        "publiconly" => Val::Bool(matches!(s.key, Key::Public(_))),
        "signaturealgorithm" | "keyexchangealgorithm" => Val::Text("RSA".into()),
        "toxmlstring" => match to_xml(s, a.bool(0)) {
            Ok(x) => Val::Text(x),
            Err(e) => return fail("ToXmlString", e),
        },
        "exportcspblob" => match to_csp_blob(s, !a.is_empty() && a.bool(0)) {
            Ok(b) => super::text::bytes_val(&b),
            Err(e) => return fail("ExportCspBlob", e),
        },
        "importcspblob" => match from_csp_blob(&super::text::arg_bytes(a.raw(0))) {
            Ok(k) => {
                s.key = k;
                Val::Void
            }
            Err(e) => return fail("ImportCspBlob", e),
        },
        "fromxmlstring" => match from_xml(&a.text(0)) {
            Ok(k) => {
                s.key = k;
                Val::Void
            }
            Err(e) => return fail("FromXmlString", e),
        },
        "importfrompem" => match from_pem(&a.text(0)) {
            Ok(k) => {
                s.key = k;
                Val::Void
            }
            Err(e) => return fail("ImportFromPem", e),
        },
        "exportrsaprivatekeypem" | "exportpkcs8privatekeypem" => {
            let r = s.private().and_then(|k| {
                if m == "exportrsaprivatekeypem" {
                    k.to_pkcs1_pem(LineEnding::LF).map(|p| p.to_string()).map_err(|e| e.to_string())
                } else {
                    k.to_pkcs8_pem(LineEnding::LF).map(|p| p.to_string()).map_err(|e| e.to_string())
                }
            });
            match pem_text(r) {
                Ok(v) => v,
                Err(e) => return fail("ExportRSAPrivateKeyPem", e),
            }
        }
        "exportrsapublickeypem" | "exportsubjectpublickeyinfopem" => {
            let r = s.public().and_then(|k| {
                if m == "exportrsapublickeypem" {
                    k.to_pkcs1_pem(LineEnding::LF).map_err(|e| e.to_string())
                } else {
                    k.to_public_key_pem(LineEnding::LF).map_err(|e| e.to_string())
                }
            });
            match pem_text(r) {
                Ok(v) => v,
                Err(e) => return fail("ExportRSAPublicKeyPem", e),
            }
        }
        "signdata" | "signhash" => {
            // RSA: (data, HashAlgorithmName, RSASignaturePadding); CSP: (data, halg)
            let data = super::text::arg_bytes(a.raw(0));
            let h = hash_name(a.raw(1));
            let hashed = if m == "signhash" { data } else { super::crypto::digest(&h, &data) };
            let padding = if a.len() >= 3 { pad_name(a.raw(2)) } else { "Pkcs1".into() };
            let r = s.private().and_then(|k| {
                if padding.eq_ignore_ascii_case("pss") {
                    let p = pss(&h).ok_or_else(|| format!("'{h}' is not a known hash algorithm."))?;
                    k.sign_with_rng(&mut OsRng, p, &hashed).map_err(|e| e.to_string())
                } else {
                    let p = pkcs1_sig(&h).ok_or_else(|| format!("'{h}' is not a known hash algorithm."))?;
                    k.sign(p, &hashed).map_err(|e| e.to_string())
                }
            });
            match r {
                Ok(sig) => super::text::bytes_val(&sig),
                Err(e) => return fail(if m == "signdata" { "SignData" } else { "SignHash" }, e),
            }
        }
        "verifydata" | "verifyhash" => {
            // RSA: (data, signature, HashAlgorithmName, padding); CSP: (data, halg, signature)
            let data = super::text::arg_bytes(a.raw(0));
            let (h, sig, padding) = if a.len() >= 4 {
                (hash_name(a.raw(2)), super::text::arg_bytes(a.raw(1)), pad_name(a.raw(3)))
            } else {
                (hash_name(a.raw(1)), super::text::arg_bytes(a.raw(2)), "Pkcs1".into())
            };
            let hashed = if m == "verifyhash" { data } else { super::crypto::digest(&h, &data) };
            let public = match s.public() {
                Ok(k) => k,
                Err(e) => return fail("VerifyData", e),
            };
            let good = if padding.eq_ignore_ascii_case("pss") {
                pss(&h).is_some_and(|p| public.verify(p, &hashed, &sig).is_ok())
            } else {
                pkcs1_sig(&h).is_some_and(|p| public.verify(p, &hashed, &sig).is_ok())
            };
            Val::Bool(good)
        }
        "encrypt" | "decrypt" => {
            // RSA: (data, RSAEncryptionPadding); CSP: (data, fOAEP) with OAEP-SHA1
            let data = super::text::arg_bytes(a.raw(0));
            let padding = match a.raw(1) {
                Some(Val::Bool(b)) => if *b { "OaepSHA1".to_string() } else { "Pkcs1".into() },
                Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Prim(_, Val::Bool(_))) => if a.bool(1) { "OaepSHA1".into() } else { "Pkcs1".into() },
                other => pad_name(other),
            };
            let oaep_hash = padding.strip_prefix("Oaep").map(|h| h.to_ascii_uppercase());
            let r = if m == "encrypt" {
                s.public().and_then(|k| match &oaep_hash {
                    Some(h) => k.encrypt(&mut OsRng, oaep(h).ok_or("Unknown padding mode.")?, &data).map_err(|e| e.to_string()),
                    None => k.encrypt(&mut OsRng, Pkcs1v15Encrypt, &data).map_err(|e| e.to_string()),
                })
            } else {
                // BC's service tier reports a failed OAEP decode of RSACryptoServiceProvider by its
                // resource key (System.Security.Cryptography resources are not shipped)
                let csp_oaep = s.ty == CSP && oaep_hash.is_some();
                s.private().and_then(|k| {
                    match &oaep_hash {
                        Some(h) => k.decrypt(oaep(h).ok_or("Unknown padding mode.")?, &data),
                        None => k.decrypt(Pkcs1v15Encrypt, &data),
                    }
                    .map_err(|_| if csp_oaep { "Cryptography_OAEPDecoding" } else { "The parameter is incorrect." }.to_string())
                })
            };
            match r {
                Ok(b) => super::text::bytes_val(&b),
                Err(e) => return fail(if m == "encrypt" { "Encrypt" } else { "Decrypt" }, e),
            }
        }
        "clear" | "dispose" => Val::Void,
        _ => return Ok(Out::Unknown),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_pem_sign_encrypt() {
        let mut s = RsaState { ty: RSA, key: Key::Pending(1024) };
        let xml = to_xml(&mut s, true).unwrap();
        assert!(xml.starts_with("<RSAKeyValue><Modulus>") && xml.contains("<DQ>") && xml.contains("<Exponent>AQAB</Exponent>"));
        // round trip through XML and PEM keeps the key
        let mut t = RsaState { ty: RSA, key: from_xml(&xml).unwrap() };
        assert_eq!(to_xml(&mut t, true).unwrap(), xml);
        let pem = t.private().unwrap().to_pkcs1_pem(LineEnding::LF).unwrap().to_string();
        let mut u = RsaState { ty: RSA, key: from_pem(&pem).unwrap() };
        assert_eq!(to_xml(&mut u, true).unwrap(), xml);
        // public-only XML verifies what the private key signed
        let hashed = super::super::crypto::digest("SHA256", b"data");
        let sig = s.private().unwrap().sign_with_rng(&mut OsRng, pss("SHA256").unwrap(), &hashed).unwrap();
        let mut p = RsaState { ty: RSA, key: from_xml(&to_xml(&mut s, false).unwrap()).unwrap() };
        assert!(p.public().unwrap().verify(pss("SHA256").unwrap(), &hashed, &sig).is_ok());
        assert!(p.private().is_err());
        let ct = p.public().unwrap().encrypt(&mut OsRng, oaep("SHA256").unwrap(), b"secret").unwrap();
        assert_eq!(s.private().unwrap().decrypt(oaep("SHA256").unwrap(), &ct).unwrap(), b"secret");
        assert!(s.private().unwrap().decrypt(Pkcs1v15Encrypt, &ct).is_err());
    }
}
