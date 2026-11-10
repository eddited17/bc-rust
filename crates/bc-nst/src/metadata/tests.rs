//! Metadata generation against the real BCApps catalog (vendor/bcapps).

use super::*;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

fn root() -> PathBuf {
    if let Ok(r) = std::env::var("BC_RUST_ROOT") {
        return PathBuf::from(r);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn cat() -> &'static Catalog {
    static CAT: OnceLock<&'static Catalog> = OnceLock::new();
    CAT.get_or_init(|| Box::leak(Box::new(Catalog::load(&al_model::default_app_dirs(&root())).expect("catalog"))))
}

const NS: &str = "Microsoft.Dynamics.Nav.Types.Metadata.";

/// Tests of the captured NST templates skip in a build without data/nst-templates/
/// (tools/nst-templates/mktemplates.py writes it; see crates/bc-nst/build.rs).
fn have_templates(test: &str) -> bool {
    let ok = actions::have_system_templates() && captured_system_part("notes").is_some() && captured_system_part("recordlinks").is_some();
    if !ok {
        eprintln!("{test}: skipped, no captured NST templates in data/nst-templates/");
    }
    ok
}

fn obj(v: &Value) -> &Value {
    assert!(v.get("$polyType").is_some(), "not a $polyType wrapper: {v}");
    &v["obj"]
}

fn ty(v: &Value) -> &str {
    v["$polyType"].as_str().unwrap().strip_prefix(NS).unwrap()
}

/// All `$polyType` objects below `v` (pre-order).
fn polys(v: &Value) -> Vec<&Value> {
    let mut out = vec![];
    fn go<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
        match v {
            Value::Object(m) => {
                if m.contains_key("$polyType") {
                    out.push(v);
                }
                for x in m.values() {
                    go(x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| go(x, out)),
            _ => {}
        }
    }
    go(v, &mut out);
    out
}

fn find<'a>(v: &'a Value, type_name: &str, name: &str) -> Option<&'a Value> {
    polys(v).into_iter().find(|p| ty(p) == type_name && obj(p)["NED_1"].as_str() == Some(name)).map(obj)
}

fn spec(o: &Value) -> u64 {
    o.get("EDSpec").and_then(|v| v.as_u64()).unwrap_or(0)
}

fn round_trip(v: &Value) {
    let s = serde_json::to_string(v).unwrap();
    let back: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(&back, v);
    assert_eq!(serde_json::to_string(&back).unwrap(), s, "key order must survive the round trip");
}

/// Wire rules that hold for every generated object.
fn check_wire(v: &Value) {
    fn go(v: &Value) {
        match v {
            Value::Null => panic!("null on the wire (NullValueHandling.Ignore)"),
            Value::Object(m) => {
                if let Some(t) = m.get("$polyType") {
                    assert!(t.as_str().unwrap().starts_with(NS), "{t}");
                    assert_eq!(m.len(), 2, "wrapper has exactly $polyType and obj");
                    match m.get("obj") {
                        Some(Value::Object(o)) => {
                            if let Some((i, _)) = o.iter().enumerate().find(|(_, (k, _))| *k == "EDSpec") {
                                assert_eq!(i, 0, "EDSpec must come first");
                            }
                        }
                        // the two enums in a [NavJsonDerivedType] hierarchy are wrapped too
                        Some(Value::Number(_)) => {
                            assert!(t.as_str().unwrap().ends_with("RunObjectType") || t.as_str().unwrap().ends_with("ActionContainerType"), "{t}")
                        }
                        other => panic!("bad obj {other:?}"),
                    }
                }
                m.values().for_each(go);
            }
            Value::Array(a) => a.iter().for_each(go),
            _ => {}
        }
    }
    go(v);
}

fn page(id: i64) -> Value {
    let v = master_page_versioned(cat(), id, 7);
    round_trip(&v);
    check_wire(&v);
    assert_eq!(ty(&v), "MasterPage");
    let o = obj(&v);
    assert_eq!(o["IDED_1"], id);
    assert_eq!(o["MP_15"], 7);
    assert_eq!(spec(o) & 0b101, 0b101, "ID and VersionNumber specified");
    // the web client dereferences these without null checks
    assert_eq!(ty(&o["MP_3"]), "ContentAreaDefinition");
    assert!(obj(&o["MP_3"])["ABD0_1"].is_array());
    assert_eq!(ty(&o["MP_8"]), "PageProperties");
    assert_eq!(ty(&obj(&o["MP_8"])["PP_1"]), "SourceObjectDefinition");
    v
}

fn props(v: &Value) -> &Value {
    obj(&obj(v)["MP_8"])
}

fn source(v: &Value) -> &Value {
    obj(&props(v)["PP_1"])
}

