//! X.509 certificates (System.Security.Cryptography.X509Certificates.X509Certificate2) on
//! `x509-cert`, with the PKCS#12 / PFX container decoded and written here: a small DER reader and
//! writer, the PKCS#12 key derivation (RFC 7292 appendix B) and MAC check, PBES1
//! pbeWithSHAAnd3-KeyTripleDES-CBC and PBES2 (PBKDF2 + AES-CBC / 3DES-CBC) decryption, and cert,
//! key and shrouded-key bags. Names print as .NET's X500DistinguishedName (most specific RDN
//! first, ", " between, values with special characters in double quotes). Local time is UTC on
//! this service tier, so the date strings are the UTC clock times in the session culture.

use super::*;
use ::rsa::pkcs1::DecodeRsaPublicKey;
use ::rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use ::rsa::{RsaPrivateKey, RsaPublicKey};
use x509_cert::der::{Decode, Tagged};

pub const CERT2: &str = "System.Security.Cryptography.X509Certificates.X509Certificate2";
const CERT: &str = "System.Security.Cryptography.X509Certificates.X509Certificate";
const PUBLIC_KEY: &str = "System.Security.Cryptography.X509Certificates.PublicKey";
const CONTENT_TYPE: &str = "System.Security.Cryptography.X509Certificates.X509ContentType";
const STORAGE_FLAGS: &str = "System.Security.Cryptography.X509Certificates.X509KeyStorageFlags";
const ENUM: &str = "System.Enum";
const REQUEST: &str = "System.Security.Cryptography.X509Certificates.CertificateRequest";
const EXT_COLLECTION: &str = "System.Security.Cryptography.X509Certificates.X509ExtensionCollection";
const BASIC_CONSTRAINTS: &str = "System.Security.Cryptography.X509Certificates.X509BasicConstraintsExtension";
const KEY_USAGE: &str = "System.Security.Cryptography.X509Certificates.X509KeyUsageExtension";
const ENHANCED_KEY_USAGE: &str = "System.Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension";
const KEY_USAGE_FLAGS: &str = "System.Security.Cryptography.X509Certificates.X509KeyUsageFlags";
const OID: &str = "System.Security.Cryptography.Oid";
const OID_COLLECTION: &str = "System.Security.Cryptography.OidCollection";
const PROPERTY_INFO: &str = "System.Reflection.PropertyInfo";

/// X509Certificate2's public properties of simple types: (name, property type).
const CERT_PROPERTIES: &[(&str, &str)] = &[
    ("Archived", "System.Boolean"),
    ("FriendlyName", "System.String"),
    ("HasPrivateKey", "System.Boolean"),
    ("Issuer", "System.String"),
    ("NotAfter", "System.DateTime"),
    ("NotBefore", "System.DateTime"),
    ("SerialNumber", "System.String"),
    ("Subject", "System.String"),
    ("Thumbprint", "System.String"),
    ("Version", "System.Int32"),
];

/// Type.GetProperties() of an emulated type (reflection for X509Certificate2).
pub fn properties(type_name: &str) -> Option<Val> {
    if !matches!(type_name, CERT2 | CERT) {
        return None;
    }
    let items = CERT_PROPERTIES
        .iter()
        .map(|(n, t)| net(NetObj::Bag(PROPERTY_INFO, vec![("name".into(), Val::Text(n.to_string())), ("propertytype".into(), net(NetObj::Prim("System.Type", Val::Text(t.to_string()))))])))
        .collect();
    Some(net(NetObj::Array(PROPERTY_INFO, items)))
}

pub const TYPES: &[&str] = &[
    CERT2, CERT, PUBLIC_KEY, CONTENT_TYPE, STORAGE_FLAGS, ENUM, REQUEST, EXT_COLLECTION, BASIC_CONSTRAINTS, KEY_USAGE, ENHANCED_KEY_USAGE, KEY_USAGE_FLAGS, OID, OID_COLLECTION,
    PROPERTY_INFO,
];

pub fn enum_names(t: &str) -> Option<&'static [(&'static str, i64)]> {
    Some(match t {
        CONTENT_TYPE => &[("Unknown", 0), ("Cert", 1), ("SerializedCert", 2), ("Pfx", 3), ("Pkcs12", 3), ("SerializedStore", 4), ("Pkcs7", 5), ("Authenticode", 6)],
        STORAGE_FLAGS => &[("DefaultKeySet", 0), ("UserKeySet", 1), ("MachineKeySet", 2), ("Exportable", 4), ("UserProtected", 8), ("PersistKeySet", 16), ("EphemeralKeySet", 32)],
        KEY_USAGE_FLAGS => &[
            ("None", 0),
            ("EncipherOnly", 1),
            ("CrlSign", 2),
            ("KeyCertSign", 4),
            ("KeyAgreement", 8),
            ("DataEncipherment", 16),
            ("KeyEncipherment", 32),
            ("NonRepudiation", 64),
            ("DigitalSignature", 128),
            ("DecipherOnly", 32768),
        ],
        _ => return super::zip::enum_names(t),
    })
}

/// A loaded certificate.
/// A CertificateRequest: subject, key, signature hash, extensions (an X509ExtensionCollection of
/// extension objects carrying their DER).
pub struct CertReq {
    pub subject: String,
    pub key: Box<RsaPrivateKey>,
    pub hash: String,
    pub pss: bool,
    pub exts: NetRef,
}

/// A certificate's RSA public key (SignedXml.CheckSignature, KeyInfo X509Data).
pub fn rsa_public_key(der: &[u8]) -> Option<RsaPublicKey> {
    let cert = x509_cert::Certificate::from_der(der).ok()?;
    RsaPublicKey::from_pkcs1_der(cert.tbs_certificate.subject_public_key_info.subject_public_key.raw_bytes()).ok()
}

pub struct CertState {
    pub der: Vec<u8>,
    pub friendly_name: String,
    pub key: Option<Box<RsaPrivateKey>>,
}

// ---------------------------------------------------------------- DER

struct Tlv<'a> {
    tag: u8,
    body: &'a [u8],
}

