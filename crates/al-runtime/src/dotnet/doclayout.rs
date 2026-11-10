//! Report layout managers (Microsoft.Dynamics.Nav.DocumentReport.*ReportManager) and
//! `Report.WordXmlPart`.
//!
//! `Report.WordXmlPart(Id [, Extended])` is Types `OfficeCustomXmlPart.Generate`: an indented UTF-16
//! document `<NavWordReportXmlPart xmlns="urn:microsoft-dynamics-nav/reports/<Name>/<Id>/">` with
//! the `BCReportInformation` block, the report labels (the `labels` section plus the
//! `<column>Caption` labels of IncludeCaption columns) sorted by name, then every top-level data
//! item with its columns sorted by name and its child data items, each element holding its own
//! name as text. The extended form uses the
//! `urn:microsoft-dynamics-businesscentral/internal/extended/reports/` namespace and adds
//! ElementType / ElementId / DataType attributes. Approximations: names go through BC's
//! MangleIntoValidXmlIdentifier rule as seen in real layouts (each run of characters that can't be
//! in an XML name becomes one `_`), BCReportInformation is written without its metadata, request
//! and company sub-elements, and element ids are the column positions.
//!
//! `WordReportManager.NewWordDocumentLayout(OutStream, XmlPart)` writes a blank .docx: an empty
//! body and the XML part as custom XML item 1, which is what Report Layouts Impl. stores when a
//! blank Word layout is created.

use super::*;

pub const WORD_REPORT_MANAGER: &str = "Microsoft.Dynamics.Nav.DocumentReport.WordReportManager";

/// MangleIntoValidXmlIdentifier as seen in BC's layouts ("Standard Sales - Invoice" ->
/// "Standard_Sales_Invoice"): each run of characters that can't be in an XML name is one `_`.
pub fn externalize(name: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in name.chars() {
        if c.is_alphanumeric() || c == '_' {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `Report.WordXmlPart(ReportId [, ExtendedInformation])`.
pub fn word_xml_part(cat: &'static al_model::Catalog, id: i64, extended: bool) -> String {
    let Some(r) = cat.reports.get(&id) else { return String::new() };
    let ns = format!(
        "{}{}/{}/",
        if extended { "urn:microsoft-dynamics-businesscentral/internal/extended/reports/" } else { "urn:microsoft-dynamics-nav/reports/" },
        externalize(&r.name),
        id
    );
    let nl = "\r\n";
    let mut out = format!("<?xml version=\"1.0\" encoding=\"utf-16\"?>{nl}<NavWordReportXmlPart xmlns=\"{ns}\">{nl}  <BCReportInformation />{nl}");
    let attrs = |kind: &str, id: usize, ty: Option<&str>| -> String {
        if !extended {
            return String::new();
        }
        match ty {
            Some(t) => format!(" ElementType=\"{kind}\" ElementId=\"{id}\" DataType=\"{t}\""),
            None => format!(" ElementType=\"{kind}\" ElementId=\"{id}\""),
        }
    };
    // labels: the labels section and the IncludeCaption captions, sorted by name
    let mut labels: Vec<String> = r.labels.iter().map(|l| l.name.clone()).collect();
    for di in &r.dataitems {
        for c in &di.columns {
            if c.include_caption {
                labels.push(format!("{}Caption", c.name));
            }
        }
    }
    labels.sort();
    labels.dedup();
    if !labels.is_empty() {
        out.push_str(&format!("  <Labels>{nl}"));
        for (i, l) in labels.iter().enumerate() {
            let n = externalize(l);
            out.push_str(&format!("    <{n}{}>{}</{n}>{nl}", attrs("Parameter", i + 1, Some("String")), esc(&n)));
        }
        out.push_str(&format!("  </Labels>{nl}"));
    }
    fn item(r: &al_model::report::ReportDef, i: usize, depth: usize, out: &mut String, attrs: &dyn Fn(&str, usize, Option<&str>) -> String) {
        let nl = "\r\n";
        let di = &r.dataitems[i];
        let pad = "  ".repeat(depth);
        let n = externalize(&di.name);
        out.push_str(&format!("{pad}<{n}{}>{nl}", attrs("DataItem", i + 1, None)));
        let mut cols: Vec<&al_model::report::ReportColumn> = di.columns.iter().collect();
        cols.sort_by(|a, b| a.name.cmp(&b.name));
        for (k, c) in cols.iter().enumerate() {
            let cn = externalize(&c.name);
            out.push_str(&format!("{pad}  <{cn}{}>{}</{cn}>{nl}", attrs("Column", k + 1, Some("String")), esc(&cn)));
        }
        for &ch in &di.children {
            item(r, ch, depth + 1, out, attrs);
        }
        out.push_str(&format!("{pad}</{n}>{nl}"));
    }
    for &root in &r.roots {
        item(r, root, 1, &mut out, &attrs);
    }
    out.push_str("</NavWordReportXmlPart>");
    out
}

/// A blank Word document whose custom XML item 1 is `xml_part`.
pub fn blank_docx(xml_part: &str) -> Vec<u8> {
    let part = xml_part.replacen("encoding=\"utf-16\"", "encoding=\"utf-8\"", 1);
    let f = |n: &str, s: &str| (n.to_string(), s.as_bytes().to_vec());
    let entries = vec![
        f("[Content_Types].xml", "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/customXml/itemProps1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.customXmlProperties+xml\"/></Types>"),
        f("_rels/.rels", "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>"),
        f("word/document.xml", "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p/><w:sectPr/></w:body></w:document>"),
        f("word/_rels/document.xml.rels", "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml\" Target=\"../customXml/item1.xml\"/></Relationships>"),
        f("customXml/item1.xml", &part),
        f("customXml/itemProps1.xml", "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?><ds:datastoreItem ds:itemID=\"{00000000-0000-0000-0000-000000000001}\" xmlns:ds=\"http://schemas.openxmlformats.org/officeDocument/2006/customXml\"><ds:schemaRefs/></ds:datastoreItem>"),
        f("customXml/_rels/item1.xml.rels", "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXmlProps\" Target=\"itemProps1.xml\"/></Relationships>"),
    ];
    super::zip::write_zip(&entries)
}

/// WordReportManager statics.
pub fn call(m: &str, a: &Args) -> R<Out> {
    match m {
        "newworddocumentlayout" => {
            let docx = blank_docx(&a.text(1));
            if let Some(t) = a.raw(0) {
                super::text::stream_write(t, &docx);
            }
            void()
        }
        _ => Ok(Out::Unknown),
    }
}
