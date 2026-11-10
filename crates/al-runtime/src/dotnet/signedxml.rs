//! System.Security.Cryptography.Xml: XML-DSig signatures (SignedXml, SignedInfo, Reference, the
//! C14N / exclusive C14N / enveloped-signature transforms, KeyInfo, DataObject) as .NET 8 implements
//! them for the System Application's Signed XML module and BaseApp's SignedXMLMgt.
//!
//! Canonical XML 1.0 (inclusive, with or without comments) and Exclusive XML Canonicalization run
//! over the dotnet::xml DOM. A subtree apex renders every namespace in scope from its ancestors
//! (.NET propagates them for #id references and for a loaded SignedInfo). URI "" is the whole
//! document and "#id" the element with that Id, both without comments. Defaults are .NET 8's: SHA-256
//! digests, RSA-SHA256 signatures, C14N canonicalization. Signatures are RSA PKCS#1 v1.5 (asym.rs).

use super::xml::{self as dom, Node, NK};
use super::*;
use base64::Engine;
use std::collections::BTreeMap;

pub const SIGNED_XML: &str = "System.Security.Cryptography.Xml.SignedXml";
pub const SIGNED_INFO: &str = "System.Security.Cryptography.Xml.SignedInfo";
pub const REFERENCE: &str = "System.Security.Cryptography.Xml.Reference";
pub const C14N: &str = "System.Security.Cryptography.Xml.XmlDsigC14NTransform";
pub const EXC_C14N: &str = "System.Security.Cryptography.Xml.XmlDsigExcC14NTransform";
pub const ENVELOPED: &str = "System.Security.Cryptography.Xml.XmlDsigEnvelopedSignatureTransform";
pub const KEY_INFO: &str = "System.Security.Cryptography.Xml.KeyInfo";
pub const KEY_INFO_X509: &str = "System.Security.Cryptography.Xml.KeyInfoX509Data";
pub const KEY_INFO_NODE: &str = "System.Security.Cryptography.Xml.KeyInfoNode";
pub const KEY_INFO_CLAUSE: &str = "System.Security.Cryptography.Xml.KeyInfoClause";
pub const DATA_OBJECT: &str = "System.Security.Cryptography.Xml.DataObject";
pub const TYPES: &[&str] = &[SIGNED_XML, SIGNED_INFO, REFERENCE, C14N, EXC_C14N, ENVELOPED, KEY_INFO, KEY_INFO_X509, KEY_INFO_NODE, KEY_INFO_CLAUSE, DATA_OBJECT];

const DSIG: &str = "http://www.w3.org/2000/09/xmldsig#";
const C14N_URL: &str = "http://www.w3.org/TR/2001/REC-xml-c14n-20010315";
const C14N_COMMENTS_URL: &str = "http://www.w3.org/TR/2001/REC-xml-c14n-20010315#WithComments";
const EXC_URL: &str = "http://www.w3.org/2001/10/xml-exc-c14n#";
const EXC_COMMENTS_URL: &str = "http://www.w3.org/2001/10/xml-exc-c14n#WithComments";
const ENVELOPED_URL: &str = "http://www.w3.org/2000/09/xmldsig#enveloped-signature";
const SHA256_URL: &str = "http://www.w3.org/2001/04/xmlenc#sha256";
const RSA_SHA256_URL: &str = "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256";

/// SignedXml's public static URL fields (Type.GetField(Name).GetValue).
pub fn static_field(name: &str) -> Option<&'static str> {
    Some(match name {
        "XmlDsigNamespaceUrl" => DSIG,
        "XmlDsigMinimalCanonicalizationUrl" => "http://www.w3.org/2000/09/xmldsig#minimal",
        "XmlDsigCanonicalizationUrl" | "XmlDsigC14NTransformUrl" => C14N_URL,
        "XmlDsigCanonicalizationWithCommentsUrl" | "XmlDsigC14NWithCommentsTransformUrl" => C14N_COMMENTS_URL,
        "XmlDsigExcC14NTransformUrl" => EXC_URL,
        "XmlDsigExcC14NWithCommentsTransformUrl" => EXC_COMMENTS_URL,
        "XmlDsigEnvelopedSignatureTransformUrl" => ENVELOPED_URL,
        "XmlDsigBase64TransformUrl" => "http://www.w3.org/2000/09/xmldsig#base64",
        "XmlDsigXPathTransformUrl" => "http://www.w3.org/TR/1999/REC-xpath-19991116",
        "XmlDsigXsltTransformUrl" => "http://www.w3.org/TR/1999/REC-xslt-19991116",
        "XmlDecryptionTransformUrl" => "http://www.w3.org/2002/07/decrypt#XML",
        "XmlLicenseTransformUrl" => "urn:mpeg:mpeg21:2003:01-REL-R-NS:licensetransform",
        "XmlDsigSHA1Url" => "http://www.w3.org/2000/09/xmldsig#sha1",
        "XmlDsigSHA256Url" => SHA256_URL,
        "XmlDsigSHA384Url" => "http://www.w3.org/2001/04/xmldsig-more#sha384",
        "XmlDsigSHA512Url" => "http://www.w3.org/2001/04/xmlenc#sha512",
        "XmlDsigDSAUrl" => "http://www.w3.org/2000/09/xmldsig#dsa-sha1",
        "XmlDsigRSASHA1Url" => "http://www.w3.org/2000/09/xmldsig#rsa-sha1",
        "XmlDsigRSASHA256Url" => RSA_SHA256_URL,
        "XmlDsigRSASHA384Url" => "http://www.w3.org/2001/04/xmldsig-more#rsa-sha384",
        "XmlDsigRSASHA512Url" => "http://www.w3.org/2001/04/xmldsig-more#rsa-sha512",
        "XmlDsigHMACSHA1Url" => "http://www.w3.org/2000/09/xmldsig#hmac-sha1",
        _ => return None,
    })
}

