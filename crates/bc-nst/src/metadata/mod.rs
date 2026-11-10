//! Page and table metadata in the wire format Microsoft's BC web server expects from the
//! service tier (`<service base URL>client/metadata/...`). Spec: `docs/nst-protocol-metadata.md`.
//! Names, types and defaults come from the BC 29 web-server assemblies (`Microsoft.Dynamics.Nav.Types`).
//!
//! # What is generated
//!
//! - [`master_page`]: the `$polyType`-wrapped `MasterPage` of a page, with `PageProperties`,
//!   `SourceObjectDefinition`, the content area (`MP_3`), factboxes (`MP_7`), the command
//!   bar (`MP_1`), expression columns (`MP_10`) and page triggers (`MP_11`). The NST-side
//!   property solver (`PropertiesSolveHelper`) is applied: captions, tooltips and option
//!   captions fall back to the field, DrillDown/Lookup/AssistEdit/DropDown/ClosingDates/
//!   Numeric/BlankZero/BlankNumbers/NotBlank/ExtendedDatatype/MaskType/Min/MaxValue/Editable/
//!   RelatedObjects/CaptionClass are filled from the table field the control is bound to.
//! - [`meta_table`]: `MetaTable` with fields, keys, field groups, relations and CalcFormulas.
//! - [`page_with_dependencies`]: `PageDependencyResult` with the page, its subpage parts
//!   and factbox pages, and the source tables not in the cache list.
//! - [`master_navigation_versioned`] / [`homepage_versioned`]: the `MasterNavigation` and
//!   the role center `MasterPage` of a profile (see [`navigation`]).
//! - [`router`]: the HTTP endpoints (`pagewithdependencies`, `pages`, `tables`, `navigation`,
//!   `homepage`, `productoverview`, `systemparts`, `configuration`, `pagedatapersonalization`).
//!   Every response carries the `MetadataToken` header.
//!
//! # Control ids (`IDED_1`) and GUIDs (`UIED_5`)
//!
//! [`ControlIndex::build`] walks a page's merged layout tree, then its actions tree, in
//! pre-order and gives every element (area, group, repeater, field, part, systempart,
//! action, group, actionref, separator, ...) one [`ControlEntry`]:
//!
//! - `id`: if the AL name is `Control<digits>` (txt2al kept the C/SIDE id that way, e.g.
//!   `Control1900383207`), that number. Otherwise a 64-bit FNV-1a hash of
//!   `"<section>/<lower-cased name>"` (section is `layout` or `actions`) folded into
//!   `1..=2147483646`. A collision with an earlier entry, or a repeated name, re-hashes with
//!   a salt `#1`, `#2`, ... The id depends only on the name, so it is stable across reloads,
//!   extensions and edits elsewhere on the page.
//! - `guid`: a name-based GUID (RFC 9562 v8) from page id, section, lower-cased name and the
//!   occurrence number of the name.
//! - The command bar containers and the navigation nodes use fixed ids (2000001076,
//!   2000009016, ...) and the fixed GUIDs the real service tier sends (see `actions.rs`,
//!   [`navigation`]); they are not invocable and not in the index.
//! - The page itself has `IDED_1` = page id and `PP_26` = [`page_guid`].
//! - Synthetic [`Section::System`] entries: the command bar ([`ids::COMMAND_BAR`]) and, for
//!   legacy `Promoted = true` actions, a promoted area, `Category_*` groups and
//!   `<action>_Promoted` refs.
//!
//! The web client sends these back: actions are invoked by `IDED_1` (`InvokePageTriggerAction`),
//! field controls report `ControlId = IDED_1`, action refs bind with `ARD_2 == IDED_1` of
//! the target, and controls are named by `ControlGUID.ToString("B")`. Resolve an id with
//! `ControlIndex::build(page).by_id(id)` and `ControlIndex::control(page, entry)`.
//!
//! # Data columns
//!
//! A field bound to the page's table uses `CDD_11 = "<field no>"`, which is also
//! `MetaField.DataColumnName`. Everything else that needs a per-row value is an
//! [`ExpressionColumn`] (see [`expression_columns`]): non-field sources, page variables
//! used by dynamic properties, `Control<id>_<Prop>` columns for non-trivial property
//! expressions and `Control<id>_DynamicCaption` for CaptionClass. Dynamic `Visible`,
//! `Enabled`, `Editable`, `ShowMandatory`, `StyleExpr`, `HideValue` and `QuickEntry`
//! strings are the literal as written (`"false"`), a page variable or table field name,
//! or the name of such a generated column.
//!
//! # Confirmed versus guessed
//!
//! Wire facts [src:TODO]: all wire names and types, `$polyType`/`obj`
//! (also for the two enums in a polymorphic hierarchy, `RunObjectType` and
//! `ActionContainerType`, e.g. `AD_8`), EDSpec bit numbers and `[DefaultValue]`s, the
//! client's null-handling (`MP_3`, `MP_8`, `PP_1`, `MN_1`, `MetaField.ClrType` must be
//! present), `ApplicationArea` as `#A,#B`, `FlowFieldType` names, FIELD filter values as the
//! providing field's id string, CONST conditions on option fields as ordinals,
//! `VDS_1 KeyFields` as `Field<n>,...`, `AD_17 PushAction = 1` for RunObject actions,
//! `PP_9 ActionsV2` enabling action refs, enum fields typed Option (35584) with
//! `EnumTypeName`, `MetaKey` booleans always written, and the solver rules.
//!
//! Confirmed from a capture of the real service tier (data/capture/nst-real/pre-fix,
//! `navigation` and `pagewithdependencies/9022`): the navigation pane layout, the command
//! bar containers (`NavigationGroupDefinition`s `Control_Promoted`, `Control_Actions` with
//! `Control_New_Document` inside, `Control_Reports`) and their GUIDs, the role center
//! `ControlContainerPlaceHolder`, `PersonalizationId` = `""`, EDSpec as the full mask (the
//! NST writes it last), and the `MetadataToken` header.
//!
//! Guessed: `Control_Related` for `area(Navigation)`, expression column names and `DFD_12`
//! kinds, captions of nested caption-less groups (left empty) and of parts and action refs
//! (taken from the target), `MetaField.Validation` and `Lookup` semantics, Media `ClrType`
//! strings, the system-part pages and the product overview. Control ids differ from the
//! real NST's (the AL compiler hashes names, `Control139` is not 139 there) but are
//! consistent within bc-rust.

