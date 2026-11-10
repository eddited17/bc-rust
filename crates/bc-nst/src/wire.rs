//! JSON wire helpers shared by the NST endpoints: the `$polyType` and
//! `typeHint` wrappers, JSON-RPC envelopes, and `ServerFault` errors.
//!
//! The web server (StreamJsonRpc + Newtonsoft) reads several of these with
//! hand-written converters that walk tokens in a fixed order, so property
//! order matters. That relies on serde_json's `preserve_order` feature (enabled
//! in this crate's Cargo.toml); the unit tests below fail without it.
//!
//! Shapes come from the BC 29 web-server assemblies (`Microsoft.Dynamics.Nav.Types`:
//! `NavPolymorphicJsonConvert`, `ObjectWithTypeInfoConverter`, `JsonTypeHintHelper`,
//! `ExceptionHandler`, `ServerFault`, `NavBaseException` and subclasses;
//! `Microsoft.Dynamics.Nav.ClientServer.JsonRpc.ClientServerJsonRpc`).

use base64::Engine as _;
use serde_json::{json, Map, Value};

/// Full name of the assembly that defines the exception types. The web server's
/// `NavExceptionSerializationBinder` accepts this exact name, and falls back to
/// `Type.GetType(typeName)` inside the same assembly for other names.
pub const TYPES_ASSEMBLY: &str = "Microsoft.Dynamics.Nav.Types, Version=29.0.0.0, Culture=neutral, PublicKeyToken=31bf3856ad364e35";

/// JSON-RPC error code StreamJsonRpc uses for exceptions thrown by a target
/// method (`JsonRpcErrorCode.InvocationError`). `ClientServerJsonRpc` always uses it.
pub const INVOCATION_ERROR: i64 = -32000;

// ---------------------------------------------------------------------------
// $polyType

/// `{"$polyType": "<full .NET type name>", "obj": {...}}`, written by
/// `NavPolymorphicJsonConvert` for every type in a `[NavJsonDerivedType]` hierarchy,
/// including when the declared type equals the concrete type.
pub fn poly(type_name: &str, obj: Value) -> Value {
    let mut m = Map::new();
    m.insert("$polyType".into(), Value::String(type_name.into()));
    m.insert("obj".into(), obj);
    Value::Object(m)
}

/// Reverse of [`poly`]: returns `(type name, obj)`.
pub fn unpoly(v: &Value) -> Option<(&str, &Value)> {
    Some((v.get("$polyType")?.as_str()?, v.get("obj")?))
}

// ---------------------------------------------------------------------------
// typeHint

/// `Microsoft.Dynamics.Nav.Types.JsonTypeHint`. On the wire the hint is the
/// integer value, not a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum TypeHint {
    None = 0,
    Int = 1,
    Short = 2,
    Float = 3,
    Decimal = 4,
    UInt = 5,
    UShort = 6,
    ULong = 7,
    Byte = 8,
    Sbyte = 9,
    Guid = 10,
    ByteArray = 11,
    DateTime = 12,
    /// value is a string `"<enum full type name>.<member name>"`
    Enum = 13,
    Uri = 14,
    NotificationInfo = 15,
    ErrorInfoData = 16,
    AutomationHandle = 17,
    UploadedTempFile = 18,
    Bool = 19,
    JsonSerializableObjectContract = 20,
    String = 21,
    TimeSpan = 22,
    Int64 = 23,
    Double = 24,
    Null = 25,
    DateTimeDataContract = 26,
    Char = 27,
}

impl TypeHint {
    pub fn from_i64(n: i64) -> Option<TypeHint> {
        use TypeHint::*;
        const ALL: [TypeHint; 28] = [
            None, Int, Short, Float, Decimal, UInt, UShort, ULong, Byte, Sbyte, Guid, ByteArray, DateTime, Enum, Uri,
            NotificationInfo, ErrorInfoData, AutomationHandle, UploadedTempFile, Bool, JsonSerializableObjectContract,
            String, TimeSpan, Int64, Double, Null, DateTimeDataContract, Char,
        ];
        ALL.get(usize::try_from(n).ok()?).copied()
    }
}

