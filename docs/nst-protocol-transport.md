# BC web server ↔ service tier protocol: transport and calls

Notes for making bc-rust answer as a Business Central service tier (NST) so that Microsoft's own web server and browser client run unchanged on top of it. They come from an analysis of the BC 29.1.57326.57497 web server made before 2026-11-10 with decompiled binaries, which bc-rust no longer uses (README, BC sources and licence), and are being re-sourced from captured traffic [src:TODO]. Items marked "unconfirmed" need a live capture against the real container in the bc-sidebyside VM.

Page metadata and the row/dataset format are in [nst-protocol-metadata.md](nst-protocol-metadata.md).

## Endpoints

The client services port (7085 by default) serves one WebSocket per BC session:

```
ws(s)://{host}:7085/{instance}/client/ws/connect
```

Confirmed against Microsoft's web server (ConnectionEstablisher builds this URL). `http(s)://{host}:{port}/{instance}/Service` only appears as the server address in error messages ("A server was not found at ...").

The same port also serves plain HTTP side endpoints under `/{instance}/client/`:

| Path | Use |
|---|---|
| `metadata` | page/table metadata (see the metadata doc) |
| `data` | data requests outside the session socket |
| `media` | images and media |
| `health` | health probe |
| `config` | client configuration |
| `uploadDownload` | file transfer |

Errors on the HTTP endpoints come back as HTTP 521 with a JSON `ServerFault` body.

## Framing and serialization

- JSON-RPC 2.0 as text WebSocket frames, one message per frame. The web server uses StreamJsonRpc 2.20 with the Newtonsoft formatter.
- `params` are positional (an array), in the order of the C# method parameters on `IClientApi`.
- Property names on the wire are the DTOs' member names [src:TODO].
- Enums are integers. Null fields are omitted. Guid, TimeSpan, Version and DateTime are strings. `byte[]` is base64.
- Wrappers that have to be reproduced exactly:
  - Polymorphic values: `{"$polyType": "<type>", "obj": {...}}`, used for instance by the callback items in `ServerResponse.requests`.
  - `object`-typed arguments and results: `{"typeHint": <int>, "value": ...}`. The hint is an integer `JsonTypeHint` (e.g. Int = 1, String = 21), not a type name.
  - `NavDataSet`: `{"compressed": ..., "count": ..., "buffers": [...]}`.
  - The `typeHint` and `NavDataSet` readers expect their properties in that fixed order. Emit them in exactly this order.

## Connection and session

1. Sign-in validation: the web server opens a short-lived connection, calls `OpenConnection`, and closes it with `CloseConnection`.
2. Session: a new WebSocket, then `OpenConnection`, `SetClientSettings`, `OpenCompany`, and then form traffic.

Credentials for NavUserPassword travel twice: as a Basic `Authorization` header on the WebSocket upgrade, and in a `Credentials` object inside the `OpenConnection` request. A rejected upgrade (401) is shown to the user as invalid credentials.

There is no reconnect logic. Any transport failure ends the browser session.

## Responses the web server insists on

- `OpenConnection` must return a valid time zone, valid LCIDs and an `enabledFeatures` list.
- `OpenCompany` must return `Permissions`.
- `OpenForm` and `CloseForm` must return a `States` array with exactly one entry per form in the request.
- Wherever a response has `requests` (queued callbacks), it may be empty or omitted, but never `null`.

## Errors

The web server only recovers from an error when the JSON-RPC error carries a `ServerFault` whose `exceptionData` is base64 of a JSON array of typed .NET exceptions. Anything else drops the session, including JSON-RPC "method not found" for a method bc-rust hasn't implemented.

Implemented in crates/bc-nst/src/wire.rs and verified against Microsoft's web server: AL errors are sent as `Microsoft.Dynamics.Nav.Types.Exceptions.NavNCLDialogException` (error dialog, session stays), unimplemented methods as `NavNCLNotSupportedOperationException`. Errors in a form's `OnOpenPage` go back in the form's `FormOpenExceptionData`. Still unconfirmed: whether the real service tier uses other exception types for TestField / record-not-found.

During session startup any fault ends the session, so every startup call needs a real answer (see the observed sequence below).

## Callbacks into the web server

