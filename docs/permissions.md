# Permissions

How Business Central decides whether code may read or change a table, and how bc-rust models it. Sources: the BC 29.1 service tier (Ncl `PermissionManager`, `PermissionSetBase`, `PermissionSet`, `RecordImplementation`, `NavUserPermissions`, `NavTestExecution`, `InherentObjectPermissionAndEntitlementsHelper`), the PermissionTestHelper add-in (`Add-ins/PermissionTestHelper/Microsoft.Dynamics.Nav.PermissionTestHelper.dll` in platform.zip), the System Application's permission modules and the test libraries (Permissions Mock 131006, Library - Lower Permissions 132217, Permission Test Catalog 132218, codeunits 130301/130302/130453).

Status (2026-11-08, 15:45 UTC): metadata permission sets are loaded and expanded; the test runner's permission step runs behind `al-test --runner-permissions`; record checks run behind `al-test --enforce-permissions` or `BC_PERMISSIONS=enforce`, for a test's effective sets and for non-SUPER users (Access Control). All of it is off by default. Code: crates/al-runtime/src/permissions.rs.

## Tables

| Table | Id | What it holds | bc-rust |
|---|---|---|---|
| Metadata Permission Set | 2000000250 | one row per `permissionset` object (App ID, Role ID = object name, Name = Caption, Assignable) | rows at platform init (platform.rs) |
| Metadata Permission | 2000000251 | the set's `Permissions` entries (object type/id, R/I/M/D/X as Yes or Indirect, security filter) | catalog table from the model (`permissions::catalog_rows`), landed 17:49 UTC |
| Metadata Permission Set Rel. | 2000000252 | IncludedPermissionSets / ExcludedPermissionSets (Type Include or Exclude) | catalog table from the model (same) |
| Tenant Permission Set / Tenant Permission / Tenant Permission Set Rel. | 2000000165 / 166 / 253 | user-defined sets, stored per tenant | ordinary tables (data only) |
| Aggregate Permission Set | 2000000167 | union of both, Scope System or Tenant, with App Name | System rows at platform init |
| Permission Set / Permission | 2000000004 / 5 | the legacy tables; Permissions Mock still does `PermissionSet.Get(RoleID)` | Permission Set rows at platform init |
| Access Control | 2000000053 | user → role (Role ID, App ID, Scope, Company Name) | the session user's SUPER row |
| Expanded Permission | 2000000254 | the flattened permissions of a set (ExpandedPermissionDataProvider: graph walk over metadata and tenant sets, wildcards merged into each object entry) | catalog table for System sets, plus the tenant sets' expansion when read (`Session::expanded_permission_rows`), landed 17:49 UTC |
| License Permission | 2000000043 | what the license allows per object | virtual, everything allowed |

`permissionsetextension` objects add `Permissions` and `IncludedPermissionSets` to the set they extend. The catalog keeps them in `permission_set_exts` and the model merges them into the extended set.

## Effective permissions

A permission is a mask per object: Read, Insert, Modify, Delete, Execute, each either direct or indirect (Ncl `PermissionMask`: direct bits 1..16, indirect bits shifted by 5). In a set, upper case (`R`) is direct and lower case (`r`) is indirect; direct implies indirect. Object id 0 means every object of the type (`table * = X`).

A role's permissions are its set's `Permissions`, plus its extensions', plus the expansion of every included set, minus the expansion of every excluded set. `Model::role` computes this once per catalog and role id and keeps it.

BC computes a session's permission on an object as license ∩ (union of the user's roles), and with membership entitlements (online only) ∩ entitlements:
- The roles come from Access Control for the session's company plus the user's security groups (`NavUserPermissions.GetRoles`), each role being the System-scope set of that id (from the app that defines it) and the Tenant-scope set of that id.
- A user with SUPER for all companies, or a server with the permission system off, gets the license permission set (`PermissionManager.HasAllPermissions`), which here means everything.
- In a test with effective permission sets (below), the roles are those sets instead, SUPER does not count (`IsSuperForAllCompanies` is false while `EffectivePermissionsInUse`), and every Tenant Permission Set is added (`GetRolePermissions` adds `TuplesOfRoleIdAndAppId` in test mode).

### Licensing and entitlements on-prem

bc-rust is an on-prem service tier without a license file. The license grants every object with full permissions (License Permission is virtual and says Yes everywhere), and membership entitlements are off (`EnableMembershipEntitlement` is an online setting). Reason: on-prem licenses limit object ranges and modules for Microsoft's pricing. They don't change what AL code does, and a customer running bc-rust has no Microsoft license file to read. So the effective permission is the union of the roles, and permission sets are the only access control. If entitlements are ever needed (a SaaS-like deployment), they belong in the same mask as one more ∩.

## Direct and indirect permissions