/// `{"typeHint": <int>, "value": ...}` for `object`-typed arguments and results
/// (`ObjectWithTypeInfoConverter`). The reader takes the first property as the
/// hint and the second as the value without looking at names, so the order is fixed.
pub fn type_hint(hint: TypeHint, value: Value) -> Value {
    let mut m = Map::new();
    m.insert("typeHint".into(), json!(hint as i32));
    m.insert("value".into(), value);
    Value::Object(m)
}

/// Wrap a plain JSON value with the hint the .NET side would compute for the
/// equivalent CLR value (strings, bools, null, int/long/double).
pub fn hinted(value: Value) -> Value {
    let hint = match &value {
        Value::Null => TypeHint::Null,
        Value::Bool(_) => TypeHint::Bool,
        Value::String(_) => TypeHint::String,
        Value::Number(n) if n.is_i64() || n.is_u64() => match n.as_i64() {
            Some(i) if i32::try_from(i).is_ok() => TypeHint::Int,
            _ => TypeHint::Int64,
        },
        Value::Number(_) => TypeHint::Double,
        Value::Array(_) | Value::Object(_) => TypeHint::JsonSerializableObjectContract,
    };
    type_hint(hint, value)
}

/// `object[]` arguments (`ObjectArrayWithTypeInfoConverter`): an array of typeHint wrappers.
pub fn hinted_array(values: impl IntoIterator<Item = Value>) -> Value {
    Value::Array(values.into_iter().map(hinted).collect())
}

/// Read a typeHint wrapper. Positional, like the .NET reader: first property is
/// the hint, second the value.
pub fn read_type_hint(v: &Value) -> Result<(TypeHint, &Value), String> {
    let m = v.as_object().ok_or("typeHint wrapper is not an object")?;
    let mut it = m.iter();
    let (_, h) = it.next().ok_or("typeHint wrapper is empty")?;
    let (_, val) = it.next().ok_or("typeHint wrapper has no value")?;
    let n = h.as_i64().ok_or("typeHint is not an integer")?;
    let hint = TypeHint::from_i64(n).ok_or_else(|| format!("unknown typeHint {n}"))?;
    Ok((hint, val))
}

// ---------------------------------------------------------------------------
// JSON-RPC envelopes

/// Successful response. `MetadataToken` is a top-level property the web server's
/// `JsonRpcMetadataTokenInspector` reads off every incoming message.
pub fn rpc_result(id: &Value, result: Value, metadata_token: Option<i64>) -> Value {
    let mut m = Map::new();
    m.insert("jsonrpc".into(), json!("2.0"));
    m.insert("id".into(), id.clone());
    m.insert("result".into(), result);
    if let Some(t) = metadata_token {
        add_tokens(&mut m, t);
    }
    Value::Object(m)
}

/// A request from bc-nst to the web server (callbacks such as `Confirm`): the real NST
/// numbers its own requests and puts the same top-level tokens on them as on results.
pub fn rpc_request(id: &Value, method: &str, params: Value, metadata_token: i64) -> Value {
    let mut m = Map::new();
    m.insert("jsonrpc".into(), json!("2.0"));
    m.insert("id".into(), id.clone());
    m.insert("method".into(), json!(method));
    m.insert("params".into(), params);
    add_tokens(&mut m, metadata_token);
    Value::Object(m)
}

/// The top-level tokens the real NST puts on every message (BC 29.1 capture). The web
/// server compares them with its cached values: a changed User/Agent token makes it
/// reload users and agents, a changed MetadataToken its metadata. bc-rust has no
/// agents and no permission changes at runtime, so those stay constant.
fn add_tokens(m: &mut Map<String, Value>, metadata_token: i64) {
    m.insert("MetadataToken".into(), json!(metadata_token));
    m.insert("UserToken".into(), json!(0));
    m.insert("AgentToken".into(), json!(0));
    m.insert("AgentTaskToken".into(), json!(0));
    m.insert("AgentPageRefreshToken".into(), json!(0));
    m.insert("PermissionToken".into(), json!(1));
}

