//! System.Xml.XmlReader / XmlTextReader: a forward-only pull reader. The text is parsed on the
//! first Read() into the node sequence .NET reports (XmlDeclaration, Element, Text, CDATA,
//! Comment, ProcessingInstruction, Whitespace, EndElement; attributes are reached with
//! MoveToFirstAttribute/MoveToNextAttribute), with .NET depths (root element = 0).

use super::xml::{self, Node, NK};
use super::*;

#[derive(Clone)]
pub struct Attr {
    pub name: String,
    pub local: String,
    pub prefix: String,
    pub ns: String,
    pub value: String,
}

#[derive(Clone)]
pub struct Ev {
    /// System.Xml.XmlNodeType value.
    pub kind: i64,
    pub name: String,
    pub local: String,
    pub prefix: String,
    pub ns: String,
    pub value: String,
    pub depth: i32,
    pub empty: bool,
    pub attrs: Vec<Attr>,
}

pub struct XmlReaderState {
    /// Source text (also what XmlDocument.Load(reader) reads).
    pub text: Rc<str>,
    events: Option<Vec<Ev>>,
    idx: isize,
    attr: Option<usize>,
    closed: bool,
}

pub fn new(text: String) -> Val {
    net(NetObj::XReader(Box::new(XmlReaderState { text: text.into(), events: None, idx: -1, attr: None, closed: false })))
}

fn ev(kind: i64, n: &Node, depth: i32) -> Ev {
    let b = n.borrow();
    Ev { kind, name: xml::qname(n), local: b.local.clone(), prefix: b.prefix.clone(), ns: b.ns.clone(), value: String::new(), depth, empty: false, attrs: vec![] }
}

fn flatten(n: &Node, depth: i32, out: &mut Vec<Ev>) {
    let k = xml::kind(n);
    match k {
        NK::Document => {
            for c in xml::children(n) {
                flatten(&c, depth, out);
            }
        }
        NK::Element => {
            let mut e = ev(1, n, depth);
            e.attrs = xml::attributes(n)
                .iter()
                .map(|a| {
                    let b = a.borrow();
                    Attr { name: xml::qname(a), local: b.local.clone(), prefix: b.prefix.clone(), ns: b.ns.clone(), value: b.value.clone() }
                })
                .collect();
            let ch = xml::children(n);
            e.empty = ch.is_empty();
            out.push(e);
            if !ch.is_empty() {
                for c in &ch {
                    flatten(c, depth + 1, out);
                }
                let mut end = ev(15, n, depth);
                end.attrs.clear();
                out.push(end);
            }
        }
        NK::Declaration => {
            let (v, e, s) = xml::decl_parts(n);
            let mut d = ev(17, n, depth);
            d.name = "xml".into();
            d.local = "xml".into();
            d.value = format!("version=\"{v}\"{}{}", if e.is_empty() { String::new() } else { format!(" encoding=\"{e}\"") }, if s.is_empty() { String::new() } else { format!(" standalone=\"{s}\"") });
            out.push(d);
        }
        NK::DocType => {
            let mut d = ev(10, n, depth);
            d.value = n.borrow().value.splitn(3, '|').nth(2).unwrap_or("").to_string();
            out.push(d);
        }
        NK::PI => {
            let mut p = ev(7, n, depth);
            p.value = n.borrow().value.clone();
            out.push(p);
        }
        NK::Text | NK::CData | NK::Comment | NK::Whitespace => {
            let kind = match k {
                NK::Text => 3,
                NK::CData => 4,
                NK::Comment => 8,
                _ => 13,
            };
            let mut t = ev(kind, n, depth);
            t.name = String::new();
            t.local = String::new();
            t.value = n.borrow().value.clone();
            out.push(t);
        }
        NK::Attribute => {}
    }
}

impl XmlReaderState {
    fn ensure(&mut self) -> Result<(), String> {
        if self.events.is_none() {
            let doc = xml::parse(&self.text, &xml::ParseOptions { preserve_whitespace: true })?;
            let mut evs = vec![];
            flatten(&doc, 0, &mut evs);
            self.events = Some(evs);
        }
        Ok(())
    }
    fn cur(&self) -> Option<&Ev> {
        let e = self.events.as_ref()?;
        if self.idx < 0 {
            return None;
        }
        e.get(self.idx as usize)
    }
    fn eof(&self) -> bool {
        self.events.as_ref().is_some_and(|e| self.idx >= e.len() as isize)
    }
}

