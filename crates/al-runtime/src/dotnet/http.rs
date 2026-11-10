//! AL HttpClient, HttpRequestMessage, HttpResponseMessage, HttpContent, HttpHeaders, Cookie and
//! the test types TestHttpRequestMessage / TestHttpResponseMessage.
//!
//! Sending a request:
//! - under the test runner, the test's `[HttpClientHandler]` handler (named in
//!   `[HandlerFunctions]`) gets a TestHttpRequestMessage and fills a TestHttpResponseMessage;
//!   returning false uses that mocked response. Tests never reach the network: a request the
//!   handler does not mock (or with no handler) fails as blocked by the TestHttpRequestPolicy.
//! - otherwise requests are blocked by the environment (IsBlockedByEnvironment) unless the
//!   setting `BC_RUST_HTTP=allow` is set, in which case they are sent with ureq (rustls).

use super::*;
use crate::value::slot;

/// Header collection; `kind` decides which header names are valid (.NET's request vs content headers).
#[derive(Default)]
pub struct Headers {
    pub items: Vec<(String, Vec<String>)>,
    pub kind: HeaderKind,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum HeaderKind {
    #[default]
    Request,
    Content,
    Response,
}

pub struct Content {
    pub body: Vec<u8>,
    pub headers: NetRef,
    pub secret: bool,
}

impl Default for Content {
    fn default() -> Self {
        Content { body: vec![], headers: headers_ref(HeaderKind::Content), secret: false }
    }
}

pub struct Client {
    pub base: String,
    pub headers: NetRef,
    pub timeout_ms: i64,
    pub response_cookies: bool,
}

impl Default for Client {
    fn default() -> Self {
        Client { base: String::new(), headers: headers_ref(HeaderKind::Request), timeout_ms: 100_000, response_cookies: false }
    }
}

pub struct Request {
    pub method: String,
    pub uri: String,
    pub secret_uri: bool,
    pub content: NetRef,
    pub headers: NetRef,
    pub cookies: Vec<(String, String)>,
}

impl Request {
    pub fn new() -> Request {
        Request { method: "GET".into(), uri: String::new(), secret_uri: false, content: content_ref(), headers: headers_ref(HeaderKind::Request), cookies: vec![] }
    }
}

impl Default for Request {
    fn default() -> Self {
        Request::new()
    }
}

pub struct Response {
    pub status: i32,
    pub reason: String,
    pub content: NetRef,
    pub headers: NetRef,
    pub blocked: bool,
    /// TestHttpResponseMessage.IsSuccessfulRequest (None = derived from the status).
    pub success: Option<bool>,
    pub cookies: Vec<(String, String)>,
}

impl Response {
    pub fn new() -> Response {
        Response { status: 0, reason: String::new(), content: content_ref(), headers: headers_ref(HeaderKind::Response), blocked: false, success: None, cookies: vec![] }
    }
}

impl Default for Response {
    fn default() -> Self {
        Response::new()
    }
}

#[derive(Default)]
pub struct TestRequest {
    pub path: String,
    pub method: String,
    pub query: Vec<(String, String)>,
    pub content: Option<NetRef>,
    pub headers: Option<NetRef>,
    pub secret_uri: bool,
}

fn headers_ref(kind: HeaderKind) -> NetRef {
    netref(NetObj::HttpHeaders(Headers { items: vec![], kind }))
}

fn content_ref() -> NetRef {
    netref(NetObj::HttpContent(Content::default()))
}

const CONTENT_HEADERS: &[&str] = &[
    "allow",
    "content-disposition",
    "content-encoding",
    "content-language",
    "content-length",
    "content-location",
    "content-md5",
    "content-range",
    "content-type",
    "expires",
    "last-modified",
];

/// Request/response header names that .NET rejects in a content header collection.
const REQUEST_ONLY_HEADERS: &[&str] = &[
    "accept",
    "accept-charset",
    "accept-encoding",
    "accept-language",
    "authorization",
    "cache-control",
    "connection",
    "cookie",
    "date",
    "expect",
    "from",
    "host",
    "if-match",
    "if-modified-since",
    "if-none-match",
    "if-range",
    "if-unmodified-since",
    "max-forwards",
    "pragma",
    "proxy-authorization",
    "range",
    "referer",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "user-agent",
    "via",
    "warning",
];

const SINGLE_VALUE: &[&str] = &["content-type", "content-length", "content-md5", "content-range", "content-location", "expires", "last-modified", "host", "authorization", "date", "from", "if-modified-since", "if-unmodified-since", "max-forwards", "referer", "user-agent"];

pub fn reason_phrase(status: i32) -> &'static str {
    match status {
        100 => "Continue",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        412 => "Precondition Failed",
        413 => "Request Entity Too Large",
        415 => "Unsupported Media Type",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// Values of a header collection (empty when the NetRef is not one).
fn header_items(h: &NetRef) -> Vec<(String, Vec<String>)> {
    match &*h.borrow() {
        NetObj::HttpHeaders(x) => x.items.clone(),
        _ => vec![],
    }
}

fn header_get(h: &NetRef, name: &str) -> Option<Vec<String>> {
    header_items(h).into_iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)
}

/// Write the values into a `var Values: array of Text` / `List of [Text]` argument.
fn fill_values(target: Option<&Val>, values: &[String]) -> Option<Val> {
    match target {
        Some(Val::List(l)) => {
            let mut lb = l.borrow_mut();
            lb.clear();
            lb.extend(values.iter().cloned().map(Val::Text));
            None
        }
        Some(Val::Array(a, _)) => {
            let mut ab = a.borrow_mut();
            for (i, e) in ab.iter_mut().enumerate() {
                *e = Val::Text(values.get(i).cloned().unwrap_or_default());
            }
            None
        }
        _ => Some(Val::List(Rc::new(std::cell::RefCell::new(values.iter().cloned().map(Val::Text).collect())))),
    }
}

fn arg_ref(a: &Args, i: usize) -> Option<NetRef> {
    a.net(i)
}

/// Body bytes of a WriteFrom argument (Text, InStream, Blob, ...).
fn body_of(v: Option<&Val>) -> Vec<u8> {
    match v {
        Some(Val::InStream(s)) => {
            let mut st = s.borrow_mut();
            let r = st.data[st.pos.min(st.data.len())..].to_vec();
            st.pos = st.data.len();
            r
        }
        Some(v @ Val::Net(_)) => super::text::arg_bytes(Some(v)),
        Some(v) => v.to_text().into_bytes(),
        None => vec![],
    }
}

/// HttpRequestType option value for a method name (the platform option has no catalog entry).
fn request_type(method: &str) -> Val {
    Val::Opt { ord: crate::interp::system_option_ord("httprequesttype", &method.to_ascii_lowercase()), enum_id: None }
}

fn url_path_and_query(url: &str) -> (String, Vec<(String, String)>) {
    let no_frag = url.split('#').next().unwrap_or(url);
    match no_frag.split_once('?') {
        Some((p, q)) => {
            let qs = q
                .split('&')
                .filter(|x| !x.is_empty())
                .map(|kv| match kv.split_once('=') {
                    Some((k, v)) => (super::uri::url_decode(k), super::uri::url_decode(v)),
                    None => (super::uri::url_decode(kv), String::new()),
                })
                .collect();
            (p.to_string(), qs)
        }
        None => (no_frag.to_string(), vec![]),
    }
}

/// The real network: only with `BC_RUST_HTTP=allow` and outside tests.
fn network_allowed() -> bool {
    std::env::var("BC_RUST_HTTP").map(|v| matches!(v.to_ascii_lowercase().as_str(), "allow" | "1" | "true" | "on")).unwrap_or(false)
}

struct Wire {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    timeout_ms: i64,
}

fn send_real(w: &Wire) -> Result<Response, String> {
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_millis(w.timeout_ms.max(1) as u64)).redirects(5).build();
    let mut req = agent.request(&w.method, &w.url);
    for (k, v) in &w.headers {
        req = req.set(k, v);
    }
    let res = if w.body.is_empty() && matches!(w.method.as_str(), "GET" | "HEAD" | "DELETE" | "OPTIONS") { req.call() } else { req.send_bytes(&w.body) };
    let r = match res {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(format!("An error occurred while sending the request. {e}")),
    };
    let mut resp = Response::new();
    resp.status = r.status() as i32;
    resp.reason = r.status_text().to_string();
    let mut rh = Headers { items: vec![], kind: HeaderKind::Response };
    let mut ch = Headers { items: vec![], kind: HeaderKind::Content };
    for name in r.headers_names() {
        let vals: Vec<String> = r.all(&name).into_iter().map(String::from).collect();
        if CONTENT_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            ch.items.push((name.clone(), vals));
        } else {
            rh.items.push((name.clone(), vals));
        }
    }
    let mut body = vec![];
    use std::io::Read;
    r.into_reader().take(256 * 1024 * 1024).read_to_end(&mut body).map_err(|e| e.to_string())?;
    resp.headers = netref(NetObj::HttpHeaders(rh));
    resp.content = netref(NetObj::HttpContent(Content { body, headers: netref(NetObj::HttpHeaders(ch)), secret: false }));
    Ok(resp)
}

