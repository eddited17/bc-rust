//! System.Data DataTable / DataColumn / DataRow (and their collections), System.Type, the Nav
//! BusinessChartData container the Business Chart module fills, and the Nav SystemEnvironment.
//! DataTable.WriteXml writes .NET's schema-less DataTable XML.

use super::*;

pub struct Table {
    pub name: String,
    pub columns: Vec<NetRef>,
    pub rows: Vec<NetRef>,
}

/// System.Data.DataSet: its name (DataSetName) and tables (DataTable objects) in order.
pub struct DsData {
    pub name: String,
    pub tables: Vec<NetRef>,
    /// Nested relations (parent table, child table): the child has a `<Parent>_Id` column.
    pub relations: Vec<(String, String)>,
}

pub struct Column {
    pub name: String,
    pub caption: String,
    /// .NET type name (System.String, System.Int32, ...).
    pub dtype: String,
}

pub struct ChartData {
    pub x_dimension: String,
    pub condensed: bool,
    pub measures: Vec<(String, i64)>,
    pub table: Option<NetRef>,
}

fn col_name(c: &NetRef) -> String {
    match &*c.borrow() {
        NetObj::DataColumn(c) => c.name.clone(),
        _ => String::new(),
    }
}

fn type_name_of(v: Option<&Val>) -> String {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Prim("System.Type", t) => t.to_text(),
            _ => "System.String".into(),
        },
        Some(Val::Text(t)) => t.clone(),
        _ => "System.String".into(),
    }
}

pub fn construct(ty: &'static str, a: &Args) -> R<Val> {
    Ok(match ty {
        "System.Data.DataTable" => net(NetObj::DataTable(Table { name: if a.is_empty() { String::new() } else { a.text(0) }, columns: vec![], rows: vec![] })),
        "System.Data.DataColumn" => {
            let name = a.text(0);
            net(NetObj::DataColumn(Column { caption: name.clone(), name, dtype: if a.len() > 1 { type_name_of(a.raw(1)) } else { "System.String".into() } }))
        }
        "System.Data.DataSet" => net(NetObj::DataSet(DsData { name: if a.is_empty() { "NewDataSet".into() } else { a.text(0) }, tables: vec![], relations: vec![] })),
        "Microsoft.Dynamics.Nav.Client.BusinessChart.BusinessChartData" => net(NetObj::ChartData(ChartData { x_dimension: String::new(), condensed: false, measures: vec![], table: None })),
        _ => net(NetObj::Prim(ty, Val::Void)),
    })
}

/// DataTable.WriteXml: `<DocumentElement><Table><Col>value</Col>...</Table>...</DocumentElement>`.
pub fn write_xml(t: &Table) -> String {
    let tag = if t.name.is_empty() { "Table".to_string() } else { super::xmlapi::encode_name(&t.name) };
    let mut out = String::from("<?xml version=\"1.0\" standalone=\"yes\"?>\r\n<DocumentElement>\r\n");
    for r in &t.rows {
        out.push_str(&format!("  <{tag}>\r\n"));
        if let NetObj::DataRow(vals) = &*r.borrow() {
            for c in &t.columns {
                let name = col_name(c);
                if let Some((_, v)) = vals.iter().find(|(k, _)| *k == name) {
                    if matches!(v, Val::Void) {
                        continue;
                    }
                    let text = match v {
                        Val::Dec(d) => d.normalize().to_string(),
                        Val::Bool(b) => b.to_string(),
                        Val::DateTime(ms) => datetime::NetDateTime::from_al(*ms).format("yyyy-MM-ddTHH:mm:ss", super::culture::invariant()),
                        o => o.to_text(),
                    };
                    let en = super::xmlapi::encode_name(&name);
                    out.push_str(&format!("    <{en}>{}</{en}>\r\n", super::xml::escape_text(&text)));
                }
            }
        }
        out.push_str(&format!("  </{tag}>\r\n"));
    }
    out.push_str("</DocumentElement>");
    out
}

