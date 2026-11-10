# Platform system tables and codeunits

BC's service tier provides about 235 system tables (ids 2000000001 and up) that are not in Microsoft's AL source in BCApps. bc-rust defines the ones the loaded AL code needs in `platform/`, and the catalog loads them like any other AL table. Until 2026-11-07 those definitions were written from memory. Ids, field numbers, types and keys were partly invented. Now they are copies of Microsoft's own definitions.

The service tier also provides system codeunits. Most of them only publish events that the platform raises itself, such as System Action Triggers and Company Triggers. `platform/` copies the ones AL code subscribes to or calls, so those subscribers resolve. "Platform events" below lists when BC raises each event and where bc-rust should raise it.

## Source

The BC 29.1.57326.57497 platform artifact contains `ModernDev/PFiles/Microsoft Dynamics NAV/290/AL Development Environment/System.app` (app "System" 29.0.57431.0). That is the symbol package the AL compiler uses. It ships the AL source of every system table, enum, interface and codeunit under `src/`, licensed MIT. To unpack it, strip the 40-byte NAVX header and unzip the rest.

`tools/platform-gen/gen.py` generates the platform files (see its README). They copy each object unchanged except for the `///` doc comments. Each object carries a comment with its path inside `src/`. Don't edit them by hand. Change the generator and rerun it.

Two independent sources agree with the copies:

- The web server's field constants in `Microsoft.Dynamics.Nav.Types` (`*Fields` classes, `SystemTables`). For all 147 copied tables that have such a class, the constants exist as fields in the AL, with one exception. `PrivacyNoticeApprovalFields.Approved` is 5, but System.app's AL has `field(4; Approved; Boolean)`. The web server never uses that constant (it goes through the `ConfirmPrivacyNoticeApproval` and `GetPrivacyNoticeApprovalState` calls), and AL code compiles against field 4, so the platform file keeps 4. The other leftovers are not AL fields: `Timestamp = 0`, the length constant `RoleIdCodeLength`, and `WindowsLanguageFields.PageEnabled = 26`.
- A capture from the real 29.1 NST (`data/capture/nst-real/pre-fix/nst.jsonl`). Its page metadata relates to AllObj field 3, Object ID, filtered on field 1, Object Type. That is the new numbering. The old file had 6 and 3.

## Layout of platform/

AL allows one namespace per file, and the parser keeps one namespace per file. bc-nst sends that namespace to the web client as `ALNamespace` in `MetaTable`, so it has to match BC. Before, everything was `System.Platform`.

- `platform/SystemTables.al` has the header comment and namespace `System.Reflection`: Field, AllObj, AllObjWithCaption, Table Metadata, All Profile, 18 more tables and 3 enums.
- `platform/system/<namespace>.al` has every other namespace, 23 files. The trigger codeunits are in `System.Environment.al` and `System.Text.al`.

A hand-written `platform/*.al` file still works. The generator skips any object such a file defines.

The selection rule is that a System.app object is included when the loaded AL source references it. Seeds came from Record variables and parameters, TableRelation, CalcFormula, page SourceTable, `Database::` constants and report, query and xmlport dataitems. For codeunits, pages and reports they came from variable and parameter types and from `Codeunit::`, `Page::` and `Report::` constants, which covers every `[EventSubscriber]`. The selection then follows the relations and enum types of the included tables, the interfaces of included enums, the Record and Enum types in interface signatures, and the types and object constants that included codeunits name. The result is 162 tables, 18 enums, 6 interfaces and 9 codeunits.

## Codeunits

System.app 29.1 has 35 codeunits, 10 pages and 1 report. The loaded AL references 17 of the codeunits and none of the pages or the report. `platform/` has 9 of the 17, and each of the 9 only publishes events.

| Id | Codeunit | Namespace | Events | Subscribers in the loaded AL |
|---|---|---|---|---|
| 2000000001 | Version Triggers | System.Environment | 3 | 2 |
| 2000000002 | Global Triggers | System.Environment | 10 | 10 |
| 2000000003 | Company Triggers | System.Environment | 4 | 5 |
| 2000000004 | UI Helper Triggers | System.Environment | 6 | 6 |
| 2000000005 | Reporting Triggers | System.Environment | 19 | 17 |
| 2000000006 | System Action Triggers | System.Environment | 43 | 31 |
| 2000000007 | Filter Helper Triggers | System.Text | 6 | 5 |
| 2000000012 | Environment Triggers | System.Environment | 4 | 2 |
| 2000000015 | Feature Management Triggers | System.Environment | 1 | 1 |

