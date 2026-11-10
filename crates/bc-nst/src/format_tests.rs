//! Format columns of decimal controls (`Control<id>_Format` / `Control<id>_CurrencyCode`), as the
//! BC 29.1 service tier sends them (data/capture/nst-real: role center part 1310, Sales This
//! Month `£#,##0.;£-#,##0.`, Average Collection Days `#,##0.0`). Skipped without vendor/bcapps.

use crate::dataset::Cell;
use crate::form::PageForm;
use al_runtime::Session;
use serde_json::Value;

fn polys(v: &Value) -> Vec<&Value> {
    let mut out = vec![];
    fn go<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
        match v {
            Value::Object(m) => {
                if m.contains_key("$polyType") {
                    out.push(v);
                }
                m.values().for_each(|x| go(x, out));
            }
            Value::Array(a) => a.iter().for_each(|x| go(x, out)),
            _ => {}
        }
    }
    go(v, &mut out);
    out
}

fn control<'a>(v: &'a Value, name: &str) -> &'a Value {
    polys(v)
        .into_iter()
        .find(|p| p["$polyType"].as_str().is_some_and(|t| t.ends_with(".ControlDefinition")) && p["obj"]["NED_1"].as_str() == Some(name))
        .map(|p| &p["obj"])
        .unwrap_or_else(|| panic!("control {name}"))
}

fn column<'a>(v: &'a Value, name: &str) -> &'a Value {
    v["obj"]["MP_10"].as_array().unwrap().iter().map(|d| &d["obj"]).find(|d| d["DFD_1"].as_str() == Some(name)).unwrap_or_else(|| panic!("MP_10 column {name}"))
}

#[test]
fn decimal_controls_have_format_and_currency_columns() {
    let Some(rt) = crate::dataset::tests::runtime() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let v = crate::metadata::master_page_versioned(rt.cat, 1310, 1);
    let c = control(&v, "Sales This Month");
    let id = c["IDED_1"].as_i64().unwrap();
    assert_eq!(c["CD2_2"], format!("Control{id}_Format"));
    assert_eq!(c["CD2_3"], format!("Control{id}_CurrencyCode"));
    // the real NST: {"DFD_1": "Control349666880_Format", "DFD_2": 1, "DFD_8": "Control349666880_Format", "DFD_12": 3}
    let f = column(&v, &format!("Control{id}_Format"));
    assert_eq!(f["DFD_2"], 1, "{f}");
    assert_eq!(f["DFD_8"], format!("Control{id}_Format"));
    assert_eq!(f["DFD_12"], 3);
    assert!(f.get("DFD_6").is_none(), "{f}");
    // {"DFD_1": "Control349666880_CurrencyCode", "DFD_2": 21, "DFD_12": 3}
    let k = column(&v, &format!("Control{id}_CurrencyCode"));
    assert_eq!(k["DFD_2"], 21, "{k}");
    assert_eq!(k["DFD_12"], 3);
    assert!(k.get("DFD_8").is_none() && k.get("DFD_6").is_none(), "{k}");
    // an Integer cue has none
    let q = control(&v, "Ongoing Sales Quotes");
    assert!(q.get("CD2_2").is_none() && q.get("CD2_3").is_none(), "{q}");
}

/// The cells of the page's format columns by control name.
fn format_cells(s: &mut Session<'static>, f: &PageForm) -> Vec<(String, Cell)> {
    let cells = f.expression_cells(s);
    f.statics
        .exprs
        .iter()
        .zip(cells)
        .filter(|(x, _)| x.fmt.is_some())
        .map(|(x, c)| {
            let id: i32 = x.name.trim_start_matches("Control").trim_end_matches("_Format").parse().unwrap();
            (f.control(id).map(|c| c.name.clone()).unwrap_or_default(), c)
        })
        .collect()
}

fn cell_of<'a>(v: &'a [(String, Cell)], name: &str) -> &'a str {
    match v.iter().find(|(n, _)| n == name).map(|(_, c)| c) {
        Some(Cell::String(s)) => s,
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn format_columns_follow_autoformat_and_decimal_places() {
    let _store = crate::dataset::tests::STORE.write().unwrap_or_else(|e| e.into_inner());
    let Some(rt) = crate::dataset::tests::runtime() else {
        eprintln!("vendor/bcapps missing, skipping");
        return;
    };
    let mut s = Session::new(rt, "CRONUS");
    // activities part: AutoFormatType 11 with GetAmountFormat() (DecimalPlaces 0:0), and
    // DecimalPlaces 1:1 alone
    let page = &s.cat.pages[&1310];
    let f = PageForm::new(&mut s, page, None, None);
    f.open(&mut s).expect("OnOpenPage");
    let v = format_cells(&mut s, &f);
    let sales = cell_of(&v, "Sales This Month");
    assert!(sales.ends_with("#,##0") || sales.contains("#,##0.;"), "no decimals: {sales}");
    assert_eq!(cell_of(&v, "Average Collection Days"), "#,##0.0");
    s.rollback();
    // customer list: Balance (LCY) is an amount (AutoFormatType 1) in LCY
    let page = &s.cat.pages[&22];
    let f = PageForm::new(&mut s, page, None, None);
    f.open(&mut s).expect("OnOpenPage");
    let v = format_cells(&mut s, &f);
    assert_eq!(cell_of(&v, "Balance (LCY)"), "#,##0.00");
    s.rollback();
}