fn field_id(table: i64, name: &str) -> i64 {
    let t = &cat().tables[&table];
    t.fields[t.field(name).unwrap()].id
}

#[test]
fn page_22_customer_list() {
    let v = page(22);
    let pp = props(&v);
    assert_eq!(pp["PP_7"], 1);
    assert_eq!(pp["PP_18"], false);
    assert_eq!(pp["PP_21"], 21);
    assert_eq!(spec(pp) & (1 << 4), 1 << 4);
    assert_eq!(pp["PP_48"], "#Basic,#Suite,#Service");
    assert_eq!(pp["PP_49"], 1);
    assert_eq!(source(&v)["SOD_2"], 18);
    // content: one repeater with field controls bound by field number
    let content = obj(&obj(&v)["MP_3"])["ABD0_1"].as_array().unwrap();
    let rep = content.iter().find(|c| ty(c) == "RepeaterDefinition").expect("repeater");
    let cols: Vec<&str> = obj(rep)["CGBD_1"].as_array().unwrap().iter().filter_map(|c| obj(c)["CDD_11"].as_str()).collect();
    assert!(cols.contains(&"1") && cols.contains(&"2"), "{cols:?}");
    let no = find(&v, "ControlDefinition", "No.").unwrap();
    assert_eq!(no["NED_3"], "No.");
    // factboxes: page parts linked to the current row and the Links/Notes system parts
    // inside one ControlContainerPlaceHolder with ContainerType 1, as the real tier sends them
    let ph = &obj(&obj(&v)["MP_7"])["ABD0_1"][0];
    assert_eq!(ty(ph), "ControlContainerPlaceHolder");
    assert_eq!(obj(ph)["CCPH_2"], 1);
    let fb = obj(ph)["CCPH_1"].as_array().unwrap();
    assert!(fb.iter().any(|c| ty(c) == "InfopartPageDefinition" && obj(c)["IPD_1"].is_array()));
    let sys: Vec<i64> = fb.iter().filter(|c| ty(c) == "InfopartSystemDefinition").map(|c| obj(c)["ISD_1"].as_i64().unwrap_or(0)).collect();
    if captured_system_part("notes").is_some() && captured_system_part("recordlinks").is_some() {
        assert!(sys.contains(&3) && sys.contains(&1), "Links and Notes: {sys:?}");
    } else {
        // without the captured parts the web client can't build them (system_part_available)
        assert!(!sys.contains(&3) && !sys.contains(&1), "no Links/Notes without captured parts: {sys:?}");
    }
    // command bar: page actions with RunObject
    let cb = obj(&obj(&v)["MP_1"]);
    assert_eq!(cb["CBD0_3"], false);
    let run_page = polys(&obj(&v)["MP_1"]).into_iter().filter(|p| ty(p) == "ActionDefinition").map(obj).find(|a| a["AD_8"]["obj"] == 1).expect("page action");
    assert_eq!(run_page["AD_8"]["$polyType"], format!("{NS}RunObjectType"), "RunObjectType is a polymorphic enum");
    assert_eq!(run_page["AD_17"], 1);
    assert!(cat().pages.contains_key(&run_page["AD_6"].as_i64().unwrap()));
}

