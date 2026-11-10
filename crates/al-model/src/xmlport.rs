//! XMLport metadata: the schema tree (text/table/field elements and attributes) with the
//! properties the runtime needs to import and export, and the node triggers registered as
//! procedures of the XMLport's code object.
//!
//! Like report data items, every `tableelement` becomes a global record variable of the XMLport
//! (named like the node, `UseTemporary` makes it temporary) and every `textelement` /
//! `textattribute` a global Text variable, so trigger code can address them by name.

use crate::catalog::*;
use crate::filters::*;
use crate::props::*;
use al_syntax::*;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum XKind {
    TextElement,
    TableElement,
    FieldElement,
    TextAttribute,
    FieldAttribute,
}

impl XKind {
    pub fn is_attribute(self) -> bool {
        matches!(self, XKind::TextAttribute | XKind::FieldAttribute)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct XNode {
    pub kind: XKind,
    /// AL name: the variable of table/text nodes, the node name of field nodes.
    pub name: String,
    /// Element or attribute name in the document (XmlName, default the AL name).
    pub xml_name: String,
    /// NamespacePrefix: a prefix declared in the XMLport's Namespaces.
    pub ns_prefix: Option<String>,
    /// tableelement: its table.
    pub table: Option<i64>,
    /// Field nodes: the record variable (a tableelement name) and the field name.
    pub record: Option<String>,
    pub field: Option<String>,
    /// tableelement: UseTemporary.
    pub temporary: bool,
    /// tableelement: LinkTable (a tableelement name) and LinkFields (`field(X)` refers to it).
    pub link_table: Option<String>,
    pub link_fields: Vec<Cond>,
    /// tableelement: SourceTableView.
    pub view: TableView,
    pub auto_save: bool,
    pub auto_update: bool,
    pub auto_replace: bool,
    /// LinkTableForceInsert: the LinkTable record is written before this element's records.
    pub link_force_insert: bool,
    /// Field nodes: FieldValidate (default yes).
    pub field_validate: bool,
    /// MinOccurs = Zero / Occurrence = Optional.
    pub optional: bool,
    /// MaxOccurs = Once.
    pub once: bool,
    /// Unbound = true (VariableText): the element repeats until currXMLport.BreakUnbound().
    pub unbound: bool,
    /// Trigger name (lower-cased) → procedure.
    pub triggers: HashMap<String, ProcRef>,
    pub children: Vec<XNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct XmlPortDef {
    pub id: i64,
    pub name: String,
    pub code: u32,
    /// Format: xml (default), variabletext, fixedtext (lower-cased).
    pub format: String,
    /// Direction: both (default), import, export (lower-cased).
    pub direction: String,
    /// FormatEvaluate = Xml: values use the XML format (Format(x, 0, 9)) instead of the
    /// C/SIDE (legacy) format.
    pub format_evaluate_xml: bool,
    pub use_request_page: bool,
    /// Text formats: separators (BC defaults: `"`, `,`, <NewLine>).
    pub field_delimiter: String,
    pub field_separator: String,
    pub record_separator: String,
    /// Schema: the top-level nodes (normally one root text element).
    pub root: Vec<XNode>,
    /// Namespaces (prefix, URI) in declaration order; prefix "" is the default namespace (also
    /// from DefaultNamespace with UseDefaultNamespace = true).
    pub namespaces: Vec<(String, String)>,
    pub props: HashMap<String, String>,
}

fn kind_of(k: &str) -> Option<XKind> {
    Some(match k {
        "textelement" => XKind::TextElement,
        "tableelement" => XKind::TableElement,
        "fieldelement" => XKind::FieldElement,
        "textattribute" => XKind::TextAttribute,
        "fieldattribute" => XKind::FieldAttribute,
        _ => return None,
    })
}

/// `"Data Exch. Def".Code` → ("Data Exch. Def", "Code").
fn split_source(toks: &[Token]) -> (Option<String>, Option<String>) {
    match toks.iter().position(|t| t.kind == Tk::Dot) {
        Some(i) => {
            let left = tokens_text(&toks[..i]);
            let right = tokens_text(&toks[i + 1..]);
            (Some(left.trim_matches('"').to_string()), Some(right.trim_matches('"').to_string()))
        }
        None => (None, Some(tokens_text(toks).trim_matches('"').to_string())),
    }
}

/// `Namespaces = "" = 'urn:a', cac = 'urn:b';` (and DefaultNamespace with UseDefaultNamespace).
fn namespaces(b: &Body) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    if let Some(p) = prop(b, "Namespaces") {
        let t = &p.value;
        let mut i = 0;
        while i + 2 < t.len() {
            if matches!(t[i].kind, Tk::Ident | Tk::QIdent) && t[i + 1].kind == Tk::Eq && t[i + 2].kind == Tk::Str {
                out.push((t[i].text.to_string(), t[i + 2].text.to_string()));
                i += 3;
            } else {
                i += 1;
            }
        }
    }
    if !out.iter().any(|(p, _)| p.is_empty()) && yes(b, "UseDefaultNamespace", false) {
        if let Some(ns) = prop_str(b, "DefaultNamespace").filter(|n| !n.is_empty()) {
            out.insert(0, (String::new(), ns));
        }
    }
    out
}

fn yes(b: &Body, name: &str, default: bool) -> bool {
    match prop_word(b, name) {
        Some(w) => matches!(w.to_ascii_lowercase().as_str(), "true" | "yes"),
        None => default,
    }
}

impl Catalog {
    /// `validate`: the XMLport's DefaultFieldsValidation, the default of FieldValidate.
    fn xmlport_node(&mut self, el: &Element, code: u32, kind: XKind, vars: &mut Vec<VarDecl>, validate: bool) -> XNode {
        let b = &el.body;
        let name = arg_name(el, 0).unwrap_or_default();
        let xml_name = prop_str(b, "XmlName").unwrap_or_else(|| name.clone());
        let (mut table, mut record, mut field, mut temporary) = (None, None, None, false);
        match kind {
            XKind::TableElement => {
                let table_name = arg_name(el, 1).unwrap_or_default();
                table = self.id_of(ObjectKind::Table, &table_name);
                temporary = yes(b, "UseTemporary", false);
                vars.push(VarDecl { name: name.clone(), ty: TypeRef::Object { kind: "record".into(), target: table_name, temporary }, attributes: vec![] });
            }
            XKind::TextElement | XKind::TextAttribute => {
                let ty = match prop_word(b, "TextType").map(|t| t.to_ascii_lowercase()) {
                    Some(t) if t == "bigtext" => TypeRef::Simple { name: "BigText".into(), len: None },
                    _ => TypeRef::Simple { name: "Text".into(), len: None },
                };
                vars.push(VarDecl { name: name.clone(), ty, attributes: vec![] });
            }
            XKind::FieldElement | XKind::FieldAttribute => {
                let (r, f) = el.args.get(1).map(|a| split_source(a)).unwrap_or((None, None));
                record = r;
                field = f;
            }
        }
        let mut triggers = HashMap::new();
        let trig: Vec<Procedure> = Self::nested_triggers(b).cloned().collect();
        for t in trig {
            let r = self.add_nested_trigger(code, &t);
            triggers.insert(t.name.to_ascii_lowercase(), r);
        }
        let min_zero = prop_word(b, "MinOccurs").is_some_and(|v| v.eq_ignore_ascii_case("zero"));
        let optional = min_zero || prop_word(b, "Occurrence").is_some_and(|v| v.eq_ignore_ascii_case("optional"));
        let kids: Vec<Element> = elements(b).filter(|c| kind_of(&c.kind).is_some()).cloned().collect();
        let children = kids.iter().map(|c| self.xmlport_node(c, code, kind_of(&c.kind).unwrap(), vars, validate)).collect();
        XNode {
            kind,
            name,
            xml_name,
            ns_prefix: prop_str(b, "NamespacePrefix").filter(|p| !p.is_empty()),
            table,
            record,
            field,
            temporary,
            link_table: prop_word(b, "LinkTable"),
            link_fields: prop(b, "LinkFields").map(|p| parse_conds(&p.value)).unwrap_or_default(),
            view: prop(b, "SourceTableView").map(|p| parse_table_view(&p.value)).unwrap_or_default(),
            auto_save: yes(b, "AutoSave", true),
            auto_update: yes(b, "AutoUpdate", false),
            auto_replace: yes(b, "AutoReplace", false),
            link_force_insert: yes(b, "LinkTableForceInsert", false),
            field_validate: yes(b, "FieldValidate", validate),
            optional,
            once: prop_word(b, "MaxOccurs").is_some_and(|v| v.eq_ignore_ascii_case("once")),
            unbound: yes(b, "Unbound", false),
            triggers,
            children,
        }
    }

    /// Model an XMLport object (called for every `xmlport` when the catalog is built).
    pub(crate) fn add_xmlport(&mut self, code: u32, o: &Object) {
        let Some(id) = o.id else { return };
        let b = &o.body;
        let mut vars = vec![];
        let mut root = vec![];
        let validate = yes(b, "DefaultFieldsValidation", true);
        let els: Vec<Element> = elements(b).filter(|e| e.kind == "schema").cloned().collect();
        for s in &els {
            let nodes: Vec<Element> = elements(&s.body).filter(|c| kind_of(&c.kind).is_some()).cloned().collect();
            for n in &nodes {
                let node = self.xmlport_node(n, code, kind_of(&n.kind).unwrap(), &mut vars, validate);
                root.push(node);
            }
        }
        let mut pp = HashMap::new();
        for p in props(b) {
            let v = if p.value.len() == 1 && p.value[0].kind == Tk::Str { p.value[0].text.to_string() } else { tokens_text(&p.value) };
            pp.insert(p.name.to_ascii_lowercase(), v);
        }
        self.code[code as usize].globals.extend(vars);
        self.xmlports.insert(
            id,
            XmlPortDef {
                id,
                name: o.name.clone(),
                code,
                format: prop_word(b, "Format").unwrap_or_else(|| "Xml".into()).to_ascii_lowercase(),
                direction: prop_word(b, "Direction").unwrap_or_else(|| "Both".into()).to_ascii_lowercase(),
                format_evaluate_xml: prop_word(b, "FormatEvaluate").is_some_and(|v| v.eq_ignore_ascii_case("xml")),
                use_request_page: yes(b, "UseRequestPage", true),
                field_delimiter: prop_str(b, "FieldDelimiter").unwrap_or_else(|| "\"".into()),
                field_separator: prop_str(b, "FieldSeparator").unwrap_or_else(|| ",".into()),
                record_separator: prop_str(b, "RecordSeparator").unwrap_or_else(|| "<NewLine>".into()),
                root,
                namespaces: namespaces(b),
                props: pp,
            },
        );
    }
}
