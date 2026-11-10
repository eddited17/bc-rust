//! RDLC page layout and Excel cells (stages 4 and 5 of the report layout engine; see
//! docs/reports.md).
//!
//! Stage 4 places the bound items: within a container, items keep their Top; an item that grows (a
//! tablix by its bound rows, a rectangle by its children) pushes the items below it down by its
//! growth (RDLC's vertical growth rule). Positions become absolute 20ths of a point, converted per
//! item as ReportViewer's Excel LayoutEngine does (left/top and width/height each converted).
//!
//! Stage 5 is LayoutEngine's grid: every placed item (rectangles and tablixes too) adds its edges;
//! edge 0 always; the sorted distinct edges are the columns and rows; a textbox's value goes to the
//! cell at its top-left edge, merged over the cells up to its right/bottom edge.

use crate::rdlc::mm_to_twips;
use crate::rdlc_bind::{BKind, BNode, ColType, Lang, ValType};
use crate::rdlc_expr::V;
use crate::rdlc_numfmt::{ExcelNumFmt, NumKind};

/// A textbox's content as the sheet needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct TextOut {
    pub value: V,
    /// The Format, as evaluated (not trimmed).
    pub format: Option<String>,
    /// The textbox's Language (None: the report's).
    pub language: Option<Lang>,
    pub vtype: ValType,
}

/// A placed item (twips, absolute).
#[derive(Debug, Clone)]
pub struct Placed {
    pub top: i32,
    pub left: i32,
    pub width: i32,
    pub height: i32,
    /// A textbox's content; None for containers.
    pub value: Option<TextOut>,
    /// The page (worksheet) it is on.
    pub page: usize,
}

/// The pages of the export: a group's page break starts a new page (BC writes one worksheet per
/// page); what follows moves up so the break's row starts at the top of the body.
#[derive(Debug, Default, Clone)]
pub struct Pager {
    pub page: usize,
    /// twips taken off absolute tops on the current page
    shift: i32,
    /// where the body starts on every page (below the page header)
    pub body_top: i32,
}

/// LayoutConvert.ConvertMMTo20thPointsUnrounded.
fn twips_f(mm: f64) -> f64 {
    mm / 25.4 * 72.0 * 20.0
}

/// Math.Round (to even), as ReportViewer rounds tablix edges.
fn round_even(x: f64) -> i32 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
        (r - x.signum()) as i32
    } else {
        r as i32
    }
}

/// Place bound items whose container's top-left is at (`y0`, `x0`) twips; returns the height
/// (twips) the items use below `y0`. One page.
pub fn place(nodes: &[BNode], y0: i32, x0: i32, out: &mut Vec<Placed>) -> i32 {
    place_pg(nodes, y0, x0, out, &mut Pager::default())
}

/// `place` across pages.
pub fn place_pg(nodes: &[BNode], y0: i32, x0: i32, out: &mut Vec<Placed>, pg: &mut Pager) -> i32 {
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by(|a, b| nodes[*a].top.partial_cmp(&nodes[*b].top).unwrap_or(std::cmp::Ordering::Equal));
    // (original bottom, growth) of the items placed so far: items below them move down
    let mut growth: Vec<(i32, i32)> = vec![];
    let mut extent = 0i32;
    for i in order {
        let n = &nodes[i];
        let (t, h) = (mm_to_twips(n.top), mm_to_twips(n.height));
        let shift: i32 = growth.iter().filter(|(bottom, _)| *bottom <= t).map(|(_, d)| *d).sum();
        let used = place_node(n, y0 + t + shift, x0 + mm_to_twips(n.left), mm_to_twips(n.width), h, out, pg);
        if used > h {
            growth.push((t + h, used - h));
        }
        extent = extent.max(t + shift + used.max(h));
    }
    extent
}

