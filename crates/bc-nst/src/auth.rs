//! Sign-in validation for Microsoft's web server (docs/authentication.md), on bc-server's credential store
//! (`bc_server::auth`, `BC_AUTH=NavUserPassword`; off by default, then any password is accepted as before).
//!
//! With NavUserPassword the web server never sends the password: it upper-cases the user name and sends
//! `appId\host\tenant\<prehash>`, where prehash = BC's GeneratePasswordHash(password), both as
//! `Authorization: Basic` on the WebSocket upgrade and as OpenConnection's Credentials
//! (ConnectionEstablisher.ApplyCredentials). The service tier takes the last part
//! (ClientServicesUserNamePasswordValidator.ExtractPropertiesFromPassword) and compares
//! GenerateSaltedPasswordHash(prehash, User Security ID) with "User Property".Password (NavUser.TryAuthenticate).
//! A rejected upgrade is a 401 without WWW-Authenticate; the web server shows it as invalid credentials.
//!
//! With Microsoft Entra ID (`BC_AUTH=Entra`, web server ClientServicesCredentialType AccessControlService)
//! the web server signs the browser in with OpenID Connect and sends the user name `" WSFED:{GUID}"`. The
//! upgrade's Basic password is `appId\host\tenant` without a token; OpenConnection's Credentials.Password
//! is `appId\host\tenant\<blob>`, the blob being base64 of .NET BinaryWriter strings with the sign-in's
//! id_token third (NavWSFederationCredentialConverter). BC validates it only in OpenConnection, and so do we.

use crate::rpc::NstState;
use al_runtime::userauth::Secret;
use al_runtime::Session;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bc_server::auth::{self, AuthError, Credential, Identity};
use std::sync::Arc;

/// The lockout's client key: every sign-in arrives through the web server, which doesn't pass the
/// browser's address on, so failures count per user name.
const CLIENT: &str = "web-server";

/// The pre-hash in a web server password `appId\host\tenant\prehash`.
pub fn prehash_of(password: &str) -> &str {
    password.rsplit('\\').next().unwrap_or(password)
}

/// The web server's Entra user name, `" WSFED:{GUID}"` (TokenCredentialProvider).
pub fn is_entra_user(user: &str) -> bool {
    user.trim_start().get(..6).is_some_and(|p| p.eq_ignore_ascii_case("WSFED:"))
}

/// The id_token in the web server's Entra credential blob: base64 of BinaryWriter strings (7-bit length
/// prefix, UTF-8) userName, authKey, token, federationOrigin, tokenId, tokenType, ...
pub fn entra_token(blob: &str) -> Option<String> {
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD.decode(blob.trim()).ok()?;
    let mut pos = 0;
    let mut strings = vec![];
    while strings.len() < 3 {
        let (mut len, mut shift) = (0usize, 0);
        loop {
            let b = *raw.get(pos)?;
            pos += 1;
            len |= ((b & 0x7f) as usize) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 28 {
                return None;
            }
        }
        strings.push(std::str::from_utf8(raw.get(pos..pos + len)?).ok()?.to_string());
        pos += len;
    }
    let t = strings.pop()?;
    (t.split('.').count() == 3).then_some(t)
}

/// An Entra web client sign-in (blocking): the id_token in OpenConnection's password, valid for
/// ExtendedSecurityTokenLifetime past its expiry, mapped to a User.
pub fn validate_entra(st: &NstState, password: &str) -> Result<Identity, AuthError> {
    let a = auth::authenticator();
    if a.cfg.entra.is_none() {
        return Err(AuthError::Missing);
    }
    let token = entra_token(prehash_of(password)).ok_or(AuthError::Invalid)?;
    let mut s = Session::try_new(st.rt, &st.cfg.company).map_err(|e| AuthError::Unavailable(e.to_string()))?;
    let r = a.authenticate_entra(&mut s, &token, None, a.cfg.entra_extended_s);
    s.rollback();
    r
}

/// Check a user name and web server password (blocking: reads User / User Property, runs PBKDF2).
pub fn validate(st: &NstState, user: &str, password: &str) -> Result<Identity, AuthError> {
    let a = auth::authenticator();
    if !a.cfg.passwords {
        return Err(AuthError::Missing);
    }
    let mut s = Session::try_new(st.rt, &st.cfg.company).map_err(|e| AuthError::Unavailable(e.to_string()))?;
    let r = a.authenticate(&mut s, user, Secret::PreHash(prehash_of(password)), CLIENT);
    s.rollback();
    r
}

/// The WebSocket upgrade's credentials: Ok(None) while `BC_AUTH` is off, Ok(user) when they validate,
/// else the 401 to answer with. An Entra upgrade carries no token: its Authorization header is removed,
/// so the session has no validated user until OpenConnection checks the token.
pub async fn check_upgrade(st: &Arc<NstState>, headers: &mut HeaderMap) -> Result<Option<Identity>, Response> {
    if !auth::enabled() {
        return Ok(None);
    }
    let Some(Credential::Basic(user, pw)) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(auth::parse_authorization) else {
        eprintln!("bc-nst: rejecting WebSocket upgrade without Basic credentials (BC_AUTH is on)");
        return Err(StatusCode::UNAUTHORIZED.into_response());
    };
    if is_entra_user(&user) {
        if auth::authenticator().cfg.entra.is_none() {
            eprintln!("bc-nst: rejecting an Entra sign-in: BC_AUTH has no Entra");
            return Err(StatusCode::UNAUTHORIZED.into_response());
        }
        headers.remove(header::AUTHORIZATION);
        return Ok(None);
    }
    let st2 = st.clone();
    let u2 = user.clone();
    match tokio::task::spawn_blocking(move || validate(&st2, &u2, &pw)).await {
        Ok(Ok(id)) => Ok(Some(id)),
        Ok(Err(e)) => {
            eprintln!("bc-nst: sign-in of '{user}' rejected: {e:?}");
            Err(StatusCode::UNAUTHORIZED.into_response())
        }
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()),
    }
}

