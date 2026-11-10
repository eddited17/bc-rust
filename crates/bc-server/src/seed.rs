//! First-run demo company. Master data goes through the real AL triggers
//! (Insert(true) / Validate / Modify(true)); setup rows and a few historical
//! ledger entries are written directly.

use crate::app::{flow_err, set};
use al_runtime::{RecHandle, Session, Val};
use rust_decimal::Decimal;

fn tid(s: &Session, name: &str) -> anyhow::Result<i64> {
    s.cat.table_by_name(name).map(|t| t.id).ok_or_else(|| anyhow::anyhow!("table {name} missing"))
}

fn raw(s: &mut Session, table: &str, fields: &[(&str, Val)]) -> anyhow::Result<RecHandle> {
    let r = s.new_record(tid(s, table)?, false);
    for (f, v) in fields {
        set(s, &r, f, v.clone())?;
    }
    s.rec_insert(&r, false).map_err(flow_err)?.map_err(|e| anyhow::anyhow!("{table}: {e}"))?;
    Ok(r)
}

/// Insert through triggers and validate fields like a user typing them in.
fn via_al(s: &mut Session, table: &str, fields: &[(&str, Val)]) -> anyhow::Result<RecHandle> {
    let r = s.new_record(tid(s, table)?, false);
    s.rec_insert(&r, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!("{table}: {e}"))?;
    for (f, v) in fields {
        let fi = r.borrow().tdef.field(f).ok_or_else(|| anyhow::anyhow!("no field {f}"))?;
        let fid = r.borrow().field(fi).id as i32;
        s.rec_validate(&r, fi, Some(v.clone()), fid).map_err(flow_err)?;
    }
    s.rec_modify(&r, true).map_err(flow_err)?.map_err(|e| anyhow::anyhow!("{table}: {e}"))?;
    Ok(r)
}

fn series(s: &mut Session, code: &str, desc: &str, start: &str) -> anyhow::Result<()> {
    raw(s, "No. Series", &[("Code", Val::text(code)), ("Description", Val::text(desc)), ("Default Nos.", Val::Bool(true)), ("Manual Nos.", Val::Bool(true))])?;
    raw(s, "No. Series Line", &[("Series Code", Val::text(code)), ("Line No.", Val::Int(10000)), ("Starting No.", Val::text(start)), ("Increment-by No.", Val::Int(10))])?;
    Ok(())
}

fn t(s: &str) -> Val {
    Val::text(s)
}
fn d(x: i64) -> Val {
    Val::Dec(Decimal::from(x))
}