/// Error response carrying a `ServerFault` in `error.data`, the shape
/// `ClientServerJsonRpc.CreateErrorDetails` produces.
pub fn rpc_fault(id: &Value, fault: &Fault, metadata_token: Option<i64>) -> Value {
    let mut err = Map::new();
    err.insert("code".into(), json!(INVOCATION_ERROR));
    err.insert("message".into(), json!(fault.message()));
    err.insert("data".into(), fault.server_fault());
    let mut m = Map::new();
    m.insert("jsonrpc".into(), json!("2.0"));
    m.insert("id".into(), id.clone());
    m.insert("error".into(), Value::Object(err));
    if let Some(t) = metadata_token {
        add_tokens(&mut m, t);
    }
    Value::Object(m)
}

/// An empty `ServerResponse` body: `requests` must be present and never null.
pub fn server_response(version: i64) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("requests".into(), json!([]));
    m.insert("VersionNumber".into(), json!(version));
    m
}

// ---------------------------------------------------------------------------
// ServerFault

/// .NET exception types bc-nst sends. All are `NavBaseException` subclasses, which
/// is what the web server needs to treat a fault as recoverable
/// (`ConnectionStateManager.HandleServerFault`; anything else aborts the channel).
/// Both have a `(string, Exception)` constructor, which `NavBaseException.Clone`
/// calls when the web server unwraps the fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// `NavNCLDialogException`: what AL `Error()`, `TestField` and relation errors
    /// raise. The web client shows it as a normal error dialog and keeps the session.
    Dialog,
    /// `NavNCLNotSupportedOperationException`: a `NavNCLException`, shown as an
    /// error popup. Used for IClientApi methods bc-nst doesn't implement yet.
    NotSupported,
    /// `NavNSValidateFieldException`: wraps the error of a failed ValidateField (the AL
    /// error is its InnerException). The web client shows it on the field instead of a
    /// modal dialog (BC 29.1 capture, data/capture/nst-real/samples/15).
    ValidateField,
    /// `NavMetadataNotFoundException`: the metadata endpoints' answer (HTTP 521) for an
    /// object id that does not exist. The web client shows an error and keeps the session
    /// (BC 29.1 capture, data/capture/nst-real/samples/19).
    MetadataNotFound,
}

impl FaultKind {
    pub fn type_name(self) -> &'static str {
        match self {
            FaultKind::Dialog => "Microsoft.Dynamics.Nav.Types.Exceptions.NavNCLDialogException",
            FaultKind::NotSupported => "Microsoft.Dynamics.Nav.Types.Exceptions.NavNCLNotSupportedOperationException",
            FaultKind::ValidateField => "Microsoft.Dynamics.Nav.Types.Exceptions.NavNSValidateFieldException",
            FaultKind::MetadataNotFound => "Microsoft.Dynamics.Nav.Types.NavMetadataNotFoundException",
        }
    }
}

/// One .NET exception, serialized the way `ExceptionHandler` does it
/// (Newtonsoft, `TypeNameHandling.Objects`, ISerializable contract).
#[derive(Debug, Clone)]
pub struct WireException {
    pub kind: FaultKind,
    pub message: String,
    /// `NavBaseException.ALCallStack`, newline separated.
    pub al_call_stack: Option<String>,
    /// `NavBaseException.DetailedErrorMessage`.
    pub detailed_message: Option<String>,
    /// `NavNCLDialogException.AutomationId` (a GUID string).
    pub automation_id: Option<String>,
    /// `InnerException`, nested in this object (without `$type`, as the real NST writes
    /// it) and also the next element of the chain.
    pub inner: Option<Box<WireException>>,
}

/// COR_E_EXCEPTION, the HResult of an exception created from a message.
const HRESULT_EXCEPTION: i64 = -2146233088;

