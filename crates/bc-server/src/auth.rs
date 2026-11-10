//! User authentication for `bc serve` and bc-nst (docs/authentication.md). Off unless `BC_AUTH` is set.
//!
//! - `BC_AUTH=NavUserPassword`: HTTP Basic with the user name and the password or the web service access key,
//!   checked against User / "User Property" in BC's own format (al_runtime::userauth). bc-nst checks the
//!   pre-hashed password that Microsoft's web server sends.
//! - `BC_AUTH=Entra` (with `BC_AUTH_ENTRA_TENANT`, `BC_AUTH_ENTRA_AUDIENCE`, optional `BC_AUTH_ENTRA_JWKS`):
//!   `Authorization: Bearer <access token>` (crate::jwt), mapped to a User by object id, user name or app id.
//! - Both: `BC_AUTH=NavUserPassword,Entra`.
//!
//! A request that authenticates runs its session as that user (`UserId()`), and `login_permissions` loads the
//! user's permission sets (enforced with `BC_PERMISSIONS=enforce`). Failed attempts are rate limited per user
//! name and client address (`BC_AUTH_LOCKOUT=<failures>/<window s>/<lock s>`, default 5/300/300).

use al_runtime::userauth::{self, Secret, UserCreds};
use al_runtime::{Runtime, Session};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long a verified password stays verified without running PBKDF2 again (the stored hash is part of
/// the cache key, so a password change takes effect at once; State and expiry are read on every request).
const VERIFIED_TTL: Duration = Duration::from_secs(300);
/// Failures from one client address over all user names, as a multiple of the per-user limit.
const ADDRESS_FACTOR: u32 = 4;

#[derive(Clone, Debug, PartialEq)]
pub struct Lockout {
    /// Failed attempts within `window` that lock a (user name, client) pair; 0 = no lockout.
    pub failures: u32,
    pub window: Duration,
    pub lock: Duration,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// NavUserPassword: Basic auth with password or web service access key, web-client pre-hashes.
    pub passwords: bool,
    pub entra: Option<crate::jwt::EntraConfig>,
    pub lockout: Lockout,
    /// Take the client address from X-Forwarded-For (only behind a proxy that sets it).
    pub trust_proxy: bool,
    /// Seconds a web client's Entra id_token stays valid past its expiry (BC's ExtendedSecurityTokenLifetime,
    /// `BC_AUTH_ENTRA_EXTENDED_LIFETIME` in hours, default 1).
    pub entra_extended_s: i64,
}

impl Config {
    pub fn off() -> Config {
        Config { passwords: false, entra: None, lockout: Lockout { failures: 5, window: Duration::from_secs(300), lock: Duration::from_secs(300) }, trust_proxy: false, entra_extended_s: 3600 }
    }

    pub fn enabled(&self) -> bool {
        self.passwords || self.entra.is_some()
    }

    /// The configuration from `BC_AUTH*` variables (`var` reads one).
    pub fn parse(var: &dyn Fn(&str) -> Option<String>) -> Result<Config, String> {
        let mut c = Config::off();
        let modes = var("BC_AUTH").unwrap_or_default();
        for m in modes.split(',').map(|m| m.trim().to_ascii_lowercase()).filter(|m| !m.is_empty()) {
            match m.as_str() {
                "off" | "none" => {}
                "navuserpassword" | "password" | "basic" => c.passwords = true,
                "entra" | "aad" | "accesscontrolservice" => {
                    let tenant = var("BC_AUTH_ENTRA_TENANT").filter(|t| !t.trim().is_empty()).ok_or("BC_AUTH=Entra needs BC_AUTH_ENTRA_TENANT (the directory id)")?;
                    let aud: Vec<String> = var("BC_AUTH_ENTRA_AUDIENCE").unwrap_or_default().split([',', ';']).map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
                    if aud.is_empty() {
                        return Err("BC_AUTH=Entra needs BC_AUTH_ENTRA_AUDIENCE (application id URI or client id)".into());
                    }
                    c.entra = Some(crate::jwt::EntraConfig::new(tenant.trim(), aud, var("BC_AUTH_ENTRA_JWKS").filter(|j| !j.is_empty())));
                }
                other => return Err(format!("BC_AUTH: unknown mode '{other}' (use NavUserPassword, Entra or off)")),
            }
        }
        if let Some(l) = var("BC_AUTH_LOCKOUT") {
            let n: Vec<u64> = l.split('/').map(|x| x.trim().parse::<u64>().map_err(|_| format!("BC_AUTH_LOCKOUT: '{l}' is not <failures>/<window s>/<lock s>"))).collect::<Result<_, _>>()?;
            match n[..] {
                [0, ..] => c.lockout.failures = 0,
                [f, w, k] => c.lockout = Lockout { failures: f as u32, window: Duration::from_secs(w), lock: Duration::from_secs(k) },
                _ => return Err(format!("BC_AUTH_LOCKOUT: '{l}' is not <failures>/<window s>/<lock s>")),
            }
        }
        c.trust_proxy = var("BC_AUTH_TRUST_PROXY").is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"));
        if let Some(h) = var("BC_AUTH_ENTRA_EXTENDED_LIFETIME") {
            let h: f64 = h.trim().parse().map_err(|_| format!("BC_AUTH_ENTRA_EXTENDED_LIFETIME: '{h}' is not a number of hours"))?;
            c.entra_extended_s = (h * 3600.0) as i64;
        }
        Ok(c)
    }
}

static AUTH: OnceLock<Authenticator> = OnceLock::new();

/// The process-wide authenticator, configured from the environment on first use. A configuration error
/// is fatal: a server that was asked to authenticate must not start open.
pub fn authenticator() -> &'static Authenticator {
    AUTH.get_or_init(|| match Config::parse(&|n| std::env::var(n).ok()) {
        Ok(c) => {
            // NavUserAccountHelper.IsUserNamePasswordAuthentication & co. (LogInManagement's forced
            // password change runs only with NavUserPassword). Opt-in for now: on bc-nst's company open,
            // LogInManagement.LogInStart then fails with "Void.FindFirst" on its global User record
            // (an interpreter bug, docs/authentication.md), which would stop every web client sign-in.
            let report_type = std::env::var("BC_AUTH_PASSWORD_CHANGE").is_ok_and(|v| v == "1");
            userauth::set_credential_type(if c.passwords && report_type {
                userauth::CredentialType::NavUserPassword
            } else if c.entra.is_some() && !c.passwords && report_type {
                userauth::CredentialType::AccessControlService
            } else {
                userauth::CredentialType::Windows
            });
            if c.enabled() {
                eprintln!("authentication: {}{}{} (lockout {:?})", if c.passwords { "NavUserPassword" } else { "" }, if c.passwords && c.entra.is_some() { " + " } else { "" }, c.entra.as_ref().map(|e| format!("Entra tenant {}", e.tenant)).unwrap_or_default(), c.lockout);
            }
            Authenticator::new(c)
        }
        Err(e) => {
            eprintln!("authentication configuration error: {e}");
            std::process::exit(2);
        }
    })
}

