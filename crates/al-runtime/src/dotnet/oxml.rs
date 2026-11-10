//! OpenXml SDK elements and parts that BaseApp builds and reads by hand around the Nav spreadsheet
//! writers and readers (RapidStart Excel exchange, OpenXML Management): XML maps (MapInfo, Schema,
//! Map, DataBinding), connections, table definition parts (Table, TableColumns, TableColumn,
//! XmlColumnProperties, AutoFilter, TableStyleInfo), single-cell XML maps (SingleXmlCells,
//! SingleXmlCell, XmlCellProperties, XmlProperties) and the worksheet's TableParts.
//!
//! An SDK element is a dotnet::xml element in the SpreadsheetML namespace. Each class maps its
//! properties to the ECMA-376 attribute or child element behind them (Table.Reference -> `ref`,
//! TableColumn.XmlColumnProperties -> `xmlColumnPr`, ...); value wrappers (StringValue,
//! UInt32Value, BooleanValue, ByteValue, EnumValue) are typed primitives with `.Value`. Parts hold
//! their root element. The writer (xlsx::WBook) writes the parts into the package with their
//! relationships and content types; the reader (xlsx::Book) parses them back from the package.
//! Other SDK objects stay the neutral sink (xlsx::sink).

use super::xlsx::{Book, WRef};
use super::xml::{self as dom, Node};
use super::*;

pub const MAIN_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
pub const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// An SDK class: full name, element local name, (property, attribute) pairs, (property, child
/// class short name) pairs. Property names are lower-case (member names are matched that way).
struct Class {
    name: &'static str,
    local: &'static str,
    attrs: &'static [(&'static str, &'static str)],
    kids: &'static [(&'static str, &'static str)],
}