fn read_tlv(b: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let tag = *b.first()?;
    let first = *b.get(1)?;
    let (len, hdr) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || b.len() < 2 + n {
            return None;
        }
        (b[2..2 + n].iter().fold(0usize, |a, x| (a << 8) | *x as usize), 2 + n)
    };
    let end = hdr.checked_add(len)?;
    if b.len() < end {
        return None;
    }
    Some((Tlv { tag, body: &b[hdr..end] }, &b[end..]))
}

fn items(mut b: &[u8]) -> Vec<Tlv<'_>> {
    let mut out = vec![];
    while let Some((t, rest)) = read_tlv(b) {
        out.push(t);
        b = rest;
    }
    out
}

fn int_of(t: &Tlv) -> u64 {
    t.body.iter().fold(0u64, |a, x| (a << 8) | *x as u64)
}

fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = body.len();
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let bytes: Vec<u8> = n.to_be_bytes().iter().copied().skip_while(|x| *x == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend_from_slice(body);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0)).collect()
}

// OIDs (encoded bodies)
const OID_DATA: &str = "2a864886f70d010701";
const OID_ENCRYPTED_DATA: &str = "2a864886f70d010706";
const OID_KEY_BAG: &str = "2a864886f70d010c0a0101";
const OID_SHROUDED_KEY_BAG: &str = "2a864886f70d010c0a0102";
const OID_CERT_BAG: &str = "2a864886f70d010c0a0103";
const OID_X509_CERT: &str = "2a864886f70d01091601";
const OID_FRIENDLY_NAME: &str = "2a864886f70d010914";
const OID_LOCAL_KEY_ID: &str = "2a864886f70d010915";
const OID_PBE_SHA1_3DES: &str = "2a864886f70d010c0103";
const OID_PBE_SHA1_2DES: &str = "2a864886f70d010c0104";
const OID_PBES2: &str = "2a864886f70d01050d";
const OID_PBKDF2: &str = "2a864886f70d01050c";
const OID_SHA1: &str = "2b0e03021a";

fn is(t: &Tlv, oid: &str) -> bool {
    t.tag == 0x06 && t.body == hex(oid).as_slice()
}

fn hash_of_oid(body: &[u8]) -> &'static str {
    let h = |s: &str| hex(s);
    if body == h("2a864886f70d0207").as_slice() || body == h(OID_SHA1).as_slice() {
        "SHA1"
    } else if body == h("2a864886f70d020a").as_slice() || body == h("608648016503040202").as_slice() {
        "SHA384"
    } else if body == h("2a864886f70d020b").as_slice() || body == h("608648016503040203").as_slice() {
        "SHA512"
    } else {
        "SHA256"
    }
}

// ---------------------------------------------------------------- PKCS#12 crypto

/// RFC 7292 appendix B.2 key derivation (`id` 1 key, 2 IV, 3 MAC key).
fn p12_kdf(alg: &str, password: &[u8], salt: &[u8], id: u8, iterations: u64, n: usize) -> Vec<u8> {
    let v = if matches!(alg, "SHA384" | "SHA512") { 128 } else { 64 };
    let fill = |x: &[u8]| -> Vec<u8> {
        if x.is_empty() {
            return vec![];
        }
        let len = v * x.len().div_ceil(v);
        x.iter().cycle().take(len).copied().collect()
    };
    let mut i_buf = fill(salt);
    i_buf.extend(fill(password));
    let mut out = vec![];
    while out.len() < n {
        let mut a = vec![id; v];
        a.extend_from_slice(&i_buf);
        let mut h = super::crypto::digest(alg, &a);
        for _ in 1..iterations.max(1) {
            h = super::crypto::digest(alg, &h);
        }
        out.extend_from_slice(&h);
        if out.len() >= n {
            break;
        }
        let b: Vec<u8> = h.iter().cycle().take(v).copied().collect();
        for chunk in i_buf.chunks_mut(v) {
            let mut carry = 1u16;
            for k in (0..chunk.len()).rev() {
                let x = chunk[k] as u16 + b[k] as u16 + carry;
                chunk[k] = x as u8;
                carry = x >> 8;
            }
        }
    }
    out.truncate(n);
    out
}

fn bmp(password: &str) -> Vec<u8> {
    let mut out: Vec<u8> = password.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
    out.extend([0, 0]);
    out
}

/// Decrypt `data` per AlgorithmIdentifier `alg` (PBES1 3DES or PBES2).
fn decrypt(alg: &Tlv, data: &[u8], password: &str, bmp_pw: &[u8]) -> Option<Vec<u8>> {
    let parts = items(alg.body);
    let (oid, params) = (parts.first()?, parts.get(1)?);
    let triple = |key: Vec<u8>, iv: Vec<u8>| super::cipher::Sym {
        ty: "System.Security.Cryptography.TripleDESCryptoServiceProvider",
        key,
        iv,
        mode: super::cipher::CBC,
        padding: super::cipher::PKCS7,
        feedback: 8,
    };
    if is(oid, OID_PBE_SHA1_3DES) || is(oid, OID_PBE_SHA1_2DES) {
        let p = items(params.body);
        let (salt, iter) = (p.first()?.body, int_of(p.get(1)?));
        let klen = if is(oid, OID_PBE_SHA1_3DES) { 24 } else { 16 };
        let key = p12_kdf("SHA1", bmp_pw, salt, 1, iter, klen);
        let iv = p12_kdf("SHA1", bmp_pw, salt, 2, iter, 8);
        return super::cipher::transform(&triple(key, iv), false, data).ok();
    }
    if is(oid, OID_PBES2) {
        let p = items(params.body);
        let kdf = items(p.first()?.body);
        let enc = items(p.get(1)?.body);
        if !is(kdf.first()?, OID_PBKDF2) {
            return None;
        }
        let kp = items(kdf.get(1)?.body);
        let salt = kp.first()?.body;
        let iter = int_of(kp.get(1)?);
        let prf = kp.iter().skip(2).find(|t| t.tag == 0x30).and_then(|t| items(t.body).first().map(|o| hash_of_oid(o.body))).unwrap_or("SHA1");
        let enc_oid = enc.first()?.body;
        let iv = enc.get(1)?.body.to_vec();
        let aes_len = match enc_oid.last()? {
            0x02 if enc_oid.starts_with(&hex("6086480165030401")) => Some(16),
            0x16 if enc_oid.starts_with(&hex("6086480165030401")) => Some(24),
            0x2a if enc_oid.starts_with(&hex("6086480165030401")) => Some(32),
            _ => None,
        };
        let sym = match aes_len {
            Some(n) => super::cipher::Sym {
                ty: "System.Security.Cryptography.Aes",
                key: super::crypto::pbkdf2(prf, password.as_bytes(), salt, iter as u32, n),
                iv,
                mode: super::cipher::CBC,
                padding: super::cipher::PKCS7,
                feedback: 16,
            },
            None if enc_oid == hex("2a864886f70d0307").as_slice() => triple(super::crypto::pbkdf2(prf, password.as_bytes(), salt, iter as u32, 24), iv),
            None => return None,
        };
        return super::cipher::transform(&sym, false, data).ok();
    }
    None
}

