//! PageForm against the Base App on the embedded store (bc-server's demo seed): list
//! rows with expression columns, ValidateField + ModifyRecord through Microsoft's AL
//! on the Customer Card, and a table relation error. Skipped without vendor/bcapps.

use crate::dataset::{self, Cell, NavDataSet, NavRecordState, PageRequestDefinition, ReadDirection, StartingPage};
use crate::form::{FormRows, PageForm};
use crate::metadata::Section;
use al_runtime::Session;

/// `session!(s)`: a session on the shared runtime, holding the store lock shared;
/// `session!(s, writes)` holds it exclusively (see `dataset::tests::STORE`).
macro_rules! session {
    ($s:ident) => {
        let _store = crate::dataset::tests::STORE.read().unwrap_or_else(|e| e.into_inner());
        let mut $s = session!(@open);
    };
    ($s:ident, writes) => {
        let _store = crate::dataset::tests::STORE.write().unwrap_or_else(|e| e.into_inner());
        let mut $s = session!(@open);
    };
    (@open) => {
        match crate::dataset::tests::runtime() {
            Some(rt) => Session::new(rt, "CRONUS"),
            None => {
                eprintln!("vendor/bcapps missing, skipping");
                return;
            }
        }
    };
}

fn first_page(n: i32) -> PageRequestDefinition {
    PageRequestDefinition { page_size: n, read_direction: ReadDirection::Next, start_from_page: StartingPage::First, include_more_data_information: true, ..Default::default() }
}

fn open(s: &mut Session<'static>, page_id: i64) -> PageForm {
    let page = &s.cat.pages[&page_id];
    let f = PageForm::new(s, page, None, None);
    f.open(s).expect("OnInit/OnOpenPage");
    f
}

/// The card's record as the client would send it back, with `field` set to `value`.
fn client_record(s: &mut Session<'static>, f: &PageForm, field: &str, value: &str) -> NavDataSet {
    let mut ds = f.record_dataset(s).unwrap();
    let tdef = f.rec.as_ref().unwrap().borrow().tdef;
    let col = tdef.fields[tdef.field(field).unwrap()].id.to_string();
    let t = &mut ds.tables[0];
    let ci = t.column_index(&col).unwrap();
    t.rows[0][ci] = Cell::String(value.into());
    ds
}

fn control_id(f: &PageForm, name: &str) -> i32 {
    f.statics.ix.find(Section::Layout, name).unwrap_or_else(|| panic!("control {name}")).id
}

#[test]
fn list_rows_carry_expression_columns_and_all_fields() {
    session!(s);
    let f = open(&mut s, 22);
    let rec = f.rec.clone().unwrap();
    let st = NavRecordState::initial(18, 22);
    let res = dataset::read_page(&mut s, &rec, &first_page(3), &st, &mut FormRows { form: &f, after_get: true }).unwrap();
    let t = &res.dataset.tables[0];
    assert_eq!(t.rows.len(), 3);
    // every expression column of the page is a row column (the client binds by name)
    for x in &f.statics.exprs {
        assert!(t.column_index(&x.name).is_some(), "expression column {} missing", x.name);
    }
    // page variable set in OnOpenPage, read per row
    let ci = t.column_index("CRMIntegrationEnabled").expect("CRMIntegrationEnabled column");
    assert_eq!(t.rows[0][ci], Cell::Bool(false));
    // a FlowField the request did not ask for keeps its column, as DBLazy
    let due = s.cat.tables[&18].fields.iter().find(|fd| fd.name == "Balance Due (LCY)").unwrap().id.to_string();
    assert_eq!(t.get(0, &due), Some(&Cell::Lazy));
    // the Rec of the page instance is on the last row read (OnAfterGetRecord ran on it)
    assert_eq!(rec.borrow().row[0], bc_store::Value::Text("C10020".into()));
}