/// OpenConnection while `BC_AUTH` is on: the user the upgrade validated must be the user in Credentials;
/// without a validated upgrade the Credentials are checked here (NavUserPassword or an Entra id_token).
/// Returns the user to run as.
pub fn open_connection(st: &NstState, cred_user: Option<&str>, cred_password: Option<&str>, upgrade_user: Option<&str>) -> Result<String, String> {
    decide(cred_user, cred_password.unwrap_or_default(), upgrade_user, &|u: &str, p: &str| {
        let r = if is_entra_user(u) { validate_entra(st, p) } else { validate(st, u, p) };
        r.map(|id| id.user_name).map_err(|e| {
            eprintln!("bc-nst: OpenConnection of '{}' rejected: {e:?}", u.trim());
            e
        })
    })
}

/// BC's text for refused credentials [src:X04].
const REJECTED: &str = "The server has rejected the client credentials.";

/// `open_connection`'s rule with the credential check `check(user, password)` given.
fn decide(cred_user: Option<&str>, password: &str, upgrade_user: Option<&str>, check: &dyn Fn(&str, &str) -> Result<String, AuthError>) -> Result<String, String> {
    match (upgrade_user, cred_user) {
        (Some(u), Some(c)) if !u.eq_ignore_ascii_case(c) => Err(REJECTED.into()),
        (Some(u), _) => Ok(u.to_string()),
        (None, Some(c)) => check(c, password).map_err(|_| REJECTED.to_string()),
        (None, None) => Err(REJECTED.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The web server's Entra blob (BinaryWriter strings, 7-bit length prefixes).
    fn blob(parts: &[&str]) -> String {
        use base64::Engine;
        let mut b = vec![];
        for p in parts {
            let mut n = p.len();
            loop {
                let byte = (n & 0x7f) as u8;
                n >>= 7;
                b.push(if n > 0 { byte | 0x80 } else { byte });
                if n == 0 {
                    break;
                }
            }
            b.extend_from_slice(p.as_bytes());
        }
        base64::engine::general_purpose::STANDARD.encode(b)
    }

    #[test]
    fn entra_credentials() {
        assert!(is_entra_user(" WSFED:{0B5C2CE0-5A43-4F2B-9B6C-1D2E3F405162}"));
        assert!(is_entra_user("wsfed:x"));
        assert!(!is_entra_user("ADMIN"));
        // a token longer than 127 bytes needs a two-byte length
        let jwt = format!("eyJhbGciOiJSUzI1NiJ9.{}.sig", "x".repeat(300));
        let b = blob(&["", "", &jwt, "", "8b1f0d2e-0000-0000-0000-000000000000", "BinaryJwt", "home", "{\"a\":\"code\"}"]);
        assert_eq!(entra_token(&b), Some(jwt.clone()));
        assert_eq!(entra_token(prehash_of(&format!("app\\host\\default\\{b}"))), Some(jwt));
        assert_eq!(entra_token(&blob(&["", "", "not-a-jwt"])), None);
        assert_eq!(entra_token("!!"), None);
        assert_eq!(entra_token(&blob(&["", ""])), None);
    }

    #[test]
    fn open_connection_rules() {
        let ok = |u: &str, _: &str| -> Result<String, AuthError> { Ok(format!("CHECKED-{}", u.trim())) };
        let no = |_: &str, _: &str| -> Result<String, AuthError> { Err(AuthError::Invalid) };
        // the validated upgrade user, also when Credentials name them in other case
        assert_eq!(decide(Some("admin"), "x", Some("ADMIN"), &no), Ok("ADMIN".into()));
        assert_eq!(decide(None, "", Some("ADMIN"), &no), Ok("ADMIN".into()));
        // Credentials for another user than the upgrade validated
        assert!(decide(Some("BOB"), "x", Some("ADMIN"), &ok).is_err());
        // no validated upgrade (Entra): the Credentials decide
        assert_eq!(decide(Some(" WSFED:{G}"), "a\\h\\t\\blob", None, &ok), Ok("CHECKED-WSFED:{G}".into()));
        assert!(decide(Some(" WSFED:{G}"), "a\\h\\t\\blob", None, &no).is_err());
        assert!(decide(None, "", None, &ok).is_err());
    }

    #[test]
    fn web_server_password_parts() {
        assert_eq!(prehash_of("app\\host\\default\\z9Ecv6HC8K9I51R+uOrvRQ==-V3"), "z9Ecv6HC8K9I51R+uOrvRQ==-V3");
        assert_eq!(prehash_of("\\\\\\abc"), "abc");
        assert_eq!(prehash_of("plain"), "plain");
    }
}