#[test]
fn page_21_customer_card() {
    let v = page(21);
    let pp = props(&v);
    assert!(pp.get("PP_7").is_none(), "Card is the default");
    assert_eq!(spec(pp) & 1, 1, "PageType specified");
    assert_eq!(pp["PP_23"], true);
    assert_eq!(pp["PP_17"], true, "OnAfterGetCurrRecord exists");
    assert_eq!(pp["PP_9"], true, "page uses action refs");
    let content = obj(&obj(&v)["MP_3"])["ABD0_1"].as_array().unwrap();
    let tabs: Vec<&str> = content.iter().filter(|c| ty(c) == "ControlGroupDefinition").map(|c| obj(c)["NED_3"].as_str().unwrap()).collect();
    assert!(tabs.starts_with(&["General", "Address & Contact", "Invoicing"]), "{tabs:?}");
    // No.: OnAssistEdit trigger, dynamic Visible through a page variable column
    let no = find(&v, "ControlDefinition", "No.").unwrap();
    assert_eq!(no["CDD_11"], "1");
    assert_eq!(no["CBD_7"], true);
    assert_eq!(no["CD2_26"], "#All");
    assert_eq!(no["UIED_2"], "NoFieldVisible");
    assert_eq!(no["CDD_13"], true);
    assert_eq!(no["CDD_6"], 1, "ExtendedDatatype None is written");
    assert_eq!(no["CDD_18"], "");
    let solver_bits = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5);
    assert_eq!(spec(no) & solver_bits, solver_bits);
    let trig: Vec<i64> = no["CDD_3"].as_array().unwrap().iter().map(|t| obj(t)["CTD_1"].as_i64().unwrap_or(0)).collect();
    assert_eq!(trig, vec![3]);
    let exprs = obj(&v)["MP_10"].as_array().unwrap();
    let nfv = exprs.iter().map(obj).find(|d| d["DFD_1"] == "NoFieldVisible").expect("NoFieldVisible column");
    assert_eq!(nfv["DFD_2"], 7);
    // Name: Importance = Promoted, ShowMandatory
    let name = find(&v, "ControlDefinition", "Name").unwrap();
    assert_eq!(name["CD2_17"], 1);
    assert_eq!(name["CDD_9"], "true");
    assert!(name["CD2_8"].as_str().is_some_and(|t| !t.is_empty()), "tooltip from the table field");
    // FlowField -> DrillDown and a CalcFormula related object
    let bal = find(&v, "ControlDefinition", "Balance (LCY)").unwrap();
    assert_eq!(bal["CBD_5"], true);
    assert!(bal["CDD_2"].as_array().unwrap().iter().any(|r| obj(r)["ROD_4"] == 2));
    // enum field -> DropDown and option captions
    let blocked = find(&v, "ControlDefinition", "Blocked").unwrap();
    assert_eq!(blocked["CBD_8"], true);
    assert!(blocked["CD2_6"].as_str().unwrap().contains(','));
    // non-field source -> expression column
    let bav = find(&v, "ControlDefinition", "BalanceAsVendor").unwrap();
    assert_eq!(bav["CDD_11"], "BalanceAsVendor");
    assert!(exprs.iter().map(obj).any(|d| d["DFD_1"] == "BalanceAsVendor" && d["DFD_2"] == 5));
    // table relation -> Lookup
    let pc = find(&v, "ControlDefinition", "Customer Posting Group").unwrap();
    assert_eq!(pc["CBD_6"], true);
    // triggers
    let pt: Vec<i64> = obj(&v)["MP_11"].as_array().unwrap().iter().map(|t| obj(t)["PTD_1"].as_i64().unwrap_or(0)).collect();
    assert!(pt.contains(&0) && pt.contains(&1) && pt.contains(&11), "{pt:?}");
}

#[test]
fn page_42_sales_order() {
    let v = page(42);
    let pp = props(&v);
    assert_eq!(pp["PP_7"], 5);
    let so = source(&v);
    assert_eq!(so["SOD_2"], 36);
    let view = obj(&so["SOD_1"]);
    let f = obj(&view["VD_2"][0]);
    assert_eq!(f["FD_2"], field_id(36, "Document Type"));
    assert_eq!(f["FD_3"], 1);
    assert_eq!(f["FD_4"], "Order");
    let lines = find(&v, "InfopartPageDefinition", "SalesLines").unwrap();
    assert_eq!(lines["IPD_6"], 46);
    assert_eq!(lines["IPD_12"], 1);
    assert_eq!(lines["IPD_3"], "IsSalesLinesEditable");
    let link = obj(&lines["IPD_1"][0]);
    assert_eq!(link["FD_2"], field_id(37, "Document No."));
    assert_eq!(link["FD_1"], 4, "SubPageLink filter group");
    assert_eq!(link["FD_3"], 2);
    assert_eq!(link["FD_4"], field_id(36, "No.").to_string());
}

#[test]
fn page_46_sales_order_subform() {
    let v = page(46);
    let pp = props(&v);
    assert_eq!(pp["PP_7"], 4);
    let so = source(&v);
    assert_eq!(so["SOD_2"], 37);
    assert_eq!(so["SOD_8"], true);
    assert_eq!(so["SOD_9"], true);
    assert_eq!(so["SOD_11"], true);
    assert_eq!(so["SOD_5"], false);
    let content = obj(&obj(&v)["MP_3"])["ABD0_1"].as_array().unwrap();
    let rep = content.iter().find(|c| ty(c) == "RepeaterDefinition").expect("repeater");
    let cols: Vec<&str> = obj(rep)["CGBD_1"].as_array().unwrap().iter().filter_map(|c| obj(c)["CDD_11"].as_str()).collect();
    assert!(cols.contains(&field_id(37, "Type").to_string().as_str()));
    // part actions live in the Actions container the web client looks for in parts
    let top = obj(&obj(&v)["MP_1"])["CBD0_1"].as_array().unwrap();
    assert!(top.iter().any(|g| obj(g)["UIED_5"] == ids::ACTION_ITEMS_GUID.to_string()));
}