/// Whether requests must authenticate (`BC_AUTH`).
pub fn enabled() -> bool {
    authenticator().cfg.enabled()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Method {
    /// Basic: password or web service access key.
    Basic,
    /// Microsoft's web server: the NavUserPassword pre-hash.
    WebClient,
    /// Entra access token.
    Bearer,
}

/// An authenticated user.
#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    /// User."User Name" as stored.
    pub user_name: String,
    pub user_sid: [u8; 16],
    pub method: Method,
}

#[derive(Debug, PartialEq)]
pub enum AuthError {
    /// No usable credentials.
    Missing,
    /// Wrong user name, password, key or token (one answer for all, so names can't be probed).
    Invalid,
    /// Too many failures; retry after.
    Locked(Duration),
    /// The user store could not be read.
    Unavailable(String),
}

impl AuthError {
    /// The HTTP answer. 401 carries the schemes that are enabled.
    pub fn response(&self, cfg: &Config) -> Response {
        let body = |code: &str, msg: &str| axum::Json(serde_json::json!({"error": {"code": code, "message": msg}}));
        match self {
            AuthError::Missing | AuthError::Invalid => {
                // text and error code as BC's API answers a bad sign-in [src:X04]
                let mut r = (StatusCode::UNAUTHORIZED, body("Authentication_InvalidCredentials", "The server has rejected the client credentials.")).into_response();
                if cfg.passwords {
                    r.headers_mut().append(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"bc-rust\", charset=\"UTF-8\""));
                }
                if cfg.entra.is_some() {
                    r.headers_mut().append(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                }
                r
            }
            AuthError::Locked(d) => {
                let mut r = (StatusCode::TOO_MANY_REQUESTS, body("Authentication_TooManyAttempts", "Too many failed sign-in attempts. Try again later.")).into_response();
                if let Ok(v) = HeaderValue::from_str(&d.as_secs().max(1).to_string()) {
                    r.headers_mut().insert(header::RETRY_AFTER, v);
                }
                r
            }
            AuthError::Unavailable(m) => {
                eprintln!("authentication unavailable: {m}");
                (StatusCode::SERVICE_UNAVAILABLE, body("Authentication_Unavailable", "The user store is not available.")).into_response()
            }
        }
    }
}

