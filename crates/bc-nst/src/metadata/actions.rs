//! Command bar (`MP_1`). The layout follows the real service tier's role center and part
//! pages (data/capture/nst-real/pre-fix): the top level of `CBD0_1` holds
//! `NavigationGroupDefinition`s, one per container,
//!
//! - `Control_Promoted` (GUID 9D7FA316-..., no `NGD_1`): `area(Promoted)` and legacy promoted actions,
//! - `Control_Actions` (`NGD_1` Actions 3, "&Actions"): `area(Processing)`, preceded by an
//!   `ActionGroupDefinition` `Control_New_Document` ("New Do&cument", GUID 1AF878F7-...)
//!   holding `area(Creation)`. GUID 31C4E566-... on root pages, 9BFB7851-... on part pages,
//! - `Control_Related` (`NGD_1` RelatedInformation 6): `area(Navigation)` (name, caption
//!   and GUID are bc-rust guesses: the captured pages have no navigation area),
//! - `Control_Reports` (`NGD_1` Reports 5, "&Reports", GUID ADC604A2-...): `area(Reporting)`.
//!
//! `area(Embedding)` and `area(Sections)` of a role center go to the navigation pane, not here.
//! Actions, groups, action refs and separators map to their definitions below the containers.

use super::alprop::{self, app_area, cbool, enum_val, src, word};
use super::el::El;
use super::ids::{hashed_guid, ControlEntry, Section, ACTION_ITEMS_GUID, COMMAND_BAR, NEW_DOCUMENT_ITEMS_GUID, PROMOTED_GUID};
use super::page::{ctext, Ctx, Gen};
use al_model::Control;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use uuid::Uuid;

/// `Control_Actions` on root pages (role center capture).
const ROOT_ACTIONS_GUID: Uuid = Uuid::from_u128(0x31C4E566_9070_42F7_82E1_EED629A94672);
/// `Control_Reports` (role center capture).
const REPORTS_GUID: Uuid = Uuid::from_u128(0xADC604A2_02B5_423A_B1BD_832EA04FE89B);

/// Top-level `NavigationGroupDefinition` of the command bar.
fn container(id: i64, name: &str, caption: &str, guid: Uuid, kind: Option<i64>, actions: Vec<Value>) -> Value {
    let mut el = El::new("NavigationGroupDefinition");
    el.num_bit("IDED_1", id, 0);
    el.put("NED_1", name);
    el.put("NED_3", caption);
    el.put("UIED_5", guid.to_string());
    el.list("CBD_1", actions);
    if let Some(k) = kind {
        el.num_bit("NGD_1", k, 10);
    }
    el.wrap()
}

/// The real service tier's system actions per page type (Control_Home with New / Manage /
/// View / Page groups, Control_Microsoft_Dynamics_NAV with Print & Send and Help, the
/// list-part Edit / View / New / Find rows, ...), extracted from a capture by
/// tools/nst-templates/mktemplates.py: list (page 22), card (21), document (42), listpart (46),
/// cardpart (9080), rolecenter (9022). `{"$slot": area}` marks where the page's own actions
/// go; a node with `"$ifslot": area` is left out when that slot is empty.
///
/// The file is not in the repository. build.rs copies it from data/nst-templates/, or writes
/// `{}` when it is missing; then every page type gets the built-in containers of `command_bar`.
const SYSTEM_TEMPLATES: &str = include_str!(concat!(env!("OUT_DIR"), "/sysactions.json"));

fn system_templates() -> &'static Map<String, Value> {
    static T: OnceLock<Map<String, Value>> = OnceLock::new();
    T.get_or_init(|| {
        let mut v: Value = serde_json::from_str(SYSTEM_TEMPLATES).unwrap_or(Value::Null);
        super::el::edspec_first(&mut v);
        let m = match v {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        if m.is_empty() {
            eprintln!("bc-nst: no system action templates (data/nst-templates/sysactions.json, see tools/nst-templates/mktemplates.py); pages get bc-nst's own command bar");
        }
        m
    })
}

/// Whether the build found the captured system action templates.
#[cfg(test)]
pub(crate) fn have_system_templates() -> bool {
    !system_templates().is_empty()
}