mod actions;
mod alprop;
mod el;
mod http;
pub mod ids;
pub mod navigation;
mod page;
mod table;
#[cfg(test)]
mod tests;

pub use http::{mounted_router, router};
pub use ids::{control_at, control_guid, page_guid, stable_id, ControlEntry, ControlIndex, Section};
pub use navigation::role_center;
pub use page::{ExpressionColumn, ExpressionKind};

use al_model::Catalog;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI64, Ordering};

static TOKEN: AtomicI64 = AtomicI64::new(1);

/// The process-wide metadata token used by the functions without a `token` argument.
/// [`MetadataState::new`] sets it. Bump it whenever the catalog changes.
pub fn metadata_token() -> i64 {
    TOKEN.load(Ordering::Relaxed)
}

pub fn set_metadata_token(token: i64) {
    TOKEN.store(token, Ordering::Relaxed);
}

/// Shared state of the metadata endpoints.
#[derive(Clone)]
pub struct MetadataState {
    pub catalog: &'static Catalog,
    /// Sent as the `MetadataToken` header and used for `MP_15` / `MetaTable.VersionNumber`.
    /// Must equal the token the JSON-RPC endpoint sends.
    pub token: i64,
    /// Profile whose role center backs `navigation`, `homepage` and `productoverview`
    /// (the metadata calls carry no session, so this is the server's default profile).
    pub profile: String,
}

pub const DEFAULT_PROFILE: &str = "BUSINESS MANAGER";

impl MetadataState {
    pub fn new(catalog: &'static Catalog, token: i64) -> MetadataState {
        set_metadata_token(token);
        MetadataState { catalog, token, profile: DEFAULT_PROFILE.to_string() }
    }

    pub fn with_profile(mut self, profile: impl Into<String>) -> MetadataState {
        self.profile = profile.into();
        self
    }
}