/// Credentials from an `Authorization` header.
#[derive(Debug, PartialEq)]
pub enum Credential {
    Basic(String, String),
    Bearer(String),
}

/// `Basic base64(user:password)` (UTF-8, or Latin-1 when not valid UTF-8, as .NET clients send it) or
/// `Bearer <token>`.
pub fn parse_authorization(v: &str) -> Option<Credential> {
    use base64::Engine;
    let (scheme, rest) = v.trim().split_once(' ')?;
    let rest = rest.trim();
    if scheme.eq_ignore_ascii_case("basic") {
        let raw = base64::engine::general_purpose::STANDARD.decode(rest).ok()?;
        let s = String::from_utf8(raw.clone()).unwrap_or_else(|_| raw.iter().map(|&b| b as char).collect());
        let (u, p) = s.split_once(':')?;
        (!u.is_empty()).then(|| Credential::Basic(u.to_string(), p.to_string()))
    } else if scheme.eq_ignore_ascii_case("bearer") && !rest.is_empty() {
        Some(Credential::Bearer(rest.to_string()))
    } else {
        None
    }
}

/// Checks credentials, remembers verified passwords and counts failures.
pub struct Authenticator {
    pub cfg: Config,
    verified: Mutex<HashMap<[u8; 32], Instant>>,
    /// Failure times per key (`u:<USER>|<client>`, `a:<client>`).
    failures: Mutex<HashMap<String, Vec<Instant>>>,
    locked: Mutex<HashMap<String, Instant>>,
    jwks: Option<crate::jwt::JwksCache>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Authenticator {
    pub fn new(cfg: Config) -> Authenticator {
        let jwks = cfg.entra.as_ref().map(|e| crate::jwt::JwksCache::new(&e.jwks));
        Authenticator { cfg, verified: Default::default(), failures: Default::default(), locked: Default::default(), jwks }
    }

    /// With fixed signing keys instead of fetching them (tests).
    pub fn with_jwks(mut self, jwks: crate::jwt::JwksCache) -> Authenticator {
        self.jwks = Some(jwks);
        self
    }

    fn keys(user: &str, client: &str) -> [String; 2] {
        [format!("u:{}|{client}", user.to_uppercase()), format!("a:{client}")]
    }

    fn check_locked(&self, keys: &[String]) -> Result<(), AuthError> {
        if self.cfg.lockout.failures == 0 {
            return Ok(());
        }
        let mut l = lock(&self.locked);
        let now = Instant::now();
        l.retain(|_, until| *until > now);
        match keys.iter().filter_map(|k| l.get(k)).max() {
            Some(until) => Err(AuthError::Locked(*until - now)),
            None => Ok(()),
        }
    }

    fn failed(&self, keys: &[String]) {
        let lo = &self.cfg.lockout;
        if lo.failures == 0 {
            return;
        }
        let now = Instant::now();
        let mut f = lock(&self.failures);
        if f.len() > 100_000 {
            f.retain(|_, v| v.last().is_some_and(|t| now.duration_since(*t) < lo.window));
        }
        for (i, k) in keys.iter().enumerate() {
            let limit = if i == 0 { lo.failures } else { lo.failures.saturating_mul(ADDRESS_FACTOR) };
            let v = f.entry(k.clone()).or_default();
            v.retain(|t| now.duration_since(*t) < lo.window);
            v.push(now);
            if v.len() as u32 >= limit {
                v.clear();
                eprintln!("authentication: {k} locked for {:?} after {limit} failures", lo.lock);
                lock(&self.locked).insert(k.clone(), now + lo.lock);
            }
        }
    }

    fn succeeded(&self, keys: &[String]) {
        lock(&self.failures).remove(&keys[0]);
    }

    fn fingerprint(u: &UserCreds, prehash: &str) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(u.sid);
        h.update([0]);
        h.update(prehash.as_bytes());
        h.update([0]);
        h.update(u.password_hash.as_bytes());
        h.finalize().into()
    }

