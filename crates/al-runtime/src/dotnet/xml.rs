//! A small mutable XML DOM shared by the AL Xml* types and DotNet System.Xml: nodes are reference
//! counted (a variable holding an XmlElement refers to the node inside its document), parents are
//! weak. Parsing uses quick-xml; serialization follows .NET (`<a />` for empty elements,
//! namespace declarations added where a node's namespace is not in scope).

use quick_xml::events::Event;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

pub type Node = Rc<RefCell<NodeData>>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NK {
    Document,
    Element,
    Attribute,
    Text,
    CData,
    Comment,
    PI,
    Declaration,
    DocType,
    Whitespace,
}

pub struct NodeData {
    pub kind: NK,
    pub prefix: String,
    pub local: String,
    pub ns: String,
    /// Text / CDATA / comment / attribute value / PI data / declaration "version|encoding|standalone"
    /// / doctype "public|system|internal".
    pub value: String,
    pub attrs: Vec<Node>,
    pub children: Vec<Node>,
    pub parent: Weak<RefCell<NodeData>>,
    /// Owner document (also for nodes that are not inserted yet).
    pub owner: Weak<RefCell<NodeData>>,
    /// AL variable that was never assigned (XmlElement / XmlNode / ...): the AL type name.
    pub uninit: Option<&'static str>,
}

pub fn node(kind: NK) -> Node {
    Rc::new(RefCell::new(NodeData {
        kind,
        prefix: String::new(),
        local: String::new(),
        ns: String::new(),
        value: String::new(),
        attrs: vec![],
        children: vec![],
        parent: Weak::new(),
        owner: Weak::new(),
        uninit: None,
    }))
}

// Documents are kept alive while any node inside them is still referenced (as in .NET, where a
// node keeps its OwnerDocument alive). Parents are weak, so the registry holds the documents and
// drops the ones nobody can reach any more (no node of the tree has an outside reference).
thread_local! {
    static DOCS: RefCell<(Vec<Node>, usize)> = const { RefCell::new((Vec::new(), 256)) };
}

fn referenced(n: &Node) -> bool {
    // the parent's child/attribute vector holds one reference; anything more is a handle
    let b = n.borrow();
    b.children.iter().chain(b.attrs.iter()).any(|c| Rc::strong_count(c) > 1 || referenced(c))
}

fn register(doc: &Node) {
    DOCS.with(|d| {
        let mut d = d.borrow_mut();
        d.0.push(doc.clone());
        if d.0.len() >= d.1 {
            d.0.retain(|x| Rc::strong_count(x) > 1 || referenced(x));
            d.1 = (d.0.len() * 2).max(256);
        }
    });
}

pub fn new_document() -> Node {
    let d = node(NK::Document);
    register(&d);
    d
}

pub fn new_uninit(al_type: &str) -> Node {
    let n = node(NK::Element);
    n.borrow_mut().uninit = Some(match al_type {
        "xmlelement" => "XmlElement",
        "xmlnode" => "XmlNode",
        "xmlattribute" => "XmlAttribute",
        "xmltext" => "XmlText",
        "xmlcdata" => "XmlCData",
        "xmlcomment" => "XmlComment",
        "xmlprocessinginstruction" => "XmlProcessingInstruction",
        "xmldeclaration" => "XmlDeclaration",
        "xmldocumenttype" => "XmlDocumentType",
        _ => "XmlNode",
    });
    n
}

pub fn element(prefix: &str, local: &str, ns: &str) -> Node {
    let n = node(NK::Element);
    {
        let mut b = n.borrow_mut();
        b.prefix = prefix.into();
        b.local = local.into();
        b.ns = ns.into();
    }
    n
}

pub fn attribute(prefix: &str, local: &str, ns: &str, value: &str) -> Node {
    let n = node(NK::Attribute);
    {
        let mut b = n.borrow_mut();
        b.prefix = prefix.into();
        b.local = local.into();
        b.ns = ns.into();
        b.value = value.into();
    }
    n
}

pub fn leaf(kind: NK, value: &str) -> Node {
    let n = node(kind);
    n.borrow_mut().value = value.into();
    n
}

pub fn pi(target: &str, data: &str) -> Node {
    let n = node(NK::PI);
    n.borrow_mut().local = target.into();
    n.borrow_mut().value = data.into();
    n
}

pub fn declaration(version: &str, encoding: &str, standalone: &str) -> Node {
    let n = node(NK::Declaration);
    n.borrow_mut().value = format!("{version}|{encoding}|{standalone}");
    n
}