The other 8 referenced codeunits stay out on purpose: Agent Utilities, Base64Convert, Extension Database Management, Feature Access Management, Feedback, Index Management, MCP Utilities and Task Pane. Each has `[Native]` procedures, which the service tier implements and System.app's AL leaves empty. bc-rust treats a codeunit that the catalog lacks as a platform object. `al-runtime` `dotnet::platform_codeunit` implements Base64Convert natively, and calls to the others go to `Session::missing`, which al-test reports under "platform calls shimmed or missing". A copy would replace both with empty AL bodies, so Base64Convert would return empty text and the missing calls would succeed silently with blank values. The generator leaves out every codeunit that has a `[Native]` procedure and names them in its output. It also leaves out a System.app object when the app source defines one of the same kind and name.

The check at the end of a run covers codeunits too. The probe dumps every procedure (attributes, parameters, return type, locals and body, without line numbers) as bc-rust's parser reads it, and all 9 copies equal System.app.

Some of Microsoft's tests call these publishers directly. Navigation Bar Sub. Test (132598) calls `"System Action Triggers".OpenGeneralSetupExperience()` and `OpenRoleBasedSetupExperience()` and expects the subscribers in the System Application's Navigation Bar Sub. Impl. to open Advanced Settings and Assisted Setup. Both tests still fail, because of the first follow-up below.

## What changed in the original 15 tables

"PG" says what an existing PostgreSQL database needs. Details are in the next section.

| Table | Before | Now (System.app) | PG |
|---|---|---|---|
| Company 2000000006 | Id 4, Business Profile Id 5 | Id 8000, Business Profile Id 8005 | copy columns |
| User 2000000120 | License Type: ..., External User, Agent, AAD Group | License Type: ..., External User, External Administrator, External Accountant, Application, AAD Group, Agent. Extra key on Windows Security ID | none |
| Field 2000000041 | SQLDataType had Undefined. OptionString Text[2048]. DataPerCompany = false | SQLDataType Varchar, Integer, Variant, BigInteger. OptionString Text[2047]. New 10 ExternalName, 60 App Package ID, 61 App Runtime Package ID, 62 OptimizeForTextSearch, 63 Access, 64 IsAllowedInCustomizations. No DataPerCompany property, as in BC | never read |
| AllObj 2000000038 | 3 Object Type, 6 Object ID, 9 Object Name, 12, 15 | 1 Object Type (adds Interface = 28), 3 Object ID, 4 Object Name, 5 Name, 60 App Package ID, 61 App Runtime Package ID, 62 AL Namespace | never read |
| AllObjWithCaption 2000000058 | 3, 6, 9, 12 Caption, 15 Subtype, 18, 21 | 1, 3, 4, 5 Name, 20 Object Caption, 30 Object Subtype, 60, 61, 62 App ID, 63 AL Namespace | never read |
| Table Metadata 2000000136 | 7 fields. TableType 5, ObsoleteState 6, Access 7 | 23 fields. TableType 11 (Query sits before Temporary), ObsoleteState 13, Access 22, plus LookupPageID, DrillDownPageId and others | never read |
| Integer 2000000026 | | same fields | never read |
| Date 2000000007 | Period Name Text[30] | Period Name Text[31], new 6 Period Invariant Name | never read |
| Access Control 2000000053 | 4 User Name, 5 Role Name, 6 Scope, 7 App ID, 8 App Name, all normal fields | 5 User Name, 7 Role Name and 10 App Name are FlowFields. 8 Scope, 9 App ID. PK is fields 1, 2, 3, 8, 9. Key on Role ID | recreate |
| User Personalization 2000000073 | 6 Profile ID, 9 Language ID, 12 Company, 24 Locale ID, 27 Time Zone, 30 App ID, 31 Scope, 32 Emit Version, 33 Restart | 9 Profile ID, 10 App ID, 11 Scope, 12 Language ID, 15 Company, 27 Locale ID, 30 Time Zone, 32 Customization Status, 34 Emit Version. FlowFields 6 User ID, 7 Full Name, 13 Language Name, 28 Region, 31 License Type, 33 Role. Keys on Profile ID and Company | recreate |
| All Profile 2000000178 | Description Text[250]. 9 to 12 named Use OneNote, OneNote Path, Use Custom Help, Custom Help Path. 18 Source | Description Text[2048]. 7 to 12 are obsolete, and 9 to 12 are named Use Record Notes, Record Notebook, Use Page Notes, Page Notebook. No field 18 | optional |
| Feature Key | id 2000000182 | id 2000000211. Mandatory By Version Text[2048], new 10 Description In English | moved |
| Record Link | id 2000000065, which is really Send-To Program | id 2000000068. 7 Description, 8 Type, 9 Note, 10 Created, 11 User ID Text[132], 12 Company, 13 Notify, 14 To User ID Text[132]. Keys on Record ID and on Company, Record ID | moved |
| Media Resources | id 2000000114, which is really Document Service. 2 Blob, 4 MediaSet Reference | id 2000000182. 2 MediaSet Reference, 3 Media Reference, 4 Blob | moved |
| Tenant Web Service 2000000168 | 6 Service Name Text[240], 9 Object ID | 6 Object ID, 9 Service Name Text[250]. Longer Object Type option list. New 13 ExcludeFieldsOutsideRepeater, 14 ExcludeNonEditableFlowFields. Key on Object Type, Object ID | recreate |

