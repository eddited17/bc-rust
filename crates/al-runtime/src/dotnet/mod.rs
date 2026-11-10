//! Native emulation of the platform types AL reaches through DotNet interop, and of the AL
//! built-in Xml* and Http* types. No .NET is involved: every type is implemented in Rust with the
//! .NET semantics the AL code depends on.
//!
//! All these values are `Val::Net(NetRef)`: a shared, mutable object (reference semantics, like
//! .NET objects and the AL Xml/Http types). A DotNet variable that holds no object is
//! `NetObj::Null(type)`, so calls on it can still reach static members and constructors
//! (`Regex := Regex.Regex(Pattern)`, `Uri.EscapeDataString(x)`).
//!
//! DotNet types without an emulation keep the old behaviour (`Val::Opaque("DotNet")`, calls are
//! logged and return neutral values), so platform helper types keep working as before.

pub mod asym;
pub mod cipher;
pub mod collections;
pub mod crypto;
pub mod culture;
pub mod data;
pub mod datetime;
pub mod doclayout;
pub mod drawing;
pub mod graph;
pub mod nav;
pub mod http;
pub mod newtonsoft;
pub mod oxml;
pub mod signedxml;
pub mod regex;
pub mod text;
pub mod uri;
pub mod xml;
pub mod xmlapi;
pub mod xmlreader;
pub mod xmlwriter;
pub mod xlsx;
pub mod xpath;
pub mod x509;
pub mod zip;

use crate::interp::*;
use crate::value::*;
use al_syntax::Expr;
use std::cell::RefCell;
use std::rc::Rc;

pub type NetRef = Rc<RefCell<NetObj>>;

pub fn net(o: NetObj) -> Val {
    Val::Net(Rc::new(RefCell::new(o)))
}

pub fn netref(o: NetObj) -> NetRef {
    Rc::new(RefCell::new(o))
}