/// Place one node at absolute (`top`, `left`) twips with its size; returns its height after growth.
fn place_node(n: &BNode, top: i32, left: i32, width: i32, height: i32, out: &mut Vec<Placed>, pg: &mut Pager) -> i32 {
    // SPBIF Reader drops an item of zero width or height (a line only when both are zero), and a
    // tablix cell of zero width or height: it adds no edges and no cell
    let zero = match n.kind {
        BKind::Line => width == 0 && height == 0,
        BKind::Rows { .. } => false,
        _ => width == 0 || height == 0,
    };
    if zero {
        return height;
    }
    let at = out.len();
    out.push(Placed { top: top - pg.shift, left, width, height, value: None, page: pg.page });
    let used = match &n.kind {
        BKind::Text { value, format, language, vtype } => {
            out[at].value = Some(TextOut { value: value.clone(), format: format.clone(), language: language.clone(), vtype: *vtype });
            height
        }
        BKind::Box(children) => place_pg(children, top, left, out, pg).max(height),
        BKind::Rows { columns, rows } => {
            // column and row edges: running unrounded sums from the tablix's origin, rounded per edge
            // (ExcelRenderer.SPBIF.Reader)
            let mut cols = vec![left];
            let mut acc = left as f64;
            for w in columns {
                acc += twips_f(*w);
                cols.push(round_even(acc));
            }
            let mut y = top as f64;
            let mut row_top = top;
            let mut first_break: Option<i32> = None;
            for r in rows {
                // a group's page break: this row starts the next page (not when it is at the top of
                // the body already)
                if r.page_break_before && row_top - pg.shift > pg.body_top {
                    first_break.get_or_insert(row_top);
                    pg.page += 1;
                    pg.shift = row_top - pg.body_top;
                }
                y += twips_f(r.height);
                let row_bottom = round_even(y);
                for c in &r.cells {
                    let (c0, c1) = (c.col.min(cols.len() - 1), (c.col + c.col_span).min(cols.len() - 1));
                    if let Some(content) = &c.content {
                        place_node(content, row_top, cols[c0], cols[c1] - cols[c0], row_bottom - row_top, out, pg);
                    }
                }
                row_top = row_bottom;
            }
            out[at].width = cols.last().copied().unwrap_or(left) - left;
            if let Some(b) = first_break {
                // on its first page the tablix ends at the break
                out[at].height = b - top;
                return row_top - top;
            }
            row_top - top
        }
        BKind::Image | BKind::Line => height,
    };
    out[at].height = used;
    used
}

/// A sheet cell.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub row: usize,
    pub col: usize,
    pub value: V,
    pub format: Option<String>,
    /// The textbox's Language (None: the report's).
    pub language: Option<Lang>,
    /// What the value is (number cell or text cell, integral or not).
    pub vtype: ValType,
    /// Merged range (last row, last col) when the item spans more than one cell.
    pub merge_to: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Default)]
pub struct Sheet {
    /// Column widths and row heights (twips).
    pub col_widths: Vec<i32>,
    pub row_heights: Vec<i32>,
    pub cells: Vec<Cell>,
}

/// LayoutEngine's grid over placed items.
pub fn sheet(placed: &[Placed]) -> Sheet {
    let mut ce = vec![0];
    let mut re = vec![0];
    for p in placed {
        ce.push(p.left);
        ce.push(p.left + p.width);
        re.push(p.top);
        re.push(p.top + p.height);
    }
    ce.sort_unstable();
    ce.dedup();
    re.sort_unstable();
    re.dedup();
    let at = |edges: &[i32], e: i32| edges.binary_search(&e).unwrap_or_else(|i| i.min(edges.len() - 1));
    let mut cells = vec![];
    for p in placed {
        let Some(TextOut { value, format, language, vtype }) = &p.value else { continue };
        if matches!(value, V::Null) || value.as_text().is_empty() {
            continue;
        }
        let (c0, c1) = (at(&ce, p.left), at(&ce, p.left + p.width).max(1) - 1);
        let (r0, r1) = (at(&re, p.top), at(&re, p.top + p.height).max(1) - 1);
        let merge_to = (c1 > c0 || r1 > r0).then_some((r1.max(r0), c1.max(c0)));
        // one cell per address: the first item that starts there (overlapping items don't add a
        // second cell)
        if cells.iter().any(|c: &Cell| c.row == r0 && c.col == c0) {
            continue;
        }
        cells.push(Cell { row: r0, col: c0, value: value.clone(), format: format.clone(), language: language.clone(), vtype: *vtype, merge_to });
    }
    Sheet {
        col_widths: ce.windows(2).map(|w| w[1] - w[0]).collect(),
        row_heights: re.windows(2).map(|w| (w[1] - w[0]).min(8180)).collect(),
        cells,
    }
}

/// The cell reference of a 0-based (row, col): "AK45".
pub fn cell_ref(row: usize, col: usize) -> String {
    format!("{}{}", crate::rdlc::column_letters(col), row + 1)
}