    /// NavUserPassword: `user` with a password, access key (Basic) or pre-hash (web client), read from the
    /// session's database. `client` identifies the caller for the lockout (an address).
    pub fn authenticate(&self, s: &mut Session, user: &str, secret: Secret, client: &str) -> Result<Identity, AuthError> {
        let keys = Self::keys(user, client);
        self.check_locked(&keys)?;
        let now = chrono::Utc::now().timestamp_millis();
        let users = userauth::users(s, now);
        let u = users.iter().find(|u| u.name.eq_ignore_ascii_case(user));
        let method = if matches!(secret, Secret::PreHash(_)) { Method::WebClient } else { Method::Basic };
        let ok = match (u, &secret) {
            (Some(u), Secret::Password(p)) if userauth::verify_key(u, p, now) => true,
            (Some(u), _) if u.enabled && !u.password_hash.is_empty() => {
                let pre = match &secret {
                    Secret::Password(p) => userauth::prehash(p),
                    Secret::PreHash(h) => h.to_string(),
                };
                let fp = Self::fingerprint(u, &pre);
                let hit = lock(&self.verified).get(&fp).is_some_and(|t| t.elapsed() < VERIFIED_TTL);
                hit || {
                    let ok = userauth::verify_prehash(u, &pre);
                    if ok {
                        let mut v = lock(&self.verified);
                        if v.len() > 10_000 {
                            v.retain(|_, t| t.elapsed() < VERIFIED_TTL);
                        }
                        v.insert(fp, Instant::now());
                    }
                    ok
                }
            }
            _ => {
                // the same work as a real check, so unknown and disabled users answer as slowly
                let _ = userauth::stored_hash_from_prehash("bc-rust", &[1; 16]);
                false
            }
        };
        match (ok, u) {
            (true, Some(u)) => {
                self.succeeded(&keys);
                Ok(Identity { user_name: u.name.clone(), user_sid: u.sid, method })
            }
            _ => {
                self.failed(&keys);
                Err(AuthError::Invalid)
            }
        }
    }

    /// Entra on the API: validate the access token and find its User (`map_entra_user`). Bad tokens count
    /// against `client` in the lockout.
    pub fn authenticate_bearer(&self, s: &mut Session, token: &str, client: &str) -> Result<Identity, AuthError> {
        self.authenticate_entra(s, token, Some(client), 0)
    }

    /// Entra: validate `token`, accepting it `extra_s` seconds past expiry, and map it to a User. With
    /// `client` None nothing counts in the lockout (web client sign-ins, where every request comes from the
    /// web server with a token Entra issued).
    pub fn authenticate_entra(&self, s: &mut Session, token: &str, client: Option<&str>, extra_s: i64) -> Result<Identity, AuthError> {
        let (Some(cfg), Some(jwks)) = (&self.cfg.entra, &self.jwks) else { return Err(AuthError::Missing) };
        // bad tokens count per client: "t:" at the per-user limit, plus the address limit
        let keys = client.map(|c| [format!("t:{c}"), format!("a:{c}")]);
        if let Some(k) = &keys {
            self.check_locked(k)?;
        }
        let fail = |e: String| {
            eprintln!("authentication: Entra token rejected: {e}");
            if let Some(k) = &keys {
                self.failed(k);
            }
            AuthError::Invalid
        };
        let kid = crate::jwt::token_kid(token).ok_or_else(|| fail("no kid".into()))?;
        let set = jwks.keys_for(&kid).map_err(AuthError::Unavailable)?;
        let c = crate::jwt::validate_with(token, &set, cfg, chrono::Utc::now().timestamp(), extra_s).map_err(|e| fail(e.to_string()))?;
        let users = userauth::users(s, chrono::Utc::now().timestamp_millis());
        match map_entra_user(&users, &c) {
            Ok((u, write_oid)) => {
                if write_oid {
                    // first sign-in: remember the object id (NavWSFederationUserAuthenticator)
                    if let Err(e) = userauth::set_object_id(s, &u.sid, &c.oid).and_then(|_| s.commit().map_err(|e| e.to_string())) {
                        eprintln!("authentication: cannot store the Authentication Object ID of {}: {e}", u.name);
                    }
                }
                Ok(Identity { user_name: u.name.clone(), user_sid: u.sid, method: Method::Bearer })
            }
            Err(e) => Err(fail(e)),
        }
    }

