//! Microsoft.Dynamics.Nav.OpenXml.Spreadsheet reading (Excel Buffer, Library - Report Validation):
//! WorkbookReader (Open from a server file or a stream, SheetNames, HasWorksheet,
//! GetWorksheetByName), WorksheetReader (RowCount, GetEnumerator / foreach) and CellData, with
//! BC's semantics [src:TODO]: shared and inline strings have format '@'; untyped and
//! number cells are parsed as a double (invariant) and printed in the current culture; the format
//! is the cell style's number format (custom numFmts, else the built-in ids); row and column
//! numbers come from the references, else follow the previous cell. The package is read with
//! crate::dotnet::zip and its parts with quick-xml.
//!
//! Writing (Excel Buffer export): WorkbookWriter (Create / Open on a server file or a stream,
//! FirstWorksheet, AddWorksheet, GetWorksheetByName, CopySheet, DeleteWorksheet, Close writes the
//! package) and WorksheetWriter (SetCellValueText / Number / Date / Time / Boolean / Formula as BC
//! stores them: text as inline strings, numbers parsed in the current culture and written
//! invariant, dates as OADates, times as day fractions; every cell's style carries its number
//! format). Layout calls (page setup, headers, column widths, ranges) are accepted; the OpenXml SDK
//! parts some callers reach for (Worksheet, comments and VML parts) are the neutral DotNet shim.

use super::*;
use quick_xml::events::Event;
use std::collections::HashMap;

pub const BOOK_READER: &str = "Microsoft.Dynamics.Nav.OpenXml.Spreadsheet.WorkbookReader";
pub const SHEET_READER: &str = "Microsoft.Dynamics.Nav.OpenXml.Spreadsheet.WorksheetReader";
const CELL_DATA: &str = "Microsoft.Dynamics.Nav.OpenXml.Spreadsheet.CellData";

pub const BOOK_WRITER: &str = "Microsoft.Dynamics.Nav.OpenXml.Spreadsheet.WorkbookWriter";
pub const SHEET_WRITER: &str = "Microsoft.Dynamics.Nav.OpenXml.Spreadsheet.WorksheetWriter";
const CELL_POSITION: &str = "Microsoft.Dynamics.Nav.OpenXml.Spreadsheet.CellPosition";
const CELL_DECORATOR: &str = "Microsoft.Dynamics.Nav.OpenXml.Spreadsheet.CellDecorator";

/// OpenXml SDK objects reached through the writers (Worksheet, WorksheetPart, comments and VML
/// parts ...): every member is another such object, it is never null and enumerates as empty.
pub const SDK_SINK: &str = "bc-rust.OpenXmlSdkObject";

pub const TYPES: &[&str] = &[BOOK_READER, SHEET_READER, CELL_DATA, BOOK_WRITER, SHEET_WRITER, CELL_POSITION, CELL_DECORATOR, SDK_SINK];

/// OpenXml SDK types the Excel Buffer and OpenXML Management code reaches through the writers
/// (parts, worksheet elements, comments, VML drawings, the Nav WorksheetHelper): emulated as sinks.
pub fn is_sdk(t: &str) -> bool {
    t == SDK_SINK || t.starts_with("DocumentFormat.OpenXml.Spreadsheet.") || t.starts_with("DocumentFormat.OpenXml.Packaging.") || t == "Microsoft.Dynamics.Nav.OpenXml.Spreadsheet.WorksheetHelper"
}

pub fn sink() -> Val {
    net(NetObj::Prim(SDK_SINK, Val::Void))
}

/// One `<c>` of a sheet.
#[derive(Clone, Debug)]
pub struct Cell {
    pub row: i64,
    pub col: i64,
    pub t: Option<String>,
    pub s: Option<usize>,
    pub v: Option<String>,
    pub inline: Option<String>,
}

#[derive(Debug, Default)]
pub struct Sheet {
    pub name: String,
    pub rows: usize,
    pub cells: Vec<Cell>,
    /// Table definition parts of the sheet (raw XML), for WorksheetPart.TableDefinitionParts.
    pub tables: Vec<Vec<u8>>,
}

#[derive(Debug, Default)]
pub struct Book {
    pub sheets: Vec<Sheet>,
    pub shared: Vec<String>,
    /// cellXfs: numFmtId per style index (None when the xf has none)
    pub xf_fmt: Vec<Option<u32>>,
    pub num_fmts: HashMap<u32, String>,
    /// xl/xmlMaps.xml (raw), for WorkbookPart.CustomXmlMappingsPart.
    pub xml_maps: Option<Vec<u8>>,
}

fn builtin_format(id: u32) -> &'static str {
    match id {
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        14 => "mm-dd-yy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        37 => "#,##0;(#,##0)",
        38 => "#,##0;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        49 => "@",
        101 => "hh:mm:ss.000",
        168 => "dd/mm/yy\\ hh:mm:ss;@",
        _ => "",
    }
}

fn attr(e: &quick_xml::events::BytesStart, name: &[u8]) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == name || a.key.local_name().as_ref() == name).and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn local(e: &[u8]) -> &[u8] {
    match e.iter().rposition(|c| *c == b':') {
        Some(i) => &e[i + 1..],
        None => e,
    }
}