Fields with `ObsoleteState = Removed` are in the copies (for example the debugger fields of User Personalization), but the catalog drops them, as the AL compiler hides them.

## Existing PostgreSQL databases

bc-store names a table `t<id>` in schema `shared` (DataPerCompany = false) or `c_<company>`, with columns `f<field no>`. bc-server and bc-nst run `sync_schema` over every catalog table at startup. `ensure()` only creates missing tables, columns and indexes. It never changes a column type, never drops a column and never changes a primary key. A table whose field numbers changed meaning therefore keeps its old columns. Startup does not fail. Reads and writes of that table fail with type errors or return wrong data. Nothing on the server side writes rows into these tables (`init_platform_data` runs only in `al-test`), so their rows came from AL code or users. Check whether a table is empty before dropping it.

The coordinator migrates the live database `bc`. This change did not touch any database.

Must be dropped and recreated, after copying any rows you want to keep:

- `shared.t2000000053` Access Control. The PK changed. To keep rows, copy f1, f2 and f3, then f6 to f8 (Scope) and f7 to f9 (App ID). The FlowFields need no copy.
- `shared.t2000000073` User Personalization. Old to new: f3 to f3, f6 to f9, f9 to f12, f12 to f15, f24 to f27, f27 to f30, f30 to f10, f31 to f11, f32 to f34. Old f15, f18, f21 and f33 are gone.
- `shared.t2000000168` Tenant Web Service. Old to new: f3 to f3, f6 to f9, f9 to f6, f12 to f12.
- `shared.t2000000182`. It held Feature Key and now belongs to Media Resources. Move its rows to `shared.t2000000211` first. The column numbers are the same.
- `shared.t2000000114`. It held Media Resources and now belongs to Document Service. Recreate `t2000000182` before moving its rows there: f1 to f1, f2 to f4, f3 to f3, f4 to f2.

Data copy, no drop:

- `shared.t2000000006` Company. The sync adds f8000 and f8005 but leaves them blank. Run `UPDATE shared.t2000000006 SET f8000 = f4, f8005 = f5`, or existing companies get a null Id.
- `shared.t2000000065`. It held Record Link and nothing uses this id now. Move its rows to `shared.t2000000068`: f1, f2 and f3 unchanged, then f4 to f7, f5 to f8, f6 to f9, f7 to f10, f8 to f11, f9 to f12, f10 to f13, f11 to f14. Then drop it.
- `shared.t1563` and `shared.t1564`, only if a database ran while the hand-written `platform/PrivacyNotice.al` existed (Privacy Notice and Privacy Notice Approval with invented ids, now 2000000237 and 2000000238). Move user approvals from `t1564` to `shared.t2000000238`: f1 and f2 unchanged, f3 (Approved) to f4, f4 (Approver User SID) to f3. In `t1563` only f1 to f3 hold data, and their numbers are the same in `shared.t2000000237`. Then drop both.

Optional:

- `shared.t2000000178` All Profile. Column types are compatible and bc-nst refills the table at every boot. Recreating it only removes the leftover column f18.
- Integer, Date, Field, AllObj, AllObjWithCaption and Table Metadata (`shared.t2000000026`, `t2000000007`, `t2000000041`, `t2000000038`, `t2000000058`, `t2000000136`). bc-rust computes these and never reads them from PostgreSQL. The sync still creates them, and AllObj, AllObjWithCaption and Table Metadata keep stale columns and PKs. Field is now per company, so the sync creates `c_<company>.t2000000041` and the shared one is left behind. Drop the shared ones to clean up.

User is unchanged. The sync adds the index on Windows Security ID. The 147 newly added tables have new ids and get created on first sync.

## Still missing

Unresolved table names in the loaded AL dropped from 126 to 4, and only one of them is a system table:

- MCP Configuration Agent. The System Application's MCP module uses it (7 references), but System.app 29.1 does not define it. The vendored BCApps is newer than this platform build.
- Capacity, AllocationAccount and Forecast are BaseApp names the probe could not resolve. They are not system tables.

System.app tables that are not in `platform/`, because nothing in the loaded AL references them (kind is where BC stores them):