#[test]
fn page_with_dependencies_42() {
    let v = page_with_dependencies_versioned(cat(), 42, &[], 3);
    round_trip(&v);
    let ids: Vec<i64> = v["MasterPages"].as_array().unwrap().iter().map(|e| obj(&e["MasterPage"])["IDED_1"].as_i64().unwrap()).collect();
    assert_eq!(ids[0], 42);
    assert!(ids.contains(&46), "{ids:?}");
    assert!(ids.len() > 5, "factbox pages too: {ids:?}");
    assert!(v["MasterPages"][0].get("PersonalizationId").is_some());
    let tables: Vec<i64> = v["MetaTables"].as_array().unwrap().iter().map(|t| t["Id"].as_i64().unwrap()).collect();
    assert!(tables.contains(&36) && tables.contains(&37), "{tables:?}");
    let cached = page_with_dependencies_versioned(cat(), 42, &[36], 3);
    let tables: Vec<i64> = cached["MetaTables"].as_array().unwrap().iter().map(|t| t["Id"].as_i64().unwrap()).collect();
    assert!(!tables.contains(&36) && tables.contains(&37));
    assert!(page_with_dependencies(cat(), 999_999_999, &[]).is_null());
}

fn table(id: i64) -> Value {
    let v = meta_table_versioned(cat(), id, 5);
    round_trip(&v);
    assert!(v.get("$polyType").is_none(), "MetaTable is not polymorphic");
    assert_eq!(v["Id"], id);
    assert_eq!(v["VersionNumber"], 5);
    let t = &cat().tables[&id];
    let fields = v["Fields"].as_array().unwrap();
    assert_eq!(fields.len(), t.fields.len());
    for f in fields {
        assert!(f["ClrType"].as_str().is_some_and(|s| !s.is_empty()), "{f}");
        assert_eq!(f["DataColumnName"], f["Id"].as_i64().unwrap().to_string());
        assert!(f["Type"].as_i64().unwrap() > 0);
    }
    // every key field must be a column (SetSortingKeys indexes tableFields by it)
    let ids: Vec<i64> = fields.iter().map(|f| f["Id"].as_i64().unwrap()).collect();
    for k in v["Keys"].as_array().unwrap() {
        for kf in k["FieldRelations"].as_array().unwrap() {
            assert!(ids.contains(&kf["Id"].as_i64().unwrap()));
        }
        for b in ["Enabled", "MaintainSqlIndex", "MaintainSiftIndex", "Clustered", "Unique"] {
            assert!(k[b].is_boolean(), "{b} always written");
        }
    }
    assert_eq!(v["Keys"][0]["Clustered"], true);
    assert_eq!(v["Keys"][0]["Unique"], true);
    v
}

fn mfield<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["Fields"].as_array().unwrap().iter().find(|f| f["Name"] == name).unwrap()
}

#[test]
fn table_18_customer() {
    let v = table(18);
    assert_eq!(v["IsDataPerCompany"], true);
    assert_eq!(v["LookupFormId"], 33, "Customer Lookup");
    assert_eq!(v["DrillDownFormId"], 22, "Customer List");
    let no = mfield(&v, "No.");
    assert_eq!(no["Type"], 31490);
    assert_eq!(no["Length"], 20);
    assert_eq!(no["ClrType"], "System.String");
    let blocked = mfield(&v, "Blocked");
    assert_eq!(blocked["Type"], 35584);
    assert_eq!(blocked["ClrType"], "System.Int32");
    assert_eq!(blocked["EnumTypeName"], "Customer Blocked");
    assert!(blocked["OptionString"].as_str().unwrap().contains("Ship"));
    let bal = mfield(&v, "Balance (LCY)");
    assert_eq!(bal["FieldClass"], 1);
    assert_eq!(bal["CalcFormula"]["FlowFieldType"], "Sum");
    assert!(bal["CalcFormula"]["Filters"].as_array().unwrap().iter().any(|f| f["FilterType"] == 2));
    let cpg = mfield(&v, "Customer Posting Group");
    assert_eq!(cpg["Relations"][0]["TableName"], "Customer Posting Group");
    let groups = v["FieldGroups"].as_array().unwrap();
    let dd = groups.iter().find(|g| g["Name"] == "DropDown").unwrap();
    assert_eq!(dd["FieldIds"][0], 1);
    assert_eq!(mfield(&v, "Image")["ClrType"], "Microsoft.Dynamics.Nav.Types.Media.NSMediaWrapper, Microsoft.Dynamics.Nav.Types");
}

#[test]
fn tables_36_37_sales() {
    let h = table(36);
    let dt = mfield(&h, "Document Type");
    assert_eq!(dt["Type"], 35584);
    assert!(dt["OptionString"].as_str().unwrap().starts_with("Quote,Order"));
    let l = table(37);
    // conditional relation on Sales Line."No.": one entry per branch, CONST conditions as ordinals
    let no = mfield(&l, "No.");
    let rels = no["Relations"].as_array().unwrap();
    assert!(rels.len() >= 4, "{}", rels.len());
    let item = rels.iter().find(|r| r["TableName"] == "Item").unwrap();
    let cond = &item["Conditions"][0];
    assert_eq!(cond["FieldId"], field_id(37, "Type"));
    assert!(cond["ConditionValue"].as_str().unwrap().parse::<i64>().is_ok(), "{cond}");
}