/// Relationship targets of a .rels part: id → (type, target).
fn rels(xml: &[u8]) -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    let mut r = quick_xml::Reader::from_reader(xml);
    let mut buf = vec![];
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(e.name().as_ref()) == b"Relationship" => {
                if let (Some(id), Some(target)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                    out.insert(id, (attr(&e, b"Type").unwrap_or_default(), target));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// `target` relative to the directory of `base` (a part path without a leading '/').
fn resolve(base: &str, target: &str) -> String {
    if let Some(t) = target.strip_prefix('/') {
        return t.to_string();
    }
    let mut parts: Vec<&str> = base.split('/').collect();
    parts.pop();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Text of the `<t>` elements under the current element until its end (rich text runs joined).
fn shared_strings(xml: &[u8]) -> Vec<String> {
    let mut out = vec![];
    let mut r = quick_xml::Reader::from_reader(xml);
    let mut buf = vec![];
    let (mut in_si, mut in_t, mut depth_t) = (false, false, 0usize);
    let mut direct = String::new();
    let mut all = String::new();
    let mut has_direct = false;
    let mut depth = 0usize;
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                depth += 1;
                let n = local(e.name().as_ref()).to_vec();
                if n == b"si" {
                    in_si = true;
                    direct.clear();
                    all.clear();
                    has_direct = false;
                    depth_t = depth;
                } else if n == b"t" && in_si {
                    in_t = true;
                    if depth == depth_t + 1 {
                        has_direct = true;
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                if local(e.name().as_ref()) == b"si" {
                    out.push(String::new());
                }
            }
            Ok(Event::Text(t)) if in_t => {
                let s = t.unescape().map(|x| x.into_owned()).unwrap_or_default();
                if depth == depth_t + 1 {
                    direct.push_str(&s);
                }
                all.push_str(&s);
            }
            Ok(Event::CData(t)) if in_t => {
                let s = String::from_utf8_lossy(&t).into_owned();
                if depth == depth_t + 1 {
                    direct.push_str(&s);
                }
                all.push_str(&s);
            }
            Ok(Event::End(e)) => {
                let n = local(e.name().as_ref()).to_vec();
                if n == b"t" {
                    in_t = false;
                } else if n == b"si" && in_si {
                    in_si = false;
                    out.push(if has_direct { direct.clone() } else { all.clone() });
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn styles(xml: &[u8], book: &mut Book) {
    let mut r = quick_xml::Reader::from_reader(xml);
    let mut buf = vec![];
    let mut in_xfs = false;
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let empty = false;
                let _ = empty;
                match local(e.name().as_ref()) {
                    b"numFmt" => {
                        if let (Some(id), Some(code)) = (attr(&e, b"numFmtId"), attr(&e, b"formatCode")) {
                            if let Ok(id) = id.parse() {
                                book.num_fmts.insert(id, code);
                            }
                        }
                    }
                    b"cellXfs" => in_xfs = true,
                    b"xf" if in_xfs => book.xf_fmt.push(attr(&e, b"numFmtId").and_then(|v| v.parse().ok())),
                    _ => {}
                }
            }
            Ok(Event::End(e)) if local(e.name().as_ref()) == b"cellXfs" => in_xfs = false,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
}

/// "AB12" → (row 12, column 28); either part may be missing.
fn cell_ref(r: &str) -> (Option<i64>, Option<i64>) {
    let letters: String = r.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits = &r[letters.len()..];
    let col = if letters.is_empty() { None } else { Some(letters.to_ascii_uppercase().bytes().fold(0i64, |a, b| a * 26 + (b - b'A' + 1) as i64)) };
    (digits.parse().ok(), col)
}

fn sheet_cells(xml: &[u8], sheet: &mut Sheet) {
    let mut r = quick_xml::Reader::from_reader(xml);
    let mut buf = vec![];
    let (mut row, mut col) = (0i64, 0i64);
    let mut cur: Option<Cell> = None;
    // 1 = <v>, 2 = <is> text
    let mut text_mode = 0u8;
    let mut in_is = false;
    loop {
        let ev = r.read_event_into(&mut buf);
        match ev {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let is_empty = matches!(ev, Ok(Event::Empty(_)));
                match local(e.name().as_ref()) {
                    b"row" => {
                        sheet.rows += 1;
                        row = attr(e, b"r").and_then(|v| v.parse().ok()).unwrap_or(row + 1);
                        col = 0;
                    }
                    b"c" => {
                        let (rr, cc) = attr(e, b"r").map(|v| cell_ref(&v)).unwrap_or((None, None));
                        if let Some(rr) = rr {
                            row = rr;
                        }
                        col = cc.unwrap_or(col + 1);
                        let c = Cell { row, col, t: attr(e, b"t"), s: attr(e, b"s").and_then(|v| v.parse().ok()), v: None, inline: None };
                        if is_empty {
                            sheet.cells.push(c);
                        } else {
                            cur = Some(c);
                        }
                    }
                    b"v" if !is_empty => text_mode = 1,
                    b"is" if !is_empty => in_is = true,
                    b"t" if in_is && !is_empty => text_mode = 2,
                    _ => {}
                }
            }
            Ok(Event::Text(t)) if text_mode > 0 => {
                let s = t.unescape().map(|x| x.into_owned()).unwrap_or_default();
                if let Some(c) = cur.as_mut() {
                    if text_mode == 1 {
                        c.v.get_or_insert_with(String::new).push_str(&s);
                    } else {
                        c.inline.get_or_insert_with(String::new).push_str(&s);
                    }
                }
            }
            Ok(Event::End(e)) => match local(e.name().as_ref()) {
                b"v" | b"t" => text_mode = 0,
                b"is" => in_is = false,
                b"c" => {
                    if let Some(c) = cur.take() {
                        sheet.cells.push(c);
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
}

/// Read an .xlsx package.
pub fn read_book(bytes: &[u8]) -> Result<Book, String> {
    let entries = zip::read_zip(bytes)?;
    let parts: HashMap<String, Vec<u8>> = entries.into_iter().map(|(n, d)| (n.trim_start_matches('/').to_string(), d)).collect();
    let root = parts.get("_rels/.rels").map(|x| rels(x)).unwrap_or_default();
    let wb_path = root.values().find(|(t, _)| t.ends_with("/officeDocument")).map(|(_, t)| t.trim_start_matches('/').to_string()).unwrap_or_else(|| "xl/workbook.xml".into());
    let wb = parts.get(&wb_path).ok_or("The file is not a valid Excel workbook.")?;
    let wb_rels_path = resolve(&wb_path, &format!("_rels/{}.rels", wb_path.rsplit('/').next().unwrap_or("workbook.xml")));
    let wrels = parts.get(&wb_rels_path).map(|x| rels(x)).unwrap_or_default();
    let mut book = Book::default();
    if let Some((_, t)) = wrels.values().find(|(ty, _)| ty.ends_with("/sharedStrings")) {
        if let Some(x) = parts.get(&resolve(&wb_path, t)) {
            book.shared = shared_strings(x);
        }
    }
    if let Some((_, t)) = wrels.values().find(|(ty, _)| ty.ends_with("/styles")) {
        if let Some(x) = parts.get(&resolve(&wb_path, t)) {
            styles(x, &mut book);
        }
    }
    if let Some((_, t)) = wrels.values().find(|(ty, _)| ty.ends_with("/xmlMaps")) {
        book.xml_maps = parts.get(&resolve(&wb_path, t)).cloned();
    }
    let mut r = quick_xml::Reader::from_reader(wb.as_slice());
    let mut buf = vec![];
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if local(e.name().as_ref()) == b"sheet" => {
                let name = attr(&e, b"name").unwrap_or_default();
                let rid = e.attributes().flatten().find(|a| a.key.local_name().as_ref() == b"id").and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()));
                let mut sheet = Sheet { name, ..Default::default() };
                if let Some((_, t)) = rid.and_then(|id| wrels.get(&id).cloned()) {
                    let sp = resolve(&wb_path, &t);
                    if let Some(x) = parts.get(&sp) {
                        sheet_cells(x, &mut sheet);
                    }
                    // table definition parts (oxml: WorksheetPart.TableDefinitionParts)
                    let srels = resolve(&sp, &format!("_rels/{}.rels", sp.rsplit('/').next().unwrap_or("sheet.xml")));
                    if let Some(rx) = parts.get(&srels) {
                        let mut rl: Vec<(String, (String, String))> = rels(rx).into_iter().collect();
                        rl.sort();
                        for (_, (ty, target)) in rl {
                            if ty.ends_with("/table") {
                                if let Some(x) = parts.get(&resolve(&sp, &target)) {
                                    sheet.tables.push(x.clone());
                                }
                            }
                        }
                    }
                }
                book.sheets.push(sheet);
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(book)
}

/// ExcelCommon.CleanUpWorksheetName.
pub fn clean_sheet_name(name: &str) -> String {
    let s: String = name.chars().filter(|c| !matches!(c, '@' | ':' | '/' | '\\' | '*' | '?' | '[' | ']')).collect();
    if s.chars().count() > 31 {
        format!("{}..", s.chars().take(29).collect::<String>())
    } else {
        s
    }
}

impl Book {
    fn sheet_index(&self, name: &str) -> Option<usize> {
        let n = clean_sheet_name(name);
        self.sheets.iter().position(|s| s.name.to_lowercase() == n.to_lowercase())
    }

    fn format_of(&self, s: Option<usize>) -> String {
        let Some(s) = s else { return String::new() };
        match self.xf_fmt.get(s).copied().flatten() {
            None => String::new(),
            Some(id) => self.num_fmts.get(&id).cloned().unwrap_or_else(|| builtin_format(id).to_string()),
        }
    }

    /// CellData.Value and Format.
    fn value_format(&self, c: &Cell) -> (String, String) {
        let fmt = self.format_of(c.s);
        let raw = c.v.clone().unwrap_or_default();
        match c.t.as_deref() {
            None | Some("n") => (number(&raw), fmt),
            Some("s") => (raw.trim().parse::<usize>().ok().and_then(|i| self.shared.get(i).cloned()).unwrap_or_default(), "@".into()),
            Some("inlineStr") => (c.inline.clone().unwrap_or_default(), "@".into()),
            Some("e") | Some("str") => (raw, "@".into()),
            Some("d") => (number(&raw), if fmt.is_empty() { "mm-dd-yy".into() } else { fmt }),
            Some("b") => (raw, fmt),
            Some(_) => (raw, fmt),
        }
    }
}

/// CellData.StringToNumber: a double parsed with the invariant culture, printed in the current one.
fn number(s: &str) -> String {
    let Ok(d) = s.trim().parse::<f64>() else { return String::new() };
    let mut t = format!("{d}");
    let dec = super::culture::current().decimal.to_string();
    if dec != "." {
        t = t.replace('.', &dec);
    }
    t
}

fn cell_data(book: &Book, c: &Cell) -> Val {
    let (value, format) = book.value_format(c);
    net(NetObj::Bag(
        CELL_DATA,
        vec![
            ("rownumber".into(), Val::Int(c.row as i32)),
            ("columnnumber".into(), Val::Int(c.col as i32)),
            ("hasvalue".into(), Val::Bool(!value.is_empty())),
            ("value".into(), Val::Text(value)),
            ("format".into(), Val::Text(format)),
        ],
    ))
}

/// The CellData of a sheet, in document order (foreach / GetEnumerator).
pub fn sheet_cells_vals(book: &Rc<Book>, idx: usize) -> Vec<Val> {
    book.sheets.get(idx).map(|s| s.cells.iter().map(|c| cell_data(book, c)).collect()).unwrap_or_default()
}

fn open(s: &mut Session, a: &Args) -> R<Val> {
    let bytes = match a.raw(0) {
        Some(Val::Text(_)) => {
            let path = a.text(0);
            match s.files.read(&path) {
                Some(b) => b,
                None => return net_err("WorkbookReader", "Open", format!("Could not find file '{path}'.")),
            }
        }
        Some(Val::Net(n)) if matches!(&*n.borrow(), NetObj::Prim(_, Val::Text(_))) => {
            let path = a.text(0);
            match s.files.read(&path) {
                Some(b) => b,
                None => return net_err("WorkbookReader", "Open", format!("Could not find file '{path}'.")),
            }
        }
        other => super::text::arg_bytes(other),
    };
    match read_book(&bytes) {
        Ok(b) => Ok(net(NetObj::XlBook(Rc::new(b)))),
        Err(e) => net_err("WorkbookReader", "Open", e),
    }
}

pub fn call(s: &mut Session, n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    if matches!(&*n.borrow(), NetObj::Ox(_)) {
        return s.ox_call(n, m, a);
    }
    if ty == "Microsoft.Dynamics.Nav.OpenXml.Spreadsheet.WorksheetHelper" && m == "appendelementtoopenxmlelement" {
        s.ox_append(a);
        return void();
    }
    // element factories on a WorksheetWriter variable that was never assigned
    if ty == SHEET_WRITER && matches!(&*n.borrow(), NetObj::Null(_)) {
        if let Some(v) = super::oxml::writer_factory(m, a) {
            return ok(v);
        }
    }
    if is_sdk(ty) {
        return ok(match m {
            "tostring" => Val::Text(String::new()),
            _ => sink(),
        });
    }
    if ty == BOOK_READER && m == "open" {
        return open(s, a).map(Out::Val);
    }
    if ty == BOOK_WRITER && matches!(m, "create" | "open") && matches!(&*n.borrow(), NetObj::Null(_)) {
        return writer_open(s, m == "create", a).map(Out::Val);
    }
    if ty == CELL_POSITION && matches!(&*n.borrow(), NetObj::Null(_)) && m == "cellposition" {
        return ok(cell_position(&a.text(0)));
    }
    if matches!(&*n.borrow(), NetObj::XlWBook(_) | NetObj::XlWSheet(..)) {
        return writer_call(s, n, m, a);
    }
    let o = n.borrow();
    match &*o {
        NetObj::XlBook(book) => {
            let book = book.clone();
            drop(o);
            ok(match m {
                "sheetnames" | "getsheetnames" => net(NetObj::Array("System.String", book.sheets.iter().map(|s| Val::Text(s.name.clone())).collect())),
                "hasworksheet" => match a.raw(0) {
                    Some(Val::Int(id)) => Val::Bool((*id as usize) >= 1 && (*id as usize) <= book.sheets.len()),
                    _ => Val::Bool(book.sheet_index(&a.text(0)).is_some()),
                },
                "getworksheetbyname" => match book.sheet_index(&a.text(0)) {
                    Some(i) => net(NetObj::XlSheet(book.clone(), i)),
                    None => return net_err("WorkbookReader", "GetWorksheetByName", format!("The worksheet was not found.\r\nParameter name: {}", a.text(0))),
                },
                "getworksheetbyid" => match a.text(0).parse::<usize>().ok().filter(|i| *i >= 1 && *i <= book.sheets.len()) {
                    Some(i) => net(NetObj::XlSheet(book.clone(), i - 1)),
                    None => return net_err("WorkbookReader", "GetWorksheetById", "The worksheet was not found."),
                },
                "firstsheetid" => Val::Int(1),
                "workbook" => super::oxml::ox(super::oxml::Ox::RBook(book.clone())),
                "close" | "dispose" => Val::Void,
                "validatedocument" => Val::Bool(true),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::XlSheet(book, idx) => {
            let (book, idx) = (book.clone(), *idx);
            drop(o);
            ok(match m {
                "rowcount" => Val::Int(book.sheets.get(idx).map(|s| s.rows).unwrap_or(0) as i32),
                "worksheet" => super::oxml::ox(super::oxml::Ox::RSheet(book.clone(), idx)),
                "name" => Val::Text(book.sheets.get(idx).map(|s| s.name.clone()).unwrap_or_default()),
                "getenumerator" => net(NetObj::Enumerator(sheet_cells_vals(&book, idx), -1)),
                "close" | "dispose" => Val::Void,
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::Bag(CELL_DATA | CELL_POSITION | CELL_DECORATOR, p) => ok(p.iter().find(|(k, _)| k == m).map(|(_, v)| v.clone()).unwrap_or(Val::Void)),
        _ => Ok(Out::Unknown),
    }
}

// ---------------------------------------------------------------- writing

#[derive(Clone, Debug)]
pub enum WValue {
    Text(String),
    /// invariant number text (also dates and times as OADates)
    Number(String),
    Formula(String),
    Bool(bool),
}

#[derive(Clone, Debug)]
pub struct WCell {
    pub value: WValue,
    pub format: String,
}

#[derive(Clone, Debug, Default)]
pub struct WSheet {
    pub name: String,
    pub id: u32,
    pub cells: std::collections::BTreeMap<(u32, u32), WCell>,
    /// OpenXml parts and worksheet elements built through the SDK (oxml.rs).
    pub ox: super::oxml::SheetParts,
}

pub struct WBook {
    pub sheets: Vec<WSheet>,
    /// a server file name, or the stream Close writes to
    pub file: Option<String>,
    pub stream: Option<Val>,
    pub preserve: bool,
    pub closed: bool,
    /// CustomXmlMappingsPart and ConnectionsPart (oxml.rs).
    pub ox_maps: Option<super::oxml::PartRef>,
    pub ox_conns: Option<super::oxml::PartRef>,
}

pub type WRef = Rc<RefCell<WBook>>;

/// "AB" → 28.
fn col_number(name: &str) -> u32 {
    name.trim().to_ascii_uppercase().bytes().filter(|b| b.is_ascii_uppercase()).fold(0u32, |a, b| a * 26 + (b - b'A' + 1) as u32)
}

/// 28 → "AB".
pub fn col_name(mut n: u32) -> String {
    let mut s = String::new();
    while n > 0 {
        let r = (n - 1) % 26;
        s.insert(0, (b'A' + r as u8) as char);
        n = (n - 1) / 26;
    }
    s
}

fn cell_position(name: &str) -> Val {
    let (row, col) = cell_ref(name.trim());
    let (row, col) = (row.unwrap_or(0), col.unwrap_or(0));
    net(NetObj::Bag(
        CELL_POSITION,
        vec![("row".into(), Val::Int(row as i32)), ("column".into(), Val::Int(col as i32)), ("columnname".into(), Val::Text(col_name(col as u32)))],
    ))
}

fn decorator(bold: bool, italic: bool, underline: u8) -> Val {
    net(NetObj::Bag(CELL_DECORATOR, vec![("isbold".into(), Val::Bool(bold)), ("isitalic".into(), Val::Bool(italic)), ("underline".into(), Val::Int(underline as i32))]))
}

/// A writable workbook from an existing package (WorkbookWriter.Open).
fn wbook_from(bytes: &[u8]) -> Result<Vec<WSheet>, String> {
    let b = read_book(bytes)?;
    Ok(b
        .sheets
        .iter()
        .enumerate()
        .map(|(i, sh)| {
            let mut ws = WSheet { name: sh.name.clone(), id: i as u32 + 1, ..Default::default() };
            for c in &sh.cells {
                let fmt = b.format_of(c.s);
                let value = match c.t.as_deref() {
                    Some("s") | Some("inlineStr") | Some("str") | Some("e") => WValue::Text(b.value_format(c).0),
                    Some("b") => WValue::Bool(c.v.as_deref() == Some("1")),
                    _ => match &c.v {
                        Some(v) => WValue::Number(v.clone()),
                        None => continue,
                    },
                };
                ws.cells.insert((c.row as u32, c.col as u32), WCell { value, format: fmt });
            }
            ws
        })
        .collect())
}

fn writer_open(s: &mut Session, create: bool, a: &Args) -> R<Val> {
    let (file, stream) = match a.raw(0) {
        Some(Val::Text(t)) => (Some(t.clone()), None),
        Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Prim(_, Val::Text(_))) => (Some(a.text(0)), None),
        Some(v) => (None, Some(v.clone())),
        None => (None, None),
    };
    let name = if create { "WorkbookWriter.Create" } else { "WorkbookWriter.Open" };
    let sheets = if create {
        if let Some(f) = &file {
            if f.is_empty() {
                return net_err("WorkbookWriter", name, "The file name is not specified.");
            }
            if s.files.exists(f) {
                return net_err("WorkbookWriter", name, format!("The file already exists.\r\nParameter name: {f}"));
            }
        }
        vec![WSheet { name: "Sheet1".into(), id: 1, ..Default::default() }]
    } else {
        let bytes = match (&file, &stream) {
            (Some(f), _) => match s.files.read(f) {
                Some(b) => b,
                None => return net_err("WorkbookWriter", name, format!("Could not find file '{f}'.")),
            },
            (None, Some(v)) => super::text::arg_bytes(Some(v)),
            _ => vec![],
        };
        match wbook_from(&bytes) {
            Ok(v) => v,
            Err(e) => return net_err("WorkbookWriter", name, e),
        }
    };
    Ok(net(NetObj::XlWBook(Rc::new(RefCell::new(WBook { sheets, file, stream, preserve: false, closed: false, ox_maps: None, ox_conns: None })))))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The package of a workbook (sheets, inline strings, styles with the number formats used).
pub fn write_book(b: &WBook) -> Vec<u8> {
    // styles: xf 0 is the default; one xf per number format used
    let mut formats: Vec<String> = vec![];
    for sh in &b.sheets {
        for c in sh.cells.values() {
            if !c.format.is_empty() && !formats.contains(&c.format) {
                formats.push(c.format.clone());
            }
        }
    }
    let builtin_id = |f: &str| (0..200u32).find(|i| builtin_format(*i) == f && !f.is_empty());
    let mut custom: Vec<(u32, String)> = vec![];
    let mut xf_ids: Vec<u32> = vec![];
    for f in &formats {
        let id = match builtin_id(f) {
            Some(i) => i,
            None => {
                let i = 164 + custom.len() as u32;
                custom.push((i, f.clone()));
                i
            }
        };
        xf_ids.push(id);
    }
    let mut st = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">");
    if !custom.is_empty() {
        st.push_str(&format!("<numFmts count=\"{}\">", custom.len()));
        for (i, f) in &custom {
            st.push_str(&format!("<numFmt numFmtId=\"{i}\" formatCode=\"{}\"/>", escape(f)));
        }
        st.push_str("</numFmts>");
    }
    st.push_str("<fonts count=\"1\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts><fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills><borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>");
    st.push_str(&format!("<cellXfs count=\"{}\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>", xf_ids.len() + 1));
    for id in &xf_ids {
        st.push_str(&format!("<xf numFmtId=\"{id}\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/>"));
    }
    st.push_str("</cellXfs></styleSheet>");
    let style_of = |f: &str| if f.is_empty() { 0 } else { formats.iter().position(|x| x == f).map(|i| i + 1).unwrap_or(0) };
    let mut parts: Vec<(String, Vec<u8>)> = vec![];
    let mut wb_sheets = String::new();
    let mut wb_rels = String::new();
    let mut table_no = 0usize;
    let mut ct_extra = String::new();
    let mut ct = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>");
    for (i, sh) in b.sheets.iter().enumerate() {
        let k = i + 1;
        let mut sd = String::new();
        let mut cur_row = 0u32;
        for ((r, c), cell) in &sh.cells {
            if *r != cur_row {
                if cur_row != 0 {
                    sd.push_str("</row>");
                }
                sd.push_str(&format!("<row r=\"{r}\">"));
                cur_row = *r;
            }
            let rf = format!("{}{r}", col_name(*c));
            let si = style_of(&cell.format);
            let sa = if si > 0 { format!(" s=\"{si}\"") } else { String::new() };
            match &cell.value {
                WValue::Text(t) => sd.push_str(&format!("<c r=\"{rf}\"{sa} t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>", escape(t))),
                WValue::Number(v) if v.is_empty() => sd.push_str(&format!("<c r=\"{rf}\"{sa}/>")),
                WValue::Number(v) => sd.push_str(&format!("<c r=\"{rf}\"{sa}><v>{}</v></c>", escape(v))),
                WValue::Formula(f) => sd.push_str(&format!("<c r=\"{rf}\"{sa}><f>{}</f></c>", escape(f.trim_start_matches('=')))),
                WValue::Bool(x) => sd.push_str(&format!("<c r=\"{rf}\"{sa} t=\"b\"><v>{}</v></c>", *x as u8)),
            }
        }
        if cur_row != 0 {
            sd.push_str("</row>");
        }
        // OpenXml parts of the sheet (oxml.rs): elements appended to the Worksheet (TableParts),
        // table definition parts and the single-cell XML map part, with their relationships
        let extra: String = sh.ox.extra.iter().map(|e| super::oxml::element_xml(e, false)).collect();
        let mut srels = String::new();
        for p in &sh.ox.tables {
            if let Some(x) = super::oxml::part_xml(p) {
                table_no += 1;
                parts.push((format!("xl/tables/table{table_no}.xml"), x));
                srels.push_str(&format!("<Relationship Id=\"{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/table\" Target=\"../tables/table{table_no}.xml\"/>", p.borrow().rid));
                ct_extra.push_str(&format!("<Override PartName=\"/xl/tables/table{table_no}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml\"/>"));
            }
        }
        if let Some(p) = &sh.ox.single {
            if let Some(x) = super::oxml::part_xml(p) {
                parts.push((format!("xl/tables/tableSingleCells{k}.xml"), x));
                srels.push_str(&format!("<Relationship Id=\"{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/tableSingleCells\" Target=\"../tables/tableSingleCells{k}.xml\"/>", p.borrow().rid));
                ct_extra.push_str(&format!("<Override PartName=\"/xl/tables/tableSingleCells{k}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.tableSingleCells+xml\"/>"));
            }
        }
        if !srels.is_empty() {
            parts.push((format!("xl/worksheets/_rels/sheet{k}.xml.rels"), format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{srels}</Relationships>").into_bytes()));
        }
        parts.push((
            format!("xl/worksheets/sheet{k}.xml"),
            format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheetData>{sd}</sheetData>{extra}</worksheet>").into_bytes(),
        ));
        wb_sheets.push_str(&format!("<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{k}\"/>", escape(&sh.name), sh.id));
        wb_rels.push_str(&format!("<Relationship Id=\"rId{k}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{k}.xml\"/>"));
        ct.push_str(&format!("<Override PartName=\"/xl/worksheets/sheet{k}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"));
    }
    let n = b.sheets.len() + 1;
    wb_rels.push_str(&format!("<Relationship Id=\"rId{n}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>"));
    // workbook-level OpenXml parts (oxml.rs): XML maps and connections
    if let Some(x) = b.ox_maps.as_ref().and_then(super::oxml::part_xml) {
        parts.push(("xl/xmlMaps.xml".into(), x));
        wb_rels.push_str("<Relationship Id=\"rIdXmlMaps\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/xmlMaps\" Target=\"xmlMaps.xml\"/>");
        ct_extra.push_str("<Override PartName=\"/xl/xmlMaps.xml\" ContentType=\"application/xml\"/>");
    }
    if let Some(x) = b.ox_conns.as_ref().and_then(super::oxml::part_xml) {
        parts.push(("xl/connections.xml".into(), x));
        wb_rels.push_str("<Relationship Id=\"rIdConnections\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/connections\" Target=\"connections.xml\"/>");
        ct_extra.push_str("<Override PartName=\"/xl/connections.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.connections+xml\"/>");
    }
    ct.push_str(&ct_extra);
    ct.push_str("</Types>");
    let mut all = vec![
        ("[Content_Types].xml".to_string(), ct.into_bytes()),
        ("_rels/.rels".into(), "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>".as_bytes().to_vec()),
        ("xl/workbook.xml".into(), format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>{wb_sheets}</sheets></workbook>").into_bytes()),
        ("xl/_rels/workbook.xml.rels".into(), format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{wb_rels}</Relationships>").into_bytes()),
        ("xl/styles.xml".into(), st.into_bytes()),
    ];
    all.extend(parts);
    zip::write_zip(&all)
}

/// Number text as BC stores it: parsed in the current culture, written invariant.
fn number_value(v: &str) -> Option<String> {
    let c = super::culture::current();
    let t: String = v.trim().chars().filter(|ch| ch.to_string() != c.group && *ch != '\u{a0}').collect();
    let t = if c.decimal != "." { t.replace(c.decimal, ".") } else { t };
    let d: f64 = t.parse().ok()?;
    Some(format!("{d}"))
}

const OA_EPOCH_TICKS: i64 = 599_264_352_000_000_000; // 1899-12-30

fn oadate(ticks: i64) -> String {
    let d = (ticks - OA_EPOCH_TICKS) as f64 / 864_000_000_000.0;
    format!("{d}")
}

fn date_value(v: &str) -> WValue {
    if v.is_empty() || v == "0" {
        return WValue::Number(String::new());
    }
    match super::datetime::parse_any(v, super::culture::current()) {
        Some(p) if p.ticks >= OA_EPOCH_TICKS - 657_435 * 864_000_000_000 => WValue::Number(oadate(p.ticks)),
        _ => match v.trim().parse::<f64>() {
            Ok(_) => WValue::Number(v.trim().to_string()),
            Err(_) => WValue::Text(v.to_string()),
        },
    }
}

fn time_value(v: &str) -> WValue {
    if v.is_empty() {
        return WValue::Number(String::new());
    }
    let day = 864_000_000_000i64;
    match super::datetime::parse_any(v, super::culture::current()) {
        Some(p) => WValue::Number(format!("{}", (p.ticks.rem_euclid(day)) as f64 / day as f64)),
        None => match v.trim().parse::<f64>() {
            Ok(d) => WValue::Number(format!("{}", d.fract().abs())),
            Err(_) => WValue::Text(v.to_string()),
        },
    }
}

fn writer_call(s: &mut Session, n: &NetRef, m: &str, a: &Args) -> R<Out> {
    let (book, sheet) = match &*n.borrow() {
        NetObj::XlWBook(b) => (b.clone(), None),
        NetObj::XlWSheet(b, i) => (b.clone(), Some(*i)),
        _ => return Ok(Out::Unknown),
    };
    let sheet_val = |i: usize| net(NetObj::XlWSheet(book.clone(), i));
    let find = |name: &str| {
        let nm = clean_sheet_name(name).to_lowercase();
        book.borrow().sheets.iter().position(|s| s.name.to_lowercase() == nm)
    };
    if sheet.is_none() {
        return ok(match m {
            "firstworksheet" => sheet_val(0),
            "sheetscount" => Val::Int(book.borrow().sheets.len() as i32),
            "hasworksheet" => Val::Bool(find(&a.text(0)).is_some()),
            "getworksheetbyname" => match find(&a.text(0)) {
                Some(i) => sheet_val(i),
                None => return net_err("WorkbookWriter", "GetWorksheetByName", format!("The worksheet was not found.\r\nParameter name: {}", a.text(0))),
            },
            "getworksheetnamebyid" => {
                let id: u32 = a.text(0).trim().parse().unwrap_or(0);
                Val::Text(book.borrow().sheets.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_default())
            }
            "addworksheet" => {
                let name = clean_sheet_name(&a.text(0));
                let mut b = book.borrow_mut();
                let id = b.sheets.iter().map(|s| s.id).max().unwrap_or(0) + 1;
                b.sheets.push(WSheet { name, id, ..Default::default() });
                let i = b.sheets.len() - 1;
                drop(b);
                sheet_val(i)
            }
            "deleteworksheet" => {
                if let Some(i) = find(&a.text(0)) {
                    book.borrow_mut().sheets.remove(i);
                }
                Val::Void
            }
            "copysheet" => {
                // (source, target, reference sheet, paste before)
                if let Some(i) = find(&a.text(0)) {
                    let mut b = book.borrow_mut();
                    let mut copy = b.sheets[i].clone();
                    copy.name = clean_sheet_name(&a.text(1));
                    copy.id = b.sheets.iter().map(|s| s.id).max().unwrap_or(0) + 1;
                    let at = b.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(&a.text(2))).map(|r| if a.bool(3) { r } else { r + 1 }).unwrap_or(b.sheets.len());
                    b.sheets.insert(at, copy);
                }
                Val::Void
            }
            "getcellposition" => cell_position(&a.text(0)),
            "clearformulacalculations" | "dispose" => Val::Void,
            "validatedocument" => Val::Bool(true),
            "close" | "save" => {
                let bytes = write_book(&book.borrow());
                let (file, stream) = {
                    let mut b = book.borrow_mut();
                    b.closed = true;
                    (b.file.clone(), b.stream.clone())
                };
                if let Some(f) = file {
                    s.files.write(&f, bytes);
                } else if let Some(t) = stream {
                    super::text::stream_write(&t, &bytes);
                }
                Val::Void
            }
            // OpenXml SDK objects (oxml.rs)
            "document" | "workbook" => super::oxml::ox(super::oxml::Ox::WBook(book.clone())),
            "workbookpart" => super::oxml::ox(super::oxml::Ox::WBookPart(book.clone())),
            "addcustomxmlmappingspart" => {
                let p = super::oxml::new_part(super::oxml::MAPS_PART, "rIdXmlMaps".into());
                book.borrow_mut().ox_maps = Some(p.clone());
                super::oxml::ox(super::oxml::Ox::Part(p))
            }
            "addconnectionspart" => {
                let p = super::oxml::new_part(super::oxml::CONNECTIONS_PART, "rIdConnections".into());
                book.borrow_mut().ox_conns = Some(p.clone());
                super::oxml::ox(super::oxml::Ox::Part(p))
            }
            "createconnection" => super::oxml::el_with("Connection", &[("id", a.text(0))]),
            "createschemafromtext" => super::oxml::schema_from_text(&a.text(0)),
            _ => return Ok(Out::Unknown),
        });
    }
    let idx = sheet.unwrap_or(0);
    let set = |row: i64, col: &str, value: WValue, format: String| {
        if let Some(sh) = book.borrow_mut().sheets.get_mut(idx) {
            let key = (row.max(0) as u32, col_number(col));
            let preserve = false;
            let _ = preserve;
            sh.cells.insert(key, WCell { value, format });
        }
    };
    let preserve = book.borrow().preserve;
    ok(match m {
        "name" if a.len() == 1 => {
            let name = clean_sheet_name(&a.text(0));
            if let Some(sh) = book.borrow_mut().sheets.get_mut(idx) {
                sh.name = name;
            }
            Val::Void
        }
        "name" => Val::Text(book.borrow().sheets.get(idx).map(|s| s.name.clone()).unwrap_or_default()),
        "setcellvaluetext" | "updatecellvaluetext" => {
            let v = a.text(2);
            if !(preserve && v.is_empty()) {
                set(a.int(0), &a.text(1), WValue::Text(v), "@".into());
            }
            Val::Void
        }
        "setcellvaluenumber" => {
            let v = a.text(2);
            if !(preserve && v.is_empty()) {
                let value = if v.is_empty() { WValue::Number(String::new()) } else { number_value(&v).map(WValue::Number).unwrap_or(WValue::Text(v)) };
                set(a.int(0), &a.text(1), value, a.text(3));
            }
            Val::Void
        }
        "setcellvaluedate" => {
            let v = a.text(2);
            if !(preserve && v.is_empty()) {
                let f = a.text(3);
                set(a.int(0), &a.text(1), date_value(&v), if f.is_empty() { "mm-dd-yy".into() } else { f });
            }
            Val::Void
        }
        "setcellvaluetime" => {
            let v = a.text(2);
            if !(preserve && v.is_empty()) {
                let f = a.text(3);
                set(a.int(0), &a.text(1), time_value(&v), if f.is_empty() { "h:mm:ss".into() } else { f });
            }
            Val::Void
        }
        "setcellvalueboolean" => {
            set(a.int(0), &a.text(1), WValue::Bool(a.bool(2)), String::new());
            Val::Void
        }
        "setcellformula" | "setcellformulawithvalue" => {
            set(a.int(0), &a.text(1), WValue::Formula(a.text(2)), a.text(3));
            Val::Void
        }
        "preservedataonupdate" if a.len() == 1 => {
            book.borrow_mut().preserve = a.bool(0);
            Val::Void
        }
        "preservedataonupdate" => Val::Bool(preserve),
        "defaultcelldecorator" => decorator(false, false, 0),
        "defaultunderlinedcelldecorator" => decorator(false, false, 1),
        "defaultdoubleunderlinedcelldecorator" => decorator(false, false, 2),
        "defaultboldcelldecorator" => decorator(true, false, 0),
        "defaultboldunderlinedcelldecorator" => decorator(true, false, 1),
        "defaultbolddoubleunderlinedcelldecorator" => decorator(true, false, 2),
        "defaultitaliccelldecorator" => decorator(false, true, 0),
        "defaultitalicunderlinedcelldecorator" => decorator(false, true, 1),
        "defaultitalicdoubleunderlinedcelldecorator" => decorator(false, true, 2),
        "defaultbolditaliccelldecorator" => decorator(true, true, 0),
        "defaultbolditalicunderlinedcelldecorator" => decorator(true, true, 1),
        "defaultbolditalicdoubleunderlinedcelldecorator" => decorator(true, true, 2),
        "getnextcolumncellname" => {
            let (r, c) = cell_ref(a.text(0).trim());
            Val::Text(format!("{}{}", col_name(c.unwrap_or(0) as u32 + 1), r.unwrap_or(1)))
        }
        "getnextrowcellname" => {
            let (r, c) = cell_ref(a.text(0).trim());
            Val::Text(format!("{}{}", col_name(c.unwrap_or(1) as u32), r.unwrap_or(0) + 1))
        }
        "addpagesetup" | "addheader" | "setcolumnwidth" | "addrange" | "addrangedatavalidation" | "addhyperlink" | "removehyperlink" | "addautocolumns" | "hidehiddencolumns" => Val::Void,
        "addcustomproperties" => Val::Bool(true),
        // OpenXml SDK parts and elements (oxml.rs); comments and VML drawings stay the neutral shim
        "worksheet" => super::oxml::ox(super::oxml::Ox::WSheet(book.clone(), idx)),
        "createtabledefinitionpart" => {
            let n = book.borrow().sheets.iter().map(|s| s.ox.tables.len()).sum::<usize>() + 1;
            let p = super::oxml::new_part(super::oxml::TABLE_PART, format!("rIdTable{n}"));
            if let Some(sh) = book.borrow_mut().sheets.get_mut(idx) {
                sh.ox.tables.push(p.clone());
            }
            super::oxml::ox(super::oxml::Ox::Part(p))
        }
        "addsinglecelltablepart" => {
            let p = super::oxml::new_part(super::oxml::SINGLE_CELLS_PART, "rIdSingleCells".into());
            if let Some(sh) = book.borrow_mut().sheets.get_mut(idx) {
                sh.ox.single = Some(p.clone());
            }
            super::oxml::ox(super::oxml::Ox::Part(p))
        }
        "createtable" => super::oxml::el_with("Table", &[("id", a.text(0))]),
        "createtablecolumns" => super::oxml::el_with("TableColumns", &[("count", a.text(0))]),
        "createtablecolumn" => super::oxml::el_with("TableColumn", &[("id", a.text(0)), ("name", a.text(1)), ("uniqueName", a.text(2))]),
        "createxmlcolumnproperties" => super::oxml::el_with("XmlColumnProperties", &[("mapId", a.text(0)), ("xpath", a.text(1)), ("xmlDataType", super::oxml::arg_text(a.raw(2)))]),
        "addsinglexmlcell" => super::oxml::el_with("SingleXmlCell", &[("id", a.text(0))]),
        "createtableparts" => super::oxml::el_with("TableParts", &[("count", a.text(0))]),
        "createtablepart" => super::oxml::table_part(&super::oxml::arg_text(a.raw(0))),
        "xmldatatype2xmldatavalues" => net(NetObj::Prim("DocumentFormat.OpenXml.EnumValue`1", Val::Text(a.text(0).to_lowercase()))),
        "getenumxmldatavalues" => net(NetObj::Prim("DocumentFormat.OpenXml.EnumValue`1", Val::Text({
            let t = super::oxml::arg_text(a.raw(0));
            if t.is_empty() { "string".into() } else { t.to_lowercase() }
        }))),
        "createworksheetcommentspart" | "createvmldrawingpart" | "createcommentlist" | "stylemanager" => sink(),
        "dispose" | "close" => Val::Void,
        _ => return Ok(Out::Unknown),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book_bytes() -> Vec<u8> {
        let ct = br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#.to_vec();
        let rels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.to_vec();
        let wb = br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#.to_vec();
        let wrels = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.to_vec();
        let ss = br#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><si><t>Caption</t></si><si><r><t>Ri</t></r><r><t>ch</t></r></si></sst>"#.to_vec();
        let st = br##"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts><numFmt numFmtId="164" formatCode="#,##0.00;-#,##0.00"/></numFmts><cellXfs><xf numFmtId="0"/><xf numFmtId="164"/><xf numFmtId="14"/></cellXfs></styleSheet>"##.to_vec();
        let sh = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1" t="s"><v>1</v></c></row><row r="3"><c r="B3" s="1"><v>1234.5</v></c><c s="2"><v>45000</v></c><c t="inlineStr"><is><t>inl</t></is></c></row></sheetData></worksheet>"#.to_vec();
        zip::write_zip(&[
            ("[Content_Types].xml".into(), ct),
            ("_rels/.rels".into(), rels),
            ("xl/workbook.xml".into(), wb),
            ("xl/_rels/workbook.xml.rels".into(), wrels),
            ("xl/sharedStrings.xml".into(), ss),
            ("xl/styles.xml".into(), st),
            ("xl/worksheets/sheet1.xml".into(), sh),
        ])
    }

    #[test]
    fn writes_and_reads_back() {
        let mut ws = WSheet { name: "Sheet1".into(), id: 1, ..Default::default() };
        ws.cells.insert((1, 1), WCell { value: WValue::Text("Caption & more".into()), format: "@".into() });
        ws.cells.insert((2, 3), WCell { value: WValue::Number("1234.5".into()), format: "#,##0.00".into() });
        ws.cells.insert((2, 4), WCell { value: WValue::Number("46000".into()), format: "dd-mm-yyyy".into() });
        ws.cells.insert((3, 1), WCell { value: WValue::Formula("=SUM(C2:C2)".into()), format: String::new() });
        let b = WBook { sheets: vec![ws], file: None, stream: None, preserve: false, closed: false, ox_maps: None, ox_conns: None };
        let r = read_book(&write_book(&b)).unwrap();
        let cells: Vec<(i64, i64, String, String)> = r.sheets[0].cells.iter().map(|c| {
            let (v, f) = r.value_format(c);
            (c.row, c.col, v, f)
        }).collect();
        assert_eq!(cells[0], (1, 1, "Caption & more".into(), "@".into()));
        assert_eq!(cells[1], (2, 3, "1234.5".into(), "#,##0.00".into()));
        assert_eq!(cells[2], (2, 4, "46000".into(), "dd-mm-yyyy".into()));
        assert_eq!(cells[3].2, "");
        assert_eq!(col_name(28), "AB");
        assert_eq!(col_number("AB"), 28);
    }

    #[test]
    fn reads_cells() {
        let b = read_book(&book_bytes()).unwrap();
        assert_eq!(b.sheets[0].name, "Data");
        assert_eq!(b.sheets[0].rows, 2);
        assert_eq!(b.sheet_index("data"), Some(0));
        let cells: Vec<(i64, i64, String, String)> = b.sheets[0].cells.iter().map(|c| {
            let (v, f) = b.value_format(c);
            (c.row, c.col, v, f)
        }).collect();
        assert_eq!(cells[0], (1, 1, "Caption".into(), "@".into()));
        assert_eq!(cells[1], (1, 3, "Rich".into(), "@".into()));
        assert_eq!(cells[2], (3, 2, "1234.5".into(), "#,##0.00;-#,##0.00".into()));
        assert_eq!(cells[3], (3, 3, "45000".into(), "mm-dd-yy".into()));
        assert_eq!(cells[4], (3, 4, "inl".into(), "@".into()));
    }
}