/// The hash of a digest or signature method URL.
fn hash_of(url: &str) -> Option<&'static str> {
    let u = url.to_ascii_lowercase();
    Some(if u.ends_with("sha1") {
        "SHA1"
    } else if u.ends_with("sha256") {
        "SHA256"
    } else if u.ends_with("sha384") {
        "SHA384"
    } else if u.ends_with("sha512") {
        "SHA512"
    } else {
        return None;
    })
}

#[derive(Clone, Default)]
pub struct Transform {
    pub alg: String,
    /// XmlDsigExcC14NTransform.InclusiveNamespacesPrefixList
    pub prefixes: String,
}

impl Transform {
    fn xml(&self, element: &str) -> String {
        if self.prefixes.is_empty() {
            format!("<{element} Algorithm=\"{}\" />", dom::escape_attr(&self.alg))
        } else {
            format!("<{element} Algorithm=\"{}\"><InclusiveNamespaces PrefixList=\"{}\" xmlns=\"{EXC_URL}\" /></{element}>", dom::escape_attr(&self.alg), dom::escape_attr(&self.prefixes))
        }
    }
}

#[derive(Default)]
pub struct RefData {
    pub uri: String,
    pub id: String,
    pub ty: String,
    pub digest_method: String,
    pub transforms: Vec<Rc<RefCell<Transform>>>,
    pub digest: Vec<u8>,
}

pub enum KeyClause {
    X509(Vec<u8>),
    Node(Node),
}

#[derive(Default)]
pub struct SxState {
    doc: Option<Node>,
    c14n: Rc<RefCell<Transform>>,
    sig_method: String,
    refs: Vec<Rc<RefCell<RefData>>>,
    key: Option<NetRef>,
    key_info: Option<Rc<RefCell<Vec<KeyClause>>>>,
    objects: Vec<Node>,
    signature_value: Vec<u8>,
    /// LoadXml: the Signature element (verification reads it as is)
    loaded: Option<Node>,
}

pub enum Sx {
    Signed(Rc<RefCell<SxState>>),
    SignedInfo(Rc<RefCell<SxState>>),
    Reference(Rc<RefCell<RefData>>),
    Transform(&'static str, Rc<RefCell<Transform>>),
    KeyInfo(Rc<RefCell<Vec<KeyClause>>>),
    X509Data(Vec<u8>),
    KeyNode(Node),
    DataObject(Rc<RefCell<Option<Node>>>),
}

pub fn type_name(s: &Sx) -> &'static str {
    match s {
        Sx::Signed(_) => SIGNED_XML,
        Sx::SignedInfo(_) => SIGNED_INFO,
        Sx::Reference(_) => REFERENCE,
        Sx::Transform(t, _) => t,
        Sx::KeyInfo(_) => KEY_INFO,
        Sx::X509Data(_) => KEY_INFO_X509,
        Sx::KeyNode(_) => KEY_INFO_NODE,
        Sx::DataObject(_) => DATA_OBJECT,
    }
}

fn sx(s: Sx) -> Val {
    net(NetObj::Sx(s))
}

fn node_of(v: Option<&Val>) -> Option<Node> {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::XNode(x, _) => Some(x.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn owner_doc(n: &Node) -> Node {
    let mut cur = n.clone();
    loop {
        if dom::kind(&cur) == NK::Document {
            return cur;
        }
        let owner = cur.borrow().owner.upgrade();
        match dom::parent(&cur).or(owner) {
            Some(p) => cur = p,
            None => return cur,
        }
    }
}

/// Constructors (SignedXml(XmlDocument | XmlElement), Reference(uri), the transforms, KeyInfo,
/// KeyInfoX509Data(cert), KeyInfoNode(element), DataObject).
pub fn construct(ty: &'static str, a: &Args) -> R<Val> {
    Ok(match ty {
        SIGNED_XML => {
            let doc = node_of(a.raw(0)).map(|n| owner_doc(&n));
            let st = SxState { doc, c14n: Rc::new(RefCell::new(Transform { alg: C14N_URL.into(), prefixes: String::new() })), ..Default::default() };
            sx(Sx::Signed(Rc::new(RefCell::new(st))))
        }
        REFERENCE => sx(Sx::Reference(Rc::new(RefCell::new(RefData { uri: if a.is_empty() { String::new() } else { a.text(0) }, digest_method: SHA256_URL.into(), ..Default::default() })))),
        C14N => {
            let comments = !a.is_empty() && a.bool(0);
            sx(Sx::Transform(C14N, Rc::new(RefCell::new(Transform { alg: if comments { C14N_COMMENTS_URL } else { C14N_URL }.into(), prefixes: String::new() }))))
        }
        EXC_C14N => {
            // (), (includeComments), (prefixList), (includeComments, prefixList)
            let (comments, prefixes) = match (a.raw(0), a.raw(1)) {
                (Some(Val::Bool(b)), Some(p)) => (*b, p.to_text()),
                (Some(Val::Bool(b)), None) => (*b, String::new()),
                (Some(p), _) => (false, p.to_text()),
                _ => (false, String::new()),
            };
            sx(Sx::Transform(EXC_C14N, Rc::new(RefCell::new(Transform { alg: if comments { EXC_COMMENTS_URL } else { EXC_URL }.into(), prefixes }))))
        }
        ENVELOPED => sx(Sx::Transform(ENVELOPED, Rc::new(RefCell::new(Transform { alg: ENVELOPED_URL.into(), prefixes: String::new() })))),
        KEY_INFO => sx(Sx::KeyInfo(Rc::new(RefCell::new(vec![])))),
        KEY_INFO_X509 => match a.raw(0) {
            Some(Val::Net(n)) => match &*n.borrow() {
                NetObj::Cert(c) => sx(Sx::X509Data(c.der.clone())),
                _ => sx(Sx::X509Data(vec![])),
            },
            _ => sx(Sx::X509Data(vec![])),
        },
        KEY_INFO_NODE => sx(Sx::KeyNode(node_of(a.raw(0)).unwrap_or_else(|| dom::element("", "KeyInfo", DSIG)))),
        DATA_OBJECT => sx(Sx::DataObject(Rc::new(RefCell::new(None)))),
        _ => return net_err(short_name(ty), short_name(ty), "Not supported."),
    })
}

// ------------------------------------------------------------------ canonicalization

#[derive(Clone, Copy)]
pub struct C14nOpts<'a> {
    comments: bool,
    /// exclusive canonicalization: the InclusiveNamespaces prefix list ("#default" = the default namespace)
    exclusive: Option<&'a [String]>,
}

fn c14n_opts(alg: &str, prefixes: &str) -> Option<(bool, bool, Vec<String>)> {
    Some(match alg {
        C14N_URL => (false, false, vec![]),
        C14N_COMMENTS_URL => (true, false, vec![]),
        EXC_URL => (false, true, prefixes.split_whitespace().map(String::from).collect()),
        EXC_COMMENTS_URL => (true, true, prefixes.split_whitespace().map(String::from).collect()),
        _ => return None,
    })
}

fn esc_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

fn esc_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            c => out.push(c),
        }
    }
}

