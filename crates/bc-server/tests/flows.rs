//! End-to-end business flows through the unmodified Base App code.
//! Skipped when vendor/bcapps is not present.

use al_runtime::{Session, Val};
use bc_server::app::*;
use rust_decimal::Decimal;

fn root() -> Option<std::path::PathBuf> {
    let r = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().ok()?;
    r.join("vendor/bcapps/src/Layers/W1/BaseApp").exists().then_some(r)
}

fn count(s: &mut Session, table: &str) -> usize {
    let r = s.new_record(s.cat.table_by_name(table).unwrap().id, false);
    let mut n = 0;
    let mut ok = s.find_first(&r, false).ok().unwrap();
    while ok {
        n += 1;
        ok = s.next(&r, 1).ok().unwrap() != 0;
    }
    n
}

#[test]
fn customer_insert_and_sales_order_posting() {
    let Some(root) = root() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let app = boot(&root, Db::Memory).unwrap();
    let mut s = Session::new(&app.rt, "CRONUS");
    bc_server::seed::seed(&mut s).unwrap();

    // customer numbers come from the No. Series through OnInsert
    let c = s.new_record(18, false);
    s.rec_insert(&c, true).map_err(flow_err).unwrap().unwrap();
    assert_eq!(get(&c, "No.").to_text(), "C10080");
    let name = c.borrow().tdef.field("Name").unwrap();
    s.rec_validate(&c, name, Some(Val::text("Tailspin Toys")), 2).map_err(flow_err).unwrap();
    assert_eq!(get(&c, "Search Name").to_text(), "TAILSPIN TOYS");

    // TableRelation validation uses BC's message
    let pt = c.borrow().tdef.field("Payment Terms Code").unwrap();
    let e = s.rec_validate(&c, pt, Some(Val::text("NOPE")), 0).err().map(|e| flow_err(e).to_string()).unwrap();
    assert!(e.starts_with("The field Payment Terms Code of table Customer contains a value (NOPE) that cannot be found"), "{e}");
    s.rollback();

    // sales order: header, customer, one item line, post with codeunit 80
    let sh = s.new_record(s.cat.table_by_name("Sales Header").unwrap().id, false);
    let order = s.enum_value("Sales Document Type", "Order").map_err(flow_err).unwrap();
    set(&mut s, &sh, "Document Type", order.clone()).unwrap();
    s.rec_insert(&sh, true).map_err(flow_err).unwrap().unwrap();
    let f = sh.borrow().tdef.field("Sell-to Customer No.").unwrap();
    s.rec_validate(&sh, f, Some(Val::text("C10000")), 2).map_err(flow_err).unwrap();
    s.rec_modify(&sh, true).map_err(flow_err).unwrap().unwrap();
    let sl = s.new_record(s.cat.table_by_name("Sales Line").unwrap().id, false);
    set(&mut s, &sl, "Document Type", order).unwrap();
    set(&mut s, &sl, "Document No.", get(&sh, "No.")).unwrap();
    set(&mut s, &sl, "Line No.", Val::Int(10000)).unwrap();
    s.rec_insert(&sl, true).map_err(flow_err).unwrap().unwrap();
    for (fname, v) in [("Type", s.enum_value("Sales Line Type", "Item").map_err(flow_err).unwrap()), ("No.", Val::text("1000")), ("Quantity", Val::Int(3))] {
        let fi = sl.borrow().tdef.field(fname).unwrap();
        let fid = sl.borrow().field(fi).id as i32;
        s.rec_validate(&sl, fi, Some(v), fid).map_err(flow_err).unwrap();
    }
    s.rec_modify(&sl, true).map_err(flow_err).unwrap().unwrap();
    assert_eq!(get(&sl, "Line Amount").as_dec(), Decimal::from(3000));
    s.commit().unwrap();

    if let Some(row) = s.rec_get_pk(&sh) {
        sh.borrow_mut().row = (row).into();
    }
    set(&mut s, &sh, "Ship", Val::Bool(true)).unwrap();
    set(&mut s, &sh, "Invoice", Val::Bool(true)).unwrap();
    let cu = s.cat.id_of(al_syntax::ObjectKind::Codeunit, "Sales-Post").unwrap();
    s.run_codeunit(cu, Some(sh.clone())).map_err(flow_err).unwrap();
    s.commit().unwrap();

    assert_eq!(count(&mut s, "Sales Invoice Header"), 1);
    assert_eq!(count(&mut s, "Sales Shipment Header"), 1);
    assert_eq!(count(&mut s, "Sales Header"), 0, "posted order is deleted");
    assert_eq!(count(&mut s, "G/L Entry"), 3);
    let gl = s.new_record(s.cat.table_by_name("G/L Entry").unwrap().id, false);
    let mut ok = s.find_first(&gl, false).map_err(flow_err).unwrap();
    let mut sum = Decimal::ZERO;
    let mut receivable = Decimal::ZERO;
    while ok {
        let a = get(&gl, "Amount").as_dec();
        sum += a;
        if get(&gl, "G/L Account No.").to_text() == "13100" {
            receivable = a;
        }
        ok = s.next(&gl, 1).map_err(flow_err).unwrap() != 0;
    }
    assert_eq!(sum, Decimal::ZERO, "G/L must balance");
    assert_eq!(receivable, Decimal::from(3750), "3000 + 25% VAT");
}