impl WireException {
    pub fn dialog(message: impl Into<String>) -> WireException {
        WireException { kind: FaultKind::Dialog, message: message.into(), al_call_stack: None, detailed_message: None, automation_id: None, inner: None }
    }

    pub fn not_supported(message: impl Into<String>) -> WireException {
        WireException { kind: FaultKind::NotSupported, ..WireException::dialog(message) }
    }

    /// `NavMetadataNotFoundException` for `<kind> <id>` (e.g. "Page 9176").
    pub fn metadata_not_found(object: &str) -> WireException {
        WireException { kind: FaultKind::MetadataNotFound, ..WireException::dialog(format!("The metadata object {object} was not found.")) }
    }

    /// The JSON object Newtonsoft writes for this exception. `$type` must come
    /// first (Newtonsoft only reads metadata properties at the start of an object).
    /// Entries follow `GetObjectData` order: NavBaseException's own values, then
    /// System.Exception's, then the subclass's. The deserialization constructors
    /// require SuppressMessage, FatalityScope, Message, InnerException, HelpURL,
    /// StackTraceString, RemoteStackTraceString, HResult and Source; the rest are
    /// read only when present.
    pub fn to_json(&self) -> Value {
        let tn = self.kind.type_name();
        let mut m = Map::new();
        m.insert("$type".into(), json!(format!("{tn}, {TYPES_ASSEMBLY}")));
        // NavBaseException.GetObjectData
        m.insert("SuppressMessage".into(), json!(false));
        // NavExceptionFatalityScope.Call (1), as the NavNCLDialogException(string) constructor sets it
        m.insert("FatalityScope".into(), json!(if self.kind == FaultKind::MetadataNotFound { 0 } else { 1 }));
        m.insert("errorLevel".into(), json!(0));
        m.insert("DiagnosticsSuppress".into(), json!(matches!(self.kind, FaultKind::ValidateField | FaultKind::MetadataNotFound)));
        m.insert("DiagnosticsMessage".into(), json!(self.message));
        m.insert("TransientDiagnosticsMessage".into(), json!(""));
        m.insert("SuppressExceptionCreatedEvent".into(), json!(false));
        m.insert("ALCallStack".into(), json!(self.al_call_stack));
        m.insert("detailedErrorMessage".into(), json!(self.detailed_message));
        m.insert("EnglishLanguageDiagnosticsMessage".into(), Value::Null);
        m.insert("UntrappableError".into(), json!(false));
        // System.Exception.GetObjectData
        m.insert("ClassName".into(), json!(tn));
        m.insert("Message".into(), json!(self.message));
        m.insert("Data".into(), Value::Null);
        m.insert(
            "InnerException".into(),
            match &self.inner {
                Some(i) => {
                    let mut j = i.to_json();
                    if let Some(o) = j.as_object_mut() {
                        o.remove("$type");
                    }
                    j
                }
                None => Value::Null,
            },
        );
        m.insert("HelpURL".into(), Value::Null);
        m.insert("StackTraceString".into(), Value::Null);
        m.insert("RemoteStackTraceString".into(), Value::Null);
        m.insert("RemoteStackIndex".into(), json!(0));
        m.insert("ExceptionMethod".into(), Value::Null);
        m.insert("HResult".into(), json!(HRESULT_EXCEPTION));
        m.insert("Source".into(), Value::Null);
        m.insert("WatsonBuckets".into(), Value::Null);
        if self.kind == FaultKind::Dialog {
            // NavNCLDialogException.GetObjectData (AppId, Version, ObjectType and
            // errorInfoData are optional on read and left out)
            m.insert("AutomationId".into(), json!(self.automation_id));
            m.insert("ApplicationInsightsConnectionString".into(), Value::Null);
            m.insert("ExtensionName".into(), Value::Null);
            m.insert("ObjectName".into(), Value::Null);
            m.insert("Publisher".into(), Value::Null);
            m.insert("ObjectId".into(), json!(0));
        }
        Value::Object(m)
    }
}