/// The namespace declarations an element makes: its xmlns attributes, plus the bindings its own
/// name and attribute names use (nodes built in code may carry no xmlns attribute).
fn own_decls(e: &Node) -> Vec<(String, String)> {
    let b = e.borrow();
    let mut v: Vec<(String, String)> = vec![];
    for a in &b.attrs {
        let ab = a.borrow();
        if ab.prefix == "xmlns" {
            v.push((ab.local.clone(), ab.value.clone()));
        } else if ab.prefix.is_empty() && ab.local == "xmlns" {
            v.push((String::new(), ab.value.clone()));
        }
    }
    let mut implicit = |p: &str, ns: &str| {
        if p == "xml" || p == "xmlns" {
            return;
        }
        if !v.iter().any(|(q, _)| q == p) {
            v.push((p.to_string(), ns.to_string()));
        }
    };
    implicit(&b.prefix, &b.ns);
    for a in &b.attrs {
        let ab = a.borrow();
        if !ab.prefix.is_empty() && ab.prefix != "xmlns" {
            implicit(&ab.prefix, &ab.ns);
        }
    }
    v
}

/// The namespaces in scope at `e` (ancestors first, nearest declaration wins).
fn in_scope(e: &Node) -> BTreeMap<String, String> {
    let mut chain = vec![e.clone()];
    let mut cur = dom::parent(e);
    while let Some(p) = cur {
        if dom::kind(&p) != NK::Element {
            break;
        }
        chain.push(p.clone());
        cur = dom::parent(&p);
    }
    let mut m = BTreeMap::new();
    for n in chain.iter().rev() {
        for (p, u) in own_decls(n) {
            m.insert(p, u);
        }
    }
    m
}

/// xml:* attributes inherited from the ancestors of a subtree apex (inclusive C14N only).
fn inherited_xml_attrs(e: &Node) -> Vec<(String, String)> {
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    let mut cur = dom::parent(e);
    while let Some(p) = cur {
        if dom::kind(&p) != NK::Element {
            break;
        }
        for a in &p.borrow().attrs {
            let ab = a.borrow();
            if ab.prefix == "xml" {
                found.entry(ab.local.clone()).or_insert_with(|| ab.value.clone());
            }
        }
        cur = dom::parent(&p);
    }
    found.into_iter().collect()
}

struct Canon<'a> {
    opts: C14nOpts<'a>,
    excluded: Option<Node>,
    out: String,
}

