# NST protocol: metadata and record data

This document covers what bc-rust must send so that Microsoft's BC web server builds pages from it. It describes the metadata the web server requests and how record rows are encoded. Connection setup, authentication and the JSON-RPC call sequence are in `docs/nst-protocol-transport.md`. Where this file mentions a JSON-RPC method, that file has the envelope.

Source: an analysis of the BC 29.1.57326.57497 web server made before 2026-11-10 with decompiled binaries, which bc-rust no longer uses (README, BC sources and licence). The facts below are being re-sourced from captured traffic between Microsoft's web client and a licensed BC [src:TODO]. Nothing has been checked against live traffic yet. Section 5 lists what needs a capture.

## 1. How the web server gets metadata

### 1.1 JSON rules that apply to every metadata object

Metadata travels as plain JSON over HTTP. The web server deserializes it with Newtonsoft.Json using `SharedJsonSettings.ClientServer`. These settings matter:

- Property names come from `[DataMember(Name=...)]`. Page metadata uses short generated names such as `MP_3` or `CD2_17`. Table metadata (`MetaTable`, `MetaField`, `MetaKey`) uses the plain C# property names.
- `NullValueHandling.Ignore`. Nearly every member also has `EmitDefaultValue=false`, so the NST leaves out any value equal to its default. A `[DefaultValue]` attribute changes the default. `SOD_4 ModifyAllowed` defaults to `true`, so the NST omits it when true and sends `false` explicitly. Section 2 lists the non-obvious defaults.
- Enums go out as integers. No string enum converter is registered for metadata.
- Polymorphic objects need a wrapper. `NavPolymorphicJsonConvert` claims every type that takes part in a `[NavJsonDerivedType]` hierarchy. In `Microsoft.Dynamics.Nav.Types.Metadata` that means every class derived from `ElementDefinition`: `MasterPage`, `PageProperties`, `SourceObjectDefinition`, `ViewDefinition`, `FilterDefinition`, every control, action and trigger. Each one is written as

  ```json
  { "$polyType": "Microsoft.Dynamics.Nav.Types.Metadata.ControlDefinition", "obj": { "IDED_1": 3, "NED_1": "No.", "CDD_11": "1" } }
  ```

  The wrapper is required even when the declared type equals the real type, and that includes the top-level `MasterPage` in a response. On read, the converter accepts `$polyType` only if it equals the declared type's full name or appears in that base type's derived-type list. Anything else throws. `MetaTable`, `MetaField`, `MetaKey`, `PageDependencyResult` and `PageResponseDependencyEntry` are not in any hierarchy and are written without a wrapper.
- `EDSpec` (uint) appears on every `ElementDefinition`. It is a bitmask of "explicitly specified" flags. The bit numbers run on down the class chain, so `ControlDefinition` uses bits 0 to 26. Every property setter that has a flag sets it when the property arrives. EDSpec therefore only matters for an explicit value that equals the default, for example `ShowFilter = false` or `PageType = Card (0)`. The private EDSpec setter replaces the whole mask. Put `EDSpec` first inside `obj`, so the setters that run after it add their bits to it. Section 2 has the bit tables.
- Captions are single strings already resolved to the session language (`NED_3 Caption`). `CaptionML` is not on the wire.

### 1.2 Endpoints

`MetadataClient` sends every request to `<service base URL>client/metadata/`. The method is GET unless the table says otherwise. Every response body is JSON of the listed type. The web server also opens a separate `client/data/` client (section 3.1).

| Path | Returns | Notes |
|---|---|---|
| `pagewithdependencies/{pageId}` POST | `PageDependencyResult` | Main call when a page opens. Body is a JSON array of table IDs the web server already has cached. Leave those tables out of `MetaTables`. Query: `dataSourceType=Query` (only for query pages), `personalizationId`, `applyPersonalization=false`, `viewName`. |
| `pages/{pageId}` | `MasterPage` wrapped | Same query options plus `withConfiguration=false`. Used on a cache miss and for `GetMasterPageWithoutConfiguration`. |
| `tables/{tableId}` | `MetaTable` | Cache miss for a single table. |
| `pages/table/{tableId}` | `MasterPage` | Generic table page. Cached under page id 9599. Optional `personalizationId`. |
| `pages/handle/{formHandle GUID}` | `MasterPage` | Filter pages, report and XMLport request pages, keyed by a server form handle. |
| `pages/template/{PageType name}` | `MasterPage` | Path segment is the enum name, e.g. `Card`. |
| `systemparts/{SystemPartTypes name}` | `MasterPage` | Notes, Links and other system parts. |
| `homepage`, `navigation`, `productoverview` | `MasterPage`, `MasterNavigation` | Role center and navigation shell. Needed for the start page but not covered here. |
| `navigation/action` POST | `ActionDefinition` | Body is a source `Reference`. |
| `queries/{id}`, `querytables/{id}` | `MetaQuery`, `MetaTable` | Query-backed pages only. |
| `configuration`, `pagedatapersonalization` POST | none | Personalization saves. Accept them and return 200. |
| `analysis/...`, `controlAddInResource`, `similarobjects` | various | Not needed for the three target pages. |

`PageDependencyResult` has no data contract, so its JSON uses C# names:

```json
{ "MasterPages": [ { "PersonalizationId": null, "MasterPage": { "$polyType": "...MasterPage", "obj": {} } } ],
  "MetaTables":  [ { "Id": 18, "Name": "Customer" } ] }
```

The web server looks up the entry whose `MasterPage.ID` equals the requested page id. If none matches, it hits a null reference. Include every page the requested page will open right away, meaning subpage parts and factbox pages. For page 42 that means 42, 46 "Sales Order Subform" and the factboxes. Include the `MetaTable` of every source table that is not in the skip list.

A non-2xx status raises `HttpRequestException` with the body text in the message. A 404 for a missing page or table should be enough. Its exact handling is unconfirmed.

### 1.3 Caching and the metadata token