struct Bag {
    cert: Option<Vec<u8>>,
    key: Option<Vec<u8>>,
    friendly: String,
    local_id: Vec<u8>,
}

fn bag_attrs(attrs: Option<&Tlv>) -> (String, Vec<u8>) {
    let (mut name, mut id) = (String::new(), vec![]);
    for a in attrs.map(|t| items(t.body)).unwrap_or_default() {
        let p = items(a.body);
        let (Some(oid), Some(set)) = (p.first(), p.get(1)) else { continue };
        let Some(v) = items(set.body).into_iter().next() else { continue };
        if is(oid, OID_FRIENDLY_NAME) {
            let u: Vec<u16> = v.body.chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
            name = String::from_utf16_lossy(&u);
        } else if is(oid, OID_LOCAL_KEY_ID) {
            id = v.body.to_vec();
        }
    }
    (name, id)
}

fn safe_bags(contents: &[u8], password: &str, bmp_pw: &[u8], out: &mut Vec<Bag>) -> Result<(), String> {
    let Some((sc, _)) = read_tlv(contents) else { return Err("bad SafeContents".into()) };
    for bag in items(sc.body) {
        let p = items(bag.body);
        let (Some(oid), Some(val)) = (p.first(), p.get(1)) else { continue };
        let (friendly, local_id) = bag_attrs(p.get(2));
        let inner = read_tlv(val.body).map(|x| x.0);
        let mut b = Bag { cert: None, key: None, friendly, local_id };
        if is(oid, OID_CERT_BAG) {
            let Some(cb) = inner else { continue };
            let cp = items(cb.body);
            if cp.first().is_some_and(|o| is(o, OID_X509_CERT)) {
                b.cert = cp.get(1).and_then(|w| read_tlv(w.body)).map(|(o, _)| o.body.to_vec());
            }
        } else if is(oid, OID_KEY_BAG) {
            b.key = Some(val.body.to_vec());
        } else if is(oid, OID_SHROUDED_KEY_BAG) {
            let Some(ek) = inner else { continue };
            let ep = items(ek.body);
            let (Some(alg), Some(data)) = (ep.first(), ep.get(1)) else { continue };
            b.key = Some(decrypt(alg, data.body, password, bmp_pw).ok_or("The specified network password is not correct.")?);
        } else {
            continue;
        }
        out.push(b);
    }
    Ok(())
}

/// Decode a PFX: (certificate DER, friendly name, private key).
fn read_pfx(pfx: &[u8], password: &str) -> Result<CertState, String> {
    let bad = || "ASN1 corrupted data.".to_string();
    let (top, _) = read_tlv(pfx).ok_or_else(bad)?;
    let parts = items(top.body);
    let auth = items(parts.get(1).ok_or_else(bad)?.body);
    if !auth.first().is_some_and(|o| is(o, OID_DATA)) {
        return Err(bad());
    }
    let auth_content = read_tlv(auth.get(1).ok_or_else(bad)?.body).ok_or_else(bad)?.0.body;
    // the MAC tells which password encoding is right ('' and null differ for empty passwords)
    let candidates: Vec<Vec<u8>> = if password.is_empty() { vec![bmp(""), vec![]] } else { vec![bmp(password)] };
    let mut bmp_pw = candidates[0].clone();
    if let Some(mac) = parts.get(2) {
        let mp = items(mac.body);
        let di = items(mp.first().ok_or_else(bad)?.body);
        let alg = hash_of_oid(items(di.first().ok_or_else(bad)?.body).first().ok_or_else(bad)?.body);
        let expected = di.get(1).ok_or_else(bad)?.body;
        let salt = mp.get(1).ok_or_else(bad)?.body;
        let iter = mp.get(2).map(int_of).unwrap_or(1);
        let found = candidates.iter().find(|pw| {
            let key = p12_kdf(alg, pw, salt, 3, iter, super::crypto::digest(alg, b"").len());
            super::crypto::hmac(alg, &key, auth_content) == expected
        });
        bmp_pw = found.ok_or("The specified network password is not correct.")?.clone();
    }
    let mut bags = vec![];
    let (safe, _) = read_tlv(auth_content).ok_or_else(bad)?;
    for ci in items(safe.body) {
        let cp = items(ci.body);
        let Some(oid) = cp.first() else { continue };
        if is(oid, OID_DATA) {
            let octets = cp.get(1).and_then(|w| read_tlv(w.body)).ok_or_else(bad)?.0.body;
            safe_bags(octets, password, &bmp_pw, &mut bags)?;
        } else if is(oid, OID_ENCRYPTED_DATA) {
            let ed = items(cp.get(1).and_then(|w| read_tlv(w.body)).ok_or_else(bad)?.0.body);
            let eci = items(ed.get(1).ok_or_else(bad)?.body);
            let (Some(alg), Some(data)) = (eci.get(1), eci.get(2)) else { return Err(bad()) };
            let plain = decrypt(alg, data.body, password, &bmp_pw).ok_or("The specified network password is not correct.")?;
            safe_bags(&plain, password, &bmp_pw, &mut bags)?;
        }
    }
    let key_bag = bags.iter().find(|b| b.key.is_some());
    let cert = key_bag
        .and_then(|k| bags.iter().find(|b| b.cert.is_some() && !k.local_id.is_empty() && b.local_id == k.local_id))
        .or_else(|| bags.iter().find(|b| b.cert.is_some()))
        .ok_or("The certificate data cannot be read.")?;
    let key = key_bag.and_then(|k| k.key.as_ref()).and_then(|d| RsaPrivateKey::from_pkcs8_der(d).ok()).map(Box::new);
    Ok(CertState { der: cert.cert.clone().unwrap_or_default(), friendly_name: cert.friendly.clone(), key })
}