pub fn doctype(name: &str, public: &str, system: &str, internal: &str) -> Node {
    let n = node(NK::DocType);
    n.borrow_mut().local = name.into();
    n.borrow_mut().value = format!("{public}|{system}|{internal}");
    n
}

pub const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
pub const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

// ---------------------------------------------------------------- accessors

pub fn kind(n: &Node) -> NK {
    n.borrow().kind
}

pub fn is_uninit(n: &Node) -> bool {
    n.borrow().uninit.is_some()
}

pub fn qname(n: &Node) -> String {
    let b = n.borrow();
    match b.kind {
        NK::Element | NK::Attribute => {
            if b.prefix.is_empty() {
                b.local.clone()
            } else {
                format!("{}:{}", b.prefix, b.local)
            }
        }
        NK::Text => "#text".into(),
        NK::CData => "#cdata-section".into(),
        NK::Comment => "#comment".into(),
        NK::Document => "#document".into(),
        NK::Whitespace => "#whitespace".into(),
        NK::Declaration => "xml".into(),
        NK::PI | NK::DocType => b.local.clone(),
    }
}

pub fn parent(n: &Node) -> Option<Node> {
    n.borrow().parent.upgrade()
}

pub fn children(n: &Node) -> Vec<Node> {
    n.borrow().children.clone()
}

pub fn attributes(n: &Node) -> Vec<Node> {
    n.borrow().attrs.clone()
}

pub fn element_children(n: &Node) -> Vec<Node> {
    n.borrow().children.iter().filter(|c| c.borrow().kind == NK::Element).cloned().collect()
}

/// The document a node belongs to (walks up; falls back to the recorded owner).
pub fn document_of(n: &Node) -> Option<Node> {
    let mut cur = n.clone();
    loop {
        if cur.borrow().kind == NK::Document {
            return Some(cur);
        }
        let p = parent(&cur);
        match p {
            Some(p) => cur = p,
            None => return cur.borrow().owner.upgrade(),
        }
    }
}

pub fn root_element(doc: &Node) -> Option<Node> {
    doc.borrow().children.iter().find(|c| c.borrow().kind == NK::Element).cloned()
}

pub fn declaration_of(doc: &Node) -> Option<Node> {
    doc.borrow().children.iter().find(|c| c.borrow().kind == NK::Declaration).cloned()
}

pub fn decl_parts(n: &Node) -> (String, String, String) {
    let v = n.borrow().value.clone();
    let mut p = v.splitn(3, '|');
    (p.next().unwrap_or("").into(), p.next().unwrap_or("").into(), p.next().unwrap_or("").into())
}

/// Concatenated text of a node (XmlNode.InnerText / XElement.Value).
pub fn inner_text(n: &Node) -> String {
    let b = n.borrow();
    match b.kind {
        NK::Text | NK::CData | NK::Whitespace | NK::Attribute | NK::Comment | NK::PI => b.value.clone(),
        NK::Element | NK::Document => {
            let mut s = String::new();
            for c in &b.children {
                if matches!(c.borrow().kind, NK::Text | NK::CData | NK::Whitespace | NK::Element) {
                    s.push_str(&inner_text(c));
                }
            }
            s
        }
        _ => String::new(),
    }
}

pub fn set_inner_text(n: &Node, text: &str) {
    let k = kind(n);
    match k {
        NK::Element | NK::Document => {
            remove_all_children(n);
            if !text.is_empty() {
                append(n, &leaf(NK::Text, text));
            }
        }
        _ => n.borrow_mut().value = text.to_string(),
    }
}

/// Namespace URI bound to `prefix` at node `n` ("" = default namespace).
pub fn lookup_ns(n: &Node, prefix: &str) -> Option<String> {
    if prefix == "xml" {
        return Some(XML_NS.into());
    }
    if prefix == "xmlns" {
        return Some(XMLNS.into());
    }
    let mut cur = Some(n.clone());
    while let Some(c) = cur {
        {
            let b = c.borrow();
            if b.kind == NK::Element {
                for a in &b.attrs {
                    let ab = a.borrow();
                    if (prefix.is_empty() && ab.prefix.is_empty() && ab.local == "xmlns") || (ab.prefix == "xmlns" && ab.local == prefix) {
                        return Some(ab.value.clone());
                    }
                }
                // the element's own name binds its prefix
                if b.prefix == prefix && !b.ns.is_empty() {
                    return Some(b.ns.clone());
                }
            }
        }
        cur = parent(&c);
    }
    if prefix.is_empty() {
        Some(String::new())
    } else {
        None
    }
}