/// The template for a page type. Dialogs, navigate pages and API pages keep only their own
/// actions (no capture shows their system actions).
pub(crate) fn system_template(page_type: &str) -> Option<&'static Value> {
    let k = match page_type.to_ascii_lowercase().as_str() {
        "list" | "worksheet" => "list",
        "card" => "card",
        "document" | "listplus" => "document",
        "listpart" => "listpart",
        "cardpart" | "headlinepart" => "cardpart",
        "rolecenter" => "rolecenter",
        _ => return None,
    };
    system_templates().get(k)
}

fn template_slots(t: &Value) -> HashSet<String> {
    fn walk(v: &Value, out: &mut HashSet<String>) {
        match v {
            Value::Object(m) => {
                if let Some(s) = m.get("$slot").and_then(Value::as_str) {
                    out.insert(s.to_string());
                }
                m.values().for_each(|x| walk(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = HashSet::new();
    walk(&t["CBD0_1"], &mut out);
    out
}

/// Copy template nodes, replacing `$slot` markers with the page's actions.
fn fill_template(nodes: &[Value], slots: &mut HashMap<&'static str, Vec<Value>>) -> Vec<Value> {
    let mut out = vec![];
    for n in nodes {
        if let Some(s) = n.get("$slot").and_then(Value::as_str) {
            out.extend(slots.get_mut(s).map(std::mem::take).unwrap_or_default());
            continue;
        }
        let Some(obj) = n.get("obj").and_then(Value::as_object) else { continue };
        if let Some(s) = obj.get("$ifslot").and_then(Value::as_str) {
            if slots.get(s).is_none_or(|v| v.is_empty()) {
                continue;
            }
        }
        let mut o = Map::new();
        for (k, v) in obj {
            match (k.as_str(), v) {
                ("$ifslot", _) => {}
                ("CBD_1", Value::Array(kids)) => {
                    o.insert(k.clone(), Value::Array(fill_template(kids, slots)));
                }
                _ => {
                    o.insert(k.clone(), v.clone());
                }
            }
        }
        let mut w = Map::new();
        w.insert("$polyType".into(), n["$polyType"].clone());
        w.insert("obj".into(), Value::Object(o));
        out.push(Value::Object(w));
    }
    out
}

/// Default captions of legacy promoted categories (`PromotedActionCategories` index order).
fn legacy_category_caption(page_prop: Option<&str>, group: &str) -> String {
    let idx = match group {
        "Category_New" => 0,
        "Category_Process" => 1,
        "Category_Report" => 2,
        g => g.strip_prefix("Category_Category").and_then(|n| n.parse::<usize>().ok()).map(|n| n - 1).unwrap_or(usize::MAX),
    };
    let custom = page_prop.map(|s| super::page::text_prop(s)).and_then(|s| s.split(',').nth(idx).map(|x| x.trim().to_string())).filter(|s| !s.is_empty());
    custom.unwrap_or_else(|| match idx {
        0 => "New".into(),
        1 => "Process".into(),
        2 => "Report".into(),
        n if n != usize::MAX => format!("Category{}", n + 1),
        _ => group.trim_start_matches("Category_").to_string(),
    })
}

impl<'a> Gen<'a> {
    pub(crate) fn command_bar(&mut self) -> Option<Value> {
        let page = self.page;
        let (mut promoted, mut creation, mut processing, mut navigation, mut reporting) = (vec![], vec![], vec![], vec![], vec![]);
        for (i, a) in page.actions.iter().enumerate() {
            if a.kind != "area" {
                if let Some(v) = self.action(a, &[i]) {
                    processing.push(v);
                }
                continue;
            }
            let kids = self.action_children(a, &[i]);
            match a.name.to_ascii_lowercase().as_str() {
                "promoted" => promoted.extend(kids),
                "creation" => creation.extend(kids),
                "processing" => processing.extend(kids),
                "navigation" => navigation.extend(kids),
                "reporting" => reporting.extend(kids),
                // role center navigation (embedding, sections) and anything unknown
                _ => {}
            }
        }
        promoted.extend(self.legacy_promoted_groups());
        let own_actions = !(promoted.is_empty() && creation.is_empty() && processing.is_empty() && navigation.is_empty() && reporting.is_empty());
        if let Some(t) = system_template(&page.page_type) {
            let e = self.ix.find(Section::System, COMMAND_BAR)?.clone();
            let mut slots: HashMap<&'static str, Vec<Value>> = HashMap::new();
            let have = template_slots(t);
            // a slot the template has no container for goes into processing (parts have
            // no Reports / Related containers in the captures)
            for (name, v) in [("creation", creation), ("reporting", reporting), ("navigation", navigation)] {
                if have.contains(name) {
                    slots.insert(name, v);
                } else if name == "creation" {
                    processing.splice(0..0, v);
                } else {
                    processing.extend(v);
                }
            }
            slots.insert("promoted", promoted);
            slots.insert("processing", processing);
            let top = fill_template(t["CBD0_1"].as_array().map(Vec::as_slice).unwrap_or(&[]), &mut slots);
            let mut el = El::new("CommandBarDefinition");
            el.num_bit("IDED_1", e.id as i64, 0);
            el.put("UIED_5", e.guid.to_string());
            el.list("CBD0_1", top);
            // ShowQuickAccessPane is always specified (EDSpec bit 2): true on lists, cards,
            // documents and card parts, false on list parts
            el.flag("CBD0_2", t["root"]["CBD0_2"].as_bool().unwrap_or(false), 2);
            // ContainsPageTemplateElementsOnly defaults to true; false once the page has actions
            if own_actions {
                el.put("CBD0_3", false);
            }
            return Some(el.wrap());
        }
        let part = matches!(page.page_type.to_ascii_lowercase().as_str(), "cardpart" | "listpart");
        let mut top = vec![];
        if !promoted.is_empty() {
            top.push(container(2000001076, "Control_Promoted", "Control_Promoted", PROMOTED_GUID, None, promoted));
        }
        if !creation.is_empty() {
            let mut nd = El::new("ActionGroupDefinition");
            nd.num_bit("IDED_1", 2000009017, 0);
            nd.put("NED_1", "Control_New_Document");
            nd.put("NED_3", "New Do&cument");
            nd.put("UIED_5", NEW_DOCUMENT_ITEMS_GUID.to_string());
            nd.list("CBD_1", creation);
            processing.insert(0, nd.wrap());
        }
        if !processing.is_empty() {
            let guid = if part { ACTION_ITEMS_GUID } else { ROOT_ACTIONS_GUID };
            top.push(container(2000009016, "Control_Actions", "&Actions", guid, Some(3), processing));
        }
        if !navigation.is_empty() {
            top.push(container(2000009019, "Control_Related", "&Related", hashed_guid("bc-rust/command-bar/related"), Some(6), navigation));
        }
        if !reporting.is_empty() {
            top.push(container(2000009022, "Control_Reports", "&Reports", REPORTS_GUID, Some(5), reporting));
        }
        if top.is_empty() {
            return None;
        }
        let e = self.ix.find(Section::System, COMMAND_BAR)?.clone();
        let mut el = El::new("CommandBarDefinition");
        el.num_bit("IDED_1", e.id as i64, 0);
        el.put("UIED_5", e.guid.to_string());
        el.list("CBD0_1", top);
        // defaults to true, which only matters for add-in-only pages
        el.put("CBD0_3", false);
        Some(el.wrap())
    }

    fn action_children(&mut self, c: &Control, path: &[usize]) -> Vec<Value> {
        let mut out = vec![];
        for (i, k) in c.children.iter().enumerate() {
            let mut p = path.to_vec();
            p.push(i);
            if let Some(v) = self.action(k, &p) {
                out.push(v);
            }
        }
        out
    }

    pub(crate) fn action(&mut self, c: &Control, path: &[usize]) -> Option<Value> {
        let e = self.ix.at(Section::Actions, path)?.clone();
        Some(match c.kind.as_str() {
            "action" => self.action_def(c, &e),
            "group" => {
                let mut el = El::new("ActionGroupDefinition");
                let caption = c.caption.clone().unwrap_or_else(|| c.name.clone());
                self.ui_base(&mut el, c, &e, Ctx::default(), &caption);
                if let Some(i) = word(c, "image") {
                    el.put("ABD_1", i);
                }
                if let Some(t) = ctext(c, "tooltip") {
                    el.put("AGD_2", t);
                }
                if let Some(s) = word(c, "showas").and_then(|s| enum_val(&[("Standard", 0), ("SplitButton", 1)], s)) {
                    el.num_bit("AGD_4", s, 10);
                }
                Self::about(&mut el, c);
                let kids = self.action_children(c, path);
                el.list("CBD_1", kids);
                el.wrap()
            }
            "actionref" => self.action_ref(c, &e),
            "separator" => {
                let mut el = El::new("SeparatorDefinition");
                self.ui_base(&mut el, c, &e, Ctx::default(), c.caption.as_deref().unwrap_or(""));
                if let Some(v) = cbool(c, "isheader") {
                    el.flag("SD_1", v, 10);
                }
                el.wrap()
            }
            "customaction" => {
                let mut el = El::new("CustomActionDefinition");
                let caption = c.caption.clone().unwrap_or_else(|| c.name.clone());
                self.ui_base(&mut el, c, &e, Ctx::default(), &caption);
                self.common_action_props(&mut el, c);
                if let Some(t) = word(c, "customactiontype").and_then(|s| enum_val(alprop::CUSTOM_ACTION_TYPES, s)) {
                    el.num("CAD0_1", t);
                }
                for (k, wire) in [("flowid", "CAD0_2"), ("flowenvironmentid", "CAD0_3"), ("flowtemplateid", "CAD0_4"), ("flowtemplatecategoryname", "CAD0_5")] {
                    if let Some(s) = ctext(c, k) {
                        el.put(wire, s);
                    }
                }
                el.wrap()
            }
            _ => return None,
        })
    }

    /// `ActionCommonPropsDefinition` members (`ACPD_*`).
    fn common_action_props(&mut self, el: &mut El, c: &Control) {
        if let Some(i) = word(c, "image") {
            el.put("ABD_1", i);
        }
        if cbool(c, "promotedisbig") == Some(true) {
            el.flag("ABD_2", true, 9);
        }
        Self::about(el, c);
        if cbool(c, "ellipsis") == Some(true) {
            el.put("ACPD_1", true);
        }
        if let Some(k) = ctext(c, "shortcutkey") {
            el.put("ACPD_2", k);
        }
        if let Some(t) = ctext(c, "tooltip") {
            el.put("ACPD_4", t);
        }
        if let Some((ty, oid, mask)) = src(c, "accessbypermission").and_then(|s| alprop::access_by_permission(self.cat, s)) {
            el.num_bit("ACPD_10", oid, 10);
            el.num_bit("ACPD_11", ty, 11);
            el.num_bit("ACPD_12", mask, 12);
        }
        if let Some(a) = word(c, "applicationarea") {
            el.str("ACPD_13", &app_area(a));
        }
        if let Some(s) = word(c, "scope").and_then(|s| enum_val(alprop::SCOPES, s)) {
            el.num_bit("ACPD_14", s, 13);
        }
        if let Some(v) = cbool(c, "infooterbar") {
            el.flag("ACPD_17", v, 14);
        }
        if cbool(c, "showcaption") == Some(false) {
            el.put("ACPD_18", false);
        }
    }

    fn action_def(&mut self, c: &Control, e: &ControlEntry) -> Value {
        let cat = self.cat;
        let mut el = El::new("ActionDefinition");
        let caption = c.caption.clone().unwrap_or_else(|| c.name.clone());
        self.ui_base(&mut el, c, e, Ctx::default(), &caption);
        self.common_action_props(&mut el, c);
        let run = src(c, "runobject").and_then(|s| alprop::run_object(cat, s));
        let run_table = run.filter(|&(rt, _)| rt == 1).and_then(|(_, id)| cat.pages.get(&id)).and_then(|p| p.source_table).and_then(|t| cat.tables.get(&t));
        if run.is_some() {
            let links = src(c, "runpagelink").map(|s| al_model::parse_conds(&alprop::lex_src(s))).unwrap_or_default();
            let fds = self.filter_defs(&links, run_table, self.table);
            if !fds.is_empty() {
                let mut ld = El::new("LinkDefinition");
                ld.list("LD_1", fds);
                el.put("AD_1", ld.wrap());
            }
            if let Some(s) = src(c, "runpageview") {
                let v = al_model::parse_table_view(&alprop::lex_src(s));
                if let Some(vd) = self.view_def(&v, run_table) {
                    el.put("AD_2", vd);
                }
            }
        }
        let has_trigger = c.triggers.contains_key("onaction")
            || ["onbeforeactionevent", "onafteractionevent"]
                .iter()
                .any(|ev| cat.subscribers.contains_key(&(al_syntax::ObjectKind::Page, self.page.id, ev.to_string(), c.name.to_ascii_lowercase())));
        if has_trigger {
            let mut t = El::new("ActionTriggerDefinition");
            t.num_bit("ATD_1", 0, 0);
            t.put("ATD_2", "OnAction");
            el.put("AD_3", t.wrap());
        }
        if cbool(c, "runpageonrec") == Some(true) {
            el.put("AD_5", true);
        }
        if let Some((rt, id)) = run {
            el.num_bit("AD_6", id, 16);
            el.poly_enum_bit("AD_8", "RunObjectType", rt, 18);
        }
        if let Some(m) = word(c, "runpagemode").and_then(|s| enum_val(alprop::RUN_PAGE_MODES, s)) {
            el.num_bit("AD_14", m, 19);
        }
        if let Some(t) = run_table {
            el.num_bit("AD_15", t.id, 20);
        }
        if run.is_some() {
            // PushActionType.RunObject: without it the web client builds no action at all
            el.num_bit("AD_17", 1, 22);
        }
        el.wrap()
    }

    fn action_ref(&mut self, c: &Control, e: &ControlEntry) -> Value {
        self.saw_actionref = true;
        let page = self.page;
        let target_name = c.target.as_deref().map(|t| t.trim_matches('"').to_string()).unwrap_or_default();
        let te = self.ix.find(Section::Actions, &target_name).cloned();
        let tc = te.as_ref().and_then(|te| self.ix.control(page, te));
        let mut el = El::new("ActionRefDefinition");
        // bc-rust guess: an action ref without a caption shows its target's caption
        let caption = c.caption.clone().or_else(|| tc.and_then(|t| t.caption.clone())).unwrap_or_else(|| c.name.clone());
        self.ui_base(&mut el, c, e, Ctx::default(), &caption);
        el.str("ARD_1", &target_name);
        if let Some(te) = &te {
            el.num_bit("ARD_2", te.id as i64, 10);
        }
        // solver: SolveApplicationArea copies the target's
        if let Some(a) = tc.and_then(|t| word(t, "applicationarea")) {
            el.str("ARD_3", &app_area(a));
        }
        el.wrap()
    }

    /// Category groups with action refs for legacy `Promoted = true` actions.
    fn legacy_promoted_groups(&mut self) -> Vec<Value> {
        let page = self.page;
        let cats: Vec<ControlEntry> = self.ix.entries.iter().filter(|e| e.section == Section::System && e.kind == "$category").cloned().collect();
        let mut out = vec![];
        for g in cats {
            let gi = self.ix.entries.iter().position(|x| x.id == g.id);
            let refs: Vec<ControlEntry> = self.ix.entries.iter().filter(|x| x.kind == "$actionref" && x.parent == gi).cloned().collect();
            let mut kids = vec![];
            for r in refs {
                let Some(target) = self.ix.control(page, &r) else { continue };
                let te = self.ix.at(Section::Actions, &r.path).cloned();
                self.saw_actionref = true;
                let mut el = El::new("ActionRefDefinition");
                el.num_bit("IDED_1", r.id as i64, 0);
                el.put("NED_1", r.name.as_str());
                el.put("NED_3", target.caption.clone().unwrap_or_else(|| target.name.clone()));
                el.put("UIED_5", r.guid.to_string());
                el.put("ARD_1", target.name.as_str());
                if let Some(te) = te {
                    el.num_bit("ARD_2", te.id as i64, 10);
                }
                if let Some(a) = word(target, "applicationarea") {
                    el.str("ARD_3", &app_area(a));
                }
                kids.push(el.wrap());
            }
            let mut el = El::new("ActionGroupDefinition");
            el.num_bit("IDED_1", g.id as i64, 0);
            el.put("NED_1", g.name.as_str());
            el.put("NED_3", legacy_category_caption(page.prop_src.get("promotedactioncategories").map(|s| s.as_str()), &g.name));
            el.put("UIED_5", g.guid.to_string());
            el.list("CBD_1", kids);
            out.push(el.wrap());
        }
        out
    }
}