impl Canon<'_> {
    fn is_excluded(&self, n: &Node) -> bool {
        self.excluded.as_ref().is_some_and(|x| Rc::ptr_eq(x, n))
    }

    /// `rendered`: the namespaces the nearest output ancestor rendered (prefix -> uri).
    fn element(&mut self, e: &Node, rendered: &BTreeMap<String, String>, apex: bool) {
        if self.is_excluded(e) {
            return;
        }
        let scope = in_scope(e);
        let (prefix, local, attrs, kids) = {
            let b = e.borrow();
            (b.prefix.clone(), b.local.clone(), b.attrs.clone(), b.children.clone())
        };
        // namespace axis
        let mut ns_out: Vec<(String, String)> = vec![];
        let mut now = rendered.clone();
        match self.opts.exclusive {
            None => {
                for (p, u) in &scope {
                    if p == "xml" {
                        continue;
                    }
                    let parent = rendered.get(p).map(String::as_str);
                    // an empty default namespace is only written to undo a rendered one
                    let need = if p.is_empty() && u.is_empty() { parent.is_some_and(|x| !x.is_empty()) } else { parent != Some(u.as_str()) };
                    if need {
                        ns_out.push((p.clone(), u.clone()));
                        now.insert(p.clone(), u.clone());
                    }
                }
            }
            Some(incl) => {
                let mut used: Vec<String> = vec![prefix.clone()];
                for a in &attrs {
                    let ab = a.borrow();
                    if !ab.prefix.is_empty() && ab.prefix != "xmlns" && ab.prefix != "xml" {
                        used.push(ab.prefix.clone());
                    }
                }
                for p in incl {
                    used.push(if p == "#default" { String::new() } else { p.clone() });
                }
                used.sort();
                used.dedup();
                for p in used {
                    if p == "xml" {
                        continue;
                    }
                    let u = scope.get(&p).cloned().unwrap_or_default();
                    let parent = rendered.get(&p).map(String::as_str);
                    let need = if p.is_empty() && u.is_empty() { parent.is_some_and(|x| !x.is_empty()) } else { parent != Some(u.as_str()) };
                    if need && (scope.contains_key(&p) || p.is_empty()) {
                        ns_out.push((p.clone(), u.clone()));
                        now.insert(p, u);
                    }
                }
            }
        }
        ns_out.sort_by(|a, b| a.0.cmp(&b.0));
        // attribute axis: sorted by namespace URI, then local name
        let mut at: Vec<(String, String, String)> = vec![]; // (ns, qname, value)
        for a in &attrs {
            let ab = a.borrow();
            if ab.prefix == "xmlns" || (ab.prefix.is_empty() && ab.local == "xmlns") {
                continue;
            }
            let q = if ab.prefix.is_empty() { ab.local.clone() } else { format!("{}:{}", ab.prefix, ab.local) };
            let ns = if ab.prefix == "xml" { dom::XML_NS.to_string() } else { ab.ns.clone() };
            at.push((ns, q, ab.value.clone()));
        }
        if apex && self.opts.exclusive.is_none() {
            for (l, v) in inherited_xml_attrs(e) {
                if !at.iter().any(|(_, q, _)| q == &format!("xml:{l}")) {
                    at.push((dom::XML_NS.to_string(), format!("xml:{l}"), v));
                }
            }
        }
        at.sort_by(|a, b| {
            let la = a.1.rsplit(':').next().unwrap_or("");
            let lb = b.1.rsplit(':').next().unwrap_or("");
            (a.0.as_str(), la).cmp(&(b.0.as_str(), lb))
        });
        let qname = if prefix.is_empty() { local.clone() } else { format!("{prefix}:{local}") };
        self.out.push('<');
        self.out.push_str(&qname);
        for (p, u) in &ns_out {
            self.out.push_str(if p.is_empty() { " xmlns=\"" } else { " xmlns:" });
            if !p.is_empty() {
                self.out.push_str(p);
                self.out.push_str("=\"");
            }
            esc_attr(u, &mut self.out);
            self.out.push('"');
        }
        for (_, q, v) in &at {
            self.out.push(' ');
            self.out.push_str(q);
            self.out.push_str("=\"");
            esc_attr(v, &mut self.out);
            self.out.push('"');
        }
        self.out.push('>');
        for k in &kids {
            self.child(k, &now);
        }
        self.out.push_str("</");
        self.out.push_str(&qname);
        self.out.push('>');
    }

    fn child(&mut self, k: &Node, rendered: &BTreeMap<String, String>) {
        match dom::kind(k) {
            NK::Element => self.element(k, rendered, false),
            NK::Text | NK::CData | NK::Whitespace => {
                let v = k.borrow().value.clone();
                esc_text(&v, &mut self.out);
            }
            NK::Comment if self.opts.comments => {
                let v = k.borrow().value.clone();
                self.out.push_str("<!--");
                self.out.push_str(&v);
                self.out.push_str("-->");
            }
            NK::PI => self.pi(k),
            _ => {}
        }
    }

    fn pi(&mut self, k: &Node) {
        let (t, v) = {
            let b = k.borrow();
            (b.local.clone(), b.value.clone())
        };
        self.out.push_str("<?");
        self.out.push_str(&t);
        if !v.is_empty() {
            self.out.push(' ');
            self.out.push_str(&v);
        }
        self.out.push_str("?>");
    }

    /// A whole document: the document element plus top-level PIs and comments, each separated from
    /// the document element by a line feed.
    fn document(&mut self, d: &Node) {
        let kids = dom::children(d);
        let root = kids.iter().position(|k| dom::kind(k) == NK::Element);
        for (i, k) in kids.iter().enumerate() {
            let before = root.is_some_and(|r| i < r);
            match dom::kind(k) {
                NK::Element => self.element(k, &BTreeMap::new(), true),
                NK::PI | NK::Comment => {
                    if dom::kind(k) == NK::Comment && !self.opts.comments {
                        continue;
                    }
                    if !before {
                        self.out.push('\n');
                    }
                    if dom::kind(k) == NK::PI {
                        self.pi(k);
                    } else {
                        let v = k.borrow().value.clone();
                        self.out.push_str("<!--");
                        self.out.push_str(&v);
                        self.out.push_str("-->");
                    }
                    if before {
                        self.out.push('\n');
                    }
                }
                _ => {}
            }
        }
    }
}

/// Canonicalize a document or an element subtree (with its in-scope namespace context).
pub fn canonicalize(apex: &Node, comments: bool, exclusive: Option<&[String]>, excluded: Option<Node>) -> String {
    let mut c = Canon { opts: C14nOpts { comments, exclusive }, excluded, out: String::new() };
    if dom::kind(apex) == NK::Document {
        c.document(apex);
    } else {
        c.element(apex, &BTreeMap::new(), true);
    }
    c.out
}

// ------------------------------------------------------------------ references