/// X509Certificate2(bytes [, password]): a PFX, a DER certificate or a PEM certificate.
pub fn load(bytes: &[u8], password: &str) -> Result<CertState, String> {
    if let Some((top, _)) = read_tlv(bytes) {
        let parts = items(top.body);
        if parts.first().is_some_and(|v| v.tag == 0x02 && int_of(v) == 3) {
            return read_pfx(bytes, password);
        }
        if x509_cert::Certificate::from_der(bytes).is_ok() {
            return Ok(CertState { der: bytes.to_vec(), friendly_name: String::new(), key: None });
        }
    }
    let text = String::from_utf8_lossy(bytes);
    if let (Some(b), Some(e)) = (text.find("-----BEGIN CERTIFICATE-----"), text.find("-----END CERTIFICATE-----")) {
        use base64::Engine;
        let body: String = text[b + 27..e].chars().filter(|c| !c.is_whitespace()).collect();
        if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(body) {
            if x509_cert::Certificate::from_der(&der).is_ok() {
                return Ok(CertState { der, friendly_name: String::new(), key: None });
            }
        }
    }
    Err("The certificate data cannot be read.".into())
}

/// A PFX with the certificate (and private key) and an empty-password SHA-1 MAC.
fn write_pfx(c: &CertState) -> Vec<u8> {
    let oid = |s: &str| tlv(0x06, &hex(s));
    let mut attrs = vec![];
    if !c.friendly_name.is_empty() {
        let name: Vec<u8> = c.friendly_name.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        attrs.push(seq(&[oid(OID_FRIENDLY_NAME), tlv(0x31, &tlv(0x1e, &name))]));
    }
    let local_id = seq(&[oid(OID_LOCAL_KEY_ID), tlv(0x31, &tlv(0x04, &[1, 0, 0, 0]))]);
    if c.key.is_some() {
        attrs.push(local_id.clone());
    }
    let mut cert_bag = vec![oid(OID_CERT_BAG), tlv(0xa0, &seq(&[oid(OID_X509_CERT), tlv(0xa0, &tlv(0x04, &c.der))]))];
    if !attrs.is_empty() {
        cert_bag.push(tlv(0x31, &attrs.concat()));
    }
    let mut bags = vec![seq(&cert_bag)];
    if let Some(k) = &c.key {
        if let Ok(doc) = k.to_pkcs8_der() {
            bags.push(seq(&[oid(OID_KEY_BAG), tlv(0xa0, doc.as_bytes()), tlv(0x31, &local_id)]));
        }
    }
    let safe = seq(&bags);
    let auth = seq(&[seq(&[oid(OID_DATA), tlv(0xa0, &tlv(0x04, &safe))])]);
    let salt = super::cipher::random_bytes(8);
    let key = p12_kdf("SHA1", &bmp(""), &salt, 3, 2048, 20);
    let mac = super::crypto::hmac("SHA1", &key, &auth);
    let mac_data = seq(&[seq(&[seq(&[oid(OID_SHA1), vec![0x05, 0x00]]), tlv(0x04, &mac)]), tlv(0x04, &salt), tlv(0x02, &[0x08, 0x00])]);
    seq(&[tlv(0x02, &[3]), seq(&[oid(OID_DATA), tlv(0xa0, &tlv(0x04, &auth))]), mac_data])
}

// ---------------------------------------------------------------- names and fields

fn attr_name(oid: &str) -> String {
    match oid {
        "2.5.4.3" => "CN",
        "2.5.4.10" => "O",
        "2.5.4.11" => "OU",
        "2.5.4.6" => "C",
        "2.5.4.7" => "L",
        "2.5.4.8" => "S",
        "2.5.4.9" => "STREET",
        "2.5.4.12" => "T",
        "2.5.4.42" => "G",
        "2.5.4.4" => "SN",
        "2.5.4.5" => "SERIALNUMBER",
        "2.5.4.43" => "I",
        "0.9.2342.19200300.100.1.25" => "DC",
        "0.9.2342.19200300.100.1.1" => "UID",
        "1.2.840.113549.1.9.1" => "E",
        _ => return format!("OID.{oid}"),
    }
    .into()
}

/// X500DistinguishedName.Name as .NET prints it.
fn dn(name: &x509_cert::name::Name) -> String {
    let mut parts = vec![];
    for rdn in name.0.iter().rev() {
        let atvs: Vec<String> = rdn
            .0
            .iter()
            .map(|atv| {
                let raw = atv.value.value();
                let value = if atv.value.tag() == x509_cert::der::Tag::BmpString {
                    let u: Vec<u16> = raw.chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
                    String::from_utf16_lossy(&u)
                } else {
                    String::from_utf8_lossy(raw).into_owned()
                };
                let quote = value.is_empty()
                    || value.starts_with(' ')
                    || value.ends_with(' ')
                    || value.contains([',', '+', '=', '"', '\n', '<', '>', '#', ';']);
                let value = if quote { format!("\"{}\"", value.replace('"', "\"\"")) } else { value };
                format!("{}={}", attr_name(&atv.oid.to_string()), value)
            })
            .collect();
        parts.push(atvs.join(" + "));
    }
    parts.join(", ")
}

fn upper_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

fn ticks_of(t: x509_cert::time::Time) -> i64 {
    t.to_unix_duration().as_secs() as i64 * 10_000_000 + 621_355_968_000_000_000
}

fn rsa_val(key: super::asym::Key) -> Val {
    net(NetObj::Rsa(Box::new(super::asym::RsaState { ty: super::asym::RSA, key })))
}