impl<'r> Session<'r> {
    /// The running test's [HttpClientHandler] procedure (marked as called).
    fn http_handler(&mut self) -> Option<(al_model::ProcRef, Rc<crate::interp::Instance>)> {
        let cat = self.cat;
        let t = self.test.as_mut()?;
        let h = t.handlers.iter_mut().find(|h| cat.proc(h.proc).attributes.iter().any(|a| a.name.eq_ignore_ascii_case("httpclienthandler")))?;
        h.called = true;
        Some((h.proc, t.inst.clone()))
    }

    /// Send a request; Ok(Err(msg)) is a failed send (Send returns false / raises when discarded).
    fn http_send(&mut self, client: &NetRef, req: &NetRef) -> R<Result<Response, (String, bool)>> {
        let (base, def_headers, timeout) = match &*client.borrow() {
            NetObj::HttpClient(c) => (c.base.clone(), c.headers.clone(), c.timeout_ms),
            _ => (String::new(), headers_ref(HeaderKind::Request), 100_000),
        };
        let (method, uri, secret_uri, content, rheaders) = match &*req.borrow() {
            NetObj::HttpRequest(r) => (r.method.clone(), r.uri.clone(), r.secret_uri, r.content.clone(), r.headers.clone()),
            _ => return Ok(Err(("The request message is not initialized.".into(), false))),
        };
        let url = if uri.contains("://") || base.is_empty() {
            uri.clone()
        } else {
            match super::uri::NetUri::parse_absolute(&base).and_then(|b| b.join(&uri)) {
                Ok(u) => u.absolute_uri(),
                Err(_) => format!("{}{}", base, uri),
            }
        };
        if url.is_empty() || !url.contains("://") {
            return Ok(Err(("An invalid request URI was provided. Either the request URI must be an absolute URI or BaseAddress must be set.".into(), false)));
        }
        if self.test.is_some() {
            if let Some((pr, inst)) = self.http_handler() {
                let (path, query) = url_path_and_query(&url);
                let mut all_headers = Headers { items: header_items(&def_headers), kind: HeaderKind::Request };
                for (k, v) in header_items(&rheaders) {
                    match all_headers.items.iter_mut().find(|(x, _)| x.eq_ignore_ascii_case(&k)) {
                        Some(e) => e.1 = v,
                        None => all_headers.items.push((k, v)),
                    }
                }
                let treq = net(NetObj::TestHttpRequest(TestRequest {
                    path,
                    method: method.clone(),
                    query,
                    content: Some(content.clone()),
                    headers: Some(netref(NetObj::HttpHeaders(all_headers))),
                    secret_uri,
                }));
                let resp_slot = slot(net(NetObj::HttpResponse(Response::new())));
                let r = self.invoke(pr, vec![Arg::Val(treq), Arg::Ref(LVal::Slot(resp_slot.clone(), None))], Some(inst), None, None)?;
                if !r.as_bool() {
                    let v = resp_slot.borrow().clone();
                    if let Val::Net(n) = v {
                        let mut resp = std::mem::take(match &mut *n.borrow_mut() {
                            NetObj::HttpResponse(x) => x,
                            _ => return Ok(Err(("The mocked response is not a TestHttpResponseMessage.".into(), false))),
                        });
                        if resp.reason.is_empty() {
                            resp.reason = reason_phrase(resp.status).to_string();
                        }
                        return Ok(Ok(resp));
                    }
                }
            }
            return err(format!(
                "The outbound HTTP request to '{}' was blocked: the test codeunit's TestHttpRequestPolicy does not allow it and no HttpClientHandler mocked it.",
                if secret_uri { "<secret uri>".to_string() } else { url }
            ));
        }
        if !network_allowed() {
            let mut resp = Response::new();
            resp.blocked = true;
            return Ok(Err((format!("The request to '{}' was blocked by the environment. Outbound HTTP requests are disabled on this server (set BC_RUST_HTTP=allow to enable them).", if secret_uri { "<secret uri>".to_string() } else { url }), true)));
        }
        let mut headers: Vec<(String, String)> = vec![];
        for (k, v) in header_items(&def_headers).into_iter().chain(header_items(&rheaders)) {
            headers.retain(|(x, _)| !x.eq_ignore_ascii_case(&k));
            headers.push((k, v.join(", ")));
        }
        let (body, cheaders) = match &*content.borrow() {
            NetObj::HttpContent(c) => (c.body.clone(), c.headers.clone()),
            _ => (vec![], headers_ref(HeaderKind::Content)),
        };
        if !body.is_empty() || matches!(method.as_str(), "POST" | "PUT" | "PATCH") {
            for (k, v) in header_items(&cheaders) {
                headers.push((k, v.join(", ")));
            }
        }
        let wire = Wire { method, url, headers, body, timeout_ms: timeout };
        Ok(send_real(&wire).map_err(|e| (e, false)))
    }