/// `MasterNavigation` of a profile's role center (`navigation`).
pub fn master_navigation_versioned(cat: &Catalog, profile: &str, token: i64) -> Value {
    navigation::navigation(cat, profile, token)
}

/// `MasterPage` of a profile's role center (`homepage`).
pub fn homepage_versioned(cat: &Catalog, profile: &str, token: i64) -> Value {
    match navigation::role_center(cat, profile) {
        Some(rc) => master_page_versioned(cat, rc, token),
        None => Value::Null,
    }
}

/// `$polyType`-wrapped `MasterPage` of a page, or `Value::Null` for an unknown page.
pub fn master_page(cat: &Catalog, page_id: i64) -> Value {
    master_page_versioned(cat, page_id, metadata_token())
}

pub fn master_page_versioned(cat: &Catalog, page_id: i64, token: i64) -> Value {
    match cat.pages.get(&page_id) {
        Some(p) => page::Gen::new(cat, p, token).master_page(),
        None => Value::Null,
    }
}

/// The expression columns of a page, in `MP_10` order (empty for an unknown page).
pub fn expression_columns(cat: &Catalog, page_id: i64) -> Vec<ExpressionColumn> {
    match cat.pages.get(&page_id) {
        Some(p) => {
            let mut g = page::Gen::new(cat, p, 0);
            let _ = g.master_page();
            g.exprs
        }
        None => vec![],
    }
}

/// `MetaTable` of a table, or `Value::Null` for an unknown table.
pub fn meta_table(cat: &Catalog, table_id: i64) -> Value {
    meta_table_versioned(cat, table_id, metadata_token())
}

pub fn meta_table_versioned(cat: &Catalog, table_id: i64, token: i64) -> Value {
    match cat.tables.get(&table_id) {
        Some(t) => table::meta_table(cat, t, token),
        None => Value::Null,
    }
}

/// Pages a page opens right away: subpage parts in the content area and factbox parts,
/// recursively (two levels), without duplicates, the page itself first.
pub fn dependent_pages(cat: &Catalog, page_id: i64) -> Vec<i64> {
    let mut out = vec![page_id];
    let mut i = 0;
    let mut depth = vec![0usize];
    while i < out.len() {
        let (id, d) = (out[i], depth[i]);
        i += 1;
        if d >= 2 {
            continue;
        }
        let Some(p) = cat.pages.get(&id) else { continue };
        let mut parts = vec![];
        collect_parts(&p.layout, &mut parts);
        for name in parts {
            if let Some(pp) = cat.page_by_name(&name) {
                if !out.contains(&pp.id) {
                    out.push(pp.id);
                    depth.push(d + 1);
                }
            }
        }
    }
    out
}

fn collect_parts(tree: &[al_model::Control], out: &mut Vec<String>) {
    for c in tree {
        if c.kind == "part" {
            if let Some(n) = &c.part_page {
                out.push(n.clone());
            }
        }
        collect_parts(&c.children, out);
    }
}

/// `PageDependencyResult`: `{"MasterPages": [{"PersonalizationId", "MasterPage"}], "MetaTables": [...]}`.
/// Source tables listed in `cached_table_ids` are left out of `MetaTables`.
pub fn page_with_dependencies(cat: &Catalog, page_id: i64, cached_table_ids: &[i64]) -> Value {
    page_with_dependencies_versioned(cat, page_id, cached_table_ids, metadata_token())
}

pub fn page_with_dependencies_versioned(cat: &Catalog, page_id: i64, cached_table_ids: &[i64], token: i64) -> Value {
    if !cat.pages.contains_key(&page_id) {
        return Value::Null;
    }
    let mut pages = vec![];
    let mut tables: Vec<i64> = vec![];
    for id in dependent_pages(cat, page_id) {
        let mp = master_page_versioned(cat, id, token);
        if mp.is_null() {
            continue;
        }
        // the real NST sends "" here, also when the request names a personalization id
        pages.push(json!({ "PersonalizationId": "", "MasterPage": mp }));
        if let Some(t) = cat.pages[&id].source_table {
            if !tables.contains(&t) && !cached_table_ids.contains(&t) {
                tables.push(t);
            }
        }
    }
    let metas: Vec<Value> = tables.iter().map(|&t| meta_table_versioned(cat, t, token)).filter(|v| !v.is_null()).collect();
    json!({ "MasterPages": pages, "MetaTables": metas })
}