- The NST sends a `MetadataToken` response header as a decimal int64 on HTTP calls, or a top-level `MetadataToken` property on JSON-RPC messages. The web server keeps the first token it sees. When a later token differs, it clears every metadata cache for that session.
- A cache entry hits only when its stored version equals the current token. The stored version for pages and tables is the object's own `VersionNumber` (`MP_15` on `MasterPage`, `VersionNumber` on `MetaTable`). Set both to the current token. A mismatch costs a server call on every lookup. Nothing breaks, but every page open becomes several round trips.
- Page cache key: `"{pageId}:{personalizationId, or pageId when empty}:{dataSourceType as int}"`, plus `":{viewName}"` when a view is set. Role center pages are cached without a personalization id. Tables, templates and system parts are keyed by id or enum. Navigation is keyed by the `applyPersonalization` bool.
- bc-rust should derive the token from its catalog generation and bump it after every reseed or AL reload. That is how the web server learns its caches are stale.

## 2. Page metadata

### 2.1 Base chain shared by pages, controls and actions

| Class (DC name) | Wire | Member | Type | Notes |
|---|---|---|---|---|
| ElementDefinition (ED) | `EDSpec` | SpecifiedProperties | uint | see 1.1 |
| IDElementDefinition (IDED) | `IDED_1` | ID | int | page id, or a numeric control/action id unique within the page. Bit 0. |
| | `IDED_2` | ClonedFromID | int | bit 1 |
| NamedElementDefinition (NED) | `NED_1` | Name | string | AL name of page, control or action |
| | `NED_2` | ALNamespace | string | |
| | `NED_3` | Caption | string | resolved caption |
| | `NED_4` | CaptionMaxLength | int | |
| UIElementDefinition (UIED) | `UIED_1` | VisibleMode | enum | Always 0, Edit 1, View 2 |
| | `UIED_2` | Visible | string | default `"true"` |
| | `UIED_5` | ControlGUID | Guid | the web server identifies controls by this. Must be stable per element. |
| | `UIED_6` | Enabled | string | default `"true"` |
| | `UIED_7` | SourceAppId | Guid | app that defines the element |

`Visible`, `Enabled` and `Editable` are strings. `TRUE`, `FALSE`, `YES`, `NO`, `1` and `0`, matched case-insensitively, are literals. Any other string is a dynamic expression. The client evaluates it against the current row, through an expression column of that name (see `MP_10` below).

### 2.2 MasterPage (`MP`)

Inherits the chain above. `IDED_1` is the page id, `NED_1` the AL name, `NED_3` the caption.

| Wire | Member | Type |
|---|---|---|
| `MP_1` | CommandBar | CommandBarDefinition (actions) |
| `MP_2` | PromptActions | ActionContainerDefinition |
| `MP_3` | ContentArea | ContentAreaDefinition (`area(Content)`) |
| `MP_4`, `MP_5`, `MP_6` | PromptArea, PromptOptionsArea, UserControlHostNavigationArea | ContentAreaDefinition |
| `MP_7` | InfopartsArea | InfopartsAreaDefinition (`area(FactBoxes)`) |
| `MP_8` | PageProperties | PageProperties |
| `MP_9` | NavigationArea | NavigationAreaDefinition |
| `MP_10` | Expressions | list of DataFieldDefinition |
| `MP_11` | Triggers | list of PageTriggerDefinition |
| `MP_12`, `MP_13` | Methods, GlobalVariables | lists |
| `MP_14` | RemovedControls | list of string |
| `MP_15` | VersionNumber | long. Set to the metadata token. |
| `MP_16`, `MP_17` | ShowFilterPane, ShowInfoPartPane | bool |
| `MP_18` | DefaultFilterColumnName | string |

`PageProperties` (`PP`). Defaults are in brackets.

| Wire | Member | Type | Notes |
|---|---|---|---|
| `PP_1` | SourceObject | SourceObjectDefinition | |
| `PP_7` | PageType | enum | Card 0, List 1, RoleCenter 2, CardPart 3, ListPart 4, Document 5, Worksheet 6, ListPlus 7, ConfirmationDialog 8, NavigatePage 9, StandardDialog 10, API 11. Bit 0. |
| `PP_8` | Extensible | bool [true] | |
| `PP_9` | ActionsV2 | bool | modern action syntax (actionref/`area(Promoted)`) |
| `PP_16` | HasOnOpenPageEvent | bool | bit 1 |
| `PP_17` | OnAfterGetCurrentRecordEnabled | bool | client then calls back after each current-row change. Bit 2. |
| `PP_18` | Editable | bool [true] | |
| `PP_20` | DataAccessIntent | enum | bit 3 |
| `PP_21` | CardFormID | int | `CardPageId` as a number. Bit 4. |
| `PP_22` | DataCaptionExpr | string | |
| `PP_23` | RefreshOnActivate | bool | |
| `PP_33`, `PP_34` | HelpRelativeUrl, HelpLink | string | |
| `PP_48` | ApplicationArea | string | |
| `PP_49` | UsageCategory | enum | bit 8 |
| `PP_2`, `PP_15` | SourceAppId, IsSystemPage | Guid, bool | |

`SourceObjectDefinition` (`SOD`): `SOD_1` SourceTableView (ViewDefinition), `SOD_2` SourceTable int (bit 0), `SOD_3` InsertAllowed [true], `SOD_4` ModifyAllowed [true], `SOD_5` LinksAllowed [true], `SOD_6` DeleteAllowed [true], `SOD_7` ShowFilter (bit 1), `SOD_8` DelayedInsert, `SOD_9` MultipleNewLines (bit 2), `SOD_10` SaveValues (bit 3), `SOD_11` AutoSplitKey (bit 4), `SOD_12` SourceTableTemporary, `SOD_14` DataCaptionFields string, `SOD_16` PageDataSource (Table 0, Query 1).

`ViewDefinition` (`VD`): `VD_1` Sorting, which is a `ViewDefinitionSorting` (`VDS_1` KeyFields string, `VDS_2` Ascending [true]), and `VD_2` TableFilters, a list of `FilterDefinition`.