/// Prefix in scope for a namespace URI at `n`.
pub fn lookup_prefix(n: &Node, ns: &str) -> Option<String> {
    let mut cur = Some(n.clone());
    while let Some(c) = cur {
        {
            let b = c.borrow();
            for a in &b.attrs {
                let ab = a.borrow();
                if ab.value == ns {
                    if ab.prefix == "xmlns" {
                        return Some(ab.local.clone());
                    }
                    if ab.prefix.is_empty() && ab.local == "xmlns" {
                        return Some(String::new());
                    }
                }
            }
        }
        cur = parent(&c);
    }
    None
}

// ---------------------------------------------------------------- tree edits

pub fn detach(n: &Node) {
    let p = parent(n);
    if let Some(p) = p {
        let mut pb = p.borrow_mut();
        pb.children.retain(|c| !Rc::ptr_eq(c, n));
        pb.attrs.retain(|c| !Rc::ptr_eq(c, n));
    }
    n.borrow_mut().parent = Weak::new();
}

fn adopt(p: &Node, c: &Node) {
    detach(c);
    let owner = if p.borrow().kind == NK::Document { Rc::downgrade(p) } else { p.borrow().owner.clone() };
    let mut cb = c.borrow_mut();
    cb.parent = Rc::downgrade(p);
    if owner.upgrade().is_some() {
        cb.owner = owner;
    }
}

fn set_owner_deep(n: &Node, doc: &Weak<RefCell<NodeData>>) {
    n.borrow_mut().owner = doc.clone();
    let (ch, at) = (children(n), attributes(n));
    for c in ch.iter().chain(at.iter()) {
        set_owner_deep(c, doc);
    }
}

pub fn append(p: &Node, c: &Node) {
    if Rc::ptr_eq(p, c) {
        return;
    }
    adopt(p, c);
    if let Some(d) = document_of(p) {
        set_owner_deep(c, &Rc::downgrade(&d));
    }
    p.borrow_mut().children.push(c.clone());
}

pub fn prepend(p: &Node, c: &Node) {
    adopt(p, c);
    if let Some(d) = document_of(p) {
        set_owner_deep(c, &Rc::downgrade(&d));
    }
    p.borrow_mut().children.insert(0, c.clone());
}

/// Insert `c` next to `reference` in its parent (after = true: after it).
pub fn insert_sibling(reference: &Node, c: &Node, after: bool) -> bool {
    let Some(p) = parent(reference) else { return false };
    adopt(&p, c);
    if let Some(d) = document_of(&p) {
        set_owner_deep(c, &Rc::downgrade(&d));
    }
    let mut pb = p.borrow_mut();
    let i = pb.children.iter().position(|x| Rc::ptr_eq(x, reference)).unwrap_or(pb.children.len());
    let at = if after { i + 1 } else { i };
    let len = pb.children.len();
    pb.children.insert(at.min(len), c.clone());
    true
}

pub fn insert_before(p: &Node, c: &Node, reference: Option<&Node>) {
    match reference {
        Some(r) if parent(r).is_some_and(|x| Rc::ptr_eq(&x, p)) => {
            insert_sibling(r, c, false);
        }
        _ => append(p, c),
    }
}

pub fn remove_all_children(n: &Node) {
    let ch = std::mem::take(&mut n.borrow_mut().children);
    for c in ch {
        c.borrow_mut().parent = Weak::new();
    }
}

pub fn replace(old: &Node, new: &Node) -> bool {
    let Some(p) = parent(old) else { return false };
    adopt(&p, new);
    let mut pb = p.borrow_mut();
    if let Some(i) = pb.children.iter().position(|x| Rc::ptr_eq(x, old)) {
        pb.children[i] = new.clone();
        drop(pb);
        old.borrow_mut().parent = Weak::new();
        return true;
    }
    false
}

/// Set (or add) an attribute; returns the attribute node.
pub fn set_attr(e: &Node, prefix: &str, local: &str, ns: &str, value: &str) -> Node {
    let existing = e.borrow().attrs.iter().find(|a| {
        let ab = a.borrow();
        ab.local == local && (ab.ns == ns || (ns.is_empty() && ab.prefix == prefix))
    }).cloned();
    match existing {
        Some(a) => {
            a.borrow_mut().value = value.into();
            a
        }
        None => {
            let a = attribute(prefix, local, ns, value);
            adopt(e, &a);
            a.borrow_mut().owner = e.borrow().owner.clone();
            e.borrow_mut().attrs.push(a.clone());
            a
        }
    }
}

