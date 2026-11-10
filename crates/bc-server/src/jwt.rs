//! Microsoft Entra ID access tokens (JWT, RS256) for `bc serve` (docs/authentication.md).
//!
//! Validation follows what BC's service tier does for on-prem Entra sign-in (ClientServicesCredentialType
//! AccessControlService, ValidAudiences): the signature against the tenant's signing keys (JWKS), the issuer of
//! the configured tenant, one of the configured audiences, and the token's lifetime. Only RS256 is accepted;
//! `alg: none`, HMAC and keys shorter than 2048 bits are refused.

use base64::Engine;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::signature::Verifier;
use rsa::{BigUint, RsaPublicKey};
use serde_json::Value as J;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Accepted clock skew between Entra and this host, seconds.
pub const LEEWAY_S: i64 = 300;

#[derive(Clone, Debug)]
pub struct EntraConfig {
    /// The directory (tenant) id, a GUID.
    pub tenant: String,
    /// Accepted `aud` values: the application id URI and/or client id (BC's ValidAudiences, AppIdUri).
    pub audiences: Vec<String>,
    /// Where the signing keys come from: an https URL or a local file (tests, air-gapped hosts).
    pub jwks: String,
}

impl EntraConfig {
    pub fn new(tenant: &str, audiences: Vec<String>, jwks: Option<String>) -> EntraConfig {
        let tenant = tenant.to_ascii_lowercase();
        let jwks = jwks.unwrap_or_else(|| format!("https://login.microsoftonline.com/{tenant}/discovery/v2.0/keys"));
        EntraConfig { tenant, audiences, jwks }
    }

    /// v2.0 and v1.0 issuers of the tenant.
    fn issuer_ok(&self, iss: &str) -> bool {
        let t = &self.tenant;
        iss.eq_ignore_ascii_case(&format!("https://login.microsoftonline.com/{t}/v2.0")) || iss.eq_ignore_ascii_case(&format!("https://sts.windows.net/{t}/"))
    }
}

/// The claims bc-rust maps to a User.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Claims {
    /// Object id of the user or service principal (User Property."Authentication Object ID").
    pub oid: String,
    /// upn, preferred_username, email or unique_name (User."Authentication Email").
    pub upn: String,
    /// The client application (appid in v1 tokens, azp in v2), for app-only tokens (User."Application ID").
    pub app_id: String,
    /// Application permission token (no user): `idtyp = app`, or no `scp` and no user name.
    pub app_only: bool,
}

#[derive(Debug, PartialEq)]
pub enum JwtError {
    Malformed(&'static str),
    Algorithm(String),
    UnknownKey(String),
    Signature,
    Expired,
    NotYetValid,
    Issuer(String),
    Audience,
    Tenant,
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JwtError::Malformed(m) => write!(f, "malformed token: {m}"),
            JwtError::Algorithm(a) => write!(f, "algorithm {a} is not accepted (RS256 only)"),
            JwtError::UnknownKey(k) => write!(f, "no signing key with kid '{k}'"),
            JwtError::Signature => write!(f, "invalid signature"),
            JwtError::Expired => write!(f, "token expired"),
            JwtError::NotYetValid => write!(f, "token not valid yet"),
            JwtError::Issuer(i) => write!(f, "issuer '{i}' is not the configured tenant"),
            JwtError::Audience => write!(f, "audience not accepted"),
            JwtError::Tenant => write!(f, "tenant id does not match"),
        }
    }
}

fn b64url(s: &str) -> Result<Vec<u8>, JwtError> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).map_err(|_| JwtError::Malformed("base64url"))
}

/// Signing keys by kid.
#[derive(Clone, Debug, Default)]
pub struct Jwks {
    pub keys: HashMap<String, RsaPublicKey>,
}

/// Parse a JWKS document (`{"keys": [{"kty": "RSA", "kid", "n", "e", "use"?}]}`); keys that are not RSA
/// signing keys of at least 2048 bits are skipped.
pub fn parse_jwks(text: &str) -> Result<Jwks, String> {
    let v: J = serde_json::from_str(text).map_err(|e| format!("JWKS is not JSON: {e}"))?;
    let mut out = Jwks::default();
    for k in v.get("keys").and_then(J::as_array).ok_or("JWKS has no keys")? {
        let s = |n: &str| k.get(n).and_then(J::as_str).unwrap_or_default();
        if s("kty") != "RSA" || !matches!(s("use"), "" | "sig") || s("kid").is_empty() {
            continue;
        }
        let (Ok(n), Ok(e)) = (b64url(s("n")), b64url(s("e"))) else { continue };
        let Ok(key) = RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e)) else { continue };
        if rsa::traits::PublicKeyParts::n(&key).bits() < 2048 {
            continue;
        }
        out.keys.insert(s("kid").to_string(), key);
    }
    Ok(out)
}