| Id | Table | Kind in BC |
|---|---|---|
| 2000000010 | Tenant Fabric Setup | tenant |
| 2000000011 | Tenant Fabric Tables | tenant |
| 2000000012 | Tenant Fabric Companies | tenant |
| 2000000013 | Tenant Fabric Export Summary | tenant |
| 2000000014 | Tenant Fabric Export Details | tenant |
| 2000000015 | Tenant Fabric Table Fields | tenant |
| 2000000016 | Tenant Fabric Enum Mapping | tenant |
| 2000000020 | Drive | virtual |
| 2000000022 | File | virtual |
| 2000000048 | Database | virtual |
| 2000000055 | SID - Account ID | virtual |
| 2000000065 | Send-To Program | application |
| 2000000066 | Style Sheet | application |
| 2000000071 | Object Metadata | application |
| 2000000074 | Profile Metadata | application |
| 2000000075 | User Metadata | tenant |
| 2000000081 | Upgrade Blob Storage | application |
| 2000000083 | Tenant Profile Setting | tenant |
| 2000000086 | Profile Configuration Symbols | tenant |
| 2000000100 | Debugger Breakpoint | application |
| 2000000101 | Debugger Call Stack | virtual |
| 2000000102 | Debugger Variable | virtual |
| 2000000103 | Debugger Watch Value | virtual |
| 2000000104 | Debugger Watch | application |
| 2000000123 | AI Feature Trial Log Entry | tenant |
| 2000000150 | NAV App Object Metadata | application |
| 2000000151 | NAV App Tenant App | application |
| 2000000152 | NAV App Data Archive | tenant |
| 2000000155 | NAV App Publish Reference | application |
| 2000000156 | NAV App Published App | tenant |
| 2000000158 | Agent Action Approval | tenant |
| 2000000160 | NAV App | application |
| 2000000161 | NAV App Dependencies | application |
| 2000000162 | NAV App Capabilities | application |
| 2000000163 | NAV App Object Prerequisites | application |
| 2000000169 | NAV App Tenant Add-In | tenant |
| 2000000174 | Recent Record | virtual |
| 2000000176 | NAV App Resource | application |
| 2000000180 | Media Set | application |
| 2000000186 | Profile Page Metadata | application |
| 2000000190 | Entitlement Set | application |
| 2000000191 | Entitlement | application |
| 2000000194 | Webhook Notification | tenant |
| 2000000195 | Membership Entitlement | application |
| 2000000198 | Page Documentation | application |
| 2000000202 | All Control Fields | virtual |
| 2000000203 | Report Data Items | virtual |
| 2000000208 | Application Resource | application |
| 2000000209 | Application Dependency | application |
| 2000000210 | Tenant Feature Key | tenant |
| 2000000222 | Designed Query Permission | tenant |
| 2000000224 | Designed Query Group | tenant |
| 2000000225 | Designed Query Management | virtual |
| 2000000226 | Query Navigation | tenant |
| 2000000227 | Extension Execution Info | virtual |
| 2000000229 | Report Printer | virtual |
| 2000000230 | Page Usage State | tenant |
| 2000000239 | Tenant Application Storage | tenant |
| 2000000243 | Ext. Business Event Definition | virtual |
| 2000000256 | Table Mappings | tenant |
| 2000000257 | Table Field Mappings | tenant |
| 2000000259 | Agent Access Control Data | tenant |
| 2000000268 | Agent Task Message Data | tenant |
| 2000000270 | Agent Task User Int. Data | tenant |
| 2000000272 | Agent Task File Data | tenant |
| 2000000273 | Agent Task Msg Attach Data | tenant |
| 2000000284 | User Consumption Log Entry | tenant |
| 2000000288 | Code Coverage Test Lookup | virtual |
| 2000000289 | Code Coverage Tests Run | virtual |
| 2000000294 | Table Relations Buffer | temporary |
| 2000000296 | Similarity Result | temporary |
| 2000000298 | Agent Troubleshoot Info Data | tenant |
| 2000000400 | Inplace Installed Application | application |

Also not copied: 11 System.app enums nothing uses (EventCategory, TestHandler, DefaultTestHandler, Permission Object Type, Agent Approval Lifetime, Agent Approval Scope, AI Feature Trial Recurrence and four Fabric enums), the ITestHandler, ITestContext, ITestDataSource and IUnknown interfaces, the 8 `[Native]` codeunits named under "Codeunits", 18 codeunits the default app set doesn't reference, all 10 pages and the report. The unreferenced codeunits are Upgrade Triggers, Extension Triggers, User Triggers, SmartList Designer Triggers, Debugger Triggers, Column Picker Helper, Semantic Search, Mime Type Utility, Designer Utilities, Permission Set Provider, Fabric Export Manager, Agent Task Internal, Agent User Intervention, Personal Agent Metadata and four Agent Default codeunits.

## Platform events