pub fn add_attr_node(e: &Node, a: &Node) {
    let (local, ns, prefix) = {
        let ab = a.borrow();
        (ab.local.clone(), ab.ns.clone(), ab.prefix.clone())
    };
    let pos = e.borrow().attrs.iter().position(|x| {
        let xb = x.borrow();
        xb.local == local && xb.ns == ns && xb.prefix == prefix
    });
    adopt(e, a);
    let mut eb = e.borrow_mut();
    match pos {
        Some(i) => eb.attrs[i] = a.clone(),
        None => eb.attrs.push(a.clone()),
    }
}

pub fn get_attr(e: &Node, name: &str, ns: Option<&str>) -> Option<Node> {
    e.borrow()
        .attrs
        .iter()
        .find(|a| {
            let ab = a.borrow();
            match ns {
                Some(ns) => ab.local == name && ab.ns == ns,
                None => {
                    let q = if ab.prefix.is_empty() { ab.local.clone() } else { format!("{}:{}", ab.prefix, ab.local) };
                    q == name
                }
            }
        })
        .cloned()
}

pub fn remove_attr(e: &Node, name: &str, ns: Option<&str>) -> bool {
    if let Some(a) = get_attr(e, name, ns) {
        detach(&a);
        return true;
    }
    false
}

/// Deep copy (unattached).
pub fn clone_deep(n: &Node) -> Node {
    let b = n.borrow();
    let c = Rc::new(RefCell::new(NodeData {
        kind: b.kind,
        prefix: b.prefix.clone(),
        local: b.local.clone(),
        ns: b.ns.clone(),
        value: b.value.clone(),
        attrs: vec![],
        children: vec![],
        parent: Weak::new(),
        owner: b.owner.clone(),
        uninit: b.uninit,
    }));
    for a in &b.attrs {
        let x = clone_deep(a);
        x.borrow_mut().parent = Rc::downgrade(&c);
        c.borrow_mut().attrs.push(x);
    }
    for ch in &b.children {
        let x = clone_deep(ch);
        x.borrow_mut().parent = Rc::downgrade(&c);
        c.borrow_mut().children.push(x);
    }
    c
}

/// Elements below `n` in document order (not including `n`).
pub fn descendants(n: &Node, out: &mut Vec<Node>) {
    for c in n.borrow().children.iter() {
        out.push(c.clone());
        descendants(c, out);
    }
}

// ---------------------------------------------------------------- parsing

pub struct ParseOptions {
    pub preserve_whitespace: bool,
}

fn split_qname(q: &str) -> (&str, &str) {
    match q.split_once(':') {
        Some((p, l)) => (p, l),
        None => ("", q),
    }
}