/// The wire `PageType` of an AL page type name (Card 0, List 1, RoleCenter 2, ...).
pub fn page_type_code(page_type: &str) -> i64 {
    alprop::enum_val(alprop::PAGE_TYPES, page_type).unwrap_or(0)
}

/// The captured `MasterPage` of a system part (Notes, RecordLinks/Links), if build.rs found
/// it in data/nst-templates/systemparts/. A missing file is the stub `{}`.
pub(crate) fn captured_system_part(part_type: &str) -> Option<Value> {
    let src = match part_type.to_ascii_lowercase().as_str() {
        "recordlinks" | "links" => include_str!(concat!(env!("OUT_DIR"), "/systemparts/recordlinks.json")),
        "notes" => include_str!(concat!(env!("OUT_DIR"), "/systemparts/notes.json")),
        _ => return None,
    };
    match serde_json::from_str::<Value>(src) {
        Ok(v) if v.get("$polyType").is_some() => Some(v),
        _ => {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                eprintln!("bc-nst: no captured system parts (data/nst-templates/systemparts/, see tools/nst-templates/mktemplates.py); Notes and Links use bc-nst's own part");
            });
            None
        }
    }
}

/// Whether pages may offer system part `part` (a page's `systempart(...; Links)`): Notes and Links
/// only when the captured parts are built in (see `system_part_versioned`).
pub(crate) fn system_part_available(part: &str) -> bool {
    match part.to_ascii_lowercase().as_str() {
        "notes" | "mynotes" => captured_system_part("notes").is_some(),
        "links" | "recordlinks" => captured_system_part("recordlinks").is_some(),
        _ => true,
    }
}

/// `MasterPage` for `systemparts/{type}` (Notes, MyNotes, RecordLinks/Links). Notes and Links
/// are the captured parts when available; otherwise the shape is a bc-rust guess: a CardPart
/// whose content is the matching system control.
pub fn system_part_versioned(part_type: &str, token: i64) -> Value {
    // Links and Notes: the real service tier's parts (a BC 29.1 capture), list pages on table
    // 2000000068 Record Link filtered on Type (0 link, 1 note). The web client binds them to
    // the host page; bc-rust's earlier guess (a bare control) made the FactBox pane fail in
    // LinksListBuilder (no binding manager). The captured parts are not in the repository:
    // without data/nst-templates/systemparts/ (tools/nst-templates/mktemplates.py) the guess
    // below is all there is.
    if let Some(mut v) = captured_system_part(part_type) {
        el::edspec_first(&mut v);
        if let Some(o) = v.get_mut("obj").and_then(Value::as_object_mut) {
            o.insert("MP_15".into(), Value::from(token));
        }
        return v;
    }
    let (ty, name, my) = match part_type.to_ascii_lowercase().as_str() {
        "notes" => ("NotesControlDefinition", "Notes", false),
        "mynotes" => ("NotesControlDefinition", "MyNotes", true),
        "recordlinks" | "links" => ("RecordLinksControlDefinition", "Links", false),
        _ => return Value::Null,
    };
    let mut ctl = el::El::new(ty);
    ctl.num_bit("IDED_1", 1, 0);
    ctl.put("NED_1", name);
    ctl.put("UIED_5", ids::hashed_guid(&format!("bc-rust/systempart/{name}")).to_string());
    if my {
        ctl.put("NCD0_1", true);
    }
    let mut ca = el::El::new("ContentAreaDefinition");
    ca.list("ABD0_1", vec![ctl.wrap()]);
    let mut pp = el::El::new("PageProperties");
    pp.put("PP_1", el::El::new("SourceObjectDefinition").wrap());
    pp.num_bit("PP_7", 3, 0);
    let mut mp = el::El::new("MasterPage");
    mp.put("NED_1", name);
    mp.put("NED_3", name);
    mp.put("MP_3", ca.wrap());
    mp.put("MP_8", pp.wrap());
    mp.num_bit("MP_15", token, 2);
    mp.wrap()
}