#[test]
fn control_ids_are_stable_and_unique() {
    for id in [21, 22, 42, 46] {
        let p = &cat().pages[&id];
        let a = ControlIndex::build(p);
        let b = ControlIndex::build(p);
        let mut ids = std::collections::HashSet::new();
        let mut guids = std::collections::HashSet::new();
        for (x, y) in a.entries.iter().zip(&b.entries) {
            assert_eq!((x.id, x.guid), (y.id, y.guid));
            assert!(x.id > 0);
            assert!(ids.insert(x.id), "duplicate id {} on page {id}", x.id);
            assert!(guids.insert(x.guid) || x.kind == "area", "duplicate guid on page {id}");
            assert_eq!(a.by_id(x.id).unwrap().name, x.name);
        }
    }
    let p21 = &cat().pages[&21];
    let ix = ControlIndex::build(p21);
    assert_eq!(ix.id_of(Section::Layout, "Control1900383207"), Some(1900383207));
    let e = ix.find(Section::Layout, "No.").unwrap();
    assert_eq!(ix.control(p21, e).unwrap().name, "No.");
    assert_eq!(stable_id(Section::Layout, "No.", 0), e.id);
    // the ids in the JSON are the index's ids
    let v = master_page(cat(), 21);
    assert_eq!(find(&v, "ControlDefinition", "No.").unwrap()["IDED_1"], e.id);
    assert_eq!(find(&v, "ControlDefinition", "No.").unwrap()["UIED_5"], e.guid.to_string());
}

#[test]
fn action_refs_point_at_actions() {
    for id in [21, 22, 42] {
        let v = master_page(cat(), id);
        let all = polys(&v);
        let action_ids: Vec<&Value> = all.iter().filter(|p| ty(p) == "ActionDefinition").map(|p| &obj(p)["IDED_1"]).collect();
        let refs: Vec<&Value> = all.iter().filter(|p| ty(p) == "ActionRefDefinition").map(|p| obj(p)).collect();
        assert!(!refs.is_empty());
        for r in refs {
            assert!(action_ids.contains(&&r["ARD_2"]), "page {id}: {} -> {}", r["NED_1"], r["ARD_1"]);
        }
        // Control_Promoted sits inside Control_Home on cards, lists and documents (real tier)
        if !have_templates("action_refs_point_at_actions (Control_Promoted)") {
            continue;
        }
        assert!(all.iter().any(|g| obj(g)["UIED_5"] == ids::PROMOTED_GUID.to_string() && obj(g)["NED_1"] == "Control_Promoted"));
    }
}

#[test]
fn expression_columns_match_mp10() {
    let cols = expression_columns(cat(), 21);
    let v = master_page(cat(), 21);
    let names: Vec<&str> = obj(&v)["MP_10"].as_array().unwrap().iter().map(|d| obj(d)["DFD_1"].as_str().unwrap()).collect();
    assert_eq!(cols.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), names);
    let bav = cols.iter().find(|c| c.name == "BalanceAsVendor").unwrap();
    assert_eq!(bav.kind, ExpressionKind::SourceExpression);
    assert_eq!(bav.source, "BalanceAsVendor");
}

/// Role center part 6325 (Power BI Embedded Report Part): `SelectReportsLink` is bound to a
/// label (a source expression) and has `Visible = not LockedToFirstElement`. The web client's
/// expression engine reads the synthesized `Control<id>_Visible` column from the form-state
/// row and throws unless the column is a form variable (`DFD_6`), which dropped the session
/// whenever the user scrolled the role center.
#[test]
fn property_columns_are_form_variables() {
    let v = page(6325);
    let link = find(&v, "ControlDefinition", "SelectReportsLink").expect("SelectReportsLink");
    let col = link["UIED_2"].as_str().unwrap().to_string();
    assert!(col.starts_with("Control") && col.ends_with("_Visible"), "{col}");
    let dfd = obj(&v)["MP_10"].as_array().unwrap().iter().map(obj).find(|d| d["DFD_1"] == col.as_str()).expect("expression column");
    assert_eq!(dfd["DFD_6"], true, "{dfd}");
    assert_eq!(dfd["DFD_12"], 3);
    assert_eq!(spec(dfd) & 0b1000, 0b1000, "DFD_6 specified");
    // and on every page: each synthesized property column is a form variable
    let c = cat();
    for &id in c.pages.keys() {
        let v = master_page_versioned(c, id, 1);
        for d in obj(&v)["MP_10"].as_array().into_iter().flatten().map(obj) {
            // a decimal control's format and currency columns are no form variables on the real
            // NST either (nst-real: `{"DFD_1": "Control349666880_Format", "DFD_2": 1, "DFD_12": 3}`)
            let name = d["DFD_1"].as_str().unwrap_or("");
            if d["DFD_12"] == 3 && !name.ends_with("_Format") && !name.ends_with("_CurrencyCode") {
                assert_eq!(d["DFD_6"], true, "page {id}: {d}");
            }
        }
    }
}