pub fn construct(ty: &'static str, a: &Args) -> R<Val> {
    match ty {
        REQUEST => {
            // (subjectName, RSA key, HashAlgorithmName, RSASignaturePadding)
            let key = match a.net(1).as_ref().map(|k| k.borrow_mut()) {
                Some(mut k) => match &mut *k {
                    NetObj::Rsa(s) => s.private().map(|k| Box::new(k.clone())),
                    _ => Err("Value cannot be null. (Parameter 'key')".into()),
                },
                None => Err("Value cannot be null. (Parameter 'key')".into()),
            };
            let key = match key {
                Ok(k) => k,
                Err(e) => return net_err("CertificateRequest", "CertificateRequest", e),
            };
            let name = |i: usize| match a.raw(i) {
                Some(Val::Net(x)) => match &*x.borrow() {
                    NetObj::Prim(_, v) => v.to_text(),
                    _ => String::new(),
                },
                Some(v) => v.to_text(),
                None => String::new(),
            };
            let exts = Rc::new(RefCell::new(NetObj::List(EXT_COLLECTION, vec![])));
            return Ok(net(NetObj::CertReq(Box::new(CertReq { subject: a.text(0), key, hash: name(2).to_ascii_uppercase(), pss: name(3).eq_ignore_ascii_case("pss"), exts }))));
        }
        BASIC_CONSTRAINTS => {
            // (certificateAuthority, hasPathLengthConstraint, pathLengthConstraint, critical)
            let mut body = vec![];
            if a.bool(0) {
                body.push(tlv(0x01, &[0xff]));
            }
            if a.bool(1) {
                body.push(der_int(a.int(2).max(0) as u64));
            }
            return Ok(extension(BASIC_CONSTRAINTS, "551d13", &seq(&body), a.bool(3)));
        }
        KEY_USAGE => {
            let flags = a.int(0) as u32;
            let mut bytes = vec![(flags & 0xff) as u8];
            if flags & 0x8000 != 0 {
                bytes.push(0x80);
            }
            while bytes.len() > 1 && *bytes.last().unwrap_or(&0) == 0 {
                bytes.pop();
            }
            let last = *bytes.last().unwrap_or(&0);
            let unused = if last == 0 { 0 } else { last.trailing_zeros() as u8 };
            let mut bit = vec![unused];
            bit.extend(bytes);
            return Ok(extension(KEY_USAGE, "551d0f", &tlv(0x03, &bit), a.bool(1)));
        }
        ENHANCED_KEY_USAGE => {
            let oids: Vec<Vec<u8>> = match a.net(0).as_ref().map(|c| c.borrow()) {
                Some(c) => match &*c {
                    NetObj::List(_, v) => v.iter().map(|o| tlv(0x06, &oid_der(&oid_value(o)))).collect(),
                    _ => vec![],
                },
                None => vec![],
            };
            return Ok(extension(ENHANCED_KEY_USAGE, "551d25", &seq(&oids), a.bool(1)));
        }
        OID => return Ok(net(NetObj::Prim(OID, Val::Text(a.text(0))))),
        OID_COLLECTION | EXT_COLLECTION => return Ok(net(NetObj::List(ty, vec![]))),
        _ => {}
    }
    if ty == CERT2 || ty == CERT {
        if a.is_empty() {
            return Ok(net(NetObj::Null(ty)));
        }
        let bytes = super::text::arg_bytes(a.raw(0));
        let password = if a.len() > 1 { a.text(1) } else { String::new() };
        return match load(&bytes, &password) {
            Ok(c) => Ok(net(NetObj::Cert(Box::new(c)))),
            Err(e) => net_err(super::short_name(ty), super::short_name(ty), e),
        };
    }
    Ok(net(NetObj::Null(ty)))
}

