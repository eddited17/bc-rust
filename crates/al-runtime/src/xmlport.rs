//! XMLports with Format = Xml: import (`Xmlport.Import(Id, InStream)`, `Xp.SetSource(..);
//! Xp.Import()`) and export (`Xmlport.Export(Id, OutStream [, Rec])`). The schema comes from
//! al_model::xmlport. Table elements are the XMLport's global record variables, text
//! elements/attributes its global Text variables; node triggers run on the XMLport instance.
//!
//! Import follows BC's order for each table element: the record is initialized (link fields set
//! from the LinkTable record, OnAfterInitRecord), attributes and child elements are assigned in
//! document order (Validate unless FieldValidate = No, then OnAfterAssignField /
//! OnAfterAssignVariable), and the record is saved before its first child table element or at its
//! end tag: OnBeforeInsertRecord, Insert (AutoSave; AutoReplace/AutoUpdate for existing
//! records, without table triggers), OnAfterInsertRecord. `currXMLport.Skip()` drops the record,
//! `Break()` stops the current table element, `Quit()` ends the run.

use crate::interp::*;
use crate::record::*;
use crate::value::*;
use al_model::{XKind, XNode, XmlPortDef};
use al_syntax::{Expr, ObjectKind};
use bc_store::Value;
use std::cell::RefCell;
use std::rc::Rc;

// ---------------------------------------------------------------- XML reader

/// An element of a parsed document: local names, with the namespace URIs they resolve to
/// (`ns`, and `attr_ns` beside `attrs`; "" for none).
#[derive(Debug, Clone, Default)]
pub struct XElem {
    pub name: String,
    pub ns: String,
    pub attrs: Vec<(String, String)>,
    pub attr_ns: Vec<String>,
    pub children: Vec<XElem>,
    /// Text directly inside this element (concatenated, entities decoded).
    pub text: String,
}

fn decode_bytes(b: &[u8]) -> String {
    if b.starts_with(&[0xFF, 0xFE]) {
        let u: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    if b.starts_with(&[0xFE, 0xFF]) {
        let u: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    // UTF-16 without BOM: '<' followed by NUL
    if b.len() >= 4 && b[0] == b'<' && b[1] == 0 {
        let u: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u);
    }
    let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(b);
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        // Windows-1252 / Latin-1 fallback
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';') else { break };
        let ent = &rest[1..end];
        let rep = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match rep {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn prefix_of(name: &str) -> &str {
    name.rsplit_once(':').map(|x| x.0).unwrap_or("")
}

/// The URI `prefix` is bound to in the scopes (innermost last); "" when unbound.
fn lookup_ns(scopes: &[Vec<(String, String)>], prefix: &str) -> String {
    if prefix == "xml" {
        return "http://www.w3.org/XML/1998/namespace".into();
    }
    scopes.iter().rev().flat_map(|s| s.iter().rev()).find(|(p, _)| p == prefix).map(|(_, u)| u.clone()).unwrap_or_default()
}

/// Parse a document into its root element.
pub fn parse_xml(bytes: &[u8]) -> Result<XElem, String> {
    let text = decode_bytes(bytes);
    let s = text.as_str();
    let mut stack: Vec<XElem> = vec![];
    // namespace declarations of the open elements
    let mut scopes: Vec<Vec<(String, String)>> = vec![];
    let mut root: Option<XElem> = None;
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() {
        if b[i] != b'<' {
            let end = s[i..].find('<').map(|k| i + k).unwrap_or(b.len());
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&decode_entities(&s[i..end]));
            }
            i = end;
            continue;
        }
        let rest = &s[i..];
        if rest.starts_with("<?") {
            i += rest.find("?>").map(|k| k + 2).ok_or("unterminated processing instruction")?;
        } else if rest.starts_with("<!--") {
            i += rest.find("-->").map(|k| k + 3).ok_or("unterminated comment")?;
        } else if rest.starts_with("<![CDATA[") {
            let k = rest.find("]]>").ok_or("unterminated CDATA")?;
            if let Some(top) = stack.last_mut() {
                top.text.push_str(&rest[9..k]);
            }
            i += k + 3;
        } else if rest.starts_with("<!") {
            i += rest.find('>').map(|k| k + 1).ok_or("unterminated declaration")?;
        } else if rest.starts_with("</") {
            let k = rest.find('>').ok_or("unterminated end tag")?;
            let name = rest[2..k].trim();
            let el = stack.pop().ok_or_else(|| format!("unexpected end tag </{name}>"))?;
            scopes.pop();
            if el.name != local(name) {
                return Err(format!("end tag </{name}> does not match <{}>", el.name));
            }
            match stack.last_mut() {
                Some(p) => p.children.push(el),
                None => root = Some(el),
            }
            i += k + 1;
        } else {
            // start tag: name, attributes, optional '/>'
            let mut j = i + 1;
            while j < b.len() && !b[j].is_ascii_whitespace() && b[j] != b'>' && b[j] != b'/' {
                j += 1;
            }
            let raw_name = s[i + 1..j].to_string();
            let mut el = XElem { name: local(&raw_name).to_string(), ..Default::default() };
            let mut decls: Vec<(String, String)> = vec![];
            let mut raw_attrs: Vec<(String, String)> = vec![];
            let mut self_close = false;
            loop {
                while j < b.len() && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j >= b.len() {
                    return Err("unterminated start tag".into());
                }
                if b[j] == b'>' {
                    j += 1;
                    break;
                }
                if b[j] == b'/' {
                    self_close = true;
                    j += 1;
                    continue;
                }
                let ns = j;
                while j < b.len() && b[j] != b'=' && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                    j += 1;
                }
                let an = s[ns..j].to_string();
                while j < b.len() && (b[j].is_ascii_whitespace() || b[j] == b'=') {
                    j += 1;
                }
                let q = *b.get(j).ok_or("unterminated attribute")?;
                if q != b'"' && q != b'\'' {
                    return Err(format!("attribute {an} has no quoted value"));
                }
                let vs = j + 1;
                let ve = s[vs..].find(q as char).map(|k| vs + k).ok_or("unterminated attribute value")?;
                let val = decode_entities(&s[vs..ve]);
                if an == "xmlns" {
                    decls.push((String::new(), val));
                } else if let Some(p) = an.strip_prefix("xmlns:") {
                    decls.push((p.to_string(), val));
                } else {
                    raw_attrs.push((an, val));
                }
                j = ve + 1;
            }
            scopes.push(decls);
            el.ns = lookup_ns(&scopes, prefix_of(&raw_name));
            for (an, val) in raw_attrs {
                // unprefixed attributes are in no namespace
                let p = prefix_of(&an);
                el.attr_ns.push(if p.is_empty() { String::new() } else { lookup_ns(&scopes, p) });
                el.attrs.push((local(&an).to_string(), val));
            }
            if self_close {
                scopes.pop();
                match stack.last_mut() {
                    Some(p) => p.children.push(el),
                    None => root = Some(el),
                }
            } else {
                stack.push(el);
            }
            i = j;
        }
    }
    if let Some(open) = stack.last() {
        return Err(format!("element <{}> is not closed", open.name));
    }
    root.ok_or_else(|| "the document has no root element".to_string())
}

