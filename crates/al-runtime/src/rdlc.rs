//! RDLC layout model and the static Excel export grid (first stage of the report layout engine; see
//! docs/reports.md). rdlc_expr, rdlc_bind and rdlc_xlsx bind it to the dataset and write the workbook.
//!
//! BC renders RDLC layouts with Microsoft ReportViewer. Its Excel export
//! (Microsoft.ReportingServices.Rendering.ExcelRenderer.Layout.LayoutEngine, in
//! Microsoft.ReportViewer.Common.dll) lays every report item of a page into one grid: each item adds
//! its left and right edge as column edges and its top and bottom edge as row edges, in 20ths of a
//! point (LayoutConvert.ConvertMMTo20thPoints); edge 0 is always one; the sorted distinct edges are
//! the sheet's columns and rows, and an item covers the cells from its left/top edge up to its
//! right/bottom edge (merged when more than one). The page header is in that grid, above the body
//! (ReportViewer without SimplePageHeaders: ERM Fixed Assets Reports' Book Value 02 tests read the
//! header's cells). The grid has only the items present at run time, so it comes from the bound
//! items (rdlc_xlsx), not from the layout alone.
//!
//! This module parses the layout: sections, report items, tablix columns/rows/cells (one TablixCell
//! per column; a cell spanning N columns is followed by N-1 empty placeholders), row and column
//! hierarchies with groups, filters and Hidden expressions, Format and Language, and the `<Code>` block.

use quick_xml::events::{BytesStart, Event};

// ------------------------------------------------------------------ generic XML tree

#[derive(Debug, Default, Clone)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    pub text: String,
}

impl Node {
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    /// Text of the child element `name`.
    pub fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.as_str())
    }
    /// The descendant at a path of element names.
    pub fn path(&self, names: &[&str]) -> Option<&Node> {
        let mut n = self;
        for p in names {
            n = n.child(p)?;
        }
        Some(n)
    }
}

fn node_of(e: &BytesStart) -> Node {
    let name = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
    let attrs = e
        .attributes()
        .flatten()
        .map(|a| (String::from_utf8_lossy(a.key.local_name().as_ref()).to_string(), a.unescape_value().map(|v| v.to_string()).unwrap_or_default()))
        .collect();
    Node { name, attrs, ..Default::default() }
}

