# Authentication

How Business Central on-prem signs users in, and how bc-rust does it. Without authentication the API takes whatever user name a client sends, and permission sets (docs/permissions.md) protect nothing. Sources: the BC 29.1 service tier (Nav.Core `CryptographyHelper`, Ncl `NavUser`, `ALDatabase`, `UserManagement`, `AuthenticationLockoutPolicy`, Service `ClientServicesUserNamePasswordValidator`), the web server's `ConnectionEstablisher` (Prod.Client.ServiceConnection), CustomSettings.config from platform.zip, and the Base App's User Card and Identity Management.

Status (2026-11-08, I14): NavUserPassword and web service access keys work for `bc serve` and for bc-nst sign-in, and `bc user` manages accounts. Sessions run as the signed-in user (`UserId()`, `UserSecurityId()`, permission sets). Entra ID works for the API (access tokens) and for the web client (the web server's id_token in OpenConnection), tested with keys made at test time; neither has met a real tenant yet. Everything is off by default (`BC_AUTH` unset); the live units don't set it.

## Turning it on

```
bc user add ADMIN --role SUPER --pg ...          # same database flags as bc serve
bc user password ADMIN < pwfile                  # first line of stdin; --change-at-next-sign-in to force a change
bc user add CLERK --role LOGIN --role "D365 BASIC" --role "D365 CUSTOMER, EDIT"
bc user key INTEGRATION --days 365               # prints a new web service access key once
bc user list | disable <NAME> | enable <NAME>
BC_AUTH=NavUserPassword BC_PERMISSIONS=enforce bc serve ...
BC_AUTH=NavUserPassword BC_PERMISSIONS=enforce bc-nst ...
```

Create at least one SUPER user with a password before switching a server on, or nobody can sign in. As in BC, an empty User table turns the permission system off, but with `BC_AUTH` on it also means no one can authenticate. Business users need LOGIN (or D365 BASIC) besides their task sets: the Base App's OnCompanyOpen subscribers read the User table, and without it the web client says "Could not open the company".

## Switches

| Variable | Values | Effect |
|---|---|---|
| `BC_AUTH` | unset or `off` (default), `NavUserPassword`, `Entra`, or both (`NavUserPassword,Entra`) | which credentials `bc serve` and bc-nst accept |
| `BC_AUTH_ENTRA_TENANT` | directory id (GUID) | required for `Entra` |
| `BC_AUTH_ENTRA_AUDIENCE` | comma list | accepted `aud` values: application id URI and/or client id (BC: `ValidAudiences`, `AppIdUri`) |
| `BC_AUTH_ENTRA_JWKS` | https URL or file | signing keys; default `https://login.microsoftonline.com/<tenant>/discovery/v2.0/keys` |
| `BC_AUTH_ENTRA_EXTENDED_LIFETIME` | hours, default 1 | how long a web client's id_token stays valid past its expiry (BC: ExtendedSecurityTokenLifetime) |
| `BC_AUTH_PASSWORD_CHANGE` | `1` | report NavUserPassword to AL (IsUserNamePasswordAuthentication), which turns on the Base App's forced password change; off until the LogInStart bug is fixed |
| `BC_AUTH_LOCKOUT` | `<failures>/<window s>/<lock s>` or `0` | lockout, default `5/300/300` |
| `BC_AUTH_TRUST_PROXY` | `1` | take the client address from `X-Forwarded-For` (only behind a proxy that sets it) |
| `BC_PERMISSIONS` | `enforce` | A1's permission checks; with `BC_AUTH` on they apply to the signed-in user |

A bad configuration stops the process at the first request (exit 2). A server asked to authenticate must not start open.

## NavUserPassword

### What BC stores

"User Property" (2000000121) field 2 Password is Text[80] and holds BC's "V3" hash:

```
prehash = base64(PBKDF2-HMAC-SHA256(UTF-8(password), salt = 16 zero bytes, 1 iteration, 16 bytes)) + "-V3"
stored  = base64(PBKDF2-HMAC-SHA256(UTF-8(prehash), salt = UserSecurityId.ToByteArray(), 100000 iterations, 16 bytes)) + "-V3"
```

`ToByteArray` is .NET's GUID byte order (the first three groups little-endian); bc-store keeps GUIDs in text order, so `userauth::dotnet_guid_bytes` swaps them. The test vectors in `crates/al-runtime/src/userauth.rs` came from running BC's own `Microsoft.Dynamics.Nav.Core.dll` under .NET 10, so a hash that BC wrote reads in bc-rust and the other way round.

We match it. PBKDF2-HMAC-SHA256 is a standard KDF, the salt is unique per user, and 100,000 rounds is below OWASP's 2023 figure (600,000) but not weak. Argon2id was the fallback if BC's scheme had been home-grown. It isn't, and a PHC argon2id string (about 97 characters) would not fit the Text[80] field. If we ever want more rounds, the `-V3` suffix is a version tag: a `-V4` with more iterations can sit next to it and be rewritten on the next successful sign-in.

The weak spot is the prehash. It is a password equivalent: the web server sends it instead of the password, and whoever captures it can sign in through the web client channel. BC has the same property. The answer is TLS on every hop and keeping the NST port off the network (see Risks).

AL writes passwords with two built-ins, now in bc-rust (`builtins.rs` → `userauth`):
- `SetUserPassword(UserSecurityId, Password)` (User Card's password AssistEdit): stores the hash, inserts the User Property row if missing, sets User."Change Password".
- `ChangeUserPassword(OldPassword, NewPassword)` (System App report "Change Password"): checks the old hash for the session user, stores the new one, clears "Change Password".

Both return false instead of raising when AL uses the return value (`[Ok :=]`). SetUserPassword for another user needs SUPER, or SECURITY when the target isn't SUPER (`userauth::may_manage_user`, after `PermissionManagement.SessionHasSuperOrSecurityPermissionsForUser`). BC 29.1 computes that rule in ALSetUserPassword but ignores its result, and relies on the table permission on User Property; we enforce the rule itself, with `BC_PERMISSIONS=enforce` (with the permission system off everyone is SUPER, as in BC).

"Change Password" is BC's own AL: the Base App's `LogInManagement.LogInStart` (OnCompanyOpen of a web client session) checks `User."Change Password"` for `UserSecurityId()` when `IdentityManagement.IsUserNamePasswordAuthentication()` is true, runs report 9810 "Change Password" (the Password Dialog), and stops the sign-in with an error if the password is still unchanged. bc-rust supplies the two platform pieces: `UserSecurityId()` per session and NavUserAccountHelper.IsUserNamePasswordAuthentication (`userauth::credential_type`). The second is opt-in for now (`BC_AUTH_PASSWORD_CHANGE=1` with `BC_AUTH=NavUserPassword`): with it, bc-nst's company open fails in LogInManagement.LogInStart with "Not implemented in bc-rust: Void.FindFirst" on the codeunit's global `User` record, for every user. The same procedure runs fine when invoked directly in a bc-server session, so it is an interpreter problem on bc-nst's OnCompanyOpen path, not in the credential code; until it is fixed the switch stays off and the forced change doesn't happen. The Password Dialog itself opened in bc-nst during the attempt (report 9810 → `Page.RunModal` callback). API sessions skip it (GuiAllowed is false), as in BC. `bc user password` doesn't set the flag unless asked (BC's Set-NAVServerUser -ChangePasswordAtNextLogOn), while SetUserPassword from the User Card always does.

### Who may sign in

`NavUser.TryAuthenticate`, in order:
1. The user exists, State is Enabled, Expiry Date is not past, and License Type is not Windows Group or AAD Group.
2. Basic auth only: the password equals the unexpired web service access key ("User Property"."WebServices Key", "WebServices Key Expiry Date"; 0DT means no expiry).
3. The salted hash of the presented password, or of the presented prehash for the web client, equals the stored hash. A user without a stored password never matches, not even with an empty password.

The access key is stored in clear text, as BC does. The User Card shows it and `IdentityManagement.GetWebServicesKey` reads it back, so hashing it would break AL. BC generates it as base64 of HMAC-SHA256 over a random GUID (44 characters). bc-rust implements NavUserAccountHelper's TryCreateWebServicesKey, TryGetWebServicesKey and TryClearWebServicesKey (UserManagement.Create/Get/ClearWebServicesKey: the own user, or SUPER / SECURITY), so the User Card's AssistEdit and Identity Management work; keys come from the OS random generator even under al-test's virtual clock. `bc user key` does the same for operators.

User names compare case-insensitively ("User Name" is Code[50]). Comparisons of hashes and keys are constant-time.

### Cost and caching

One check is 100,000 HMAC rounds, about 10 ms on this host. `bc serve` checks every request, so `bc_server::auth::Authenticator` keeps a verified-password cache for 5 minutes. The key is SHA-256 of (user security id, prehash, stored hash). A password change alters the stored hash, so the old entry stops matching at once. The User rows are read on every request, so disabling a user or an expiry date takes effect at the next request. Access keys are cheap to check and are not cached. Unknown and disabled users run a dummy PBKDF2, so the response time doesn't reveal which user names exist.

## API (`bc serve`)

`crates/bc-server/src/auth.rs`, an axum middleware on the whole router (API v2.0, the own web client's `/api/*` and its static files):
- `Authorization: Basic base64(user:password)` with the password or the access key. The header is decoded as UTF-8, else as Latin-1 (.NET clients).
- `Authorization: Bearer <token>` when Entra is on (below).
- Anything else: 401 with `WWW-Authenticate: Basic realm="bc-rust", charset="UTF-8"` (and `Bearer` when Entra is on) and the body `{"error": {"code": "Authentication_InvalidCredentials", "message": "The server has rejected the client credentials."}}`. Wrong user, wrong password and disabled user all get the same answer.

A1's permission hook in odata.rs (`BC_PERMISSIONS=enforce`) checks the API page's source table for the session user and answers 403. Without `BC_AUTH` it takes the Basic user name unchecked; with `BC_AUTH` on it leaves the verified user that `bind` set.

The middleware runs the check in a blocking task and puts the identity into a tokio task-local. `auth::spawn_blocking` carries it into the blocking thread that runs the request, and `auth::bind` (called where api.rs and odata.rs open their session) sets `UserId()` and calls `login_permissions()`. With auth off nothing changes: no middleware work, sessions run as ADMIN.

The browser client at `/` gets the browser's own Basic prompt. That is enough for a first version; a sign-in page with a session cookie can replace it.

## Web client (bc-nst and Microsoft's web server)

`ConnectionEstablisher.ApplyCredentials` with ClientServicesCredentialType NavUserPassword:
- upper-cases the user name;
- replaces the password with `GeneratePasswordHash(password)` (the prehash above);
- sends `Authorization: Basic base64(USER:appId\host\tenant\prehash)` on the WebSocket upgrade to `/{instance}/client/ws/connect`;
- sends the same user and password string in OpenConnection's `Credentials`.

The service tier takes the last `\` part (`ExtractPropertiesFromPassword`) and treats it as a prehash, since the token is not a `NavUserNameBasicAuthenticationToken`, so the access key is not accepted here. Only Basic auth on the API takes keys.

bc-nst (`crates/bc-nst/src/auth.rs`) does the same with `BC_AUTH=NavUserPassword`:
- `ws_connect` validates the upgrade's Basic credentials before upgrading; a failure is a 401 without `WWW-Authenticate` (what the real NST sends). The web server turns that into its "invalid credentials" sign-in error.
- OpenConnection requires the `Credentials` user to be the user the upgrade validated, or, with no validated upgrade, validates the `Credentials` itself.
- OpenCompany sets the session's `UserId()` and calls `login_permissions()`.

With auth off, bc-nst behaves as before: one configured user (`admin`), any password, and a warning in the log.

### Entra ID in the web client

The web server (ClientServicesCredentialType AccessControlService) signs the browser in with OpenID Connect itself (`AadApplicationId`, `AadAuthorityUri`, redirect URI `https://<host>/<instance>/SignIn`, response type `code id_token`). It never asks for an access token. It stores the sign-in's id_token and sends it with every new service tier connection:
- user name `" WSFED:{GUID}"` (with the leading space);
- WebSocket upgrade: Basic `" WSFED:{GUID}":appId\host\tenant`, no token;
- OpenConnection: `Credentials.Password` = `appId\host\tenant\<blob>`, the blob being base64 of .NET BinaryWriter strings (userName, authKey, id_token, federationOrigin, tokenId, "BinaryJwt", HomeAccountId, code context, client IP).

BC validates it only in OpenConnection, and bc-nst does the same: an Entra upgrade passes without a user, OpenConnection unwraps the id_token (`auth::entra_token`) and validates it like an API token. The id_token's audience is the web server's application id, so `BC_AUTH_ENTRA_AUDIENCE` must contain the `AadApplicationId` GUID (BC: ValidAudiences). The web server never refreshes the token: a session that opens a new connection after expiry is refused, and BC allows ExtendedSecurityTokenLifetime hours (default 1 here) past `exp`. Failures don't count in the lockout, since every request comes from the web server with a token Entra issued.

Not done: the web server's own app registration and settings (needs a tenant), the AuthenticationObjectId/UserUpn fields of OpenConnection's answer (only Copilot uses them), and redeeming the authorization code for AL's token cache.

## Entra ID bearer tokens

`crates/bc-server/src/jwt.rs`. A token is accepted when all of these hold:
- header `alg` is exactly `RS256` (`none`, `HS256` and anything else are refused before any key is looked at) and `kid` names a key in the tenant's JWKS;
- the RSA PKCS#1 v1.5 SHA-256 signature verifies; keys under 2048 bits are not loaded;
- `exp` and `nbf` hold with 300 s of clock skew;
- `iss` is `https://login.microsoftonline.com/<tenant>/v2.0` or `https://sts.windows.net/<tenant>/` and `tid`, if present, is the tenant. Multi-tenant (`common`, `organizations`) is not supported;
- `aud` (a string or an array) is one of `BC_AUTH_ENTRA_AUDIENCE`.

Signing keys are fetched on first use, again after 24 hours, and at most once a minute when a token names an unknown kid (Entra rolls keys). A failed refresh keeps the old keys.

The token maps to an enabled User with BC's rules (NavWSFederationUserAuthenticator, `auth::map_entra_user`):
1. the user whose "User Property"."Authentication Object ID" is `oid` (also when the email changed);
2. else the user whose User."Authentication Email" is the token's user name (`upn`, `preferred_username`, `email` or `unique_name`), but only if that user has no object id yet. This is the first sign-in, and bc-rust stores the `oid` then, as BC does;
3. an `oid` and an email that point to two different users are refused;
4. app-only tokens (`idtyp = app`, or no `scp` and no user name) map by User."Application ID" = `azp` or `appid`, BC's Microsoft Entra Application users.

Entra security groups are not read.

Unit tests generate a fresh 2048-bit key per run and cover a valid v2 and v1 token, audience arrays, expiry and not-before with leeway, wrong tenant, issuer and audience, an app-only token, a foreign key under our kid, an unknown kid, a payload changed after signing, `alg: none`, `HS256`, malformed tokens and short keys. No key material is in the repo.

## Lockout and rate limiting

BC's lockout (`LockoutPolicyFailedAuthenticationCount`, `...Window`, default 0 = off) disables the user after N failures in a window. Anyone who knows a user name can then lock that user out until an admin re-enables them. We don't copy that.

bc-rust counts failures per (user name, client address) and per client address:
- 5 failures in 300 s for one user name from one address lock that pair for 300 s (`BC_AUTH_LOCKOUT`);
- 4 times that many failures from one address, across user names, lock the address (password spraying);
- while locked, even correct credentials get 429 with `Retry-After`;
- a success clears the pair's count. Nothing is written to the database, and a restart forgets the counters.

The client address is the TCP peer. Behind rockit-proxy every request comes from the proxy, so set `BC_AUTH_TRUST_PROXY=1` there; never set it on a port that clients reach directly, because they could forge `X-Forwarded-For`. For bc-nst the peer is always Microsoft's web server, so its failures count per user name only. An attacker who knows a user name can keep that user out of the web client for 5 minutes at a time, but not for good.

Bad Entra tokens count per address (a `t:` key at the per-user limit, plus the address limit).

## Session identity

| | BC | bc-rust now |
|---|---|---|
| `UserId()` | the user's name | the signed-in user (API and web client) |
| `UserSecurityId()` | the user's GUID | the signed-in user's (API, web client, and background sessions they start); without `BC_AUTH`, the User row named `UserId()` (ADMIN by default), else the platform id |
| permission sets | `NavUserPermissions.GetRoles` | `Session::login_permissions` reads Access Control for `UserId()` (A1) |
| `SystemCreatedBy` / `SystemModifiedBy` | the user's GUID | the session's `UserSecurityId()`: set on insert (both) and modify (`SystemModifiedBy`) |
| User Login (table 9008) | written at company open of interactive clients (System Initialization.OnAfterLogin) | bc-nst: every web client session (OpenCompany); bc serve: loading its own web client (`/api/home`, at most once a minute per user, crates/bc-server/src/companyopen.rs); never for API, OData or background sessions |

`Session::user_sid` holds it: `auth::apply` sets it for API requests, bc-nst's OpenCompany and the background-session workers set it from the User table (`userauth::set_session_user`). Without an authenticated user, `auth::bind` (bc serve), bc-nst and page background tasks call `userauth::resolve_session_user`, which caches found ids on the Runtime for a minute (every API request is a session). al-test keeps the platform id: its platform data names that id ADMIN.

User Login matters beyond the sign-in page: the Base App's Cash Flow Forecast Update (job queue codeunit 842) deletes its own entry when `UserLoginTimeTracker.AnyUserLoggedInSinceDate(two weeks ago)` is false. Before I14 bc serve recorded no logins, so on the live `bc` the entry deleted itself after its first run.

## Demo

`crates/bc-server/tests/auth_api.rs` runs the real router on the seed company in memory with `BC_AUTH=NavUserPassword`, `BC_AUTH_LOCKOUT=3/60/60` and `BC_PERMISSIONS=enforce`:

| Request | Result |
|---|---|
| no credentials (API and `/api/home`) | 401, `WWW-Authenticate: Basic` |
| CLERK, wrong password | 401 |
| CLERK, right password, GET customers | 200 |
| CLERK (D365 CUSTOMER, EDIT), POST customer | 400 "Sorry, the current permissions prevented the action" from Customer.OnInsert, so the session runs as CLERK |
| ADMIN (SUPER), POST customer | 201 |
| CLERK, GET vendors | 403 "(TableData 23 Vendor Read ...)" from A1's API hook; ADMIN 200 |
| READER with the access key | 200; with a wrong key 401 |
| DISABLED with the right password | 401 |
| READER after 3 failures, then the right key | 429 with `Retry-After` |
| ADMIN meanwhile | 200 |

Scratch runs on 2026-11-08 (PostgreSQL scratch databases, 127.0.0.1 only; the transcripts are not in the repository):
- `bc serve` with users made by `bc user`: the table above over curl. A wrong password answers in about 11 ms (one PBKDF2), the first right one in 17 ms, repeats in 1 ms (cache).
- bc-nst behind Microsoft's web server (dev copy on 8191): a wrong password, an unknown user and a disabled user each get the web server's own "The specified user name or password is not correct, or you do not have a valid user account" on the sign-in page; ADMIN reaches the role center; a user with LOGIN, D365 BASIC and D365 CUSTOMER, EDIT reaches the role center; a user with only D365 CUSTOMER, EDIT signs in but gets "Could not open the company ... (TableData 2000000120 User Read)" from OnCompanyOpen.

## Risks and gaps

- Transport: Basic auth and the web client's prehash are password equivalents. Serve them only over TLS (rockit-proxy terminates it) and keep `bc serve`, bc-nst and the web server ports on 127.0.0.1 behind the proxy.
- The NST's HTTP side endpoints (`/{instance}/client/metadata`, ...) are not checked. They return page metadata, not business data.
- No password policy beyond what the AL Password Handler checks when AL calls it.
- Entra: no group membership, not tried against a live tenant.
- The forced password change is opt-in (`BC_AUTH_PASSWORD_CHANGE=1`) until LogInManagement.LogInStart's `Void.FindFirst` on bc-nst is fixed.

## Web client sessions and restarts

Two things sign a web client user out on the next page load, both with an antiforgery error in the web server log that is only a side effect (I14 investigation):
- A bc-nst built without data/nst-templates served its own Links factbox, the web server's LinksListBuilder failed on it, and the abandoned web session took the stored credentials with it. bc-nst now leaves Notes and Links out when the captured parts aren't built in. The live builds have them.
- Restarting the web server: it keeps the signed-in user's credentials only in memory, so every user signs in again. That is Microsoft's design.
- Lockout counters live in memory per process; several `bc serve` processes count separately.