/// The report's rendered rows (report.rs Dataset::lines: depth, data item, column name/value) as
/// BC's flat dataset: one row per line that no deeper line follows, with its ancestors' columns.
/// Column types are left Unknown (Dataset::with_types sets them).
pub fn flat_rows(lines: &[(usize, &'static str, Vec<(&'static str, String)>)]) -> crate::rdlc_bind::Dataset {
    let mut columns: Vec<String> = vec![];
    let mut rows: Vec<Vec<(String, String)>> = vec![];
    let mut stack: Vec<&Vec<(&'static str, String)>> = vec![];
    for (i, (depth, _, cols)) in lines.iter().enumerate() {
        stack.truncate(*depth);
        stack.push(cols);
        let leaf = lines.get(i + 1).is_none_or(|(d, _, _)| *d <= *depth);
        if !leaf {
            continue;
        }
        let row: Vec<(String, String)> = stack.iter().flat_map(|c| c.iter().map(|(n, v)| (n.to_string(), v.clone()))).collect();
        for (n, _) in &row {
            if !columns.iter().any(|c| c == n) {
                columns.push(n.clone());
            }
        }
        rows.push(row);
    }
    let rows = rows
        .into_iter()
        .map(|r| columns.iter().map(|c| r.iter().find(|(n, _)| n == c).map(|(_, v)| typed(v)).unwrap_or(V::Null)).collect())
        .collect();
    crate::rdlc_bind::Dataset::new(columns, rows)
}

/// A dataset value as the layout sees it: report.rs writes Boolean columns as True/False (their
/// RDLC type is Boolean); everything else stays text and converts where an expression needs it.
fn typed(v: &str) -> V {
    match v {
        "True" => V::Bool(true),
        "False" => V::Bool(false),
        _ => V::Text(v.to_string()),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A number cell's value text: the number as VB writes it (no grouping, invariant).
fn number_text(v: &V) -> Option<String> {
    let t = v.as_text();
    let t = t.trim();
    if t.is_empty() || t.starts_with('0') && t.len() > 1 && !t.starts_with("0.") {
        return None; // "00123" stays text (codes)
    }
    let n: f64 = t.parse().ok()?;
    Some(crate::rdlc_expr::fmt_num((n * 1e10).round() / 1e10))
}

/// A number cell's format (rdlc_numfmt): the textbox's Format in the cell's culture (its Language,
/// else the report's), for its kind of number. A culture that is an assumption (O-2) is recorded
/// only when the conversion reads culture data.
fn cell_format(c: &Cell, report_lang: &Lang) -> ExcelNumFmt {
    let Some(f) = c.format.as_deref() else { return ExcelNumFmt::General };
    let lang = c.language.as_ref().unwrap_or(report_lang);
    let kind = if c.vtype == ValType::Integral { NumKind::Integral } else { NumKind::Fractional };
    let (fmt, culture_read) = crate::rdlc_numfmt::to_excel_in(f, &lang.name, kind);
    if culture_read && lang.assumed {
        crate::assume::hit("U2.UserLanguage");
    }
    fmt
}

/// The workbook (one worksheet) of a sheet.
pub fn workbook(sheet: &Sheet) -> Vec<u8> {
    workbook_pages(std::slice::from_ref(sheet))
}

/// One worksheet's XML; number formats go into `fmts` (custom numFmt ids from 164, shared by the
/// workbook's sheets). `report_lang`: the culture of textboxes without a Language.
fn worksheet_xml(sheet: &Sheet, fmts: &mut Vec<String>, report_lang: &Lang) -> String {
    let mut by_row: std::collections::BTreeMap<usize, Vec<&Cell>> = Default::default();
    for c in &sheet.cells {
        by_row.entry(c.row).or_default().push(c);
    }
    let mut sd = String::new();
    for r in 0..sheet.row_heights.len().max(by_row.keys().last().map_or(0, |r| r + 1)) {
        let ht = sheet.row_heights.get(r).map(|h| format!(" ht=\"{}\" customHeight=\"1\"", *h as f64 / 20.0)).unwrap_or_default();
        let Some(cells) = by_row.get_mut(&r) else {
            sd.push_str(&format!("<row r=\"{}\"{ht}/>", r + 1));
            continue;
        };
        cells.sort_by_key(|c| c.col);
        sd.push_str(&format!("<row r=\"{}\"{ht}>", r + 1));
        for c in cells.iter() {
            let rf = cell_ref(c.row, c.col);
            // only numbers get a number format (R-CELL-1); a value of an AL text type stays a text
            // cell even when it looks like a number (W-TEXTNUM; T4 probe P4: Code and Text values
            // such as 10000, 01234, -5, 12.50 are inline strings under every Format)
            let number = number_text(&c.value).filter(|_| matches!(c.value, V::Num(_) | V::Text(_)));
            match number.filter(|_| c.vtype != ValType::Text) {
                Some(n) => {
                    // General: no style; a code: one cellXfs entry per distinct code
                    let style = match cell_format(c, report_lang) {
                        ExcelNumFmt::General => String::new(),
                        ExcelNumFmt::Code(code) => {
                            let k = fmts.iter().position(|f| *f == code).unwrap_or_else(|| {
                                fmts.push(code);
                                fmts.len() - 1
                            });
                            format!(" s=\"{}\"", k + 1)
                        }
                    };
                    sd.push_str(&format!("<c r=\"{rf}\"{style}><v>{n}</v></c>"))
                }
                None => sd.push_str(&format!("<c r=\"{rf}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>", esc(&c.value.as_text()))),
            }
        }
        sd.push_str("</row>");
    }
    // column widths in characters (Excel's default font: about 7 px per character at 96 dpi)
    let cols: String = sheet
        .col_widths
        .iter()
        .enumerate()
        .map(|(i, w)| format!("<col min=\"{0}\" max=\"{0}\" width=\"{1:.2}\" customWidth=\"1\"/>", i + 1, (*w as f64 / 20.0 * 96.0 / 72.0 / 7.0).max(0.1)))
        .collect();
    let merges: Vec<String> = sheet.cells.iter().filter_map(|c| c.merge_to.map(|(r1, c1)| format!("<mergeCell ref=\"{}:{}\"/>", cell_ref(c.row, c.col), cell_ref(r1, c1)))).collect();
    let merge_xml = if merges.is_empty() { String::new() } else { format!("<mergeCells count=\"{}\">{}</mergeCells>", merges.len(), merges.join("")) };
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">{}<sheetData>{sd}</sheetData>{merge_xml}</worksheet>", if cols.is_empty() { String::new() } else { format!("<cols>{cols}</cols>") })
}

/// The workbook of a report's pages, one worksheet each (BC's Excel export writes one per page),
/// in the invariant culture.
pub fn workbook_pages(sheets: &[Sheet]) -> Vec<u8> {
    workbook_pages_in(sheets, &Lang::default())
}

/// `numFmt` formatCode text: tab, CR and LF as character references, so the attribute keeps them.
fn esc_code(s: &str) -> String {
    esc(s).replace('\t', "&#x9;").replace('\n', "&#xA;").replace('\r', "&#xD;")
}

/// [workbook_pages] with the culture of textboxes without a Language of their own.
pub fn workbook_pages_in(sheets: &[Sheet], report_lang: &Lang) -> Vec<u8> {
    let mut fmts: Vec<String> = vec![];
    let wss: Vec<String> = sheets.iter().map(|s| worksheet_xml(s, &mut fmts, report_lang)).collect();
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">{}<fonts count=\"1\"><font><sz val=\"10\"/><name val=\"Arial\"/></font></fonts><fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills><borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs><cellXfs count=\"{}\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>{}</cellXfs></styleSheet>",
        if fmts.is_empty() { String::new() } else { format!("<numFmts count=\"{}\">{}</numFmts>", fmts.len(), fmts.iter().enumerate().map(|(i, f)| format!("<numFmt numFmtId=\"{}\" formatCode=\"{}\"/>", 164 + i, esc_code(f))).collect::<String>()) },
        fmts.len() + 1,
        (0..fmts.len()).map(|i| format!("<xf numFmtId=\"{}\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/>", 164 + i)).collect::<String>()
    );
    let n = wss.len().max(1);
    let sheet_list: String = (1..=n).map(|i| format!("<sheet name=\"Sheet{i}\" sheetId=\"{i}\" r:id=\"rId{i}\"/>")).collect();
    let wb = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>{sheet_list}</sheets></workbook>");
    let sheet_rels: String = (1..=n).map(|i| format!("<Relationship Id=\"rId{i}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{i}.xml\"/>")).collect();
    let wbrels = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{sheet_rels}<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>", n + 1);
    let sheet_types: String = (1..=n).map(|i| format!("<Override PartName=\"/xl/worksheets/sheet{i}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>")).collect();
    let ct = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>{sheet_types}<Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/></Types>");
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>";
    let mut parts: Vec<(String, Vec<u8>)> = vec![
        ("[Content_Types].xml".into(), ct.into_bytes()),
        ("_rels/.rels".into(), rels.as_bytes().to_vec()),
        ("xl/workbook.xml".into(), wb.into_bytes()),
        ("xl/_rels/workbook.xml.rels".into(), wbrels.into_bytes()),
    ];
    if wss.is_empty() {
        parts.push(("xl/worksheets/sheet1.xml".into(), worksheet_xml(&Sheet::default(), &mut vec![], report_lang).into_bytes()));
    }
    for (i, ws) in wss.into_iter().enumerate() {
        parts.push((format!("xl/worksheets/sheet{}.xml", i + 1), ws.into_bytes()));
    }
    parts.push(("xl/styles.xml".into(), styles.into_bytes()));
    crate::dotnet::zip::write_zip(&parts)
}

/// What a report run hands the Excel export besides the layout, its rows and labels.
#[derive(Debug, Default, Clone)]
pub struct ExportEnv {
    /// The report's language tag: what `User!Language` evaluates to (T4 probe P3: en-US in a
    /// session in 1033, whatever CurrReport.FormatRegion says).
    pub user_language: String,
    /// The report's FormatRegion (CurrReport.FormatRegion, else its language's tag): the culture of
    /// textboxes without any Language (an assumption, O-2).
    pub format_region: String,
    /// Each dataset column's AL type class, by name (others are Unknown).
    pub column_types: Vec<(String, ColType)>,
}

/// Render an RDLC layout to an Excel workbook, as BC's Excel export lays it out: the page header
/// above the body in one grid, statically and dynamically hidden items left out.
/// `labels` are the report's labels and column captions (RDLC's parameters). Without a report
/// run's [ExportEnv]: no format region, column types unknown.
pub fn excel(rdlc_xml: &str, lines: &[(usize, &'static str, Vec<(&'static str, String)>)], labels: &[(String, String)], user_id: &str) -> Result<Vec<u8>, String> {
    excel_with(rdlc_xml, lines, labels, user_id, &ExportEnv::default())
}