/// Parse a document. Errors are .NET-style messages.
pub fn parse(text: &str, opts: &ParseOptions) -> Result<Node, String> {
    let text = text.trim_start_matches('\u{feff}');
    let doc = new_document();
    let mut reader = quick_xml::Reader::from_str(text);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = true;
    let mut stack: Vec<Node> = vec![doc.clone()];
    let pos_err = |reader: &quick_xml::Reader<&[u8]>, msg: &str| -> String {
        let p = reader.buffer_position() as usize;
        let before = &text[..p.min(text.len())];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 1;
        format!("{msg} Line {line}, position {col}.")
    };
    let mut seen_root = false;
    loop {
        let ev_start = reader.buffer_position() as usize;
        let ev = match reader.read_event() {
            Ok(e) => e,
            Err(e) => return Err(pos_err(&reader, &format!("The XML is not well-formed ({e}).").replace("..", "."))),
        };
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let is_empty = matches!(ev, Event::Empty(_));
                let qn = std::str::from_utf8(e.name().as_ref()).unwrap_or("").to_string();
                let (prefix, local) = split_qname(&qn);
                let el = element(prefix, local, "");
                let parent_node = stack.last().unwrap().clone();
                if parent_node.borrow().kind == NK::Document {
                    if seen_root {
                        return Err(pos_err(&reader, "There are multiple root elements."));
                    }
                    seen_root = true;
                }
                append(&parent_node, &el);
                for a in e.attributes().with_checks(false) {
                    let a = a.map_err(|x| pos_err(&reader, &format!("The XML is not well-formed ({x}).")))?;
                    let k = std::str::from_utf8(a.key.as_ref()).unwrap_or("").to_string();
                    let v = a.unescape_value().map(|c| c.into_owned()).unwrap_or_default();
                    let (ap, al) = split_qname(&k);
                    let at = attribute(ap, al, "", &v.replace("\r\n", " ").replace(['\n', '\t', '\r'], " "));
                    adopt(&el, &at);
                    el.borrow_mut().attrs.push(at);
                }
                // resolve namespaces now that the declarations are in place
                let ns = lookup_ns(&el, prefix).unwrap_or_default();
                if !prefix.is_empty() && lookup_ns(&el, prefix).is_none() {
                    return Err(pos_err(&reader, &format!("'{prefix}' is an undeclared prefix.")));
                }
                el.borrow_mut().ns = ns;
                for at in attributes(&el) {
                    let (ap, al) = {
                        let b = at.borrow();
                        (b.prefix.clone(), b.local.clone())
                    };
                    let ans = if ap == "xmlns" || (ap.is_empty() && al == "xmlns") {
                        XMLNS.to_string()
                    } else if ap.is_empty() {
                        String::new()
                    } else {
                        lookup_ns(&el, &ap).unwrap_or_default()
                    };
                    at.borrow_mut().ns = ans;
                }
                // empty elements are closed right away
                if is_empty {
                    continue;
                }
                stack.push(el);
            }
            Event::End(_) => {
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            Event::Text(t) => {
                let s = match t.unescape() {
                    Ok(s) => s.into_owned(),
                    Err(e) => return Err(pos_err(&reader, &format!("The XML is not well-formed ({e})."))),
                };
                let p = stack.last().unwrap().clone();
                let ws_only = s.chars().all(|c| c.is_whitespace());
                if p.borrow().kind == NK::Document {
                    if !ws_only {
                        // XmlTextReader points at the first character that is not whitespace
                        let at = ev_start + text[ev_start.min(text.len())..].len() - text[ev_start.min(text.len())..].trim_start().len();
                        let before = &text[..at.min(text.len())];
                        let line = before.matches('\n').count() + 1;
                        let col = before.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 1;
                        return Err(format!("Data at the root level is invalid. Line {line}, position {col}."));
                    }
                    continue;
                }
                if ws_only && !opts.preserve_whitespace {
                    continue;
                }
                let s = s.replace("\r\n", "\n");
                // merge with a preceding text node
                let last = p.borrow().children.last().cloned();
                match last {
                    Some(l) if l.borrow().kind == NK::Text => l.borrow_mut().value.push_str(&s),
                    _ => append(&p, &leaf(if ws_only { NK::Whitespace } else { NK::Text }, &s)),
                }
            }
            Event::CData(c) => {
                let s = String::from_utf8_lossy(&c.into_inner()).into_owned();
                let p = stack.last().unwrap().clone();
                append(&p, &leaf(NK::CData, &s));
            }
            Event::Comment(c) => {
                let s = String::from_utf8_lossy(&c.into_inner()).into_owned();
                let p = stack.last().unwrap().clone();
                append(&p, &leaf(NK::Comment, &s));
            }
            Event::Decl(d) => {
                                let v = d.version().map(|x| String::from_utf8_lossy(&x).into_owned()).unwrap_or_else(|_| "1.0".into());
                let enc = d.encoding().and_then(|x| x.ok()).map(|x| String::from_utf8_lossy(&x).into_owned()).unwrap_or_default();
                let sa = d.standalone().and_then(|x| x.ok()).map(|x| String::from_utf8_lossy(&x).into_owned()).unwrap_or_default();
                append(&doc, &declaration(&v, &enc, &sa));
            }
            Event::PI(p) => {
                let raw = String::from_utf8_lossy(&p.into_inner()).into_owned();
                let (t, data) = match raw.split_once(char::is_whitespace) {
                    Some((t, d)) => (t.to_string(), d.trim_start().to_string()),
                    None => (raw.clone(), String::new()),
                };
                let parent_node = stack.last().unwrap().clone();
                append(&parent_node, &pi(&t, &data));
            }
            Event::DocType(d) => {
                let raw = String::from_utf8_lossy(&d.into_inner()).into_owned();
                let raw = raw.trim();
                let (name, rest) = match raw.split_once(char::is_whitespace) {
                    Some((n, r)) => (n.to_string(), r.trim().to_string()),
                    None => (raw.to_string(), String::new()),
                };
                let internal = rest.find('[').map(|i| rest[i + 1..rest.rfind(']').unwrap_or(rest.len())].to_string()).unwrap_or_default();
                let ext = rest.split('[').next().unwrap_or("").trim().to_string();
                let quoted: Vec<String> = ext.split(['"', '\'']).enumerate().filter(|(i, _)| i % 2 == 1).map(|(_, s)| s.to_string()).collect();
                let (public, system) = if ext.starts_with("PUBLIC") {
                    (quoted.first().cloned().unwrap_or_default(), quoted.get(1).cloned().unwrap_or_default())
                } else if ext.starts_with("SYSTEM") {
                    (String::new(), quoted.first().cloned().unwrap_or_default())
                } else {
                    (String::new(), String::new())
                };
                append(&doc, &doctype(&name, &public, &system, &internal));
            }
            Event::Eof => break,
        }
    }
    if stack.len() > 1 {
        let open = qname(stack.last().unwrap());
        return Err(pos_err(&reader, &format!("Unexpected end of file has occurred. The following elements are not closed: {open}.")));
    }
    if !seen_root {
        return Err("Root element is missing.".into());
    }
    Ok(doc)
}

