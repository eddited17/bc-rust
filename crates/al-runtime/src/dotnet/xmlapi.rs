//! The AL Xml* types (XmlDocument, XmlElement, XmlNode, XmlNodeList, XmlAttribute,
//! XmlAttributeCollection, XmlNamespaceManager, XmlDeclaration, XmlText, ... built on LINQ to XML
//! in BC) and the DotNet System.Xml types (XmlDocument, XmlNode, XmlNodeList, XmlNamespaceManager,
//! ...), both over the DOM in xml.rs.

use super::xml::{self, Node, NK};
use super::*;

fn xnode(n: Node, al: bool) -> Val {
    net(NetObj::XNode(n, al))
}

fn node_arg(v: Option<&Val>) -> Option<Node> {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::XNode(x, _) if !xml::is_uninit(x) => Some(x.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn nsmgr_arg(v: Option<&Val>) -> Option<Vec<(String, String)>> {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::XNsMgr(m, _) => Some(m.borrow().clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Text to parse from a ReadFrom/Load argument: Text, InStream, .NET stream or reader.
fn source_text(v: Option<&Val>) -> String {
    super::text::read_all_text(v)
}

fn preserve_ws(v: Option<&Val>) -> bool {
    match v {
        Some(Val::Net(n)) => matches!(&*n.borrow(), NetObj::XReadOptions(true) | NetObj::XWriteOptions(true)),
        _ => false,
    }
}

/// Write serialized XML to a `var Text` or an OutStream / .NET stream argument.
fn write_out(s: &mut Session, f: &mut Frame, a: &Args, i: usize, text: &str) -> R<()> {
    match a.raw(i) {
        Some(Val::OutStream(t)) => {
            super::text::outstream_append(t, text.as_bytes());
            Ok(())
        }
        Some(Val::Net(n)) if matches!(&*n.borrow(), NetObj::Stream(..) | NetObj::StreamWriter(_)) => {
            let target = match &*n.borrow() {
                NetObj::StreamWriter(t) => t.clone(),
                _ => Val::Net(n.clone()),
            };
            super::text::stream_write(&target, text.as_bytes());
            Ok(())
        }
        Some(Val::Net(n)) if matches!(&*n.borrow(), NetObj::XmlWriter(_)) => {
            super::xmlwriter::write_raw(&Val::Net(n.clone()), text);
            Ok(())
        }
        Some(Val::Net(n)) if matches!(&*n.borrow(), NetObj::Writer(..) | NetObj::StringBuilder(_)) => {
            match &mut *n.borrow_mut() {
                NetObj::Writer(_, b) | NetObj::StringBuilder(b) => b.push_str(text),
                _ => {}
            }
            Ok(())
        }
        _ => s.net_out(f, a, i, Val::Text(text.to_string())),
    }
}

fn xml_err<T>(msg: impl Into<String>) -> R<T> {
    err(msg.into())
}

/// Content for Add/AddFirst/...: nodes (cloned when they already have a parent, as in LINQ to
/// XML), attributes, or text.
fn content_nodes(a: &Args, from: usize) -> Vec<Node> {
    let mut out = vec![];
    for i in from..a.len() {
        match a.raw(i) {
            Some(Val::Net(n)) => match &*n.borrow() {
                NetObj::XNode(x, _) if !xml::is_uninit(x) => {
                    let x = if xml::parent(x).is_some() || xml::kind(x) == NK::Document { clone_content(x) } else { x.clone() };
                    out.push(x);
                }
                NetObj::XNodeList(l, _) => out.extend(l.iter().map(clone_content)),
                _ => {}
            },
            Some(Val::Void) | None => {}
            Some(v) => out.push(xml::leaf(NK::Text, &v.to_text())),
        }
    }
    out
}

fn clone_content(x: &Node) -> Node {
    if xml::kind(x) == NK::Document {
        return xml::root_element(x).map(|r| xml::clone_deep(&r)).unwrap_or_else(|| xml::leaf(NK::Text, ""));
    }
    xml::clone_deep(x)
}

fn add_content(target: &Node, nodes: Vec<Node>, first: bool) {
    let mut at = 0;
    for c in nodes {
        if xml::kind(&c) == NK::Attribute {
            xml::add_attr_node(target, &c);
            continue;
        }
        if xml::kind(&c) == NK::Text {
            // adjacent text merges (LINQ to XML)
            let last = if first { None } else { xml::children(target).last().cloned() };
            if let Some(l) = last.filter(|l| xml::kind(l) == NK::Text) {
                let v = c.borrow().value.clone();
                l.borrow_mut().value.push_str(&v);
                continue;
            }
        }
        if first {
            xml::prepend(target, &c);
            // keep the order of several items
            let mut tb = target.borrow_mut();
            let pos = tb.children.iter().position(|x| Rc::ptr_eq(x, &c)).unwrap_or(0);
            let item = tb.children.remove(pos);
            let len = tb.children.len();
            tb.children.insert(at.min(len), item);
            at += 1;
        } else {
            xml::append(target, &c);
        }
    }
}

fn filter_elements(nodes: Vec<Node>, a: &Args) -> Vec<Node> {
    if a.is_empty() {
        return nodes;
    }
    let name = a.text(0);
    let ns = if a.len() > 1 { Some(a.text(1)) } else { None };
    nodes
        .into_iter()
        .filter(|e| {
            let b = e.borrow();
            match &ns {
                Some(ns) => b.local == name && b.ns == *ns,
                None => b.local == name && b.ns.is_empty() || (b.ns.is_empty() && xml::qname(e) == name),
            }
        })
        .collect()
}

fn descendant_elements(n: &Node) -> Vec<Node> {
    let mut d = vec![];
    xml::descendants(n, &mut d);
    d.into_iter().filter(|x| xml::kind(x) == NK::Element).collect()
}

impl<'r> Session<'r> {
    /// Static members of the AL Xml types: `XmlDocument.ReadFrom`, `XmlElement.Create`, ...
    pub fn al_xml_static(&mut self, f: &mut Frame, class: &str, m: &str, a: &Args, discard: bool) -> R<Out> {
        match (class, m) {
            ("xmldocument", "readfrom") => self.xml_read_from(f, a, discard),
            ("xmldocument", "create") => {
                let d = xml::new_document();
                add_content(&d, content_nodes(a, 0), false);
                ok(xnode(d, true))
            }
            ("xmlelement", "create") => {
                let name = a.text(0);
                let (ns, from) = match a.raw(1) {
                    Some(Val::Text(_)) => (a.text(1), 2),
                    _ => (String::new(), 1),
                };
                if name.contains(':') || name.is_empty() || name.contains(' ') {
                    return xml_err(format!("The '{}' character, hexadecimal value 0x{:X}, cannot be included in a name.", name.chars().find(|c| *c == ':' || *c == ' ').unwrap_or(' '), name.chars().find(|c| *c == ':' || *c == ' ').unwrap_or(' ') as u32));
                }
                let e = xml::element("", &name, &ns);
                add_content(&e, content_nodes(a, from), false);
                ok(xnode(e, true))
            }
            ("xmlattribute", "create") => {
                let (local, ns, v) = if a.len() >= 3 { (a.text(0), a.text(1), a.text(2)) } else { (a.text(0), String::new(), a.text(1)) };
                let prefix = if local == "xmlns" { "" } else { "" };
                ok(xnode(xml::attribute(prefix, &local, if local == "xmlns" { xml::XMLNS } else { &ns }, &v), true))
            }
            ("xmlattribute", "createnamespacedeclaration") => {
                let (p, ns) = (a.text(0), a.text(1));
                let at = if p.is_empty() { xml::attribute("", "xmlns", xml::XMLNS, &ns) } else { xml::attribute("xmlns", &p, xml::XMLNS, &ns) };
                ok(xnode(at, true))
            }
            ("xmltext", "create") => ok(xnode(xml::leaf(NK::Text, &a.text(0)), true)),
            ("xmlcdata", "create") => ok(xnode(xml::leaf(NK::CData, &a.text(0)), true)),
            ("xmlcomment", "create") => ok(xnode(xml::leaf(NK::Comment, &a.text(0)), true)),
            ("xmlprocessinginstruction", "create") => ok(xnode(xml::pi(&a.text(0), &a.text(1)), true)),
            ("xmldeclaration", "create") => ok(xnode(xml::declaration(&a.text(0), &a.text(1), &a.text(2)), true)),
            ("xmldocumenttype", "create") => ok(xnode(xml::doctype(&a.text(0), &a.text(1), &a.text(2), &a.text(3)), true)),
            ("xmlnamespacemanager", "create") => ok(net(NetObj::XNsMgr(Rc::new(std::cell::RefCell::new(vec![])), true))),
            _ => Ok(Out::Unknown),
        }
    }

    fn xml_read_from(&mut self, f: &mut Frame, a: &Args, discard: bool) -> R<Out> {
        let text = source_text(a.raw(0));
        let ws = a.len() == 3 && preserve_ws(a.raw(1));
        let out_i = a.len() - 1;
        match xml::parse(&text, &xml::ParseOptions { preserve_whitespace: ws }) {
            Ok(d) => {
                // ReadFrom(..., var XmlElement) reads an element
                let target_is_element = matches!(a.raw(out_i), Some(Val::Net(n)) if matches!(&*n.borrow(), NetObj::XNode(x, true) if xml::al_type_name(x) == "XmlElement"));
                let v = if target_is_element {
                    let r = xml::root_element(&d).unwrap();
                    xml::detach(&r);
                    xnode(r, true)
                } else {
                    xnode(d, true)
                };
                self.net_out(f, a, out_i, v)?;
                ok(Val::Bool(true))
            }
            Err(e) => {
                if discard {
                    return xml_err(e);
                }
                self.last_error = e;
                ok(Val::Bool(false))
            }
        }
    }

    /// Members of the AL Xml types.
    pub fn al_xml_call(&mut self, f: &mut Frame, n: &NetRef, m: &str, a: &Args, discard: bool) -> R<Out> {
        let o = n.borrow();
        match &*o {
            NetObj::XNode(x, _) => {
                let x = x.clone();
                drop(o);
                self.al_node_call(f, n, &x, m, a, discard)
            }
            NetObj::XNodeList(l, _) => {
                let l = l.clone();
                drop(o);
                ok(match m {
                    "count" => Val::Int(l.len() as i32),
                    "get" => {
                        let i = a.int(0);
                        let item = l.get((i - 1).max(0) as usize).filter(|_| i >= 1).cloned();
                        if a.len() == 2 {
                            let r = item.is_some();
                            if let Some(x) = item {
                                self.net_out(f, a, 1, xnode(x, true))?;
                            }
                            Val::Bool(r)
                        } else {
                            match item {
                                Some(x) => xnode(x, true),
                                None => return xml_err(format!("Index {i} is out of range of the XmlNodeList (Count = {}).", l.len())),
                            }
                        }
                    }
                    _ => return Ok(Out::Unknown),
                })
            }
            NetObj::XAttrs(e, _) => {
                let e = e.clone();
                drop(o);
                let attrs: Vec<Node> = xml::attributes(&e);
                ok(match m {
                    "count" => Val::Int(attrs.len() as i32),
                    "get" => {
                        let found = match a.raw(0) {
                            Some(Val::Int(_) | Val::BigInt(_)) => {
                                let i = a.int(0);
                                attrs.get((i - 1).max(0) as usize).filter(|_| i >= 1).cloned()
                            }
                            _ if a.len() == 3 => xml::get_attr(&e, &a.text(0), Some(&a.text(1))),
                            _ => xml::get_attr(&e, &a.text(0), None),
                        };
                        let r = found.is_some();
                        if let Some(x) = found {
                            self.net_out(f, a, a.len() - 1, xnode(x, true))?;
                        }
                        Val::Bool(r)
                    }
                    "set" => {
                        let (local, ns, v) = if a.len() == 3 { (a.text(0), a.text(1), a.text(2)) } else { (a.text(0), String::new(), a.text(1)) };
                        xml::set_attr(&e, "", &local, &ns, &v);
                        Val::Void
                    }
                    "remove" => {
                        let r = match a.raw(0) {
                            Some(Val::Net(_)) => match node_arg(a.raw(0)) {
                                Some(at) => {
                                    xml::detach(&at);
                                    true
                                }
                                None => false,
                            },
                            _ if a.len() == 2 => xml::remove_attr(&e, &a.text(0), Some(&a.text(1))),
                            _ => xml::remove_attr(&e, &a.text(0), None),
                        };
                        Val::Bool(r)
                    }
                    "removeall" => {
                        for at in attrs {
                            xml::detach(&at);
                        }
                        Val::Void
                    }
                    _ => return Ok(Out::Unknown),
                })
            }
            NetObj::XNsMgr(mgr, _) => {
                let mgr = mgr.clone();
                drop(o);
                ok(match m {
                    "addnamespace" => {
                        mgr.borrow_mut().push((a.text(0), a.text(1)));
                        Val::Void
                    }
                    "removenamespace" => {
                        let (p, u) = (a.text(0), a.text(1));
                        mgr.borrow_mut().retain(|(x, y)| !(x == &p && y == &u));
                        Val::Void
                    }
                    "hasnamespace" => Val::Bool(mgr.borrow().iter().any(|(p, _)| *p == a.text(0))),
                    "lookupnamespace" => {
                        let found = mgr.borrow().iter().rev().find(|(p, _)| *p == a.text(0)).map(|(_, u)| u.clone());
                        if a.len() == 2 {
                            let r = found.is_some();
                            self.net_out(f, a, 1, Val::Text(found.unwrap_or_default()))?;
                            Val::Bool(r)
                        } else {
                            Val::Text(found.unwrap_or_default())
                        }
                    }
                    "lookupprefix" => {
                        let found = mgr.borrow().iter().rev().find(|(_, u)| *u == a.text(0)).map(|(p, _)| p.clone());
                        if a.len() == 2 {
                            let r = found.is_some();
                            self.net_out(f, a, 1, Val::Text(found.unwrap_or_default()))?;
                            Val::Bool(r)
                        } else {
                            Val::Text(found.unwrap_or_default())
                        }
                    }
                    "nametable" | "pushscope" | "popscope" => Val::Void,
                    _ => return Ok(Out::Unknown),
                })
            }
            NetObj::XNameTable(_) => Ok(Out::Unknown),
            NetObj::XReadOptions(p) | NetObj::XWriteOptions(p) => {
                let cur = *p;
                drop(o);
                if m != "preservewhitespace" {
                    return Ok(Out::Unknown);
                }
                if a.len() == 1 {
                    let v = a.bool(0);
                    match &mut *n.borrow_mut() {
                        NetObj::XReadOptions(p) | NetObj::XWriteOptions(p) => *p = v,
                        _ => {}
                    }
                    return void();
                }
                ok(Val::Bool(cur))
            }
            _ => Ok(Out::Unknown),
        }
    }

    fn al_node_call(&mut self, f: &mut Frame, _this: &NetRef, x: &Node, m: &str, a: &Args, discard: bool) -> R<Out> {
        // statics reachable through a variable of the type
        match m {
            "readfrom" => return self.xml_read_from(f, a, discard),
            "create" | "createnamespacedeclaration" => {
                let class = xml::al_type_name(x).to_ascii_lowercase();
                return self.al_xml_static(f, &class, m, a, discard);
            }
            _ => {}
        }
        if xml::is_uninit(x) {
            let t = xml::al_type_name(x);
            if m.starts_with("is") {
                return ok(Val::Bool(false));
            }
            return xml_err(format!("The {t} variable has not been initialized (calling {m})."));
        }
        let k = xml::kind(x);
        let as_kind = |want: &[NK], tname: &str| -> R<Out> {
            if want.contains(&k) {
                ok(xnode(x.clone(), true))
            } else {
                xml_err(format!("Unable to convert from Microsoft.Dynamics.Nav.Runtime.Nav{} to Microsoft.Dynamics.Nav.Runtime.Nav{tname}.", xml::al_type_name(x)))
            }
        };
        match m {
            "asxmlnode" => return ok(xnode(x.clone(), true)),
            "asxmlelement" => return as_kind(&[NK::Element], "XmlElement"),
            "asxmldocument" => return as_kind(&[NK::Document], "XmlDocument"),
            "asxmlattribute" => return as_kind(&[NK::Attribute], "XmlAttribute"),
            "asxmltext" => return as_kind(&[NK::Text, NK::Whitespace], "XmlText"),
            "asxmlcdata" => return as_kind(&[NK::CData], "XmlCData"),
            "asxmlcomment" => return as_kind(&[NK::Comment], "XmlComment"),
            "asxmldeclaration" => return as_kind(&[NK::Declaration], "XmlDeclaration"),
            "asxmldocumenttype" => return as_kind(&[NK::DocType], "XmlDocumentType"),
            "asxmlprocessinginstruction" => return as_kind(&[NK::PI], "XmlProcessingInstruction"),
            "isxmlelement" => return ok(Val::Bool(k == NK::Element)),
            "isxmldocument" => return ok(Val::Bool(k == NK::Document)),
            "isxmlattribute" => return ok(Val::Bool(k == NK::Attribute)),
            "isxmltext" => return ok(Val::Bool(matches!(k, NK::Text | NK::Whitespace))),
            "isxmlcdata" => return ok(Val::Bool(k == NK::CData)),
            "isxmlcomment" => return ok(Val::Bool(k == NK::Comment)),
            "isxmldeclaration" => return ok(Val::Bool(k == NK::Declaration)),
            "isxmldocumenttype" => return ok(Val::Bool(k == NK::DocType)),
            "isxmlprocessinginstruction" => return ok(Val::Bool(k == NK::PI)),
            "writeto" => {
                let (opts_i, out_i) = if a.len() == 2 { (Some(0), 1) } else { (None, 0) };
                let indent = !opts_i.is_some_and(|i| preserve_ws(a.raw(i)));
                let text = xml::write(x, &xml::WriteOpts { indent, decl: true });
                write_out(self, f, a, out_i, &text)?;
                return ok(Val::Bool(true));
            }
            "selectnodes" | "selectsinglenode" => {
                let ns = if a.len() == 3 { nsmgr_arg(a.raw(1)).unwrap_or_default() } else { vec![] };
                let res = match xml::kind(x) {
                    _ => xpath::select(x, &a.text(0), &ns),
                };
                let nodes = match res {
                    Ok(n) => n,
                    Err(e) if discard => return err(e),
                    Err(e) => {
                        self.last_error = e;
                        return ok(Val::Bool(false));
                    }
                };
                let out_i = a.len() - 1;
                if m == "selectnodes" {
                    self.net_out(f, a, out_i, net(NetObj::XNodeList(nodes, true)))?;
                    return ok(Val::Bool(true));
                }
                return match nodes.into_iter().next() {
                    Some(nd) => {
                        self.net_out(f, a, out_i, xnode(nd, true))?;
                        ok(Val::Bool(true))
                    }
                    None if discard => xml_err(format!("The XPath expression '{}' did not select a node.", a.text(0))),
                    None => ok(Val::Bool(false)),
                };
            }
            "getdocument" | "ownerdocument" => {
                let d = xml::document_of(x);
                if a.len() == 1 {
                    let r = d.is_some();
                    if let Some(d) = d {
                        self.net_out(f, a, 0, xnode(d, true))?;
                    }
                    return ok(Val::Bool(r));
                }
                return ok(d.map(|d| xnode(d, true)).unwrap_or(Val::Void));
            }
            "getparent" => {
                let p = xml::parent(x).filter(|p| xml::kind(p) == NK::Element);
                if a.len() == 1 {
                    let r = p.is_some();
                    if let Some(p) = p {
                        self.net_out(f, a, 0, xnode(p, true))?;
                    }
                    return ok(Val::Bool(r));
                }
                return ok(p.map(|p| xnode(p, true)).unwrap_or(Val::Void));
            }
            "remove" => {
                xml::detach(x);
                return void();
            }
            "replacewith" => {
                let nodes = content_nodes(a, 0);
                let Some(p) = xml::parent(x) else { return xml_err("The parent is missing.") };
                let mut prev = x.clone();
                for c in nodes {
                    xml::insert_sibling(&prev, &c, true);
                    prev = c;
                }
                let _ = p;
                xml::detach(x);
                return void();
            }
            "addafterself" | "addbeforeself" => {
                if xml::parent(x).is_none() {
                    return xml_err("The parent is missing.");
                }
                let nodes = content_nodes(a, 0);
                let mut anchor = x.clone();
                if m == "addafterself" {
                    for c in nodes {
                        xml::insert_sibling(&anchor, &c, true);
                        anchor = c;
                    }
                } else {
                    for c in nodes {
                        xml::insert_sibling(&anchor, &c, false);
                    }
                }
                return void();
            }
            "innertext" if a.len() == 1 => {
                xml::set_inner_text(x, &a.text(0));
                return void();
            }
            "innertext" | "value" if !matches!(k, NK::Declaration) => return ok(Val::Text(xml::inner_text(x))),
            "innerxml" => return ok(Val::Text(xml::inner_xml(x))),
            "outerxml" => return ok(Val::Text(xml::outer_xml(x))),
            _ => {}
        }
        match k {
            NK::Document => self.al_document_call(f, x, m, a),
            NK::Element => self.al_element_call(f, x, m, a),
            NK::Attribute => ok(match m {
                "name" => Val::Text(xml::qname(x)),
                "localname" => Val::Text(x.borrow().local.clone()),
                "namespaceuri" => Val::Text(x.borrow().ns.clone()),
                "namespaceprefix" | "prefix" => Val::Text(x.borrow().prefix.clone()),
                "value" if a.len() == 1 => {
                    x.borrow_mut().value = a.text(0);
                    Val::Void
                }
                "isnamespacedeclaration" => Val::Bool(x.borrow().ns == xml::XMLNS),
                _ => return Ok(Out::Unknown),
            }),
            NK::Declaration => {
                let (v, e, s) = xml::decl_parts(x);
                ok(match m {
                    "version" => Val::Text(v),
                    "encoding" => Val::Text(e),
                    "standalone" => Val::Text(s),
                    _ => return Ok(Out::Unknown),
                })
            }
            NK::DocType => {
                let parts: Vec<String> = x.borrow().value.splitn(3, '|').map(String::from).collect();
                ok(match m {
                    "name" | "getname" => Val::Text(x.borrow().local.clone()),
                    "publicid" | "getpublicid" => Val::Text(parts.first().cloned().unwrap_or_default()),
                    "systemid" | "getsystemid" => Val::Text(parts.get(1).cloned().unwrap_or_default()),
                    "internalsubset" | "getinternalsubset" => Val::Text(parts.get(2).cloned().unwrap_or_default()),
                    _ => return Ok(Out::Unknown),
                })
            }
            NK::PI => ok(match m {
                "target" | "gettarget" => Val::Text(x.borrow().local.clone()),
                "data" | "getdata" => Val::Text(x.borrow().value.clone()),
                _ => return Ok(Out::Unknown),
            }),
            _ => ok(match m {
                "value" if a.len() == 1 => {
                    x.borrow_mut().value = a.text(0);
                    Val::Void
                }
                _ => return Ok(Out::Unknown),
            }),
        }
    }

    fn al_container_call(&mut self, f: &mut Frame, x: &Node, m: &str, a: &Args) -> R<Option<Val>> {
        Ok(Some(match m {
            "add" => {
                add_content(x, content_nodes(a, 0), false);
                Val::Void
            }
            "addfirst" => {
                add_content(x, content_nodes(a, 0), true);
                Val::Void
            }
            "getchildnodes" => net(NetObj::XNodeList(xml::children(x).into_iter().filter(|c| !matches!(xml::kind(c), NK::Declaration)).collect(), true)),
            "getchildelements" => net(NetObj::XNodeList(filter_elements(xml::element_children(x), a), true)),
            "getdescendantelements" => net(NetObj::XNodeList(filter_elements(descendant_elements(x), a), true)),
            "getdescendantnodes" => {
                let mut d = vec![];
                xml::descendants(x, &mut d);
                net(NetObj::XNodeList(d, true))
            }
            "removenodes" => {
                xml::remove_all_children(x);
                Val::Void
            }
            "replacenodes" => {
                let nodes = content_nodes(a, 0);
                xml::remove_all_children(x);
                add_content(x, nodes, false);
                Val::Void
            }
            "haschildnodes" => Val::Bool(!xml::children(x).is_empty()),
            "firstchild" => xml::children(x).first().map(|c| xnode(c.clone(), true)).unwrap_or(Val::Void),
            "lastchild" => xml::children(x).last().map(|c| xnode(c.clone(), true)).unwrap_or(Val::Void),
            _ => {
                let _ = f;
                return Ok(None);
            }
        }))
    }

    fn al_document_call(&mut self, f: &mut Frame, x: &Node, m: &str, a: &Args) -> R<Out> {
        if let Some(v) = self.al_container_call(f, x, m, a)? {
            return ok(v);
        }
        ok(match m {
            "getroot" => {
                let r = xml::root_element(x);
                let found = r.is_some();
                if let Some(r) = r {
                    self.net_out(f, a, 0, xnode(r, true))?;
                }
                Val::Bool(found)
            }
            "getdeclaration" => {
                let d = xml::declaration_of(x);
                let found = d.is_some();
                if let Some(d) = d {
                    self.net_out(f, a, 0, xnode(d, true))?;
                }
                Val::Bool(found)
            }
            "setdeclaration" => {
                if let Some(old) = xml::declaration_of(x) {
                    xml::detach(&old);
                }
                if let Some(d) = node_arg(a.raw(0)) {
                    let d = if xml::parent(&d).is_some() { xml::clone_deep(&d) } else { d };
                    xml::prepend(x, &d);
                }
                Val::Void
            }
            "getdocumenttype" => {
                let d = xml::children(x).into_iter().find(|c| xml::kind(c) == NK::DocType);
                let found = d.is_some();
                if let Some(d) = d {
                    self.net_out(f, a, 0, xnode(d, true))?;
                }
                Val::Bool(found)
            }
            "nametable" => net(NetObj::XNameTable(true)),
            "name" => Val::Text(String::new()),
            _ => return Ok(Out::Unknown),
        })
    }

    fn al_element_call(&mut self, f: &mut Frame, x: &Node, m: &str, a: &Args) -> R<Out> {
        if let Some(v) = self.al_container_call(f, x, m, a)? {
            return ok(v);
        }
        ok(match m {
            "name" => Val::Text(xml::qname(x)),
            "localname" => Val::Text(x.borrow().local.clone()),
            "namespaceuri" => Val::Text(x.borrow().ns.clone()),
            "namespaceprefix" | "prefix" => Val::Text(x.borrow().prefix.clone()),
            "attributes" => net(NetObj::XAttrs(x.clone(), true)),
            "hasattributes" => Val::Bool(!xml::attributes(x).is_empty()),
            "haselements" => Val::Bool(!xml::element_children(x).is_empty()),
            "isempty" => Val::Bool(xml::children(x).is_empty()),
            "setattribute" => {
                let (local, ns, v) = if a.len() >= 3 { (a.text(0), a.text(1), a.text(2)) } else { (a.text(0), String::new(), a.text(1)) };
                xml::set_attr(x, "", &local, &ns, &v);
                Val::Void
            }
            "removeattribute" => {
                match a.raw(0) {
                    Some(Val::Net(_)) => {
                        if let Some(at) = node_arg(a.raw(0)) {
                            xml::detach(&at);
                        }
                    }
                    _ if a.len() == 2 => {
                        xml::remove_attr(x, &a.text(0), Some(&a.text(1)));
                    }
                    _ => {
                        xml::remove_attr(x, &a.text(0), None);
                    }
                }
                Val::Void
            }
            "removeallattributes" => {
                for at in xml::attributes(x) {
                    xml::detach(&at);
                }
                Val::Void
            }
            "getnamespaceofprefix" => {
                let r = xml::lookup_ns(x, &a.text(0));
                let found = r.is_some();
                self.net_out(f, a, 1, Val::Text(r.unwrap_or_default()))?;
                Val::Bool(found)
            }
            "getprefixofnamespace" => {
                let r = xml::lookup_prefix(x, &a.text(0));
                let found = r.is_some();
                self.net_out(f, a, 1, Val::Text(r.unwrap_or_default()))?;
                Val::Bool(found)
            }
            _ => return Ok(Out::Unknown),
        })
    }

    // ------------------------------------------------------------ DotNet System.Xml

    pub fn net_xml_call(&mut self, f: &mut Frame, n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
        // enums and statics
        match ty {
            "System.Xml.XmlNodeType" => {
                return ok(match super::text::enum_names(ty).and_then(|v| v.iter().find(|(x, _)| x.eq_ignore_ascii_case(m)).map(|(_, v)| *v)) {
                    Some(v) => enum_val(ty, v),
                    None => return Ok(Out::Unknown),
                });
            }
            "System.Xml.DtdProcessing" => {
                return ok(match m {
                    "prohibit" => enum_val(ty, 0),
                    "ignore" => enum_val(ty, 1),
                    "parse" => enum_val(ty, 2),
                    _ => return Ok(Out::Unknown),
                });
            }
            "System.Xml.XmlConvert" => return xml_convert(m, a),
            "System.Xml.XmlReaderSettings" | "System.Xml.XmlUrlResolver" | "System.Xml.XmlResolver" => {
                return match m {
                    "dtdprocessing" | "xmlresolver" | "credentials" | "ignorewhitespace" | "ignorecomments" | "maxcharactersfromentities" | "prohibitdtd" => void(),
                    _ => Ok(Out::Unknown),
                };
            }
            "System.Xml.Formatting" => {
                return ok(match m {
                    "none" => enum_val(ty, 0),
                    "indented" => enum_val(ty, 1),
                    _ => return Ok(Out::Unknown),
                });
            }
            "System.Xml.XmlWriter" | "System.Xml.XmlTextWriter" if m == "create" => {
                let target = a.raw(0).cloned().unwrap_or(Val::Void);
                let args = Args { exprs: &[], v: vec![target] };
                return ok(super::xmlwriter::construct(&args));
            }
            "System.Xml.XmlWriterSettings" => {
                return match m {
                    "indent" | "omitxmldeclaration" | "encoding" | "newlinechars" | "indentchars" | "conformancelevel" => void(),
                    _ => Ok(Out::Unknown),
                };
            }
            "System.Xml.XmlReader" | "System.Xml.XmlTextReader" if m == "create" => {
                let t = match a.raw(0) {
                    // Create(string): a file (server files) or URI
                    Some(Val::Text(path)) => match self.files.read(path) {
                        Some(b) => super::text::decode(65001, &b),
                        // the type the call was made on (XmlTextReader.Create names XmlTextReader)
                        None => return net_err(if ty == "System.Xml.XmlTextReader" { "XmlTextReader" } else { "XmlReader" }, "Create", format!("Could not find file '{}'.", crate::files::full_path(path))),
                    },
                    // Create(OutStream): what was written to it so far
                    Some(Val::OutStream(t)) => match &**t {
                        crate::streams::OutTarget::Buffer(b) => super::text::decode(65001, &b.borrow()),
                        crate::streams::OutTarget::Field(r, fi) => match &r.borrow().row[*fi] {
                            bc_store::Value::Blob(b) => super::text::decode(65001, b),
                            _ => String::new(),
                        },
                    },
                    other => source_text(other),
                };
                return ok(super::xmlreader::new(t));
            }
            _ => {}
        }
        let o = n.borrow();
        match &*o {
            NetObj::XNode(x, _) => {
                let x = x.clone();
                drop(o);
                self.net_node_call(f, &x, m, a)
            }
            NetObj::XNodeList(l, _) => {
                let l = l.clone();
                drop(o);
                ok(match m {
                    "count" => Val::Int(l.len() as i32),
                    "item" | "itemof" | "get_itemof" | "get" => match l.get(a.int(0) as usize) {
                        Some(x) => xnode(x.clone(), false),
                        None => net(NetObj::Null("System.Xml.XmlNode")),
                    },
                    "getenumerator" => net(NetObj::Enumerator(l.iter().map(|x| xnode(x.clone(), false)).collect(), -1)),
                    _ => return Ok(Out::Unknown),
                })
            }
            NetObj::XAttrs(e, _) => {
                let e = e.clone();
                drop(o);
                let attrs = xml::attributes(&e);
                let null = || net(NetObj::Null("System.Xml.XmlAttribute"));
                ok(match m {
                    "count" => Val::Int(attrs.len() as i32),
                    "item" | "itemof" | "get_itemof" => match a.raw(0) {
                        Some(Val::Int(_) | Val::BigInt(_)) => attrs.get(a.int(0) as usize).map(|x| xnode(x.clone(), false)).unwrap_or_else(null),
                        _ => xml::get_attr(&e, &a.text(0), if a.len() > 1 { Some(a.text(1)) } else { None }.as_deref()).map(|x| xnode(x, false)).unwrap_or_else(null),
                    },
                    "getnameditem" => xml::get_attr(&e, &a.text(0), if a.len() > 1 { Some(a.text(1)) } else { None }.as_deref()).map(|x| xnode(x, false)).unwrap_or_else(null),
                    "setnameditem" | "append" => match node_arg(a.raw(0)) {
                        Some(at) => {
                            xml::add_attr_node(&e, &at);
                            xnode(at, false)
                        }
                        None => return null_ref("XmlAttributeCollection", m),
                    },
                    "removenameditem" => {
                        let at = xml::get_attr(&e, &a.text(0), if a.len() > 1 { Some(a.text(1)) } else { None }.as_deref());
                        if let Some(at) = &at {
                            xml::detach(at);
                        }
                        at.map(|x| xnode(x, false)).unwrap_or_else(null)
                    }
                    "remove" => {
                        if let Some(at) = node_arg(a.raw(0)) {
                            xml::detach(&at);
                        }
                        Val::Void
                    }
                    "removeall" => {
                        for at in attrs {
                            xml::detach(&at);
                        }
                        Val::Void
                    }
                    "getenumerator" => net(NetObj::Enumerator(attrs.into_iter().map(|x| xnode(x, false)).collect(), -1)),
                    _ => return Ok(Out::Unknown),
                })
            }
            NetObj::XNsMgr(mgr, _) => {
                let mgr = mgr.clone();
                drop(o);
                ok(match m {
                    "addnamespace" => {
                        mgr.borrow_mut().push((a.text(0), a.text(1)));
                        Val::Void
                    }
                    "lookupnamespace" => mgr.borrow().iter().rev().find(|(p, _)| *p == a.text(0)).map(|(_, u)| Val::Text(u.clone())).unwrap_or(Val::Void),
                    "lookupprefix" => mgr.borrow().iter().rev().find(|(_, u)| *u == a.text(0)).map(|(p, _)| Val::Text(p.clone())).unwrap_or(Val::Void),
                    "hasnamespace" => Val::Bool(mgr.borrow().iter().any(|(p, _)| *p == a.text(0))),
                    "defaultnamespace" => Val::Text(mgr.borrow().iter().rev().find(|(p, _)| p.is_empty()).map(|(_, u)| u.clone()).unwrap_or_default()),
                    "removenamespace" => {
                        let (p, u) = (a.text(0), a.text(1));
                        mgr.borrow_mut().retain(|(x, y)| !(x == &p && y == &u));
                        Val::Void
                    }
                    "pushscope" | "popscope" => Val::Bool(true),
                    "nametable" => net(NetObj::XNameTable(false)),
                    _ => return Ok(Out::Unknown),
                })
            }
            NetObj::XmlWriter(_) => {
                drop(o);
                super::xmlwriter::call(n, m, a)
            }
            NetObj::XReader(_) => {
                drop(o);
                super::xmlreader::call(n, m, a)
            }
            NetObj::XNameTable(_) => match m {
                "add" | "get" => ok(Val::Text(a.text(0))),
                _ => Ok(Out::Unknown),
            },
            NetObj::Reader(_, _, _) => {
                drop(o);
                super::text::call(self, f, n, "System.IO.StringReader", m, a)
            }
            NetObj::Null(_) => Ok(Out::Unknown),
            _ => Ok(Out::Unknown),
        }
    }

    fn net_node_call(&mut self, f: &mut Frame, x: &Node, m: &str, a: &Args) -> R<Out> {
        let _ = f;
        let k = xml::kind(x);
        let null_node = || net(NetObj::Null("System.Xml.XmlNode"));
        let opt = |n: Option<Node>| n.map(|n| xnode(n, false)).unwrap_or_else(null_node);
        let doc_of = |x: &Node| if xml::kind(x) == NK::Document { Some(x.clone()) } else { xml::document_of(x) };
        let set = a.len() == 1;
        let r = match m {
            // ---- document-level
            "loadxml" | "load" => {
                // Load(string) reads a (server) file; Load(stream / reader) its content
                let text = match (m, a.raw(0)) {
                    ("loadxml", _) => a.text(0),
                    ("load", Some(Val::Text(path))) => match self.files.read(path) {
                        Some(b) => super::text::decode(65001, &b),
                        None => return net_err("XmlDocument", "Load", format!("Could not find file '{}'.", crate::files::full_path(path))),
                    },
                    _ => source_text(a.raw(0)),
                };
                let ws = x.borrow().prefix == "preserve";
                match xml::parse(&text, &xml::ParseOptions { preserve_whitespace: ws }) {
                    Ok(d) => {
                        xml::remove_all_children(x);
                        for c in xml::children(&d) {
                            xml::append(x, &c);
                        }
                        Val::Void
                    }
                    Err(e) => return super::net_err_code("XmlDocument", if m == "loadxml" { "LoadXml" } else { "Load" }, super::XML_EXCEPTION_CODE, e),
                }
            }
            "save" => {
                let indent = x.borrow().prefix != "preserve";
                let text = xml::write(x, &xml::WriteOpts { indent, decl: true });
                match a.raw(0) {
                    // Save(string): a (server) file
                    Some(Val::Text(path)) => {
                        let path = path.clone();
                        self.files.write(&path, text.into_bytes());
                    }
                    _ => write_out(self, f, a, 0, &text)?,
                }
                Val::Void
            }
            "writeto" | "writecontentto" => {
                let text = if m == "writeto" { xml::outer_xml(x) } else { xml::inner_xml(x) };
                write_out(self, f, a, 0, &text)?;
                Val::Void
            }
            "preservewhitespace" if set => {
                x.borrow_mut().prefix = if a.bool(0) { "preserve".into() } else { String::new() };
                Val::Void
            }
            "preservewhitespace" => Val::Bool(x.borrow().prefix == "preserve"),
            "documentelement" => opt(doc_of(x).and_then(|d| xml::root_element(&d))),
            "documenttype" => opt(doc_of(x).and_then(|d| xml::children(&d).into_iter().find(|c| xml::kind(c) == NK::DocType))),
            "ownerdocument" => {
                if k == NK::Document {
                    null_node()
                } else {
                    opt(xml::document_of(x))
                }
            }
            "nametable" => net(NetObj::XNameTable(false)),
            "createelement" => {
                let (p, l, ns) = match a.len() {
                    3 => (a.text(0), a.text(1), a.text(2)),
                    2 => {
                        let q = a.text(0);
                        match q.split_once(':') {
                            Some((p, l)) => (p.to_string(), l.to_string(), a.text(1)),
                            None => (String::new(), q, a.text(1)),
                        }
                    }
                    _ => {
                        let q = a.text(0);
                        match q.split_once(':') {
                            Some((p, l)) => (p.to_string(), l.to_string(), String::new()),
                            None => (String::new(), q, String::new()),
                        }
                    }
                };
                let e = xml::element(&p, &l, &ns);
                if let Some(d) = doc_of(x) {
                    e.borrow_mut().owner = Rc::downgrade(&d);
                }
                xnode(e, false)
            }
            "createattribute" => {
                let (p, l, ns) = match a.len() {
                    3 => (a.text(0), a.text(1), a.text(2)),
                    2 => {
                        let q = a.text(0);
                        match q.split_once(':') {
                            Some((p, l)) => (p.to_string(), l.to_string(), a.text(1)),
                            None => (String::new(), q, a.text(1)),
                        }
                    }
                    _ => {
                        let q = a.text(0);
                        match q.split_once(':') {
                            Some((p, l)) => (p.to_string(), l.to_string(), if p == "xmlns" { xml::XMLNS.into() } else { String::new() }),
                            None => (String::new(), q.clone(), if q == "xmlns" { xml::XMLNS.into() } else { String::new() }),
                        }
                    }
                };
                let at = xml::attribute(&p, &l, &ns, "");
                if let Some(d) = doc_of(x) {
                    at.borrow_mut().owner = Rc::downgrade(&d);
                }
                xnode(at, false)
            }
            "createnode" => {
                let t = match a.raw(0) {
                    Some(Val::Net(t)) => super::to_text(t).to_ascii_lowercase(),
                    Some(v) => v.to_text().to_ascii_lowercase(),
                    None => "element".into(),
                };
                let q = a.text(1);
                let ns = a.text(2);
                let (p, l) = match q.split_once(':') {
                    Some((p, l)) => (p.to_string(), l.to_string()),
                    None => (String::new(), q.clone()),
                };
                let nd = match t.as_str() {
                    "attribute" | "2" => xml::attribute(&p, &l, &ns, ""),
                    "text" | "3" => xml::leaf(NK::Text, ""),
                    "comment" | "8" => xml::leaf(NK::Comment, ""),
                    _ => xml::element(&p, &l, &ns),
                };
                if let Some(d) = doc_of(x) {
                    nd.borrow_mut().owner = Rc::downgrade(&d);
                }
                xnode(nd, false)
            }
            "createtextnode" => xnode(xml::leaf(NK::Text, &a.text(0)), false),
            "createcdatasection" => xnode(xml::leaf(NK::CData, &a.text(0)), false),
            "createcomment" => xnode(xml::leaf(NK::Comment, &a.text(0)), false),
            "createwhitespace" | "createsignificantwhitespace" => xnode(xml::leaf(NK::Whitespace, &a.text(0)), false),
            "createxmldeclaration" => xnode(xml::declaration(&a.text(0), &a.text(1), &a.text(2)), false),
            "createprocessinginstruction" => xnode(xml::pi(&a.text(0), &a.text(1)), false),
            "createdocumenttype" => xnode(xml::doctype(&a.text(0), &a.text(1), &a.text(2), &a.text(3)), false),
            "importnode" => match node_arg(a.raw(0)) {
                Some(src) => {
                    let c = if a.bool(1) { xml::clone_deep(&src) } else { xml::element(&src.borrow().prefix, &src.borrow().local, &src.borrow().ns) };
                    if let Some(d) = doc_of(x) {
                        c.borrow_mut().owner = Rc::downgrade(&d);
                    }
                    xnode(c, false)
                }
                None => return null_ref("XmlDocument", "ImportNode"),
            },
            "getelementsbytagname" => {
                let name = a.text(0);
                let ns = if a.len() > 1 { Some(a.text(1)) } else { None };
                let mut d = vec![];
                xml::descendants(x, &mut d);
                let l: Vec<Node> = d
                    .into_iter()
                    .filter(|e| {
                        xml::kind(e) == NK::Element
                            && match &ns {
                                Some(ns) => (e.borrow().local == name || name == "*") && (e.borrow().ns == *ns || ns == "*"),
                                None => name == "*" || xml::qname(e) == name,
                            }
                    })
                    .collect();
                net(NetObj::XNodeList(l, false))
            }
            // ---- tree
            "appendchild" | "prependchild" => match node_arg(a.raw(0)) {
                Some(c) => {
                    if xml::kind(&c) == NK::Attribute {
                        return net_err("XmlNode", "AppendChild", "The specified node cannot be inserted as the valid child of this node, because the specified node is the wrong type.");
                    }
                    if m == "appendchild" {
                        xml::append(x, &c);
                    } else {
                        xml::prepend(x, &c);
                    }
                    xnode(c, false)
                }
                None => return null_ref("XmlNode", "AppendChild"),
            },
            "insertbefore" | "insertafter" => match node_arg(a.raw(0)) {
                Some(c) => {
                    let r = node_arg(a.raw(1));
                    match (m, r) {
                        ("insertbefore", r) => xml::insert_before(x, &c, r.as_ref()),
                        (_, Some(r)) => {
                            xml::insert_sibling(&r, &c, true);
                        }
                        (_, None) => xml::prepend(x, &c),
                    }
                    xnode(c, false)
                }
                None => return null_ref("XmlNode", m),
            },
            "removechild" => match node_arg(a.raw(0)) {
                Some(c) => {
                    xml::detach(&c);
                    xnode(c, false)
                }
                None => return null_ref("XmlNode", "RemoveChild"),
            },
            "replacechild" => match (node_arg(a.raw(0)), node_arg(a.raw(1))) {
                (Some(new), Some(old)) => {
                    xml::replace(&old, &new);
                    xnode(old, false)
                }
                _ => return null_ref("XmlNode", "ReplaceChild"),
            },
            "removeall" => {
                xml::remove_all_children(x);
                for at in xml::attributes(x) {
                    xml::detach(&at);
                }
                Val::Void
            }
            "clonenode" | "clone" => xnode(if m == "clone" || a.bool(0) { xml::clone_deep(x) } else { xml::element(&x.borrow().prefix, &x.borrow().local, &x.borrow().ns) }, false),
            "childnodes" => net(NetObj::XNodeList(xml::children(x), false)),
            "firstchild" => opt(xml::children(x).first().cloned()),
            "lastchild" => opt(xml::children(x).last().cloned()),
            "haschildnodes" => Val::Bool(!xml::children(x).is_empty()),
            "parentnode" => opt(xml::parent(x).filter(|_| k != NK::Attribute)),
            "ownerelement" => opt(xml::parent(x).filter(|_| k == NK::Attribute)),
            "nextsibling" | "previoussibling" => {
                let sib = xml::parent(x).and_then(|p| {
                    let ch = xml::children(&p);
                    let i = ch.iter().position(|c| Rc::ptr_eq(c, x))?;
                    if m == "nextsibling" {
                        ch.get(i + 1).cloned()
                    } else if i > 0 {
                        ch.get(i - 1).cloned()
                    } else {
                        None
                    }
                });
                opt(sib)
            }
            "selectsinglenode" | "selectnodes" => {
                let ns = nsmgr_arg(a.raw(1)).unwrap_or_default();
                let nodes = match xpath::select(x, &a.text(0), &ns) {
                    Ok(v) => v,
                    Err(e) => return net_err("XmlNode", if m == "selectnodes" { "SelectNodes" } else { "SelectSingleNode" }, e),
                };
                if m == "selectnodes" {
                    net(NetObj::XNodeList(nodes, false))
                } else {
                    opt(nodes.into_iter().next())
                }
            }
            // ---- names and values
            "name" => Val::Text(xml::qname(x)),
            "localname" => Val::Text(if matches!(k, NK::Element | NK::Attribute) { x.borrow().local.clone() } else { xml::qname(x) }),
            "namespaceuri" => Val::Text(x.borrow().ns.clone()),
            "prefix" if set => {
                x.borrow_mut().prefix = a.text(0);
                Val::Void
            }
            "prefix" => Val::Text(x.borrow().prefix.clone()),
            "nodetype" => enum_val("System.Xml.XmlNodeType", xml::node_type_num(x)),
            "value" if set => {
                match k {
                    NK::Element | NK::Document => return net_err("XmlNode", "Value", "Cannot set a value on node type 'Element'."),
                    _ => x.borrow_mut().value = a.text(0),
                }
                Val::Void
            }
            "value" => match k {
                NK::Element | NK::Document | NK::DocType => Val::Void,
                NK::Declaration => {
                    let (v, e, s) = xml::decl_parts(x);
                    let mut t = format!("version=\"{v}\"");
                    if !e.is_empty() {
                        t.push_str(&format!(" encoding=\"{e}\""));
                    }
                    if !s.is_empty() {
                        t.push_str(&format!(" standalone=\"{s}\""));
                    }
                    Val::Text(t)
                }
                _ => Val::Text(x.borrow().value.clone()),
            },
            "innertext" if set => {
                xml::set_inner_text(x, &a.text(0));
                Val::Void
            }
            "innertext" => Val::Text(xml::inner_text(x)),
            "innerxml" if set => {
                match xml::parse_fragment(&a.text(0), Some(x)) {
                    Ok(nodes) => {
                        xml::remove_all_children(x);
                        for c in nodes {
                            xml::append(x, &c);
                        }
                    }
                    Err(e) => return net_err("XmlNode", "InnerXml", e),
                }
                Val::Void
            }
            "innerxml" => Val::Text(xml::inner_xml(x)),
            "outerxml" => Val::Text(xml::outer_xml(x)),
            "attributes" => {
                if k == NK::Element {
                    net(NetObj::XAttrs(x.clone(), false))
                } else {
                    net(NetObj::Null("System.Xml.XmlAttributeCollection"))
                }
            }
            "hasattributes" => Val::Bool(!xml::attributes(x).is_empty()),
            "getattribute" => Val::Text(xml::get_attr(x, &a.text(0), if a.len() > 1 { Some(a.text(1)) } else { None }.as_deref()).map(|at| at.borrow().value.clone()).unwrap_or_default()),
            "getattributenode" => opt(xml::get_attr(x, &a.text(0), if a.len() > 1 { Some(a.text(1)) } else { None }.as_deref())),
            "hasattribute" => Val::Bool(xml::get_attr(x, &a.text(0), if a.len() > 1 { Some(a.text(1)) } else { None }.as_deref()).is_some()),
            "setattribute" => {
                let (q, ns, v) = if a.len() == 3 { (a.text(0), a.text(1), a.text(2)) } else { (a.text(0), String::new(), a.text(1)) };
                let (p, l) = match q.split_once(':') {
                    Some((p, l)) => (p.to_string(), l.to_string()),
                    None => (String::new(), q.clone()),
                };
                let ns = if p == "xmlns" || q == "xmlns" { xml::XMLNS.to_string() } else { ns };
                xml::set_attr(x, &p, &l, &ns, &v);
                Val::Void
            }
            "setattributenode" => match node_arg(a.raw(0)) {
                Some(at) => {
                    xml::add_attr_node(x, &at);
                    xnode(at, false)
                }
                None => return null_ref("XmlElement", "SetAttributeNode"),
            },
            "removeattribute" => {
                xml::remove_attr(x, &a.text(0), if a.len() > 1 { Some(a.text(1)) } else { None }.as_deref());
                Val::Void
            }
            "removeallattributes" => {
                for at in xml::attributes(x) {
                    xml::detach(&at);
                }
                Val::Void
            }
            "isempty" => Val::Bool(xml::children(x).is_empty()),
            "item" | "get_item" => opt(xml::element_children(x).into_iter().find(|c| xml::qname(c) == a.text(0))),
            "getnamespaceofprefix" => Val::Text(xml::lookup_ns(x, &a.text(0)).unwrap_or_default()),
            "getprefixofnamespace" => Val::Text(xml::lookup_prefix(x, &a.text(0)).unwrap_or_default()),
            // declaration / doctype / PI
            "version" if k == NK::Declaration => Val::Text(xml::decl_parts(x).0),
            "encoding" | "standalone" if k == NK::Declaration => {
                let (v, e, s) = xml::decl_parts(x);
                if set {
                    let (e2, s2) = if m == "encoding" { (a.text(0), s) } else { (e, a.text(0)) };
                    x.borrow_mut().value = format!("{v}|{e2}|{s2}");
                    Val::Void
                } else {
                    Val::Text(if m == "encoding" { e } else { s })
                }
            }
            "publicid" | "systemid" | "internalsubset" if k == NK::DocType => {
                let parts: Vec<String> = x.borrow().value.splitn(3, '|').map(String::from).collect();
                let i = match m {
                    "publicid" => 0,
                    "systemid" => 1,
                    _ => 2,
                };
                Val::Text(parts.get(i).cloned().unwrap_or_default())
            }
            "target" if k == NK::PI => Val::Text(x.borrow().local.clone()),
            "data" if k == NK::PI || k == NK::Comment || k == NK::Text || k == NK::CData => Val::Text(x.borrow().value.clone()),
            "specified" => Val::Bool(true),
            "getenumerator" => net(NetObj::Enumerator(xml::children(x).into_iter().map(|c| xnode(c, false)).collect(), -1)),
            "normalize" => Val::Void,
            "validate" | "schemas" => return Ok(Out::Unknown),
            _ => return Ok(Out::Unknown),
        };
        ok(r)
    }
}

fn xml_convert(m: &str, a: &Args) -> R<Out> {
    ok(match m {
        "decodename" => {
            let s = a.text(0);
            let mut out = String::new();
            let mut rest = s.as_str();
            while let Some(i) = rest.find("_x") {
                out.push_str(&rest[..i]);
                let t = &rest[i..];
                if t.len() >= 7 && t.as_bytes()[6] == b'_' {
                    if let Some(c) = u32::from_str_radix(&t[2..6], 16).ok().and_then(char::from_u32) {
                        out.push(c);
                        rest = &t[7..];
                        continue;
                    }
                }
                out.push_str("_x");
                rest = &t[2..];
            }
            out.push_str(rest);
            Val::Text(out)
        }
        "encodename" | "encodelocalname" => {
            let s = a.text(0);
            let mut out = String::new();
            for (i, c) in s.chars().enumerate() {
                let ok_ = c.is_alphabetic() || c == '_' || (i > 0 && (c.is_alphanumeric() || c == '-' || c == '.')) || (c == ':' && m == "encodename");
                if ok_ {
                    out.push(c);
                } else {
                    out.push_str(&format!("_x{:04X}_", c as u32));
                }
            }
            Val::Text(out)
        }
        "tostring" => Val::Text(match a.get(0) {
            Val::Bool(b) => b.to_string(),
            Val::Dec(d) => d.normalize().to_string(),
            Val::DateTime(ms) => crate::builtins::format_val(&Val::DateTime(ms), 9),
            Val::Net(n) => match &*n.borrow() {
                NetObj::DateTime(d) => d.format("o", super::culture::invariant()),
                NetObj::DateTimeOffset(u, off) => datetime::format_offset(*u, *off, "o", super::culture::invariant()),
                _ => super::to_text(&n),
            },
            v => v.to_text(),
        }),
        "toboolean" => Val::Bool(matches!(a.text(0).trim(), "true" | "1")),
        "toint32" => Val::Int(a.text(0).trim().parse().unwrap_or(0)),
        "toint64" => Val::BigInt(a.text(0).trim().parse().unwrap_or(0)),
        "todecimal" | "todouble" => Val::Dec(a.text(0).trim().parse().unwrap_or_default()),
        "todatetime" => match datetime::parse_iso(a.text(0).trim()) {
            Some(p) => net(NetObj::DateTime(p.datetime())),
            None => return net_err("XmlConvert", "ToDateTime", "The string was not recognized as a valid DateTime."),
        },
        "todatetimeoffset" => match datetime::parse_iso(a.text(0).trim()) {
            Some(p) => net(NetObj::DateTimeOffset(p.utc_ticks(), p.offset.unwrap_or(0))),
            None => return net_err("XmlConvert", "ToDateTimeOffset", "The string was not recognized as a valid DateTime."),
        },
        _ => return Ok(Out::Unknown),
    })
}

/// XmlConvert.EncodeLocalName: characters that cannot appear in an XML name become `_xHHHH_`.
pub fn encode_name(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        let ok_ = c.is_alphabetic() || c == '_' || (i > 0 && (c.is_alphanumeric() || c == '-' || c == '.'));
        if ok_ {
            out.push(c);
        } else {
            out.push_str(&format!("_x{:04X}_", c as u32));
        }
    }
    out
}