pub fn call(n: &NetRef, m: &str, a: &Args) -> R<Out> {
    let mut o = n.borrow_mut();
    let NetObj::XReader(r) = &mut *o else { return Ok(Out::Unknown) };
    let fail = |msg: String| -> R<Out> { net_err("XmlReader", "Read", msg) };
    // node properties (attribute-aware)
    let prop = |r: &XmlReaderState, f: &dyn Fn(&Ev, Option<&Attr>) -> Val| -> Val {
        match r.cur() {
            Some(e) => f(e, r.attr.and_then(|i| e.attrs.get(i))),
            None => f(&Ev { kind: 0, name: String::new(), local: String::new(), prefix: String::new(), ns: String::new(), value: String::new(), depth: 0, empty: false, attrs: vec![] }, None),
        }
    };
    let r: &mut XmlReaderState = &mut **r;
    ok(match m {
        "read" => {
            if r.closed {
                return ok(Val::Bool(false));
            }
            if let Err(e) = r.ensure() {
                drop(o);
                return fail(e);
            }
            r.attr = None;
            let len = r.events.as_ref().map(|e| e.len()).unwrap_or(0) as isize;
            if r.idx < len {
                r.idx += 1;
            }
            Val::Bool(r.idx < len)
        }
        "nodetype" => enum_val("System.Xml.XmlNodeType", prop(r, &|e, a| Val::Int(if a.is_some() { 2 } else { e.kind as i32 })).as_i64()),
        "name" => prop(r, &|e, a| Val::Text(a.map(|a| a.name.clone()).unwrap_or_else(|| e.name.clone()))),
        "localname" => prop(r, &|e, a| Val::Text(a.map(|a| a.local.clone()).unwrap_or_else(|| e.local.clone()))),
        "prefix" => prop(r, &|e, a| Val::Text(a.map(|a| a.prefix.clone()).unwrap_or_else(|| e.prefix.clone()))),
        "namespaceuri" => prop(r, &|e, a| Val::Text(a.map(|a| a.ns.clone()).unwrap_or_else(|| e.ns.clone()))),
        "value" => prop(r, &|e, a| Val::Text(a.map(|a| a.value.clone()).unwrap_or_else(|| e.value.clone()))),
        "hasvalue" => prop(r, &|e, a| Val::Bool(a.is_some() || matches!(e.kind, 3 | 4 | 7 | 8 | 13 | 14 | 17 | 10))),
        "depth" => prop(r, &|e, a| Val::Int(e.depth + a.is_some() as i32)),
        "isemptyelement" => prop(r, &|e, a| Val::Bool(a.is_none() && e.kind == 1 && e.empty)),
        "hasattributes" => prop(r, &|e, _| Val::Bool(!e.attrs.is_empty())),
        "attributecount" => prop(r, &|e, _| Val::Int(e.attrs.len() as i32)),
        "eof" => Val::Bool(r.eof()),
        "readstate" => Val::Int(if r.closed {
            4
        } else if r.idx < 0 {
            0
        } else if r.eof() {
            3
        } else {
            1
        }),
        "movetofirstattribute" | "movetonextattribute" => {
            let n_attrs = r.cur().map(|e| e.attrs.len()).unwrap_or(0);
            let next = match (m, r.attr) {
                ("movetofirstattribute", _) | (_, None) => 0,
                (_, Some(i)) => i + 1,
            };
            if next < n_attrs {
                r.attr = Some(next);
                Val::Bool(true)
            } else {
                Val::Bool(false)
            }
        }
        "movetoattribute" => {
            let found = r.cur().and_then(|e| match a.raw(0) {
                Some(Val::Int(i)) => Some(*i as usize).filter(|i| *i < e.attrs.len()),
                _ => e.attrs.iter().position(|x| x.name == a.text(0)),
            });
            if let Some(i) = found {
                r.attr = Some(i);
            }
            Val::Bool(found.is_some())
        }
        "movetoelement" => {
            let was = r.attr.is_some();
            r.attr = None;
            Val::Bool(was)
        }
        "getattribute" | "item" | "get_item" => r
            .cur()
            .and_then(|e| match a.raw(0) {
                Some(Val::Int(i)) => e.attrs.get(*i as usize),
                _ => e.attrs.iter().find(|x| x.name == a.text(0) || (a.len() > 1 && x.local == a.text(0) && x.ns == a.text(1))),
            })
            .map(|x| Val::Text(x.value.clone()))
            .unwrap_or(Val::Void),
        "movetocontent" | "isstartelement" => {
            if let Err(e) = r.ensure() {
                drop(o);
                return fail(e);
            }
            r.attr = None;
            if r.idx < 0 {
                r.idx = 0;
            }
            let evs = r.events.as_ref().unwrap();
            while (r.idx as usize) < evs.len() && matches!(evs[r.idx as usize].kind, 7 | 8 | 10 | 13 | 14 | 17) {
                r.idx += 1;
            }
            let kind = r.cur().map(|e| e.kind).unwrap_or(0);
            if m == "isstartelement" {
                Val::Bool(kind == 1 && (a.is_empty() || r.cur().is_some_and(|e| e.name == a.text(0))))
            } else {
                enum_val("System.Xml.XmlNodeType", kind)
            }
        }
        "readtofollowing" => {
            if let Err(e) = r.ensure() {
                drop(o);
                return fail(e);
            }
            r.attr = None;
            let name = a.text(0);
            let evs = r.events.as_ref().unwrap();
            let mut i = r.idx + 1;
            while (i as usize) < evs.len() && !(evs[i as usize].kind == 1 && evs[i as usize].name == name) {
                i += 1;
            }
            r.idx = i;
            Val::Bool((i as usize) < evs.len())
        }
        "readelementcontentasstring" | "readelementstring" | "readstring" | "readinnerxml" => {
            // text content of the current element; positions after its end
            let Some(start) = r.cur().cloned() else { return ok(Val::Text(String::new())) };
            r.attr = None;
            let evs = r.events.as_ref().unwrap();
            let mut text = String::new();
            let mut i = r.idx as usize + 1;
            if start.kind == 1 && !start.empty {
                while i < evs.len() && !(evs[i].kind == 15 && evs[i].depth == start.depth) {
                    if matches!(evs[i].kind, 3 | 4 | 13 | 14) {
                        text.push_str(&evs[i].value);
                    }
                    i += 1;
                }
                i += 1;
            } else if matches!(start.kind, 3 | 4) {
                text = start.value.clone();
            }
            r.idx = if m == "readstring" { (i as isize - 1).max(r.idx) } else { i as isize };
            Val::Text(text)
        }
        "skip" => {
            if let Some(start) = r.cur().cloned() {
                let evs = r.events.as_ref().unwrap();
                let mut i = r.idx as usize + 1;
                if start.kind == 1 && !start.empty {
                    while i < evs.len() && !(evs[i].kind == 15 && evs[i].depth == start.depth) {
                        i += 1;
                    }
                    i += 1;
                }
                r.idx = i as isize;
                r.attr = None;
            }
            Val::Void
        }
        "close" | "dispose" => {
            r.closed = true;
            Val::Void
        }
        "settings" | "xmlresolver" | "dtdprocessing" | "whitespacehandling" | "namespaces" | "normalization" => Val::Void,
        _ => return Ok(Out::Unknown),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_sequence_and_depths() {
        let mut r = XmlReaderState { text: "<?xml version=\"1.0\"?><a x=\"1\"><b>t</b><c/></a>".into(), events: None, idx: -1, attr: None, closed: false };
        r.ensure().unwrap();
        let kinds: Vec<(i64, i32, String)> = r.events.as_ref().unwrap().iter().map(|e| (e.kind, e.depth, e.name.clone())).collect();
        assert_eq!(
            kinds,
            vec![(17, 0, "xml".into()), (1, 0, "a".into()), (1, 1, "b".into()), (3, 2, String::new()), (15, 1, "b".into()), (1, 1, "c".into()), (15, 0, "a".into())]
        );
        assert_eq!(r.events.as_ref().unwrap()[1].attrs[0].value, "1");
    }
}