    /// Store a send result into the `var Response` argument and return Send's Boolean.
    fn http_finish(&mut self, f: &mut Frame, a: &Args, out_i: usize, r: Result<Response, (String, bool)>, discard: bool) -> R<Out> {
        match r {
            Ok(resp) => {
                self.net_out(f, a, out_i, net(NetObj::HttpResponse(resp)))?;
                ok(Val::Bool(true))
            }
            Err((msg, blocked)) => {
                let mut resp = Response::new();
                resp.blocked = blocked;
                self.net_out(f, a, out_i, net(NetObj::HttpResponse(resp)))?;
                if discard {
                    return err(msg);
                }
                self.last_error = msg;
                ok(Val::Bool(false))
            }
        }
    }

    pub fn http_call(&mut self, f: &mut Frame, n: &NetRef, m: &str, a: &Args, discard: bool) -> R<Out> {
        let kind = match &*n.borrow() {
            NetObj::HttpClient(_) => 0,
            NetObj::HttpRequest(_) => 1,
            NetObj::HttpResponse(_) => 2,
            NetObj::HttpContent(_) => 3,
            NetObj::HttpHeaders(_) => 4,
            NetObj::TestHttpRequest(_) => 5,
            _ => 6,
        };
        match kind {
            0 => self.http_client_call(f, n, m, a, discard),
            1 => self.http_request_call(f, n, m, a),
            2 => http_response_call(self, f, n, m, a),
            3 => self.http_content_call(f, n, m, a),
            4 => self.http_headers_call(f, n, m, a, discard),
            5 => {
                let o = n.borrow();
                let NetObj::TestHttpRequest(t) = &*o else { return Ok(Out::Unknown) };
                ok(match m {
                    "path" => Val::Text(t.path.clone()),
                    "requesttype" => request_type(&t.method),
                    "queryparameters" => Val::Dict(Rc::new(std::cell::RefCell::new(t.query.iter().map(|(k, v)| (Val::Text(k.clone()), Val::Text(v.clone()))).collect()))),
                    "content" => Val::Net(t.content.clone().unwrap_or_else(content_ref)),
                    "headers" => Val::Net(t.headers.clone().unwrap_or_else(|| headers_ref(HeaderKind::Request))),
                    "hassecreturi" => Val::Bool(t.secret_uri),
                    _ => return Ok(Out::Unknown),
                })
            }
            _ => {
                let mut o = n.borrow_mut();
                let NetObj::Cookie(name, value) = &mut *o else { return Ok(Out::Unknown) };
                ok(match m {
                    "name" if a.len() == 1 => {
                        *name = a.text(0);
                        Val::Void
                    }
                    "name" => Val::Text(name.clone()),
                    "value" if a.len() == 1 => {
                        *value = a.text(0);
                        Val::Void
                    }
                    "value" => Val::Text(value.clone()),
                    "domain" | "path" => Val::Text(String::new()),
                    "expires" => Val::DateTime(bc_store::DATETIME_UNDEFINED),
                    "httponly" | "secure" => Val::Bool(false),
                    _ => return Ok(Out::Unknown),
                })
            }
        }
    }