/// Chart of Accounts: `IndentationColumn = NameIndent`, a page variable. The client reads that
/// column from every row (tree depth) and closes the page when no column has the name.
#[test]
fn indentation_column_page_variable() {
    let v = page(16);
    let rep = polys(&v).into_iter().find(|p| ty(p) == "RepeaterDefinition").map(obj).expect("repeater");
    assert_eq!(rep["RD_4"], "NameIndent");
    let dfd = obj(&v)["MP_10"].as_array().unwrap().iter().map(obj).find(|d| d["DFD_1"] == "NameIndent").expect("NameIndent column");
    assert!(dfd["DFD_2"].is_null() || dfd["DFD_2"] == 0, "Integer (0, the default, is left out)");
    assert_eq!(dfd["DFD_6"], true);
    assert_eq!(dfd["DFD_9"], true, "varies per row");
    assert!(expression_columns(cat(), 16).iter().any(|c| c.name == "NameIndent" && c.in_repeater));
}

#[test]
fn every_page_generates() {
    let c = cat();
    let mut n = 0;
    for &id in c.pages.keys() {
        let v = master_page_versioned(c, id, 1);
        check_wire(&v);
        let ix = ControlIndex::build(&c.pages[&id]);
        let mut seen = std::collections::HashSet::new();
        for e in &ix.entries {
            assert!(seen.insert(e.id), "page {id}: duplicate control id");
        }
        n += 1;
    }
    for &id in c.tables.keys() {
        let v = meta_table_versioned(c, id, 1);
        assert!(v["Fields"].is_array());
    }
    assert!(n > 1000);
}

#[test]
fn system_parts() {
    assert!(system_part_versioned("Outlook", 2).is_null());
    if !have_templates("system_parts") {
        // bc-nst's own part: a card part around the Links control
        let v = system_part_versioned("RecordLinks", 2);
        check_wire(&v);
        assert_eq!(ty(&obj(&v)["MP_3"]["obj"]["ABD0_1"][0]), "RecordLinksControlDefinition");
        assert_eq!(obj(&v)["MP_15"], 2);
        return;
    }
    let v = system_part_versioned("RecordLinks", 2);
    check_wire(&v);
    // the real tier's part: a list on Record Link (2000000068) in a content placeholder
    let ph = &obj(&v)["MP_3"]["obj"]["ABD0_1"][0];
    assert_eq!(ty(ph), "ControlContainerPlaceHolder");
    assert_eq!(ty(&obj(ph)["CCPH_1"][0]), "RecordLinksControlDefinition");
    assert_eq!(obj(&v)["MP_15"], 2);
    assert_eq!(obj(&v)["MP_8"]["obj"]["PP_1"]["obj"]["SOD_2"], 2000000068);
    assert!(system_part_versioned("Outlook", 2).is_null());
}

#[test]
fn http_endpoints() {
    use std::io::{Read, Write};
    let st = MetadataState::new(cat(), 9);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let app = mounted_router(st);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let l = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(l, app).await.unwrap()
        });
    });
    let call = |req: String| -> (String, Value) {
        let mut s = std::net::TcpStream::connect(addr).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = String::new();
        s.read_to_string(&mut buf).unwrap();
        let (head, body) = buf.split_once("\r\n\r\n").unwrap();
        (head.to_string(), serde_json::from_str(body).unwrap_or(Value::Null))
    };
    let (head, body) = call("GET /BC/client/metadata/pages/21 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".into());
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.to_ascii_lowercase().contains("metadatatoken: 9"), "{head}");
    assert_eq!(obj(&body)["IDED_1"], 21);
    let req_body = "[18]";
    let (head, body) = call(format!(
        "POST /BC/client/metadata/pagewithdependencies/22?personalizationId= HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{req_body}",
        req_body.len()
    ));
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(obj(&body["MasterPages"][0]["MasterPage"])["IDED_1"], 22);
    assert!(body["MetaTables"].as_array().unwrap().iter().all(|t| t["Id"] != 18));
    let (head, body) = call("GET /BC/client/metadata/tables/37 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".into());
    assert!(head.starts_with("HTTP/1.1 200"));
    assert_eq!(body["Name"], "Sales Line");
    // a page id that does not exist: NavMetadataNotFoundException with HTTP 521, as the real
    // NST answers (a 404 made the web server drop the session)
    let (head, body) = call("GET /BC/client/metadata/pages/999999999 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n".into());
    assert!(head.starts_with("HTTP/1.1 521"), "{head}");
    let ex = crate::wire::decode_exception_data(body["exceptionData"].as_str().unwrap()).unwrap();
    assert_eq!(ex[0]["ClassName"], "Microsoft.Dynamics.Nav.Types.NavMetadataNotFoundException");
    assert_eq!(ex[0]["Message"], "The metadata object Page 999999999 was not found.");
    assert_eq!(ex[0]["FatalityScope"], 0);
    let req_body = "[]";
    let (head, _) = call(format!(
        "POST /BC/client/metadata/pagewithdependencies/9176 HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{req_body}",
        req_body.len()
    ));
    assert!(head.starts_with("HTTP/1.1 521"), "{head}");
}

