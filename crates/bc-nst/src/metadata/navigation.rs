//! `MasterNavigation` (`navigation`, `productoverview`) built from the role center page of
//! a profile. The shape follows a capture of the real service tier
//! (data/capture/nst-real/pre-fix, BC 29, profile BUSINESS MANAGER):
//!
//! ```text
//! MasterNavigation  IDED_1 1, NED_1 "Navigation", NED_3 "Dynamics 365 Business Central"
//!   MN_1 CommandBarDefinition (global actions; empty here)
//!   MN_2 CommandBarDefinition (departments command bar; empty here)
//!   MN_3 NavigationPaneDefinition
//!     NPD_1 NavigationGroupDefinition "Control_Home" (NGD_1 Home 0, HomeItemsGroupGuid)
//!             ActionDefinition "Control_Role_Center" (HomepageActionGuid, AD_6 = role center)
//!             ActionContainerPlaceHolder (ACPH_1 HomeItems 4) -> area(Embedding) actions
//!           NavigationGroupDefinition "Control_MyActivityButton" (NGD_1 ActivityButtons 1)
//!             ActionContainerPlaceHolder (ACPH_1 ActivityButtons 5) -> area(Sections) groups
//! ```
//!
//! The web server builds its navigation tree from `MN_3` (`NavigationTreeProvider`); the
//! node with the homepage GUID becomes `NavigationStart`, and without it opening the default
//! page fails with "no permission to open profile". The real NST also sends a Departments
//! group (hidden by the web client) and a Product Overview group; bc-rust leaves them out.

use super::el::El;
use super::ids::hashed_guid;
use super::page::Gen;
use al_model::{Catalog, ProfileDef};
use serde_json::Value;
use uuid::Uuid;

/// `NavigationTreeProvider.HomepageActionGuid`.
pub const HOMEPAGE_ACTION_GUID: Uuid = Uuid::from_u128(0xEC29FEAF_58AE_4C7A_8C7F_792D5CFD9AA0);
/// `NavigationTreeProvider.HomeItemsGroupGuid`.
pub const HOME_ITEMS_GROUP_GUID: Uuid = Uuid::from_u128(0x27F878F7_1EB4_4B4C_A6D5_55E87F26029B);
/// `NavigationTreeProvider.ProductOverviewGroupGuid`.
pub const PRODUCT_OVERVIEW_GROUP_GUID: Uuid = Uuid::from_u128(0x0712F7A1_4368_4C41_ADBB_C4FB332E8EEB);
// GUIDs of the system navigation nodes, as the real NST sends them.
const NAVIGATION_PANE_GUID: Uuid = Uuid::from_u128(0xDF1AA864_6D25_4F2A_8620_C078B7B4CA17);
const ACTIVITY_BUTTONS_GUID: Uuid = Uuid::from_u128(0xF25C9447_C6A0_4C5D_9405_0C3AFC696556);
const GLOBAL_COMMAND_BAR_GUID: Uuid = Uuid::from_u128(0xDD2BDF5A_5852_41E8_9D06_ABAE890E2FFF);
const DEPARTMENTS_COMMAND_BAR_GUID: Uuid = Uuid::from_u128(0x641FA567_E660_4AA5_8ED9_44DD97D7FB10);
/// Base Application app id, used in `ProfileKey`.
const BASE_APP_ID: &str = "437dbf0e-84ff-417a-965d-ed2bb9650972";

/// The profile by id (case-insensitive), falling back to the first profile with a role center.
pub fn profile<'a>(cat: &'a Catalog, profile_id: &str) -> Option<&'a ProfileDef> {
    cat.profiles
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(profile_id) && p.role_center.is_some_and(|rc| cat.pages.contains_key(&rc)))
        .or_else(|| cat.profiles.iter().find(|p| p.role_center.is_some_and(|rc| cat.pages.contains_key(&rc))))
}

/// Role center page id of a profile.
pub fn role_center(cat: &Catalog, profile_id: &str) -> Option<i64> {
    profile(cat, profile_id).and_then(|p| p.role_center)
}

fn nav_group(id: i64, name: &str, caption: &str, guid: Uuid, kind: i64, tooltip: Option<&str>, actions: Vec<Value>) -> Value {
    let mut el = El::new("NavigationGroupDefinition");
    el.num_bit("IDED_1", id, 0);
    el.put("NED_1", name);
    el.str("NED_3", caption);
    el.put("UIED_5", guid.to_string());
    el.list("CBD_1", actions);
    el.num_bit("NGD_1", kind, 10);
    if let Some(t) = tooltip {
        el.str("NGD_2", t);
    }
    el.wrap()
}