/// A fault to send back for a failed call: an exception chain (outermost first)
/// plus the response queue that `ServerFault.responseQueue` carries.
#[derive(Debug, Clone)]
pub struct Fault {
    pub chain: Vec<WireException>,
    /// Callbacks queued before the error (`ServerResponse`, processed by the web
    /// server even when the call failed). Empty for now.
    pub response_queue: Map<String, Value>,
}

impl Fault {
    pub fn new(e: WireException) -> Fault {
        Fault { chain: vec![e], response_queue: server_response(0) }
    }

    /// An AL runtime error (`Error()`, TestField, relation checks, ...).
    pub fn al(msg: impl Into<String>) -> Fault {
        Fault::new(WireException::dialog(msg))
    }

    /// An IClientApi method bc-nst doesn't implement yet. Answering with
    /// JSON-RPC "method not found" would end the browser session; this doesn't.
    pub fn not_implemented(method: &str) -> Fault {
        Fault::new(WireException::not_supported(format!("The operation '{method}' is not supported by this server (bc-rust) yet.")))
    }

    /// A failed ValidateField: `NavNSValidateFieldException` around the cause.
    pub fn validate_field(cause: Fault) -> Fault {
        let first = cause.chain.first().cloned();
        let outer = WireException {
            kind: FaultKind::ValidateField,
            message: cause.message().to_string(),
            al_call_stack: first.as_ref().and_then(|e| e.al_call_stack.clone()),
            detailed_message: None,
            automation_id: None,
            inner: first.map(Box::new),
        };
        let mut chain = vec![outer];
        chain.extend(cause.chain);
        Fault { chain, response_queue: cause.response_queue }
    }

    pub fn message(&self) -> &str {
        self.chain.first().map(|e| e.message.as_str()).unwrap_or("")
    }

    /// `exceptionData`: base64 of the UTF-8 JSON array `ExceptionHandler` writes,
    /// one object per exception in the InnerException chain.
    pub fn exception_data(&self) -> String {
        let arr = Value::Array(self.chain.iter().map(WireException::to_json).collect());
        base64::engine::general_purpose::STANDARD.encode(arr.to_string())
    }

    /// The `ServerFault` data contract: `{"exceptionData": "...", "responseQueue": {...}}`.
    pub fn server_fault(&self) -> Value {
        let mut m = Map::new();
        m.insert("exceptionData".into(), json!(self.exception_data()));
        m.insert("responseQueue".into(), Value::Object(self.response_queue.clone()));
        Value::Object(m)
    }
}

impl From<&al_runtime::AlError> for Fault {
    fn from(e: &al_runtime::AlError) -> Fault {
        let mut x = WireException::dialog(e.msg.clone());
        if !e.stack.is_empty() {
            x.al_call_stack = Some(e.stack.join("\n"));
        }
        Fault::new(x)
    }
}

impl From<al_runtime::Flow> for Fault {
    fn from(f: al_runtime::Flow) -> Fault {
        match f {
            al_runtime::Flow::Err(e) => Fault::from(&*e),
            // Exit/Break/Continue escaping a trigger: not an error the user can act on
            _ => Fault::al("Internal error: unexpected control flow out of an AL trigger."),
        }
    }
}

/// Decode `exceptionData` back to the JSON array (tests, logging).
pub fn decode_exception_data(s: &str) -> Result<Value, String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(s).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// .NET value formats

/// `TimeZoneInfo.ToSerializedString()` for UTC, the format
/// `UserSettings.serializedTimeZoneInfo` carries:
/// `Id;BaseUtcOffsetMinutes;DisplayName;StandardName;DaylightName;[rules];`
pub fn utc_time_zone(id: &str) -> String {
    format!("{};0;(UTC) Coordinated Universal Time;Coordinated Universal Time;Coordinated Universal Time;;", escape_tz(id))
}

/// TimeZoneInfo string serializer escapes `\`, `;`, `[` and `]` with a backslash.
fn escape_tz(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | ';' | '[' | ']') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