BC's service tier raises the trigger-codeunit events itself, from client calls, the session lifecycle, record writes and report runs. No AL code raises them. This list draws on the `///` comments in System.app's `src/System Codeunits/Triggers`, the 79 subscribers in the default app set, and the web client calls in `data/capture/nst-real`. The last column says whether bc-rust should raise each event, and where. Since 2026-11-07, bc-nst raises these (`crates/bc-nst/src/platform.rs`, test `platform_trigger_events` in `crates/bc-nst/tests/ws_session.rs`): Company Triggers OnCompanyOpen, OnCompanyOpenCompleted (isolated) and OnCompanyClose; System Action Triggers GetAutoStartTours, GetIsLegacyActionBarEnabled, GetRoleCenterBannerPartID, Get/SetNotificationStatus and GetDefaultRoleCenterID (at startup); UI Helper Triggers GetSystemIndicator and CaptionClassTranslate; Filter Helper Triggers Make*Filter in EvaluateFilter. al-runtime itself raises Reporting Triggers SubstituteReport (`crates/al-runtime/src/report.rs`, test `substitute_report_event`), so bc-server and al-test get it too. bc-server raises none of the others yet.

- "yes" means loaded AL depends on the event in normal use, and bc-rust already has the code path where it belongs.
- "later" means the feature the event belongs to is not in bc-rust yet (layouts, printing, Excel, a system-action call).
- "no" means bc-rust has no counterpart (SaaS environment copy, on-prem tokens, Copilot), the event is obsolete, or nothing subscribes.
- "(bare)" marks subscribers with a bare event name. They only fire after the al-model fix under "Follow-ups".

To raise an event from Rust, call `Session::raise_event(ObjectKind::Codeunit, <id>, "<Event>", "", args, None, None)` with arguments named like the publisher's parameters. `bc_nst::form::resolve_caption_class` does this for codeunit 42. Pass `var` parameters as `Arg::Ref(LVal::Slot(..))` and read the slots back afterwards. `raise_event` returns at once when nothing subscribes.

### Company Triggers (2000000003)

| Event | BC raises it | Subscribers and what they do | bc-rust |
|---|---|---|---|
| OnCompanyOpen | While a session opens a company, before the client UI loads. `Scope('OnPrem')`. | System Initialization Impl. (bare) runs codeunit Azure AD User Management, sets the signup context and writes the environment login record for UI clients, then raises `"System Initialization".OnAfterInitialization`. | yes. `Conn::open_company` in `crates/bc-nst/src/rpc.rs`, after `Session::try_new` and the user setup. Per session in bc-server too. Its API opens a session per request (`api.rs`), so it pays this on every call. |
| OnCompanyOpenCompleted | After the company open finished. Isolated (`BusinessEvent(false, true)`), so a failing subscriber rolls back its own changes and does not fail the open. | System Initialization Impl. (bare) raises `"System Initialization".OnAfterLogin`, which BaseApp's LogInManagement and others subscribe to. | yes. Right after OnCompanyOpen. `raise_event` has no isolation, so catch and log subscriber errors at the call site. |
| OnCompanyClose | While a session closes its company. | LogInManagement runs `CompanyClose`, which ends the login for UI clients. Job Queue User Handler reschedules the user's job queue entries when `GuiAllowed`. | yes. `Conn::close_company` in `rpc.rs`, before `s.rollback()`. That rollback discards whatever the subscribers write unless it is committed first. |
| OpenCompanySettings | The user opens company settings from the web client's settings menu. | Navigation Bar Sub. Impl. (bare) raises `OnBeforeDefaultOpenCompanySettings` and errors when nobody handles it. | later, with the system-action call. |

### System Action Triggers (2000000006)

Most events answer a web client call or run an action from the client's menus. bc-nst answers some of these calls with fixed values in `Conn::call` (`rpc.rs`). The real NST capture shows the web client sending GetAutoStartTours, GetIsLegacyActionBarEnabled and GetRoleCenterBannerPartId while it builds the shell.