/// Parse a fragment (InnerXml / XmlElement content): wrapped in a dummy root.
pub fn parse_fragment(text: &str, ns_context: Option<&Node>) -> Result<Vec<Node>, String> {
    // carry namespace declarations in scope so prefixes resolve
    let mut decls = String::new();
    if let Some(ctx) = ns_context {
        let mut cur = Some(ctx.clone());
        while let Some(c) = cur {
            for a in attributes(&c) {
                let ab = a.borrow();
                if ab.prefix == "xmlns" || (ab.prefix.is_empty() && ab.local == "xmlns") {
                    decls.push_str(&format!(" {}=\"{}\"", if ab.prefix.is_empty() { "xmlns".to_string() } else { format!("xmlns:{}", ab.local) }, escape_attr(&ab.value)));
                }
            }
            cur = parent(&c);
        }
    }
    let doc = parse(&format!("<__frag{decls}>{text}</__frag>"), &ParseOptions { preserve_whitespace: true })?;
    let root = root_element(&doc).unwrap();
    let ch = children(&root);
    for c in &ch {
        c.borrow_mut().parent = Weak::new();
    }
    Ok(ch)
}

// ---------------------------------------------------------------- serialization

pub fn escape_text(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            c => o.push(c),
        }
    }
    o
}

pub fn escape_attr(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\n' => o.push_str("&#xA;"),
            '\r' => o.push_str("&#xD;"),
            '\t' => o.push_str("&#x9;"),
            c => o.push(c),
        }
    }
    o
}

pub struct WriteOpts {
    pub indent: bool,
    pub decl: bool,
}

/// Namespace bindings that are in scope while writing.
type Scope = Vec<(String, String)>;

fn scope_lookup<'a>(scope: &'a Scope, prefix: &str) -> Option<&'a str> {
    scope.iter().rev().find(|(p, _)| p == prefix).map(|(_, u)| u.as_str())
}