    fn http_client_call(&mut self, f: &mut Frame, n: &NetRef, m: &str, a: &Args, discard: bool) -> R<Out> {
        let send_with = |s: &mut Session, method: &str, url: String, content: Option<NetRef>| -> NetRef {
            let _ = s;
            let mut r = Request::new();
            r.method = method.to_string();
            r.uri = url;
            if let Some(c) = content {
                r.content = c;
            }
            netref(NetObj::HttpRequest(r))
        };
        match m {
            "send" => {
                let Some(req) = arg_ref(a, 0) else { return err("HttpClient.Send requires an HttpRequestMessage") };
                let r = self.http_send(n, &req)?;
                return self.http_finish(f, a, 1, r, discard);
            }
            "get" | "delete" => {
                let req = send_with(self, &m.to_ascii_uppercase(), a.text(0), None);
                let r = self.http_send(n, &req)?;
                return self.http_finish(f, a, 1, r, discard);
            }
            "post" | "put" | "patch" => {
                let req = send_with(self, &m.to_ascii_uppercase(), a.text(0), arg_ref(a, 1));
                let r = self.http_send(n, &req)?;
                return self.http_finish(f, a, 2, r, discard);
            }
            _ => {}
        }
        let mut o = n.borrow_mut();
        let NetObj::HttpClient(c) = &mut *o else { return Ok(Out::Unknown) };
        ok(match m {
            "clear" => {
                *c = Client::default();
                Val::Void
            }
            "defaultrequestheaders" => Val::Net(c.headers.clone()),
            "timeout" if a.len() == 1 => {
                c.timeout_ms = match a.get(0) {
                    Val::Duration(d) => d,
                    v => v.as_i64(),
                };
                Val::Void
            }
            "timeout" => Val::Duration(c.timeout_ms),
            "setbaseaddress" => {
                let b = a.text(0);
                let parsed = super::uri::NetUri::parse_absolute(&b);
                let okb = b.is_empty() || parsed.is_ok();
                if okb {
                    // HttpClient.BaseAddress is a Uri: normalized, with the root path
                    c.base = parsed.map(|u| u.absolute_uri()).unwrap_or_default();
                }
                Val::Bool(okb)
            }
            "getbaseaddress" => Val::Text(c.base.clone()),
            "useresponsecookies" => {
                c.response_cookies = a.is_empty() || a.bool(0);
                Val::Bool(true)
            }
            "addcertificate" | "usedefaultnetworkwindowsauthentication" | "usewindowsauthentication" | "useservercertificatevalidation" | "allowedmaxresponsecontentsize"
            | "setmaxresponsecontentsize" => Val::Bool(true),
            _ => return Ok(Out::Unknown),
        })
    }