pub enum NetObj {
    // ------------------------------------------------------------ .NET
    /// A DotNet variable without an object, with its declared .NET type.
    Null(&'static str),
    /// Boxed AL value with a .NET type: String, Int32, Boolean, Decimal, Char, enums (RegexOptions, ...).
    Prim(&'static str, Val),
    DateTime(datetime::NetDateTime),
    /// UTC ticks and the offset in minutes.
    DateTimeOffset(i64, i32),
    /// Ticks (100 ns).
    TimeSpan(i64),
    TimeZone(Rc<datetime::Zone>),
    Culture(&'static culture::Culture, culture::View),
    Regex(Rc<regex::NetRegex>),
    MatchCollection(Rc<Vec<Rc<regex::MatchData>>>),
    Match(Rc<regex::MatchData>),
    GroupCollection(Rc<regex::MatchData>),
    Group(Rc<regex::MatchData>, usize),
    CaptureCollection(Rc<regex::MatchData>, usize),
    Capture(i32, i32, String),
    Uri(Rc<uri::NetUri>),
    UriBuilder(uri::Builder),
    NameValueCollection(Vec<(Option<String>, Vec<String>)>),
    StringBuilder(String),
    /// Code page (65001 UTF-8, 1200 UTF-16 LE, 1201 UTF-16 BE, 20127 ASCII, 12000 UTF-32, 1252, ...).
    Encoding(i32, bool),
    /// .NET array: element type and items.
    Array(&'static str, Vec<Val>),
    /// List`1, ArrayList, Queue, Stack (the type name tells them apart).
    List(&'static str, Vec<Val>),
    /// Dictionary`2, Hashtable, SortedDictionary`2, HashSet`1 (values ignored).
    Dict(&'static str, Vec<(Val, Val)>),
    KeyValue(Val, Val),
    Enumerator(Vec<Val>, isize),
    Version(Vec<i32>),
    /// MemoryStream / Stream: bytes and position.
    Stream(Vec<u8>, usize),
    /// StringReader / StreamReader / XmlTextReader over text; position in chars.
    Reader(&'static str, Rc<str>, usize),
    /// StringWriter target text.
    Writer(&'static str, String),
    /// StreamWriter over a stream (MemoryStream or AL OutStream).
    StreamWriter(Val),
    /// StreamWriter with a non-UTF-8 encoding: target and code page.
    EncodedWriter(Val, i32),
    Exception(String),
    /// A typed exception (GetLastErrorObject): type, Message, InnerException.
    ExceptionOf(&'static str, String, Option<Val>),
    XmlReaderSettings,
    /// XmlTextWriter / XmlWriter.
    XmlWriter(Box<xmlwriter::XmlWriterState>),
    /// XmlReader / XmlTextReader (pull reader).
    XReader(Box<xmlreader::XmlReaderState>),
    // ------------------------------------------------------------ XML (DotNet System.Xml and AL Xml*)
    /// A node; `al` is true for the AL Xml* types (XmlDocument, XmlElement, XmlNode, ...).
    XNode(xml::Node, bool),
    XNodeList(Vec<xml::Node>, bool),
    /// Attribute collection of an element.
    XAttrs(xml::Node, bool),
    XNsMgr(Rc<RefCell<Vec<(String, String)>>>, bool),
    XNameTable(bool),
    XReadOptions(bool),
    XWriteOptions(bool),
    // ------------------------------------------------------------ HTTP (AL Http*)
    HttpClient(http::Client),
    HttpRequest(http::Request),
    HttpResponse(http::Response),
    HttpContent(http::Content),
    HttpHeaders(http::Headers),
    TestHttpRequest(http::TestRequest),
    Cookie(String, String),
    // ------------------------------------------------------------ Newtonsoft.Json.Linq
    /// JObject / JArray / JValue: a node of the crate::json tree.
    JTok(crate::json::JRef),
    /// JProperty: the object holding it and its name.
    JProp(crate::json::JRef, String),
    // ------------------------------------------------------------ System.Drawing, System.Net.Mail
    Bitmap(drawing::Bitmap),
    /// An image loaded with Image.FromStream: the encoded bytes, size and format.
    Image(Rc<drawing::ImageData>),
    /// Graphics drawing on a Bitmap.
    Graphics(NetRef),
    /// ARGB.
    Color(u32),
    Brush(u32),
    /// MailAddress: address and display name.
    MailAddress(String, String),
    // ------------------------------------------------------------ System.Data, Nav business chart
    DataTable(data::Table),
    /// Columns / Rows collection views of a DataTable.
    DataColumns(NetRef),
    DataRows(NetRef),
    /// System.Data.DataSet: its tables (DataTable objects) and name; Tables is DataTables(the set).
    DataSet(data::DsData),
    DataTables(NetRef),
    /// OpenXml SDK elements, parts and workbook/worksheet views (oxml.rs).
    Ox(oxml::Ox),
    /// System.Security.Cryptography.Xml (signedxml.rs)
    Sx(signedxml::Sx),
    /// Row values by column name.
    DataRow(Vec<(String, Val)>),
    DataColumn(data::Column),
    ChartData(data::ChartData),
    /// Stopwatch: start instant while running, accumulated elapsed time.
    Stopwatch(Option<std::time::Instant>, std::time::Duration),
    /// BinaryWriter over a stream (MemoryStream or AL OutStream).
    BinaryWriter(Val),
    /// BinaryReader over a stream (MemoryStream or AL InStream).
    BinaryReader(Val),
    /// Newtonsoft JsonTextReader: the document's tokens, read one by one (newtonsoft.rs).
    JReader(Box<newtonsoft::JReader>),
    /// An AL InStream seen as a .NET System.IO.Stream (BinaryReader.BaseStream): 0-based Position.
    InStreamView(Rc<RefCell<crate::streams::InStreamState>>),
    // ------------------------------------------------------------ Azure AD Graph client (graph.rs)
    /// Property bag: UserInfo, GroupInfo, ServicePlanInfo, SkuInfo, RoleInfo, TenantInfo, ...
    Bag(&'static str, Vec<(String, Val)>),
    GraphMock(Box<graph::Directory>),
    GraphPage(graph::Page),
    // ------------------------------------------------------------ service-tier helpers (nav.rs)
    /// MockAzureKeyVaultSecretProvider: secrets and certificates by name.
    KeyVaultMock(Vec<(String, String)>, Vec<(String, String)>),
    /// AzureKeyVaultClientHelper with its secret provider.
    KeyVaultClient(Option<NetRef>),
    /// MemoryMappedFile: name and the shared buffer.
    Mmf(String, nav::Buffer),
    MmfView(nav::Buffer, usize),
    /// HashAlgorithm / KeyedHashAlgorithm: algorithm and HMAC key.
    Hasher(&'static str, Option<Vec<u8>>),
    /// Rfc2898DeriveBytes: PBKDF2 state (bytes already handed out).
    Pbkdf2 { alg: &'static str, password: Vec<u8>, salt: Vec<u8>, iterations: u32, pos: usize },
    /// RijndaelManaged / Aes (cipher.rs).
    Sym(Box<cipher::Sym>),
    /// ICryptoTransform: (settings, encrypt).
    Transform(Box<cipher::Sym>, bool),
    /// CryptoStream in write mode.
    CryptoStream(Box<cipher::CryptoW>),
    /// RSA / RSACryptoServiceProvider (asym.rs).
    Rsa(Box<asym::RsaState>),
    /// X509Certificate2 (x509.rs).
    Cert(Box<x509::CertState>),
    /// CertificateRequest (x509.rs).
    CertReq(Box<x509::CertReq>),
    /// ZipArchive / ZipArchiveEntry (zip.rs).
    Zip(Box<zip::ZipState>),
    ZipEntry(zip::Entry),
    /// OpenXml WorkbookReader / WorksheetReader (xlsx.rs).
    XlBook(Rc<xlsx::Book>),
    XlSheet(Rc<xlsx::Book>, usize),
    /// OpenXml WorkbookWriter / WorksheetWriter (xlsx.rs).
    XlWBook(xlsx::WRef),
    XlWSheet(xlsx::WRef, usize),
    // ------------------------------------------------------------ platform codeunits without AL source
    Base64Convert,
    /// Codeunit 2000000023 "MCP Utilities" (implemented by the service tier, no AL source).
    McpUtilities,
    // ------------------------------------------------------------ System.IO.Compression
    /// GZipStream (crate::gzip).
    GZip(Box<crate::gzip::GZip>),
}

// ---------------------------------------------------------------- type registry

/// .NET types with an emulation (DotNet variables of other types stay opaque shims).
const IMPLEMENTED: &[&str] = &[
    "System.String",
    "System.Char",
    "System.Int16",
    "System.Int32",
    "System.Int64",
    "System.UInt16",
    "System.UInt32",
    "System.UInt64",
    "System.Byte",
    "System.Decimal",
    "System.Double",
    "System.Single",
    "System.Boolean",
    "System.Object",
    "System.Guid",
    "System.DateTime",
    "System.DateTimeOffset",
    "System.DateTimeKind",
    "System.DayOfWeek",
    "System.TimeSpan",
    "System.TimeZoneInfo",
    "System.Globalization.CultureInfo",
    "System.Globalization.NumberFormatInfo",
    "System.Globalization.DateTimeFormatInfo",
    "System.Globalization.TextInfo",
    "System.Globalization.DateTimeStyles",
    "System.Globalization.NumberStyles",
    "System.Globalization.CompareOptions",
    "System.Text.RegularExpressions.Regex",
    "System.Text.RegularExpressions.RegexOptions",
    "System.Text.RegularExpressions.Match",
    "System.Text.RegularExpressions.MatchCollection",
    "System.Text.RegularExpressions.Group",
    "System.Text.RegularExpressions.GroupCollection",
    "System.Text.RegularExpressions.Capture",
    "System.Text.RegularExpressions.CaptureCollection",
    "System.Uri",
    "System.UriBuilder",
    "System.UriKind",
    "System.UriPartial",
    "System.UriComponents",
    "System.UriFormat",
    "System.Web.HttpUtility",
    "System.Net.WebUtility",
    "System.Collections.Specialized.NameValueCollection",
    "System.Convert",
    "System.Math",
    "System.BitConverter",
    "System.Environment",
    "System.Threading.Thread",
    "System.Drawing.Bitmap",
    "System.Drawing.Image",
    "System.Drawing.ImageFormatConverter",
    "System.Drawing.Graphics",
    "System.Drawing.Color",
    "System.Drawing.ColorTranslator",
    "System.Drawing.SolidBrush",
    "System.Drawing.Brush",
    "System.Drawing.Imaging.ImageFormat",
    "System.Drawing.Imaging.Encoder",
    "System.Drawing.Imaging.EncoderParameter",
    "System.Drawing.Imaging.EncoderParameters",
    "System.Drawing.Rectangle",
    "System.Net.Mail.MailAddress",
    "System.Diagnostics.Stopwatch",
    "Microsoft.Dynamics.Nav.LicensingService.Model.UserInfo",
    "Microsoft.Dynamics.Nav.LicensingService.Model.GroupInfo",
    "Microsoft.Dynamics.Nav.LicensingService.Model.ServicePlanInfo",
    "Microsoft.Dynamics.Nav.LicensingService.Model.SkuInfo",
    "Microsoft.Dynamics.Nav.LicensingService.Model.RoleInfo",
    "Microsoft.Dynamics.Nav.LicensingService.Model.TenantInfo",
    "Microsoft.Dynamics.Nav.LicensingService.Model.LicenseUnitsInfo",
    "Microsoft.Dynamics.Nav.LicensingService.Model.VerifiedDomainInfo",
    "Microsoft.Dynamics.Nav.AzureADGraphClient.MockGraphQuery",
    "Microsoft.Dynamics.Nav.AzureADGraphClient.GraphQuery",
    "Microsoft.Dynamics.Nav.AzureADGraphClient.UserInfoPage",
    "Microsoft.Dynamics.Nav.AzureADGraphClient.GroupInfoPage",
    "Microsoft.Dynamics.Nav.AzureADGraphClient.UserIdsPage",
    "Microsoft.Dynamics.Nav.CrmCustomizationHelper.CrmHelper",
    "Microsoft.Dynamics.Nav.Runtime.PermissionTestHelper",
    // LastError.Rethrow() (Job Queue Start Codeunit)
    "Microsoft.Dynamics.Nav.Runtime.LastError",
    // the Outlook add-in's command names and host types
    "Microsoft.Dynamics.Nav.Client.Hosts.OutlookCommand",
    "Microsoft.Dynamics.Nav.Client.Hosts.OfficeHostType",
    "Microsoft.Dynamics.Nav.NavUserAccount.NavUserAccountHelper",
    // crate::environment
    "Microsoft.Dynamics.Nav.NavUserAccount.NavTenantSettingsHelper",
    "Microsoft.Dynamics.Nav.Types.ALConfigSettings",
    "MockTest.MockAzureKeyVaultSecret.MockAzureKeyVaultSecretProvider",
    "Microsoft.Dynamics.Nav.AzureKeyVaultClient.AzureKeyVaultClientHelper",
    "Microsoft.Dynamics.Nav.Runtime.Encryption.IAzureKeyVaultSecretProvider",
    "System.IO.MemoryMappedFiles.MemoryMappedFile",
    "System.IO.MemoryMappedFiles.MemoryMappedViewStream",
    "System.IO.BinaryWriter",
    "System.IO.BinaryReader",
    "System.Type",
    "System.Data.DataTable",
    "System.Data.DataColumn",
    "System.Data.DataRow",
    "System.Data.DataColumnCollection",
    "System.Data.DataRowCollection",
    "System.Data.DataSet",
    "System.Data.DataTableCollection",
    "Microsoft.Dynamics.Nav.Client.BusinessChart.BusinessChartData",
    "Microsoft.Dynamics.Nav.Client.BusinessChart.DataMeasureType",
    "Microsoft.Dynamics.Nav.SystemEnvironment.SystemEnvironment",
    "System.StringComparison",
    "System.StringSplitOptions",
    "System.Text.Encoding",
    "System.Text.UTF8Encoding",
    "System.Text.UnicodeEncoding",
    "System.Text.ASCIIEncoding",
    "System.Text.StringBuilder",
    "System.Text.NormalizationForm",
    "System.Array",
    "System.Collections.Generic.List`1",
    "System.Collections.Generic.Dictionary`2",
    "System.Collections.Generic.SortedDictionary`2",
    "System.Collections.Generic.HashSet`1",
    "System.Collections.Generic.KeyValuePair`2",
    "System.Collections.Generic.IEnumerator`1",
    "System.Collections.Generic.IEnumerable`1",
    "System.Collections.Generic.IList`1",
    "System.Collections.Generic.ICollection`1",
    "System.Collections.Generic.IReadOnlyList`1",
    "System.Collections.Generic.Dictionary`2+KeyCollection",
    "System.Collections.Generic.Dictionary`2+ValueCollection",
    "System.Collections.ArrayList",
    "System.Collections.Hashtable",
    "System.Collections.Queue",
    "System.Collections.Stack",
    "System.Collections.IEnumerator",
    "System.Collections.IEnumerable",
    "System.Collections.IList",
    "System.Collections.ICollection",
    "System.Version",
    "System.Exception",
    "System.Xml.XmlException",
    "System.IO.MemoryStream",
    "System.IO.Stream",
    "System.IO.StreamReader",
    "System.IO.StreamWriter",
    "System.IO.StringReader",
    "System.IO.StringWriter",
    "System.IO.TextReader",
    "System.IO.TextWriter",
    "System.IO.SeekOrigin",
    // server files (crate::files::net_io_call)
    "System.IO.Path",
    "System.IO.Directory",
    "System.IO.DirectoryInfo",
    "System.IO.FileInfo",
    // crate::gzip
    "System.IO.Compression.GZipStream",
    "System.IO.Compression.CompressionMode",
    "System.IO.Compression.CompressionLevel",
    "System.Xml.XmlDocument",
    "System.Xml.XmlNode",
    "System.Xml.XmlElement",
    "System.Xml.XmlAttribute",
    "System.Xml.XmlText",
    "System.Xml.XmlCDataSection",
    "System.Xml.XmlComment",
    "System.Xml.XmlProcessingInstruction",
    "System.Xml.XmlDeclaration",
    "System.Xml.XmlDocumentType",
    "System.Xml.XmlNodeList",
    "System.Xml.XmlNamedNodeMap",
    "System.Xml.XmlAttributeCollection",
    "System.Xml.XmlNamespaceManager",
    "System.Xml.XmlNameTable",
    "System.Xml.NameTable",
    "System.Xml.XmlNodeType",
    "System.Xml.XmlConvert",
    "System.Xml.XmlReaderSettings",
    "System.Xml.XmlReader",
    "System.Xml.XmlTextReader",
    "System.Xml.DtdProcessing",
    "System.Xml.XmlUrlResolver",
    "System.Xml.XmlResolver",
    "System.Xml.XmlTextWriter",
    "System.Xml.XmlWriter",
    "System.Xml.XmlWriterSettings",
    "System.Xml.Formatting",
    "Newtonsoft.Json.Linq.JObject",
    "Newtonsoft.Json.Linq.JArray",
    "Newtonsoft.Json.Linq.JToken",
    "Newtonsoft.Json.Linq.JValue",
    "Newtonsoft.Json.Linq.JProperty",
    "Newtonsoft.Json.Linq.JContainer",
    "Newtonsoft.Json.Linq.JTokenType",
    "Newtonsoft.Json.JsonConvert",
    "Newtonsoft.Json.Formatting",
    "Newtonsoft.Json.JsonTextReader",
    "Microsoft.Dynamics.Nav.DocumentReport.WordReportManager",
];

/// The .NET type name of a DotNet alias (`dotnet { ... type("System.Uri"; Uri) }`), with a
/// fallback for well-known short names that some apps use without their own declaration.
pub fn resolve_type(cat: &'static al_model::Catalog, alias: &str) -> &'static str {
    let l = alias.to_ascii_lowercase();
    if let Some(t) = cat.dotnet_types.get(&l) {
        return t.as_str();
    }
    if let Some(t) = IMPLEMENTED.iter().find(|t| t.eq_ignore_ascii_case(alias) || short_name(t).eq_ignore_ascii_case(alias)) {
        return t;
    }
    // unknown: keep the alias itself (leaked once per distinct name)
    intern(alias)
}

fn intern(s: &str) -> &'static str {
    thread_local! {
        static NAMES: RefCell<Vec<&'static str>> = const { RefCell::new(vec![]) };
    }
    NAMES.with(|n| {
        let mut n = n.borrow_mut();
        if let Some(x) = n.iter().find(|x| **x == s) {
            return *x;
        }
        let x: &'static str = Box::leak(s.to_string().into_boxed_str());
        n.push(x);
        x
    })
}

/// `System.Collections.Generic.Dictionary`2` → `Dictionary`; `System.String[]` → `String[]`.
pub fn short_name(t: &str) -> &str {
    let t = t.split('+').next_back().unwrap_or(t);
    let t = t.rsplit('.').next().unwrap_or(t);
    t.split('`').next().unwrap_or(t)
}

/// A .NET type given as text (`System.String`, an alias, a short name) as an interned name.
pub fn resolve_type_name(t: &str) -> &'static str {
    let t = t.trim();
    if let Some(x) = IMPLEMENTED.iter().find(|x| x.eq_ignore_ascii_case(t) || short_name(x).eq_ignore_ascii_case(t)) {
        return x;
    }
    if let Some(c) = crate::record::CATALOG.get() {
        if let Some(x) = c.dotnet_types.get(&t.to_ascii_lowercase()) {
            return x.as_str();
        }
    }
    intern(t)
}

/// Items of an enumerable argument (AL List, .NET collection).
pub fn enumerate_val(v: Option<&Val>) -> Option<Vec<Val>> {
    match v? {
        Val::Net(n) => enumerate(n),
        Val::List(l) => Some(l.borrow().clone()),
        Val::Array(a, _) => Some(a.borrow().clone()),
        _ => None,
    }
}

/// GetDotNetType(x): the .NET type name of a DotNet value (as text).
pub fn get_type(v: &Val) -> Val {
    match v {
        Val::Net(n) => Val::Text(net_type(&n.borrow()).to_string()),
        Val::Text(_) => Val::Text("System.String".into()),
        Val::Int(_) => Val::Text("System.Int32".into()),
        Val::BigInt(_) => Val::Text("System.Int64".into()),
        Val::Dec(_) => Val::Text("System.Decimal".into()),
        Val::Bool(_) => Val::Text("System.Boolean".into()),
        Val::Char(_) => Val::Text("System.Char".into()),
        Val::DateTime(_) => Val::Text("System.DateTime".into()),
        Val::Guid(_) => Val::Text("System.Guid".into()),
        _ => Val::Text("System.Object".into()),
    }
}

pub fn is_implemented(t: &str) -> bool {
    // arrays only when their element type is emulated (NavPermissionSetRelation[] stays a shim)
    match t.strip_suffix("[]") {
        Some(e) => is_implemented(e),
        None => IMPLEMENTED.contains(&t) || crypto::HASH_TYPES.contains(&t) || cipher::TYPES.contains(&t) || asym::TYPES.contains(&t) || x509::TYPES.contains(&t) || zip::TYPES.contains(&t) || xlsx::TYPES.contains(&t) || xlsx::is_sdk(t) || oxml::is_value_type(t) || signedxml::TYPES.contains(&t),
    }
}

/// Default value of a `DotNet X` variable.
pub fn default_dotnet(cat: &'static al_model::Catalog, alias: &str) -> Val {
    let t = resolve_type(cat, alias);
    if is_implemented(t) {
        net(NetObj::Null(t))
    } else {
        Val::Opaque("DotNet")
    }
}

/// AL built-in types implemented here: the default value of a variable of that type.
pub fn default_simple(name_lower: &str) -> Option<Val> {
    Some(match name_lower {
        "xmldocument" => net(NetObj::XNode(xml::new_document(), true)),
        // XmlElement / XmlNode / ... variables start uninitialized
        "xmlelement" | "xmlnode" | "xmlattribute" | "xmltext" | "xmlcdata" | "xmlcomment" | "xmlprocessinginstruction" | "xmldeclaration"
        | "xmldocumenttype" => net(NetObj::XNode(xml::new_uninit(name_lower), true)),
        "xmlnodelist" => net(NetObj::XNodeList(vec![], true)),
        "xmlattributecollection" => net(NetObj::XAttrs(xml::new_uninit("xmlelement"), true)),
        "xmlnamespacemanager" => net(NetObj::XNsMgr(Rc::new(RefCell::new(vec![])), true)),
        "xmlnametable" => net(NetObj::XNameTable(true)),
        "xmlreadoptions" => net(NetObj::XReadOptions(false)),
        "xmlwriteoptions" => net(NetObj::XWriteOptions(false)),
        "httpclient" => net(NetObj::HttpClient(http::Client::default())),
        "httprequestmessage" => net(NetObj::HttpRequest(http::Request::new())),
        "httpresponsemessage" => net(NetObj::HttpResponse(http::Response::new())),
        "httpcontent" => net(NetObj::HttpContent(http::Content::default())),
        "httpheaders" => net(NetObj::HttpHeaders(http::Headers::default())),
        "testhttprequestmessage" => net(NetObj::TestHttpRequest(http::TestRequest::default())),
        "testhttpresponsemessage" => net(NetObj::HttpResponse(http::Response::new())),
        "cookie" => net(NetObj::Cookie(String::new(), String::new())),
        _ => return None,
    })
}

/// Names that are used as static classes: `XmlDocument.ReadFrom(...)`, `XmlElement.Create(...)`.
pub fn is_static_type(name_lower: &str) -> bool {
    matches!(
        name_lower,
        "xmldocument"
            | "xmlelement"
            | "xmlattribute"
            | "xmltext"
            | "xmlcdata"
            | "xmlcomment"
            | "xmlprocessinginstruction"
            | "xmldeclaration"
            | "xmldocumenttype"
            | "xmlnamespacemanager"
            | "sessioninformation"
    )
}

pub const MCP_UTILITIES: &str = "MCP Utilities";

/// Codeunit 2000000023 "MCP Utilities": the system MCP tools as (name, description) dictionaries.
/// The tool names are the ones Microsoft Learn lists [src:X01]; the descriptions are bc-rust's own
/// wording (bc-rust does not serve these tools, the System Application only lists them).
fn mcp_utilities(m: &str, a: &Args) -> R<Out> {
    let dict = |items: &[(&str, &str)]| Val::Dict(Rc::new(RefCell::new(items.iter().map(|(k, v)| (Val::Text(k.to_string()), Val::Text(v.to_string()))).collect())));
    match m {
        "getsystemtoolsindynamicmode" => ok(dict(&[
            ("bc_actions_search", "Look up the API actions of this configuration by keyword. Follow up with bc_actions_describe to see what an action takes."),
            ("bc_actions_describe", "Show the input schema of one API action, so that bc_actions_invoke can be called with the right arguments."),
            ("bc_actions_invoke", "Run one API action with arguments that follow the schema bc_actions_describe returned for it."),
        ])),
        "getsystemtoolsindataquery" => ok(dict(&[
            ("bc_data_find_tables", "Look up tables by name or by meaning; each match comes with its table number."),
            ("bc_data_get_table_schema", "List the fields of one table."),
            ("bc_data_get_table_relations", "List the relations of one table: the tables it points to, or the tables that point to it."),
            ("bc_data_query", "Check an AL query and return its columns; with returnData set to true it also runs the query and returns the rows."),
        ])),
        // GetParentMCPTools(Dictionary of [Integer (page), Text (API version)]): each page's parent
        // pages from IMcpUtilsService.BuildChildParentPartsRelationship (not in Ncl). Approximation:
        // no page-part relationships, so every page maps to an empty list (BC's result for a page
        // the service has no parents for).
        "getparentmcptools" => {
            let pages: Vec<Val> = match a.raw(0) {
                Some(Val::Dict(d)) => d.borrow().iter().map(|(k, _)| k.clone()).collect(),
                _ => vec![],
            };
            ok(Val::Dict(Rc::new(RefCell::new(pages.into_iter().map(|p| (p, Val::List(Rc::new(RefCell::new(vec![]))))).collect()))))
        }
        _ => Ok(Out::Unknown),
    }
}

/// Platform codeunits that exist only in the service tier (no AL source): `Codeunit Base64Convert`.
pub fn platform_codeunit(name: &str) -> Option<Val> {
    let n = name.trim_matches('"').to_ascii_lowercase();
    match n.rsplit('.').next().unwrap_or(&n) {
        "base64convert" => Some(net(NetObj::Base64Convert)),
        "mcp utilities" => Some(net(NetObj::McpUtilities)),
        _ => None,
    }
}

/// Text methods of AL types that are represented as Text here (SecretText.Unwrap / IsEmpty).
pub fn text_method(s: &str, m: &str, a: &[Val]) -> Option<Val> {
    let _ = a;
    Some(match m {
        "unwrap" => Val::Text(s.to_string()),
        "isempty" => Val::Bool(s.is_empty()),
        _ => return None,
    })
}

pub fn type_name(n: &NetRef) -> &'static str {
    match &*n.borrow() {
        NetObj::XNode(x, true) => xml::al_type_name(x),
        NetObj::XNodeList(_, true) => "XmlNodeList",
        NetObj::XAttrs(_, true) => "XmlAttributeCollection",
        NetObj::XNsMgr(_, true) => "XmlNamespaceManager",
        NetObj::XNameTable(true) => "XmlNameTable",
        NetObj::XReadOptions(_) => "XmlReadOptions",
        NetObj::XWriteOptions(_) => "XmlWriteOptions",
        NetObj::HttpClient(_) => "HttpClient",
        NetObj::HttpRequest(_) => "HttpRequestMessage",
        NetObj::HttpResponse(_) => "HttpResponseMessage",
        NetObj::HttpContent(_) => "HttpContent",
        NetObj::HttpHeaders(_) => "HttpHeaders",
        NetObj::TestHttpRequest(_) => "TestHttpRequestMessage",
        NetObj::Cookie(..) => "Cookie",
        _ => "DotNet",
    }
}

/// The .NET type of a DotNet object (for GetType / error messages).
pub fn net_type(o: &NetObj) -> &'static str {
    match o {
        NetObj::Null(t) | NetObj::Prim(t, _) => t,
        NetObj::DateTime(_) => "System.DateTime",
        NetObj::DateTimeOffset(..) => "System.DateTimeOffset",
        NetObj::TimeSpan(_) => "System.TimeSpan",
        NetObj::TimeZone(_) => "System.TimeZoneInfo",
        NetObj::Culture(_, v) => v.type_name(),
        NetObj::Regex(_) => "System.Text.RegularExpressions.Regex",
        NetObj::MatchCollection(_) => "System.Text.RegularExpressions.MatchCollection",
        NetObj::Match(_) => "System.Text.RegularExpressions.Match",
        NetObj::GroupCollection(_) => "System.Text.RegularExpressions.GroupCollection",
        NetObj::Group(..) => "System.Text.RegularExpressions.Group",
        NetObj::CaptureCollection(..) => "System.Text.RegularExpressions.CaptureCollection",
        NetObj::Capture(..) => "System.Text.RegularExpressions.Capture",
        NetObj::Uri(_) => "System.Uri",
        NetObj::UriBuilder(_) => "System.UriBuilder",
        NetObj::NameValueCollection(_) => "System.Collections.Specialized.NameValueCollection",
        NetObj::StringBuilder(_) => "System.Text.StringBuilder",
        NetObj::Encoding(..) => "System.Text.Encoding",
        NetObj::Array(..) => "System.Array",
        NetObj::List(t, _) | NetObj::Dict(t, _) | NetObj::Reader(t, ..) | NetObj::Writer(t, _) => t,
        NetObj::StreamWriter(_) | NetObj::EncodedWriter(..) => "System.IO.StreamWriter",
        NetObj::KeyValue(..) => "System.Collections.Generic.KeyValuePair`2",
        NetObj::Enumerator(..) => "System.Collections.IEnumerator",
        NetObj::Version(_) => "System.Version",
        NetObj::Stream(..) => "System.IO.MemoryStream",
        NetObj::Exception(_) => "System.Exception",
        NetObj::ExceptionOf(t, ..) => t,
        NetObj::Base64Convert => "Base64Convert",
        NetObj::McpUtilities => MCP_UTILITIES,
        NetObj::GZip(_) => "System.IO.Compression.GZipStream",
        NetObj::Bitmap(_) => "System.Drawing.Bitmap",
        NetObj::Image(_) => "System.Drawing.Image",
        NetObj::Graphics(_) => "System.Drawing.Graphics",
        NetObj::Color(_) => "System.Drawing.Color",
        NetObj::Brush(_) => "System.Drawing.SolidBrush",
        NetObj::MailAddress(..) => "System.Net.Mail.MailAddress",
        NetObj::Stopwatch(..) => "System.Diagnostics.Stopwatch",
        NetObj::Bag(t, _) => t,
        NetObj::Hasher(_, Some(_)) => "System.Security.Cryptography.KeyedHashAlgorithm",
        NetObj::Hasher(_, None) => "System.Security.Cryptography.HashAlgorithm",
        NetObj::Pbkdf2 { .. } => "System.Security.Cryptography.Rfc2898DeriveBytes",
        NetObj::Sym(s) => s.ty,
        NetObj::Transform(..) => "System.Security.Cryptography.ICryptoTransform",
        NetObj::CryptoStream(_) => "System.Security.Cryptography.CryptoStream",
        NetObj::Rsa(s) => s.ty,
        NetObj::Cert(_) => x509::CERT2,
        NetObj::Zip(_) => zip::ARCHIVE,
        NetObj::ZipEntry(_) => zip::ENTRY,
        NetObj::XlBook(_) => xlsx::BOOK_READER,
        NetObj::XlSheet(..) => xlsx::SHEET_READER,
        NetObj::XlWBook(_) => xlsx::BOOK_WRITER,
        NetObj::XlWSheet(..) => xlsx::SHEET_WRITER,
        NetObj::CertReq(_) => "System.Security.Cryptography.X509Certificates.CertificateRequest",
        NetObj::KeyVaultMock(..) => nav::KEY_VAULT_MOCK,
        NetObj::KeyVaultClient(_) => nav::KEY_VAULT_CLIENT,
        NetObj::Mmf(..) => nav::MMF,
        NetObj::MmfView(..) => nav::MMF_VIEW,
        NetObj::GraphMock(_) => "Microsoft.Dynamics.Nav.AzureADGraphClient.MockGraphQuery",
        NetObj::GraphPage(_) => "Microsoft.Dynamics.Nav.AzureADGraphClient.UserInfoPage",
        NetObj::BinaryWriter(_) => "System.IO.BinaryWriter",
        NetObj::BinaryReader(_) => "System.IO.BinaryReader",
        NetObj::JReader(_) => newtonsoft::JSON_TEXT_READER,
        NetObj::InStreamView(_) => text::IN_STREAM_VIEW,
        NetObj::DataTable(_) => "System.Data.DataTable",
        NetObj::DataColumns(_) => "System.Data.DataColumnCollection",
        NetObj::DataRows(_) => "System.Data.DataRowCollection",
        NetObj::DataRow(_) => "System.Data.DataRow",
        NetObj::DataSet(_) => "System.Data.DataSet",
        NetObj::DataTables(_) => "System.Data.DataTableCollection",
        NetObj::Ox(o) => oxml::type_name(o),
        NetObj::Sx(s) => signedxml::type_name(s),
        NetObj::DataColumn(_) => "System.Data.DataColumn",
        NetObj::ChartData(_) => "Microsoft.Dynamics.Nav.Client.BusinessChart.BusinessChartData",
        NetObj::JTok(j) => match &*j.borrow() {
            crate::json::J::Obj(_) => "Newtonsoft.Json.Linq.JObject",
            crate::json::J::Arr(_) => "Newtonsoft.Json.Linq.JArray",
            _ => "Newtonsoft.Json.Linq.JValue",
        },
        NetObj::JProp(..) => "Newtonsoft.Json.Linq.JProperty",
        NetObj::XmlReaderSettings => "System.Xml.XmlReaderSettings",
        NetObj::XmlWriter(_) => "System.Xml.XmlTextWriter",
        NetObj::XReader(_) => "System.Xml.XmlReader",
        NetObj::XNode(x, _) => xml::net_type_name(x),
        NetObj::XNodeList(..) => "System.Xml.XmlNodeList",
        NetObj::XAttrs(..) => "System.Xml.XmlAttributeCollection",
        NetObj::XNsMgr(..) => "System.Xml.XmlNamespaceManager",
        NetObj::XNameTable(_) => "System.Xml.NameTable",
        _ => "System.Object",
    }
}

/// The AL value inside a boxed .NET primitive / DateTime / TimeSpan (None for objects).
pub fn unbox(n: &NetRef) -> Option<Val> {
    match &*n.borrow() {
        NetObj::Prim(_, v) => Some(v.clone()),
        NetObj::DateTime(d) => Some(Val::DateTime(d.to_al())),
        NetObj::DateTimeOffset(utc, _) => Some(Val::DateTime(datetime::ticks_to_ms(*utc))),
        NetObj::TimeSpan(t) => Some(Val::Duration(t / 10_000)),
        _ => None,
    }
}

/// `v` with boxed .NET primitives replaced by their AL value.
pub fn unboxed(v: Val) -> Val {
    match &v {
        Val::Net(n) => unbox(n).unwrap_or(v),
        _ => v,
    }
}

pub fn to_text(n: &NetRef) -> String {
    let o = n.borrow();
    match &*o {
        NetObj::Null(_) => String::new(),
        NetObj::Prim(t, v) => text::prim_to_string(t, v),
        NetObj::DateTime(d) => d.format("G", culture::current()),
        NetObj::DateTimeOffset(u, off) => datetime::format_offset(*u, *off, "G", culture::current()),
        NetObj::TimeSpan(t) => datetime::format_timespan(*t, ""),
        NetObj::TimeZone(z) => z.display(),
        NetObj::Culture(c, _) => c.name.to_string(),
        NetObj::Regex(r) => r.pattern.clone(),
        NetObj::Match(m) => m.groups[0].value.clone(),
        NetObj::Group(m, g) => m.groups[*g].value.clone(),
        NetObj::Capture(_, _, v) => v.clone(),
        NetObj::Uri(u) => u.to_display(),
        NetObj::UriBuilder(b) => b.to_uri().to_display(),
        NetObj::StringBuilder(s) => s.clone(),
        NetObj::Version(v) => v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("."),
        NetObj::KeyValue(k, v) => format!("[{}, {}]", k.to_text(), v.to_text()),
        NetObj::Exception(m) => m.clone(),
        NetObj::ExceptionOf(_, m, _) => m.clone(),
        NetObj::XNode(x, _) => xml::outer_xml(x),
        NetObj::Writer(_, s) => s.clone(),
        NetObj::JTok(j) => newtonsoft::token_to_string(j, true),
        NetObj::MailAddress(addr, disp) => {
            if disp.is_empty() {
                addr.clone()
            } else {
                format!("\"{disp}\" <{addr}>")
            }
        }
        o => net_type(o).to_string(),
    }
}

pub fn net_equals(a: &NetRef, b: &NetRef) -> bool {
    if Rc::ptr_eq(a, b) {
        return true;
    }
    match (&*a.borrow(), &*b.borrow()) {
        (NetObj::Null(_), NetObj::Null(_)) => true,
        (NetObj::XNode(x, _), NetObj::XNode(y, _)) => Rc::ptr_eq(x, y),
        (NetObj::Uri(x), NetObj::Uri(y)) => x.to_display() == y.to_display(),
        (NetObj::Version(x), NetObj::Version(y)) => x == y,
        _ => match (unbox(a), unbox(b)) {
            (Some(x), Some(y)) => equals(&x, &y),
            _ => false,
        },
    }
}

pub fn is_null(v: &Val) -> bool {
    match v {
        Val::Net(n) => matches!(&*n.borrow(), NetObj::Null(_)),
        _ => false,
    }
}

/// Box an AL value assigned to a `DotNet X` variable.
pub fn coerce_dotnet(cat: &'static al_model::Catalog, alias: &str, v: Val) -> Val {
    let t = resolve_type(cat, alias);
    if !is_implemented(t) {
        return v;
    }
    match v {
        Val::Net(_) => v,
        Val::Void => net(NetObj::Null(t)),
        Val::DateTime(ms) if t == "System.DateTime" => net(NetObj::DateTime(datetime::NetDateTime::from_al(ms))),
        Val::DateTime(ms) if t == "System.DateTimeOffset" => net(NetObj::DateTimeOffset(datetime::ms_to_ticks(ms), 0)),
        Val::Duration(d) if t == "System.TimeSpan" => net(NetObj::TimeSpan(d * 10_000)),
        Val::Text(_) | Val::Char(_) | Val::Int(_) | Val::BigInt(_) | Val::Dec(_) | Val::Bool(_) | Val::Guid(_) | Val::Opt { .. } | Val::Duration(_) => {
            net(NetObj::Prim(t, v))
        }
        Val::Date(_) | Val::Time(_) | Val::DateTime(_) => net(NetObj::Prim(t, v)),
        other => other,
    }
}

/// Items of a foreach over a DotNet enumerable or an AL XmlNodeList / XmlAttributeCollection.
pub fn enumerate(n: &NetRef) -> Option<Vec<Val>> {
    let o = n.borrow();
    Some(match &*o {
        NetObj::Array(_, v) | NetObj::List(_, v) | NetObj::Enumerator(v, _) => v.clone(),
        NetObj::Dict(t, d) if t.contains("HashSet") => d.iter().map(|(k, _)| k.clone()).collect(),
        NetObj::Dict(_, d) => d.iter().map(|(k, v)| net(NetObj::KeyValue(k.clone(), v.clone()))).collect(),
        NetObj::MatchCollection(m) => m.iter().map(|x| net(NetObj::Match(x.clone()))).collect(),
        NetObj::XlSheet(b, i) => xlsx::sheet_cells_vals(b, *i),
        NetObj::Prim(t, _) if *t == xlsx::SDK_SINK => vec![],
        NetObj::GroupCollection(m) => (0..m.groups.len()).map(|g| net(NetObj::Group(m.clone(), g))).collect(),
        NetObj::CaptureCollection(m, g) => regex::captures_of(m, *g),
        NetObj::XNodeList(l, al) => l.iter().map(|x| net(NetObj::XNode(x.clone(), *al))).collect(),
        NetObj::XAttrs(e, al) => xml::attributes(e).into_iter().map(|x| net(NetObj::XNode(x, *al))).collect(),
        NetObj::XNode(x, al) => xml::children(x).into_iter().map(|c| net(NetObj::XNode(c, *al))).collect(),
        NetObj::NameValueCollection(v) => v.iter().map(|(k, _)| k.clone().map(Val::Text).unwrap_or(Val::Void)).collect(),
        NetObj::Prim(_, Val::Text(s)) => s.chars().map(Val::Char).collect(),
        NetObj::JTok(j) => match &*j.borrow() {
            crate::json::J::Arr(a) => a.iter().cloned().map(newtonsoft::jtok).collect(),
            crate::json::J::Obj(o) => o.iter().map(|(k, _)| net(NetObj::JProp(j.clone(), k.clone()))).collect(),
            _ => vec![],
        },
        _ => return None,
    })
}

/// Overload resolution: how well a Net value fits a parameter type (`base_name` of the TypeRef).
pub fn type_score(param: &str, n: &NetRef) -> i32 {
    let o = n.borrow();
    let al_xml = matches!(&*o, NetObj::XNode(_, true) | NetObj::XNodeList(_, true) | NetObj::XAttrs(_, true) | NetObj::XNsMgr(_, true));
    match param {
        "dotnet" => {
            if al_xml || type_name(n) != "DotNet" {
                -10
            } else {
                4
            }
        }
        "variant" => 1,
        p if p.starts_with("xml") || p.starts_with("http") || p.starts_with("testhttp") => {
            let tn = type_name(n).to_ascii_lowercase();
            if tn == p {
                5
            } else if al_xml && (p == "xmlnode" || tn == "xmlnode") {
                3
            } else {
                -10
            }
        }
        _ => -5,
    }
}

// ---------------------------------------------------------------- calls

/// Arguments of a call on a Net value: evaluated values plus the expressions (for `var`/`out`).
pub struct Args<'e> {
    pub exprs: &'e [Expr],
    pub v: Vec<Val>,
}

impl Args<'_> {
    pub fn len(&self) -> usize {
        self.v.len()
    }
    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }
    pub fn get(&self, i: usize) -> Val {
        self.v.get(i).cloned().map(unboxed).unwrap_or(Val::Void)
    }
    pub fn raw(&self, i: usize) -> Option<&Val> {
        self.v.get(i)
    }
    pub fn text(&self, i: usize) -> String {
        self.v.get(i).map(|v| v.to_text()).unwrap_or_default()
    }
    pub fn int(&self, i: usize) -> i64 {
        self.v.get(i).map(|v| v.as_i64()).unwrap_or(0)
    }
    pub fn bool(&self, i: usize) -> bool {
        self.v.get(i).is_some_and(|v| v.as_bool())
    }
    pub fn net(&self, i: usize) -> Option<NetRef> {
        match self.v.get(i) {
            Some(Val::Net(n)) => Some(n.clone()),
            _ => None,
        }
    }
}

/// Result of a member lookup on an emulated type.
pub enum Out {
    Val(Val),
    /// Member unknown for this type: logged, a neutral value is returned (old shim behaviour).
    Unknown,
}

pub fn ok(v: Val) -> R<Out> {
    Ok(Out::Val(v))
}

pub fn void() -> R<Out> {
    Ok(Out::Val(Val::Void))
}

/// GetLastErrorCode of a failed DotNet call that threw an XmlException: NavSession.GetErrorCode of
/// the NavNCLDotNetInvokeException ("DotNetInvoke") and of its inner exception ("Xml").
pub const XML_EXCEPTION_CODE: &str = "DotNetInvoke:Xml";

/// `net_err` with the GetLastErrorCode BC gives the exception (NavSession.GetLastErrorCode:
/// `<outer>:<inner>` exception type names without Nav/NavNcl and Exception).
pub fn net_err_code<T>(type_name: &str, member: &str, code: &'static str, msg: impl AsRef<str>) -> R<T> {
    match net_err_plain::<T>(type_name, member, msg) {
        Err(Flow::Err(mut e)) => {
            e.code = code;
            Err(Flow::Err(e))
        }
        other => other,
    }
}

/// GetLastErrorObject after a failed DotNet call: the NavNCLDotNetInvokeException with the .NET
/// exception as its InnerException (only XmlException is modelled; other errors give None).
/// A rethrown error's code as the 'static str AlError keeps (the set of codes is small).
fn intern_code(code: &str) -> &'static str {
    static CODES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut l = CODES.lock().unwrap();
    if let Some(c) = l.iter().find(|c| **c == code) {
        return c;
    }
    let c: &'static str = Box::leak(code.to_string().into_boxed_str());
    l.push(c);
    c
}

pub fn last_error_object(msg: &str, code: &str) -> Option<Val> {
    // a division by zero is .NET's DivideByZeroException (GetBaseException: itself)
    if code == "AL:DivideByZero" {
        return Some(net(NetObj::ExceptionOf("System.DivideByZeroException", msg.to_string(), None)));
    }
    let inner_type = match code {
        XML_EXCEPTION_CODE => "System.Xml.XmlException",
        _ => return None,
    };
    let inner_msg = msg.split_once("failed with this message: ").map(|x| x.1).unwrap_or(msg).to_string();
    let inner = net(NetObj::ExceptionOf(inner_type, inner_msg, None));
    Some(net(NetObj::ExceptionOf("Microsoft.Dynamics.Nav.Types.Exceptions.NavNCLDotNetInvokeException", msg.to_string(), Some(inner))))
}

/// .NET exception message as BC shows it for a failing DotNet call: `A call to System.Uri failed
/// with this message: ...` (constructor) or `A call to System.UriBuilder.Port failed ...`.
pub fn net_err<T>(type_name: &str, member: &str, msg: impl AsRef<str>) -> R<T> {
    let code = dotnet_error_code(type_name, msg.as_ref());
    match net_err_plain::<T>(type_name, member, msg) {
        Err(Flow::Err(mut e)) => {
            e.code = code;
            Err(Flow::Err(e))
        }
        other => other,
    }
}

/// GetLastErrorCode of a failed DotNet call: `DotNetInvoke:<inner>` (NavSession.GetLastErrorCode /
/// GetErrorCode: the NavNCLDotNetInvokeException and its inner .NET exception, type names without
/// "NavNcl" and "Exception"). The inner exception is the one .NET throws with this message; messages
/// passed through from a parser take the exception of the type that parses. A System.Exception
/// gives `DotNetInvoke:` (its name is all "Exception"). Error Message.LogLastError needs the code.
pub fn dotnet_error_code(type_name: &str, msg: &str) -> &'static str {
    let has = |p: &str| msg.contains(p);
    let starts = |p: &str| msg.starts_with(p);
    if starts("Could not find file") || starts("Unable to find the specified file") {
        "DotNetInvoke:FileNotFound"
    } else if starts("Could not find a part of the path") {
        "DotNetInvoke:DirectoryNotFound"
    } else if has("Index was outside the bounds of the array") || starts("There is no row at position") || starts("Cannot find table") || starts("Cannot find column") {
        "DotNetInvoke:IndexOutOfRange"
    } else if has("Index was out of range") || has("out of the range of valid values") || has("Index is equal to or greater than Count") || has("Index and length must refer to a location") || has("un-representable DateTime") || starts("Valid values are between") {
        "DotNetInvoke:ArgumentOutOfRange"
    } else if has("Value was either too large or too small") {
        "DotNetInvoke:Overflow"
    } else if starts("Value cannot be null") {
        "DotNetInvoke:ArgumentNull"
    } else if has("Object reference not set to an instance of an object") {
        "DotNetInvoke:NullReference"
    } else if has("was not present in the dictionary") {
        "DotNetInvoke:KeyNotFound"
    } else if has("Input string was not in a correct format")
        || has("Could not find any recognizable digits")
        || has("was not recognized as a valid")
        || has("is not a valid Base-64 string")
        || has("is not a valid hex string")
        || starts("Guid should contain")
        || has("Format specifier was invalid")
        || has("not in the form required for an e-mail address")
        || has("7-bit encoded integer")
        || starts("Index (zero based) must be")
    {
        "DotNetInvoke:Format"
    } else if starts("Culture is not supported") {
        "DotNetInvoke:CultureNotFound"
    } else if has("time zone") && has("was not found") {
        "DotNetInvoke:TimeZoneNotFound"
    } else if starts("Unable to read beyond the end of the stream") {
        "DotNetInvoke:EndOfStream"
    } else if starts("Queue empty") || starts("Stack empty") || starts("Collection empty") || starts("Sequence contains no elements") || starts("Enumeration has either not started") || starts("This operation is not supported for a relative URI") || starts("The specified node cannot be inserted") || starts("Cannot set a value on node type") || has("only available while tests run") {
        "DotNetInvoke:InvalidOperation"
    } else if starts("No data is available for encoding") || starts("Cannot modify read-only archive") {
        "DotNetInvoke:NotSupported"
    } else if starts("Cannot create a file when that file already exists") {
        "DotNetInvoke:IO"
    } else if starts("A column named") {
        "DotNetInvoke:DuplicateName"
    } else if starts("Invalid URI") && !has("(Parameter") {
        "DotNetInvoke:UriFormat"
    } else if starts("Specified key is not a valid size") || starts("Specified initialization vector") || starts("Specified cipher mode") || starts("Specified padding mode") || starts("Specified block size") || starts("Specified feedback size") || starts("The certificate data cannot be read") || starts("Invalid content type") {
        "DotNetInvoke:Cryptographic"
    } else if starts("Unexpected character encountered while parsing") || starts("Error reading ") {
        "DotNetInvoke:JsonReader"
    } else if starts("An item with the same key has already been added")
        || has("is not a supported encoding name")
        || starts("Requested value")
        || starts("Version string portion")
        || has("Property with the same name already exists")
        || starts("String cannot be of zero length")
        || starts("Parameter is not valid")
        || has("Parameter name:")
        || starts("The file name is not specified")
        || (starts("Invalid URI") && has("(Parameter"))
    {
        "DotNetInvoke:Argument"
    } else {
        // a parser's message: the exception of the type that parses
        match short_name(resolve_type_name(type_name)) {
            "XmlDocument" | "XmlReader" | "XmlTextReader" | "XmlTextWriter" | "DataSet" => "DotNetInvoke:Xml",
            "XmlNode" if !msg.contains("XPath") && !msg.contains("xpath") => "DotNetInvoke:Xml",
            "XmlNode" => "DotNetInvoke:XPath",
            "Regex" => "DotNetInvoke:RegexParse",
            "JsonTextReader" | "JObject" | "JArray" | "JToken" | "JValue" => "DotNetInvoke:JsonReader",
            "Uri" | "UriBuilder" => "DotNetInvoke:UriFormat",
            "ZipArchive" | "GZipStream" => "DotNetInvoke:InvalidData",
            "CryptoStream" | "ICryptoTransform" | "RSAEncryptionHelper" | "CertificateRequest" | "X509Certificate2" | "RSACryptoServiceProvider" => "DotNetInvoke:Cryptographic",
            "ColorTranslator" => "DotNetInvoke:Argument",
            _ => "DotNetInvoke:",
        }
    }
}

fn net_err_plain<T>(type_name: &str, member: &str, msg: impl AsRef<str>) -> R<T> {
    let full = resolve_type_name(type_name);
    // the member as written in AL (dispatch works on lower-cased names)
    let written = CUR_MEMBER.with(|c| c.borrow().clone());
    let member = if written.eq_ignore_ascii_case(member) { written.as_str() } else { member };
    if member.eq_ignore_ascii_case(short_name(full)) {
        return err(format!("A call to {full} failed with this message: {}", msg.as_ref()));
    }
    let member = member.chars().next().map(|c| c.to_uppercase().collect::<String>() + &member[c.len_utf8()..]).unwrap_or_default();
    err(format!("A call to {full}.{member} failed with this message: {}", msg.as_ref()))
}

thread_local! {
    static CUR_MEMBER: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_member(name: &str) {
    CUR_MEMBER.with(|c| {
        let mut c = c.borrow_mut();
        c.clear();
        c.push_str(name);
    });
}

pub fn null_ref<T>(t: &str, member: &str) -> R<T> {
    net_err(t, member, "Object reference not set to an instance of an object.")
}

impl<'r> Session<'r> {
    pub fn net_args<'e>(&mut self, f: &mut Frame, exprs: &'e [Expr]) -> R<Args<'e>> {
        let mut v = Vec::with_capacity(exprs.len());
        for e in exprs {
            v.push(self.eval(f, e)?);
        }
        Ok(Args { exprs, v })
    }

    /// Global functions that belong to the platform types implemented here (the fallback of
    /// `builtin`): GetDotNetType, CanLoadType, SecretStrSubstNo, collectible-error queries.
    pub fn net_builtin(&mut self, f: &mut Frame, name: &str, args: &[Expr]) -> R<Option<Val>> {
        Ok(Some(match name {
            "getdotnettype" => {
                let v = self.eval(f, &args[0])?;
                get_type(&v)
            }
            "canloadtype" => {
                for a in args {
                    let _ = self.eval(f, a)?;
                }
                Val::Bool(true)
            }
            "secretstrsubstno" => {
                let a = self.net_args(f, args)?;
                let rest: Vec<Val> = (1..a.len()).map(|i| a.get(i)).collect();
                Val::Text(crate::builtins::str_subst(&a.text(0), &rest))
            }
            "iscollectingerrors" | "hascollectederrors" => Val::Bool(false),
            "clearcollectederrors" => Val::Void,
            _ => return Ok(None),
        }))
    }

    /// Write an `out`/`var` argument of a call.
    pub fn net_out(&mut self, f: &mut Frame, a: &Args, i: usize, v: Val) -> R<()> {
        if let Some(e) = a.exprs.get(i) {
            let lv = self.lvalue(f, e)?;
            self.write_lval(&lv, v)?;
        }
        Ok(())
    }

    /// Method or property call on a Net value (`m` is lower-cased).
    pub fn net_method(&mut self, f: &mut Frame, n: &NetRef, m: &str, name: &str, args: &[Expr], discard: bool) -> R<Val> {
        let a = self.net_args(f, args)?;
        set_member(name);
        let kind = net_kind(n);
        let r = match kind {
            Kind::AlXml => self.al_xml_call(f, n, m, &a, discard),
            Kind::Http => self.http_call(f, n, m, &a, discard),
            Kind::Net => self.dotnet_call(f, n, m, &a),
        };
        match r? {
            Out::Val(v) => Ok(v),
            Out::Unknown => {
                let t = match kind {
                    Kind::Net => net_type(&n.borrow()).to_string(),
                    _ => type_name(n).to_string(),
                };
                *self.missing.entry(format!("dotnet {t}.{name}")).or_default() += 1;
                match kind {
                    // unimplemented members of the AL types are runtime gaps
                    Kind::AlXml | Kind::Http => err(format!("Not implemented in bc-rust: {}.{name}", type_name(n))),
                    Kind::Net => {
                        crate::assume::hit_member("DotNet.Neutral", &format!("{}.{name}", short_name(&t)));
                        Ok(Val::Void)
                    }
                }
            }
        }
    }

    /// `XmlDocument.ReadFrom(...)`, `XmlElement.Create(...)`: static members of the AL Xml types.
    pub fn net_static(&mut self, f: &mut Frame, class: &str, name: &str, args: &[Expr]) -> R<Val> {
        let discard = std::mem::take(&mut self.discard);
        let a = self.net_args(f, args)?;
        let m = name.to_ascii_lowercase();
        if class == "sessioninformation" {
            return Ok(match m.as_str() {
                // the running AL frames (crate::sysfuncs al_callstack)
                "callstack" => Val::Text(self.al_callstack()),
                "sqlrowsread" | "sqlstatementsexecuted" | "aitokensused" => {
                    crate::assume::hit("SessionInformation.Counters");
                    Val::BigInt(0)
                }
                _ => {
                    *self.missing.entry(format!("SessionInformation.{name}")).or_default() += 1;
                    crate::assume::hit("SessionInformation.Counters");
                    Val::Void
                }
            });
        }
        match self.al_xml_static(f, class, &m, &a, discard)? {
            Out::Val(v) => Ok(v),
            Out::Unknown => {
                *self.missing.entry(format!("{class}.{name}")).or_default() += 1;
                err(format!("Not implemented in bc-rust: {class}.{name}"))
            }
        }
    }

    /// Property read used as an lvalue's current value.
    pub fn net_get_prop(&mut self, n: &NetRef, name: &str) -> Val {
        let m = name.to_ascii_lowercase();
        let a = Args { exprs: &[], v: vec![] };
        let mut f = dummy_frame(self);
        let r = match net_kind(n) {
            Kind::AlXml => self.al_xml_call(&mut f, n, &m, &a, false),
            Kind::Http => self.http_call(&mut f, n, &m, &a, false),
            Kind::Net => self.dotnet_call(&mut f, n, &m, &a),
        };
        match r {
            Ok(Out::Val(v)) => v,
            _ => Val::Void,
        }
    }

    /// Property assignment: `UriBuilder.Query := X`, `Response.HttpStatusCode := 200`.
    pub fn net_set_prop(&mut self, n: &NetRef, name: &str, v: Val) -> R<()> {
        let m = name.to_ascii_lowercase();
        set_member(name);
        let a = Args { exprs: &[], v: vec![v] };
        let mut f = dummy_frame(self);
        let r = match net_kind(n) {
            Kind::AlXml => self.al_xml_call(&mut f, n, &m, &a, true),
            Kind::Http => self.http_call(&mut f, n, &m, &a, true),
            Kind::Net => self.dotnet_call(&mut f, n, &m, &a),
        };
        match r? {
            Out::Val(_) => Ok(()),
            Out::Unknown => {
                let t = match net_kind(n) {
                    Kind::Net => net_type(&n.borrow()).to_string(),
                    _ => type_name(n).to_string(),
                };
                *self.missing.entry(format!("dotnet {t}.{name} :=")).or_default() += 1;
                crate::assume::hit_member("DotNet.Neutral", &format!("{}.{name} :=", short_name(&t)));
                Ok(())
            }
        }
    }

    /// DotNet member call: constructors and static members on null variables, instance members otherwise.
    fn dotnet_call(&mut self, f: &mut Frame, n: &NetRef, m: &str, a: &Args) -> R<Out> {
        let ty = net_type(&n.borrow());
        // constructor: X := X.TypeName(...)
        if m.eq_ignore_ascii_case(short_name(ty)) && !matches!(ty, "System.Array" | "System.Convert" | "System.Math") {
            return self.dotnet_new(f, ty, a).map(Out::Val);
        }
        if m == "gettype" && (a.is_empty() || ty != "System.Type") {
            return ok(net(NetObj::Prim("System.Type", Val::Text(ty.to_string()))));
        }
        // OutlookCommand (ClientExtensions.dll): static command names
        if ty == "Microsoft.Dynamics.Nav.Client.Hosts.OutlookCommand" {
            let v = match m {
                "newsalesquote" => "New-SalesQuote",
                "newsalesorder" => "New-SalesOrder",
                "newsalesinvoice" => "New-SalesInvoice",
                "newsalescreditmemo" => "New-SalesCreditMemo",
                "newpurchaseinvoice" => "New-PurchaseInvoice",
                "newpurchasecreditmemo" => "New-PurchaseCreditMemo",
                "newpurchaseorder" => "New-PurchaseOrder",
                _ => return Ok(Out::Unknown),
            };
            return ok(Val::Text(v.into()));
        }
        // OfficeHostType (ClientExtensions.dll): static host type names
        if ty == "Microsoft.Dynamics.Nav.Client.Hosts.OfficeHostType" {
            let v = match m {
                "outlookitemread" => "Outlook-ItemRead",
                "outlookitemedit" => "Outlook-ItemEdit",
                "outlookhyperlink" => "Outlook-Hyperlink",
                "outlookpopout" => "Outlook-PopOut",
                "outlooktaskpane" => "Outlook-TaskPane",
                "outlookmobileapp" => "Outlook-MobileApp",
                _ => return Ok(Out::Unknown),
            };
            return ok(Val::Text(v.into()));
        }
        // LastError.Rethrow(): the session's last error again, with its code
        if m == "rethrow" && ty == "Microsoft.Dynamics.Nav.Runtime.LastError" {
            let code = intern_code(&self.last_error_code);
            return Err(Flow::Err(Box::new(AlError { msg: self.last_error.clone(), stack: vec![], code })));
        }
        if m == "equals" && a.len() == 1 {
            return ok(Val::Bool(match a.raw(0) {
                Some(Val::Net(o)) => net_equals(n, o),
                Some(v) => unbox(n).is_some_and(|x| equals(&x, v)),
                None => false,
            }));
        }
        // exception objects of GetLastErrorObject
        let exc = match &*n.borrow() {
            NetObj::ExceptionOf(_, msg, inner) => Some((msg.clone(), inner.clone())),
            _ => None,
        };
        if let Some((msg, inner)) = exc {
            return ok(match m {
                "message" | "tostring" => Val::Text(msg),
                "innerexception" => inner.unwrap_or_else(|| net(NetObj::Null("System.Exception"))),
                "getbaseexception" => {
                    let mut cur = Val::Net(n.clone());
                    loop {
                        let next = match &cur {
                            Val::Net(x) => match &*x.borrow() {
                                NetObj::ExceptionOf(_, _, Some(i)) => Some(i.clone()),
                                _ => None,
                            },
                            _ => None,
                        };
                        match next {
                            Some(i) => cur = i,
                            None => break cur,
                        }
                    }
                }
                "stacktrace" | "source" | "helplink" => Val::Text(String::new()),
                _ => return Ok(Out::Unknown),
            });
        }
        let ns = ty.rsplit_once('.').map(|x| x.0).unwrap_or("");
        let r = match ns {
            // credential type and web service access keys (crate::userauth)
            _ if ty == nav::USER_ACCOUNT_HELPER && matches!(m, "isusernamepasswordauthentication" | "isaccesscontrolserviceauthentication" | "trycreatewebserviceskey" | "tryclearwebserviceskey" | "trygetwebserviceskey") => {
                Out::Val(self.user_account_call(f, m, a)?.unwrap_or(Val::Void))
            }
            // the user's authentication data and companies (crate::sysfuncs user_account_member)
            _ if ty == nav::USER_ACCOUNT_HELPER && crate::sysfuncs::USER_ACCOUNT_MEMBERS.contains(&m) => Out::Val(self.user_account_member(f, m, a)?),
            _ if nav::is_nav_type(ty) => nav::call(self, n, ty, m, a)?,
            _ if ty == crate::environment::TENANT_SETTINGS_HELPER => self.tenant_settings_call(f, m, a)?,
            _ if ty == crate::environment::CONFIG_SETTINGS => self.config_settings_call(m)?,
            _ if crypto::HASH_TYPES.contains(&ty) => crypto::call(n, ty, m, a)?,
            _ if cipher::TYPES.contains(&ty) && cipher::enum_names(ty).is_none() => cipher::call(n, ty, m, a)?,
            _ if asym::TYPES.contains(&ty) => asym::call(n, ty, m, a)?,
            _ if signedxml::TYPES.contains(&ty) => self.sx_call(n, m, a)?,
            // FieldInfo (Type.GetField): GetValue / Name
            "System.Reflection" if ty == "System.Reflection.FieldInfo" => match &*n.borrow() {
                NetObj::Bag(_, p) => {
                    let key = if m == "getvalue" { "value" } else { m };
                    Out::Val(p.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or(Val::Void))
                }
                _ => Out::Unknown,
            },
            _ if x509::TYPES.contains(&ty) && x509::enum_names(ty).is_none() => x509::call(n, ty, m, a)?,
            _ if zip::TYPES.contains(&ty) => zip::call(n, ty, m, a)?,
            _ if xlsx::TYPES.contains(&ty) || xlsx::is_sdk(ty) => xlsx::call(self, n, ty, m, a)?,
            _ if oxml::is_value_type(ty) => match oxml::value_call(n, m, a) {
                Some(r) => r?,
                None => Out::Unknown,
            },
            _ if ty == "Base64Convert" => text::base64_convert(self, f, m, a)?,
            _ if ty == MCP_UTILITIES => mcp_utilities(m, a)?,
            _ if ty == doclayout::WORD_REPORT_MANAGER => doclayout::call(m, a)?,
            "Microsoft.Dynamics.Nav.LicensingService.Model" | "Microsoft.Dynamics.Nav.AzureADGraphClient" | "Microsoft.Dynamics.Nav.CrmCustomizationHelper" => graph::call(self, f, n, ty, m, a)?,
            "Newtonsoft.Json.Linq" | "Newtonsoft.Json" => newtonsoft::call(self, f, n, ty, m, a)?,
            "System.Drawing" | "System.Drawing.Imaging" => drawing::call(n, ty, m, a)?,
            // DataSet.ReadXml / WriteXml read and write the session's files and streams
            "System.Data" if matches!(&*n.borrow(), NetObj::DataSet(_)) && matches!(m, "readxml" | "readxmlschema" | "writexml") => self.dataset_io(n, m, a)?,
            "System.Data" | "Microsoft.Dynamics.Nav.Client.BusinessChart" | "Microsoft.Dynamics.Nav.SystemEnvironment" => data::call(n, ty, m, a)?,
            _ if ty == "System.Type" => data::call(n, ty, m, a)?,
            "System.Net.Mail" => uri::mail_call(n, m, a)?,
            "System.Xml" => self.net_xml_call(f, n, ty, m, a)?,
            "System.Text.RegularExpressions" => regex::call(self, f, n, ty, m, a)?,
            "System.Globalization" => culture::call(n, ty, m, a)?,
            "System.IO.Compression" => crate::gzip::call(n, ty, m, a)?,
            "System.Collections.Generic" | "System.Collections" | "System.Collections.Specialized" => collections::call(self, f, n, ty, m, a)?,
            _ => match ty {
                "System.DateTime" | "System.DateTimeOffset" | "System.TimeSpan" | "System.TimeZoneInfo" | "System.DateTimeKind" | "System.DayOfWeek" => {
                    datetime::call(self, f, n, ty, m, a)?
                }
                "System.Uri" | "System.UriBuilder" | "System.UriKind" | "System.UriPartial" | "System.UriComponents" | "System.UriFormat" | "System.Web.HttpUtility"
                | "System.Net.WebUtility" => uri::call(self, f, n, ty, m, a)?,
                "System.Array" => collections::call(self, f, n, ty, m, a)?,
                "System.IO.Path" | "System.IO.Directory" | "System.IO.DirectoryInfo" | "System.IO.FileInfo" => self.net_io_call(n, ty, m, a)?,
                t if t.ends_with("[]") => collections::call(self, f, n, ty, m, a)?,
                "System.Diagnostics.Stopwatch" | "System.IO.BinaryWriter" | "System.IO.BinaryReader" | text::IN_STREAM_VIEW => text::misc_call(n, ty, m, a)?,
                _ => text::call(self, f, n, ty, m, a)?,
            },
        };
        if let Out::Unknown = r {
            // members every object has
            match m {
                "tostring" if a.is_empty() => return ok(Val::Text(to_text(n))),
                "gethashcode" => return ok(Val::Int(Rc::as_ptr(n) as usize as i32 & 0x7fff_ffff)),
                "dispose" | "close" | "flush" => return void(),
                _ => {}
            }
            if let NetObj::Null(_) = &*n.borrow() {
                if matches!(m, "tostring" | "length" | "count" | "value" | "name") {
                    return null_ref(short_name(ty), m);
                }
            }
        }
        Ok(r)
    }

    /// `new T(...)`.
    fn dotnet_new(&mut self, f: &mut Frame, ty: &'static str, a: &Args) -> R<Val> {
        let _ = f;
        Ok(match ty {
            "System.DateTime" | "System.DateTimeOffset" | "System.TimeSpan" => datetime::construct(ty, a)?,
            "System.Globalization.CultureInfo" => culture::construct(a)?,
            "System.Text.RegularExpressions.Regex" => regex::construct(a)?,
            "System.Uri" | "System.UriBuilder" => uri::construct(ty, a)?,
            t if t.starts_with("Newtonsoft.Json") => newtonsoft::construct(ty, a)?,
            t if t.starts_with("System.Drawing.") => drawing::construct(ty, a)?,
            "System.Net.Mail.MailAddress" => uri::mail_construct(a)?,
            "System.Diagnostics.Stopwatch" => net(NetObj::Stopwatch(None, std::time::Duration::ZERO)),
            t if nav::is_nav_type(t) => {
                if t == nav::PERMISSION_TEST_HELPER {
                    // PermissionTestHelper() calls Clear(): no effective permission sets
                    self.sys.perm.clear_effective();
                }
                nav::construct(ty, a)
            }
            t if crypto::HASH_TYPES.contains(&t) => crypto::construct(ty, a)?,
            t if cipher::TYPES.contains(&t) => cipher::construct(ty, a)?,
            t if asym::TYPES.contains(&t) => asym::construct(ty, a)?,
            t if x509::TYPES.contains(&t) => x509::construct(ty, a)?,
            t if zip::TYPES.contains(&t) => zip::construct(ty, a)?,
            t if xlsx::is_sdk(t) || oxml::is_value_type(t) => oxml::construct(t, a),
            t if signedxml::TYPES.contains(&t) => signedxml::construct(t, a)?,
            t if t.starts_with(graph::MODEL) || t.starts_with(graph::CLIENT) || t.ends_with(".CrmHelper") => graph::construct(ty, a),
            "System.IO.BinaryWriter" => net(NetObj::BinaryWriter(a.raw(0).cloned().unwrap_or(Val::Void))),
            "System.IO.BinaryReader" => net(NetObj::BinaryReader(a.raw(0).cloned().unwrap_or(Val::Void))),
            t if t.starts_with("System.Data.") || t.starts_with("Microsoft.Dynamics.Nav.Client.BusinessChart.") || t == "Microsoft.Dynamics.Nav.SystemEnvironment.SystemEnvironment" => data::construct(ty, a)?,
            "System.IO.DirectoryInfo" | "System.IO.FileInfo" => Session::net_io_new(ty, a),
            "System.IO.Compression.GZipStream" => crate::gzip::construct(a)?,
            "System.Xml.XmlDocument" => net(NetObj::XNode(xml::new_document(), false)),
            "System.Xml.XmlNamespaceManager" => net(NetObj::XNsMgr(Rc::new(RefCell::new(vec![])), false)),
            "System.Xml.NameTable" | "System.Xml.XmlNameTable" => net(NetObj::XNameTable(false)),
            "System.Xml.XmlReaderSettings" | "System.Xml.XmlUrlResolver" | "System.Xml.XmlWriterSettings" => net(NetObj::XmlReaderSettings),
            "System.Xml.XmlTextWriter" | "System.Xml.XmlWriter" => xmlwriter::construct(a),
            "System.Xml.XmlTextReader" => {
                let t = match a.raw(0) {
                    Some(Val::Net(r)) => match &*r.borrow() {
                        NetObj::Reader(_, s, p) => s[s.char_indices().nth(*p).map(|x| x.0).unwrap_or(s.len())..].to_string(),
                        NetObj::Stream(b, _) => String::from_utf8_lossy(b).into_owned(),
                        _ => String::new(),
                    },
                    Some(Val::InStream(s)) => {
                        let st = s.borrow();
                        String::from_utf8_lossy(&st.data[st.pos.min(st.data.len())..]).into_owned()
                    }
                    Some(v) => v.to_text(),
                    None => String::new(),
                };
                xmlreader::new(t)
            }
            _ => text::construct(self, ty, a)?,
        })
    }
}

enum Kind {
    Net,
    AlXml,
    Http,
}

fn net_kind(n: &NetRef) -> Kind {
    match &*n.borrow() {
        NetObj::XNode(_, true) | NetObj::XNodeList(_, true) | NetObj::XAttrs(_, true) | NetObj::XNsMgr(_, true) | NetObj::XNameTable(true) | NetObj::XReadOptions(_) | NetObj::XWriteOptions(_) => {
            Kind::AlXml
        }
        NetObj::HttpClient(_) | NetObj::HttpRequest(_) | NetObj::HttpResponse(_) | NetObj::HttpContent(_) | NetObj::HttpHeaders(_) | NetObj::TestHttpRequest(_) | NetObj::Cookie(..) => Kind::Http,
        _ => Kind::Net,
    }
}

/// A frame for property access outside an AL call (no locals; `out` arguments are not used).
fn dummy_frame(s: &mut Session) -> Frame {
    let code = s.test.as_ref().map(|t| t.inst.code).unwrap_or(0);
    Frame {
        code,
        locals: vec![],
        globals: Rc::new(Globals { code, vars: vec![] }),
        rec: None,
        xrec: None,
        with: vec![],
        inst: None,
        ret: None,
        ret_ty: None,
        curr_field: 0,
    }
}

/// .NET enum value: `RegexOptions.IgnoreCase`, `UriKind.Absolute`, ... as a boxed Int32.
pub fn enum_val(t: &'static str, v: i64) -> Val {
    net(NetObj::Prim(t, Val::Int(v as i32)))
}

/// A `DotNet X` parameter: passed as a shared wrapper (assignments reach the caller).
pub fn is_dotnet_param(t: &al_syntax::TypeRef) -> bool {
    matches!(t, al_syntax::TypeRef::Object { kind, .. } if kind == "dotnet")
}

#[cfg(test)]
mod error_code_tests {
    use super::dotnet_error_code as code;

    #[test]
    fn dotnet_errors_get_bc_codes() {
        // NavSession.GetLastErrorCode: DotNetInvoke:<inner exception without "Exception">
        assert_eq!(code("XmlTextReader", "Could not find file 'C:\\x.xml'."), "DotNetInvoke:FileNotFound");
        assert_eq!(code("Convert", "Input string was not in a correct format."), "DotNetInvoke:Format");
        assert_eq!(code("Convert", "Value was either too large or too small for an Int16."), "DotNetInvoke:Overflow");
        assert_eq!(code("Array", "Index was outside the bounds of the array."), "DotNetInvoke:IndexOutOfRange");
        assert_eq!(code("JArray", "Index was out of range. Must be non-negative and less than the size of the collection. (Parameter 'index')"), "DotNetInvoke:ArgumentOutOfRange");
        assert_eq!(code("Dictionary", "The given key 'a' was not present in the dictionary."), "DotNetInvoke:KeyNotFound");
        assert_eq!(code("Queue", "Queue empty."), "DotNetInvoke:InvalidOperation");
        assert_eq!(code("UriBuilder", "Invalid URI: The URI scheme is not valid. (Parameter 'value')"), "DotNetInvoke:Argument");
        assert_eq!(code("Uri", "Invalid URI: The format of the URI could not be determined."), "DotNetInvoke:UriFormat");
        assert_eq!(code("Regex", "Invalid pattern '(' at offset 1. Not enough )'s."), "DotNetInvoke:RegexParse");
        assert_eq!(code("XmlDocument", "Data at the root level is invalid. Line 1, position 1."), "DotNetInvoke:Xml");
        // an unclassified System.Exception: its name is all "Exception"
        assert_eq!(code("WorkbookReader", "The worksheet was not found."), "DotNetInvoke:");
    }
}