fn write_node(n: &Node, out: &mut String, scope: &mut Scope, opts: &WriteOpts, depth: usize) {
    let b = n.borrow();
    match b.kind {
        NK::Document => {
            let mut first = true;
            for c in &b.children {
                if c.borrow().kind == NK::Declaration && !opts.decl {
                    continue;
                }
                if opts.indent && !first {
                    out.push_str("\r\n");
                }
                first = false;
                write_node(c, out, scope, opts, depth);
            }
        }
        NK::Element => {
            let mark = scope.len();
            let mut name = if b.prefix.is_empty() { b.local.clone() } else { format!("{}:{}", b.prefix, b.local) };
            out.push('<');
            let mut extra = String::new();
            // explicit xmlns attributes
            for a in &b.attrs {
                let ab = a.borrow();
                if ab.prefix == "xmlns" {
                    scope.push((ab.local.clone(), ab.value.clone()));
                } else if ab.prefix.is_empty() && ab.local == "xmlns" {
                    scope.push((String::new(), ab.value.clone()));
                }
            }
            // the element's namespace: add a declaration when it is not in scope
            if scope_lookup(scope, &b.prefix).unwrap_or("") != b.ns {
                if b.prefix.is_empty() && !b.ns.is_empty() {
                    // LINQ to XML style: reuse a prefix bound to this namespace if any
                    if let Some((p, _)) = scope.iter().rev().find(|(p, u)| u == &b.ns && !p.is_empty() && scope_lookup(scope, p) == Some(u.as_str())).cloned() {
                        name = format!("{p}:{}", b.local);
                    } else {
                        extra.push_str(&format!(" xmlns=\"{}\"", escape_attr(&b.ns)));
                        scope.push((String::new(), b.ns.clone()));
                    }
                } else if b.prefix.is_empty() && b.ns.is_empty() {
                    extra.push_str(" xmlns=\"\"");
                    scope.push((String::new(), String::new()));
                } else {
                    extra.push_str(&format!(" xmlns:{}=\"{}\"", b.prefix, escape_attr(&b.ns)));
                    scope.push((b.prefix.clone(), b.ns.clone()));
                }
            }
            out.push_str(&name);
            for a in &b.attrs {
                let ab = a.borrow();
                let mut an = if ab.prefix.is_empty() { ab.local.clone() } else { format!("{}:{}", ab.prefix, ab.local) };
                if !ab.ns.is_empty() && ab.ns != XMLNS && ab.ns != XML_NS {
                    if ab.prefix.is_empty() || scope_lookup(scope, &ab.prefix) != Some(ab.ns.as_str()) {
                        match scope.iter().rev().find(|(p, u)| u == &ab.ns && !p.is_empty()).cloned() {
                            Some((p, _)) => an = format!("{p}:{}", ab.local),
                            None => {
                                let p = if ab.prefix.is_empty() { format!("p{}", scope.len()) } else { ab.prefix.clone() };
                                extra.push_str(&format!(" xmlns:{p}=\"{}\"", escape_attr(&ab.ns)));
                                scope.push((p.clone(), ab.ns.clone()));
                                an = format!("{p}:{}", ab.local);
                            }
                        }
                    }
                }
                out.push_str(&format!(" {an}=\"{}\"", escape_attr(&ab.value)));
            }
            out.push_str(&extra);
            if b.children.is_empty() {
                out.push_str(" />");
            } else {
                out.push('>');
                let mixed = b.children.iter().any(|c| matches!(c.borrow().kind, NK::Text | NK::CData | NK::Whitespace));
                for c in &b.children {
                    if opts.indent && !mixed {
                        out.push_str("\r\n");
                        out.push_str(&"  ".repeat(depth + 1));
                    }
                    write_node(c, out, scope, opts, depth + 1);
                }
                if opts.indent && !mixed {
                    out.push_str("\r\n");
                    out.push_str(&"  ".repeat(depth));
                }
                out.push_str(&format!("</{name}>"));
            }
            scope.truncate(mark);
        }
        NK::Attribute => out.push_str(&escape_attr(&b.value)),
        NK::Text | NK::Whitespace => out.push_str(&escape_text(&b.value)),
        NK::CData => out.push_str(&format!("<![CDATA[{}]]>", b.value)),
        NK::Comment => out.push_str(&format!("<!--{}-->", b.value)),
        NK::PI => {
            if b.value.is_empty() {
                out.push_str(&format!("<?{}?>", b.local));
            } else {
                out.push_str(&format!("<?{} {}?>", b.local, b.value));
            }
        }
        NK::Declaration => {
            drop(b);
            let (v, e, s) = decl_parts(n);
            out.push_str(&format!("<?xml version=\"{}\"", if v.is_empty() { "1.0" } else { &v }));
            if !e.is_empty() {
                out.push_str(&format!(" encoding=\"{e}\""));
            }
            if !s.is_empty() {
                out.push_str(&format!(" standalone=\"{s}\""));
            }
            out.push_str("?>");
        }
        NK::DocType => {
            let parts: Vec<&str> = b.value.splitn(3, '|').collect();
            let (p, s, i) = (parts.first().copied().unwrap_or(""), parts.get(1).copied().unwrap_or(""), parts.get(2).copied().unwrap_or(""));
            out.push_str(&format!("<!DOCTYPE {}", b.local));
            if !p.is_empty() {
                out.push_str(&format!(" PUBLIC \"{p}\" \"{s}\""));
            } else if !s.is_empty() {
                out.push_str(&format!(" SYSTEM \"{s}\""));
            }
            if !i.is_empty() {
                out.push_str(&format!(" [{i}]"));
            }
            out.push('>');
        }
    }
}

/// Namespace bindings in scope above `n` (for serializing a node inside a document).
fn ancestor_scope(n: &Node) -> Scope {
    let mut chain = vec![];
    let mut cur = parent(n);
    while let Some(c) = cur {
        chain.push(c.clone());
        cur = parent(&c);
    }
    let mut scope = vec![];
    for c in chain.iter().rev() {
        let b = c.borrow();
        if b.kind != NK::Element {
            continue;
        }
        for a in &b.attrs {
            let ab = a.borrow();
            if ab.prefix == "xmlns" {
                scope.push((ab.local.clone(), ab.value.clone()));
            } else if ab.prefix.is_empty() && ab.local == "xmlns" {
                scope.push((String::new(), ab.value.clone()));
            }
        }
        scope.push((b.prefix.clone(), b.ns.clone()));
    }
    scope
}