    /// Check an `Authorization` header value in a session of `company`.
    pub fn authenticate_header(&self, rt: &'static Runtime, company: &str, header: Option<&str>, client: &str) -> Result<Identity, AuthError> {
        let cred = header.and_then(parse_authorization);
        if !matches!((&cred, self.cfg.passwords, self.cfg.entra.is_some()), (Some(Credential::Basic(..)), true, _) | (Some(Credential::Bearer(_)), _, true)) {
            return Err(AuthError::Missing);
        }
        let mut s = Session::try_new(rt, company).map_err(|e| AuthError::Unavailable(e.to_string()))?;
        let r = match &cred {
            Some(Credential::Basic(u, p)) => self.authenticate(&mut s, u, Secret::Password(p), client),
            Some(Credential::Bearer(t)) => self.authenticate_bearer(&mut s, t, client),
            None => Err(AuthError::Missing),
        };
        s.rollback();
        r
    }
}

/// The User of an Entra token, BC's rules (NavWSFederationUserAuthenticator): the user whose Authentication
/// Object ID is the token's `oid` (also when the email changed); else the user whose Authentication Email
/// is the token's user name, if that user has no object id yet (first sign-in: Ok((user, true)) asks to
/// store the oid); an oid and an email that point to two different users are refused. App-only tokens map
/// by Application ID. Only enabled users.
pub fn map_entra_user<'u>(users: &'u [UserCreds], c: &crate::jwt::Claims) -> Result<(&'u UserCreds, bool), String> {
    let enabled = || users.iter().filter(|u| u.enabled);
    if c.app_only {
        return enabled().find(|u| !c.app_id.is_empty() && u.application_id.eq_ignore_ascii_case(&c.app_id)).map(|u| (u, false)).ok_or_else(|| format!("no enabled user for application {}", c.app_id));
    }
    let by_oid = enabled().find(|u| !c.oid.is_empty() && u.authentication_object_id.eq_ignore_ascii_case(&c.oid));
    let by_mail = enabled().find(|u| !c.upn.is_empty() && u.authentication_email.eq_ignore_ascii_case(&c.upn));
    match (by_oid, by_mail) {
        (Some(a), Some(b)) if a.sid != b.sid => Err(format!("oid {} and {} belong to different users", c.oid, c.upn)),
        (Some(a), _) => Ok((a, false)),
        (None, Some(b)) if b.authentication_object_id.is_empty() && !c.oid.is_empty() => Ok((b, true)),
        (None, Some(b)) => Err(format!("{} is user {} with another object id", c.upn, b.name)),
        (None, None) => Err(format!("no enabled user for oid {} / {}", c.oid, c.upn)),
    }
}

/// The caller's address for the lockout: the peer address, or with `BC_AUTH_TRUST_PROXY` the first
/// X-Forwarded-For entry.
pub fn client_address(headers: &HeaderMap, peer: Option<std::net::SocketAddr>, trust_proxy: bool) -> String {
    if trust_proxy {
        if let Some(f) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()).and_then(|v| v.split(',').next()) {
            return f.trim().to_string();
        }
    }
    peer.map(|p| p.ip().to_string()).unwrap_or_else(|| "?".into())
}

tokio::task_local! {
    static IDENT: Identity;
}

thread_local! {
    static CURRENT: RefCell<Option<Identity>> = const { RefCell::new(None) };
}

/// Axum middleware for `bc serve`'s router: authenticate, then run the request as that user. Does nothing
/// while `BC_AUTH` is off.
pub async fn layer(State(ctx): State<&'static crate::api::Ctx>, req: Request, next: Next) -> Response {
    let a = authenticator();
    if !a.cfg.enabled() {
        return next.run(req).await;
    }
    let peer = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0);
    let client = client_address(req.headers(), peer, a.cfg.trust_proxy);
    let hv = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).map(str::to_string);
    let (rt, company) = (ctx.rt, ctx.company.clone());
    let r = tokio::task::spawn_blocking(move || a.authenticate_header(rt, &company, hv.as_deref(), &client)).await.unwrap_or_else(|e| Err(AuthError::Unavailable(e.to_string())));
    match r {
        Ok(id) => IDENT.scope(id, next.run(req)).await,
        Err(e) => e.response(&a.cfg),
    }
}