    fn http_request_call(&mut self, f: &mut Frame, n: &NetRef, m: &str, a: &Args) -> R<Out> {
        let mut o = n.borrow_mut();
        let NetObj::HttpRequest(r) = &mut *o else { return Ok(Out::Unknown) };
        let set = a.len() == 1;
        let v = match m {
            "method" if set => {
                r.method = a.text(0).to_ascii_uppercase();
                Val::Void
            }
            "method" => Val::Text(r.method.clone()),
            "setrequesturi" | "setsecretrequesturi" => {
                let u = a.text(0);
                let okv = u.is_empty() || super::uri::NetUri::parse_absolute(&u).is_ok() || u.starts_with('/') || !u.contains(' ');
                if okv {
                    r.uri = u;
                    r.secret_uri = m == "setsecretrequesturi";
                }
                Val::Bool(okv)
            }
            "getrequesturi" => Val::Text(if r.secret_uri { String::new() } else { r.uri.clone() }),
            "getsecretrequesturi" => Val::Text(r.uri.clone()),
            "hassecreturi" => Val::Bool(r.secret_uri),
            "content" if set => {
                match a.net(0) {
                    Some(c) if matches!(&*c.borrow(), NetObj::HttpContent(_)) => r.content = c,
                    _ => r.content = content_ref(),
                }
                Val::Void
            }
            "content" => Val::Net(r.content.clone()),
            "getheaders" => {
                let h = r.headers.clone();
                drop(o);
                self.net_out(f, a, 0, Val::Net(h))?;
                return ok(Val::Bool(true));
            }
            "setcookie" => {
                let (k, v) = match a.net(0) {
                    Some(c) => match &*c.borrow() {
                        NetObj::Cookie(k, v) => (k.clone(), v.clone()),
                        _ => (a.text(0), a.text(1)),
                    },
                    None => (a.text(0), a.text(1)),
                };
                r.cookies.retain(|(x, _)| x != &k);
                r.cookies.push((k, v));
                Val::Bool(true)
            }
            "getcookie" => {
                let c = r.cookies.iter().find(|(k, _)| *k == a.text(0)).cloned();
                drop(o);
                let found = c.is_some();
                if let Some((k, v)) = c {
                    self.net_out(f, a, 1, net(NetObj::Cookie(k, v)))?;
                }
                return ok(Val::Bool(found));
            }
            "getcookienames" => Val::List(Rc::new(std::cell::RefCell::new(r.cookies.iter().map(|(k, _)| Val::Text(k.clone())).collect()))),
            "removecookie" => {
                let k = a.text(0);
                let had = r.cookies.iter().any(|(x, _)| *x == k);
                r.cookies.retain(|(x, _)| *x != k);
                Val::Bool(had)
            }
            _ => return Ok(Out::Unknown),
        };
        ok(v)
    }

