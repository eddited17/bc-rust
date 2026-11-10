//! Stable numeric control ids (`IDED_1`) and GUIDs (`UIED_5`) for every element of a page.
//! The scheme is described in the module docs of `metadata`; [`ControlIndex`] is the single
//! place that assigns them, so the metadata, rpc and dataset code always agree.

use al_model::{Control, PageDef};
use std::collections::HashMap;
use uuid::Uuid;

/// Which tree of the [`PageDef`] an entry lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    /// `PageDef.layout` (`area(Content)`, `area(FactBoxes)` and everything below).
    Layout,
    /// `PageDef.actions` (`area(Processing)`, ... and everything below).
    Actions,
    /// Elements bc-rust synthesizes that have no AL counterpart: the command bar, a
    /// `Control_Promoted` area for legacy `Promoted = true` actions, legacy promoted
    /// category groups and the action refs generated for them.
    System,
}

impl Section {
    fn tag(self) -> &'static str {
        match self {
            Section::Layout => "layout",
            Section::Actions => "actions",
            Section::System => "system",
        }
    }
}

/// Fixed `ControlGUID`s the web client recognizes (`CommandBarActionHelper`).
pub const NEW_DOCUMENT_ITEMS_GUID: Uuid = Uuid::from_u128(0x1AF878F7_1EB4_4B40_A6D5_55E87F26029B);
/// Container of `area(Promoted)` (`CommandBarActionHelper.ActionRefContainerGuid`).
pub const PROMOTED_GUID: Uuid = Uuid::from_u128(0x9D7FA316_FE52_4A80_A0A2_1CEBC7325FDA);
/// "Action items" container (`CommandBarActionHelper.PartActionItemsGuid`). bc-rust uses it
/// for `area(Processing)` on every page; part pages need it so the web client finds their actions.
pub const ACTION_ITEMS_GUID: Uuid = Uuid::from_u128(0x9BFB7851_C660_4082_9115_D127551F6A6A);

/// Name of the synthetic command bar entry ([`Section::System`]).
pub const COMMAND_BAR: &str = "$CommandBar";
/// Name of the synthetic promoted area, used only when the page has legacy promoted
/// actions but no `area(Promoted)` ([`Section::System`]).
pub const LEGACY_PROMOTED_AREA: &str = "$Promoted";

/// One element of a page with its wire identity.
#[derive(Debug, Clone)]
pub struct ControlEntry {
    /// `IDED_1`. Unique within the page.
    pub id: i32,
    /// `UIED_5`.
    pub guid: Uuid,
    /// AL name as written (`"No."`, `"Control1900383207"`), or the synthetic name.
    pub name: String,
    /// AL element kind (`field`, `group`, `repeater`, `part`, `systempart`, `area`, `action`,
    /// `actionref`, `separator`, `customaction`, ...) or, for [`Section::System`] entries,
    /// `$commandbar`, `$area`, `$category`, `$actionref`.
    pub kind: String,
    pub section: Section,
    /// Index path into `PageDef.layout` / `PageDef.actions`. For legacy `$actionref` entries,
    /// the path of the promoted action in `PageDef.actions`; empty for other system entries.
    pub path: Vec<usize>,
    /// Index of the parent entry in [`ControlIndex::entries`].
    pub parent: Option<usize>,
}

/// Every element of one page, in tree pre-order (layout, then actions, then system).
#[derive(Debug, Clone)]
pub struct ControlIndex {
    pub page_id: i64,
    pub entries: Vec<ControlEntry>,
    by_id: HashMap<i32, usize>,
    by_name: HashMap<(Section, String), usize>,
    by_path: HashMap<(Section, Vec<usize>), usize>,
}