static CLASSES: &[Class] = &[
    Class {
        name: "DocumentFormat.OpenXml.Spreadsheet.Table",
        local: "table",
        attrs: &[("id", "id"), ("name", "name"), ("displayname", "displayName"), ("reference", "ref"), ("totalsrowshown", "totalsRowShown"), ("connectionid", "connectionId"), ("tabletype", "tableType"), ("headerrowcount", "headerRowCount")],
        kids: &[("autofilter", "AutoFilter"), ("tablecolumns", "TableColumns"), ("tablestyleinfo", "TableStyleInfo")],
    },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.TableColumns", local: "tableColumns", attrs: &[("count", "count")], kids: &[] },
    Class {
        name: "DocumentFormat.OpenXml.Spreadsheet.TableColumn",
        local: "tableColumn",
        attrs: &[("id", "id"), ("name", "name"), ("uniquename", "uniqueName"), ("dataformatid", "dataDxfId")],
        kids: &[("xmlcolumnproperties", "XmlColumnProperties")],
    },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.XmlColumnProperties", local: "xmlColumnPr", attrs: &[("mapid", "mapId"), ("xpath", "xpath"), ("xmldatatype", "xmlDataType"), ("denormalized", "denormalized")], kids: &[] },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.AutoFilter", local: "autoFilter", attrs: &[("reference", "ref")], kids: &[] },
    Class {
        name: "DocumentFormat.OpenXml.Spreadsheet.TableStyleInfo",
        local: "tableStyleInfo",
        attrs: &[("name", "name"), ("showfirstcolumn", "showFirstColumn"), ("showlastcolumn", "showLastColumn"), ("showrowstripes", "showRowStripes"), ("showcolumnstripes", "showColumnStripes")],
        kids: &[],
    },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.SingleXmlCells", local: "singleXmlCells", attrs: &[], kids: &[] },
    Class {
        name: "DocumentFormat.OpenXml.Spreadsheet.SingleXmlCell",
        local: "singleXmlCell",
        attrs: &[("id", "id"), ("cellreference", "r"), ("connectionid", "connectionId")],
        kids: &[("xmlcellproperties", "XmlCellProperties")],
    },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.XmlCellProperties", local: "xmlCellPr", attrs: &[("id", "id"), ("uniquename", "uniqueName")], kids: &[("xmlproperties", "XmlProperties")] },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.XmlProperties", local: "xmlPr", attrs: &[("mapid", "mapId"), ("xpath", "xpath"), ("xmldatatype", "xmlDataType")], kids: &[] },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.MapInfo", local: "MapInfo", attrs: &[("selectionnamespaces", "SelectionNamespaces")], kids: &[] },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.Schema", local: "Schema", attrs: &[("id", "ID"), ("schemareference", "SchemaRef"), ("namespace", "Namespace")], kids: &[] },
    Class {
        name: "DocumentFormat.OpenXml.Spreadsheet.Map",
        local: "Map",
        attrs: &[
            ("id", "ID"),
            ("name", "Name"),
            ("rootelement", "RootElement"),
            ("schemaid", "SchemaID"),
            ("showimportexporterrors", "ShowImportExportValidationErrors"),
            ("autofit", "AutoFit"),
            ("appenddata", "Append"),
            ("preserveautofilterstate", "PreserveSortAFLayout"),
            ("preserveformat", "PreserveFormat"),
        ],
        kids: &[("databinding", "DataBinding")],
    },
    Class {
        name: "DocumentFormat.OpenXml.Spreadsheet.DataBinding",
        local: "DataBinding",
        attrs: &[("databindingname", "DataBindingName"), ("filebinding", "FileBinding"), ("connectionid", "ConnectionID"), ("filebindingname", "FileBindingName"), ("databindingloadmode", "DataBindingLoadMode")],
        kids: &[],
    },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.Connections", local: "connections", attrs: &[], kids: &[] },
    Class {
        name: "DocumentFormat.OpenXml.Spreadsheet.Connection",
        local: "connection",
        attrs: &[("id", "id"), ("name", "name"), ("type", "type"), ("background", "background"), ("refreshedversion", "refreshedVersion")],
        kids: &[("webqueryproperties", "WebQueryProperties")],
    },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.WebQueryProperties", local: "webPr", attrs: &[("xmlsource", "xml"), ("sourcedata", "sourceData"), ("url", "url"), ("htmltables", "htmlTables")], kids: &[] },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.TableParts", local: "tableParts", attrs: &[("count", "count")], kids: &[] },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.TablePart", local: "tablePart", attrs: &[], kids: &[] },
    Class { name: "DocumentFormat.OpenXml.Spreadsheet.Sheets", local: "sheets", attrs: &[], kids: &[] },
];

/// An element whose class isn't listed (e.g. the xs:schema inside Schema).
const UNKNOWN: &str = "DocumentFormat.OpenXml.Spreadsheet.OpenXmlUnknownElement";

const VALUE_TYPES: &[&str] = &[
    "DocumentFormat.OpenXml.StringValue",
    "DocumentFormat.OpenXml.UInt32Value",
    "DocumentFormat.OpenXml.Int32Value",
    "DocumentFormat.OpenXml.BooleanValue",
    "DocumentFormat.OpenXml.ByteValue",
    "DocumentFormat.OpenXml.EnumValue`1",
];

pub fn is_value_type(t: &str) -> bool {
    VALUE_TYPES.contains(&t)
}

fn class_by_name(name: &str) -> Option<&'static Class> {
    CLASSES.iter().find(|c| c.name == name || c.name.rsplit('.').next() == Some(name))
}

fn class_by_local(local: &str) -> &'static str {
    CLASSES.iter().find(|c| c.local == local).map(|c| c.name).unwrap_or(UNKNOWN)
}

pub type PartRef = Rc<RefCell<PartData>>;

/// A package part: its SDK class, root element and relationship id.
pub struct PartData {
    pub kind: &'static str,
    pub root: Option<Node>,
    pub rid: String,
}

pub const TABLE_PART: &str = "DocumentFormat.OpenXml.Packaging.TableDefinitionPart";
pub const MAPS_PART: &str = "DocumentFormat.OpenXml.Packaging.CustomXmlMappingsPart";
pub const CONNECTIONS_PART: &str = "DocumentFormat.OpenXml.Packaging.ConnectionsPart";
pub const SINGLE_CELLS_PART: &str = "DocumentFormat.OpenXml.Packaging.SingleCellTablePart";