fn find_id(n: &Node, id: &str) -> Option<Node> {
    if dom::kind(n) == NK::Element {
        let hit = n.borrow().attrs.iter().any(|a| {
            let ab = a.borrow();
            matches!(ab.local.as_str(), "Id" | "ID" | "id") && ab.prefix.is_empty() && ab.value == id
        });
        if hit {
            return Some(n.clone());
        }
    }
    for k in dom::children(n) {
        if let Some(x) = find_id(&k, id) {
            return Some(x);
        }
    }
    None
}

/// The octets a reference digests: the URI's node-set through the transforms (enveloped removes
/// `signature`), canonicalized by the last C14N transform or by inclusive C14N.
fn reference_octets(doc: &Node, objects: &[Node], r: &RefData, signature: Option<&Node>) -> Result<String, String> {
    let apex = if r.uri.is_empty() {
        doc.clone()
    } else if let Some(id) = r.uri.strip_prefix('#') {
        find_id(doc, id).or_else(|| objects.iter().find_map(|o| find_id(o, id))).ok_or_else(|| format!("Malformed reference element."))?
    } else {
        return Err("Unable to resolve Uri.".into());
    };
    let mut excluded = None;
    let mut canon: Option<(bool, bool, Vec<String>)> = None;
    for t in &r.transforms {
        let t = t.borrow();
        if t.alg == ENVELOPED_URL {
            // the Signature element: the loaded one when it is in this document, else the
            // document's first ds:Signature (XmlDsigEnvelopedSignatureTransform.SignaturePosition)
            excluded = match signature {
                Some(s) if Rc::ptr_eq(&owner_doc(s), doc) => Some(s.clone()),
                _ => first_signature(doc),
            };
        } else if let Some(o) = c14n_opts(&t.alg, &t.prefixes) {
            canon = Some(o);
        } else {
            return Err(format!("Unknown transform has been encountered."));
        }
    }
    // a same-document reference drops comments; a #WithComments transform cannot bring them back
    let (comments, exclusive, prefixes) = canon.unwrap_or((false, false, vec![]));
    let _ = comments;
    Ok(canonicalize(&apex, false, exclusive.then_some(prefixes.as_slice()), excluded))
}

fn first_signature(n: &Node) -> Option<Node> {
    if dom::kind(n) == NK::Element {
        let b = n.borrow();
        if b.local == "Signature" && b.ns == DSIG {
            drop(b);
            return Some(n.clone());
        }
    }
    dom::children(n).iter().find_map(first_signature)
}