1. Queued `ServerCallbackRequest` items inside any response (`requests`).
2. Live calls from the service tier while a web-server request is still outstanding: `Confirm`, `FormRunModal`, message and dialog methods. The user's answer comes back as an `EndClientCall*` call (`EndClientCall`, `EndClientFormRunModalCall`, `EndClientFileActionCall`, ...).

Unconfirmed: whether the live calls are JSON-RPC requests or notifications.

bc-rust's interpreter currently answers `Confirm` with its default and collects `Message` text. Both have to become real callbacks.

## Minimal method set

Enough to sign in, show a role center, open the Customer list and card, edit and save a field, and run an action:

| Method | Purpose | bc-rust implementation |
|---|---|---|
| `OpenConnection` | session start; returns user settings (time zone, LCIDs, features) | session object, settings from config |
| `SetClientSettings` | client locale/time zone etc. | store on session |
| `OpenCompany` | select company; returns permissions | company = PostgreSQL schema; permissions: allow all |
| `OpenForm` | open a page (role center, list, card, subpage); returns form state | page instance + Rec from `al-model` PageDef, run `OnOpenPage` |
| `GetPage` | fetch a page of rows for a form | record scan with keyset paging (already in bc-store) |
| `ValidateField` | user edits a field | `Session::rec_validate` + page control `OnValidate` |
| `InsertRecord` / `ModifyRecord` | save | `rec_insert(true)` / `rec_modify(true)` (+ `OnInsertRecord`/`OnModifyRecord`) |
| `ActionField` | page action `OnAction` triggers | already exists as `/api/page/{id}/action/{name}` in bc-server |
| `InvokeApplicationMethod` | codeunit actions (`Run`) and page triggers `NewRecord`, `AfterGetCurrRecord` | interpreter `invoke` |
| `CloseForm` | close page; returns `States` | drop page instance |
| `CloseConnection` | end session | drop session, roll back open transaction |

Page actions go through `ActionField`, not `InvokeApplicationMethod`.

## Observed startup sequence (Microsoft's web server against bc-nst)

1. Sign-in validation: `OpenConnection`, `CloseConnection` (then a 302 with auth cookies).
2. Session: `OpenConnection` → `SetClientSettings` → `GetIsLegacyActionBarEnabled` → `GetPageUsageState` ×2 → `GetAddInRegistrations` → `OpenCompany("")` → `GetPage` with no form (FormId 0) on table 2000000178 All Profile, for the profile cache → (next calls not yet observed).

## Confirmed from real traffic

A full recording of Microsoft's web server against the real BC 29.1 service tier is in data/capture/nst-real/ (tools/nst-proxy/ records it; notes.md compares it with bc-nst).

- Real startup: `OpenConnection`, `SetClientSettings`, `GetIsLegacyActionBarEnabled`, `GetPageUsageState` ×2, `GetAddInRegistrations`, `OpenCompany`, then form-less `GetPage` on All Profile (2000000178) and User Personalization (2000000073).
- Role center: `metadata/navigation`, `pagewithdependencies/9022`, `GetRoleCenterBannerPartId`, then `OpenForm` + `GetPage` per part. The role center page itself is never opened with `OpenForm`.
- Each page: `pagewithdependencies/{id}`, one `OpenForm` per form (main form first, then each part), `GetPage`, `InvokeApplicationMethod` (`AfterGetCurrRecord`).
- Field edit: `ValidateField`, then `ModifyRecord` immediately, then list/factbox refreshes.
- Field validation errors are `NavNSValidateFieldException` wrapping the cause (shown on the field), not a dialog exception.
- Every service-tier message carries six token fields (`MetadataToken`, `UserToken`, `AgentToken`, `AgentTaskToken`, `AgentPageRefreshToken`, `PermissionToken`). `VersionNumber` is 0 in results.
- Calls from the service tier into the web server (`CreateDotNetHandle`, `InvokeAutomationMethod`, ...) are JSON-RPC requests with the service tier's own id counter. The web server acks with `null` and later sends `EndClientAutomationMethodCall` while its own request is still pending.
- Messages over about 1 KB are split into several WebSocket frames. No compression on the socket, datasets or metadata.
- Unknown method: JSON-RPC error -32601. Rejected upgrade: 401 without `WWW-Authenticate`. Missing page metadata: HTTP 521 with a ServerFault.

## Still open

- `MetadataToken` semantics across metadata changes.
- Exception types for TestField and record-not-found errors outside field validation.