/// The root property of each part class.
fn part_root_prop(kind: &str) -> &'static str {
    match kind {
        TABLE_PART => "table",
        MAPS_PART => "mapinfo",
        CONNECTIONS_PART => "connections",
        _ => "singlexmlcells",
    }
}

pub fn new_part(kind: &'static str, rid: String) -> PartRef {
    Rc::new(RefCell::new(PartData { kind, root: None, rid }))
}

/// OpenXml objects: elements, parts, and the Workbook / WorkbookPart / Worksheet / WorksheetPart of
/// a Nav writer (W*) or reader (R*).
pub enum Ox {
    El(&'static str, Node),
    Part(PartRef),
    WBook(WRef),
    WBookPart(WRef),
    WSheet(WRef, usize),
    WSheetPart(WRef, usize),
    RBook(Rc<Book>),
    RBookPart(Rc<Book>),
    RSheet(Rc<Book>, usize),
    RSheetPart(Rc<Book>, usize),
}

pub fn type_name(o: &Ox) -> &'static str {
    match o {
        Ox::El(c, _) => *c,
        Ox::Part(p) => p.borrow().kind,
        Ox::WBook(_) | Ox::RBook(_) => "DocumentFormat.OpenXml.Spreadsheet.Workbook",
        Ox::WBookPart(_) | Ox::RBookPart(_) => "DocumentFormat.OpenXml.Packaging.WorkbookPart",
        Ox::WSheet(..) | Ox::RSheet(..) => "DocumentFormat.OpenXml.Spreadsheet.Worksheet",
        Ox::WSheetPart(..) | Ox::RSheetPart(..) => "DocumentFormat.OpenXml.Packaging.WorksheetPart",
    }
}

pub fn ox(o: Ox) -> Val {
    net(NetObj::Ox(o))
}

/// A new element of an SDK class (main namespace).
pub fn new_el(class: &'static str) -> Val {
    let local = class_by_name(class).map(|c| c.local).unwrap_or("unknown");
    ox(Ox::El(class_by_name(class).map(|c| c.name).unwrap_or(UNKNOWN), dom::element("", local, MAIN_NS)))
}

fn el_val(n: &Node) -> Val {
    let local = n.borrow().local.clone();
    ox(Ox::El(class_by_local(&local), n.clone()))
}

fn null(t: &'static str) -> Val {
    net(NetObj::Null(t))
}

/// The text of a value wrapper or plain value (Booleans as 1 / 0).
fn attr_text(v: Option<&Val>) -> String {
    let v = match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Prim(_, x) => x.clone(),
            NetObj::Null(_) => return String::new(),
            _ => Val::Void,
        },
        Some(v) => v.clone(),
        None => Val::Void,
    };
    match v {
        Val::Bool(b) => (b as u8).to_string(),
        o => o.to_text(),
    }
}

/// Constructors of SDK classes and value wrappers (others: the sink).
pub fn construct(ty: &'static str, a: &Args) -> Val {
    match ty {
        "DocumentFormat.OpenXml.StringValue" => net(NetObj::Prim(ty, Val::Text(if a.is_empty() { String::new() } else { a.text(0) }))),
        "DocumentFormat.OpenXml.UInt32Value" | "DocumentFormat.OpenXml.Int32Value" | "DocumentFormat.OpenXml.ByteValue" => net(NetObj::Prim(ty, Val::BigInt(if a.is_empty() { 0 } else { a.get(0).as_i64() }))),
        "DocumentFormat.OpenXml.BooleanValue" => net(NetObj::Prim(ty, Val::Bool(!a.is_empty() && a.bool(0)))),
        "DocumentFormat.OpenXml.EnumValue`1" => net(NetObj::Prim(ty, Val::Text(if a.is_empty() { String::new() } else { a.text(0) }))),
        "Microsoft.Dynamics.Nav.OpenXml.Spreadsheet.WorksheetHelper" => net(NetObj::Prim(ty, Val::Void)),
        t if class_by_name(t).is_some() => new_el(class_by_name(t).unwrap().name),
        _ => super::xlsx::sink(),
    }
}

