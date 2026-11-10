//! P9: the RDLC layout model (crate::rdlc) against BC's real layouts. The Excel grid itself comes from
//! the bound items (crate::rdlc_xlsx, tests/p9_rdlc_engine_tests.rs).
use al_runtime::rdlc::{self, Item, Kind};

fn layout(rel: &str) -> rdlc::Layout {
    let p = format!("{}/../../vendor/bcapps/src/Layers/W1/BaseApp/{rel}", env!("CARGO_MANIFEST_DIR"));
    rdlc::parse(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))).unwrap()
}

fn find<'a>(items: &'a [Item], name: &str) -> Option<&'a Item> {
    for it in items {
        if it.name == name {
            return Some(it);
        }
        let found = match &it.kind {
            Kind::Rectangle(c) => find(c, name),
            Kind::Tablix(t) => t.rows.iter().flat_map(|r| r.cells.iter()).filter_map(|c| c.item.as_ref()).find_map(|i| find(std::slice::from_ref(i), name)),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

#[test]
fn archived_sales_quote_model() {
    let l = layout("Sales/Archive/ArchivedSalesQuote.rdlc");
    // the page header's addresses come from Code.GetData; the body's hidden table3 stores them
    let h = l.page_header.as_ref().expect("page header");
    let mut getdata = 0;
    fn count(items: &[Item], n: &mut usize) {
        for it in items {
            match &it.kind {
                Kind::Textbox { value, .. } if value.to_ascii_lowercase().contains("code.getdata(") => *n += 1,
                Kind::Rectangle(c) => count(c, n),
                _ => {}
            }
        }
    }
    count(&h.items, &mut getdata);
    assert!(getdata > 20, "header GetData textboxes: {getdata}");
    assert!(l.code.contains("Public Function SetData"));
    let table3 = find(&l.body.items, "table3").expect("table3");
    assert!(table3.statically_hidden());
    let vat = find(&l.body.items, "Table_VAT").expect("Table_VAT");
    assert!(vat.hidden.as_deref().unwrap_or_default().contains("VATAmountLine__VAT_Identifier_"));
    let Kind::Tablix(t) = &vat.kind else { panic!("Table_VAT is a tablix") };
    assert_eq!(t.filters.len(), 1);
}

#[test]
fn tablix_rows_have_one_cell_per_column() {
    // Job Actual To Budget's JobTask: 18 columns; spanning cells are followed by placeholders
    let l = layout("Projects/Project/Reports/JobActualToBudget.rdlc");
    let it = find(&l.body.items, "JobTask").expect("JobTask");
    let Kind::Tablix(t) = &it.kind else { panic!("JobTask is a tablix") };
    assert_eq!(t.columns.len(), 18);
    assert!(t.rows.iter().all(|r| r.cells.len() == t.columns.len()));
    let spans: Vec<usize> = t.rows.iter().flat_map(|r| r.cells.iter().map(|c| c.col_span)).filter(|s| *s > 1).collect();
    assert!(!spans.is_empty());
    for r in &t.rows {
        let mut i = 0;
        while i < r.cells.len() {
            let span = r.cells[i].col_span.max(1);
            assert!(r.cells[i + 1..i + span].iter().all(|c| c.item.is_none()), "cells under a span are placeholders");
            i += span;
        }
    }
}

#[test]
fn column_letters() {
    for (i, s) in [(0, "A"), (25, "Z"), (26, "AA"), (36, "AK"), (701, "ZZ"), (702, "AAA")] {
        assert_eq!(rdlc::column_letters(i), s);
    }
}