A table operation is allowed (`PermissionSetBase.InternalHasPermissionsDoNotCheckIncludedExcludedPermissions`, `VerifyIndirectPermissions`, `PermissionSet.VerifyInherentPermissions`) when one of these holds:
1. the roles grant it directly;
2. the roles grant it at least indirectly, and the object whose method runs (`Session.CurrentMethodScope.ApplicationObject`) has it in its `Permissions` property. An extension object asks its base object (`NavApplicationObjectBase.GetIndirectPermissionMask`), so a tableextension's or pageextension's `Permissions` count for the base object; bc-rust merges them by base object when the model is built;
3. inherent permissions grant it: the table's `InherentPermissions` property, or an `[InherentPermissions(PermissionObjectType::TableData, Database::X, 'rm')]` attribute on the running method. Upper case grants directly; lower case grants when the object's `Permissions` has the operation too (VerifyInherentPermissions → VerifyIndirectPermissions). The Business Foundation No. Series code depends on this: `GetNextNoInternal` modifies No. Series Line through `'m'`. bc-rust tracks the running procedure in `Session::proc_stack`, next to `call_stack`;
4. for Read, and for ModifyAll/DeleteAll, the table is empty (`checkForEmptyTable`).

Only the running method's object counts. BC does not walk the call stack: a codeunit with `Permissions = tabledata "G/L Entry" = i` lets its own code insert G/L entries, not the code it calls.

Not checked: temporary records, virtual tables (unless the platform marks them `VirtualWithPermissionChecks`), query-backed tables, and Read on Object (2000000001), Access Control (2000000053) and Object Metadata (2000000071) (`RecordImplementation.IsPermissionCheckNeeded`). The virtual tables come from Types `SystemTables`: 66 virtual tables without checks (Integer, Date, Field, AllObj, the metadata and permission virtual tables, ...), plus Permission Set and Permission (virtual with UsePermissionSetsFromExtensions) and Active Session (TrackSessionsInMemory). 13 virtual tables keep their checks (2000000247, 2000000260-264, 2000000274-278, 2000000282, 2000000299; an inline data blob in Types.dll). Every other system table (User, Company, Tenant Permission Set, ...) is checked like an application table.

### When reads are checked

BC checks a read after the query, not before (RecordImplementation):
- Get: only when the row was found. A Get that finds nothing never raises a permission error (Cost Account Mgt does `CostType.Get(GLAcc."No.")` for every new G/L account).
- IsEmpty: only when there are rows.
- Find, FindFirst, FindLast, FindSet: when a row was found, or, when nothing was found, if filters or marks are set (then the empty-table rule decides).
- Count: when the count is above 0 or filters are set.

bc-rust keeps the allowed path free: when the roles allow the read, nothing else happens. When they don't, `check_record_method` runs the read unchecked and applies these rules to its result.

## Enforcement in bc-rust

`Session::check_record_method` runs at the top of `record_method` (recops.rs), which both engines call for every record method AL code calls. Platform code that reads or writes rows itself is not checked unless it asks: the API pages' data provider should call `Session::check_table_op(page code, table, op)` the way BC's page data provider checks the source table (see the demo below). Company init, media and links are system operations, as in BC.

| Record method | Needs | Empty table excuses |
|---|---|---|
| Get, Find, FindFirst, FindLast, FindSet, IsEmpty, Count, CountApprox, CalcSums | Read | yes |
| Insert | Insert | no |
| Modify, Rename | Modify | no |
| ModifyAll | Modify | yes |
| Delete | Delete | no |
| DeleteAll | Delete | yes |

ReadPermission and WritePermission (Insert, Modify and Delete) answer from the same mask.

Writes are checked where BC checks them (RecordImplementation): Insert, Modify and Delete at the database operation, after the OnInsert/OnModify/OnDelete trigger and the OnBefore events, so an error those raise comes first (`Session::check_pending_write`, called from `rec_insert_x`/`rec_modify_x`/`rec_delete_x`). ModifyAll and DeleteAll are checked up front, with the empty-table rule, only on BC's bulk path (NavRecord.CanUseBulkModifyAll/CanUseBulkDeleteAll): no RunTrigger with the trigger defined, no OnBefore/OnAfter Modify/Delete event subscribers, and no global database trigger for the table (Global Triggers.GetDatabaseTableTriggerSetup, asked with checks off). Otherwise BC loops record by record and checks each record it touches, so a filter that matches nothing checks nothing. In Microsoft's tests the test libraries' Backup Management (codeunit 130011) sets the database triggers for every table 1..99999 of a company, so ModifyAll/DeleteAll on application tables never take the bulk path there.