pub fn call(n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    if matches!(ty, OID_COLLECTION | EXT_COLLECTION) {
        let mut o = n.borrow_mut();
        let NetObj::List(_, v) = &mut *o else { return Ok(Out::Unknown) };
        return ok(match m {
            "add" => {
                v.push(a.raw(0).cloned().unwrap_or(Val::Void));
                Val::Int(v.len() as i32 - 1)
            }
            "count" => Val::Int(v.len() as i32),
            "item" | "get_item" => v.get(a.int(0).max(0) as usize).cloned().unwrap_or(Val::Void),
            _ => return Ok(Out::Unknown),
        });
    }
    if ty == OID {
        let v = match &*n.borrow() {
            NetObj::Prim(_, v) => v.to_text(),
            _ => String::new(),
        };
        return ok(match m {
            "value" => Val::Text(v),
            "friendlyname" => Val::Text(String::new()),
            _ => return Ok(Out::Unknown),
        });
    }
    if let NetObj::Bag(t, p) = &*n.borrow() {
        if matches!(*t, BASIC_CONSTRAINTS | KEY_USAGE | ENHANCED_KEY_USAGE) {
            return ok(match m {
                "critical" => p.iter().find(|(k, _)| k == "critical").map(|(_, v)| v.clone()).unwrap_or(Val::Bool(false)),
                "rawdata" => p.iter().find(|(k, _)| k == "der").map(|(_, v)| v.clone()).unwrap_or(Val::Void),
                _ => return Ok(Out::Unknown),
            });
        }
    }
    if let NetObj::CertReq(_) = &*n.borrow() {
        return request_call(n, m, a);
    }
    if ty == PROPERTY_INFO {
        let props = match &*n.borrow() {
            NetObj::Bag(_, p) => p.clone(),
            _ => return Ok(Out::Unknown),
        };
        let get = |k: &str| props.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone()).unwrap_or(Val::Void);
        return match m {
            "name" | "propertytype" => ok(get(m)),
            // PropertyInfo.GetValue(obj): the property of the certificate
            "getvalue" => match a.net(0) {
                Some(obj) => {
                    let name = get("name").to_text().to_ascii_lowercase();
                    if name == "archived" {
                        return ok(Val::Bool(false));
                    }
                    let oty = net_type(&obj.borrow());
                    call(&obj, oty, &name, &Args { exprs: &[], v: vec![] })
                }
                None => ok(Val::Void),
            },
            _ => Ok(Out::Unknown),
        };
    }
    if ty == ENUM {
        // Enum.Parse(type, name [, ignoreCase]) with the type as a System.Type or its name
        return match m {
            "parse" | "tryparse" => {
                let tname = match a.raw(0) {
                    Some(Val::Net(x)) => match &*x.borrow() {
                        NetObj::Prim(_, v) => v.to_text(),
                        o => net_type(o).to_string(),
                    },
                    Some(v) => v.to_text(),
                    None => String::new(),
                };
                let t = super::resolve_type_name(&tname);
                let name = a.text(1);
                let ic = a.len() > 2 && a.bool(2);
                let found = super::text::enum_names(t).and_then(|names| {
                    name.split(',')
                        .map(|p| {
                            let p = p.trim();
                            p.parse::<i64>().ok().or_else(|| names.iter().find(|(x, _)| if ic { x.eq_ignore_ascii_case(p) } else { *x == p }).map(|(_, v)| *v))
                        })
                        .try_fold(0i64, |acc, v| v.map(|v| acc | v))
                });
                match found {
                    Some(v) => ok(enum_val(t, v)),
                    None => net_err("Enum", "Parse", format!("Requested value '{name}' was not found.")),
                }
            }
            _ => Ok(Out::Unknown),
        };
    }
    let mut o = n.borrow_mut();
    match &mut *o {
        NetObj::Bag(PUBLIC_KEY, p) => ok(p.iter().find(|(k, _)| k == m).map(|(_, v)| v.clone()).unwrap_or(Val::Void)),
        NetObj::Cert(c) => {
            let Ok(cert) = x509_cert::Certificate::from_der(&c.der) else { return net_err(super::short_name(ty), m, "The certificate data cannot be read.") };
            let tbs = &cert.tbs_certificate;
            let spki_bits = tbs.subject_public_key_info.subject_public_key.raw_bytes();
            let culture = super::culture::current();
            let date = |t: x509_cert::time::Time| super::datetime::NetDateTime { ticks: ticks_of(t), kind: super::datetime::DtKind::Local };
            ok(match m {
                "subject" | "subjectname" => Val::Text(dn(&tbs.subject)),
                "issuer" | "issuername" => Val::Text(dn(&tbs.issuer)),
                "thumbprint" | "getcerthashstring" => Val::Text(upper_hex(&super::crypto::digest("SHA1", &c.der))),
                "getcerthash" => super::text::bytes_val(&super::crypto::digest("SHA1", &c.der)),
                "serialnumber" | "getserialnumberstring" => Val::Text(upper_hex(tbs.serial_number.as_bytes())),
                "getserialnumber" => super::text::bytes_val(&tbs.serial_number.as_bytes().iter().rev().copied().collect::<Vec<_>>()),
                "friendlyname" if a.len() == 1 => {
                    c.friendly_name = a.text(0);
                    Val::Void
                }
                "friendlyname" => Val::Text(c.friendly_name.clone()),
                "notbefore" => net(NetObj::DateTime(date(tbs.validity.not_before))),
                "notafter" => net(NetObj::DateTime(date(tbs.validity.not_after))),
                "geteffectivedatestring" => Val::Text(date(tbs.validity.not_before).format("G", culture)),
                "getexpirationdatestring" => Val::Text(date(tbs.validity.not_after).format("G", culture)),
                "version" => Val::Int(tbs.version as i32 + 1),
                "hasprivatekey" => Val::Bool(c.key.is_some()),
                "privatekey" | "getrsaprivatekey" => match &c.key {
                    Some(k) => rsa_val(super::asym::Key::Private(k.clone())),
                    None => net(NetObj::Null(super::asym::RSA)),
                },
                "getpublickey" => super::text::bytes_val(spki_bits),
                "getrsapublickey" => match RsaPublicKey::from_pkcs1_der(spki_bits) {
                    Ok(k) => rsa_val(super::asym::Key::Public(Box::new(k))),
                    Err(_) => net(NetObj::Null(super::asym::RSA)),
                },
                "publickey" => {
                    let key = match RsaPublicKey::from_pkcs1_der(spki_bits) {
                        Ok(k) => rsa_val(super::asym::Key::Public(Box::new(k))),
                        Err(_) => net(NetObj::Null(super::asym::RSA)),
                    };
                    net(NetObj::Bag(PUBLIC_KEY, vec![("key".into(), key), ("encodedkeyvalue".into(), super::text::bytes_val(spki_bits))]))
                }
                "getrawcertdata" | "rawdata" => super::text::bytes_val(&c.der),
                "export" => {
                    let kind = a.int(0);
                    match kind {
                        1 => super::text::bytes_val(&c.der),
                        3 => super::text::bytes_val(&write_pfx(c)),
                        _ => return net_err(super::short_name(ty), "Export", "Invalid content type."),
                    }
                }
                "verify" => Val::Bool(true),
                "reset" | "dispose" => Val::Void,
                "tostring" => Val::Text(format!("[Subject]\r\n  {}\r\n\r\n[Issuer]\r\n  {}\r\n\r\n[Serial Number]\r\n  {}\r\n\r\n[Thumbprint]\r\n  {}\r\n", dn(&tbs.subject), dn(&tbs.issuer), upper_hex(tbs.serial_number.as_bytes()), upper_hex(&super::crypto::digest("SHA1", &c.der)))),
                _ => return Ok(Out::Unknown),
            })
        }
        _ => Ok(Out::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Base64 PFX that the AL procedure `name` of the X509Certificate2 test codeunit returns.
    fn al_pfx(name: &str) -> Option<Vec<u8>> {
        use base64::Engine;
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../vendor/bcapps/src/System Application/Test/Cryptography Management/src/X509Certificate2Test.Codeunit.al");
        let src = std::fs::read_to_string(path).ok()?;
        let start = src.find(&format!("local procedure {name}()"))?;
        let body = &src[start..start + src[start..].find("end;")?];
        let b64: String = body.split('\'').skip(1).step_by(2).collect();
        base64::engine::general_purpose::STANDARD.decode(b64).ok()
    }

    #[test]
    fn loads_al_test_pfx() {
        let Some(pfx) = al_pfx("GetCertificateBase64") else { return };
        let c = load(&pfx, "").unwrap();
        let cert = x509_cert::Certificate::from_der(&c.der).unwrap();
        assert_eq!(dn(&cert.tbs_certificate.subject), "CN=Joe's-Software-Emporium");
        assert_eq!(dn(&cert.tbs_certificate.issuer), "CN=Root Agency");
        assert_eq!(upper_hex(&super::super::crypto::digest("SHA1", &c.der)), "55A0AE83959E7245E0A04FA1BA5F4024AE0D1235");
        assert!(c.key.is_none());
        // our own export reads back
        let again = load(&write_pfx(&c), "").unwrap();
        assert_eq!(again.der, c.der);
        let Some(pfx2) = al_pfx("GetCertWithPrivateKeyBase64") else { return };
        let c2 = load(&pfx2, "").unwrap();
        assert!(c2.key.is_some());
        assert!(load(&write_pfx(&c2), "").unwrap().key.is_some());
    }

    #[test]
    fn self_signed_round_trip() {
        let key = RsaPrivateKey::new(&mut ::rsa::rand_core::OsRng, 1024).unwrap();
        let exts = Rc::new(RefCell::new(NetObj::List(EXT_COLLECTION, vec![])));
        let req = CertReq { subject: "CN=www.abc.com, C=US".into(), key: Box::new(key), hash: "SHA256".into(), pss: false, exts };
        let n = net(NetObj::CertReq(Box::new(req)));
        let Val::Net(n) = n else { panic!() };
        let now = 638_000_000_000_000_000i64;
        let args = Args { exprs: &[], v: vec![net(NetObj::DateTimeOffset(now, 0)), net(NetObj::DateTimeOffset(now + 864_000_000_000, 0))] };
        let Ok(Out::Val(Val::Net(c))) = request_call(&n, "createselfsigned", &args) else { panic!() };
        let NetObj::Cert(c) = &*c.borrow() else { panic!() };
        let cert = x509_cert::Certificate::from_der(&c.der).unwrap();
        assert_eq!(dn(&cert.tbs_certificate.subject), "CN=www.abc.com, C=US");
        let Ok(Out::Val(csr)) = request_call(&n, "createsigningrequest", &Args { exprs: &[], v: vec![] }) else { panic!() };
        assert!(super::super::text::arg_bytes(Some(&csr)).starts_with(&[0x30, 0x82]));
    }

    #[test]
    fn p12_kdf_vector() {
        // RFC 7292 style check against OpenSSL's PKCS12_key_gen_uni (password "smeg", SHA-1)
        let k = p12_kdf("SHA1", &bmp("smeg"), &hex("0a58cf64530d823f"), 1, 1, 24);
        assert_eq!(upper_hex(&k), "8AAAE6297B6CB04642AB5B077851284EB7128F1A2A7FBCA3");
    }
}

// ---------------------------------------------------------------- CertificateRequest

fn der_int(v: u64) -> Vec<u8> {
    let mut b: Vec<u8> = v.to_be_bytes().iter().copied().skip_while(|x| *x == 0).collect();
    if b.is_empty() || b[0] & 0x80 != 0 {
        b.insert(0, 0);
    }
    tlv(0x02, &b)
}

/// Encoded body of a dotted OID.
fn oid_der(dotted: &str) -> Vec<u8> {
    let arcs: Vec<u64> = dotted.split('.').filter_map(|x| x.trim().parse().ok()).collect();
    if arcs.len() < 2 {
        return vec![];
    }
    let mut out = vec![];
    let mut push = |mut v: u64| {
        let mut tmp = vec![(v & 0x7f) as u8];
        v >>= 7;
        while v > 0 {
            tmp.push(0x80 | (v & 0x7f) as u8);
            v >>= 7;
        }
        tmp.reverse();
        out.extend(tmp);
    };
    push(arcs[0] * 40 + arcs[1]);
    for a in &arcs[2..] {
        push(*a);
    }
    out
}

fn oid_value(v: &Val) -> String {
    match v {
        Val::Net(x) => match &*x.borrow() {
            NetObj::Prim(_, v) => v.to_text(),
            _ => String::new(),
        },
        v => v.to_text(),
    }
}

/// An X509Extension object: its DER (Extension SEQUENCE) and criticality.
fn extension(ty: &'static str, oid_hex: &str, value: &[u8], critical: bool) -> Val {
    let mut parts = vec![tlv(0x06, &hex(oid_hex))];
    if critical {
        parts.push(tlv(0x01, &[0xff]));
    }
    parts.push(tlv(0x04, value));
    net(NetObj::Bag(ty, vec![("der".into(), super::text::bytes_val(&seq(&parts))), ("critical".into(), Val::Bool(critical))]))
}

fn attr_oid(name: &str) -> Option<&'static str> {
    Some(match name.trim().to_ascii_uppercase().as_str() {
        "CN" => "2.5.4.3",
        "O" => "2.5.4.10",
        "OU" => "2.5.4.11",
        "C" => "2.5.4.6",
        "L" => "2.5.4.7",
        "S" | "ST" => "2.5.4.8",
        "STREET" => "2.5.4.9",
        "T" | "TITLE" => "2.5.4.12",
        "G" => "2.5.4.42",
        "SN" => "2.5.4.4",
        "SERIALNUMBER" => "2.5.4.5",
        "DC" => "0.9.2342.19200300.100.1.25",
        "UID" => "0.9.2342.19200300.100.1.1",
        "E" | "EMAIL" | "EMAILADDRESS" => "1.2.840.113549.1.9.1",
        _ => return None,
    })
}