/// Writes the generated JSON for page 21 (and its table) to crates/bc-nst/tests/golden/ so
/// it can be diffed against a capture from the real service tier. Token fixed at 1. The
/// output embeds the captured templates, so git ignores the directory, and the test writes
/// nothing without them.
#[test]
fn golden_page_21() {
    if !have_templates("golden_page_21") {
        return;
    }
    let dir = root().join("crates/bc-nst/tests/golden");
    std::fs::create_dir_all(&dir).unwrap();
    let mp = master_page_versioned(cat(), 21, 1);
    assert_eq!(mp, master_page_versioned(cat(), 21, 1), "generation is deterministic");
    let deps = page_with_dependencies_versioned(cat(), 21, &[], 1);
    let table = meta_table_versioned(cat(), 18, 1);
    let nav = master_navigation_versioned(cat(), DEFAULT_PROFILE, 1);
    let rc = master_page_versioned(cat(), 9022, 1);
    for (name, v) in [
        ("page-21.masterpage.json", &mp),
        ("page-21.pagewithdependencies.json", &deps),
        ("table-18.metatable.json", &table),
        ("navigation.business-manager.json", &nav),
        ("page-9022.masterpage.json", &rc),
    ] {
        let mut s = serde_json::to_string_pretty(v).unwrap();
        s.push('\n');
        std::fs::write(dir.join(name), s).unwrap();
    }
}

/// Shape of the real service tier's `navigation` reply (data/capture/nst-real/pre-fix).
#[test]
fn navigation_business_manager() {
    let v = master_navigation_versioned(cat(), DEFAULT_PROFILE, 4);
    round_trip(&v);
    check_wire(&v);
    assert_eq!(ty(&v), "MasterNavigation");
    let o = obj(&v);
    assert_eq!(o["MN_4"], 4);
    // the web server builds both command bars without null checks
    assert_eq!(ty(&o["MN_1"]), "CommandBarDefinition");
    assert_eq!(ty(&o["MN_2"]), "CommandBarDefinition");
    let groups = obj(&o["MN_3"])["NPD_1"].as_array().unwrap();
    let home = obj(&groups[0]);
    assert_eq!(home["NED_1"], "Control_Home");
    assert_eq!(home["UIED_5"], navigation::HOME_ITEMS_GROUP_GUID.to_string());
    assert!(home.get("NGD_1").is_none() && spec(home) & (1 << 10) != 0, "Home (0) is specified");
    let start = obj(&home["CBD_1"][0]);
    assert_eq!(start["UIED_5"], navigation::HOMEPAGE_ACTION_GUID.to_string());
    assert_eq!(start["AD_6"], 9022);
    assert_eq!(start["AD_8"]["obj"], 1);
    let items = obj(&home["CBD_1"][1]);
    assert_eq!(items["ACPH_1"]["$polyType"], format!("{NS}ActionContainerType"));
    assert_eq!(items["ACPH_1"]["obj"], 4);
    let names: Vec<&str> = items["CBD_1"].as_array().unwrap().iter().filter_map(|a| obj(a)["NED_1"].as_str()).collect();
    assert_eq!(names, ["Customers", "Vendors", "Items", "Bank Accounts", "Chart of Accounts"]);
    let activity = obj(&groups[1]);
    assert_eq!(activity["NGD_1"], 1);
    let sections = obj(&activity["CBD_1"][0]);
    assert_eq!(sections["ACPH_1"]["obj"], 5);
    assert!(sections["CBD_1"].as_array().unwrap().iter().any(|g| obj(g)["NED_3"] == "Finance"));
    assert!(master_navigation_versioned(cat(), "NO SUCH PROFILE", 4).get("$polyType").is_some(), "falls back to a profile with a role center");
}