| Event | BC raises it | Subscribers and what they do | bc-rust |
|---|---|---|---|
| GetAutoStartTours | Web client call GetAutoStartTours. | User Settings Impl. (bare) returns the user's Teaching Tips setting. | yes. Replace the fixed `false` in `Conn::call`. |
| GetIsLegacyActionBarEnabled | Web client call GetIsLegacyActionBarEnabled. | User Settings Impl. returns the user's setting. | yes. Replace the fixed `false`. |
| GetRoleCenterBannerPartID | Web client call GetRoleCenterBannerPartId. `InternalEvent`. | Checklist Banner Impl. (bare) returns page Checklist Banner when the user's checklist should show. | yes. Replace the fixed `0`. |
| GetNotificationStatus, SetNotificationStatus | Web client calls of the same names, when a notification is about to show or the user turns one off. | My Platform Notifications reads or writes the user's My Notifications rows. | yes. Replace the fixed `true` and `null`. |
| GetDefaultRoleCenterID | When the platform needs a default role center for a user. | Default Role Center Impl. (bare) returns `GetDefaultRoleCenterId()`, a role center page it checks against AllObjWithCaption. | yes. Where bc-nst picks the role center from the profile (`"Role Center ID"` in `rpc.rs`). |
| OpenSettings, OpenGeneralSetupExperience, OpenRoleBasedSetupExperience, OpenCopilotCapabilities, OpenAppSourceMarket, OpenLastErrorPage, OpenPermissionSetCard, InvokeExtensionInstallation | Actions in the web client's menus and links: My Settings, Advanced Settings, Assisted Setup, Copilot capabilities, AppSource, the last error, a permission set, an AppSource install link. | User Settings Impl. (bare), Navigation Bar Sub. Impl. (bare), AppSource Product Manager (bare), Help & Support Management, Permission Set Card Subscriber and Extension Marketplace (bare). Each opens a page. | later. Capture the call the web client sends for these first. bc-nst logs any call it doesn't know ("not implemented: ..."). |
| GetFindEntriesPageId, GetPerformanceTroubleshooterPageId, GetProfilerSchedulesPageId, GetDataSearchPageId | When the client needs the page id of Find entries, the performance profiler, profiler schedules or company data search. | Help & Support Management returns Navigate. Profiling Data Processor and Scheduled Perf. Profiler Impl. (both bare) return their pages. Nothing subscribes to GetDataSearchPageId. | later. OpenConnection sends `DataSearchPageId: 0`. |
| GetSupportInformation | When the client shows the support contact. | Help & Support Management reads Support Contact Information unless one of its own subscribers answers first. | later |
| OnOpenInExcel, OnEditInExcelWithStructuredFilter | Open in Excel and Edit in Excel. | Document Service Management, Edit in Excel Impl. (bare). | later |
| ConfirmPrivacyNoticeApproval, GetPrivacyNoticeApprovalState | When a feature needs the user's approval of a privacy notice. | Privacy Notice Impl. (bare). | later. OpenCompany sends `PrivacyNoticeStates: {}`. |
| GetPowerPlatformEnvironmentId | Power Automate integration. | Flow Service Management. | later. OpenCompany sends `PowerPlatformEnvironment: {}`. |
| CustomizeChart | Customizing a chart part. | Generic Chart Mgt. | later |
| ChangeCompany | Company switch from the Outlook add-in. | Office Management. | no |
| GetUserToken | On-prem Entra token for a resource. | Azure AD Mgt. | no |
| GetCopilotCapabilityStatus, GetCopilotCapabilityInfo, GetPageSummary, GetAgentTaskMessagePageId, OnFeedbackEvent | Copilot and agents. | Copilot Capability Impl., Agent Task Impl. and Agent Task Log Entry, all bare. | no, until bc-rust has Copilot or agents. |
| OpenContactMSSales, OnEditInExcel, OnEditInExcelWithSearchString, OpenDataSearchPage, GetDataSearchSetup, GetDisplayPageId, OnCopilotFeedbackProperties, OnCopilotSystemActionInvokedProperties, GetAgentTaskEvalExecutionContext, OnAfterAgentTaskStatusChanged, OpenDebugger | | Nothing in the default app set subscribes. OpenDebugger is obsolete. | no |

### UI Helper Triggers (2000000004)

| Event | BC raises it | Subscribers and what they do | bc-rust |
|---|---|---|---|
| CaptionClassTranslate | Whenever the platform resolves a `CaptionClass` property, and for the AL function `CaptionClassTranslate()`. | Caption Class Impl. (bare) handles area 3 itself and raises `"Caption Class".OnResolveCaptionClass` for the other areas. | yes. `captionclasstranslate` in `al-runtime/src/builtins.rs` returns its argument unchanged. `bc_nst::form::resolve_caption_class` copies Caption Class Impl. by raising codeunit 42 directly. Both can raise this event once bare names index. |
| GetSystemIndicator | When the client shows the company badge. | LogInManagement reads System Indicator, System Indicator Style and Custom System Indicator Text from Company Information. | yes. `Conn::open_company` sends `Indicator: {"text": "", "style": 0}`, and `get_allowed_companies` fills SystemIndicatorText and SystemIndicatorStyle. |
| AutoFormatTranslate | When the platform formats a decimal field or control that has AutoFormatType and AutoFormatExpression, such as an amount in a currency. | Auto Format Impl. (bare) returns the format string. | later. Nothing in bc-rust reads AutoFormatType yet. bc-nst's decimal formatting is the place. |
| GetDefaultRoundingPrecision | When the platform needs the default amount rounding precision. | Auto Format Impl. (bare) returns `ReadRounding()`. | later, with AutoFormatTranslate. |
| GetCueStyle | When a cue on a role center shows its value. | Cues And KPIs Impl. (bare) returns the style from the cue's thresholds. | later, with cue groups in bc-nst. |
| GetApplicationLanguage | When the platform picks the application language. | Language Impl. (bare) returns `GetDefaultApplicationLanguageId()`. | later. bc-nst takes the LCID from OpenConnection and SetClientSettings. |