/// X500DistinguishedName(string) as DER: the string lists the most specific RDN first.
fn name_der(dn_text: &str) -> Result<Vec<u8>, String> {
    let mut parts = vec![];
    let (mut cur, mut quoted) = (String::new(), false);
    for ch in dn_text.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                cur.push(ch);
            }
            ',' | ';' if !quoted => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    let mut rdns = vec![];
    for p in parts.iter().rev() {
        let Some((k, v)) = p.split_once('=') else { return Err("The string contains an invalid X500 name attribute key, oid, value or delimiter.".into()) };
        let oid = if let Some(o) = k.trim().strip_prefix("OID.") { o.to_string() } else { attr_oid(k).ok_or("The string contains an invalid X500 name attribute key, oid, value or delimiter.")?.to_string() };
        let mut v = v.trim().to_string();
        if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
            v = v[1..v.len() - 1].replace("\"\"", "\"");
        }
        let printable = v.chars().all(|c| c.is_ascii_alphanumeric() || " '()+,-./:=?".contains(c));
        let tag = if oid == "2.5.4.6" || printable { 0x13 } else if oid.starts_with("1.2.840.113549.1.9.1") { 0x16 } else { 0x0c };
        rdns.push(tlv(0x31, &seq(&[tlv(0x06, &oid_der(&oid)), tlv(tag, v.as_bytes())])));
    }
    Ok(seq(&rdns))
}