`FilterDefinition` (`FD`): `FD_1` FilterGroup int, `FD_2` FieldID int, `FD_3` FilterType (CONST 0, FILTER 1, FIELD 2), `FD_4` FilterValue string, `FD_5` ValueIsFilter bool. The same type carries `SubPageLink`, `RunPageLink` and `SourceTableView` filters.

### 2.3 Control tree

`ContentAreaDefinition` (`CAD`) and `InfopartsAreaDefinition` (`IAD`) both hold `ABD0_1` Controls, a list of `ControlBaseDefinition`. Each entry is wrapped with its concrete type.

`ControlBaseDefinition` (`CBD`) is common to all controls and actions: `CBD_1` Actions (list, field-level or part actions), `CBD_2` ActionContainers, `CBD_3` CaptionColumnName, `CBD_4` CaptionClass, `CBD_5` DrillDown, `CBD_6` Lookup, `CBD_7` AssistEdit, `CBD_8` DropDown, `CBD_9` ClosingDates, `CBD_10` Numeric, `CBD_11` DateFormula, `CBD_13` HelpLink, `CBD_15`/`CBD_17` AboutText/AboutTitle. The bools DrillDown through Numeric own EDSpec bits 2 to 7.

The web server picks a builder by concrete type:

| AL element | Wire type (DC) | Own members |
|---|---|---|
| `group` | `ControlGroupDefinition` (CGD) | none. Inherits `CGBD_1` Controls, `CGBD_2` InstructionalText, `CGBD_4` Editable string ["true"], `CGBD_5` ShowCaption [true]. A top-level group renders as a FastTab, a nested one as a sub-group. |
| `repeater` | `RepeaterDefinition` (RD) | `RD_1` ShowAsTree (bit 9), `RD_2` TreeInitialState, `RD_3` FreezeColumnID (bit 10), `RD_4` IndentationColumnName, `RD_5` IndentationControls, plus `CGBD_1` |
| `grid` | `GridLayoutDefinition` (GLD) | `GLD_1` Layout |
| `fixed` | probably `ColumnLayoutDefinition` (CLD) | none, unconfirmed |
| `field` | `ControlDefinition` (CD2) | see below |
| `part` | `InfopartPageDefinition` (IPD) | see below. The same type serves subpages in `MP_3` and factboxes in `MP_7`. |
| `systempart` | `InfopartSystemDefinition` | system part type |

Field controls inherit from `ControlDataboundDefinition` (`CDD`):

| Wire | Member | Type | Notes |
|---|---|---|---|
| `CDD_1` | TableRelations | list of TableRelationDefinition | drives lookups |
| `CDD_2` | RelatedObjects | list of RelatedObjectDefinition | permission checks |
| `CDD_3` | Triggers | list of ControlTriggerDefinition | which field triggers exist |
| `CDD_4`, `CDD_5` | Style, StyleExpr | enum, string | bit 9 |
| `CDD_6` | ExtendedDatatype | enum | None 1, PhoneNo 2, URL 3, EMail 4, Ratio 5, Masked 6. Bit 10. |
| `CDD_8` | Width | int | bit 12 |
| `CDD_9` | ShowMandatory | string | expression |
| `CDD_11` | DataColumnName | string | the data binding. Section 3.3. |
| `CDD_12` | Editable | string | expression |
| `CDD_13` | TableEditable | bool | from the field's `Editable`. Bit 13. |
| `CDD_15` | SourceExpressionIsAssignable | bool [true] | |
| `CDD_16`, `CDD_17` | MinValue, MaxValue | string | |
| `CDD_18` | Image | string | cues |

`ControlDefinition` adds: `CD2_2` FormatColumnName, `CD2_3` CurrencyCodeColumnName, `CD2_4` DecimalPlaces string such as `"0:5"`, `CD2_6` OptionCaption, `CD2_8` ToolTip, `CD2_11` NotBlank (bit 17), `CD2_12` CharAllowed, `CD2_13` ValuesAllowed, `CD2_14` BlankNumbers (bit 18), `CD2_15` BlankZero (bit 19), `CD2_16` MultiLine, `CD2_17` Importance (Standard 0, Promoted 1, Additional 2; bit 20), `CD2_18` DrillDownFormID (bit 21), `CD2_19` LookupFormID (bit 22), `CD2_20` NavigationPageId, `CD2_21` HideValue ["false"], `CD2_22` ControlAddIn, `CD2_26` ApplicationArea, `CD2_27` QuickEntry ["true"], `CD2_28` ShowCaption [true], `CD2_29`/`CD2_30` RowSpan/ColumnSpan [1].

`ControlTriggerDefinition` (`CTD`): `CTD_1` TriggerType (OnValidate 0, OnLookup 1, OnDrillDown 2, OnAssistEdit 3, OnControlAddIn 4, OnAfterLookup 5) and `CTD_2` Name.

`TableRelationDefinition` (`TRD`): `TRD_1` Conditions, a list of `ConditionDefinition` (`CD0_1` FieldID, `CD0_2` ConditionType, `CD0_3` ConditionValue). Then `TRD_2` Filters (list of FD), `TRD_3` TableID, `TRD_4` TableName, `TRD_5` FieldID. A conditional relation (`if (Type = const(Item)) Item`) becomes one entry per branch.