/// The `kid` of a token's header, without validating anything.
pub fn token_kid(token: &str) -> Option<String> {
    let h = b64url(token.split('.').next()?).ok()?;
    serde_json::from_slice::<J>(&h).ok()?.get("kid")?.as_str().map(str::to_string)
}

/// Validate an access token at time `now` (Unix seconds).
pub fn validate(token: &str, keys: &Jwks, cfg: &EntraConfig, now: i64) -> Result<Claims, JwtError> {
    validate_with(token, keys, cfg, now, 0)
}

/// `validate` that accepts the token for `extra_s` seconds past its expiry: the web server re-sends the
/// id_token of the browser sign-in with every new service tier connection and never refreshes it, and
/// BC accepts it for ExtendedSecurityTokenLifetime hours longer (NavJwtSecurityTokenFactory).
pub fn validate_with(token: &str, keys: &Jwks, cfg: &EntraConfig, now: i64, extra_s: i64) -> Result<Claims, JwtError> {
    let mut parts = token.split('.');
    let (Some(h64), Some(p64), Some(s64), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return Err(JwtError::Malformed("expected three parts")) };
    let header: J = serde_json::from_slice(&b64url(h64)?).map_err(|_| JwtError::Malformed("header"))?;
    let alg = header.get("alg").and_then(J::as_str).unwrap_or_default();
    if alg != "RS256" {
        return Err(JwtError::Algorithm(alg.to_string()));
    }
    let kid = header.get("kid").and_then(J::as_str).ok_or(JwtError::Malformed("no kid"))?;
    let key = keys.keys.get(kid).ok_or_else(|| JwtError::UnknownKey(kid.to_string()))?;
    let sig = Signature::try_from(b64url(s64)?.as_slice()).map_err(|_| JwtError::Signature)?;
    let signed = &token[..h64.len() + 1 + p64.len()];
    VerifyingKey::<sha2_010::Sha256>::new(key.clone()).verify(signed.as_bytes(), &sig).map_err(|_| JwtError::Signature)?;
    let c: J = serde_json::from_slice(&b64url(p64)?).map_err(|_| JwtError::Malformed("payload"))?;
    let num = |n: &str| c.get(n).and_then(J::as_i64);
    let s = |n: &str| c.get(n).and_then(J::as_str).unwrap_or_default().to_string();
    let exp = num("exp").ok_or(JwtError::Malformed("no exp"))?;
    if now >= exp + LEEWAY_S + extra_s.max(0) {
        return Err(JwtError::Expired);
    }
    if num("nbf").is_some_and(|nbf| now + LEEWAY_S < nbf) {
        return Err(JwtError::NotYetValid);
    }
    let iss = s("iss");
    if !cfg.issuer_ok(&iss) {
        return Err(JwtError::Issuer(iss));
    }
    if c.get("tid").is_some() && !s("tid").eq_ignore_ascii_case(&cfg.tenant) {
        return Err(JwtError::Tenant);
    }
    let aud_ok = |a: &str| cfg.audiences.iter().any(|x| x.trim_end_matches('/').eq_ignore_ascii_case(a.trim_end_matches('/')));
    let aud = match c.get("aud") {
        Some(J::String(a)) => aud_ok(a),
        Some(J::Array(v)) => v.iter().filter_map(J::as_str).any(aud_ok),
        _ => false,
    };
    if !aud {
        return Err(JwtError::Audience);
    }
    let upn = ["upn", "preferred_username", "email", "unique_name"].iter().map(|n| s(n)).find(|v| !v.is_empty()).unwrap_or_default();
    let app_id = [s("azp"), s("appid")].into_iter().find(|v| !v.is_empty()).unwrap_or_default();
    let app_only = s("idtyp") == "app" || (c.get("scp").is_none() && upn.is_empty());
    Ok(Claims { oid: s("oid"), upn, app_id, app_only })
}

/// The tenant's signing keys, fetched on first use, again after a day, and at most once a minute
/// when a token names a kid we don't know (Entra rolls its keys).
pub struct JwksCache {
    src: String,
    state: std::sync::Mutex<(Option<Instant>, Jwks)>,
}