fn escape(s: &str, attr: bool) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '&' => o.push_str("&amp;"),
            '"' if attr => o.push_str("&quot;"),
            _ => o.push(c),
        }
    }
    o
}

// ---------------------------------------------------------------- text formats

/// The text of a FieldDelimiter / FieldSeparator / RecordSeparator property, with BC's tokens
/// (`<None>`, `<NewLine>`, `<CR/LF>`, `<CR>`, `<LF>`, `<TAB>`).
fn separator_text(p: &str) -> String {
    match p.to_ascii_lowercase().as_str() {
        "<none>" => String::new(),
        "<newline>" | "<cr/lf>" => "\r\n".into(),
        "<cr>" => "\r".into(),
        "<lf>" => "\n".into(),
        "<tab>" => "\t".into(),
        _ => p.to_string(),
    }
}

/// Records and their fields of a VariableText document. A field that starts with the delimiter
/// runs to the closing delimiter (a doubled delimiter inside is one delimiter character) and may
/// contain separators. A CR/LF record separator also accepts a bare LF. Empty lines are skipped.
pub fn split_variable_text(text: &str, delim: &str, sep: &str, rec_sep: &str) -> Vec<Vec<String>> {
    let mut records = vec![];
    let mut fields: Vec<String> = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut field_start = true;
    let mut rest = text;
    let end_record = |fields: &mut Vec<String>, cur: &mut String, records: &mut Vec<Vec<String>>| {
        if !(fields.is_empty() && cur.is_empty()) {
            fields.push(std::mem::take(cur));
            records.push(std::mem::take(fields));
        }
    };
    while !rest.is_empty() {
        if !delim.is_empty() && rest.starts_with(delim) {
            if field_start && !quoted {
                quoted = true;
                field_start = false;
                rest = &rest[delim.len()..];
                continue;
            }
            if quoted {
                rest = &rest[delim.len()..];
                if rest.starts_with(delim) {
                    cur.push_str(delim);
                    rest = &rest[delim.len()..];
                } else {
                    quoted = false;
                }
                continue;
            }
        }
        if !quoted {
            if !sep.is_empty() && rest.starts_with(sep) {
                fields.push(std::mem::take(&mut cur));
                rest = &rest[sep.len()..];
                field_start = true;
                continue;
            }
            let rec_len = if !rec_sep.is_empty() && rest.starts_with(rec_sep) {
                rec_sep.len()
            } else if rec_sep == "\r\n" && rest.starts_with('\n') {
                1
            } else {
                0
            };
            if rec_len > 0 {
                end_record(&mut fields, &mut cur, &mut records);
                rest = &rest[rec_len..];
                field_start = true;
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        cur.push(c);
        field_start = false;
        rest = &rest[c.len_utf8()..];
    }
    end_record(&mut fields, &mut cur, &mut records);
    records
}

// ---------------------------------------------------------------- run state

#[derive(Default)]
struct RunState {
    skip: bool,
    brk: bool,
    break_unbound: bool,
    quit: bool,
    /// Export: the XMLport's Namespaces, declared on the first (root) element written.
    ns_decls: Option<Vec<(String, String)>>,
    /// currXMLport.FieldDelimiter / FieldSeparator / RecordSeparator set by the XMLport's code.
    field_delimiter: Option<String>,
    field_separator: Option<String>,
    record_separator: Option<String>,
}

/// Whether a schema node has child elements (attributes alone leave it a leaf with a text value).
fn has_element_children(node: &XNode) -> bool {
    node.children.iter().any(|c| !c.kind.is_attribute())
}

/// Error code of the control flow currXMLport.Skip / Break / BreakUnbound / Quit raise.
const XP_CONTROL: &str = "XmlPortControl";

/// A trigger's result with the XMLport control flow it raised turned into a normal end.
fn control(r: R<Val>) -> R<()> {
    match r {
        Err(Flow::Err(e)) if e.code == XP_CONTROL => Ok(()),
        r => r.map(|_| ()),
    }
}

/// Element or attribute name with its NamespacePrefix.
fn qname(node: &XNode) -> String {
    match &node.ns_prefix {
        Some(p) => format!("{p}:{}", node.xml_name),
        None => node.xml_name.clone(),
    }
}

/// The namespace URI a schema node's elements (attributes) are in: its NamespacePrefix's, else
/// for elements the default namespace; None when the XMLport declares nothing for it.
fn expected_ns(def: &XmlPortDef, node: &XNode) -> Option<String> {
    let find = |p: &str| def.namespaces.iter().find(|(x, _)| x == p).map(|(_, u)| u.clone());
    match &node.ns_prefix {
        Some(p) => find(p),
        None if !node.kind.is_attribute() => find(""),
        None => None,
    }
}

/// A document name `name` in namespace `ns` is schema node `node`: the local names match, and a
/// namespaced document name is in the node's namespace (documents without namespaces still match).
fn name_matches(def: &XmlPortDef, node: &XNode, name: &str, ns: &str) -> bool {
    if node.xml_name != name && !node.xml_name.eq_ignore_ascii_case(name) {
        return false;
    }
    match expected_ns(def, node) {
        Some(want) if !ns.is_empty() => want == ns,
        Some(_) => true,
        None => ns.is_empty() || def.namespaces.is_empty() || node.kind.is_attribute(),
    }
}

thread_local! {
    static RUNS: RefCell<Vec<RunState>> = const { RefCell::new(vec![]) };
}

fn with_run<T>(f: impl FnOnce(&mut RunState) -> T) -> Option<T> {
    RUNS.with(|r| r.borrow_mut().last_mut().map(f))
}

fn take_flag(get: impl Fn(&mut RunState) -> &mut bool) -> bool {
    with_run(|s| std::mem::take(get(s))).unwrap_or(false)
}

/// Per-instance settings of an XMLport variable (SetSource / SetDestination / SetTableView).
#[derive(Default)]
pub struct XmlPortVarState {
    pub source: Option<Rc<Vec<u8>>>,
    pub dest: Option<Val>,
    pub views: Vec<(String, RecHandle)>,
}

thread_local! {
    static VARS: RefCell<Vec<(std::rc::Weak<Instance>, XmlPortVarState)>> = const { RefCell::new(vec![]) };
}

fn var_state<T>(inst: &Rc<Instance>, f: impl FnOnce(&mut XmlPortVarState) -> T) -> T {
    VARS.with(|v| {
        let mut v = v.borrow_mut();
        v.retain(|(w, _)| w.strong_count() > 0);
        let pos = v.iter().position(|(w, _)| w.upgrade().is_some_and(|i| Rc::ptr_eq(&i, inst)));
        let i = match pos {
            Some(i) => i,
            None => {
                v.push((Rc::downgrade(inst), XmlPortVarState::default()));
                v.len() - 1
            }
        };
        f(&mut v[i].1)
    })
}

fn al_err(e: String) -> Flow {
    Flow::Err(Box::new(AlError::new(e)))
}

/// Value of a field formatted for an XMLport (FormatEvaluate = Xml: Format(x, 0, 9)).
fn xml_format(fd: &al_model::FieldDef, v: &Value) -> String {
    match v {
        Value::Bool(b) => if *b { "true" } else { "false" }.into(),
        Value::Option(o) => o.to_string(),
        Value::Date(d) => {
            if *d == 0 {
                String::new()
            } else {
                match date_to_naive(*d) {
                    Some(n) => n.format("%Y-%m-%d").to_string(),
                    None => String::new(),
                }
            }
        }
        _ => to_val(fd, v).to_text(),
    }
}

impl<'r> Session<'r> {
    fn xp_def(&self, id: i64) -> R<&'static XmlPortDef> {
        match self.cat.xmlports.get(&id) {
            Some(d) => Ok(d),
            None => err(format!("The XMLport {id} does not exist.")),
        }
    }

    fn xp_global(&mut self, inst: &Rc<Instance>, name: &str) -> Option<Val> {
        let g = self.instance_globals(inst);
        g.vars.iter().find(|v| v.name.eq_ignore_ascii_case(name)).map(|v| v.slot.borrow().clone())
    }

    fn xp_set_global(&mut self, inst: &Rc<Instance>, name: &str, val: Val) {
        let g = self.instance_globals(inst);
        if let Some(v) = g.vars.iter().find(|v| v.name.eq_ignore_ascii_case(name)) {
            *v.slot.borrow_mut() = val;
        }
    }

    fn xp_rec(&mut self, inst: &Rc<Instance>, name: &str) -> R<RecHandle> {
        match self.xp_global(inst, name) {
            Some(Val::Record(r)) => Ok(r),
            _ => err(format!("The XMLport has no table element {name}.")),
        }
    }

    fn xp_trigger(&mut self, node: &XNode, name: &str, inst: &Rc<Instance>) -> R<()> {
        if let Some(pr) = node.triggers.get(name) {
            control(self.invoke(*pr, vec![], Some(inst.clone()), None, None))?;
        }
        Ok(())
    }

    fn xp_object_trigger(&mut self, def: &XmlPortDef, name: &str, inst: &Rc<Instance>) -> R<()> {
        if let Some(t) = self.code(def.code).trigger(name) {
            control(self.invoke(al_model::ProcRef { code: def.code, proc: t }, vec![], Some(inst.clone()), None, None))?;
        }
        Ok(())
    }

    /// Run an XMLport import of `bytes` on `inst`.
    pub fn xmlport_import(&mut self, def: &'static XmlPortDef, inst: &Rc<Instance>, bytes: &[u8]) -> R<()> {
        if def.format == "variabletext" {
            return self.xp_import_text(def, inst, bytes);
        }
        if def.format != "xml" {
            *self.missing.entry(format!("XMLport import, Format = {}", def.format)).or_default() += 1;
            return err(format!("Not implemented in bc-rust: XMLport import with Format = {}", def.format));
        }
        let doc = parse_xml(bytes).map_err(|e| al_err(format!("The XML document could not be read: {e}")))?;
        RUNS.with(|r| r.borrow_mut().push(RunState::default()));
        let res = (|| -> R<()> {
            self.xp_object_trigger(def, "OnInitXmlPort", inst)?;
            self.xp_object_trigger(def, "OnPreXmlPort", inst)?;
            let Some(root) = def.root.iter().find(|n| name_matches(def, n, &doc.name, &doc.ns)) else {
                let expected = def.root.first().map(|n| n.xml_name.clone()).unwrap_or_default();
                return err(format!("The element '{}' is not expected; the XMLport expects '{expected}'.", doc.name));
            };
            self.xp_import_node(def, root, &doc, inst)?;
            if !with_run(|s| s.quit).unwrap_or(false) {
                self.xp_object_trigger(def, "OnPostXmlPort", inst)?;
            }
            Ok(())
        })();
        RUNS.with(|r| r.borrow_mut().pop());
        res
    }

    fn xp_import_node(&mut self, def: &'static XmlPortDef, node: &'static XNode, el: &XElem, inst: &Rc<Instance>) -> R<()> {
        match node.kind {
            XKind::TableElement => self.xp_import_record(def, node, el, inst),
            XKind::TextElement => {
                if !has_element_children(node) {
                    // a leaf: its attributes (read with the start tag), then its text
                    if !node.children.is_empty() {
                        self.xp_import_children(def, node, el, inst, None)?;
                    }
                    self.xp_set_global(inst, &node.name, Val::Text(el.text.clone()));
                    self.xp_trigger(node, "onafterassignvariable", inst)?;
                    return Ok(());
                }
                self.xp_import_children(def, node, el, inst, None)
            }
            XKind::FieldElement => {
                if !node.children.is_empty() {
                    self.xp_import_children(def, node, el, inst, None)?;
                }
                self.xp_assign_field(node, &el.text, inst)
            }
            XKind::TextAttribute | XKind::FieldAttribute => Ok(()),
        }
    }

    /// Attributes and child elements of `el` under schema node `node`. `rec` is the record of a
    /// table element (saved before its first child table element, and written again at its end).
    fn xp_import_children(&mut self, def: &'static XmlPortDef, node: &'static XNode, el: &XElem, inst: &Rc<Instance>, mut rec: Option<(&'static XNode, &mut bool)>) -> R<()> {
        for a in node.children.iter().filter(|c| c.kind.is_attribute()) {
            let Some((_, v)) = el.attrs.iter().enumerate().find(|(k, (n, _))| name_matches(def, a, n, el.attr_ns.get(*k).map(|s| s.as_str()).unwrap_or(""))).map(|(_, x)| x) else { continue };
            match a.kind {
                XKind::FieldAttribute => self.xp_assign_field(a, v, inst)?,
                _ => {
                    self.xp_set_global(inst, &a.name, Val::Text(v.clone()));
                    self.xp_trigger(a, "onafterassignvariable", inst)?;
                }
            }
        }
        let mut broken: Vec<*const XNode> = vec![];
        for child in &el.children {
            if with_run(|s| s.quit).unwrap_or(false) {
                break;
            }
            let Some(cn) = node.children.iter().find(|c| !c.kind.is_attribute() && name_matches(def, c, &child.name, &child.ns)) else { continue };
            if broken.contains(&(cn as *const XNode)) {
                continue;
            }
            if cn.kind == XKind::TableElement {
                if let Some((tn, saved)) = rec.as_mut() {
                    if !**saved {
                        **saved = true;
                        self.xp_save_record(tn, inst)?;
                    }
                }
            }
            self.xp_import_node(def, cn, child, inst)?;
            if cn.kind == XKind::TableElement && take_flag(|s| &mut s.brk) {
                broken.push(cn as *const XNode);
            }
        }
        Ok(())
    }

    fn xp_import_record(&mut self, def: &'static XmlPortDef, node: &'static XNode, el: &XElem, inst: &Rc<Instance>) -> R<()> {
        self.xp_init_record(node, inst)?;
        let mut saved = false;
        self.xp_import_children(def, node, el, inst, Some((node, &mut saved)))?;
        if !saved {
            self.xp_save_record(node, inst)?;
        } else {
            // written before its child table elements: written again at its end with what the
            // children changed (NavXmlPortTableNode.WriteRecord(alwaysModify: true): the modify path)
            let r = self.xp_rec(inst, &node.name)?;
            if self.rec_get_pk_r(&r)?.is_some() {
                self.xp_trigger(node, "onbeforemodifyrecord", inst)?;
                if !take_flag(|s| &mut s.skip) {
                    if node.auto_save {
                        let triggers = !r.borrow().is_temp();
                        self.rec_modify(&r, triggers)?.map_err(al_err)?;
                    }
                    self.xp_trigger(node, "onaftermodifyrecord", inst)?;
                }
            }
        }
        Ok(())
    }

    /// Start a table element's record: a fresh row, the link fields from the LinkTable record,
    /// OnAfterInitRecord.
    fn xp_init_record(&mut self, node: &'static XNode, inst: &Rc<Instance>) -> R<()> {
        let r = self.xp_rec(inst, &node.name)?;
        // initialize: a fresh row, then the link fields from the LinkTable record
        {
            let mut rb = r.borrow_mut();
            let mut fresh = rb.schema.new_row();
            self.init_row(rb.tdef, &mut fresh);
            rb.row = (fresh).into();
            rb.xrow = None;
        }
        if let Some(lt) = &node.link_table {
            let link = self.xp_rec(inst, lt)?;
            for c in &node.link_fields {
                let (Some(fi), Some(si)) = (r.borrow().tdef.field(&c.field), link.borrow().tdef.field(&c.value)) else { continue };
                let v = link.borrow().row[si].clone();
                r.borrow_mut().row[fi] = v;
            }
        }
        with_run(|s| s.skip = false);
        self.xp_trigger(node, "onafterinitrecord", inst)
    }

    /// Import a Format = VariableText document: every record (line) of the file is one record of
    /// a table element, its fields go to the table element's field and text elements in schema
    /// order (missing trailing fields are left out, as for MinOccurs = Zero elements). The table
    /// elements take the lines in schema order: one with MaxOccurs = Once takes one line, any
    /// other takes lines until the end of the file or currXMLport.Break().
    fn xp_import_text(&mut self, def: &'static XmlPortDef, inst: &Rc<Instance>, bytes: &[u8]) -> R<()> {
        fn tables<'a>(nodes: &'a [XNode], out: &mut Vec<&'a XNode>) {
            for n in nodes {
                match n.kind {
                    XKind::TableElement => out.push(n),
                    XKind::TextElement => tables(&n.children, out),
                    _ => {}
                }
            }
        }
        let text = decode_bytes(bytes);
        let records = split_variable_text(&text, &separator_text(&def.field_delimiter), &separator_text(&def.field_separator), &separator_text(&def.record_separator));
        let mut table_nodes = vec![];
        tables(&def.root, &mut table_nodes);
        RUNS.with(|r| r.borrow_mut().push(RunState::default()));
        let res = (|| -> R<()> {
            self.xp_object_trigger(def, "OnInitXmlPort", inst)?;
            self.xp_object_trigger(def, "OnPreXmlPort", inst)?;
            let mut lines = records.iter().peekable();
            'tables: for node in table_nodes {
                while let Some(fields) = lines.next() {
                    if with_run(|s| s.quit).unwrap_or(false) {
                        break 'tables;
                    }
                    self.xp_init_record(node, inst)?;
                    let cols = node.children.iter().filter(|c| matches!(c.kind, XKind::FieldElement | XKind::TextElement));
                    let mut values = fields.iter();
                    for col in cols {
                        // NavXmlPortTextReader.ProcessRecord: an Unbound element takes every
                        // remaining field of the line, any other element one field; fields
                        // beyond the elements are skipped (ReadToRecordSeparator)
                        let take: Vec<&String> = if col.unbound { values.by_ref().collect() } else { values.next().into_iter().collect() };
                        if take.is_empty() {
                            break;
                        }
                        for value in take {
                            if col.kind == XKind::FieldElement {
                                self.xp_assign_field(col, value, inst)?;
                            } else {
                                self.xp_set_global(inst, &col.name, Val::Text(value.clone()));
                                self.xp_trigger(col, "onafterassignvariable", inst)?;
                            }
                        }
                    }
                    self.xp_save_record(node, inst)?;
                    if take_flag(|s| &mut s.brk) || node.once || lines.peek().is_none() {
                        break;
                    }
                }
            }
            if !with_run(|s| s.quit).unwrap_or(false) {
                self.xp_object_trigger(def, "OnPostXmlPort", inst)?;
            }
            Ok(())
        })();
        RUNS.with(|r| r.borrow_mut().pop());
        res
    }

    /// Saving an imported record (NavXmlPortTableNode): with AutoReplace / AutoUpdate an existing
    /// record is modified (OnBeforeModifyRecord, Modify, OnAfterModifyRecord), otherwise inserted
    /// (OnBeforeInsertRecord, Insert, OnAfterInsertRecord). AutoSave writes with the table's
    /// triggers unless the record is temporary (ALInsert / ALModify(DataError, !IsTemporary)).
    fn xp_save_record(&mut self, node: &'static XNode, inst: &Rc<Instance>) -> R<()> {
        let r = self.xp_rec(inst, &node.name)?;
        let triggers = !r.borrow().is_temp();
        if node.auto_replace || node.auto_update {
            if let Some(existing) = self.rec_get_pk_r(&r)? {
                if node.auto_update && !node.auto_replace {
                    // fields not given in the document keep their stored values
                    let (fresh, tdef) = {
                        let rb = r.borrow();
                        let mut f = rb.schema.new_row();
                        self.init_row(rb.tdef, &mut f);
                        (f, rb.tdef)
                    };
                    let mut rb = r.borrow_mut();
                    for i in 0..tdef.fields.len() {
                        if rb.row[i] == fresh[i] {
                            rb.row[i] = existing[i].clone();
                        }
                    }
                }
                self.xp_trigger(node, "onbeforemodifyrecord", inst)?;
                if take_flag(|s| &mut s.skip) {
                    return Ok(());
                }
                if node.auto_save {
                    self.rec_modify(&r, triggers)?.map_err(al_err)?;
                }
                return self.xp_trigger(node, "onaftermodifyrecord", inst);
            }
        }
        self.xp_trigger(node, "onbeforeinsertrecord", inst)?;
        if take_flag(|s| &mut s.skip) {
            return Ok(());
        }
        if node.auto_save {
            // an existing record fails with BC's "already exists" error
            self.rec_insert(&r, triggers)?.map_err(al_err)?;
        }
        self.xp_trigger(node, "onafterinsertrecord", inst)
    }

    fn xp_assign_field(&mut self, node: &'static XNode, text: &str, inst: &Rc<Instance>) -> R<()> {
        let (Some(rn), Some(fname)) = (&node.record, &node.field) else { return Ok(()) };
        let r = self.xp_rec(inst, rn)?;
        let Some(fi) = r.borrow().tdef.field(fname) else { return err(format!("Field {fname} does not exist in table {}", r.borrow().tdef.name)) };
        let fd = r.borrow().field(fi);
        let evf = |n: &str| self.enum_ordinal_for_field(fd, n);
        let Some(sv) = text_to_store(fd, text, &evf, self.work_date) else {
            return err(format!("The value '{text}' can't be evaluated into type {:?} (field {} of table {}).", fd.ty, fd.name, r.borrow().tdef.name));
        };
        if node.field_validate {
            let v = to_val(fd, &sv);
            let cf = self.curr_field_no(&r);
            self.rec_validate(&r, fi, Some(v), cf)?;
        } else {
            r.borrow_mut().row[fi] = sv;
        }
        self.xp_trigger(node, "onafterassignfield", inst)
    }

    // ------------------------------------------------------------ export

    /// Run an XMLport export on `inst`, returning the document.
    pub fn xmlport_export(&mut self, def: &'static XmlPortDef, inst: &Rc<Instance>) -> R<Vec<u8>> {
        if def.format == "variabletext" {
            return self.xp_export_text(def, inst);
        }
        if def.format != "xml" {
            *self.missing.entry(format!("XMLport export, Format = {}", def.format)).or_default() += 1;
            return err(format!("Not implemented in bc-rust: XMLport export with Format = {}", def.format));
        }
        RUNS.with(|r| r.borrow_mut().push(RunState { ns_decls: Some(def.namespaces.clone()), ..Default::default() }));
        let res = (|| -> R<Vec<u8>> {
            self.xp_object_trigger(def, "OnInitXmlPort", inst)?;
            self.xp_object_trigger(def, "OnPreXmlPort", inst)?;
            let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n");
            for n in &def.root {
                self.xp_export_node(n, inst, &mut out, 0)?;
            }
            self.xp_object_trigger(def, "OnPostXmlPort", inst)?;
            Ok(out.into_bytes())
        })();
        RUNS.with(|r| r.borrow_mut().pop());
        res
    }

    /// Format = VariableText export: one line per table element record, its field and text
    /// elements as delimited fields joined by the field separator, the record separator after each
    /// line; an Unbound text element repeats (OnBeforePassVariable each time) until
    /// currXMLport.BreakUnbound(). Child table elements write their lines after their parent's.
    fn xp_export_text(&mut self, def: &'static XmlPortDef, inst: &Rc<Instance>) -> R<Vec<u8>> {
        let seps = (separator_text(&def.field_delimiter), separator_text(&def.field_separator), separator_text(&def.record_separator));
        RUNS.with(|r| r.borrow_mut().push(RunState::default()));
        let res = (|| -> R<String> {
            self.xp_object_trigger(def, "OnInitXmlPort", inst)?;
            self.xp_object_trigger(def, "OnPreXmlPort", inst)?;
            let mut out = String::new();
            for n in &def.root {
                if with_run(|s| s.quit).unwrap_or(false) {
                    break;
                }
                self.xp_export_text_node(n, inst, &mut out, &seps)?;
            }
            if !with_run(|s| s.quit).unwrap_or(false) {
                self.xp_object_trigger(def, "OnPostXmlPort", inst)?;
            }
            Ok(out)
        })();
        RUNS.with(|r| r.borrow_mut().pop());
        let text = res?;
        let cp = match def.props.get("textencoding").map(|v| v.to_ascii_lowercase()) {
            Some(v) if v == "windows" => 1252,
            Some(v) if v == "msdos" => 437,
            Some(v) if v == "utf16" => 1200,
            _ => 65001,
        };
        Ok(crate::dotnet::text::encode(cp, &text, false))
    }

    fn xp_export_text_node(&mut self, node: &'static XNode, inst: &Rc<Instance>, out: &mut String, seps: &(String, String, String)) -> R<()> {
        match node.kind {
            XKind::TableElement => {
                let r = self.xp_rec(inst, &node.name)?;
                self.apply_conds(&r, &node.view.conds, None, 2)?;
                if let Some(lt) = &node.link_table {
                    let link = self.xp_rec(inst, lt)?;
                    for c in &node.link_fields {
                        let (Some(fi), Some(si)) = (r.borrow().tdef.field(&c.field), link.borrow().tdef.field(&c.value)) else { continue };
                        let v = link.borrow().row[si].clone();
                        let text = to_val(link.borrow().field(si), &v).to_text();
                        let mut rb = r.borrow_mut();
                        rb.filters.retain(|f| !(f.field == fi && f.group == 4));
                        rb.filters.push(bc_store::filter::FieldFilter { field: fi, node: bc_store::filter::FNode::Eq(v), text, group: 4 });
                    }
                }
                self.xp_trigger(node, "onprexmlitem", inst)?;
                let mut found = self.find_first(&r, false)?;
                while found {
                    if with_run(|s| s.quit).unwrap_or(false) {
                        break;
                    }
                    with_run(|s| s.skip = false);
                    self.xp_trigger(node, "onaftergetrecord", inst)?;
                    if take_flag(|s| &mut s.brk) {
                        break;
                    }
                    if !take_flag(|s| &mut s.skip) {
                        let mut fields: Vec<String> = vec![];
                        let mut child_tables = vec![];
                        for c in &node.children {
                            match c.kind {
                                XKind::TableElement => child_tables.push(c),
                                XKind::TextElement | XKind::FieldElement if c.unbound => {
                                    // repeats until BreakUnbound (a bound on runaway loops)
                                    for _ in 0..1_000_000 {
                                        with_run(|s| s.break_unbound = false);
                                        let v = self.xp_value(c, inst)?;
                                        if take_flag(|s| &mut s.break_unbound) || with_run(|s| s.quit).unwrap_or(false) {
                                            break;
                                        }
                                        if let Some(v) = v {
                                            fields.push(v);
                                        }
                                    }
                                }
                                XKind::TextElement | XKind::FieldElement => {
                                    if let Some(v) = self.xp_value(c, inst)? {
                                        fields.push(v);
                                    }
                                }
                                _ => {}
                            }
                        }
                        // the separators the XMLport's code set (OnPreXmlPort ...), else its properties
                        let (d, s, rs) = with_run(|st| {
                            (
                                st.field_delimiter.clone().unwrap_or_else(|| seps.0.clone()),
                                st.field_separator.clone().unwrap_or_else(|| seps.1.clone()),
                                st.record_separator.clone().unwrap_or_else(|| seps.2.clone()),
                            )
                        })
                        .unwrap_or_else(|| seps.clone());
                        out.push_str(&fields.iter().map(|f| format!("{d}{f}{d}")).collect::<Vec<_>>().join(&s));
                        out.push_str(&rs);
                        for ct in child_tables {
                            self.xp_export_text_node(ct, inst, out, seps)?;
                        }
                    }
                    if node.once {
                        break;
                    }
                    found = self.next(&r, 1)? != 0;
                }
                Ok(())
            }
            // a text element above the table elements is a container
            XKind::TextElement if has_element_children(node) => {
                if self.xp_value(node, inst)?.is_none() {
                    return Ok(());
                }
                for c in &node.children {
                    self.xp_export_text_node(c, inst, out, seps)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn xp_value(&mut self, node: &'static XNode, inst: &Rc<Instance>) -> R<Option<String>> {
        with_run(|s| s.skip = false);
        let trig = if matches!(node.kind, XKind::FieldAttribute | XKind::FieldElement) { "onbeforepassfield" } else { "onbeforepassvariable" };
        self.xp_trigger(node, trig, inst)?;
        if take_flag(|s| &mut s.skip) {
            return Ok(None);
        }
        Ok(Some(match node.kind {
            XKind::FieldAttribute | XKind::FieldElement => {
                let (Some(rn), Some(fname)) = (&node.record, &node.field) else { return Ok(None) };
                let r = self.xp_rec(inst, rn)?;
                let rb = r.borrow();
                match rb.tdef.field(fname) {
                    Some(fi) => xml_format(rb.field(fi), &rb.row[fi]),
                    None => String::new(),
                }
            }
            _ => self.xp_global(inst, &node.name).map(|v| v.to_text()).unwrap_or_default(),
        }))
    }

    fn xp_export_element(&mut self, node: &'static XNode, inst: &Rc<Instance>, out: &mut String, depth: usize, text: Option<String>) -> R<()> {
        let pad = "  ".repeat(depth);
        let name = qname(node);
        out.push_str(&pad);
        out.push('<');
        out.push_str(&name);
        // the XMLport's Namespaces are declared on the root element
        if let Some(decls) = with_run(|s| s.ns_decls.take()).flatten() {
            for (p, uri) in decls {
                if p.is_empty() {
                    out.push_str(&format!(" xmlns=\"{}\"", escape(&uri, true)));
                } else {
                    out.push_str(&format!(" xmlns:{p}=\"{}\"", escape(&uri, true)));
                }
            }
        }
        for a in node.children.iter().filter(|c| c.kind.is_attribute()) {
            if let Some(v) = self.xp_value(a, inst)? {
                out.push_str(&format!(" {}=\"{}\"", qname(a), escape(&v, true)));
            }
        }
        let kids: Vec<&'static XNode> = node.children.iter().filter(|c| !c.kind.is_attribute()).collect();
        if kids.is_empty() {
            match text {
                Some(t) if !t.is_empty() => out.push_str(&format!(">{}</{}>\n", escape(&t, false), name)),
                _ => out.push_str(" />\n"),
            }
            return Ok(());
        }
        let mut body = String::new();
        for k in kids {
            self.xp_export_node(k, inst, &mut body, depth + 1)?;
        }
        if body.is_empty() {
            out.push_str(" />\n");
        } else {
            out.push_str(">\n");
            out.push_str(&body);
            out.push_str(&pad);
            out.push_str(&format!("</{}>\n", name));
        }
        Ok(())
    }

    fn xp_export_node(&mut self, node: &'static XNode, inst: &Rc<Instance>, out: &mut String, depth: usize) -> R<()> {
        match node.kind {
            XKind::TableElement => {
                let r = self.xp_rec(inst, &node.name)?;
                // SourceTableView (filter group 2), links from the LinkTable record
                self.apply_conds(&r, &node.view.conds, None, 2)?;
                if !node.view.sorting.is_empty() {
                    let names: Vec<&str> = node.view.sorting.iter().map(|s| s.as_str()).collect();
                    let k = r.borrow().tdef.keys.iter().position(|k| {
                        k.fields.len() >= names.len() && names.iter().enumerate().all(|(i, n)| r.borrow().tdef.fields[k.fields[i]].name.eq_ignore_ascii_case(n))
                    });
                    if let Some(k) = k {
                        r.borrow_mut().key = k;
                    }
                }
                if let Some(lt) = &node.link_table {
                    let link = self.xp_rec(inst, lt)?;
                    for c in &node.link_fields {
                        let (Some(fi), Some(si)) = (r.borrow().tdef.field(&c.field), link.borrow().tdef.field(&c.value)) else { continue };
                        let v = link.borrow().row[si].clone();
                        let text = to_val(link.borrow().field(si), &v).to_text();
                        let mut rb = r.borrow_mut();
                        rb.filters.retain(|f| !(f.field == fi && f.group == 4));
                        rb.filters.push(bc_store::filter::FieldFilter { field: fi, node: bc_store::filter::FNode::Eq(v), text, group: 4 });
                    }
                }
                self.xp_trigger(node, "onprexmlitem", inst)?;
                let mut found = self.find_first(&r, false)?;
                // NavXmlPortExporter.ProcessTableElement: a table element without records and with
                // MinOccurs = Once (the default) is written once anyway, without OnAfterGetRecord
                if !found && !node.optional {
                    return self.xp_export_element(node, inst, out, depth, None);
                }
                while found {
                    with_run(|s| s.skip = false);
                    self.xp_trigger(node, "onaftergetrecord", inst)?;
                    if take_flag(|s| &mut s.brk) {
                        break;
                    }
                    if !take_flag(|s| &mut s.skip) {
                        self.xp_export_element(node, inst, out, depth, None)?;
                    }
                    if node.once {
                        break;
                    }
                    found = self.next(&r, 1)? != 0;
                }
                Ok(())
            }
            XKind::TextElement | XKind::FieldElement => {
                // OnBeforePassVariable / OnBeforePassField run for every element, also one with
                // children (its trigger often fills them in), and Skip drops the whole subtree; an
                // element with child elements has no text of its own
                match self.xp_value(node, inst)? {
                    Some(t) => self.xp_export_element(node, inst, out, depth, Some(if has_element_children(node) { String::new() } else { t })),
                    None => Ok(()),
                }
            }
            XKind::TextAttribute | XKind::FieldAttribute => Ok(()),
        }
    }

    // ------------------------------------------------------------ entry points

    /// `Xmlport.Import(Id, InStream [, Rec])` / `Xmlport.Export(Id, OutStream [, Rec])`.
    pub fn xmlport_static(&mut self, f: &mut Frame, m: &str, args: &[Expr]) -> R<Val> {
        let a = self.eval_all_args(f, args)?;
        let id = match a.first() {
            Some(Val::Text(t)) => self.cat.id_of(ObjectKind::XmlPort, t).unwrap_or(0),
            Some(v) => v.as_i64(),
            None => 0,
        };
        let def = self.xp_def(id)?;
        let inst = Instance::new(def.code);
        if let Some(Val::Record(src)) = a.get(2) {
            self.xp_set_view(def, &inst, src)?;
        }
        match m {
            "import" => {
                let bytes = match a.get(1) {
                    Some(Val::InStream(s)) => {
                        let st = s.borrow();
                        st.data[st.pos.min(st.data.len())..].to_vec()
                    }
                    _ => vec![],
                };
                self.xmlport_import(def, &inst, &bytes)?;
                Ok(Val::Bool(true))
            }
            _ => {
                let doc = self.xmlport_export(def, &inst)?;
                if let Some(Val::OutStream(t)) = a.get(1) {
                    crate::media::write_out(t, &doc);
                }
                Ok(Val::Bool(true))
            }
        }
    }

    fn eval_all_args(&mut self, f: &mut Frame, args: &[Expr]) -> R<Vec<Val>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            out.push(self.eval(f, a)?);
        }
        Ok(out)
    }

    /// SetTableView(Rec): the filters of `src` on the table element of the same table.
    fn xp_set_view(&mut self, def: &'static XmlPortDef, inst: &Rc<Instance>, src: &RecHandle) -> R<()> {
        fn find(nodes: &'static [XNode], table: i64) -> Option<&'static XNode> {
            nodes.iter().find_map(|n| if n.kind == XKind::TableElement && n.table == Some(table) { Some(n) } else { find(&n.children, table) })
        }
        let tid = src.borrow().tdef.id;
        if let Some(n) = find(&def.root, tid) {
            let r = self.xp_rec(inst, &n.name)?;
            let (filters, key) = {
                let s = src.borrow();
                (s.filters.clone(), s.key)
            };
            let mut rb = r.borrow_mut();
            rb.filters = filters;
            rb.key = key;
        }
        Ok(())
    }

    /// Methods on an XMLport variable: SetSource, SetDestination, Import, Export, SetTableView,
    /// Run, Filename, TextEncoding. Returns None for the XMLport's own procedures.
    pub fn xmlport_method(&mut self, f: &mut Frame, inst: &Rc<Instance>, name: &str, args: &[Expr]) -> R<Option<Val>> {
        let m = name.to_ascii_lowercase();
        let Some(def) = self.cat.xmlports.values().find(|d| d.code == inst.code) else { return Ok(None) };
        let v = match m.as_str() {
            "setsource" => {
                let a = self.eval_all_args(f, args)?;
                let bytes = match a.first() {
                    Some(Val::InStream(s)) => {
                        let st = s.borrow();
                        Some(Rc::new(st.data[st.pos.min(st.data.len())..].to_vec()))
                    }
                    _ => None,
                };
                var_state(inst, |v| v.source = bytes);
                Val::Void
            }
            "setdestination" => {
                let a = self.eval_all_args(f, args)?;
                var_state(inst, |v| v.dest = a.into_iter().next());
                Val::Void
            }
            "settableview" => {
                let a = self.eval_all_args(f, args)?;
                if let Some(Val::Record(src)) = a.first() {
                    self.xp_set_view(def, inst, src)?;
                }
                Val::Void
            }
            "import" => {
                let src = var_state(inst, |v| v.source.clone()).unwrap_or_default();
                self.xmlport_import(def, inst, &src)?;
                Val::Bool(true)
            }
            "export" => {
                let doc = self.xmlport_export(def, inst)?;
                if let Some(Val::OutStream(t)) = var_state(inst, |v| v.dest.clone()) {
                    crate::media::write_out(&t, &doc);
                }
                Val::Bool(true)
            }
            "run" => {
                *self.missing.entry("XMLport.Run (needs a file dialog)".into()).or_default() += 1;
                crate::assume::hit("Xmlport.Run");
                Val::Void
            }
            "filename" | "textencoding" | "fieldseparator" | "fielddelimiter" | "recordseparator" | "tableseparator" | "importfile" | "currentpath" | "useRequestPage" | "userequestpage" => {
                let _ = self.eval_all_args(f, args)?;
                Val::Text(String::new())
            }
            _ => return Ok(None),
        };
        Ok(Some(v))
    }

    /// currXMLport.Skip() / Break() / BreakUnbound() / Quit() and properties.
    /// currXMLport.Skip / Break / BreakUnbound / Quit: like BC (NavXmlPort throws a
    /// NavControlException) they end the running trigger, also from a procedure it calls; the
    /// XMLport reads the flag after the trigger.
    pub fn curr_xmlport(&mut self, f: &mut Frame, m: &str, args: &[Expr]) -> R<Val> {
        // FieldDelimiter / FieldSeparator / RecordSeparator: set (one argument) or read
        if matches!(m, "fielddelimiter" | "fieldseparator" | "recordseparator") {
            let a = self.eval_all_args(f, args)?;
            if let Some(v) = a.first() {
                let v = separator_text(&v.to_text());
                with_run(|s| match m {
                    "fielddelimiter" => s.field_delimiter = Some(v),
                    "fieldseparator" => s.field_separator = Some(v),
                    _ => s.record_separator = Some(v),
                });
                return Ok(Val::Void);
            }
            let cur = with_run(|s| match m {
                "fielddelimiter" => s.field_delimiter.clone(),
                "fieldseparator" => s.field_separator.clone(),
                _ => s.record_separator.clone(),
            })
            .flatten()
            .unwrap_or_default();
            return Ok(Val::Text(cur));
        }
        match m {
            "skip" => with_run(|s| s.skip = true),
            "break" => with_run(|s| s.brk = true),
            "breakunbound" => with_run(|s| s.break_unbound = true),
            "quit" => with_run(|s| s.quit = true),
            _ => {
                let _ = self.eval_all_args(f, args)?;
                return Ok(Val::Text(String::new()));
            }
        };
        Err(Flow::Err(Box::new(AlError { msg: format!("currXMLport.{m}"), stack: vec![], code: XP_CONTROL })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_resolve() {
        let d = parse_xml(br#"<Invoice xmlns="urn:i" xmlns:cbc="urn:c"><cbc:ID a="1" cbc:b="2">x</cbc:ID><Line xmlns=""/></Invoice>"#).unwrap();
        assert_eq!((d.name.as_str(), d.ns.as_str()), ("Invoice", "urn:i"));
        let id = &d.children[0];
        assert_eq!((id.name.as_str(), id.ns.as_str(), id.text.as_str()), ("ID", "urn:c", "x"));
        assert_eq!(id.attr_ns, vec![String::new(), "urn:c".to_string()]);
        assert_eq!(d.children[1].ns, "");
    }

    #[test]
    fn variable_text() {
        let t = "AED;784;0.25\r\nAUD;036\r\n\r\n";
        assert_eq!(split_variable_text(t, "", ";", "\r\n"), vec![vec!["AED", "784", "0.25"], vec!["AUD", "036"]]);
        // delimited fields may hold separators; a doubled delimiter is one character; bare LF
        let t = "\"a,b\",\"say \"\"hi\"\"\",3\n\"\",x";
        assert_eq!(split_variable_text(t, "\"", ",", "\r\n"), vec![vec!["a,b", "say \"hi\"", "3"], vec!["", "x"]]);
        assert_eq!(separator_text("<None>"), "");
        assert_eq!(separator_text("<TAB>"), "\t");
        assert_eq!(separator_text(";"), ";");
    }
}
