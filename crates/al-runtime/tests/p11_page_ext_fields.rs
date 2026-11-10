//! A base page's control bound to a field that a table extension adds (BC 29's Base Application
//! moved manufacturing fields such as Item Journal Line "Output Quantity" into tableextensions, and
//! page Production Journal shows them) is bound to that table field. Pages load before table
//! extensions, so the control used to have no field: TestPage SetValue then assigned the value
//! without the field's OnValidate.

use al_model::{Catalog, Control};

const APP: &str = r#"
table 50795 "P11 Line"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
    keys
    {
        key(PK; "No.") { Clustered = true; }
    }
}

tableextension 50795 "P11 Line Ext" extends "P11 Line"
{
    fields
    {
        field(50; "Ext Qty"; Decimal) { }
    }
}

page 50795 "P11 Lines"
{
    PageType = List;
    SourceTable = "P11 Line";

    layout
    {
        area(Content)
        {
            repeater(Lines)
            {
                field("No."; Rec."No.") { }
                field("Ext Qty"; Rec."Ext Qty") { }
            }
        }
    }
}
"#;

fn find<'a>(cs: &'a [Control], name: &str) -> Option<&'a Control> {
    for c in cs {
        if c.name == name {
            return Some(c);
        }
        if let Some(x) = find(&c.children, name) {
            return Some(x);
        }
    }
    None
}

#[test]
fn base_page_binds_table_extension_fields() {
    // one fixed directory, the file replaced atomically: repeated and concurrent runs leave one small
    // directory behind instead of one per run (nothing is deleted)
    let dir = std::env::temp_dir().join("p11-pagext-test");
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = dir.join(format!("App.al.{}.{:?}.part", std::process::id(), std::thread::current().id()));
    std::fs::write(&tmp, APP).unwrap();
    std::fs::rename(&tmp, dir.join("App.al")).unwrap();
    let cat = Catalog::load(&[dir.clone()]).expect("catalog");
    assert_eq!(cat.stats.parse_errors, 0, "{:?}", cat.warnings);
    let page = cat.pages.get(&50795).expect("page");
    let t = cat.tables.get(&50795).expect("table");
    let no = find(&page.layout, "No.").expect("No. control");
    let ext = find(&page.layout, "Ext Qty").expect("Ext Qty control");
    assert_eq!(no.field, t.field("No."));
    assert!(t.field("Ext Qty").is_some());
    assert_eq!(ext.field, t.field("Ext Qty"), "a control on a table-extension field is bound to it");
}