fn digest_of(method: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let h = hash_of(method).ok_or_else(|| "Could not create hash algorithm object.".to_string())?;
    Ok(super::crypto::digest(h, data))
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn unb64(s: &str) -> Vec<u8> {
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(t).unwrap_or_default()
}

fn signed_info_xml(st: &SxState) -> String {
    let c = st.c14n.borrow();
    let mut x = format!("<SignedInfo xmlns=\"{DSIG}\">{}<SignatureMethod Algorithm=\"{}\" />", c.xml("CanonicalizationMethod"), dom::escape_attr(&st.sig_method));
    for r in &st.refs {
        let r = r.borrow();
        x.push_str("<Reference");
        if !r.id.is_empty() {
            x.push_str(&format!(" Id=\"{}\"", dom::escape_attr(&r.id)));
        }
        x.push_str(&format!(" URI=\"{}\"", dom::escape_attr(&r.uri)));
        if !r.ty.is_empty() {
            x.push_str(&format!(" Type=\"{}\"", dom::escape_attr(&r.ty)));
        }
        x.push('>');
        if !r.transforms.is_empty() {
            x.push_str("<Transforms>");
            for t in &r.transforms {
                x.push_str(&t.borrow().xml("Transform"));
            }
            x.push_str("</Transforms>");
        }
        x.push_str(&format!("<DigestMethod Algorithm=\"{}\" /><DigestValue>{}</DigestValue></Reference>", dom::escape_attr(&r.digest_method), b64(&r.digest)));
    }
    x.push_str("</SignedInfo>");
    x
}

fn key_info_xml(clauses: &[KeyClause]) -> String {
    let mut x = String::from("<KeyInfo>");
    for c in clauses {
        match c {
            KeyClause::X509(der) => x.push_str(&format!("<X509Data><X509Certificate>{}</X509Certificate></X509Data>", b64(der))),
            KeyClause::Node(n) => x.push_str(&dom::outer_xml(n)),
        }
    }
    x.push_str("</KeyInfo>");
    x
}

fn parse_element(xml: &str) -> Result<Node, String> {
    let doc = dom::parse(xml, &dom::ParseOptions { preserve_whitespace: true })?;
    dom::root_element(&doc).ok_or_else(|| "Root element is missing.".to_string())
}

fn child_el(e: &Node, local: &str) -> Option<Node> {
    dom::element_children(e).into_iter().find(|c| c.borrow().local == local)
}

fn attr(e: &Node, name: &str) -> String {
    dom::get_attr(e, name, None).map(|a| a.borrow().value.clone()).unwrap_or_default()
}

/// A Transform / CanonicalizationMethod element as a Transform (with an InclusiveNamespaces list).
fn transform_of(e: &Node) -> Transform {
    let prefixes = child_el(e, "InclusiveNamespaces").map(|i| attr(&i, "PrefixList")).unwrap_or_default();
    Transform { alg: attr(e, "Algorithm"), prefixes }
}

/// Parsed Signature element: SignedInfo node, canonicalization, signature method, references,
/// signature value, key info element.
struct Parsed {
    signed_info: Node,
    c14n: Transform,
    sig_method: String,
    refs: Vec<RefData>,
    value: Vec<u8>,
    key_info: Option<Node>,
}

fn parse_signature(sig: &Node) -> Result<Parsed, String> {
    let si = child_el(sig, "SignedInfo").ok_or("Malformed element SignedInfo.")?;
    let c14n = child_el(&si, "CanonicalizationMethod").map(|c| transform_of(&c)).unwrap_or(Transform { alg: C14N_URL.into(), prefixes: String::new() });
    let sig_method = child_el(&si, "SignatureMethod").map(|c| attr(&c, "Algorithm")).unwrap_or_default();
    let mut refs = vec![];
    for r in dom::element_children(&si).into_iter().filter(|c| c.borrow().local == "Reference") {
        let transforms = child_el(&r, "Transforms").map(|t| dom::element_children(&t).iter().filter(|x| x.borrow().local == "Transform").map(|x| Rc::new(RefCell::new(transform_of(x)))).collect()).unwrap_or_default();
        refs.push(RefData {
            uri: attr(&r, "URI"),
            id: attr(&r, "Id"),
            ty: attr(&r, "Type"),
            digest_method: child_el(&r, "DigestMethod").map(|d| attr(&d, "Algorithm")).unwrap_or_default(),
            transforms,
            digest: child_el(&r, "DigestValue").map(|d| unb64(&dom::inner_text(&d))).unwrap_or_default(),
        });
    }
    let value = child_el(sig, "SignatureValue").map(|v| unb64(&dom::inner_text(&v))).unwrap_or_default();
    Ok(Parsed { signed_info: si, c14n, sig_method, refs, value, key_info: child_el(sig, "KeyInfo") })
}

fn rsa_from_key_info(ki: &Node) -> Option<::rsa::RsaPublicKey> {
    if let Some(kv) = child_el(ki, "KeyValue").and_then(|k| child_el(&k, "RSAKeyValue")) {
        let n = ::rsa::BigUint::from_bytes_be(&unb64(&child_el(&kv, "Modulus").map(|m| dom::inner_text(&m)).unwrap_or_default()));
        let e = ::rsa::BigUint::from_bytes_be(&unb64(&child_el(&kv, "Exponent").map(|m| dom::inner_text(&m)).unwrap_or_default()));
        return ::rsa::RsaPublicKey::new(n, e).ok();
    }
    let cert = child_el(ki, "X509Data").and_then(|x| child_el(&x, "X509Certificate"))?;
    super::x509::rsa_public_key(&unb64(&dom::inner_text(&cert)))
}

fn rsa_of(v: Option<&Val>) -> Option<::rsa::RsaPublicKey> {
    match v {
        Some(Val::Net(n)) => match &mut *n.borrow_mut() {
            NetObj::Rsa(s) => super::asym::public_key(s).ok(),
            NetObj::Cert(c) => super::x509::rsa_public_key(&c.der),
            _ => None,
        },
        _ => None,
    }
}

/// CheckSignature: every reference's digest, then the signature over the canonical SignedInfo.
fn check(st: &SxState, key: Option<::rsa::RsaPublicKey>) -> Result<bool, String> {
    let doc = st.doc.clone().ok_or("A document must be loaded.")?;
    // the Signature element: loaded, or the one this SignedXml computed
    let (sig, in_doc) = match &st.loaded {
        Some(s) => (s.clone(), true),
        None => {
            if st.signature_value.is_empty() {
                return Err("A signature must be computed or loaded.".into());
            }
            let x = format!("<Signature xmlns=\"{DSIG}\">{}<SignatureValue>{}</SignatureValue></Signature>", signed_info_xml(st), b64(&st.signature_value));
            (parse_element(&x)?, false)
        }
    };
    let p = parse_signature(&sig)?;
    for r in &p.refs {
        let octets = reference_octets(&doc, &st.objects, r, in_doc.then_some(&sig))?;
        if digest_of(&r.digest_method, octets.as_bytes())? != r.digest {
            return Ok(false);
        }
    }
    let (comments, exclusive, prefixes) = c14n_opts(&p.c14n.alg, &p.c14n.prefixes).ok_or("Could not create the XML transformation identified by the URI.")?;
    let canon = canonicalize(&p.signed_info, comments, exclusive.then_some(prefixes.as_slice()), None);
    let key = match key.or_else(|| p.key_info.as_ref().and_then(rsa_from_key_info)) {
        Some(k) => k,
        None => return Ok(false),
    };
    let h = hash_of(&p.sig_method).ok_or("SignatureDescription could not be created for the signature algorithm supplied.")?;
    let hashed = super::crypto::digest(h, canon.as_bytes());
    Ok(super::asym::pkcs1_verify(&key, h, &hashed, &p.value))
}

fn compute(st: &mut SxState) -> Result<(), String> {
    let doc = st.doc.clone().ok_or("A document must be loaded.")?;
    let key = st.key.clone().ok_or("Signing key is not loaded.")?;
    if st.sig_method.is_empty() {
        st.sig_method = RSA_SHA256_URL.into();
    }
    for r in st.refs.clone() {
        let octets = reference_octets(&doc, &st.objects, &r.borrow(), None)?;
        let d = digest_of(&r.borrow().digest_method, octets.as_bytes())?;
        r.borrow_mut().digest = d;
    }
    let si = parse_element(&signed_info_xml(st))?;
    let (comments, exclusive, prefixes) = {
        let c = st.c14n.borrow();
        c14n_opts(&c.alg, &c.prefixes).ok_or("Could not create the XML transformation identified by the URI.")?
    };
    let canon = canonicalize(&si, comments, exclusive.then_some(prefixes.as_slice()), None);
    let h = hash_of(&st.sig_method).ok_or("SignatureDescription could not be created for the signature algorithm supplied.")?;
    let hashed = super::crypto::digest(h, canon.as_bytes());
    let sig = match &mut *key.borrow_mut() {
        NetObj::Rsa(s) => super::asym::pkcs1_sign(s, h, &hashed)?,
        _ => return Err("Signing key is not loaded.".into()),
    };
    st.signature_value = sig;
    st.loaded = None;
    Ok(())
}

fn signature_xml(st: &SxState) -> String {
    let mut x = format!("<Signature xmlns=\"{DSIG}\">{}<SignatureValue>{}</SignatureValue>", signed_info_xml(st), b64(&st.signature_value));
    if let Some(ki) = &st.key_info {
        x.push_str(&key_info_xml(&ki.borrow()));
    }
    for o in &st.objects {
        x.push_str(&dom::outer_xml(o));
    }
    x.push_str("</Signature>");
    x
}

impl<'r> crate::interp::Session<'r> {
    pub fn sx_call(&mut self, n: &NetRef, m: &str, a: &Args) -> R<Out> {
        let o = match &*n.borrow() {
            NetObj::Sx(s) => match s {
                Sx::Signed(x) => Sx::Signed(x.clone()),
                Sx::SignedInfo(x) => Sx::SignedInfo(x.clone()),
                Sx::Reference(x) => Sx::Reference(x.clone()),
                Sx::Transform(t, x) => Sx::Transform(t, x.clone()),
                Sx::KeyInfo(x) => Sx::KeyInfo(x.clone()),
                Sx::X509Data(d) => Sx::X509Data(d.clone()),
                Sx::KeyNode(e) => Sx::KeyNode(e.clone()),
                Sx::DataObject(x) => Sx::DataObject(x.clone()),
            },
            _ => return Ok(Out::Unknown),
        };
        let set = a.len() == 1;
        match o {
            Sx::Signed(st) => match m {
                "signinginfo" | "signedinfo" => ok(sx(Sx::SignedInfo(st))),
                "signingkey" if set => {
                    st.borrow_mut().key = match a.raw(0) {
                        Some(Val::Net(k)) => Some(k.clone()),
                        _ => None,
                    };
                    void()
                }
                "signingkey" => ok(st.borrow().key.clone().map(Val::Net).unwrap_or(net(NetObj::Null("System.Security.Cryptography.AsymmetricAlgorithm")))),
                "keyinfo" if set => {
                    if let Some(Val::Net(k)) = a.raw(0) {
                        if let NetObj::Sx(Sx::KeyInfo(c)) = &*k.borrow() {
                            st.borrow_mut().key_info = Some(c.clone());
                        }
                    }
                    void()
                }
                "keyinfo" => {
                    let c = st.borrow_mut().key_info.get_or_insert_with(|| Rc::new(RefCell::new(vec![]))).clone();
                    ok(sx(Sx::KeyInfo(c)))
                }
                "addreference" => {
                    if let Some(Val::Net(r)) = a.raw(0) {
                        if let NetObj::Sx(Sx::Reference(x)) = &*r.borrow() {
                            st.borrow_mut().refs.push(x.clone());
                        }
                    }
                    void()
                }
                "addobject" => {
                    if let Some(Val::Net(r)) = a.raw(0) {
                        if let NetObj::Sx(Sx::DataObject(x)) = &*r.borrow() {
                            if let Some(e) = x.borrow().clone() {
                                st.borrow_mut().objects.push(e);
                            }
                        }
                    }
                    void()
                }
                "computesignature" => match compute(&mut st.borrow_mut()) {
                    Ok(()) => void(),
                    Err(e) => net_err("SignedXml", "ComputeSignature", e),
                },
                "getxml" => {
                    let x = signature_xml(&st.borrow());
                    match parse_element(&x) {
                        Ok(e) => ok(net(NetObj::XNode(e, true))),
                        Err(e) => net_err("SignedXml", "GetXml", e),
                    }
                }
                "loadxml" => match node_of(a.raw(0)) {
                    Some(e) if e.borrow().local == "Signature" => {
                        let mut s = st.borrow_mut();
                        if let Ok(p) = parse_signature(&e) {
                            s.c14n = Rc::new(RefCell::new(p.c14n));
                            s.sig_method = p.sig_method;
                            s.refs = p.refs.into_iter().map(|r| Rc::new(RefCell::new(r))).collect();
                            s.signature_value = p.value;
                        }
                        s.loaded = Some(e);
                        void()
                    }
                    _ => net_err("SignedXml", "LoadXml", "Malformed element Signature."),
                },
                "checksignature" => {
                    // (), (AsymmetricAlgorithm), (X509Certificate2, verifySignatureOnly)
                    let key = if a.is_empty() { None } else { rsa_of(a.raw(0)) };
                    if !a.is_empty() && key.is_none() {
                        return net_err("SignedXml", "CheckSignature", "Value cannot be null. (Parameter 'key')");
                    }
                    match check(&st.borrow(), key) {
                        Ok(b) => ok(Val::Bool(b)),
                        Err(e) => net_err("SignedXml", "CheckSignature", e),
                    }
                }
                "signaturevalue" => ok(super::text::bytes_val(&st.borrow().signature_value)),
                "signaturemethod" => ok(Val::Text(st.borrow().sig_method.clone())),
                _ => Ok(Out::Unknown),
            },
            Sx::SignedInfo(st) => match m {
                "canonicalizationmethod" if set => {
                    let alg = a.text(0);
                    let c = st.borrow().c14n.clone();
                    let mut c = c.borrow_mut();
                    c.alg = alg;
                    c.prefixes.clear();
                    void()
                }
                "canonicalizationmethod" => ok(Val::Text(st.borrow().c14n.borrow().alg.clone())),
                "canonicalizationmethodobject" => {
                    let c = st.borrow().c14n.clone();
                    let ty = if c.borrow().alg.starts_with(EXC_URL) { EXC_C14N } else { C14N };
                    ok(sx(Sx::Transform(ty, c)))
                }
                "signaturemethod" if set => {
                    st.borrow_mut().sig_method = a.text(0);
                    void()
                }
                "signaturemethod" => ok(Val::Text(st.borrow().sig_method.clone())),
                _ => Ok(Out::Unknown),
            },
            Sx::Reference(r) => match m {
                "uri" if set => {
                    r.borrow_mut().uri = a.text(0);
                    void()
                }
                "uri" => ok(Val::Text(r.borrow().uri.clone())),
                "type" if set => {
                    r.borrow_mut().ty = a.text(0);
                    void()
                }
                "type" => ok(Val::Text(r.borrow().ty.clone())),
                "id" if set => {
                    r.borrow_mut().id = a.text(0);
                    void()
                }
                "id" => ok(Val::Text(r.borrow().id.clone())),
                "digestmethod" if set => {
                    r.borrow_mut().digest_method = a.text(0);
                    void()
                }
                "digestmethod" => ok(Val::Text(r.borrow().digest_method.clone())),
                "digestvalue" => ok(super::text::bytes_val(&r.borrow().digest)),
                "addtransform" => {
                    if let Some(Val::Net(t)) = a.raw(0) {
                        if let NetObj::Sx(Sx::Transform(_, x)) = &*t.borrow() {
                            r.borrow_mut().transforms.push(x.clone());
                        }
                    }
                    void()
                }
                _ => Ok(Out::Unknown),
            },
            Sx::Transform(_, t) => match m {
                "inclusivenamespacesprefixlist" if set => {
                    t.borrow_mut().prefixes = a.text(0);
                    void()
                }
                "inclusivenamespacesprefixlist" => ok(Val::Text(t.borrow().prefixes.clone())),
                "algorithm" => ok(Val::Text(t.borrow().alg.clone())),
                _ => Ok(Out::Unknown),
            },
            Sx::KeyInfo(c) => match m {
                "addclause" => {
                    if let Some(Val::Net(k)) = a.raw(0) {
                        match &*k.borrow() {
                            NetObj::Sx(Sx::X509Data(d)) => c.borrow_mut().push(KeyClause::X509(d.clone())),
                            NetObj::Sx(Sx::KeyNode(e)) => c.borrow_mut().push(KeyClause::Node(e.clone())),
                            _ => {}
                        }
                    }
                    void()
                }
                "count" => ok(Val::Int(c.borrow().len() as i32)),
                "getxml" => match parse_element(&key_info_xml(&c.borrow())) {
                    Ok(e) => ok(net(NetObj::XNode(e, true))),
                    Err(e) => net_err("KeyInfo", "GetXml", e),
                },
                _ => Ok(Out::Unknown),
            },
            Sx::DataObject(d) => match m {
                "loadxml" => {
                    *d.borrow_mut() = node_of(a.raw(0));
                    void()
                }
                "getxml" => ok(match d.borrow().clone() {
                    Some(e) => net(NetObj::XNode(e, true)),
                    None => net(NetObj::Null("System.Xml.XmlElement")),
                }),
                _ => Ok(Out::Unknown),
            },
            Sx::X509Data(_) | Sx::KeyNode(_) => Ok(Out::Unknown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(x: &str) -> Node {
        dom::parse(x, &dom::ParseOptions { preserve_whitespace: true }).unwrap()
    }

    #[test]
    fn canonical_xml_matches_the_spec_examples() {
        // W3C C14N 1.0 section 3.3 (start/end tags), simplified: namespace and attribute order,
        // empty elements, superfluous declarations, attribute escaping
        let d = doc(r#"<doc><e1   /><e2   ></e2><e3   name = "elem3"   id="elem3"   /><e5 a:attr="out" b:attr="sorted" attr2="all" attr="I'm" xmlns:b="http://www.ietf.org" xmlns:a="http://www.w3.org" xmlns="http://example.org"/><e6 xmlns="" xmlns:a="http://www.w3.org"><e7 xmlns="http://www.ietf.org"><e8 xmlns="" xmlns:a="http://www.w3.org"><e9 xmlns="" xmlns:a="http://www.ietf.org"/></e8></e7></e6></doc>"#);
        assert_eq!(
            canonicalize(&d, false, None, None),
            r#"<doc><e1></e1><e2></e2><e3 id="elem3" name="elem3"></e3><e5 xmlns="http://example.org" xmlns:a="http://www.w3.org" xmlns:b="http://www.ietf.org" attr="I'm" attr2="all" b:attr="sorted" a:attr="out"></e5><e6 xmlns:a="http://www.w3.org"><e7 xmlns="http://www.ietf.org"><e8 xmlns=""><e9 xmlns:a="http://www.ietf.org"></e9></e8></e7></e6></doc>"#
        );
    }

    #[test]
    fn pis_and_comments_outside_the_document_element() {
        let d = doc("<?xml version=\"1.0\"?>\n<?xml-stylesheet href=\"doc.xsl\"\n   type=\"text/xsl\"   ?>\n<doc>Hello, world!<!-- Comment 1 --></doc>\n<?pi-without-data     ?>\n<!-- Comment 2 -->");
        assert_eq!(canonicalize(&d, false, None, None), "<?xml-stylesheet href=\"doc.xsl\"\n   type=\"text/xsl\"   ?>\n<doc>Hello, world!</doc>\n<?pi-without-data?>");
    }
}