fn sign(req: &CertReq, data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    // (AlgorithmIdentifier, signature)
    let (alg_oid, hash) = match req.hash.as_str() {
        "SHA384" => ("2a864886f70d01010c", "SHA384"),
        "SHA512" => ("2a864886f70d01010d", "SHA512"),
        "SHA1" => ("2a864886f70d010105", "SHA1"),
        _ => ("2a864886f70d01010b", "SHA256"),
    };
    if req.pss {
        return Err("PSS signatures are not supported for certificate requests in bc-rust.".into());
    }
    let digest = super::crypto::digest(hash, data);
    let scheme = match hash {
        "SHA384" => ::rsa::Pkcs1v15Sign::new::<sha2_010::Sha384>(),
        "SHA512" => ::rsa::Pkcs1v15Sign::new::<sha2_010::Sha512>(),
        "SHA1" => ::rsa::Pkcs1v15Sign::new::<sha1::Sha1>(),
        _ => ::rsa::Pkcs1v15Sign::new::<sha2_010::Sha256>(),
    };
    let sig = req.key.sign(scheme, &digest).map_err(|e| e.to_string())?;
    Ok((seq(&[tlv(0x06, &hex(alg_oid)), vec![0x05, 0x00]]), sig))
}

fn spki(req: &CertReq) -> Result<Vec<u8>, String> {
    use ::rsa::pkcs8::EncodePublicKey;
    req.key.to_public_key().to_public_key_der().map(|d| d.as_bytes().to_vec()).map_err(|e| e.to_string())
}

fn ext_ders(req: &CertReq) -> Vec<Vec<u8>> {
    match &*req.exts.borrow() {
        NetObj::List(_, v) => v
            .iter()
            .filter_map(|e| match e {
                Val::Net(x) => match &*x.borrow() {
                    NetObj::Bag(_, p) => p.iter().find(|(k, _)| k == "der").map(|(_, d)| super::text::arg_bytes(Some(d))),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// UTCTime before 2050, GeneralizedTime after (RFC 5280).
fn der_time(utc_ticks: i64) -> Vec<u8> {
    let dt = super::datetime::ticks_to_naive(utc_ticks);
    use chrono::Datelike;
    if (1950..2050).contains(&dt.year()) {
        tlv(0x17, dt.format("%y%m%d%H%M%SZ").to_string().as_bytes())
    } else {
        tlv(0x18, dt.format("%Y%m%d%H%M%SZ").to_string().as_bytes())
    }
}

fn utc_ticks_arg(v: Option<&Val>) -> i64 {
    match v {
        Some(Val::Net(x)) => match &*x.borrow() {
            NetObj::DateTimeOffset(t, _) => *t,
            NetObj::DateTime(d) => d.ticks,
            _ => 0,
        },
        Some(Val::DateTime(ms)) => super::datetime::NetDateTime::from_al(*ms).ticks,
        _ => 0,
    }
}

fn request_call(n: &NetRef, m: &str, a: &Args) -> R<Out> {
    let o = n.borrow();
    let NetObj::CertReq(req) = &*o else { return Ok(Out::Unknown) };
    let fail = |member: &str, e: String| net_err::<Out>("CertificateRequest", member, e);
    match m {
        "certificateextensions" => ok(Val::Net(req.exts.clone())),
        "subjectname" => ok(Val::Text(req.subject.clone())),
        "createsigningrequest" => {
            let subject = match name_der(&req.subject) {
                Ok(d) => d,
                Err(e) => return fail("CreateSigningRequest", e),
            };
            let key = match spki(req) {
                Ok(k) => k,
                Err(e) => return fail("CreateSigningRequest", e),
            };
            let exts = ext_ders(req);
            let attrs = if exts.is_empty() {
                tlv(0xa0, &[])
            } else {
                tlv(0xa0, &seq(&[tlv(0x06, &hex("2a864886f70d01090e")), tlv(0x31, &seq(&exts))]))
            };
            let info = seq(&[der_int(0), subject, key, attrs]);
            match sign(req, &info) {
                Ok((alg, sig)) => {
                    let mut bits = vec![0u8];
                    bits.extend(sig);
                    ok(super::text::bytes_val(&seq(&[info, alg, tlv(0x03, &bits)])))
                }
                Err(e) => fail("CreateSigningRequest", e),
            }
        }
        "createselfsigned" => {
            let subject = match name_der(&req.subject) {
                Ok(d) => d,
                Err(e) => return fail("CreateSelfSigned", e),
            };
            let key = match spki(req) {
                Ok(k) => k,
                Err(e) => return fail("CreateSelfSigned", e),
            };
            let (nb, na) = (utc_ticks_arg(a.raw(0)), utc_ticks_arg(a.raw(1)));
            if na < nb {
                return fail("CreateSelfSigned", "The provided notAfter value is earlier than the notBefore value.".into());
            }
            let mut serial = super::cipher::random_bytes(8);
            serial[0] &= 0x7f;
            let alg = match sign(req, b"") {
                Ok((alg, _)) => alg,
                Err(e) => return fail("CreateSelfSigned", e),
            };
            let exts = ext_ders(req);
            let mut tbs = vec![tlv(0xa0, &der_int(2)), tlv(0x02, &serial), alg.clone(), subject.clone(), seq(&[der_time(nb), der_time(na)]), subject, key];
            if !exts.is_empty() {
                tbs.push(tlv(0xa3, &seq(&exts)));
            }
            let tbs = seq(&tbs);
            match sign(req, &tbs) {
                Ok((alg, sig)) => {
                    let mut bits = vec![0u8];
                    bits.extend(sig);
                    let der = seq(&[tbs, alg, tlv(0x03, &bits)]);
                    ok(net(NetObj::Cert(Box::new(CertState { der, friendly_name: String::new(), key: Some(req.key.clone()) }))))
                }
                Err(e) => fail("CreateSelfSigned", e),
            }
        }
        _ => Ok(Out::Unknown),
    }
}