/// [excel] for a report run: its format region and its dataset's column types.
pub fn excel_with(rdlc_xml: &str, lines: &[(usize, &'static str, Vec<(&'static str, String)>)], labels: &[(String, String)], user_id: &str, env: &ExportEnv) -> Result<Vec<u8>, String> {
    let layout = crate::rdlc::parse(rdlc_xml)?;
    let ds = flat_rows(lines).with_types(&env.column_types);
    let mut st = crate::rdlc_bind::RunState::with_code(&layout.code);
    st.user_id = user_id.to_string();
    st.user_language = env.user_language.clone();
    for (n, c) in labels {
        st.params.insert(n.clone(), V::Text(c.clone()));
    }
    let report_lang = crate::rdlc_bind::report_language(layout.language.as_deref(), &ds, &mut st, &env.format_region);
    let nodes = crate::rdlc_bind::bind_body(&layout, &ds, &mut st)?;
    let mut placed = vec![];
    // the page header is in the grid above the body (ReportViewer's LayoutEngine without
    // SimplePageHeaders: Reader adds the header's height to the body's top)
    let y0 = layout.page_header.as_ref().map_or(0, |h| mm_to_twips(h.height));
    let mut pg = Pager { body_top: y0, ..Default::default() };
    place_pg(&nodes, y0, 0, &mut placed, &mut pg);
    let pages = pg.page + 1;
    // each page's header, after that page's body: its Code.GetData sees what that page's
    // Code.SetData stored; Globals!PageNumber / TotalPages
    if let Some(h) = &layout.page_header {
        for p in 0..pages {
            st.set_page(p, pages);
            let hn = crate::rdlc_bind::bind_section(&h.items, &ds, &mut st)?;
            place_pg(&hn, 0, 0, &mut placed, &mut Pager { page: p, ..Default::default() });
        }
    }
    let sheets: Vec<Sheet> = (0..pages).map(|p| sheet(&placed.iter().filter(|x| x.page == p).cloned().collect::<Vec<_>>())).collect();
    if let Ok(path) = std::env::var("BC_RDLC_DUMP") {
        // debugging aid: the sheets' cells, appended to the file the variable names
        use std::io::Write;
        let mut s = String::new();
        for (p, sh) in sheets.iter().enumerate() {
            s.push_str(&format!("=== page {} of {}: {} rows {} cols {} cells\n", p + 1, pages, sh.row_heights.len(), sh.col_widths.len(), sh.cells.len()));
            for c in &sh.cells {
                s.push_str(&format!("{} {:?}{}\n", cell_ref(c.row, c.col), c.value, c.merge_to.map(|(r, k)| format!(" ..{}", cell_ref(r, k))).unwrap_or_default()));
            }
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(s.as_bytes());
        }
    }
    Ok(workbook_pages_in(&sheets, &report_lang))
}