/// `ActionContainerPlaceHolder` that carries the role center's own actions of one area.
fn placeholder(id: i64, p: &ProfileDef, rc_name: &str, container: i64, guid: Uuid, actions: Vec<Value>) -> Value {
    let mut el = El::new("ActionContainerPlaceHolder");
    el.num_bit("IDED_1", id, 0);
    el.put("NED_1", rc_name);
    el.put("NED_3", rc_name);
    el.put("UIED_5", guid.to_string());
    el.list("CBD_1", actions);
    el.poly_enum_bit("ACPH_1", "ActionContainerType", container, 10);
    el.put("ProfileKey", format!("{}|{BASE_APP_ID}|Tenant", p.name));
    el.wrap()
}

/// Actions of every `area(<area>)` of the role center, converted like command bar actions.
fn area_actions(g: &mut Gen, area: &str) -> Vec<Value> {
    let page = g.page;
    let mut out = vec![];
    for (i, a) in page.actions.iter().enumerate() {
        if a.kind == "area" && a.name.eq_ignore_ascii_case(area) {
            for (j, c) in a.children.iter().enumerate() {
                if let Some(v) = g.action(c, &[i, j]) {
                    out.push(v);
                }
            }
        }
    }
    out
}

fn command_bar(guid: Uuid) -> Value {
    let mut el = El::new("CommandBarDefinition");
    el.put("UIED_5", guid.to_string());
    el.list("CBD0_1", vec![]);
    el.wrap()
}

fn master_navigation(groups: Vec<Value>, token: i64) -> Value {
    let mut pane = El::new("NavigationPaneDefinition");
    pane.num_bit("IDED_1", 2000000374, 0);
    pane.put("UIED_5", NAVIGATION_PANE_GUID.to_string());
    pane.list("NPD_1", groups);
    let mut mn = El::new("MasterNavigation");
    mn.num_bit("IDED_1", 1, 0);
    mn.put("NED_1", "Navigation");
    mn.put("NED_3", "Dynamics 365 Business Central");
    mn.put("MN_1", command_bar(GLOBAL_COMMAND_BAR_GUID));
    mn.put("MN_2", command_bar(DEPARTMENTS_COMMAND_BAR_GUID));
    mn.put("MN_3", pane.wrap());
    mn.num_bit("MN_4", token, 2);
    mn.wrap()
}

/// `MasterNavigation` for `navigation`, or `Value::Null` when no profile has a role center.
pub fn navigation(cat: &Catalog, profile_id: &str, token: i64) -> Value {
    let Some(p) = profile(cat, profile_id) else { return Value::Null };
    let Some(rc) = p.role_center else { return Value::Null };
    let page = &cat.pages[&rc];
    let mut g = Gen::new(cat, page, token);
    // the homepage action: opens the role center page
    let mut home = El::new("ActionDefinition");
    home.num_bit("IDED_1", 2000006024, 0);
    home.put("NED_1", "Control_Role_Center");
    home.put("NED_3", "Role Center");
    home.put("UIED_5", HOMEPAGE_ACTION_GUID.to_string());
    home.num_bit("AD_6", rc, 16);
    home.poly_enum_bit("AD_8", "RunObjectType", 1, 18);
    home.num_bit("AD_17", 1, 22);
    let embedding = area_actions(&mut g, "embedding");
    let home_items = placeholder(2000009025, p, &page.name, 4, hashed_guid(&format!("bc-rust/home-items/{rc}")), embedding);
    let tooltip = p.description.as_str();
    let mut groups = vec![nav_group(2000006023, "Control_Home", "Home", HOME_ITEMS_GROUP_GUID, 0, Some(tooltip), vec![home.wrap(), home_items])];
    let sections = area_actions(&mut g, "sections");
    if !sections.is_empty() {
        let ph = placeholder(2000009026, p, &page.name, 5, hashed_guid(&format!("bc-rust/activity-buttons/{rc}")), sections);
        groups.push(nav_group(2000006026, "Control_MyActivityButton", "Control_MyActivityButton", ACTIVITY_BUTTONS_GUID, 1, None, vec![ph]));
    }
    master_navigation(groups, token)
}

/// `MasterNavigation` for `productoverview`: one Product Overview group with the role
/// center's home items and sections (a bc-rust guess at its content).
pub fn product_overview(cat: &Catalog, profile_id: &str, token: i64) -> Value {
    let Some(p) = profile(cat, profile_id) else { return Value::Null };
    let Some(rc) = p.role_center else { return Value::Null };
    let page = &cat.pages[&rc];
    let mut g = Gen::new(cat, page, token);
    let mut items = area_actions(&mut g, "embedding");
    items.extend(area_actions(&mut g, "sections"));
    let group = nav_group(2000006030, "Control_ProductOverview", "Product Overview", PRODUCT_OVERVIEW_GROUP_GUID, 12, None, items);
    master_navigation(vec![group], token)
}