Cost: when enforcement is off, or no effective sets are in use and the user is SUPER, a check is one bool test (`sys.perm.active`). When it is on, the allowed operations for (calling code object, table) are computed once per session and kept in a hash map, which `PermissionTestHelper.Clear/AddEffectivePermissionSet` and `login_permissions` empty. There is no per-row work; the empty-table probe reads one row and only runs after a denial.

The error is BC's `NavPermissionException`: "Sorry, the current permissions prevented the action. (TableData 18 Customer Read: <app>)" with error code `Permission`, so `GetLastErrorCode` and `Assert.ExpectedErrorCode('TestWrapped:Permission')` see the BC code. The app is the app of the calling object.

Not yet enforced:
- Execute permission on objects (BC checks Execute when an object runs and effective sets are in use, `NavSession` around line 3113). The runner always assigns 'All Objects' (X on every table, report, codeunit, page, XMLport and query), so this only matters after `SetExactPermissionSet`; tables, codeunits and pages also often have `InherentPermissions = X`. Plan: one check per (code object) per session in `Session::invoke` when `active`, cached like table data.
- FlowField source tables (`VerifyPermissionsCalculatedFields`), security filters, security groups.

## Production users

`Session::login_permissions()` (call it after the session's user and company are set) reads the session user's Access Control rows: User."User Name" = `user_id` gives the security id; rows with Company Name '' or the session company count. A System-scope SUPER row means no checks. Otherwise the user's permission is the union of their System sets (from the model) and Tenant sets (Tenant Permission + Tenant Permission Set Rel., with System sets they include), and checks run when `BC_PERMISSIONS=enforce`. As in BC, the permission system is off while the User table is empty. A user name that isn't in the User table gets no roles (BC refuses such a login). Gaps: security groups (Entra/Windows groups) aren't read. (`UserSecurityId()` is per session since I14: docs/authentication.md, "Session identity".)

Demo (2026-11-08, scratch `bc serve --memory` on 127.0.0.1, `BC_PERMISSIONS=enforce`, API v2.0 with Basic auth, demo-only hook in odata.rs that sets `user_id`, calls `login_permissions` and `check_table_op` with the API page as calling object). Users: ADMIN (SUPER), CLERK (D365 CUSTOMER, EDIT), CLERK2 (D365 BASIC + D365 CUSTOMER, EDIT), READER (D365 READ):

| User | Request | Result |
|---|---|---|
| ADMIN | GET vendors | 200 |
| CLERK | GET customers | 200 |
| CLERK | POST customers | 400: Customer.OnInsert (AL) reads Sales & Receivables Setup, which the role lacks: "(TableData 311 Sales & Receivables Setup Read: Base Application)" |
| CLERK | GET vendors | 403 "(TableData 23 Vendor Read: _Exclude_APIV2_)" |
| CLERK2 | POST customers | 201 |
| CLERK2 | GET vendors | 200 (D365 BASIC reads) |
| CLERK2 | POST vendors | 403 Vendor Insert |
| READER | GET vendors | 200 |
| READER | POST customers | 403 Customer Insert |
| NOBODY | GET customers | 403 (not in the User table) |

The CLERK case shows both layers: the API layer let the Insert through, and the AL trigger's own reads were checked by `record_method`.

## SUPER

SUPER is an ordinary permission set in the System Application (`permissionset 31 SUPER`, plus `SUPER (DATA)` 7368), but the platform treats a user with SUPER for all companies as having all permissions without looking at sets (`NavUserPermissions.IsSuperForAllCompanies`, `PermissionManager.HasAllPermissions`). bc-rust's platform init gives the session user SUPER in Access Control. While a test has effective permission sets, SUPER does not apply.

## Security filters

A permission can carry a "Security Filter" (a TableFilter on the table). With a record's `SecurityFiltering` = Filtered the filter is added silently; Validated raises an error on a filtered-out row; Ignored skips it; Disallowed refuses a record with a security filter. `Record.SetPermissionFilter` copies the user's filters into the record. Not implemented: no metadata permission set in the Base App or System Application uses one, so they only come from tenant data. The plan is to add them to the record's filters in group 6 or 7 when the record is opened, PostgreSQL-side like any other filter.

## TestPermissions and the test runner

`TestPermissions` on a test codeunit or method is Restrictive, NonRestrictive, Disabled or InheritFromTestCodeunit. The AL compiler resolves it into the method's `[NavTest]` attribute (Microsoft.Dynamics.Nav.CodeAnalysis `TestAttributeEmitter.AddTestPermissions`): the method's `[TestPermissions]` attribute unless its ordinal is 0 (InheritFromTestCodeunit), else the codeunit's `TestPermissions` property, whose default is **Restrictive** (ObjectParser's property table). The runtime passes that value to the runner's OnBeforeTestRun; a method without a NavTest attribute also counts as Restrictive (`NavTestExecution.LeaveTestMethod`). The AL side doesn't decide anything else: 130453 only asks `= Disabled`, and 130302 / Permission Test Catalog treat Restrictive and NonRestrictive alike (D365 Full Access plus defaults); only the old internal runners' `GetPermissionErrors` tells them apart. The W1 test apps have 1,419 test codeunits: 1,040 Disabled, 308 NonRestrictive, 11 Restrictive and 60 without the property.