/// Parse an XML document into a tree (element local names, attributes, concatenated text).
pub fn parse_tree(xml: &str) -> Result<Node, String> {
    let mut reader = quick_xml::Reader::from_str(xml.trim_start_matches('\u{feff}'));
    let mut stack: Vec<Node> = vec![Node { name: "#document".into(), ..Default::default() }];
    loop {
        match reader.read_event().map_err(|e| format!("RDLC XML: {e}"))? {
            Event::Start(e) => stack.push(node_of(&e)),
            Event::Empty(e) => {
                let n = node_of(&e);
                stack.last_mut().ok_or("RDLC XML: unbalanced")?.children.push(n);
            }
            Event::End(_) => {
                let n = stack.pop().ok_or("RDLC XML: unbalanced")?;
                stack.last_mut().ok_or("RDLC XML: unbalanced")?.children.push(n);
            }
            Event::Text(t) => {
                let s = t.unescape().map_err(|e| format!("RDLC XML: {e}"))?;
                if let Some(n) = stack.last_mut() {
                    n.text.push_str(&s);
                }
            }
            Event::CData(c) => {
                if let Some(n) = stack.last_mut() {
                    n.text.push_str(&String::from_utf8_lossy(&c));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut doc = stack.pop().ok_or("RDLC XML: empty")?;
    if !stack.is_empty() {
        return Err("RDLC XML: unclosed elements".into());
    }
    doc.children.pop().ok_or_else(|| "RDLC XML: no root element".into())
}

// ------------------------------------------------------------------ sizes

/// An RDLC size ("2.5cm", "1in", "10mm", "12pt", "1pc") in millimeters (ReportSize).
pub fn size_mm(s: &str) -> f64 {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let v: f64 = num.trim().parse().unwrap_or(0.0);
    match unit.trim().to_ascii_lowercase().as_str() {
        "in" => v * 25.4,
        "cm" => v * 10.0,
        "mm" => v,
        "pt" => v * 25.4 / 72.0,
        "pc" => v * 25.4 / 6.0,
        _ => v,
    }
}

/// LayoutConvert.ConvertMMTo20thPoints: millimeters to 20ths of a point (twips), rounded.
pub fn mm_to_twips(mm: f64) -> i32 {
    let v = mm / 25.4 * 72.0 * 20.0 + 0.00051;
    // Math.Round(v, 3) then the int cast (truncation toward zero)
    ((v * 1000.0).round() / 1000.0) as i32
}

// ------------------------------------------------------------------ layout model

#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub kind: Kind,
    /// Position relative to the containing item (mm); 0 for a tablix cell's content.
    pub top: f64,
    pub left: f64,
    pub width: f64,
    pub height: f64,
    /// Visibility/Hidden: "true"/"false" or an expression.
    pub hidden: Option<String>,
}

impl Item {
    /// Hidden = true (constant): ReportViewer does not lay the item out, so it adds no edges.
    pub fn statically_hidden(&self) -> bool {
        self.hidden.as_deref().is_some_and(|h| h.trim().eq_ignore_ascii_case("true"))
    }
}

#[derive(Debug, Clone)]
pub enum Kind {
    /// `format` and `language`: the TextRun Style's, else the Textbox Style's (Style.Format,
    /// Style.Language; [MS-RDL]), as written (a literal or an expression).
    Textbox { value: String, format: Option<String>, language: Option<String> },
    Rectangle(Vec<Item>),
    Tablix(Tablix),
    Image,
    Line,
    Other(String),
}

#[derive(Debug, Clone, Default)]
pub struct Tablix {
    /// TablixColumns widths (mm).
    pub columns: Vec<f64>,
    pub rows: Vec<TablixRow>,
    /// TablixRowHierarchy: leaf members correspond, in order, to `rows`.
    pub row_members: Vec<Member>,
    /// TablixColumnHierarchy: leaf members correspond, in order, to `columns`.
    pub col_members: Vec<Member>,
    pub dataset: Option<String>,
    pub filters: Vec<Filter>,
}

/// A tablix member: static (no group, one instance) or dynamic (one instance per group value; a
/// group without GroupExpressions is the details group: one instance per row).
#[derive(Debug, Clone, Default)]
pub struct Member {
    pub group: Option<Group>,
    pub children: Vec<Member>,
    pub hidden: Option<String>,
    /// TablixHeader: the header cell's size (mm) and content (row headers left of the body).
    pub header: Option<(f64, Option<Item>)>,
}

impl Member {
    /// Number of leaf members (body rows or columns) under this member.
    pub fn leaves(&self) -> usize {
        if self.children.is_empty() {
            1
        } else {
            self.children.iter().map(|c| c.leaves()).sum()
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Group {
    pub name: String,
    pub exprs: Vec<String>,
    pub filters: Vec<Filter>,
    /// PageBreak: BreakLocation (lower case: between, start, end, startandend) and the Disabled
    /// expression, if any.
    pub page_break: Option<(String, Option<String>)>,
}

/// A data region or group filter: `expr operator values` (Equal, NotEqual, GreaterThan, In, Like ...).
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub expr: String,
    pub op: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TablixRow {
    pub height: f64,
    pub cells: Vec<TablixCell>,
}

#[derive(Debug, Clone, Default)]
pub struct TablixCell {
    pub item: Option<Item>,
    pub col_span: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Section {
    pub height: f64,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub body: Section,
    pub page_header: Option<Section>,
    pub page_footer: Option<Section>,
    /// Report width (mm): the body's width.
    pub width: f64,
    /// The layout's custom code (`<Code>`, VB.NET).
    pub code: String,
    /// Report.Language as written (a literal or an expression, usually `=User!Language`): the
    /// culture of textboxes without a Language of their own ([MS-RDL] Style.Language).
    pub language: Option<String>,
}

fn dim(n: &Node, name: &str) -> f64 {
    n.text_of(name).map(size_mm).unwrap_or(0.0)
}

fn report_items(n: Option<&Node>) -> Vec<Item> {
    n.map(|ri| ri.children.iter().filter_map(item_of).collect()).unwrap_or_default()
}

fn item_of(n: &Node) -> Option<Item> {
    let kind = match n.name.as_str() {
        "Textbox" => {
            // the first TextRun's value (paragraphs of one textbox are joined by the renderer)
            let mut values = vec![];
            if let Some(ps) = n.child("Paragraphs") {
                for p in ps.children_named("Paragraph") {
                    if let Some(trs) = p.child("TextRuns") {
                        for tr in trs.children_named("TextRun") {
                            let style = |name: &str| tr.path(&["Style", name]).map(|f| f.text.clone());
                            values.push((tr.text_of("Value").unwrap_or_default().to_string(), style("Format"), style("Language")));
                        }
                    }
                }
            }
            let format = values.iter().find_map(|(_, f, _)| f.clone()).or_else(|| n.path(&["Style", "Format"]).map(|f| f.text.clone()));
            // [MS-RDL] Style.Language: TextRun level, else Textbox level (else the report's)
            let language = values.iter().find_map(|(_, _, l)| l.clone()).or_else(|| n.path(&["Style", "Language"]).map(|f| f.text.clone()));
            let value = values.into_iter().map(|(v, _, _)| v).collect::<Vec<_>>().join("");
            Kind::Textbox { value, format, language }
        }
        "Rectangle" => Kind::Rectangle(report_items(n.child("ReportItems"))),
        "Tablix" => Kind::Tablix(tablix_of(n)),
        "Image" => Kind::Image,
        "Line" => Kind::Line,
        "Subreport" | "Chart" | "GaugePanel" | "Map" | "CustomReportItem" => Kind::Other(n.name.clone()),
        _ => return None,
    };
    Some(Item {
        name: n.attr("Name").unwrap_or_default().to_string(),
        kind,
        top: dim(n, "Top"),
        left: dim(n, "Left"),
        width: dim(n, "Width"),
        height: dim(n, "Height"),
        hidden: n.path(&["Visibility", "Hidden"]).map(|h| h.text.trim().to_string()),
    })
}

fn filters_of(n: Option<&Node>) -> Vec<Filter> {
    n.map(|fs| {
        fs.children_named("Filter")
            .map(|f| Filter {
                expr: f.text_of("FilterExpression").unwrap_or_default().to_string(),
                op: f.text_of("Operator").unwrap_or("Equal").to_string(),
                values: f.child("FilterValues").map(|vs| vs.children_named("FilterValue").map(|v| v.text.clone()).collect()).unwrap_or_default(),
            })
            .collect()
    })
    .unwrap_or_default()
}

fn members_of(n: Option<&Node>) -> Vec<Member> {
    n.and_then(|h| h.child("TablixMembers"))
        .map(|ms| {
            ms.children_named("TablixMember")
                .map(|m| Member {
                    group: m.child("Group").map(|g| Group {
                        name: g.attr("Name").unwrap_or_default().to_string(),
                        exprs: g.child("GroupExpressions").map(|es| es.children_named("GroupExpression").map(|e| e.text.clone()).collect()).unwrap_or_default(),
                        filters: filters_of(g.child("Filters")),
                        page_break: g.child("PageBreak").and_then(|pb| {
                            let at = pb.text_of("BreakLocation")?.trim().to_ascii_lowercase();
                            Some((at, pb.text_of("Disabled").map(|d| d.trim().to_string())))
                        }),
                    }),
                    children: members_of(Some(m)),
                    hidden: m.path(&["Visibility", "Hidden"]).map(|h| h.text.trim().to_string()),
                    header: m.child("TablixHeader").map(|h| (dim(h, "Size"), h.child("CellContents").and_then(|cc| cc.children.iter().find_map(item_of)))),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tablix_of(n: &Node) -> Tablix {
    let body = n.child("TablixBody");
    let columns = body
        .and_then(|b| b.child("TablixColumns"))
        .map(|cs| cs.children_named("TablixColumn").map(|c| dim(c, "Width")).collect())
        .unwrap_or_default();
    let rows = body
        .and_then(|b| b.child("TablixRows"))
        .map(|rs| {
            rs.children_named("TablixRow")
                .map(|r| TablixRow {
                    height: dim(r, "Height"),
                    cells: r
                        .child("TablixCells")
                        .map(|cs| {
                            cs.children_named("TablixCell")
                                .map(|c| {
                                    let contents = c.child("CellContents");
                                    TablixCell {
                                        item: contents.and_then(|cc| cc.children.iter().find_map(item_of)),
                                        col_span: contents.and_then(|cc| cc.text_of("ColSpan")).and_then(|v| v.trim().parse().ok()).unwrap_or(1),
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    Tablix {
        columns,
        rows,
        row_members: members_of(n.child("TablixRowHierarchy")),
        col_members: members_of(n.child("TablixColumnHierarchy")),
        dataset: n.text_of("DataSetName").map(|s| s.to_string()),
        filters: filters_of(n.child("Filters")),
    }
}

fn section_of(n: Option<&Node>) -> Option<Section> {
    let n = n?;
    Some(Section { height: dim(n, "Height"), items: report_items(n.child("ReportItems")) })
}

/// Parse an RDLC file (2008/2010/2016 schemas: Report/Body or Report/ReportSections/ReportSection).
pub fn parse(xml: &str) -> Result<Layout, String> {
    let root = parse_tree(xml)?;
    if root.name != "Report" {
        return Err(format!("RDLC: root element is {}, not Report", root.name));
    }
    let sect = root.path(&["ReportSections", "ReportSection"]).unwrap_or(&root);
    let body = section_of(sect.child("Body")).unwrap_or_default();
    let page = sect.child("Page");
    Ok(Layout {
        body,
        page_header: section_of(page.and_then(|p| p.child("PageHeader")).or_else(|| sect.child("PageHeader"))),
        page_footer: section_of(page.and_then(|p| p.child("PageFooter")).or_else(|| sect.child("PageFooter"))),
        width: dim(sect, "Width"),
        code: root.text_of("Code").unwrap_or_default().to_string(),
        language: root.text_of("Language").map(|l| l.to_string()),
    })
}

// ------------------------------------------------------------------ cell addresses

/// Excel column letters of a 0-based column index (0 = A, 26 = AA).
pub fn column_letters(mut i: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push((b'A' + (i % 26) as u8) as char);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    s.iter().rev().collect()
}