pub fn write(n: &Node, opts: &WriteOpts) -> String {
    let mut out = String::new();
    let mut scope = ancestor_scope(n);
    write_node(n, &mut out, &mut scope, opts, 0);
    out
}

/// .NET OuterXml (no indentation, declaration included).
pub fn outer_xml(n: &Node) -> String {
    write(n, &WriteOpts { indent: false, decl: true })
}

pub fn inner_xml(n: &Node) -> String {
    let b = n.borrow();
    match b.kind {
        NK::Element | NK::Document => {
            let mut out = String::new();
            let mut scope = ancestor_scope(n);
            if b.kind == NK::Element {
                scope.push((b.prefix.clone(), b.ns.clone()));
                for a in &b.attrs {
                    let ab = a.borrow();
                    if ab.prefix == "xmlns" {
                        scope.push((ab.local.clone(), ab.value.clone()));
                    } else if ab.prefix.is_empty() && ab.local == "xmlns" {
                        scope.push((String::new(), ab.value.clone()));
                    }
                }
            }
            let opts = WriteOpts { indent: false, decl: true };
            for c in &b.children {
                write_node(c, &mut out, &mut scope, &opts, 0);
            }
            out
        }
        NK::Attribute => escape_attr(&b.value),
        _ => b.value.clone(),
    }
}

/// AL type name of a node value.
pub fn al_type_name(n: &Node) -> &'static str {
    let b = n.borrow();
    if let Some(t) = b.uninit {
        return t;
    }
    match b.kind {
        NK::Document => "XmlDocument",
        NK::Element => "XmlElement",
        NK::Attribute => "XmlAttribute",
        NK::Text | NK::Whitespace => "XmlText",
        NK::CData => "XmlCData",
        NK::Comment => "XmlComment",
        NK::PI => "XmlProcessingInstruction",
        NK::Declaration => "XmlDeclaration",
        NK::DocType => "XmlDocumentType",
    }
}

pub fn net_type_name(n: &Node) -> &'static str {
    match kind(n) {
        NK::Document => "System.Xml.XmlDocument",
        NK::Element => "System.Xml.XmlElement",
        NK::Attribute => "System.Xml.XmlAttribute",
        NK::Text => "System.Xml.XmlText",
        NK::Whitespace => "System.Xml.XmlWhitespace",
        NK::CData => "System.Xml.XmlCDataSection",
        NK::Comment => "System.Xml.XmlComment",
        NK::PI => "System.Xml.XmlProcessingInstruction",
        NK::Declaration => "System.Xml.XmlDeclaration",
        NK::DocType => "System.Xml.XmlDocumentType",
    }
}

/// System.Xml.XmlNodeType value.
pub fn node_type_num(n: &Node) -> i64 {
    match kind(n) {
        NK::Element => 1,
        NK::Attribute => 2,
        NK::Text => 3,
        NK::CData => 4,
        NK::PI => 7,
        NK::Comment => 8,
        NK::Document => 9,
        NK::DocType => 10,
        NK::Whitespace => 13,
        NK::Declaration => 17,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let src = r#"<?xml version="1.0" encoding="utf-8"?><a xmlns="urn:x" xmlns:p="urn:p"><p:b c="1">t&amp;x</p:b><d /></a>"#;
        let d = parse(src, &ParseOptions { preserve_whitespace: false }).unwrap();
        assert_eq!(outer_xml(&d), src);
        let r = root_element(&d).unwrap();
        assert_eq!(r.borrow().ns, "urn:x");
        let b = element_children(&r)[0].clone();
        assert_eq!(b.borrow().ns, "urn:p");
        assert_eq!(inner_text(&r), "t&x");
    }

    #[test]
    fn new_nodes_get_declarations() {
        let d = new_document();
        let r = element("", "root", "urn:r");
        append(&d, &r);
        let c = element("", "child", "urn:r");
        append(&r, &c);
        set_attr(&c, "", "id", "", "5");
        assert_eq!(outer_xml(&d), r#"<root xmlns="urn:r"><child id="5" /></root>"#);
        assert!(parse("<a><b></a>", &ParseOptions { preserve_whitespace: false }).is_err());
    }
}