/// `.Value` of a value wrapper (get / set).
pub fn value_call(n: &NetRef, m: &str, a: &Args) -> Option<R<Out>> {
    let mut o = n.borrow_mut();
    let NetObj::Prim(t, v) = &mut *o else { return None };
    if !is_value_type(t) {
        return None;
    }
    Some(match m {
        "value" if a.len() == 1 => {
            *v = match (&*v, a.get(0)) {
                (Val::Bool(_), x) => Val::Bool(x.as_bool()),
                (Val::BigInt(_), x) => Val::BigInt(x.as_i64()),
                (_, x) => Val::Text(x.to_text()),
            };
            void()
        }
        "value" | "tostring" | "innertext" => ok(v.clone()),
        "hasvalue" => ok(Val::Bool(true)),
        _ => Ok(Out::Unknown),
    })
}

fn find_child(e: &Node, local: &str) -> Option<Node> {
    dom::element_children(e).into_iter().find(|c| c.borrow().local == local)
}

/// Set a child-element property: replace the existing child of that name, else append.
fn set_child(e: &Node, local: &str, child: &Node) {
    if let Some(old) = find_child(e, local) {
        dom::replace(&old, child);
    } else {
        dom::detach(child);
        dom::append(e, child);
    }
}

fn el_of(v: Option<&Val>) -> Option<Node> {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Ox(Ox::El(_, e)) => Some(e.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Element markup: SpreadsheetML elements written here (namespace on the part root, `r:` for the
/// relationship id), foreign content (the XSD in Schema) as it was parsed.
pub fn element_xml(e: &Node, root: bool) -> String {
    let (local, ns, attrs, kids, value) = {
        let b = e.borrow();
        (b.local.clone(), b.ns.clone(), b.attrs.clone(), b.children.clone(), b.value.clone())
    };
    if dom::kind(e) != dom::NK::Element {
        return dom::outer_xml(e);
    }
    if ns != MAIN_NS {
        return dom::outer_xml(e);
    }
    let mut out = format!("<{local}");
    if root {
        out.push_str(&format!(" xmlns=\"{MAIN_NS}\" xmlns:r=\"{REL_NS}\""));
    }
    for a in &attrs {
        let b = a.borrow();
        if b.prefix == "xmlns" || (b.prefix.is_empty() && b.local == "xmlns") {
            continue;
        }
        let q = if b.prefix.is_empty() { b.local.clone() } else { format!("{}:{}", b.prefix, b.local) };
        out.push_str(&format!(" {q}=\"{}\"", dom::escape_attr(&b.value)));
    }
    let _ = value;
    if kids.is_empty() {
        out.push_str("/>");
        return out;
    }
    out.push('>');
    for k in &kids {
        if dom::kind(k) == dom::NK::Element {
            out.push_str(&element_xml(k, false));
        } else {
            out.push_str(&dom::outer_xml(k));
        }
    }
    out.push_str(&format!("</{local}>"));
    out
}

/// Parse a part's XML into its root element.
pub fn parse_root(xml: &[u8]) -> Option<Node> {
    let text = super::text::decode(65001, xml);
    let doc = dom::parse(text.trim_start_matches('\u{feff}'), &dom::ParseOptions { preserve_whitespace: false }).ok()?;
    dom::root_element(&doc)
}

impl<'r> crate::interp::Session<'r> {
    /// Members of OpenXml objects (crate::dotnet::NetObj::Ox).
    pub fn ox_call(&mut self, n: &NetRef, m: &str, a: &Args) -> R<Out> {
        let o = match &*n.borrow() {
            NetObj::Ox(o) => match o {
                Ox::El(c, e) => Ox::El(*c, e.clone()),
                Ox::Part(p) => Ox::Part(p.clone()),
                Ox::WBook(b) => Ox::WBook(b.clone()),
                Ox::WBookPart(b) => Ox::WBookPart(b.clone()),
                Ox::WSheet(b, i) => Ox::WSheet(b.clone(), *i),
                Ox::WSheetPart(b, i) => Ox::WSheetPart(b.clone(), *i),
                Ox::RBook(b) => Ox::RBook(b.clone()),
                Ox::RBookPart(b) => Ox::RBookPart(b.clone()),
                Ox::RSheet(b, i) => Ox::RSheet(b.clone(), *i),
                Ox::RSheetPart(b, i) => Ox::RSheetPart(b.clone(), *i),
            },
            _ => return Ok(Out::Unknown),
        };
        match o {
            Ox::El(class, e) => self.ox_element(class, &e, m, a),
            Ox::Part(p) => {
                let prop = part_root_prop(p.borrow().kind);
                if m == prop {
                    if let Some(root) = el_of(a.raw(0)) {
                        p.borrow_mut().root = Some(root);
                        return void();
                    }
                    let root = p.borrow().root.clone();
                    return ok(match root {
                        Some(r) => el_val(&r),
                        None => null("DocumentFormat.OpenXml.OpenXmlElement"),
                    });
                }
                ok(match m {
                    "getstream" | "feeddata" => super::xlsx::sink(),
                    _ => return Ok(Out::Unknown),
                })
            }
            Ox::WBook(b) => ok(match m {
                "workbookpart" => ox(Ox::WBookPart(b)),
                "save" => Val::Void,
                _ => super::xlsx::sink(),
            }),
            Ox::WBookPart(b) => ok(match m {
                "customxmlmappingspart" => match b.borrow().ox_maps.clone() {
                    Some(p) => ox(Ox::Part(p)),
                    None => null(MAPS_PART),
                },
                "workbook" => ox(Ox::WBook(b)),
                _ => super::xlsx::sink(),
            }),
            Ox::WSheet(b, i) => ok(match m {
                "worksheetpart" => ox(Ox::WSheetPart(b, i)),
                _ => super::xlsx::sink(),
            }),
            Ox::WSheetPart(b, i) => {
                let bb = b.borrow();
                let Some(sh) = bb.sheets.get(i) else { return ok(super::xlsx::sink()) };
                ok(match m {
                    "singlecelltablepart" => match &sh.ox.single {
                        Some(p) => ox(Ox::Part(p.clone())),
                        None => null(SINGLE_CELLS_PART),
                    },
                    "getidofpart" => match a.raw(0) {
                        Some(Val::Net(x)) => match &*x.borrow() {
                            NetObj::Ox(Ox::Part(p)) => Val::Text(p.borrow().rid.clone()),
                            _ => Val::Text(String::new()),
                        },
                        _ => Val::Text(String::new()),
                    },
                    "tabledefinitionparts" => net(NetObj::Array(TABLE_PART, sh.ox.tables.iter().map(|p| ox(Ox::Part(p.clone()))).collect())),
                    "worksheet" => {
                        drop(bb);
                        ox(Ox::WSheet(b, i))
                    }
                    _ => super::xlsx::sink(),
                })
            }
            Ox::RBook(book) => ok(match m {
                "workbookpart" => ox(Ox::RBookPart(book)),
                "sheets" => {
                    let s = dom::element("", "sheets", MAIN_NS);
                    for _ in &book.sheets {
                        dom::append(&s, &dom::element("", "sheet", MAIN_NS));
                    }
                    el_val(&s)
                }
                _ => super::xlsx::sink(),
            }),
            Ox::RBookPart(book) => ok(match m {
                "workbook" => ox(Ox::RBook(book)),
                "customxmlmappingspart" => match book.xml_maps.as_ref().and_then(|x| parse_root(x)) {
                    Some(root) => {
                        let p = new_part(MAPS_PART, String::new());
                        p.borrow_mut().root = Some(root);
                        ox(Ox::Part(p))
                    }
                    None => null(MAPS_PART),
                },
                _ => super::xlsx::sink(),
            }),
            Ox::RSheet(book, i) => ok(match m {
                "worksheetpart" => ox(Ox::RSheetPart(book, i)),
                _ => super::xlsx::sink(),
            }),
            Ox::RSheetPart(book, i) => ok(match m {
                "tabledefinitionparts" => {
                    let parts: Vec<Val> = book
                        .sheets
                        .get(i)
                        .map(|s| {
                            s.tables
                                .iter()
                                .filter_map(|x| parse_root(x))
                                .map(|root| {
                                    let p = new_part(TABLE_PART, String::new());
                                    p.borrow_mut().root = Some(root);
                                    ox(Ox::Part(p))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    net(NetObj::Array(TABLE_PART, parts))
                }
                "worksheet" => ox(Ox::RSheet(book, i)),
                _ => super::xlsx::sink(),
            }),
        }
    }

    fn ox_element(&mut self, class: &'static str, e: &Node, m: &str, a: &Args) -> R<Out> {
        let cls = class_by_name(class);
        // attribute properties
        if let Some((_, attr)) = cls.and_then(|c| c.attrs.iter().find(|(p, _)| *p == m)) {
            if a.len() == 1 {
                let v = attr_text(a.raw(0));
                dom::set_attr(e, "", attr, "", &v);
                return void();
            }
            let wrapper: &'static str = if matches!(m, "reference" | "name" | "displayname" | "uniquename" | "xpath" | "cellreference" | "rootelement" | "schemaid" | "selectionnamespaces" | "url") {
                "DocumentFormat.OpenXml.StringValue"
            } else {
                "DocumentFormat.OpenXml.StringValue"
            };
            return ok(match dom::get_attr(e, attr, None) {
                Some(x) => net(NetObj::Prim(wrapper, Val::Text(x.borrow().value.clone()))),
                None => null(wrapper),
            });
        }
        // child-element properties (`T.TableColumns := X`, `T.TableColumns(X)`, `T.TableColumns()`)
        if let Some((_, kid)) = cls.and_then(|c| c.kids.iter().find(|(p, _)| *p == m)) {
            let kc = class_by_name(kid);
            let local = kc.map(|c| c.local).unwrap_or("unknown");
            if let Some(child) = el_of(a.raw(0)) {
                set_child(e, local, &child);
                return void();
            }
            return ok(match find_child(e, local) {
                Some(c) => el_val(&c),
                None => null(kc.map(|c| c.name).unwrap_or(UNKNOWN)),
            });
        }
        ok(match m {
            // TablePart.Id is the relationship id
            "id" if class.ends_with(".TablePart") => {
                if a.len() == 1 {
                    dom::set_attr(e, "r", "id", REL_NS, &attr_text(a.raw(0)));
                    Val::Void
                } else {
                    match dom::get_attr(e, "id", Some(REL_NS)) {
                        Some(x) => net(NetObj::Prim("DocumentFormat.OpenXml.StringValue", Val::Text(x.borrow().value.clone()))),
                        None => null("DocumentFormat.OpenXml.StringValue"),
                    }
                }
            }
            "firstchild" => match dom::element_children(e).first() {
                Some(c) => el_val(c),
                None => null(UNKNOWN),
            },
            "lastchild" => match dom::element_children(e).last() {
                Some(c) => el_val(c),
                None => null(UNKNOWN),
            },
            "parent" => match dom::parent(e) {
                Some(p) if dom::kind(&p) == dom::NK::Element => el_val(&p),
                _ => null(UNKNOWN),
            },
            "childelements" => net(NetObj::Array("DocumentFormat.OpenXml.OpenXmlElement", dom::element_children(e).iter().map(el_val).collect())),
            "haschildren" => Val::Bool(!dom::element_children(e).is_empty()),
            "getenumerator" => net(NetObj::Enumerator(dom::element_children(e).iter().map(el_val).collect(), -1)),
            "appendchild" | "append" => {
                if let Some(c) = el_of(a.raw(0)) {
                    dom::detach(&c);
                    dom::append(e, &c);
                }
                a.raw(0).cloned().unwrap_or(Val::Void)
            }
            "remove" => {
                dom::detach(e);
                Val::Void
            }
            "removeallchildren" => {
                dom::remove_all_children(e);
                Val::Void
            }
            "localname" => Val::Text(e.borrow().local.clone()),
            "innertext" => Val::Text(dom::inner_text(e)),
            // InnerXml := X: the fragment parsed in the SpreadsheetML namespace (OpenXML
            // Management.CleanMapInfo strips the x: prefixes and sets it back)
            "innerxml" if a.len() == 1 => {
                let wrapped = format!("<w xmlns=\"{MAIN_NS}\">{}</w>", a.text(0));
                if let Ok(doc) = dom::parse(&wrapped, &dom::ParseOptions { preserve_whitespace: false }) {
                    if let Some(w) = dom::root_element(&doc) {
                        dom::remove_all_children(e);
                        for c in dom::children(&w) {
                            dom::detach(&c);
                            dom::append(e, &c);
                        }
                    }
                }
                Val::Void
            }
            "innerxml" => Val::Text(dom::children(e).iter().map(|c| sdk_xml(c, false)).collect()),
            "outerxml" => Val::Text(sdk_xml(e, true)),
            // WriteTo(XmlWriter): the element's markup to the writer (a server file for
            // XmlWriter.Create(FileName))
            "writeto" => {
                let xml = element_xml(e, false);
                if let Some(target) = a.raw(0) {
                    match super::xmlwriter::file_target(target) {
                        Some(path) => {
                            let mut d = self.files.read(&path).unwrap_or_default();
                            d.extend_from_slice(xml.as_bytes());
                            self.files.write(&path, d);
                        }
                        None => super::xmlwriter::write_text_to(target, &xml),
                    }
                }
                Val::Void
            }
            "clonenode" => el_val(&dom::clone_deep(e)),
            _ => return Ok(Out::Unknown),
        })
    }

    /// WorksheetHelper.AppendElementToOpenXmlElement(Parent, Child): the child appended to an
    /// element, or to the writer's worksheet.
    pub fn ox_append(&mut self, a: &Args) {
        let Some(child) = el_of(a.raw(1)) else { return };
        if let Some(Val::Net(p)) = a.raw(0) {
            match &*p.borrow() {
                NetObj::Ox(Ox::El(_, e)) => {
                    dom::detach(&child);
                    dom::append(e, &child);
                }
                NetObj::Ox(Ox::WSheet(b, i)) => {
                    if let Some(sh) = b.borrow_mut().sheets.get_mut(*i) {
                        sh.ox.extra.push(child);
                    }
                }
                _ => {}
            }
        }
    }
}

/// OpenXml parts of a writer's worksheet.
#[derive(Clone, Default)]
pub struct SheetParts {
    pub tables: Vec<PartRef>,
    pub single: Option<PartRef>,
    /// Elements appended to the Worksheet (TableParts), written after sheetData.
    pub extra: Vec<Node>,
}

impl std::fmt::Debug for SheetParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SheetParts({} tables)", self.tables.len())
    }
}

/// A new element of an SDK class (short name) with attributes (XML names).
pub fn el_with(class: &str, attrs: &[(&str, String)]) -> Val {
    let c = class_by_name(class);
    let e = dom::element("", c.map(|c| c.local).unwrap_or("unknown"), MAIN_NS);
    for (k, v) in attrs {
        dom::set_attr(&e, "", k, "", v);
    }
    ox(Ox::El(c.map(|c| c.name).unwrap_or(UNKNOWN), e))
}

/// WorksheetWriter.CreateTablePart(RelationshipId).
pub fn table_part(rid: &str) -> Val {
    let e = dom::element("", "tablePart", MAIN_NS);
    dom::set_attr(&e, "r", "id", REL_NS, rid);
    ox(Ox::El("DocumentFormat.OpenXml.Spreadsheet.TablePart", e))
}

/// WorkbookWriter.CreateSchemaFromText(Xsd): a Schema element holding the parsed schema.
pub fn schema_from_text(xsd: &str) -> Val {
    let s = dom::element("", "Schema", MAIN_NS);
    if let Ok(doc) = dom::parse(xsd.trim_start_matches('\u{feff}'), &dom::ParseOptions { preserve_whitespace: false }) {
        if let Some(root) = dom::root_element(&doc) {
            dom::detach(&root);
            dom::append(&s, &root);
        }
    }
    ox(Ox::El("DocumentFormat.OpenXml.Spreadsheet.Schema", s))
}

/// The value text of an argument (value wrappers unwrapped, Booleans as 1 / 0).
pub fn arg_text(v: Option<&Val>) -> String {
    attr_text(v)
}

/// A part's XML (declaration and root with the namespaces), if it has a root.
pub fn part_xml(p: &PartRef) -> Option<Vec<u8>> {
    let root = p.borrow().root.clone()?;
    Some(format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>{}", element_xml(&root, true)).into_bytes())
}

/// OuterXml as the OpenXml SDK writes it: SpreadsheetML elements with the `x:` prefix, the
/// namespace declared (last) on the element asked for; foreign content (the XSD) as parsed.
pub fn sdk_xml(e: &Node, declare: bool) -> String {
    if dom::kind(e) != dom::NK::Element {
        return dom::outer_xml(e);
    }
    let (local, ns, attrs, kids) = {
        let b = e.borrow();
        (b.local.clone(), b.ns.clone(), b.attrs.clone(), b.children.clone())
    };
    if ns != MAIN_NS {
        return dom::outer_xml(e);
    }
    let mut out = format!("<x:{local}");
    let mut rel = false;
    for a in &attrs {
        let b = a.borrow();
        if b.prefix == "xmlns" || (b.prefix.is_empty() && b.local == "xmlns") {
            continue;
        }
        if b.prefix == "r" {
            rel = true;
        }
        let q = if b.prefix.is_empty() { b.local.clone() } else { format!("{}:{}", b.prefix, b.local) };
        out.push_str(&format!(" {q}=\"{}\"", dom::escape_attr(&b.value)));
    }
    if rel {
        out.push_str(&format!(" xmlns:r=\"{REL_NS}\""));
    }
    if declare {
        out.push_str(&format!(" xmlns:x=\"{MAIN_NS}\""));
    }
    if kids.is_empty() {
        out.push_str(" />");
        return out;
    }
    out.push('>');
    for k in &kids {
        out.push_str(&sdk_xml(k, false));
    }
    out.push_str(&format!("</x:{local}>"));
    out
}

/// Element factories of the Nav WorksheetWriter that need no workbook (they also work on a
/// WorksheetWriter variable that was never assigned, as OpenXML Management uses them).
pub fn writer_factory(m: &str, a: &Args) -> Option<Val> {
    Some(match m {
        "createtable" => el_with("Table", &[("id", a.text(0))]),
        "createtablecolumns" => el_with("TableColumns", &[("count", a.text(0))]),
        "createtablecolumn" => el_with("TableColumn", &[("id", a.text(0)), ("name", a.text(1)), ("uniqueName", a.text(2))]),
        "createxmlcolumnproperties" => el_with("XmlColumnProperties", &[("mapId", a.text(0)), ("xpath", a.text(1)), ("xmlDataType", attr_text(a.raw(2)))]),
        "addsinglexmlcell" => el_with("SingleXmlCell", &[("id", a.text(0))]),
        "createtableparts" => el_with("TableParts", &[("count", a.text(0))]),
        "createtablepart" => table_part(&attr_text(a.raw(0))),
        "xmldatatype2xmldatavalues" => net(NetObj::Prim("DocumentFormat.OpenXml.EnumValue`1", Val::Text(a.text(0).to_lowercase()))),
        "getenumxmldatavalues" => {
            let t = attr_text(a.raw(0));
            net(NetObj::Prim("DocumentFormat.OpenXml.EnumValue`1", Val::Text(if t.is_empty() { "string".into() } else { t.to_lowercase() })))
        }
        _ => return None,
    })
}