pub fn seed(s: &mut Session) -> anyhow::Result<usize> {
    let mut n = 0usize;
    raw(s, "Company Information", &[("Name", t("CRONUS (bc-rust)")), ("Address", t("5 The Ring")), ("City", t("Atlanta"))])?;
    raw(s, "General Ledger Setup", &[("LCY Code", t("USD"))])?;
    raw(s, "Marketing Setup", &[])?;
    raw(s, "Source Code Setup", &[("Sales", t("SALES")), ("Purchases", t("PURCHASES")), ("General Journal", t("GENJNL"))])?;
    for (c, desc) in [("SALES", "Sales"), ("PURCHASES", "Purchases"), ("GENJNL", "General Journal")] {
        raw(s, "Source Code", &[("Code", t(c)), ("Description", t(desc))])?;
    }
    series(s, "CUST", "Customers", "C10000")?;
    series(s, "VEND", "Vendors", "V10000")?;
    series(s, "ITEM", "Items", "1000")?;
    series(s, "S-ORD", "Sales Orders", "S-ORD101001")?;
    series(s, "S-INV", "Sales Invoices", "S-INV102001")?;
    series(s, "S-INV+", "Posted Sales Invoices", "103001")?;
    series(s, "S-SHPT", "Posted Sales Shipments", "102001")?;
    series(s, "S-CR", "Sales Credit Memos", "S-CR104001")?;
    series(s, "S-CR+", "Posted Sales Credit Memos", "104001")?;
    series(s, "S-QUO", "Sales Quotes", "S-QUO1001")?;
    series(s, "GJNL-GEN", "General Journal", "G00001")?;
    raw(
        s,
        "Sales & Receivables Setup",
        &[
            ("Customer Nos.", t("CUST")),
            ("Order Nos.", t("S-ORD")),
            ("Invoice Nos.", t("S-INV")),
            ("Posted Invoice Nos.", t("S-INV+")),
            ("Posted Shipment Nos.", t("S-SHPT")),
            ("Credit Memo Nos.", t("S-CR")),
            ("Posted Credit Memo Nos.", t("S-CR+")),
            ("Quote Nos.", t("S-QUO")),
            // enum "Sales Line Type"::Item, as in CRONUS (new lines default to items)
            ("Document Default Line Type", Val::Opt { ord: 2, enum_id: None }),
        ],
    )?;
    raw(s, "Purchases & Payables Setup", &[("Vendor Nos.", t("VEND")), ("Document Default Line Type", Val::Opt { ord: 2, enum_id: None })])?;
    raw(s, "Inventory Setup", &[("Item Nos.", t("ITEM"))])?;
    for (c, desc) in [("US", "United States"), ("GB", "United Kingdom"), ("DE", "Germany"), ("DK", "Denmark")] {
        raw(s, "Country/Region", &[("Code", t(c)), ("Name", t(desc))])?;
    }
    for (c, desc) in [("EUR", "Euro"), ("GBP", "Pound Sterling"), ("DKK", "Danish Krone")] {
        raw(s, "Currency", &[("Code", t(c)), ("Description", t(desc))])?;
    }
    for (c, desc) in [("1M(8D)", "1 Month/2% 8 days"), ("14 DAYS", "Net 14 days"), ("30 DAYS", "Net 30 days")] {
        raw(s, "Payment Terms", &[("Code", t(c)), ("Description", t(desc))])?;
    }
    for (c, desc) in [("PCS", "Piece"), ("BOX", "Box"), ("HOUR", "Hour")] {
        raw(s, "Unit of Measure", &[("Code", t(c)), ("Description", t(desc))])?;
    }
    for (c, desc) in [("DOMESTIC", "Domestic customers and vendors"), ("FOREIGN", "Foreign customers and vendors")] {
        raw(s, "Customer Posting Group", &[("Code", t(c)), ("Description", t(desc)), ("Receivables Account", t("13100"))])?;
        raw(s, "Vendor Posting Group", &[("Code", t(c)), ("Description", t(desc))])?;
        raw(s, "Gen. Business Posting Group", &[("Code", t(c)), ("Description", t(desc))])?;
    }
    raw(s, "VAT Business Posting Group", &[("Code", t("DOMESTIC")), ("Description", t("Domestic"))])?;
    raw(s, "VAT Business Posting Group", &[("Code", t("FOREIGN")), ("Description", t("Foreign"))])?;
    raw(s, "VAT Product Posting Group", &[("Code", t("VAT25")), ("Description", t("Standard VAT (25%)"))])?;
    raw(s, "Gen. Product Posting Group", &[("Code", t("RETAIL")), ("Description", t("Retail"))])?;
    raw(s, "Inventory Posting Group", &[("Code", t("RESALE")), ("Description", t("Resale items"))])?;
    for bus in ["DOMESTIC", "FOREIGN"] {
        raw(s, "General Posting Setup", &[("Gen. Bus. Posting Group", t(bus)), ("Gen. Prod. Posting Group", t("RETAIL")), ("Sales Account", t("40100")), ("COGS Account", t("50100")), ("Inventory Adjmt. Account", t("14100")), ("Purch. Account", t("50100"))])?;
        raw(s, "VAT Posting Setup", &[("VAT Bus. Posting Group", t(bus)), ("VAT Prod. Posting Group", t("VAT25")), ("VAT %", d(if bus == "DOMESTIC" { 25 } else { 0 })), ("VAT Identifier", t("VAT25")), ("Sales VAT Account", t("20100")), ("Purchase VAT Account", t("20100"))])?;
    }
    raw(s, "General Posting Setup", &[("Gen. Bus. Posting Group", t("")), ("Gen. Prod. Posting Group", t("RETAIL")), ("Inventory Adjmt. Account", t("14100")), ("COGS Account", t("50100"))])?;
    raw(s, "Inventory Posting Setup", &[("Location Code", t("")), ("Invt. Posting Group Code", t("RESALE")), ("Inventory Account", t("14100"))])?;
    // every singleton setup table ("... Setup" with a "Primary Key" PK) gets its empty row,
    // like a freshly created BC company
    let singletons: Vec<i64> = s
        .cat
        .tables
        .values()
        .filter(|t| t.name.ends_with("Setup") && t.pk().fields.len() == 1 && t.fields[t.pk().fields[0]].name == "Primary Key" && !t.temporary_only)
        .map(|t| t.id)
        .collect();
    for id in singletons {
        let r = s.new_record(id, false);
        if s.rec_get_pk(&r).is_none() {
            s.rec_insert(&r, false).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
        }
    }
    let accounts: &[(&str, &str, i32)] = &[
        ("10000", "BALANCE SHEET", 4),
        ("10100", "Checking account", 0),
        ("10200", "Savings account", 0),
        ("13100", "Accounts Receivable", 0),
        ("14100", "Inventory", 0),
        ("20100", "Accounts Payable", 0),
        ("30000", "INCOME STATEMENT", 4),
        ("40100", "Sales, Retail", 0),
        ("50100", "Cost of Goods Sold", 0),
        ("60100", "Wages", 0),
    ];
    for (no, name, ty) in accounts {
        raw(s, "G/L Account", &[("No.", t(no)), ("Name", t(name)), ("Search Name", Val::text(name.to_uppercase())), ("Account Type", Val::Opt { ord: *ty, enum_id: None })])?;
        n += 1;
    }
    s.commit()?;

    let customers: &[(&str, &str, &str, &str)] = &[
        ("Adatum Corporation", "192 Market Square", "Atlanta", "US"),
        ("Trey Research", "153 Thomas Drive", "Chicago", "US"),
        ("School of Fine Art", "10 High Tower Green", "Miami", "US"),
        ("Alpine Ski House", "10 Deerfield Road", "London", "GB"),
        ("Relecloud", "25 Water Way", "Copenhagen", "DK"),
        ("Coho Winery", "1 Vineyard Lane", "Napa", "US"),
        ("Fabrikam, Inc.", "51 Industrial Park", "Berlin", "DE"),
        ("Litware, Inc.", "77 Software Street", "Seattle", "US"),
    ];
    let mut cust_nos = vec![];
    for (name, addr, city, cc) in customers {
        let r = via_al(s, "Customer", &[("Name", t(name)), ("Address", t(addr)), ("City", t(city)), ("Country/Region Code", t(cc))])?;
        let no = r.borrow().get_val(r.borrow().tdef.field("No.").unwrap()).to_text();
        let grp = if *cc == "US" { "DOMESTIC" } else { "FOREIGN" };
        set(s, &r, "Customer Posting Group", t(grp))?;
        set(s, &r, "Gen. Bus. Posting Group", t(grp))?;
        set(s, &r, "VAT Bus. Posting Group", t(grp))?;
        set(s, &r, "Payment Terms Code", t("30 DAYS"))?;
        s.rec_modify(&r, false).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
        cust_nos.push(no);
        n += 1;
    }
    let vendors: &[(&str, &str)] = &[("Fabrikam Supplies", "Dallas"), ("First Up Consultants", "Boston"), ("Graphic Design Institute", "Denver"), ("Wide World Importers", "Hamburg")];
    for (name, city) in vendors {
        via_al(s, "Vendor", &[("Name", t(name)), ("City", t(city))])?;
        n += 1;
    }
    s.commit()?;
    let items: &[(&str, i64, i64)] = &[("ATHENS Desk", 1000, 780), ("PARIS Guest Chair, black", 190, 125), ("ROME Guest Chair, green", 190, 125), ("TOKYO Guest Chair, blue", 190, 125), ("MEXICO Swivel Chair, black", 190, 145), ("LONDON Swivel Chair, blue", 190, 145)];
    for (desc, price, cost) in items {
        match via_al(s, "Item", &[("Description", t(desc)), ("Unit Price", d(*price)), ("Unit Cost", d(*cost))]) {
            Ok(r) => {
                let no = r.borrow().get_val(r.borrow().tdef.field("No.").unwrap()).to_text();
                raw(s, "Item Unit of Measure", &[("Item No.", t(&no)), ("Code", t("PCS")), ("Qty. per Unit of Measure", d(1))])?;
                for (f, v) in [("Base Unit of Measure", "PCS"), ("Sales Unit of Measure", "PCS"), ("Purch. Unit of Measure", "PCS"), ("Gen. Prod. Posting Group", "RETAIL"), ("VAT Prod. Posting Group", "VAT25"), ("Inventory Posting Group", "RESALE")] {
                    set(s, &r, f, t(v))?;
                }
                s.rec_modify(&r, false).map_err(flow_err)?.map_err(|e| anyhow::anyhow!(e))?;
                n += 1
            }
            Err(e) => {
                eprintln!("seed: item {desc} via AL failed ({e}); inserting directly");
                s.rollback();
                break;
            }
        }
    }
    s.commit()?;

    // historical receivables so the Balance FlowFields have something to sum
    let invoice = Val::Opt { ord: 2, enum_id: None }; // enum "Gen. Journal Document Type"::Invoice
    let mut entry = 1_000_000;
    for (i, no) in cust_nos.iter().enumerate() {
        for k in 0..(i % 3 + 1) {
            let amt = Decimal::from(((i as i64 + 3) * 1375 + k as i64 * 412) % 9000 + 250);
            let date = Val::Date(al_runtime::value::today() - 2 * (30 * k as i32 + 5));
            raw(
                s,
                "Cust. Ledger Entry",
                &[
                    ("Entry No.", Val::Int(entry)),
                    ("Customer No.", t(no)),
                    ("Posting Date", date.clone()),
                    ("Document Type", invoice.clone()),
                    ("Document No.", Val::text(format!("H{:06}", entry - 1_000_000 + 1))),
                    ("Description", Val::text(format!("Opening invoice H{:06}", entry - 1_000_000 + 1))),
                    ("Open", Val::Bool(true)),
                ],
            )?;
            raw(
                s,
                "Detailed Cust. Ledg. Entry",
                &[
                    ("Entry No.", Val::Int(entry)),
                    ("Cust. Ledger Entry No.", Val::Int(entry)),
                    ("Customer No.", t(no)),
                    ("Posting Date", date),
                    ("Entry Type", Val::Opt { ord: 1, enum_id: None }),
                    ("Document Type", invoice.clone()),
                    ("Amount", Val::Dec(amt)),
                    ("Amount (LCY)", Val::Dec(amt)),
                    ("Debit Amount", Val::Dec(amt)),
                    ("Debit Amount (LCY)", Val::Dec(amt)),
                    // the initial entry carries the ledger entry's Amount FlowField
                    ("Ledger Entry Amount", Val::Bool(true)),
                    ("Document No.", Val::text(format!("H{:06}", entry - 1_000_000 + 1))),
                ],
            )?;
            entry += 1;
        }
    }
    s.commit()?;
    Ok(n)
}