#[test]
fn validate_and_modify_run_the_al_triggers() {
    // modifies C10000 (rolled back at the end): no other test reads while it runs
    session!(s, writes);
    let f = open(&mut s, 21);
    // position on C10000 as OpenForm does for a bookmark
    let probe = {
        let rec = f.rec.as_ref().unwrap();
        rec.borrow_mut().row[0] = bc_store::Value::Text("C10000".into());
        let b = rec.borrow();
        dataset::encode_bookmark(&b.schema, &b.row)
    };
    assert!(f.position_on_bookmark(&mut s, &probe));

    // ValidateField on Phone No.: Rec.Validate runs the table's OnValidate, nothing is saved yet
    let ds = client_record(&mut s, &f, "Phone No.", "+1 425 555 0199");
    let c = f.control(control_id(&f, "Phone No.")).unwrap();
    assert!(f.validate_control(&mut s, &ds, c).unwrap_or_else(|e| panic!("{}", crate::wire::Fault::from(e).message().to_string())), "record is in the database");
    let tdef = f.rec.as_ref().unwrap().borrow().tdef;
    let phone = tdef.field("Phone No.").unwrap();
    assert_eq!(f.rec.as_ref().unwrap().borrow().row[phone], bc_store::Value::Text("+1 425 555 0199".into()));
    let stored = s.rec_get_pk(f.rec.as_ref().unwrap()).unwrap();
    assert_eq!(stored[phone], bc_store::Value::Text(String::new()), "ValidateField does not save");

    // ModifyRecord with the record the client got back: Modify(true), persisted
    let back = f.record_dataset(&mut s).unwrap();
    f.modify(&mut s, &back).unwrap_or_else(|e| panic!("{}", crate::wire::Fault::from(e).message().to_string()));
    let stored = s.rec_get_pk(f.rec.as_ref().unwrap()).unwrap();
    assert_eq!(stored[phone], bc_store::Value::Text("+1 425 555 0199".into()));
    s.rollback();
}

#[test]
fn table_relation_error_is_a_validate_field_fault() {
    session!(s);
    let f = open(&mut s, 21);
    let rec = f.rec.clone().unwrap();
    rec.borrow_mut().row[0] = bc_store::Value::Text("C10000".into());
    let bm = {
        let b = rec.borrow();
        dataset::encode_bookmark(&b.schema, &b.row)
    };
    assert!(f.position_on_bookmark(&mut s, &bm));
    let ds = client_record(&mut s, &f, "Country/Region Code", "ZZ");
    let c = f.control(control_id(&f, "Country/Region Code")).unwrap();
    let e = match f.validate_control(&mut s, &ds, c) {
        Ok(_) => panic!("ZZ is not a Country/Region"),
        Err(e) => e,
    };
    let fault = crate::wire::Fault::validate_field(crate::wire::Fault::from(e));
    assert!(fault.message().contains("ZZ"), "{}", fault.message());
    let chain = crate::wire::decode_exception_data(&fault.exception_data()).unwrap();
    let chain = chain.as_array().unwrap();
    assert!(chain[0]["$type"].as_str().unwrap().starts_with("Microsoft.Dynamics.Nav.Types.Exceptions.NavNSValidateFieldException,"));
    assert_eq!(chain[0]["InnerException"]["ClassName"], "Microsoft.Dynamics.Nav.Types.Exceptions.NavNCLDialogException");
    assert!(chain[0]["InnerException"].get("$type").is_none());
    assert!(chain[1]["$type"].as_str().unwrap().contains("NavNCLDialogException"));
    s.rollback();
}


/// The client's SubPageLink filters (TableView group 4) land on the page's Rec in group 4,
/// where page 1286 "Payment Rec. Match Details" reads them in OnAfterGetCurrRecord
/// (`FilterGroup(4)`, `Evaluate(StatementLineNo, GetFilter("Statement Line No."))`).
#[test]
fn state_filter_groups_reach_the_page_record() {
    session!(s);
    let rec = s.new_record(274, false);
    let state = serde_json::json!({"TableView": {"TableId": 274, "CurrentFilters": [
        {"FilterGroupNo": 0, "Filters": [{"FilterField": 6, "FilterValue": "X*", "IsExactValue": false}]},
        {"FilterGroupNo": 4, "Filters": [
            {"FilterField": 1, "FilterValue": "CHECKING", "IsExactValue": true},
            {"FilterField": 3, "FilterValue": "10000", "IsExactValue": true},
            {"FilterField": 20, "FilterValue": "1", "IsExactValue": true}]}]}});
    crate::form::apply_state_filter_groups(&s, &rec, &state);
    let r = rec.borrow();
    let line_no = r.tdef.field_no(3).unwrap();
    let f = r.filters.iter().find(|f| f.field == line_no).expect("Statement Line No. filter");
    assert_eq!((f.group, f.text.as_str()), (4, "10000"));
    assert_eq!(r.filters.iter().filter(|f| f.group == 4).count(), 3);
    // the user's group 0 stays with GetPage
    assert!(r.filters.iter().all(|f| f.group != 0));
    assert_eq!(r.filter_group, 0);
}