/// `tokio::task::spawn_blocking` that carries the request's user into the blocking closure (see `bind`).
pub fn spawn_blocking<F, T>(f: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let id = IDENT.try_with(Clone::clone).ok();
    tokio::task::spawn_blocking(move || {
        struct Reset(Option<Identity>);
        impl Drop for Reset {
            fn drop(&mut self) {
                CURRENT.with(|c| *c.borrow_mut() = self.0.take());
            }
        }
        let _reset = Reset(CURRENT.with(|c| c.replace(id)));
        f()
    })
}

/// The authenticated user of the request this thread works for.
pub fn current() -> Option<Identity> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Run session `s` as the request's user: UserId(), UserSecurityId() and its permission sets. Without one
/// (BC_AUTH off) the session keeps the default user and takes that user's User Security ID.
pub fn bind(s: &mut Session) {
    match current() {
        Some(id) => apply(s, &id),
        None => al_runtime::userauth::resolve_session_user(s),
    }
}

/// Run session `s` as `id`: UserId(), UserSecurityId() and the user's permission sets.
pub fn apply(s: &mut Session, id: &Identity) {
    s.user_id = id.user_name.clone();
    s.user_sid = id.user_sid;
    s.login_permissions();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(v: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |n| v.iter().find(|(k, _)| *k == n).map(|(_, x)| x.to_string())
    }

    #[test]
    fn config() {
        let c = Config::parse(&env(&[])).unwrap();
        assert!(!c.enabled());
        assert!(!Config::parse(&env(&[("BC_AUTH", "off")])).unwrap().enabled());
        let c = Config::parse(&env(&[("BC_AUTH", "NavUserPassword"), ("BC_AUTH_LOCKOUT", "3/60/120")])).unwrap();
        assert!(c.passwords && c.entra.is_none());
        assert_eq!(c.lockout, Lockout { failures: 3, window: Duration::from_secs(60), lock: Duration::from_secs(120) });
        assert_eq!(Config::parse(&env(&[("BC_AUTH", "basic"), ("BC_AUTH_LOCKOUT", "0")])).unwrap().lockout.failures, 0);
        assert!(Config::parse(&env(&[("BC_AUTH", "Entra")])).is_err());
        assert!(Config::parse(&env(&[("BC_AUTH", "Entra"), ("BC_AUTH_ENTRA_TENANT", "t")])).is_err());
        assert!(Config::parse(&env(&[("BC_AUTH", "windows")])).is_err());
        assert!(Config::parse(&env(&[("BC_AUTH", "basic"), ("BC_AUTH_LOCKOUT", "5/60")])).is_err());
        let c = Config::parse(&env(&[("BC_AUTH", "NavUserPassword, Entra"), ("BC_AUTH_ENTRA_TENANT", "T1"), ("BC_AUTH_ENTRA_AUDIENCE", "api://bc, 1234")])).unwrap();
        let e = c.entra.unwrap();
        assert_eq!((e.tenant.as_str(), e.audiences.len(), e.jwks.as_str()), ("t1", 2, "https://login.microsoftonline.com/t1/discovery/v2.0/keys"));
    }

    #[test]
    fn authorization_header() {
        assert_eq!(parse_authorization("Basic Q0xFUks6cGE6c3M="), Some(Credential::Basic("CLERK".into(), "pa:ss".into())));
        // Latin-1 from .NET clients
        assert_eq!(parse_authorization("basic SvxyZzp4"), Some(Credential::Basic("Jürg".into(), "x".into())));
        assert_eq!(parse_authorization("Bearer abc.def.ghi"), Some(Credential::Bearer("abc.def.ghi".into())));
        assert_eq!(parse_authorization("Basic OnB3"), None); // empty user name
        assert_eq!(parse_authorization("Negotiate abc"), None);
        assert_eq!(parse_authorization("Basic !!"), None);
    }

    #[test]
    fn client_addresses() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        let peer = Some("100.64.0.2:5555".parse().unwrap());
        assert_eq!(client_address(&h, peer, false), "100.64.0.2");
        assert_eq!(client_address(&h, peer, true), "203.0.113.7");
        assert_eq!(client_address(&HeaderMap::new(), None, true), "?");
    }

    #[test]
    fn lockout_counts_per_user_and_address() {
        let mut c = Config::off();
        c.passwords = true;
        c.lockout = Lockout { failures: 3, window: Duration::from_secs(60), lock: Duration::from_secs(30) };
        let a = Authenticator::new(c);
        let k = Authenticator::keys("clerk", "1.2.3.4");
        a.failed(&k);
        a.failed(&k);
        assert!(a.check_locked(&k).is_ok());
        a.succeeded(&k);
        a.failed(&k);
        a.failed(&k);
        assert!(a.check_locked(&k).is_ok(), "a success resets the count");
        a.failed(&k);
        assert!(matches!(a.check_locked(&k), Err(AuthError::Locked(_))));
        // another client address for the same user is not locked
        assert!(a.check_locked(&Authenticator::keys("CLERK", "5.6.7.8")).is_ok());
        // the address limit: 3 * 4 failures over different user names
        let mut last = Ok(());
        for i in 0..12 {
            let k = Authenticator::keys(&format!("user{i}"), "9.9.9.9");
            last = a.check_locked(&k);
            a.failed(&k);
        }
        assert!(last.is_ok());
        assert!(matches!(a.check_locked(&Authenticator::keys("someone", "9.9.9.9")), Err(AuthError::Locked(_))));
        let r = AuthError::Locked(Duration::from_secs(12)).response(&a.cfg);
        assert_eq!((r.status(), r.headers()[header::RETRY_AFTER].to_str().unwrap()), (StatusCode::TOO_MANY_REQUESTS, "12"));
        let r = AuthError::Invalid.response(&a.cfg);
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert!(r.headers()[header::WWW_AUTHENTICATE].to_str().unwrap().starts_with("Basic realm="));
    }

    #[test]
    fn entra_user_mapping() {
        let u = |name: &str, sid: u8, mail: &str, oid: &str| UserCreds { sid: [sid; 16], name: name.into(), enabled: true, authentication_email: mail.into(), authentication_object_id: oid.into(), ..Default::default() };
        let users = vec![u("ANNA", 1, "anna@x.example", ""), u("BEN", 2, "ben@x.example", "oid-ben"), u("CARL", 3, "carl@x.example", "oid-carl")];
        let c = |oid: &str, upn: &str| crate::jwt::Claims { oid: oid.into(), upn: upn.into(), ..Default::default() };
        // first sign-in by email writes the oid; later the oid alone is enough (the email may change)
        assert_eq!(map_entra_user(&users, &c("oid-anna", "ANNA@x.example")).map(|(u, w)| (u.name.as_str(), w)), Ok(("ANNA", true)));
        assert_eq!(map_entra_user(&users, &c("oid-ben", "new-ben@x.example")).map(|(u, w)| (u.name.as_str(), w)), Ok(("BEN", false)));
        // an email whose user is bound to another object id, and an oid/email pair of two users
        assert!(map_entra_user(&users, &c("oid-evil", "ben@x.example")).is_err());
        assert!(map_entra_user(&users, &c("oid-ben", "carl@x.example")).is_err());
        assert!(map_entra_user(&users, &c("oid-nobody", "nobody@x.example")).is_err());
        let mut app = c("", "");
        app.app_only = true;
        app.app_id = "11111111-2222-3333-4444-555555555555".into();
        let mut svc = u("SVC", 4, "", "");
        svc.application_id = app.app_id.clone();
        assert_eq!(map_entra_user(&[svc], &app).map(|(u, _)| u.name.clone()), Ok("SVC".to_string()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn identity_reaches_blocking_closure() {
        let id = Identity { user_name: "CLERK".into(), user_sid: [7; 16], method: Method::Basic };
        let seen = IDENT.scope(id.clone(), async { spawn_blocking(current).await.unwrap() }).await;
        assert_eq!(seen, Some(id));
        // outside a request (and after one) the blocking pool's threads carry no user
        assert_eq!(spawn_blocking(current).await.unwrap(), None);
    }
}