#[test]
fn role_center_page() {
    let v = page(9022);
    let pp = props(&v);
    assert_eq!(pp["PP_7"], 2);
    let content = obj(&obj(&v)["MP_3"])["ABD0_1"].as_array().unwrap();
    let ccph = content.iter().find(|c| ty(c) == "ControlContainerPlaceHolder").expect("role center area");
    assert_eq!(obj(ccph)["CCPH_2"], 2);
    let parts: Vec<i64> = obj(ccph)["CCPH_1"].as_array().unwrap().iter().filter_map(|p| obj(p)["IPD_6"].as_i64()).collect();
    assert!(parts.starts_with(&[1440, 1310]), "{parts:?}");
    // embedding and sections go to navigation, not to the command bar
    let top: Vec<&str> = obj(&obj(&v)["MP_1"])["CBD0_1"].as_array().unwrap().iter().map(|g| obj(g)["NED_1"].as_str().unwrap()).collect();
    // the real tier's role center template (captured page 9022)
    if have_templates("role_center_page (command bar)") {
        assert_eq!(top, ["Control_Microsoft_Dynamics_NAV", "Control_Promoted", "Control_Actions", "Control_Reports", "Control_Microsoft_Dynamics_NAV_Help"]);
    }
    let deps = page_with_dependencies_versioned(cat(), 9022, &[], 4);
    assert!(deps["MasterPages"].as_array().unwrap().iter().all(|e| e["PersonalizationId"] == ""));
    assert_eq!(homepage_versioned(cat(), DEFAULT_PROFILE, 4), master_page_versioned(cat(), 9022, 4));
}

#[test]
fn command_bar_layout() {
    // card: the real tier's template (captured page 21) with the page's actions in its
    // slots; creation nests in Control_Actions behind a separator
    if !have_templates("command_bar_layout") {
        return;
    }
    let v = master_page(cat(), 21);
    let cb = obj(&obj(&v)["MP_1"]);
    assert_eq!(cb["CBD0_2"], true, "ShowQuickAccessPane (FactBox pane toggle)");
    let top = cb["CBD0_1"].as_array().unwrap();
    let names: Vec<(&str, &str)> = top.iter().map(|g| (ty(g), obj(g)["NED_1"].as_str().unwrap())).collect();
    assert_eq!(
        names,
        [
            ("NavigationGroupDefinition", "Control_Microsoft_Dynamics_NAV"),
            ("NavigationGroupDefinition", "Control_Home"),
            ("NavigationGroupDefinition", "Control_Actions"),
            ("NavigationGroupDefinition", "Control_Reports"),
            ("NavigationGroupDefinition", "Control_Related_Information"),
            ("ActionDefinition", "Control_Microsoft_Dynamics_NAV_Help"),
        ]
    );
    let home: Vec<&str> = obj(&top[1])["CBD_1"].as_array().unwrap().iter().map(|g| obj(g)["NED_1"].as_str().unwrap()).collect();
    assert_eq!(home, ["New", "Control_Manage", "Control_Promoted", "Control_View", "Control_ShowAttached", "Page"]);
    let manage: Vec<i64> = obj(&obj(&top[1])["CBD_1"][1])["CBD_1"].as_array().unwrap().iter().map(|a| obj(a)["AD_18"].as_i64().unwrap()).collect();
    assert_eq!(manage, [37, 14, 18, 12], "View, Edit, New, Delete system actions");
    let actions = obj(&top[2]);
    assert_eq!(actions["NGD_1"], 3);
    let new_doc = actions["CBD_1"].as_array().unwrap().iter().map(obj).find(|a| a["NED_1"] == "Control_New_Document").expect("New Document group");
    assert_eq!(new_doc["UIED_5"], ids::NEW_DOCUMENT_ITEMS_GUID.to_string());
    assert!(!new_doc["CBD_1"].as_array().unwrap().is_empty());
    assert_eq!(obj(&top[3])["NGD_1"], 5);
    // list part: Manage List, the page's actions, then the list rows' Edit / View / New
    let v = master_page(cat(), 46);
    let cb = obj(&obj(&v)["MP_1"]);
    let acts = cb["CBD0_1"].as_array().unwrap().iter().map(obj).find(|g| g["NED_1"] == "Control_Actions").unwrap();
    let names: Vec<&str> = acts["CBD_1"].as_array().unwrap().iter().filter_map(|a| obj(a)["NED_1"].as_str()).collect();
    assert_eq!(names.first(), Some(&"Control_Open_Full_List"));
    assert!(names.ends_with(&["Control_List", "Control_Edit", "Control_View", "Control_New", "Control_FindRow", "Control_Microsoft_Excel_By_Server"]), "{names:?}");
}