    fn http_content_call(&mut self, f: &mut Frame, n: &NetRef, m: &str, a: &Args) -> R<Out> {
        match m {
            "writefrom" => {
                let body = body_of(a.raw(0));
                let is_text = !matches!(a.raw(0), Some(Val::InStream(_)));
                let mut o = n.borrow_mut();
                let NetObj::HttpContent(c) = &mut *o else { return Ok(Out::Unknown) };
                c.body = body;
                // a new body resets the headers; text gets StringContent's default content type
                c.headers = headers_ref(HeaderKind::Content);
                if is_text {
                    if let NetObj::HttpHeaders(h) = &mut *c.headers.borrow_mut() {
                        h.items.push(("Content-Type".into(), vec!["text/plain; charset=utf-8".into()]));
                    }
                }
                return void();
            }
            "readas" => {
                let (body, cs) = match &*n.borrow() {
                    NetObj::HttpContent(c) => (c.body.clone(), header_get(&c.headers, "Content-Type").and_then(|v| v.first().cloned()).unwrap_or_default()),
                    _ => return Ok(Out::Unknown),
                };
                let target_stream = matches!(a.raw(0), Some(Val::InStream(_)) | Some(Val::Opaque("InStream")));
                let v = if target_stream {
                    crate::streams::in_stream(body)
                } else {
                    let cp = if cs.to_ascii_lowercase().contains("utf-16") { 1200 } else if cs.to_ascii_lowercase().contains("iso-8859-1") { 28591 } else { 65001 };
                    Val::Text(super::text::decode(cp, &body))
                };
                self.net_out(f, a, 0, v)?;
                return ok(Val::Bool(true));
            }
            "getheaders" => {
                let h = match &*n.borrow() {
                    NetObj::HttpContent(c) => c.headers.clone(),
                    _ => return Ok(Out::Unknown),
                };
                self.net_out(f, a, 0, Val::Net(h))?;
                return ok(Val::Bool(true));
            }
            _ => {}
        }
        let mut o = n.borrow_mut();
        let NetObj::HttpContent(c) = &mut *o else { return Ok(Out::Unknown) };
        ok(match m {
            "clear" => {
                *c = Content::default();
                Val::Void
            }
            "issecretcontent" => Val::Bool(c.secret),
            _ => return Ok(Out::Unknown),
        })
    }