`InfopartPageDefinition` (`IPD`): `IPD_1` SubFormLink (list of FD, the `SubPageLink`), `IPD_2` SubFormView, `IPD_3` Editable ["true"], `IPD_4` ShowFilter ["true"], `IPD_5` ProviderID (bit 9, the `Provider` control id for factboxes), `IPD_6` PagePartID (bit 10, the part's page id), `IPD_10` ApplicationArea, `IPD_11` RunObjectSrcTable, `IPD_12` UpdatePropagation (SubPart 0, Both 1; bit 15), `IPD_17` Multiplicity [Many].

`DataFieldDefinition` (`DFD`), the entries of `MP_10`, declares every value column that is not a plain table field. That covers page variables, procedure calls and dynamic Visible/Editable/Style sources. Members: `DFD_1` Name (the column name), `DFD_2` Datatype (`DataType` enum: Integer 0, Decimal 5, Option 6, Boolean 7, Date 9, Time 10, GUID 17, DateTime 19, Text 21, Code 23), `DFD_3` DataLength, `DFD_4` OptionString, `DFD_5` EnumId, `DFD_6` IsDataSetVar, `DFD_7` ExpressionIsAssignable, `DFD_8` SourceExpression, `DFD_9` InRepeaterGroup, `DFD_10`/`DFD_11` TableNo/FieldNo, `DFD_12` ExpressionType, `DFD_14` OptionCaption, `DFD_15` OptionValues. Note that `Datatype` uses the small `DataType` numbering, not `NavType`.

`PageTriggerDefinition` (`PTD`): `PTD_1` TriggerType (OnInit 0, OnOpenPage 1, OnClosePage 2, OnFindRecord 3, OnNextRecord 4, OnAfterGetRecord 5, OnNewRecord 6, OnInsertRecord 7, OnModifyRecord 8, OnDeleteRecord 9, OnQueryClosePage 10, OnAfterGetCurrRecord 11) and `PTD_2` Name.

The NST resolves defaults before sending. The page-level `PropertiesSolveHelper.SolvePropertiesDefaulting(MasterPage, MetaTable, ...)` runs only on the server. The web server calls it for navigation metadata and nothing else. Field-derived control properties therefore arrive already filled in, and bc-rust has to fill them the same way. A property counts as unset when its EDSpec bit is clear. The solver handles Lookup (true when the field has a table relation or an OnLookup trigger), DrillDown (FlowFields and drill-down pages), AssistEdit, DropDown, ClosingDates, DateFormula, BlankNumbers, BlankZero, NotBlank, ExtendedDatatype, MaskType, Min/MaxValue (with type-specific rules for Date, Time, DateTime, Decimal and BigInteger), CaptionClass, DecimalPlaces, CurrencyCodeColumnName and RelatedObjects for `AccessByPermission`.

### 2.4 Actions

`CommandBarDefinition` (`CBD0`) holds `CBD0_1` Actions, a list of `ActionBaseDefinition`, and `CBD0_2` ShowQuickAccessPane. Every action type inherits the UIED/CBD chain, so actions have `NED_3` Caption, `UIED_2` Visible and `UIED_6` Enabled. Groups nest their children in `CBD_1`.

| AL element | Wire type | Members |
|---|---|---|
| all actions | `ActionBaseDefinition` (ABD) | `ABD_1` Image, `ABD_2` IsIconBig (bit 9) |
| | `ActionCommonPropsDefinition` (ACPD) | `ACPD_1` Ellipsis, `ACPD_2` ShortCutKey, `ACPD_4` ToolTip, `ACPD_13` ApplicationArea, `ACPD_14` Scope (bit 13), `ACPD_17` InFooterBar (bit 14), `ACPD_18` ShowCaption [true], `ACPD_10..12` AccessByObject* |
| `action` | `ActionDefinition` (AD) | `AD_1` RunFormLink (LinkDefinition, `LD_1` TableFilters), `AD_2` RunFormView, `AD_3` Trigger (`ATD_1` OnAction 0, `ATD_2` Name; present when OnAction has code), `AD_5` RunPageOnRec, `AD_6` TargetID (bit 16, RunObject id), `AD_7` SystemActionType, `AD_8` RunObjectType (None 0, Page 1, Report 2, Codeunit 3, XMLport 4, Query 5; bit 18), `AD_14` RunPageMode (View 0, Edit 1, Create 2; bit 19), `AD_15` RunObjectSrcTable, `AD_18` MenuActionType |
| `group` | `ActionGroupDefinition` (AGD) | `AGBD_1` DeleteIfEmpty, `AGD_2` ToolTip, `AGD_4` ShowAs (Standard 0, SplitButton 1), `ProfileKey` |
| `actionref` | `ActionRefDefinition` (ARD) | `ARD_1` TargetName, `ARD_2` TargetID (the `IDED_1` of the target action), `ARD_3` ApplicationArea |
| `separator` | `SeparatorDefinition` (SD) | `SD_1` IsHeader, `SD_2` IsHeadingDynamic |
| system area | `ActionContainerPlaceHolder` (ACPH) | `ACPH_1` ContainerType, `ProfileKey` |

AL action areas map to `ActionContainerType`: Creation is NewDocumentItems 0, Processing is ActionItems 1, Navigation is RelatedInformation 2, Reporting is Reports 3, Promoted is 10. The web client also recognizes some groups by fixed `ControlGUID`. New-document items use `1AF878F7-1EB4-4B40-A6D5-55E87F26029B`, the promoted action-ref container uses `9D7FA316-FE52-4A80-A0A2-1CEBC7325FDA` and part action items use `9BFB7851-C660-4082-9115-D127551F6A6A`. Static reading did not settle how the top level of `CBD0_1` encodes each area. It could be groups with these GUIDs, placeholders, or `ActionContainerDefinition` (`ACD0_1` Actions, `ACD0_3` ActionContainerType). This is open question 1.

#### Captured templates (not in the repository)

The real service tier adds its own system actions to every command bar: Control_Home with New, Manage, View and Page, Print & Send, Help, and the Edit, View, New and Find rows of list parts. Its Notes and Links FactBoxes come from `systemparts/Notes` and `systemparts/RecordLinks`. bc-nst copies both from a capture of a real BC 29.1 service tier. Captured traffic stays out of the repository, so you generate the files from your own container:

1. Record Microsoft's web client against your BC 29.1 container with tools/nst-proxy (see its README). Open the Customer List (22), the Customer Card (21), a Sales Order (42, with its lines part 46), the Business Manager role center (9022) and the Notes and Links FactBoxes.
2. Run `python3 tools/nst-templates/mktemplates.py <nst.jsonl>`. It writes `data/nst-templates/sysactions.json` and `data/nst-templates/systemparts/{notes,recordlinks}.json`. Set `BC_NST_TEMPLATES` to read them from another directory.
3. Run `touch crates/bc-nst/build.rs` and rebuild. `crates/bc-nst/build.rs` compiles the files in.

Without the files, the build prints a warning and bc-nst still works. Each page gets bc-nst's own containers (Control_Promoted, Control_Actions with Control_New_Document, Control_Related, Control_Reports) without the system actions, and Notes and Links each become a card part around the matching system control. The bc-nst tests that compare against the captured layout skip, and `golden_page_21` writes nothing.

### 2.5 The three target pages

- **22 Customers.** `PP_7 = 1`, `SOD_2 = 18`, `PP_18 = false` (page is not editable), `PP_21 = 21`. `MP_3` holds one `RepeaterDefinition` whose children are `ControlDefinition`s bound to `"1"`, `"2"` and so on. `MP_7` holds factboxes as `InfopartPageDefinition`, linked to the current row through `IPD_1`, and `InfopartSystemDefinition` for Links and Notes. The command bar carries actions with `AD_8 = 1` and page targets.
- **21 Customer Card.** `PP_7 = 0`. Card is the default, so set EDSpec bit 0 to mark it specified. `MP_3` holds one top-level `ControlGroupDefinition` per FastTab, such as General, Address & Contact and Invoicing, each with field controls and nested groups. `CD2_17` Importance drives "show more".
- **42 Sales Order.** `PP_7 = 5`, `SOD_2 = 36`. `MP_3` holds groups plus an `InfopartPageDefinition` named `SalesLines` with `IPD_6 = 46`, `IPD_12 = 1` (Both), and `IPD_1` = [Document No. = FIELD(No.)]. Page 46 comes back in the same `pagewithdependencies` result: `PP_7 = 4`, `SOD_2 = 37`, `SOD_8`/`SOD_9`/`SOD_11 = true`, a repeater with the line fields, plus `MetaTable`s 36 and 37.

### 2.6 Table metadata (`MetaTable`)

Plain JSON with C# names. Newtonsoft fills it through a constructor that matches names case-insensitively, so key order is free.

`MetaTable`: `Id`, `Name`, `Caption`, `CaptionMaxLength`, `VersionNumber` (the token), `Timestamp`, `LookupFormId`, `DrillDownFormId`, `NavigationPageId`, `DataCaptionFields` (int[]), `IsDataPerCompany`, `TableType` (Normal 0, Temporary 6), `PasteIsValid`, `SourceAppId`, `ObsoleteState`, `DataClassification`, `Access`, `InherentPermissions`, `Fields`, `Keys`, `FieldGroups`, `Translations` (optional).

`MetaField`, for each field:

| Member | Type | Notes |
|---|---|---|
| `Id`, `Name`, `Caption`, `CaptionMaxLength` | | |
| `Type` | `NavType` int | Code 31490, Text 31489, Integer 34560, BigInteger 36096, Decimal 12800, Boolean 34048, Date 11776, Time 11777, DateTime 37376, Duration 36864, DateFormula 11798, Option 35584, Enum 77569, GUID 37120, BLOB 33794, Media 26208, MediaSet 26209, RecordID 4989, TableFilter 4913 |
| `ClrType` | string | .NET type name resolved with `Type.GetType`: `System.String`, `System.Int32`, `System.Int64`, `System.Decimal`, `System.Boolean`, `System.DateTime`, `System.TimeSpan`, `System.Guid`, `System.Byte[]`. This picks the dataset column type. FlowFilter fields are always strings. |
| `Length` | int | Code/Text length |
| `DataColumnName` | string | defaults to the `Id` as a string |
| `FieldClass` | enum | Normal 0, FlowField 1, FlowFilter 2 |
| `Editable`, `Enabled` | bool [true] | |
| `OptionString`, `OptionCaption` | string | comma-separated names and captions |
| `OptionValues`, `OptionOrdinalValues` | int[] | ordinals. When both are empty, values default to 0..n-1. |
| `EnumTypeId`, `EnumTypeName`, `Extensible` | | for enum fields |
| `Relations` | list of `MetaFieldRelation` | `TableId`, `TableName`, `FieldId`, `Filters` (MetaFilter: `FieldId`, `FilterType`, `FilterValue`, `FilterGroup`, `ValueIsFilter`, `OnlyMaxLimit`), `Conditions` (MetaCondition: `FieldId`, `ConditionType`, `ConditionValue`) |
| `CalcFormula` | `MetaCalcFormula` | `TableId`, `FieldId`, `Filters`, `FlowFieldType` string, `ReverseSign` |
| `ValidateRelation` | bool [true] | |
| `NotBlank`, `BlankZero`, `BlankNumbers`, `DecimalPlaces`, `ClosingDates`, `MinValue`, `MaxValue`, `InitValue`, `CharAllowed`, `ValuesAllowed`, `ExtendedDatatype`, `MaskType`, `Width`, `CaptionClass`, `AutoIncrement`, `ToolTip`, `AccessByObjectType/ID/PermissionMask`, `ObsoleteState`, `DataClassification` | | as in AL |

`MetaKey`: `Name`, `KeyName`, `FieldRelations` (`[{ "Id": 1, "Name": "No." }]`), `SumIndexFields`, `IncludedFields`, `Enabled`/`MaintainSqlIndex`/`MaintainSiftIndex` [true], `Clustered`, `Unique`. The client offers enabled keys as sort orders.

`MetaFieldGroup`: `Name` (`DropDown`, `Brick`), `FieldIds` int[], `Caption`.

No separate enum endpoint exists. Option and enum metadata always ride on the field: names in `OptionString`, captions in `OptionCaption` (a control can override with `CD2_6`), ordinals in `OptionValues`. The row value is the int ordinal.

### 2.7 Skeleton for a tiny card page

I built this example for a hypothetical page 50100 "Demo Card" on table 50100 "Demo". It shows the nesting and wrappers. It is not a capture. `T.` stands for the namespace prefix `Microsoft.Dynamics.Nav.Types.Metadata.`, so write the full name on the wire.

```json
{ "$polyType": "T.MasterPage", "obj": {
  "IDED_1": 50100, "NED_1": "Demo Card", "NED_3": "Demo Card", "MP_15": 7,
  "MP_8": { "$polyType": "T.PageProperties", "obj": {
      "EDSpec": 1,
      "PP_1": { "$polyType": "T.SourceObjectDefinition", "obj": { "SOD_2": 50100 } } } },
  "MP_3": { "$polyType": "T.ContentAreaDefinition", "obj": { "ABD0_1": [
      { "$polyType": "T.ControlGroupDefinition", "obj": {
          "IDED_1": 2, "NED_1": "General", "NED_3": "General",
          "UIED_5": "6b1f0c1e-0000-5000-8000-000000000002",
          "CGBD_1": [
            { "$polyType": "T.ControlDefinition", "obj": {
                "IDED_1": 3, "NED_1": "No.", "NED_3": "No.",
                "UIED_5": "6b1f0c1e-0000-5000-8000-000000000003",
                "CDD_11": "1", "CDD_13": true, "CD2_8": "Specifies the number.",
                "CD2_17": 1, "CD2_26": "All" } },
            { "$polyType": "T.ControlDefinition", "obj": {
                "IDED_1": 4, "NED_1": "Status", "NED_3": "Status",
                "UIED_5": "6b1f0c1e-0000-5000-8000-000000000004",
                "CDD_11": "4", "CDD_13": true, "CD2_6": "Open,Released", "CD2_26": "All",
                "CBD_8": true } } ] } } ] } },
  "MP_1": { "$polyType": "T.CommandBarDefinition", "obj": { "CBD0_1": [
      { "$polyType": "T.ActionDefinition", "obj": {
          "IDED_1": 10, "NED_1": "OpenList", "NED_3": "Demo List",
          "UIED_5": "6b1f0c1e-0000-5000-8000-00000000000a",
          "AD_8": 1, "AD_6": 50101, "ACPD_13": "All", "ABD_1": "List" } } ] } }
} }
```

`EDSpec: 1` on PageProperties marks `PageType` as specified, because Card is 0 and `PP_7` is therefore omitted. `CBD_8` turns the Status field into a dropdown. The matching table:

```json
{ "Id": 50100, "Name": "Demo", "Caption": "Demo", "VersionNumber": 7, "IsDataPerCompany": true,
  "Fields": [
    { "Id": 1, "Name": "No.", "Caption": "No.", "Type": 31490, "ClrType": "System.String", "Length": 20, "NotBlank": true },
    { "Id": 2, "Name": "Name", "Caption": "Name", "Type": 31489, "ClrType": "System.String", "Length": 100 },
    { "Id": 3, "Name": "Blocked", "Caption": "Blocked", "Type": 34048, "ClrType": "System.Boolean" },
    { "Id": 4, "Name": "Status", "Caption": "Status", "Type": 35584, "ClrType": "System.Int32",
      "OptionString": "Open,Released", "OptionCaption": "Open,Released", "OptionValues": [0, 1],
      "EnumTypeId": 50100, "EnumTypeName": "Demo Status" } ],
  "Keys": [ { "Name": "PK", "FieldRelations": [ { "Id": 1, "Name": "No." } ], "Clustered": true, "Unique": true } ] }
```

## 3. Record data

### 3.1 Where rows come from

Page data goes over JSON-RPC, not the metadata REST client. The web server calls `IClientApi.GetPage(PageRequestDefinition, NavRecordState)` and expects a `DataSetResponse` back. Its members are `dataSet` (a NavDataSet, private field name), `State` (NavRecordState) and the `ServerResponse` base members `requests`, `VersionNumber` and `ClientContext`. Datasets also show up inside `NavRecordState.FormVariables` and `AutoKeyValues`.

A second path, `DataClient.GetPage`, POSTs a `GetPageRequest` (`PageRequestDefinition`, `State`, `GetRowsRequest`, `MaxRowCount`) to `client/data/page` and copies the response body to a stream. Only `StreamData` uses it, probably for exports. Its body format is unconfirmed (question 9).

### 3.2 NavDataSet on JSON-RPC

`NavDataSetConverter` writes a dataset as

```json
{ "compressed": false, "count": 1, "buffers": ["<base64 of the binary dataset>"] }
```

The reader is positional. It skips the property names and reads a bool, an int, then `count` base64 strings, so the three properties must come in exactly this order. The decoder concatenates the buffers. When `compressed` is true, the joined bytes are raw DEFLATE (RFC 1951, no zlib or gzip header). The NST compresses only above a configured size threshold. bc-rust can always send `compressed: false` with a single buffer.

### 3.3 Binary layout

All values follow .NET `BinaryWriter` rules: little-endian integers, bool as one byte, and strings as a 7-bit variable-length byte count followed by UTF-8 bytes.

```
DataSet:  name:string  lcid:i32  tableCount:i32
          tableCount x  { tableName:string  lcid:i32  columnCount:i32
                          columnCount x { columnName:string  typeCode:u8  isLazy:bool
                                          extPropCount:i32  extPropCount x (typedValue key, typedValue value) } }
          then for each table in order: rowCount:i32, then rowCount x columnCount cells
Cell:     marker:u8  [payload when marker = 2]
          marker 0 DBNull, 1 DBNotChanged, 2 value follows, 3 zero (compact form of 0 or 0m), 4 DBLazy
typedValue (ext props only): typeCode:u8 written as a Byte cell (marker 2 + byte), then a cell of that type
```

The client builds its own sets with LCID 127 (invariant) and `name = "dataSet"`. `extPropCount` is normally 0. Do not send marker 1 inside page data: the JSON-RPC reader has row deduplication off, so the client would keep a literal "not changed" value. Marker 3 is optional. The NST uses it only in compact mode.

`typeCode` values (`NavTypeCode`): Object 0, DBNull 1, Boolean 2, Char 3, SByte 4, Byte 5, Int16 6, UInt16 7, Int32 8, UInt32 9, Int64 10, UInt64 11, Single 12, Double 13, Decimal 14, DateTime 15, TimeSpan 16, String 17, Guid 18, ByteArray 19, Media 20, MediaSet 21, DBLazy 22, DBNotChanged 23.

Payloads: Boolean 1 byte. Int32 4 bytes, Int64 8. Decimal 16 bytes, as three int32 words (lo, mid, hi) of the 96-bit integer plus an int32 flags word holding the scale in bits 16 to 23 and the sign in bit 31. DateTime is int64 ticks (100 ns since 0001-01-01) then int32 kind (Unspecified 0, Utc 1, Local 2). The reader turns Unspecified into Local. TimeSpan is int64 ticks. Guid is 16 bytes in .NET `ToByteArray` order, where the first three groups are little-endian. ByteArray is an int32 length then the bytes. Media and MediaSet are a bool "wrapped" flag, an int32 length, then the bytes.

Column set for a page table. The client reads `Tables[0]` and finds cells by column name, never by position:

- `"0"`: Int64, the row version or timestamp
- `"bookmark"`: ByteArray, the row's position (3.5)
- `"suggestionAttributes"`: String, normally DBNull
- one column per table field, named by `MetaField.DataColumnName` (the field number as a decimal string), typed from `ClrType`
- one column per `MP_10` expression, named by `DFD_1`
- FlowFilter fields as String

The client creates its own tables with `"0"`, `"bookmark"` and `"suggestionAttributes"` first. Copy that order to be safe. Server-built tables seem to use the table id as the table name: the web server reads a system table result as `Tables["2000000071"]`. Use the id until a capture shows otherwise.

### 3.4 AL type to cell

| AL type | Column type | Encoding |
|---|---|---|
| Code, Text | String 17 | stored text. Blank is `""`, not DBNull. Code is uppercase already. |
| Integer | Int32 8 | |
| BigInteger | Int64 10 | |
| Decimal | Decimal 14 | keep the scale, since display formatting follows `CD2_4` |
| Boolean | Boolean 2 | |
| Date | DateTime 15 | midnight of the date, kind Unspecified or Local. `0D` = ticks 0 (`DateTime.MinValue`). A closing date is the same day at 23:59:59.000. |
| Time | DateTime 15 | date part 0001-01-02 plus the time of day. `0T` = ticks 0. |
| DateTime | DateTime 15 | UTC ticks with kind Utc (1). The client converts to the user's time zone. `0DT` = ticks 0. |
| Duration | TimeSpan 16 | ticks |
| Option, Enum | Int32 8 | ordinal value |
| Guid | Guid 18 | 16 bytes, .NET order |
| DateFormula | String 17 | |
| BLOB | ByteArray 19 | usually a lazy column, DBLazy until requested |
| Media, MediaSet | Media 20 / MediaSet 21 | Unconfirmed. Leave out of the first version. |

### 3.5 Bookmarks and paging

The `bookmark` cell is opaque to the web server. The web server never parses it. It only returns the bytes as `PageRequestDefinition.StartingBookmark`, `NavRecordState.CurrentRecord`, `SelectedRecords`/`NonSelectedRecords` and `InsertLower/UpperBoundBookmark`. It also puts a URL-safe form, with trailing zero bytes trimmed, into deep links. bc-rust may define its own format as long as the bytes are stable and decode back to the record. Table id plus encoded primary key values is the obvious choice.

`PageRequestDefinition` members: `PageSize`, `PageSizeInOppositeDirection`, `ReadDirection` (Next 0, Previous 1), `StartFromPage` (Specific 0, First 1, Last 2), `StartingBookmark`, `ExcludeStartingRecord`, `IncludeMoreDataInformation`, `IncludeNonRowData`, `NormalFields` and `CalcFields` (the field ids to fill, FlowFields in the second), `LoadFieldsHint`, `LookupFieldIds`/`LookupFieldValues`, `RequestRecentEntries`, `IsSubFormUpdateRequest`.

`NavRecordState` carries the view in both directions. `TableView` is a `NavTableView` with `TableId`, `CurrentSortingFieldIds` int[], `Ascending`, `CurrentFilters` and `SearchFilter`. Filter groups are `{FilterGroupNo, Filters: [NavFilter {FilterField, FilterValue, ...}]}`. The reply sets `MoreDataInReadDirection` and `MoreDataInOppositeDirection`, which the client uses for scrolling. The likely semantics are inferred, not confirmed. With `First` or `Last`, read from that end in key order. With `Specific`, start at the bookmark, skipping it when `ExcludeStartingRecord` is set. Return up to `PageSize` rows in `ReadDirection`, plus up to `PageSizeInOppositeDirection` rows on the other side.

## 4. Mapping bc-rust's model to the wire

| bc-rust (`crates/al-model`) | Wire target | Gap |
|---|---|---|
| `PageDef.id`, `name`, `caption` | `IDED_1`, `NED_1`, `NED_3` | none |
| `PageDef.page_type` (string) | `PP_7` int | map the name to the enum |
| `PageDef.source_table` | `SOD_2` | none |
| `PageDef.card_page` (name) | `PP_21` | resolve name to id |
| `PageDef.props["editable" / "insertallowed" / "modifyallowed" / "deleteallowed" / "delayedinsert" / "multiplenewlines" / "autosplitkey" / "datacaptionexpression"]` | `PP_18`, `SOD_3/4/6/8/9/11`, `PP_22` | parse raw strings into bools. Set EDSpec bits for explicit defaults. |
| `PageDef.view` (`TableView`) | `SOD_1` ViewDefinition | sorting and condition field names become field ids. Descending becomes `VDS_2 = false`. |
| `PageDef.layout` | `MP_3` and `MP_7` | split by area kind (Content vs FactBoxes) |
| `PageDef.actions` | `MP_1` CommandBar | area encoding is open (question 1) |
| page triggers in code objects | `MP_11`, `PP_16`, `PP_17` | needs to know which triggers have bodies, including extensions and subscribers |
| `Control.kind` | concrete wire type | group, repeater, grid, field, part, systempart, action, actionref, separator per 2.3 and 2.4. Still to map: cuegroup, fixed, usercontrol, label. |
| `Control.name`, `caption` | `NED_1`, `NED_3` | caption falls back to the field caption, then the field name |
| no id or GUID | `IDED_1`, `UIED_5` | **add**: stable per-page numeric ids and GUIDs, e.g. hash of page id + control path |
| `Control.field` | `CDD_11` = field id string | none |
| `Control.source_expr` (non-field) | `MP_10` DataFieldDefinition + `CDD_11` = its name | **add**: typing of expressions and per-row evaluation in data results |
| `Control.props["visible" / "enabled" / "editable" / "showmandatory" / "style" / "styleexpr"]` | `UIED_2`, `UIED_6`, `CDD_12`, `CDD_9`, `CDD_4/5` | literals pass through. Expressions need generated expression columns. |
| `Control.props["importance" / "tooltip" / "applicationarea" / "multiline" / "quickentry" / "showcaption"]` | `CD2_17`, `CD2_8`, `CD2_26`, `CD2_16`, `CD2_27`, `CD2_28` | enum and bool parsing |
| `Control.triggers` | `CDD_3` + Lookup/DrillDown/AssistEdit flags | none |
| `Control.part_page`, `sub_page_link` | `IPD_6`, `IPD_1` | resolve the page name and both field names to ids |
| action `runobject`, `runpagelink`, `runpagemode`, `image`, `promoted`, `shortcutkey` props | `AD_8`+`AD_6`, `AD_1`, `AD_14`, `ABD_1`, `ARD_*`, `ACPD_2` | parse RunObject and resolve ids. Map legacy `Promoted` to actionrefs. |
| `TableDef.id`, `name`, `caption`, `data_per_company` | `Id`, `Name`, `Caption`, `IsDataPerCompany` | none |
| `TableDef.lookup_page`, `drilldown_page`, `data_caption_fields` (names) | `LookupFormId`, `DrillDownFormId`, `DataCaptionFields` | resolve to ids |
| `TableDef.table_type` (string) | `TableType` | enum mapping |
| `TableDef.keys` (`KeyDef`) | `Keys` | key `Name`, field id/name pairs, `SumIndexFields`. MaintainSqlIndex and SIFT not modelled. |
| no field groups | `FieldGroups` | **add**: parse `fieldgroups` (DropDown, Brick) |
| `FieldDef.id`, `name`, `caption`, `tooltip`, `init_value`, `editable`, `not_blank`, `auto_increment`, `blank_zero`, `decimal_places` | same-named MetaField members | none |
| `FieldDef.ty` | `Type`, `ClrType`, `Length` | derive all three from `FieldType`. Decide Option vs Enum for enum fields. |
| `FieldDef.class` | `FieldClass` | KeyFieldReference not needed |
| `FieldDef.calc` (`CalcFormula`) | `CalcFormula` | resolve table and field ids. `FlowFieldType` string format unknown. |
| `FieldDef.options`, `option_captions` | `OptionString`, `OptionCaption`, `OptionValues` | join with commas |
| `EnumDef.values` | `OptionString`, `OptionCaption`, `OptionOrdinalValues`, `EnumTypeId`, `EnumTypeName`, `Extensible` | fill from the enum referenced by `FieldType::Enum` |
| `FieldDef.relation` (`RelationBranch`) | `Relations` and control `CDD_1` | `when` becomes Conditions, `conds` become Filters. All names become ids. |
| `filters::Cond` | FD / MetaFilter / MetaCondition | Const/Filter/Field become 0/1/2. `as_filter` becomes ValueIsFilter. `upper_limit` becomes OnlyMaxLimit. |
| not modelled | `ClosingDates`, `ExtendedDatatype`, `CaptionClass`, `MinValue`/`MaxValue`, `CharAllowed`, `ValuesAllowed`, `Width`, `BlankNumbers`, `AutoFormatType`/`AutoFormatExpression`, `ValidateTableRelation`, `AccessByPermission`, `ObsoleteState`, `DataClassification` | **add** to FieldDef. The solver needs them. |
| store rows | `"0"` and `"bookmark"` cells | **add**: per-row version counter and a stable bookmark codec |
| catalog generation | `MetadataToken`, `MP_15`, `VersionNumber` | **add**: a monotonically increasing token, bumped on reload |

The biggest pieces of work, in order:

1. Name-to-id resolution for every cross-reference.
2. Stable control and action ids and GUIDs.
3. A port of the NST's field-to-control default solver.
4. Typed expression columns for non-field sources and dynamic properties.
5. The missing field properties.

## 5. Open questions for a live capture

1. How the top level of `CBD0_1` represents action areas (Processing, Navigation, Reporting, Creation, Promoted), and what `PP_9 ActionsV2` changes.
2. Real `DataSetName` and table name in a `GetPage` reply, and whether NST tables keep `"0"`, `"bookmark"`, `"suggestionAttributes"` first.
3. The NST bookmark byte format. It only matters for keeping old deep links working.
4. DateTime kind the NST writes for Date and Time cells, and whether it uses compact mode (marker 3).
5. `MetaField.Type` for enum fields (Option 35584 or Enum 77569) and the exact `ClrType` strings, especially Media and BLOB.
6. `FD_4 FilterValue` for FIELD links in `IPD_1` and `AD_1`: a parent field id or a name. Also the `VDS_1 KeyFields` format.
7. The minimum member set the builders tolerate. Does a control without `UIED_5` or `IDED_1` break anything?
8. Whether EDSpec placement inside `obj` changes behaviour, i.e. whether the NST writes it first or last.
9. `client/data/page` response format, and which `ServerResponse` members (`requests`, `VersionNumber`) the web server checks on `GetPage`.
10. Grammar of dynamic Visible/Editable strings. Is it a bare expression column name, or can it hold `not` and operators?
11. `PersonalizationId` value in `PageResponseDependencyEntry` for a user with no personalization: null, empty, or the page id.
12. Which calls a first page open makes after `pagewithdependencies`, to confirm that version numbers equal to the token avoid the extra `pages/{id}` and `tables/{id}` requests.
