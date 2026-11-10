//! System.Xml.XmlTextWriter / XmlWriter: a forward-only writer that appends to its target
//! (StringWriter over a StringBuilder, MemoryStream, AL OutStream), formatted like .NET's
//! XmlTextWriter without indentation (`<a />` for empty elements, namespace declarations written
//! where a prefix is first used).

use super::xml::{escape_attr, escape_text};
use super::*;

pub struct XmlWriterState {
    target: Val,
    /// Open elements: qualified name and the namespace bindings they declared.
    stack: Vec<(String, Vec<(String, String)>)>,
    start_open: bool,
    root_written: bool,
    encoding: Option<String>,
}

/// The server file an XmlWriter writes to (`XmlWriter.Create(FileName)`), if it is one.
pub fn file_target(v: &Val) -> Option<String> {
    match v {
        Val::Net(n) => match &*n.borrow() {
            NetObj::XmlWriter(st) => match &st.target {
                Val::Text(p) => Some(p.clone()),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// XmlNode.WriteTo(XmlWriter): the node's markup through the writer to its target (a pending start
/// tag is closed first). False when `v` is not an XmlWriter.
pub fn write_raw(v: &Val, text: &str) -> bool {
    let Val::Net(n) = v else { return false };
    let target = {
        let mut o = n.borrow_mut();
        let NetObj::XmlWriter(st) = &mut *o else { return false };
        st.close_start();
        st.root_written = true;
        st.target.clone()
    };
    write_text_to(&target, text);
    true
}

/// Append text to a writer target.
pub fn write_text_to(target: &Val, text: &str) {
    match target {
        Val::Net(n) => {
            let inner = match &mut *n.borrow_mut() {
                NetObj::StringBuilder(s) | NetObj::Writer(_, s) => {
                    s.push_str(text);
                    None
                }
                NetObj::StreamWriter(t) => Some(t.clone()),
                NetObj::Stream(..) | NetObj::CryptoStream(_) => Some(target.clone()),
                _ => None,
            };
            match inner {
                Some(t @ Val::Net(_)) if matches!(&t, Val::Net(x) if Rc::ptr_eq(x, n)) => super::text::stream_write(&t, text.as_bytes()),
                Some(t) => write_text_to(&t, text),
                None => {}
            }
        }
        Val::OutStream(t) => super::text::outstream_append(t, text.as_bytes()),
        _ => {}
    }
}

fn is_text_target(v: &Val) -> bool {
    match v {
        Val::Net(n) => match &*n.borrow() {
            NetObj::StringBuilder(_) | NetObj::Writer(..) => true,
            NetObj::StreamWriter(t) => is_text_target(t),
            _ => false,
        },
        _ => false,
    }
}

pub fn construct(a: &Args) -> Val {
    let target = a.raw(0).cloned().unwrap_or(Val::Void);
    let encoding = if is_text_target(&target) {
        Some("utf-16".to_string())
    } else {
        match a.raw(1) {
            Some(Val::Net(e)) => match &*e.borrow() {
                NetObj::Encoding(cp, _) => Some(if *cp == 65001 { "utf-8".to_string() } else if *cp == 1200 { "utf-16".into() } else { format!("windows-{cp}") }),
                _ => None,
            },
            _ => None,
        }
    };
    net(NetObj::XmlWriter(Box::new(XmlWriterState { target, stack: vec![], start_open: false, root_written: false, encoding })))
}

impl XmlWriterState {
    fn out(&self, s: &str) {
        write_text_to(&self.target, s);
    }
    fn close_start(&mut self) {
        if self.start_open {
            self.out(">");
            self.start_open = false;
        }
    }
    fn lookup(&self, prefix: &str) -> Option<&str> {
        self.stack.iter().rev().flat_map(|(_, d)| d.iter().rev()).find(|(p, _)| p == prefix).map(|(_, u)| u.as_str())
    }
    fn start_element(&mut self, prefix: &str, local: &str, ns: Option<&str>) {
        self.close_start();
        let q = if prefix.is_empty() { local.to_string() } else { format!("{prefix}:{local}") };
        self.out(&format!("<{q}"));
        let mut decl = vec![];
        if let Some(ns) = ns {
            if self.lookup(prefix).unwrap_or("") != ns {
                if prefix.is_empty() {
                    self.out(&format!(" xmlns=\"{}\"", escape_attr(ns)));
                } else {
                    self.out(&format!(" xmlns:{prefix}=\"{}\"", escape_attr(ns)));
                }
                decl.push((prefix.to_string(), ns.to_string()));
            }
        }
        self.stack.push((q, decl));
        self.start_open = true;
        self.root_written = true;
    }
    fn end_element(&mut self, full: bool) -> Result<(), String> {
        let Some((q, _)) = self.stack.pop() else { return Err("There was no XML start tag open.".into()) };
        if self.start_open && !full {
            self.out(" />");
            self.start_open = false;
        } else {
            self.close_start();
            self.out(&format!("</{q}>"));
        }
        Ok(())
    }
    fn attribute(&mut self, prefix: &str, local: &str, ns: Option<&str>, value: &str) {
        let q = if prefix.is_empty() { local.to_string() } else { format!("{prefix}:{local}") };
        self.out(&format!(" {q}=\"{}\"", escape_attr(value)));
        if let (Some(ns), false) = (ns, prefix.is_empty() || prefix == "xmlns") {
            if self.lookup(prefix) != Some(ns) {
                self.out(&format!(" xmlns:{prefix}=\"{}\"", escape_attr(ns)));
                if let Some(top) = self.stack.last_mut() {
                    top.1.push((prefix.to_string(), ns.to_string()));
                }
            }
        }
    }
}

pub fn call(n: &NetRef, m: &str, a: &Args) -> R<Out> {
    let mut o = n.borrow_mut();
    let NetObj::XmlWriter(w) = &mut *o else { return Ok(Out::Unknown) };
    let fail = |msg: String| -> R<Out> { net_err("XmlTextWriter", m, msg) };
    match m {
        "writestartdocument" => {
            let mut d = String::from("<?xml version=\"1.0\"");
            if let Some(e) = &w.encoding {
                d.push_str(&format!(" encoding=\"{e}\""));
            }
            if !a.is_empty() {
                d.push_str(if a.bool(0) { " standalone=\"yes\"" } else { " standalone=\"no\"" });
            }
            d.push_str("?>");
            w.out(&d);
        }
        "writestartelement" => match a.len() {
            1 => w.start_element("", &a.text(0), None),
            2 => w.start_element("", &a.text(0), Some(&a.text(1))),
            _ => w.start_element(&a.text(0), &a.text(1), Some(&a.text(2))),
        },
        "writeelementstring" => {
            let (p, l, ns, v) = match a.len() {
                2 => (String::new(), a.text(0), None, a.text(1)),
                3 => (String::new(), a.text(0), Some(a.text(1)), a.text(2)),
                _ => (a.text(0), a.text(1), Some(a.text(2)), a.text(3)),
            };
            w.start_element(&p, &l, ns.as_deref());
            if !v.is_empty() {
                w.close_start();
                w.out(&escape_text(&v));
            }
            if let Err(e) = w.end_element(false) {
                drop(o);
                return fail(e);
            }
        }
        "writeattributestring" => {
            let (p, l, ns, v) = match a.len() {
                2 => (String::new(), a.text(0), None, a.text(1)),
                3 => (String::new(), a.text(0), Some(a.text(1)), a.text(2)),
                _ => (a.text(0), a.text(1), Some(a.text(2)), a.text(3)),
            };
            if !w.start_open {
                drop(o);
                return fail("Token StartAttribute in state Content would result in an invalid XML document.".into());
            }
            w.attribute(&p, &l, ns.as_deref(), &v);
        }
        "writestring" => {
            w.close_start();
            w.out(&escape_text(&a.text(0)));
        }
        "writeraw" => {
            w.close_start();
            w.out(&a.text(0));
        }
        "writecdata" => {
            w.close_start();
            w.out(&format!("<![CDATA[{}]]>", a.text(0)));
        }
        "writecomment" => {
            w.close_start();
            w.out(&format!("<!--{}-->", a.text(0)));
        }
        "writeprocessinginstruction" => {
            w.close_start();
            let t = a.text(1);
            w.out(&if t.is_empty() { format!("<?{}?>", a.text(0)) } else { format!("<?{} {t}?>", a.text(0)) });
        }
        "writeendelement" | "writefullendelement" => {
            if let Err(e) = w.end_element(m == "writefullendelement") {
                drop(o);
                return fail(e);
            }
        }
        "writeenddocument" => {
            if !w.root_written {
                drop(o);
                return fail("Document does not have a root element.".into());
            }
            while !w.stack.is_empty() {
                let _ = w.end_element(false);
            }
        }
        "flush" => {}
        "close" | "dispose" => {
            while !w.stack.is_empty() {
                let _ = w.end_element(false);
            }
        }
        "formatting" | "indentation" | "namespaces" | "quotechar" | "indentchar" => {}
        "lookupprefix" => {
            let ns = a.text(0);
            let p = w.stack.iter().rev().flat_map(|(_, d)| d.iter().rev()).find(|(_, u)| *u == ns).map(|(p, _)| Val::Text(p.clone())).unwrap_or(Val::Void);
            return ok(p);
        }
        _ => return Ok(Out::Unknown),
    }
    void()
}