Microsoft's AL test runner (130450 with 130453 "ALTestRunner Reset Environment" subscribed to OnBeforeTestMethodRun) does this before every test method:
1. Codeunit.Run(130301 "Reset State Before Test Run"): `Library - Random.SetSeed(1)`; `RunLegacyPermissionSet` = Permission Test Catalog.InitializePermissionSetForTest(Disabled) = Library - Lower Permissions.StartLoggingNAVPermissions() = Permissions Mock.Start(), which constructs PermissionTestHelper, which clears the effective sets; `ClearTemporaryNotificationContext`.
2. Unless the method is Disabled, Codeunit.Run(130302 "Set State Test Run"): InitializePermissionSetForTest(Restrictive) = StartLoggingNAVPermissions('D365 Full Access') = Start, then PushPermissionSetInternal('D365 Full Access', defaults): Permissions Mock.Set (Clear, Assign 'All Objects', Assign 'D365 Full Access'), Assign 'TEST TABLES', Assign 'D365 BASIC' and 'LOCAL' if it exists.

The PermissionTestHelper constructor calls `Clear()` (the add-in's source), so every `PermissionsMock.Start()` starts a method with no effective sets. bc-rust clears them in `dotnet_new`; without that, a Disabled method inherited the previous method's lowered sets under codeunit isolation, and its `Initialize` failed in Library - Setup Storage.Restore.

`PermissionsMock.Assign` does `PermissionSet.Get(RoleID)` on the legacy table and then `PermissionTestHelper.AddEffectivePermissionSet(RoleID)`, which adds the role id to `NavTestExecution.EffectivePermissionSets`. Tests then lower permissions with `LibraryLowerPermissions.SetO365Basic`, `SetCustomerView`, `PushPermissionSet` and so on, which go through the same Set/Assign calls. Clear and Dispose empty the set. The runners' after-test "You must change permissions at least once" check (`GetPermissionErrors`) is only called by the old internal runners (TestRunner-Internal), not by 130450, so al-test doesn't emulate it.

al-test mapping:
- `runner_reset` (default) does step 1 without `RunLegacyPermissionSet`.
- `--runner-permissions` adds `RunLegacyPermissionSet` and step 2 (`runner_permissions`, with `permissions::test_permissions` deciding Disabled). The DotNet PermissionTestHelper is emulated in dotnet/nav.rs on `SysState::perm`.
- `--enforce-permissions` implies `--runner-permissions` and turns the checks on.

History: a first try of the whole 130301 (round 5, 2026-11-07) failed 682 tests on w1demo and 819 on Contoso with "The Permission Set does not exist", because the legacy Permission Set table was empty. platform.rs now fills it, so the step runs cleanly.

## Measurements

Slices: the test codeunits whose results enforcement can change, meaning every codeunit that is not Disabled plus Disabled ones that lower permissions (W1 402 codeunits / 6,556 enabled tests on w1demo; Contoso 486 / 7,670 on contoso). Same binary for each pair, `--jobs 8`, deterministic runs, 2026-11-08.

| Pair | W1 slice | Contoso slice |
|---|---|---|
| runner step on vs off (`--runner-permissions`) | +0 / −0 | +0 / −1 (Email Retry Test wall-clock flake) |
| enforce vs off, first build (wildcard bug, no method attribute, helper not cleared) | +3 / −498 | |
| enforce vs off, + constructor clear + method `[InherentPermissions]` | +4 / −289 | +7 / −234 |
| enforce vs off, + reads checked after the query + BC's system-table split | (running) | (running) |

The losses so far are false denials, not BC behavior. In order of size they were the helper not clearing (setup tables denied in Initialize), No. Series Line (method attribute), and reads BC checks only after the query (Cost Type Get, Analysis View IsEmpty, warehouse lines). Each was fixed [src:TODO], and the gains are tests that expect a permission error.

## Next slices

1. Execute checks on objects; FlowField source tables.
2. Production: wire `login_permissions` and `check_table_op` into the real API and web-client sessions (with the owners of odata.rs and bc-nst), `UserSecurityId()` per user, security groups; then decide the `bc serve` default.
3. Security filters.
4. Raise the global database triggers themselves (OnDatabaseInsert/Modify/Delete/Rename, docs/platform-tables.md); today only the bulk-path decision asks GetDatabaseTableTriggerSetup.