pub fn call(n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    if ty == "System.Type" {
        return ok(match m {
            "gettype" if !a.is_empty() => net(NetObj::Prim("System.Type", Val::Text(a.text(0)))),
            // Type.GetProperties(): the public properties of the emulated types that have a list
            "getproperties" => match &*n.borrow() {
                NetObj::Prim(_, t) => match super::x509::properties(&t.to_text()) {
                    Some(v) => v,
                    None => return Ok(Out::Unknown),
                },
                _ => return Ok(Out::Unknown),
            },
            // Type.GetField(Name): SignedXml's public static URL fields (FieldInfo.GetValue)
            "getfield" => match &*n.borrow() {
                NetObj::Prim(_, t) if t.to_text() == super::signedxml::SIGNED_XML => match super::signedxml::static_field(&a.text(0)) {
                    Some(v) => net(NetObj::Bag("System.Reflection.FieldInfo", vec![("value".into(), Val::Text(v.into())), ("name".into(), Val::Text(a.text(0)))])),
                    None => net(NetObj::Null("System.Reflection.FieldInfo")),
                },
                _ => return Ok(Out::Unknown),
            },
            "fullname" | "name" | "tostring" => match &*n.borrow() {
                NetObj::Prim(_, t) => Val::Text(if m == "name" { super::short_name(&t.to_text()).to_string() } else { t.to_text() }),
                _ => Val::Text(String::new()),
            },
            _ => return Ok(Out::Unknown),
        });
    }
    if ty == "Microsoft.Dynamics.Nav.SystemEnvironment.SystemEnvironment" {
        return ok(match m {
            "newline" => Val::Text("\r\n".into()),
            "machinename" => Val::Text("BCRUST".into()),
            _ => return Ok(Out::Unknown),
        });
    }
    let mut o = n.borrow_mut();
    match &mut *o {
        NetObj::DataSet(ds) => ok(match m {
            "tables" => {
                drop(o);
                net(NetObj::DataTables(n.clone()))
            }
            "datasetname" if a.len() == 1 => {
                ds.name = a.text(0);
                Val::Void
            }
            "datasetname" => Val::Text(ds.name.clone()),
            "clear" => {
                for t in &ds.tables {
                    if let NetObj::DataTable(t) = &mut *t.borrow_mut() {
                        t.rows.clear();
                    }
                }
                Val::Void
            }
            "acceptchanges" | "begininit" | "endinit" | "dispose" => Val::Void,
            _ => return Ok(Out::Unknown),
        }),
        NetObj::DataTables(dsref) => {
            let dsref = dsref.clone();
            drop(o);
            let db = dsref.borrow();
            let NetObj::DataSet(ds) = &*db else { return Ok(Out::Unknown) };
            let table_name = |t: &NetRef| match &*t.borrow() {
                NetObj::DataTable(t) => t.name.clone(),
                _ => String::new(),
            };
            ok(match m {
                "count" => Val::Int(ds.tables.len() as i32),
                "item" | "get_item" => {
                    let found = match a.get(0) {
                        Val::Int(_) | Val::BigInt(_) => ds.tables.get(a.int(0).max(0) as usize).cloned(),
                        v => ds.tables.iter().find(|t| table_name(t).eq_ignore_ascii_case(&v.to_text())).cloned(),
                    };
                    match found {
                        Some(t) => Val::Net(t),
                        None => {
                            drop(db);
                            return net_err("DataTableCollection", "Item", format!("Cannot find table {}.", a.get(0).to_text()));
                        }
                    }
                }
                "contains" => Val::Bool(ds.tables.iter().any(|t| table_name(t).eq_ignore_ascii_case(&a.text(0)))),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::DataTable(t) => ok(match m {
            "columns" => {
                drop(o);
                net(NetObj::DataColumns(n.clone()))
            }
            "rows" => {
                drop(o);
                net(NetObj::DataRows(n.clone()))
            }
            "newrow" => net(NetObj::DataRow(t.columns.iter().map(|c| (col_name(c), Val::Void)).collect())),
            "clear" => {
                t.rows.clear();
                Val::Void
            }
            "tablename" if a.len() == 1 => {
                t.name = a.text(0);
                Val::Void
            }
            "tablename" => Val::Text(t.name.clone()),
            "locale" if a.len() == 1 => Val::Void,
            "locale" => super::culture::culture_val(super::culture::invariant(), super::culture::View::Culture),
            "writexml" => {
                let xml = write_xml(t);
                drop(o);
                match a.raw(0) {
                    Some(Val::OutStream(tg)) => super::text::outstream_append(tg, xml.as_bytes()),
                    Some(v) => super::xmlwriter::write_text_to(v, &xml),
                    None => {}
                }
                Val::Void
            }
            "acceptchanges" | "beginloaddata" | "endloaddata" | "dispose" => Val::Void,
            _ => return Ok(Out::Unknown),
        }),
        NetObj::DataColumns(tref) => {
            let tref = tref.clone();
            drop(o);
            let mut tb = tref.borrow_mut();
            let NetObj::DataTable(t) = &mut *tb else { return Ok(Out::Unknown) };
            ok(match m {
                "clear" => {
                    t.columns.clear();
                    Val::Void
                }
                "add" => {
                    let c = match a.net(0) {
                        Some(c) if matches!(&*c.borrow(), NetObj::DataColumn(_)) => c,
                        _ => {
                            let name = a.text(0);
                            netref(NetObj::DataColumn(Column { caption: name.clone(), name, dtype: if a.len() > 1 { type_name_of(a.raw(1)) } else { "System.String".into() } }))
                        }
                    };
                    let name = col_name(&c);
                    if t.columns.iter().any(|x| col_name(x).eq_ignore_ascii_case(&name)) {
                        drop(tb);
                        return net_err("DataColumnCollection", "Add", format!("A column named '{name}' already belongs to this DataTable."));
                    }
                    t.columns.push(c.clone());
                    Val::Net(c)
                }
                "count" => Val::Int(t.columns.len() as i32),
                "item" | "get_item" => {
                    let found = match a.get(0) {
                        Val::Int(_) | Val::BigInt(_) => t.columns.get(a.int(0) as usize).cloned(),
                        v => t.columns.iter().find(|c| col_name(c).eq_ignore_ascii_case(&v.to_text())).cloned(),
                    };
                    match found {
                        Some(c) => Val::Net(c),
                        None => {
                            drop(tb);
                            return net_err("DataColumnCollection", "Item", "Cannot find column.");
                        }
                    }
                }
                "contains" => Val::Bool(t.columns.iter().any(|c| col_name(c).eq_ignore_ascii_case(&a.text(0)))),
                "indexof" => Val::Int(t.columns.iter().position(|c| col_name(c).eq_ignore_ascii_case(&a.text(0))).map(|i| i as i32).unwrap_or(-1)),
                "remove" => {
                    let k = a.text(0);
                    t.columns.retain(|c| !col_name(c).eq_ignore_ascii_case(&k));
                    Val::Void
                }
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::DataRows(tref) => {
            let tref = tref.clone();
            drop(o);
            let mut tb = tref.borrow_mut();
            let NetObj::DataTable(t) = &mut *tb else { return Ok(Out::Unknown) };
            ok(match m {
                "add" => {
                    if let Some(r) = a.net(0) {
                        t.rows.push(r);
                    }
                    Val::Void
                }
                "count" => Val::Int(t.rows.len() as i32),
                "item" | "get_item" => match t.rows.get(a.int(0) as usize) {
                    Some(r) => Val::Net(r.clone()),
                    None => {
                        drop(tb);
                        // .NET's DataRowCollection message [src:X06]
                        return net_err("DataRowCollection", "Item", format!("There is no row at position {}.", a.int(0)));
                    }
                },
                "clear" => {
                    t.rows.clear();
                    Val::Void
                }
                "removeat" => {
                    let i = a.int(0) as usize;
                    if i < t.rows.len() {
                        t.rows.remove(i);
                    }
                    Val::Void
                }
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::DataRow(vals) => ok(match m {
            "item" | "get_item" => {
                // columns by name (a row added to a table can also be indexed by position)
                let key = a.get(0);
                let name = match &key {
                    Val::Int(_) | Val::BigInt(_) => vals.get(key.as_i64() as usize).map(|(k, _)| k.clone()).unwrap_or_default(),
                    k => k.to_text(),
                };
                if a.len() == 2 {
                    let v = a.get(1);
                    match vals.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(&name)) {
                        Some(e) => e.1 = v,
                        None => vals.push((name, v)),
                    }
                    Val::Void
                } else {
                    vals.iter().find(|(k, _)| k.eq_ignore_ascii_case(&name)).map(|(_, v)| v.clone()).unwrap_or(Val::Void)
                }
            }
            "isnull" => Val::Bool(!vals.iter().any(|(k, v)| k.eq_ignore_ascii_case(&a.text(0)) && !matches!(v, Val::Void))),
            // SetParentRow(Parent): the relation column both rows have (`<Parent>_Id`) gets the
            // parent's key (a new one when the parent has none yet)
            "setparentrow" => {
                let Some(p) = a.net(0) else { return void() };
                if Rc::ptr_eq(&p, n) {
                    return void();
                }
                drop(o);
                let key = {
                    let mut pb = p.borrow_mut();
                    let NetObj::DataRow(pv) = &mut *pb else { return void() };
                    let cb = n.borrow();
                    let NetObj::DataRow(cv) = &*cb else { return void() };
                    let Some(k) = cv.iter().map(|(k, _)| k.clone()).find(|k| k.ends_with("_Id") && pv.iter().any(|(pk, _)| pk == k)) else { return void() };
                    let e = pv.iter_mut().find(|(pk, _)| *pk == k).unwrap();
                    if matches!(e.1, Val::Void) {
                        e.1 = Val::Int(next_row_id());
                    }
                    (k, e.1.clone())
                };
                if let NetObj::DataRow(cv) = &mut *n.borrow_mut() {
                    if let Some(e) = cv.iter_mut().find(|(k, _)| *k == key.0) {
                        e.1 = key.1;
                    }
                }
                Val::Void
            }
            "itemarray" => net(NetObj::Array("System.Object", vals.iter().map(|(_, v)| v.clone()).collect())),
            "delete" | "acceptchanges" => Val::Void,
            _ => return Ok(Out::Unknown),
        }),
        NetObj::DataColumn(c) => {
            let set = a.len() == 1;
            ok(match m {
                "caption" if set => {
                    c.caption = a.text(0);
                    Val::Void
                }
                "caption" => Val::Text(c.caption.clone()),
                "columnname" if set => {
                    c.name = a.text(0);
                    Val::Void
                }
                "columnname" => Val::Text(c.name.clone()),
                "datatype" if set => {
                    c.dtype = type_name_of(a.raw(0));
                    Val::Void
                }
                "datatype" => net(NetObj::Prim("System.Type", Val::Text(c.dtype.clone()))),
                "allowdbnull" | "readonly" | "unique" | "defaultvalue" | "maxlength" => Val::Void,
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::ChartData(cd) => {
            let set = a.len() == 1;
            ok(match m {
                "clearmeasures" => {
                    cd.measures.clear();
                    Val::Void
                }
                "addmeasure" => {
                    cd.measures.push((a.text(0), a.int(1)));
                    Val::Void
                }
                "showchartcondensed" if set => {
                    cd.condensed = a.bool(0);
                    Val::Void
                }
                "showchartcondensed" => Val::Bool(cd.condensed),
                "xdimension" if set => {
                    cd.x_dimension = a.text(0);
                    Val::Void
                }
                "xdimension" => Val::Text(cd.x_dimension.clone()),
                "datatable" if set => {
                    cd.table = a.net(0);
                    Val::Void
                }
                "datatable" => cd.table.clone().map(Val::Net).unwrap_or(Val::Void),
                _ => return Ok(Out::Unknown),
            })
        }
        _ => Ok(Out::Unknown),
    }
}

/// JsonConvert.SerializeObject(BusinessChartData): the chart as JSON for the control add-in.
pub fn chart_json(cd: &ChartData) -> String {
    use crate::json::{jref, write, J};
    let mut rows = vec![];
    if let Some(t) = &cd.table {
        if let NetObj::DataTable(t) = &*t.borrow() {
            for r in &t.rows {
                if let NetObj::DataRow(vals) = &*r.borrow() {
                    rows.push(jref(J::Obj(vals.iter().map(|(k, v)| (k.clone(), crate::json::from_val(v))).collect())));
                }
            }
        }
    }
    let measures = cd.measures.iter().map(|(n, t)| jref(J::Obj(vec![("Caption".into(), jref(J::Str(n.clone()))), ("MeasureType".into(), jref(J::Num((*t).into())))]))).collect();
    let o = J::Obj(vec![
        ("XDimension".into(), jref(J::Str(cd.x_dimension.clone()))),
        ("ShowChartCondensed".into(), jref(J::Bool(cd.condensed))),
        ("Measures".into(), jref(J::Arr(measures))),
        ("DataTable".into(), jref(J::Arr(rows))),
    ]);
    let mut out = String::new();
    write(&o, &mut out);
    out
}

// ------------------------------------------------------------ DataSet.ReadXml

/// A table being inferred: name, columns (name, .NET type) in order, rows as (column, value).
struct Inferred {
    name: String,
    cols: Vec<(String, &'static str)>,
    rows: Vec<Vec<(String, Val)>>,
    next_id: i32,
}

fn ensure_col(t: &mut Inferred, name: &str, ty: &'static str) {
    if !t.cols.iter().any(|(c, _)| c == name) {
        t.cols.push((name.to_string(), ty));
    }
}

/// An element is a table row when it has element children or attributes (others are columns).
fn is_table_element(el: &super::xml::Node) -> bool {
    !super::xml::element_children(el).is_empty() || super::xml::attributes(el).iter().any(|x| { let b = x.borrow(); b.prefix != "xmlns" && b.local != "xmlns" })
}

fn infer_row(tables: &mut Vec<Inferred>, el: &super::xml::Node, parent: Option<(usize, i32)>) {
    let name = el.borrow().local.clone();
    let ti = match tables.iter().position(|t| t.name == name) {
        Some(i) => i,
        None => {
            tables.push(Inferred { name: name.clone(), cols: vec![], rows: vec![], next_id: 0 });
            tables.len() - 1
        }
    };
    let mut row: Vec<(String, Val)> = vec![];
    for at in super::xml::attributes(el) {
        let (p, l, v) = {
            let b = at.borrow();
            (b.prefix.clone(), b.local.clone(), b.value.clone())
        };
        if p == "xmlns" || l == "xmlns" {
            continue;
        }
        ensure_col(&mut tables[ti], &l, "System.String");
        row.push((l, Val::Text(v)));
    }
    let key = format!("{name}_Id");
    let mut own_id: Option<i32> = None;
    for c in super::xml::element_children(el) {
        if is_table_element(&c) {
            let id = match own_id {
                Some(id) => id,
                None => {
                    let id = tables[ti].next_id;
                    tables[ti].next_id += 1;
                    own_id = Some(id);
                    id
                }
            };
            ensure_col(&mut tables[ti], &key, "System.Int32");
            infer_row(tables, &c, Some((ti, id)));
        } else {
            let col = c.borrow().local.clone();
            ensure_col(&mut tables[ti], &col, "System.String");
            row.push((col, Val::Text(super::xml::inner_text(&c))));
        }
    }
    if let Some(id) = own_id {
        row.push((key, Val::Int(id)));
    }
    if let Some((pt, pid)) = parent {
        let fk = format!("{}_Id", tables[pt].name);
        ensure_col(&mut tables[ti], &fk, "System.Int32");
        row.push((fk, Val::Int(pid)));
        let rel = (tables[pt].name.clone(), name.clone());
        if !RELS.with(|r| r.borrow().contains(&rel)) {
            RELS.with(|r| r.borrow_mut().push(rel));
        }
    }
    tables[ti].rows.push(row);
}

thread_local! {
    /// Relations found by the inference in progress (read_xml_infer collects them).
    static RELS: RefCell<Vec<(String, String)>> = const { RefCell::new(vec![]) };
    static ROW_ID: std::cell::Cell<i32> = const { std::cell::Cell::new(1_000_000) };
}

/// A fresh key for a parent row that SetParentRow relates (unique in the session).
fn next_row_id() -> i32 {
    ROW_ID.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    })
}

/// DataSet.ReadXml without a schema (.NET's inference): the document element is the DataSet when
/// its children are all tables, otherwise it is a table itself; an element with element children
/// or attributes is a table row, other elements are System.String columns in first-seen order. A
/// nested table is related to its parent: the parent gets a `<Parent>_Id` key and the child a
/// `<Parent>_Id` column (System.Int32, after the columns seen before them), as .NET adds them.
pub fn read_xml_infer(ds: &mut DsData, root: &super::xml::Node) {
    RELS.with(|r| r.borrow_mut().clear());
    let mut tables: Vec<Inferred> = vec![];
    let kids = super::xml::element_children(root);
    if !kids.is_empty() && kids.iter().all(is_table_element) && super::xml::attributes(root).is_empty() {
        ds.name = root.borrow().local.clone();
        for k in kids {
            infer_row(&mut tables, &k, None);
        }
    } else {
        infer_row(&mut tables, root, None);
    }
    for rel in RELS.with(|r| r.borrow().clone()) {
        if !ds.relations.contains(&rel) {
            ds.relations.push(rel);
        }
    }
    add_tables(ds, tables);
}

/// The inferred (or schema) tables into the DataSet: an existing table of the same name gets the
/// new columns and the rows, otherwise a new table.
fn add_tables(ds: &mut DsData, tables: Vec<Inferred>) {
    for t in tables {
        // into an existing table of the same name (columns added), or a new one
        let existing = ds.tables.iter().find(|x| matches!(&*x.borrow(), NetObj::DataTable(d) if d.name == t.name)).cloned();
        let tref = existing.unwrap_or_else(|| {
            let r = netref(NetObj::DataTable(Table { name: t.name.clone(), columns: vec![], rows: vec![] }));
            ds.tables.push(r.clone());
            r
        });
        let mut tb = tref.borrow_mut();
        let NetObj::DataTable(dt) = &mut *tb else { continue };
        for (c, ty) in &t.cols {
            if !dt.columns.iter().any(|x| col_name(x) == *c) {
                dt.columns.push(netref(NetObj::DataColumn(Column { name: c.clone(), caption: c.clone(), dtype: ty.to_string() })));
            }
        }
        let names: Vec<String> = dt.columns.iter().map(col_name).collect();
        for r in t.rows {
            let vals = names.iter().map(|c| (c.clone(), r.iter().find(|(k, _)| k == c).map(|(_, v)| v.clone()).unwrap_or(Val::Void))).collect();
            dt.rows.push(netref(NetObj::DataRow(vals)));
        }
    }
}

/// The xs:element particles of an element's complexType (through sequence / choice / all).
fn xs_particles(el: &super::xml::Node) -> Vec<super::xml::Node> {
    fn walk(n: &super::xml::Node, out: &mut Vec<super::xml::Node>) {
        for c in super::xml::element_children(n) {
            let local = c.borrow().local.clone();
            match local.as_str() {
                "element" => out.push(c),
                "sequence" | "choice" | "all" | "complexType" => walk(&c, out),
                _ => {}
            }
        }
    }
    let mut out = vec![];
    walk(el, &mut out);
    out
}

fn xs_attr(el: &super::xml::Node, name: &str) -> String {
    super::xml::get_attr(el, name, None).map(|a| a.borrow().value.clone()).unwrap_or_default()
}

fn xs_is_complex(el: &super::xml::Node) -> bool {
    super::xml::element_children(el).iter().any(|c| c.borrow().local == "complexType")
}

/// .NET type of an XSD simple type (System.String unless it is a known one).
fn xs_type(t: &str) -> &'static str {
    match t.rsplit(':').next().unwrap_or(t) {
        "int" => "System.Int32",
        "long" => "System.Int64",
        "decimal" => "System.Decimal",
        "boolean" => "System.Boolean",
        "dateTime" => "System.DateTime",
        "double" => "System.Double",
        _ => "System.String",
    }
}

fn xs_table(tables: &mut Vec<Inferred>, rels: &mut Vec<(String, String)>, el: &super::xml::Node, parent: Option<usize>) {
    let name = xs_attr(el, "name");
    let ti = match tables.iter().position(|t| t.name == name) {
        Some(i) => i,
        None => {
            tables.push(Inferred { name: name.clone(), cols: vec![], rows: vec![], next_id: 0 });
            tables.len() - 1
        }
    };
    let key = format!("{name}_Id");
    for c in xs_particles(el) {
        if xs_is_complex(&c) {
            ensure_col(&mut tables[ti], &key, "System.Int32");
            xs_table(tables, rels, &c, Some(ti));
        } else {
            let col = xs_attr(&c, "name");
            ensure_col(&mut tables[ti], &col, xs_type(&xs_attr(&c, "type")));
        }
    }
    if let Some(pt) = parent {
        let fk = format!("{}_Id", tables[pt].name);
        ensure_col(&mut tables[ti], &fk, "System.Int32");
        let rel = (tables[pt].name.clone(), name);
        if !rels.contains(&rel) {
            rels.push(rel);
        }
    }
}

/// DataSet.ReadXmlSchema: the tables, columns and nested relations of an XSD (top-level element:
/// the DataSet when all its particles are complex, else a table), keys as for ReadXml.
pub fn read_xml_schema(ds: &mut DsData, schema: &super::xml::Node) {
    let Some(root) = super::xml::element_children(schema).into_iter().find(|c| c.borrow().local == "element") else { return };
    let mut tables: Vec<Inferred> = vec![];
    let mut rels = vec![];
    let parts = xs_particles(&root);
    if !parts.is_empty() && parts.iter().all(xs_is_complex) {
        ds.name = xs_attr(&root, "name");
        for p in parts {
            xs_table(&mut tables, &mut rels, &p, None);
        }
    } else {
        xs_table(&mut tables, &mut rels, &root, None);
    }
    for rel in rels {
        if !ds.relations.contains(&rel) {
            ds.relations.push(rel);
        }
    }
    add_tables(ds, tables);
}

/// DataSet.WriteXml: the rows nested by the relations (a child row inside the parent row whose
/// `<Parent>_Id` it has), relation columns left out, null values left out.
pub fn write_dataset_xml(ds: &DsData) -> String {
    let tables: Vec<(String, Vec<String>, Vec<Vec<(String, Val)>>)> = ds
        .tables
        .iter()
        .filter_map(|t| match &*t.borrow() {
            NetObj::DataTable(d) => Some((
                d.name.clone(),
                d.columns.iter().map(col_name).collect(),
                d.rows.iter().filter_map(|r| match &*r.borrow() {
                    NetObj::DataRow(v) => Some(v.clone()),
                    _ => None,
                }).collect(),
            )),
            _ => None,
        })
        .collect();
    let is_rel_col = |c: &str| ds.relations.iter().any(|(p, _)| c == format!("{p}_Id"));
    fn emit(ds: &DsData, tables: &[(String, Vec<String>, Vec<Vec<(String, Val)>>)], ti: usize, filter: Option<(&str, &Val)>, depth: usize, out: &mut String, is_rel_col: &dyn Fn(&str) -> bool) {
        let (name, cols, rows) = &tables[ti];
        let pad = "  ".repeat(depth);
        for r in rows {
            if let Some((k, v)) = filter {
                if !r.iter().any(|(c, x)| c == k && crate::value::equals(x, v)) {
                    continue;
                }
            }
            let en = super::xmlapi::encode_name(name);
            out.push_str(&format!("{pad}<{en}>\r\n"));
            for c in cols {
                if is_rel_col(c) {
                    continue;
                }
                if let Some((_, v)) = r.iter().find(|(k, _)| k == c) {
                    if matches!(v, Val::Void) {
                        continue;
                    }
                    let cn = super::xmlapi::encode_name(c);
                    out.push_str(&format!("{pad}  <{cn}>{}</{cn}>\r\n", super::xml::escape_text(&v.to_text())));
                }
            }
            let key = format!("{name}_Id");
            if let Some((_, kv)) = r.iter().find(|(k, _)| *k == key) {
                for (_, child) in ds.relations.iter().filter(|(p, _)| p == name) {
                    if let Some(ci) = tables.iter().position(|t| &t.0 == child) {
                        emit(ds, tables, ci, Some((&key, kv)), depth + 1, out, is_rel_col);
                    }
                }
            }
            out.push_str(&format!("{pad}</{en}>\r\n"));
        }
    }
    let name = super::xmlapi::encode_name(if ds.name.is_empty() { "NewDataSet" } else { &ds.name });
    let mut out = format!("<?xml version=\"1.0\" standalone=\"yes\"?>\r\n<{name}>\r\n");
    for (ti, (tname, _, _)) in tables.iter().enumerate() {
        if ds.relations.iter().any(|(_, c)| c == tname) {
            continue;
        }
        emit(ds, &tables, ti, None, 1, &mut out, &is_rel_col);
    }
    out.push_str(&format!("</{name}>"));
    out
}

impl<'r> crate::interp::Session<'r> {
    /// DataSet.ReadXml(FileName | Stream), ReadXmlSchema(FileName | Stream) and WriteXml(Stream)
    /// on the session's files and streams.
    pub fn dataset_io(&mut self, n: &NetRef, m: &str, a: &Args) -> R<Out> {
        if m == "writexml" {
            let xml = match &*n.borrow() {
                NetObj::DataSet(ds) => write_dataset_xml(ds),
                _ => return Ok(Out::Unknown),
            };
            match a.raw(0) {
                Some(Val::Text(path)) => self.files.write(path, xml.into_bytes()),
                Some(Val::OutStream(t)) => super::text::outstream_append(t, xml.as_bytes()),
                Some(v) => super::text::stream_write(v, xml.as_bytes()),
                None => {}
            }
            return void();
        }
        match m {
            "readxml" | "readxmlschema" => {
                let bytes = match a.raw(0) {
                    Some(Val::Text(path)) => match self.files.read(path) {
                        Some(b) => b,
                        None => return net_err("DataSet", if m == "readxml" { "ReadXml" } else { "ReadXmlSchema" }, format!("Could not find file '{}'.", crate::files::full_path(path))),
                    },
                    Some(Val::InStream(s)) => {
                        let st = s.borrow();
                        st.data[st.pos.min(st.data.len())..].to_vec()
                    }
                    v => super::text::arg_bytes(v),
                };
                let text = super::text::decode(65001, &bytes);
                let text = text.trim_start_matches('\u{feff}');
                let doc = match super::xml::parse(text, &super::xml::ParseOptions { preserve_whitespace: false }) {
                    Ok(d) => d,
                    Err(e) => return net_err("DataSet", "ReadXml", e),
                };
                let Some(root) = super::xml::root_element(&doc) else { return net_err("DataSet", "ReadXml", "Root element is missing.") };
                if let NetObj::DataSet(ds) = &mut *n.borrow_mut() {
                    if m == "readxmlschema" {
                        read_xml_schema(ds, &root);
                    } else {
                        read_xml_infer(ds, &root);
                    }
                }
                ok(Val::Int(2))
            }
            _ => Ok(Out::Unknown),
        }
    }
}

#[cfg(test)]
mod dataset_tests {
    use super::*;

    fn doc_root(xml: &str) -> super::super::xml::Node {
        let d = super::super::xml::parse(xml, &super::super::xml::ParseOptions { preserve_whitespace: false }).unwrap();
        super::super::xml::root_element(&d).unwrap()
    }

    fn shape(ds: &DsData) -> Vec<(String, Vec<String>, usize)> {
        ds.tables
            .iter()
            .map(|t| match &*t.borrow() {
                NetObj::DataTable(d) => (d.name.clone(), d.columns.iter().map(col_name).collect(), d.rows.len()),
                _ => unreachable!(),
            })
            .collect()
    }

    const PACKAGE: &str = "<DataList><SalesHeaderList><TableID>36</TableID><PackageCode>P1</PackageCode><SalesHeader><DocumentType>Order</DocumentType><No>1001</No></SalesHeader><SalesHeader><DocumentType>Order</DocumentType><No>1002</No></SalesHeader></SalesHeaderList></DataList>";

    #[test]
    fn infers_nested_tables_like_dotnet() {
        let mut ds = DsData { name: "NewDataSet".into(), tables: vec![], relations: vec![] };
        read_xml_infer(&mut ds, &doc_root(PACKAGE));
        assert_eq!(ds.name, "DataList");
        let s = shape(&ds);
        assert_eq!(s[0], ("SalesHeaderList".into(), vec!["TableID".into(), "PackageCode".into(), "SalesHeaderList_Id".into()], 1));
        assert_eq!(s[1], ("SalesHeader".into(), vec!["DocumentType".into(), "No".into(), "SalesHeaderList_Id".into()], 2));
        assert_eq!(ds.relations, vec![("SalesHeaderList".to_string(), "SalesHeader".to_string())]);
        // nested WriteXml, relation columns left out, and back
        let xml = write_dataset_xml(&ds);
        assert!(xml.contains("<SalesHeaderList>\r\n    <TableID>36</TableID>\r\n    <PackageCode>P1</PackageCode>\r\n    <SalesHeader>"), "{xml}");
        assert!(!xml.contains("_Id"), "{xml}");
        let mut again = DsData { name: String::new(), tables: vec![], relations: vec![] };
        read_xml_infer(&mut again, &doc_root(&xml));
        assert_eq!(shape(&again), s);
    }

    #[test]
    fn reads_rapidstart_schema() {
        let xsd = r#"<xsd:schema xmlns:xsd="http://www.w3.org/2001/XMLSchema"><xsd:element name="DataList"><xsd:complexType><xsd:sequence><xsd:element name="SalesHeaderList"><xsd:complexType><xsd:sequence><xsd:element type="xsd:string" name="TableID"/><xsd:element type="xsd:string" name="PackageCode"/><xsd:element name="SalesHeader" maxOccurs="unbounded"><xsd:complexType><xsd:sequence><xsd:element type="xsd:string" name="DocumentType"/><xsd:element type="xsd:string" name="No"/></xsd:sequence></xsd:complexType></xsd:element></xsd:sequence></xsd:complexType></xsd:element></xsd:sequence></xsd:complexType></xsd:element></xsd:schema>"#;
        let mut ds = DsData { name: "NewDataSet".into(), tables: vec![], relations: vec![] };
        read_xml_schema(&mut ds, &doc_root(xsd));
        assert_eq!(ds.name, "DataList");
        let s = shape(&ds);
        assert_eq!(s[0].1, vec!["TableID".to_string(), "PackageCode".into(), "SalesHeaderList_Id".into()]);
        assert_eq!(s[1].1, vec!["DocumentType".to_string(), "No".into(), "SalesHeaderList_Id".into()]);
        assert_eq!(s[0].2 + s[1].2, 0);
        assert_eq!(ds.relations.len(), 1);
    }
}