    fn http_headers_call(&mut self, f: &mut Frame, n: &NetRef, m: &str, a: &Args, discard: bool) -> R<Out> {
        let mut o = n.borrow_mut();
        let NetObj::HttpHeaders(h) = &mut *o else { return Ok(Out::Unknown) };
        let name = a.text(0);
        let lname = name.to_ascii_lowercase();
        let pos = h.items.iter().position(|(k, _)| k.eq_ignore_ascii_case(&name));
        match m {
            "add" | "tryaddwithoutvalidation" => {
                let value = a.text(1);
                if m == "add" {
                    let bad = name.is_empty()
                        || name.chars().any(|c| c.is_whitespace() || "()<>@,;:\\\"/[]?={}".contains(c))
                        || (h.kind == HeaderKind::Request && CONTENT_HEADERS.contains(&lname.as_str()))
                        || (h.kind == HeaderKind::Content && REQUEST_ONLY_HEADERS.contains(&lname.as_str()))
                        || (pos.is_some() && SINGLE_VALUE.contains(&lname.as_str()));
                    if bad {
                        drop(o);
                        let msg = if pos.is_some() && SINGLE_VALUE.contains(&lname.as_str()) {
                            format!("Cannot add value because header '{name}' does not support multiple values.")
                        } else {
                            format!("Misused header name, '{name}'. Make sure request headers are used with HttpRequestMessage, response headers with HttpResponseMessage, and content headers with HttpContent objects.")
                        };
                        if discard {
                            return err(msg);
                        }
                        self.last_error = msg;
                        return ok(Val::Bool(false));
                    }
                }
                match pos {
                    Some(i) => h.items[i].1.push(value),
                    None => h.items.push((name, vec![value])),
                }
                ok(Val::Bool(true))
            }
            "contains" => ok(Val::Bool(pos.is_some())),
            "containssecret" => ok(Val::Bool(false)),
            "remove" => {
                if let Some(i) = pos {
                    h.items.remove(i);
                }
                ok(Val::Bool(pos.is_some()))
            }
            "clear" => {
                h.items.clear();
                void()
            }
            "keys" => ok(Val::List(Rc::new(std::cell::RefCell::new(h.items.iter().map(|(k, _)| Val::Text(k.clone())).collect())))),
            "getvalues" | "getsecretvalues" => {
                let vals = pos.map(|i| h.items[i].1.clone());
                drop(o);
                let found = vals.is_some();
                let vals = vals.unwrap_or_default();
                if let Some(v) = fill_values(a.raw(1), &vals) {
                    self.net_out(f, a, 1, v)?;
                }
                ok(Val::Bool(found))
            }
            _ => Ok(Out::Unknown),
        }
    }
}

fn http_response_call(s: &mut Session, f: &mut Frame, n: &NetRef, m: &str, a: &Args) -> R<Out> {
    let mut o = n.borrow_mut();
    let NetObj::HttpResponse(r) = &mut *o else { return Ok(Out::Unknown) };
    let set = a.len() == 1;
    let v = match m {
        "httpstatuscode" if set => {
            r.status = a.int(0) as i32;
            Val::Void
        }
        "httpstatuscode" => Val::Int(r.status),
        "reasonphrase" if set => {
            r.reason = a.text(0);
            Val::Void
        }
        "reasonphrase" => Val::Text(if r.reason.is_empty() { reason_phrase(r.status).to_string() } else { r.reason.clone() }),
        "issuccessstatuscode" | "issuccessfulrequest" if set => {
            r.success = Some(a.bool(0));
            Val::Void
        }
        "issuccessstatuscode" | "issuccessfulrequest" => Val::Bool(r.success.unwrap_or((200..300).contains(&r.status))),
        "isblockedbyenvironment" if set => {
            r.blocked = a.bool(0);
            Val::Void
        }
        "isblockedbyenvironment" => Val::Bool(r.blocked),
        "content" if set => {
            if let Some(c) = a.net(0) {
                r.content = c;
            }
            Val::Void
        }
        "content" => Val::Net(r.content.clone()),
        "headers" => Val::Net(r.headers.clone()),
        "getcookienames" => Val::List(Rc::new(std::cell::RefCell::new(r.cookies.iter().map(|(k, _)| Val::Text(k.clone())).collect()))),
        "getcookie" => {
            let c = r.cookies.iter().find(|(k, _)| *k == a.text(0)).cloned();
            drop(o);
            let found = c.is_some();
            if let Some((k, v)) = c {
                s.net_out(f, a, 1, net(NetObj::Cookie(k, v)))?;
            }
            return ok(Val::Bool(found));
        }
        _ => return Ok(Out::Unknown),
    };
    ok(v)
}