### Reporting Triggers (2000000005)

BaseApp's ReportManagement (codeunit 44) subscribes to 16 of the 19 events. Most forward to ReportManagement's own `OnAfter...` events, where apps hook in.

| Event | BC raises it | Subscribers and what they do | bc-rust |
|---|---|---|---|
| SubstituteReport | Before a report run started by id: the static Report.Run, RunModal, Execute, Print, SaveAs* and RunRequestPage, page actions with RunObject = Report, and Report.GetSubstituteReportId, with the run mode (Normal, ParametersOnly, Execute, Print, SaveAs, RunModal). Report variables (`Rep.Run()`) are not substituted. NewReportId starts at -1, and only a different id counts. | ReportManagement raises `OnAfterSubstituteReport`, where apps replace one report id with another. | raised since 2026-11-07: `Session::report_substitute` in `al-runtime/src/report.rs`, called by `report_static` before it creates the instance and by the TestPage RunObject action (`testpage.rs`). |
| SelectedBuiltinLayoutType, SelectReportLayoutCode, HasCustomLayout, FetchReportLayoutByCode, SelectReportLayoutUI | While the platform picks the layout to render. SelectReportLayoutCode and FetchReportLayoutByCode exist for BC 20 compatibility. | ReportManagement reads Report Layout Selection and Custom Report Layout. | later, when `report.rs` renders layouts. |
| CustomDocumentMergerEx | Rendering a layout of type Custom. | ReportManagement raises `OnCustomDocumentMergerEx`. | later |
| OnIntermediateDocumentReady, OnDocumentReady, OnDocumentDownload, OnDocumentPrintReady, GetFilename | After rendering: an intermediate document, the final document, a download to the client, a document ready to print, the file name for a download. | ReportManagement forwards each to an `OnAfter...` event. | later |
| GetPrinterName, GetPaperTrayForReport, SetupPrinters | Printing: the printer for a report, its paper trays, the printers the client can pick. | ReportManagement. | later |
| ScheduleReport | The user picks Schedule on a report request page. | Job Queue Management creates a job queue entry. | later, with request pages in bc-nst. |
| GetCompanyMetadata | A report layout uses the company metadata payload. | ReportManagement fills it. | later |
| ApplicationReportMergeStrategy, WordDocumentMergerAppMode | Obsolete. | none | no |

### Global Triggers (2000000002)

The only trigger codeunit without `SingleInstance`. Its one subscriber is BaseApp's GlobalTriggerManagement (codeunit 49).

| Event | BC raises it | Subscribers and what they do | bc-rust |
|---|---|---|---|
| GetDatabaseTableTriggerSetup | When the platform needs to know which database triggers a table has. | GlobalTriggerManagement asks CRM Integration Management, API Webhook Notification Mgt. and, in a normal execution context, Change Log Management to set the four flags. One more subscriber hooks its `OnAfterGetDatabaseTableTriggerSetup`. | yes. Ask on the first write to a table in a session and cache the four flags per table in `Session`. |
| OnDatabaseInsert, OnDatabaseModify, OnDatabaseDelete, OnDatabaseRename | After a record write, for tables whose flag is set. System.app says "at database level", so AL writes count, not only page edits. | GlobalTriggerManagement writes Change Log entries, Dataverse integration records and API webhook notifications. | yes. `rec_insert`, `rec_modify`, `rec_delete` and the `rename` arm in `al-runtime/src/recops.rs`, after the write, with a RecordRef of the record (and of the old one for rename). Check on the side-by-side VM whether BC also raises them for `Insert(false)`. |
| GetGlobalTableTriggerMask, OnGlobalInsert, OnGlobalModify, OnGlobalDelete, OnGlobalRename | The page-level variant, for changes users make on pages, on tables whose mask bit is set. | GlobalTriggerManagement forwards them to its own `OnAfter...` events, and nothing in the default app set subscribes to those. | no, until something subscribes. |

### Filter Helper Triggers (2000000007)

| Event | BC raises it | Subscribers and what they do | bc-rust |
|---|---|---|---|
| MakeDateFilter, MakeDateTimeFilter, MakeTimeFilter, MakeTextFilter, MakeCodeFilter | When a user enters a filter for a field of that type, before the platform parses it. | Filter Tokens Impl. (bare) replaces tokens such as TODAY, WORKDATE, USER and COMPANY, plus their translations. | yes. `Conn::evaluate_filter` in `rpc.rs` and wherever bc-nst applies a filter from the client, before `parse_field_filter`. Pick the event from the field type. |
| MakeIntFilter | Integer fields. | none | no |

### Version Triggers (2000000001)