/// Newtonsoft's TimeSpan format (`TimeSpan.ToString()`, "c"): `[d.]hh:mm:ss`.
pub fn timespan(secs: u64) -> String {
    let (d, r) = (secs / 86400, secs % 86400);
    let hms = format!("{:02}:{:02}:{:02}", r / 3600, r % 3600 / 60, r % 60);
    if d > 0 {
        format!("{d}.{hms}")
    } else {
        hms
    }
}

pub const NIL_GUID: &str = "00000000-0000-0000-0000-000000000000";

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(v: &Value) -> Vec<&str> {
        v.as_object().unwrap().keys().map(|k| k.as_str()).collect()
    }

    #[test]
    fn type_hint_order_is_fixed() {
        let v = type_hint(TypeHint::Int, json!(5));
        assert_eq!(keys(&v), ["typeHint", "value"]);
        assert_eq!(v.to_string(), r#"{"typeHint":1,"value":5}"#);
        assert_eq!(read_type_hint(&v).unwrap(), (TypeHint::Int, &json!(5)));
        assert_eq!(hinted(json!("x"))["typeHint"], 21);
        assert_eq!(hinted(json!(null))["typeHint"], 25);
        assert_eq!(hinted(json!(5_000_000_000i64))["typeHint"], 23);
    }

    #[test]
    fn poly_wrapper() {
        let v = poly("Microsoft.Dynamics.Nav.Types.Metadata.MasterPage", json!({"EDSpec": 1, "IDED_1": 22}));
        assert_eq!(keys(&v), ["$polyType", "obj"]);
        assert_eq!(unpoly(&v).unwrap().0, "Microsoft.Dynamics.Nav.Types.Metadata.MasterPage");
    }

    #[test]
    fn preserve_order_is_enabled() {
        // NavDataSet's reader walks compressed, count, buffers positionally
        let mut m = Map::new();
        m.insert("compressed".into(), json!(false));
        m.insert("count".into(), json!(0));
        m.insert("buffers".into(), json!([]));
        assert_eq!(Value::Object(m).to_string(), r#"{"compressed":false,"count":0,"buffers":[]}"#);
    }

    #[test]
    fn al_error_fault() {
        let mut e = al_runtime::AlError::new("Name must have a value in Customer: No.=C10000.".into());
        e.stack = vec!["Customer.OnInsert".into(), "Page 21".into()];
        let f = Fault::from(&e);
        let r = rpc_fault(&json!(7), &f, None);
        assert_eq!(r["error"]["code"], -32000);
        assert_eq!(r["error"]["message"], e.msg.as_str());
        let sf = &r["error"]["data"];
        assert_eq!(keys(sf), ["exceptionData", "responseQueue"]);
        assert_eq!(sf["responseQueue"]["requests"], json!([]));
        let arr = decode_exception_data(sf["exceptionData"].as_str().unwrap()).unwrap();
        let x = &arr[0];
        assert_eq!(keys(x)[0], "$type");
        assert_eq!(
            x["$type"],
            "Microsoft.Dynamics.Nav.Types.Exceptions.NavNCLDialogException, Microsoft.Dynamics.Nav.Types, Version=29.0.0.0, Culture=neutral, PublicKeyToken=31bf3856ad364e35"
        );
        for k in ["SuppressMessage", "FatalityScope", "Message", "InnerException", "HelpURL", "StackTraceString", "RemoteStackTraceString", "HResult", "Source"] {
            assert!(x.get(k).is_some(), "missing {k}");
        }
        assert_eq!(x["Message"], e.msg.as_str());
        assert_eq!(x["ALCallStack"], "Customer.OnInsert\nPage 21");
    }

    #[test]
    fn utc_zone_string() {
        assert_eq!(utc_time_zone("UTC"), "UTC;0;(UTC) Coordinated Universal Time;Coordinated Universal Time;Coordinated Universal Time;;");
        assert_eq!(timespan(1200), "00:20:00");
        assert_eq!(timespan(90061), "1.01:01:01");
    }
}