impl ControlIndex {
    pub fn build(page: &PageDef) -> ControlIndex {
        let mut ix = ControlIndex { page_id: page.id, entries: vec![], by_id: HashMap::new(), by_name: HashMap::new(), by_path: HashMap::new() };
        let mut occ: HashMap<(Section, String), u32> = HashMap::new();
        ix.walk(Section::Layout, &page.layout, &mut vec![], None, &mut occ);
        ix.walk(Section::Actions, &page.actions, &mut vec![], None, &mut occ);
        ix.add(Section::System, COMMAND_BAR, "$commandbar", vec![], None, &mut occ);
        // legacy `Promoted = true` actions become action refs in category groups
        let mut promoted = vec![];
        collect_promoted(&page.actions, &mut vec![], &mut promoted);
        if !promoted.is_empty() {
            let has_area = page.actions.iter().any(|a| a.kind == "area" && a.name.eq_ignore_ascii_case("promoted"));
            let area = if has_area { None } else { Some(ix.add(Section::System, LEGACY_PROMOTED_AREA, "$area", vec![], None, &mut occ)) };
            for (path, c) in promoted {
                let cat = legacy_category_name(c);
                let group = match ix.find(Section::System, &cat) {
                    Some(_) => ix.by_name[&(Section::System, cat.to_ascii_lowercase())],
                    None => ix.add(Section::System, &cat, "$category", vec![], area, &mut occ),
                };
                ix.add(Section::System, &format!("{}_Promoted", c.name), "$actionref", path, Some(group), &mut occ);
            }
        }
        ix
    }

    fn walk(&mut self, section: Section, tree: &[Control], path: &mut Vec<usize>, parent: Option<usize>, occ: &mut HashMap<(Section, String), u32>) {
        for (i, c) in tree.iter().enumerate() {
            path.push(i);
            let me = self.add(section, &c.name, &c.kind, path.clone(), parent, occ);
            self.walk(section, &c.children, path, Some(me), occ);
            path.pop();
        }
    }

    fn add(&mut self, section: Section, name: &str, kind: &str, path: Vec<usize>, parent: Option<usize>, occ: &mut HashMap<(Section, String), u32>) -> usize {
        let lname = name.to_ascii_lowercase();
        let n = occ.entry((section, lname.clone())).or_insert(0);
        let occurrence = *n;
        *n += 1;
        let mut salt = occurrence;
        let mut id = stable_id(section, &lname, salt);
        while self.by_id.contains_key(&id) {
            salt += 1;
            id = stable_id(section, &lname, salt);
        }
        let guid = fixed_guid(section, kind, &lname).unwrap_or_else(|| control_guid(self.page_id, section, &lname, occurrence));
        let idx = self.entries.len();
        self.entries.push(ControlEntry { id, guid, name: name.to_string(), kind: kind.to_string(), section, path: path.clone(), parent });
        self.by_id.insert(id, idx);
        self.by_name.entry((section, lname)).or_insert(idx);
        if section != Section::System {
            self.by_path.insert((section, path), idx);
        }
        idx
    }

    /// Entry with `IDED_1 == id`.
    pub fn by_id(&self, id: i32) -> Option<&ControlEntry> {
        self.by_id.get(&id).map(|&i| &self.entries[i])
    }

    /// First entry of `section` with this AL name (case-insensitive).
    pub fn find(&self, section: Section, name: &str) -> Option<&ControlEntry> {
        self.by_name.get(&(section, name.to_ascii_lowercase())).map(|&i| &self.entries[i])
    }

    pub fn id_of(&self, section: Section, name: &str) -> Option<i32> {
        self.find(section, name).map(|e| e.id)
    }

    /// Entry of the control at `path` in the layout or actions tree.
    pub fn at(&self, section: Section, path: &[usize]) -> Option<&ControlEntry> {
        self.by_path.get(&(section, path.to_vec())).map(|&i| &self.entries[i])
    }

    /// The AL control behind an entry (for `$actionref` entries, the promoted action).
    pub fn control<'a>(&self, page: &'a PageDef, e: &ControlEntry) -> Option<&'a Control> {
        let tree = match e.section {
            Section::Layout => &page.layout,
            Section::Actions | Section::System => &page.actions,
        };
        control_at(tree, &e.path)
    }
}

/// Follow an index path through a control tree.
pub fn control_at<'a>(tree: &'a [Control], path: &[usize]) -> Option<&'a Control> {
    let (first, rest) = path.split_first()?;
    let mut c = tree.get(*first)?;
    for &i in rest {
        c = c.children.get(i)?;
    }
    Some(c)
}