| Event | BC raises it | Subscribers and what they do | bc-rust |
|---|---|---|---|
| GetApplicationVersion, GetApplicationBuild | When the platform reports the application version and build. | Application System Constants returns `ApplicationVersion()` and `ApplicationBuild()`. | not now. In the vendored source `ApplicationVersion()` returns the placeholder `'!Build Version!'`, which Microsoft's build replaces. `ApplicationBuild()` returns the revision of `ModuleInfo.AppVersion`, which bc-rust's ModuleInfo stub reports as 0. bc-nst's configured values are better. The real NST sends ApplicationVersion `29.1.57326.57497` and ApplicationBuild `57497` in OpenConnection. |
| GetReleaseVersion | | none | no |

### Environment and Feature Management Triggers (2000000012, 2000000015)

| Event | BC raises it | Subscribers and what they do | bc-rust |
|---|---|---|---|
| OnAfterCopyEnvironmentPerDatabase, OnAfterCopyEnvironmentPerCompany | After BC online copies an environment, once per database and once per company. | Environment Cleanup Impl. (bare) raises `"Environment Cleanup".OnClearDatabaseConfig` and `OnClearCompanyConfig`, where apps clear their configuration in the copy. | no, unless bc-rust gets a tool that copies a database. Such a tool should raise them. |
| OnAfterCopyEnvironmentToSandbox, OnAfterCopyEnvironmentToSandboxPerCompany | After a copy to a sandbox. | none | no |
| OpenFeatureManagement | The user opens Feature Management. | Feature Management Impl. (bare) runs page Feature Management unless a subscriber handles it. | later, with the system-action call. |

### Not in platform/

Nothing in the default app set subscribes to Upgrade Triggers, Extension Triggers, User Triggers, SmartList Designer Triggers or Debugger Triggers. BCApps runs install and upgrade code through codeunits with `Subtype = Install` and `Subtype = Upgrade` instead. The generator copies any of them as soon as AL code references it.

## Follow-ups outside platform/

- `index_subscribers` in `crates/al-model/src/catalog.rs` drops every subscriber whose event name is a bare identifier, and it does so without a warning. AL allows both `'OnCompanyOpen'` and `OnCompanyOpen`. The parser turns the second form into `Expr::Ident`, and the `let event = match &a.args[2]` arm only accepts `Expr::Str`. In the default app set 285 of 3431 `[EventSubscriber]` attributes use a bare name, 35 of them on trigger codeunits. Every Filter Helper, Feature Management and Environment Triggers subscriber is among them, as are both company-open subscribers, 5 of the 6 UI Helper Triggers subscribers and 19 of the 31 System Action Triggers subscribers. The fix is one line: `Expr::Str(s) | Expr::Ident(s) => s.to_ascii_lowercase()`. I tried it in a private copy of the tree. Indexed subscriptions went from 3136 to 3421, warnings stayed at 0, and both Navigation Bar Sub. Test cases passed. The other 250 subscribers (Upgrade Tag, Inventory Adjustment, Retention Policy and others) start firing as well, so the fix needs a full parity run before it lands.
- bc-rust raises none of the events under "Platform events" yet. Until they are, the copied codeunits only make subscribers resolve, and AL code that calls a publisher directly reaches its subscribers.
- The 8 `[Native]` codeunits need a runtime hook before they can be copied: a way to route a `[Native]` procedure to Rust, falling back to `Session::missing`. With that hook in place, drop the `[Native]` rule in `gen.py`.
- `crates/al-runtime/src/virt.rs` gives Table Metadata."TableType" ordinal 5 for temporary tables. In BC's option list 5 is Query and Temporary is 6. It also fills only ID, Name, Caption, DataPerCompany and TableType of the 23 fields.
- The next run of the bc-nst `golden_page_21` test rewrites `crates/bc-nst/tests/golden/page-21.pagewithdependencies.json`. Relations to AllObjWithCaption change from field 6 to field 3, filtered on field 1. The real NST capture has this numbering too.

## Regenerating

```sh
CARGO_TARGET_DIR=/tmp/cargo-h python3 -I tools/platform-gen/gen.py \
    --app <artifacts>/29.1.57326.57497/platform.zip
```

`tools/platform-gen/README.md` describes the inputs, the selection and the check at the end of a run. Every generated table and codeunit must equal System.app as bc-rust's parser reads it, and all 162 tables and 9 codeunits do. The run also prints the catalog's subscribers to unknown publishers, which are 0 now. Before the trigger codeunits were copied there were 44: Reporting Triggers 17, System Action Triggers 12, Global Triggers 10, Version Triggers 2, Company Triggers 2 and UI Helper Triggers 1. Rerunning on unchanged inputs leaves `platform/` as it is. For a newer BC build, pass its `platform.zip`, then review the layout changes and update this file. A new trigger codeunit or event needs a row under "Platform events".