impl JwksCache {
    pub fn new(src: &str) -> JwksCache {
        JwksCache { src: src.to_string(), state: std::sync::Mutex::new((None, Jwks::default())) }
    }

    /// A cache with fixed keys (tests).
    pub fn fixed(keys: Jwks) -> JwksCache {
        JwksCache { src: String::new(), state: std::sync::Mutex::new((Some(Instant::now()), keys)) }
    }

    fn load(&self) -> Result<Jwks, String> {
        if self.src.starts_with("https://") {
            let body = ureq::get(&self.src).timeout(Duration::from_secs(10)).call().map_err(|e| format!("JWKS fetch {}: {e}", self.src))?.into_string().map_err(|e| e.to_string())?;
            parse_jwks(&body)
        } else {
            parse_jwks(&std::fs::read_to_string(&self.src).map_err(|e| format!("JWKS file {}: {e}", self.src))?)
        }
    }

    /// Keys that include `kid` if the source has it.
    pub fn keys_for(&self, kid: &str) -> Result<Jwks, String> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let age = st.0.map(|t| t.elapsed());
        let stale = age.is_none_or(|a| a > Duration::from_secs(24 * 3600));
        let missing = !st.1.keys.contains_key(kid) && age.is_none_or(|a| a > Duration::from_secs(60));
        if !self.src.is_empty() && (stale || missing) {
            match self.load() {
                Ok(k) => *st = (Some(Instant::now()), k),
                Err(e) if st.1.keys.is_empty() => return Err(e),
                Err(e) => eprintln!("bc auth: {e} (keeping the cached keys)"),
            }
        }
        Ok(st.1.clone())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use rsa::signature::{SignatureEncoding, Signer};
    use rsa::traits::PublicKeyParts;
    use rsa::RsaPrivateKey;
    use serde_json::json;

    pub const TENANT: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";
    pub const AUD: &str = "api://bc-rust-test";

    /// A fresh RSA key pair per test run (no key material in the repo).
    pub fn keypair() -> RsaPrivateKey {
        RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap()
    }

    fn enc(b: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    }

    pub fn jwks_json(k: &RsaPrivateKey, kid: &str) -> String {
        let p = k.to_public_key();
        json!({"keys": [{"kty": "RSA", "use": "sig", "kid": kid, "n": enc(&p.n().to_bytes_be()), "e": enc(&p.e().to_bytes_be())}]}).to_string()
    }

    pub fn sign(k: &RsaPrivateKey, header: J, claims: J) -> String {
        let input = format!("{}.{}", enc(header.to_string().as_bytes()), enc(claims.to_string().as_bytes()));
        let sk = rsa::pkcs1v15::SigningKey::<sha2_010::Sha256>::new(k.clone());
        format!("{input}.{}", enc(&sk.sign(input.as_bytes()).to_bytes()))
    }

    pub fn claims(now: i64) -> J {
        json!({"iss": format!("https://login.microsoftonline.com/{TENANT}/v2.0"), "aud": AUD, "tid": TENANT, "exp": now + 3600, "nbf": now - 10, "iat": now - 10,
               "oid": "0b5c2ce0-5a43-4f2b-9b6c-1d2e3f405162", "preferred_username": "clerk@contoso.example", "scp": "Financials.ReadWrite.All", "azp": "11111111-2222-3333-4444-555555555555"})
    }