fn collect_promoted<'a>(tree: &'a [Control], path: &mut Vec<usize>, out: &mut Vec<(Vec<usize>, &'a Control)>) {
    for (i, c) in tree.iter().enumerate() {
        path.push(i);
        if c.kind == "action" && c.props.get("promoted").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
            out.push((path.clone(), c));
        }
        collect_promoted(&c.children, path, out);
        path.pop();
    }
}

/// `Category_Process`, `Category_Report`, `Category_Category4`, ... for a legacy promoted action.
pub(crate) fn legacy_category_name(c: &Control) -> String {
    let cat = c.props.get("promotedcategory").map(|s| s.as_str()).unwrap_or("Process");
    let lc = cat.to_ascii_lowercase();
    match lc.as_str() {
        "new" => "Category_New".into(),
        "process" => "Category_Process".into(),
        "report" => "Category_Report".into(),
        _ => match lc.strip_prefix("category").and_then(|n| n.parse::<u32>().ok()) {
            Some(n) => format!("Category_Category{n}"),
            None => format!("Category_{cat}"),
        },
    }
}

fn fixed_guid(section: Section, kind: &str, lname: &str) -> Option<Uuid> {
    match (section, kind, lname) {
        (Section::Actions, "area", "creation") => Some(NEW_DOCUMENT_ITEMS_GUID),
        (Section::Actions, "area", "processing") => Some(ACTION_ITEMS_GUID),
        (Section::Actions, "area", "promoted") => Some(PROMOTED_GUID),
        (Section::System, "$area", _) => Some(PROMOTED_GUID),
        _ => None,
    }
}

/// `IDED_1` for an element name. Names of the form `Control<digits>` (the txt2al convention
/// that keeps the original C/SIDE control id) map to that number. Every other name, and a
/// retry after a collision (`salt > 0`), maps to a 64-bit FNV-1a hash of section and
/// lower-cased name, folded into `1..=i32::MAX-1`.
pub fn stable_id(section: Section, name: &str, salt: u32) -> i32 {
    let bare = name.to_ascii_lowercase();
    let l = format!("{}/{bare}", section.tag());
    if salt == 0 {
        if let Some(d) = bare.strip_prefix("control") {
            if !d.is_empty() && d.len() <= 10 && d.bytes().all(|b| b.is_ascii_digit()) {
                if let Ok(n) = d.parse::<i32>() {
                    if n > 0 {
                        return n;
                    }
                }
            }
        }
    }
    let mut h = fnv1a(FNV_OFFSET, b"bc-rust/control-id/");
    h = fnv1a(h, l.as_bytes());
    if salt > 0 {
        h = fnv1a(h, format!("#{salt}").as_bytes());
    }
    (1 + mix(h) % (i32::MAX as u64 - 1)) as i32
}

/// `UIED_5` of an element: hash of page id, section, lower-cased name and the occurrence
/// number of that name in the section (0 unless an extension repeats a name).
pub fn control_guid(page_id: i64, section: Section, name: &str, occurrence: u32) -> Uuid {
    hashed_guid(&format!("bc-rust/control-guid/{page_id}/{}/{}/{occurrence}", section.tag(), name.to_ascii_lowercase()))
}

/// `PP_26 ControlGUID` of a page.
pub fn page_guid(page_id: i64) -> Uuid {
    hashed_guid(&format!("bc-rust/page-guid/{page_id}"))
}

/// Deterministic 128-bit name-based GUID (RFC 9562 version 8, variant 10).
pub fn hashed_guid(key: &str) -> Uuid {
    let a = mix(fnv1a(FNV_OFFSET, key.as_bytes()));
    let b = mix(fnv1a(FNV_OFFSET ^ 0x9E37_79B9_7F4A_7C15, key.as_bytes()) ^ a.rotate_left(29));
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&a.to_be_bytes());
    bytes[8..].copy_from_slice(&b.to_be_bytes());
    bytes[6] = (bytes[6] & 0x0F) | 0x80;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Uuid::from_bytes(bytes)
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv1a(mut h: u64, data: &[u8]) -> u64 {
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// splitmix64 finalizer, so nearby names spread over the whole range.
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}
