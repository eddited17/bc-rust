# Assumptions and guessed defaults

bc-rust answers some calls with a neutral value (true, false, 0, '', nothing) where Business
Central does real work. Such an answer is a guess: a test that relies on it can pass here and
fail on BC, and a production feature can report success and do nothing. This file is the
register of those guesses, with what each assumes and where the code is.

The rule for new code is **refuse, don't guess**: an unsupported call raises
`Not implemented in bc-rust: ...`. A guessed default is allowed only when it is registered
in `crates/al-runtime/src/assume.rs` (`REGISTRY`) and listed here. The unit tests in that
module fail when a registered name has no call site, a call site names an unregistered
default, or this file has no row for an entry.

## How al-test reports them

Every registered arm calls `crate::assume::hit("Name")`. A refusal is an ordinary AL error,
so `asserterror`, a `[TryFunction]` or `if Codeunit.Run(...)` can catch it; every such
catch goes through `Session::note_error`, which records the refusal
(`crate::assume::note_caught`). al-test reads both after each test and writes three fields
next to the existing ones in `*-results.jsonl` (`outcome` keeps its values):

| Field | Values |
|---|---|
| `class` | `pass`; `flagged_pass`: passed, but touched a registered default; `not_trusted`: passed, but a refusal was swallowed on its path; `fail`: outcome fail or error; `not_run`: disabled and not run, or never started because its codeunit was abandoned |
| `assumed` | the registered defaults the test touched (omitted when none) |
| `swallowed` | the first lines of the refusals swallowed (at most 8; omitted when none) |

The summary prints a `classes:` line for the CI-comparable tests and the defaults and
refusals behind the flagged and untrusted passes. A refusal is a message that starts with
`Not implemented in bc-rust`, `Unknown variable`, `Method '`, `Cannot call expression`,
`Cannot index`, `Cannot resolve`, `Record method`, `'in' requires` or
`Expected a field reference` (`assume::REFUSALS`).

Flagged passes lower the headline only if you count them as failures. The CI-comparable figure
(`outcome` = pass) is unchanged; the classes say how much of it rests on a guess.

## Registered defaults (flagged)