    #[test]
    fn rs256_tokens() {
        let k = keypair();
        let keys = parse_jwks(&jwks_json(&k, "k1")).unwrap();
        let cfg = EntraConfig::new(TENANT, vec![AUD.into()], None);
        let now = 1_800_000_000;
        let hdr = json!({"alg": "RS256", "typ": "JWT", "kid": "k1"});
        let tok = sign(&k, hdr.clone(), claims(now));
        assert_eq!(token_kid(&tok).as_deref(), Some("k1"));
        let c = validate(&tok, &keys, &cfg, now).unwrap();
        assert_eq!(c, Claims { oid: "0b5c2ce0-5a43-4f2b-9b6c-1d2e3f405162".into(), upn: "clerk@contoso.example".into(), app_id: "11111111-2222-3333-4444-555555555555".into(), app_only: false });
        // v1 issuer, audience in an array, trailing slash
        let mut v1 = claims(now);
        v1["iss"] = json!(format!("https://sts.windows.net/{TENANT}/"));
        v1["aud"] = json!(["other", format!("{AUD}/")]);
        assert!(validate(&sign(&k, hdr.clone(), v1), &keys, &cfg, now).is_ok());
        // lifetime with leeway
        assert!(validate(&tok, &keys, &cfg, now + 3600 + LEEWAY_S - 1).is_ok());
        assert_eq!(validate(&tok, &keys, &cfg, now + 3600 + LEEWAY_S), Err(JwtError::Expired));
        // the web client's extended lifetime
        assert!(validate_with(&tok, &keys, &cfg, now + 3600 + LEEWAY_S + 100, 3600).is_ok());
        assert_eq!(validate_with(&tok, &keys, &cfg, now + 3600 + LEEWAY_S + 3600, 3600), Err(JwtError::Expired));
        let mut early = claims(now);
        early["nbf"] = json!(now + LEEWAY_S + 5);
        assert_eq!(validate(&sign(&k, hdr.clone(), early), &keys, &cfg, now), Err(JwtError::NotYetValid));
        // wrong tenant, issuer, audience
        let mut c2 = claims(now);
        c2["iss"] = json!("https://login.microsoftonline.com/00000000-0000-0000-0000-000000000000/v2.0");
        assert!(matches!(validate(&sign(&k, hdr.clone(), c2), &keys, &cfg, now), Err(JwtError::Issuer(_))));
        let mut c3 = claims(now);
        c3["tid"] = json!("00000000-0000-0000-0000-000000000000");
        assert_eq!(validate(&sign(&k, hdr.clone(), c3), &keys, &cfg, now), Err(JwtError::Tenant));
        let mut c4 = claims(now);
        c4["aud"] = json!("https://graph.microsoft.com");
        assert_eq!(validate(&sign(&k, hdr.clone(), c4), &keys, &cfg, now), Err(JwtError::Audience));
        // app-only token
        let mut app = claims(now);
        app.as_object_mut().unwrap().remove("scp");
        app.as_object_mut().unwrap().remove("preferred_username");
        app["idtyp"] = json!("app");
        assert!(validate(&sign(&k, hdr.clone(), app), &keys, &cfg, now).unwrap().app_only);
    }

    #[test]
    fn rejects_forgeries() {
        let k = keypair();
        let other = keypair();
        let keys = parse_jwks(&jwks_json(&k, "k1")).unwrap();
        let cfg = EntraConfig::new(TENANT, vec![AUD.into()], None);
        let now = 1_800_000_000;
        let hdr = json!({"alg": "RS256", "kid": "k1"});
        // signed by another key under our kid
        assert_eq!(validate(&sign(&other, hdr.clone(), claims(now)), &keys, &cfg, now), Err(JwtError::Signature));
        // unknown kid
        assert!(matches!(validate(&sign(&k, json!({"alg": "RS256", "kid": "k2"}), claims(now)), &keys, &cfg, now), Err(JwtError::UnknownKey(_))));
        // payload changed after signing
        let tok = sign(&k, hdr.clone(), claims(now));
        let mut c = claims(now);
        c["preferred_username"] = json!("admin@contoso.example");
        let parts: Vec<&str> = tok.split('.').collect();
        let forged = format!("{}.{}.{}", parts[0], enc(c.to_string().as_bytes()), parts[2]);
        assert_eq!(validate(&forged, &keys, &cfg, now), Err(JwtError::Signature));
        // alg none and HS256 (key confusion) are refused before any key is used
        let none = format!("{}.{}.", enc(json!({"alg": "none", "kid": "k1"}).to_string().as_bytes()), enc(claims(now).to_string().as_bytes()));
        assert_eq!(validate(&none, &keys, &cfg, now), Err(JwtError::Algorithm("none".into())));
        let hs = format!("{}.{}.AAAA", enc(json!({"alg": "HS256", "kid": "k1"}).to_string().as_bytes()), enc(claims(now).to_string().as_bytes()));
        assert_eq!(validate(&hs, &keys, &cfg, now), Err(JwtError::Algorithm("HS256".into())));
        assert!(matches!(validate("a.b", &keys, &cfg, now), Err(JwtError::Malformed(_))));
        // short keys are not loaded
        let small = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
        assert!(parse_jwks(&jwks_json(&small, "s")).unwrap().keys.is_empty());
    }

    #[test]
    fn jwks_cache_fixed() {
        let k = keypair();
        let c = JwksCache::fixed(parse_jwks(&jwks_json(&k, "kid-a")).unwrap());
        assert!(c.keys_for("kid-a").unwrap().keys.contains_key("kid-a"));
        // a fixed cache has no source to refresh from
        assert!(!c.keys_for("kid-b").unwrap().keys.contains_key("kid-b"));
    }
}