Two entries are recorded in \`assumed\` but do not make a pass a flagged pass
(\`assume::NOT_FLAGGING\`): \`Record.SecurityFiltering\` and \`Record.Consistent\`. They skip a
check rather than guess a value, and nearly every posting touches them (W1 quick slice,
2026-11-09: 6,860 and 3,720 of 21,979 passing tests), so as flags they would hide the guesses
that matter. Security filters act only with permissions enforced; a \`Consistent(false)\` still
set at Commit would be a posting bug that the tests' ledger assertions see.

| Name | What bc-rust assumes instead of doing the work | Code |
|---|---|---|
| `DateFormula.P09` | A date formula holding U+0000..U+0007 or a decimal digit of another script (T4:P09: those inputs are not settled): U1 unsettled: P09. | `crates/al-runtime/src/datefml.rs` Probe::P09 (note_assumed) |
| `DateFormula.P10` | A date formula text longer than 32 characters that also fails to read, or that holds lower case (the case of %1 is not measured): refused, message U1 unsettled: P10. | `crates/al-runtime/src/datefml.rs` Probe::P10 (note_assumed) |
| `DateFormula.P11` | Format(DateFormula) with format 0 or 1 in Vietnamese (C and D letters not measured) or a language id outside BC's list: U1 unsettled: P11. | `crates/al-runtime/src/datefml.rs` Probe::P11 (note_assumed) |
| `DateFormula.P12` | Evaluate of unbracketed date formula text in Vietnamese or a language id outside BC's list: U1 unsettled: P12. | `crates/al-runtime/src/datefml.rs` Probe::P12 (note_assumed) |
| `DateFormula.P13` | Date formula error texts outside English (the probes saw English in one German session, German in another): U1 unsettled: P13; Evaluate's return form still gives false. | `crates/al-runtime/src/datefml.rs` Probe::P13 (note_assumed) |
| `DateFormula.P14` | The order of two different DateFormula values (BC orders SQL and temporary tables differently): U1 unsettled: P14 for < > in AL; value::compare orders by stored text (flagged). | `crates/al-runtime/src/datefml.rs` Probe::P14 (note_assumed) |
| `DateFormula.P15` | Format(DateFormula, Length, n) with a length and a format number other than 0, or with a format string: U1 unsettled: P15. | `crates/al-runtime/src/datefml.rs` Probe::P15 (note_assumed) |
| `DateFormula.P17` | CalcDate(Text) with unbracketed text in Vietnamese or a language id outside BC's list: U1 unsettled: P17. | `crates/al-runtime/src/datefml.rs` Probe::P17 (note_assumed) |
| `System.SerialNumber` | SerialNumber() and ApplicationPath() return ''. | `crates/al-runtime/src/builtins.rs` builtin "serialnumber" \| "applicationpath" |
| `System.ExportObjects` | ExportObjects/DebugMode do nothing and return true. | `crates/al-runtime/src/builtins.rs` builtin "exportobjects" \| "debugmode" |
| `System.HasCollation` | HasCollation returns false (no collation model). | `crates/al-runtime/src/builtins.rs` builtin "hascollation" |
| `System.CodeCoverage` | Code coverage is not recorded; the calls do nothing. | `crates/al-runtime/src/builtins.rs` builtin "codecoverageinclude" \| "codecoveragelog" \| "codecoveragerefresh" |
| `Database.AlterKey` | Database.AlterKey does not enable or disable the key; returns true. | `crates/al-runtime/src/builtins.rs` static_call ("database", "alterkey") |
| `Database.RowVersion` | LastUsedRowVersion/MinimumActiveRowVersion return 0 (no row versions): delta queries on SystemRowVersion see nothing new. | `crates/al-runtime/src/builtins.rs` static_call ("database", "lastusedrowversion" \| "minimumactiverowversion") |
| `Session.StartSession` | Without bc serve's background pool (al-test, bc-nst) StartSession returns false and runs nothing. | `crates/al-runtime/src/builtins.rs` static_call ("session", "startsession") |
| `Session.IsSessionActive` | Without the background pool IsSessionActive returns false. | `crates/al-runtime/src/builtins.rs` static_call ("session", "issessionactive") |
| `NavApp.IsEntitled` | Every app is entitled and licensed: IsEntitled true, IsUnlicensed false. | `crates/al-runtime/src/builtins.rs` static_call ("navapp", "isentitled" \| "isunlicensed") |
| `RequestPage.Static` | RequestPage.* statics evaluate their arguments and return false. | `crates/al-runtime/src/builtins.rs` static_call ("requestpage", _) |
| `Xmlport.Run` | XMLport.Run (needs a file dialog) does nothing and returns LookupCancel. | `crates/al-runtime/src/builtins.rs` static_call ("xmlport", "run") and xmlport.rs "run" |
| `RecordRef.Unopened` | An unopened RecordRef answers Close/IsDirty false and Number 0 instead of the BC error. | `crates/al-runtime/src/builtins.rs` value_method unopened RecordRef "close" \| "isdirty" \| "number" |
| `DotNet.Neutral` | An unimplemented .NET member returns nothing (or an opaque DotNet value) and property writes are dropped. Each hit also records `DotNet.Neutral:<Type>.<Member>` (`assume::hit_member`), which never flags by itself, so `assumed` and the summary split the family by member. | `crates/al-runtime/src/builtins.rs` value_method Val::Opaque("DotNet"); dotnet/mod.rs net_call Kind::Net unknown member, net_set_prop |
| `PlatformObject.Neutral` | A method of a platform object without an implementation returns nothing. Each hit also records `PlatformObject.Neutral:<object>.<Member>` (the variable's declared object, e.g. a native codeunit without AL source). | `crates/al-runtime/src/builtins.rs` value_method Val::Opaque(t) fallback |
| `Record.IsDirty` | Rec.IsDirty is always false (no per-field dirty tracking). | `crates/al-runtime/src/recops.rs` record_method "isdirty" |
| `Record.SecurityFiltering` | SecurityFiltering/SetPermissionFilter are accepted and ignored (no security filters). | `crates/al-runtime/src/recops.rs` record_method "securityfiltering" \| "setpermissionfilter" |
| `Record.Consistent` | Rec.Consistent(false) is ignored: Commit does not check transaction consistency. | `crates/al-runtime/src/recops.rs` record_method "consistent" |
| `Report.HasLayout` | Report.RdlcLayout/WordLayout/ExcelLayout return false (no layout stream handed out). | `crates/al-runtime/src/report.rs` report_static "rdlclayout" \| "wordlayout" \| "excellayout" |
| `Report.ValidateAndPrepareLayout` | Report.ValidateAndPrepareLayout does nothing. | `crates/al-runtime/src/report.rs` report_static "validateandpreparelayout" |
| `Report.DefaultLayout` | Report.DefaultLayout returns RDLC whatever the report declares. | `crates/al-runtime/src/report.rs` report_static/report_method "defaultlayout" |
| `Report.TargetFormat` | TargetFormat returns option 0 whatever the requested output. | `crates/al-runtime/src/report.rs` report_method/curr_report "targetformat" |
| `Report.PrintOnlyIfDetail` | Report variable PrintOnlyIfDetail/Preview/IsTraced return false. | `crates/al-runtime/src/report.rs` report_method "printonlyifdetail" \| "preview" \| "istraced" |
| `Report.OutputDiscarded` | A report's printed or previewed output is not rendered: only the dataset is processed and the document is discarded. | `crates/al-runtime/src/report.rs` report_run_finish (Default/Preview/Print output, rendering skipped) |
| `CurrReport.IsReadOnly` | CurrReport.IsReadOnly/PrintOnlyIfDetail return false. | `crates/al-runtime/src/report.rs` curr_report "isreadonly" \| "printonlyifdetail" |
| `CurrReport.ShowOutput` | CurrReport.ShowOutput returns true (sections always shown). | `crates/al-runtime/src/report.rs` curr_report "showoutput" |
| `CurrReport.PageNo` | CurrReport.PageNo returns 1 (no pagination). | `crates/al-runtime/src/report.rs` curr_report "pageno" |
| `CurrReport.TotalsCausedBy` | CurrReport.TotalsCausedBy returns 0. | `crates/al-runtime/src/report.rs` curr_report "totalscausedby" |
| `CurrReport.SetTableView` | CurrReport.SetTableView/SetSelectionFilter are ignored. | `crates/al-runtime/src/report.rs` curr_report "settableview" \| "setselectionfilter" |
| `CurrReport.Unknown` | An unknown CurrReport method returns nothing. | `crates/al-runtime/src/report.rs` curr_report fallback |
| `SessionInformation.Counters` | SessionInformation counters return 0; other members return nothing. | `crates/al-runtime/src/dotnet/mod.rs` net_static SessionInformation "sqlrowsread" \| "sqlstatementsexecuted" \| "aitokensused" and others |
| `U2.CultureData` | A named culture's number format data (decimal digits, negative sign, currency and percent patterns) is taken to be the invariant culture's (O-3). | `crates/al-runtime/src/rdlc_numfmt.rs` culture |
| `U2.NegativeForm` | For a format class probe P1 does not cover, a negative whose display is the negative sign before the positive display is written as an explicit second section, pos;-pos (O-19). | `crates/al-runtime/src/rdlc_numfmt.rs` record (NEG_FORM) |
| `U2.OpenFormat` | A Format string the converter does not write a code for gives the cell the General format: one the public sources leave open (padded strings, a single non-ASCII letter, bad quotes, empty or extra sections, several %, per mille, odd commas or exponents; BC writes a code for most of them), one where BC writes a #VALUE! cell (X on a Decimal, B, invalid letters), a value-dependent code (G with a precision) or a hex text cell (X on an Integer), or one over 1,024 characters; the member names the item (O-1, O-7, O-9 to O-13, length). | `crates/al-runtime/src/rdlc_numfmt.rs` record (Open) |
| `U2.UserLanguage` | A textbox without any Language (none on its TextRun, Textbox or Report) is formatted in the report's FormatRegion (O-2). | `crates/al-runtime/src/rdlc_xlsx.rs` cell_format |

## Identity defaults (implemented by stream S2 on 2026-11-09; no longer guesses)

| Call | Before | Now | Commit |
|---|---|---|---|
| `UserSecurityId()`, `Database.UserSecurityId` | one constant security id (`{11111111-...}`) for every user and session | the session's own user (`Session::user_sid`) | 6c7b852 |
| `Session.CurrentSessionId` | returned '' | the session id (`workers::current_session_id`) | 6c7b852 |
| `IsolatedStorage.*` | Set/SetEncrypted returned true and stored nothing; Get/Contains/Delete returned false | stored per DataScope, SetEncrypted encrypted at rest (`Session::isolated_storage`) | 09b4ceb |

## Real answers since I15 (stream T; no longer guesses)

Each follows the BC 29.1 service tier [src:TODO].

| Call | Before | Now (BC source) |
|---|---|---|
| `CurrentTransactionType([Type])`, `CurrentTransactionType := Type` | always UpdateNoLocks; the setter was ignored | the running transaction's type: a test's own (`TestCtx::txn`, one per test method) or the session's (`SysState::txn`, reset when the host commits or rolls back a unit of work), starting as UpdateNoLocks. Before the transaction's first statement it is set; after it, the same or a weaker type is ignored and a stronger one raises an error (bc-rust's own text, docs/sources.md row X12) Commit() and an error's rollback start over. A report run sets its TransactionType property (default UpdateNoLocks) the same way; a type other than UpdateNoLocks always takes effect, because BC commits before such a report. Approximation: bc-rust counts writes of non-temporary records as the first statement, BC counts reads too. (`TransactionManager.CurrentTransactionType`, `NavReport.RunReportInternalAsync`/`RunReportCoreAsync`) |
| `RoundDateTime(DateTime [, Precision] [, Direction])` | returned the DateTime unrounded | rounds the client-local milliseconds since 0000-01-01 to Precision (default 1000): '<' down, '>' up, '=' (default) to the nearer, a half up; 0DT stays 0DT; Precision <= 0 or another Direction is an error. (`ALSystemDate.ALRoundDateTime`) Time literals keep their milliseconds now (`235959.999T`). |
| `CurrPage.Editable([Value])`, `CurrPage.Editable := Value`, `PageVar.Editable([Value])`, `PageVar.Editable := Value` | CurrPage.Editable always true; PageVar.Editable false; setters ignored | the page instance's Editable: the page's Editable property (default true) until AL sets it. The open mode does not change it (OpenView pages answer true too): BC's `NavForm.Editable` is initialised from the metadata only, the client applies View mode itself. A page extension's code reads and sets its own instance's value. (`NavForm.InitializeFromMetadata`) |
| `CurrReport.Preview` | false | true while a report runs whose request page closed with Preview (`ReportIntent.Preview`), false otherwise and after the run. (`NavReport.preview`) |
| `NavTestExecution.IsInTestMode()` | nothing (false) | true while a test codeunit runs in the session (not in a background session inside a test). (`NavTestExecution.IsInTestMode`) |
| `NavUserAccountHelper` IsDelegatedUser, IsUserDelegatedAdmin, IsUserDelegatedHelpdesk, TryGetAuthenticationKey, GetAuthenticationStatus, IsPasswordSet, GetAllowedCompanies | nothing (DotNet.Neutral) | The delegated-user checks read the user's Entra directory roles, which no bc-rust sign-in carries: false. TryGetAuthenticationKey: the User Property row's "Authentication Key" (false without SUPER/SECURITY for another user or without a row). GetAuthenticationStatus: Active (2) with an "Authentication Object ID", else Inactive (1) with an "Authentication Email", else Disabled (0); an error without the rows. IsPasswordSet: the User Property row has a Password. A user without a User Property row reads as an empty row: BC creates that row with the User row (`SystemTableTriggers.OnBeforeInsert`), bc-rust does not. GetAllowedCompanies: every company (company permissions are not modelled). (`UserManagement`, `NavUser`, `NavUserAccountHelper`) |
| `"Agent Utilities".BlockPageFromBeingOpenedByAgent()` (native codeunit 2000000022) | nothing (PlatformObject.Neutral) | does nothing: BC raises an error only in an agent session, and bc-rust runs none. (`Codeunit2000000022`) |
| `SessionInformation.Callstack` | '' | the running AL frames, innermost first, each followed by `\`: `<Object>(<ObjectType> <Id>).<Method>[(Trigger)] line <N> - <App> by <Publisher> version <Version>`; names that are not plain identifiers are quoted, a field or page-control trigger is `"<Field> - <Trigger>"`, N is the frame's current statement line minus its procedure's signature line. Each frame's line is kept by both engines (`Session::cur_line`, `frame_lines`). Differences from BC: al-test runs the test method directly, so the test runner's own frames at the bottom are missing; event publishers are not frames; report and XMLport data item triggers are named without their data item. [src:A29.1] |
| `Sid()` | '' (flagged) | the session user's "Windows Security ID" from the User table ('' when the user has none). `Sid(Name)`: see the fixed environment facts. (`ALDatabase.ALSid`) |

## TestPage defaults (testpage.rs belongs to stream S4; not flagged yet)

| Call | What bc-rust assumes | Code |
|---|---|---|
| `TestAction.Visible/Enabled` | true | `crates/al-runtime/src/testpage.rs` "visible" \| "enabled" |
| `TestPage.Expand/Activate` | do nothing | `crates/al-runtime/src/testpage.rs` "expand" \| "activate" |
| `TestPage.IsExpanded` | true | `crates/al-runtime/src/testpage.rs` "isexpanded" |
| `TestField.HideValue` | false | `crates/al-runtime/src/testpage.rs` "hidevalue" |

## Fixed environment facts (not flagged)

These answer what a BC online service tier with a web client would answer, so a test cannot
tell them apart from BC.

| Call | Answer | Code |
|---|---|---|
| `IsServiceTier` | true | `crates/al-runtime/src/builtins.rs` builtin "isservicetier" |
| `CurrentClientType`, `DefaultClientType`, `Session.CurrentClientType` | Web | builtin "currentclienttype"; static_call ("session", ...) |
| `WindowsLanguage` | 1033 | builtin "windowslanguage" |
| `CurrentExecutionMode` | Standard | builtin "currentexecutionmode" |
| `NavUserAccountHelper.IsWindowsAuthentication()` | false | `crates/al-runtime/src/sysfuncs.rs` user_account_member. No bc-rust sign-in authenticates with a Windows account or group, whatever the nominal credential type: `crate::userauth::CredentialType` is called Windows while BC_AUTH is unset (and with BC_AUTH=NavUserPassword unless BC_AUTH_PASSWORD_CHANGE=1), but that only means "no password check". Answering true would send AL's user and security-group pages down Windows paths (Windows user names, Windows group SIDs, `Sid(Name)` lookups) this tier cannot serve. With it false and `IsUserNamePasswordAuthentication`/`IsAccessControlServiceAuthentication` false too, AL takes the paths it takes on BC online. Coordinator decision, I15 04:45 UTC. |
| `Sid(Name)` | '' | `crates/al-runtime/src/sysfuncs.rs` sys_builtin "sid": BC translates the name through the host's Windows accounts (`NTAccount.Translate`) and answers '' when it is not mapped; this tier has no Windows accounts, like BC online |
| `Rec.SetLoadFields/AddLoadFields/LoadFields/ReadIsolation (method form)`, `FieldActive`, `AreFieldsLoaded` | every field is always loaded; on PostgreSQL with table locks `ReadIsolation` is honoured | `crates/al-runtime/src/recops.rs` record_method |
| `Yield`, `Session.EnableVerboseTelemetry`, `CurrReport.CreateTotals/NewPage/NewPagePerGroup` | do nothing (no observable effect in BC either outside telemetry and layout) | builtins.rs, report.rs |

## Refusals ("Not implemented in bc-rust: ...")

These raise an error instead of guessing. A test that catches one is reported `not_trusted`
when it passes.

| Site | What is refused |
|---|---|
| `crates/al-runtime/src/builtins.rs` `Session::miss` | any unknown global function, static (`Class.Method`), RecordId or KeyRef method |
| `crates/al-runtime/src/sysfuncs.rs` | unknown system function |
| `crates/al-runtime/src/enckey.rs` | unknown encryption function |
| `crates/al-runtime/src/tasks.rs` | unknown TaskScheduler method |
| `crates/al-runtime/src/files.rs` | File methods without an implementation |
| `crates/al-runtime/src/streams.rs` | Blob, InStream, OutStream methods without an implementation |
| `crates/al-runtime/src/xmlport.rs` | XMLport import/export with a Format other than Xml/VariableText/FixedText |
| `crates/al-runtime/src/json.rs` | JsonObject/JsonArray/JsonValue methods, JsonValue.As<type> |
| `crates/al-runtime/src/errinfo.rs` | ErrorInfo methods without an implementation |
| `crates/al-runtime/src/recops.rs` | FieldRef methods without an implementation |
| `crates/al-runtime/src/platobj.rs` | Notification, SessionSettings, FilterPageBuilder methods without an implementation |
| `crates/al-runtime/src/query.rs` | Query methods without an implementation |
| `crates/al-runtime/src/dotnet/mod.rs` | AL Xml and Http type members; AL Xml statics |
| `crates/al-runtime/src/resources.rs` | NavApp methods without an implementation |
| `crates/al-runtime/src/dialog.rs` | Dialog methods without an implementation |
| `crates/al-runtime/src/report.rs` | report rendering of a layout bc-rust cannot render, Report methods |
| `crates/al-runtime/src/testpage.rs` | TestPage members without an implementation |

## Intentional differences from BC (date formulas)

- D1: `Format(DateFormula, 0, n)` for n other than 0, 1, 2, 9 raises `Standard format number <n> does not exist for the type 'DateFormula'.` (German: `Die Standardformatnummer <n> ist für den Typ 'DateFormula' nicht vorhanden.`), code InvalidStandardFormat. BC names the type 'Duration'. The one AL Language.Tests case that differs: format-evaluate/Y62DateFormulaFormat.al::DateFormulaFormat_OtherStandards_English.
- D2: date formula error texts write each line break as `\`, the form the AL Language.Tests corpus measured on BC 29.0; probes on BC 29.1 read CR LF there.
