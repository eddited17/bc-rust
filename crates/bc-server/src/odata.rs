//! Business Central API v2.0 (`/api/v2.0/companies({id})/<entitySet>`), served
//! from the AL API pages of the APIV2 app with the page record lifecycle
//! (field OnValidate, OnInsertRecord / OnModifyRecord / OnDeleteRecord), and their
//! bound actions (`POST .../<entitySet>(<id>)/Microsoft.NAV.<name>`, see [`bound_action`]).

use crate::api::{ApiErr, Ctx};
use al_model::{Control, FieldClass, FieldDef, FieldType, PageDef};
use al_syntax::{Expr, ProcKind, TypeRef};
use al_runtime::record::{text_to_store, to_store, to_val};
use al_runtime::{Instance, RecHandle, Session, Val};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value as J};
use std::collections::HashMap;

pub const COMPANY_ID: &str = "8f8e6f3c-1a6b-4d5e-9c3b-2a1d0e7f6a55";

/// An error with BC's OData error body `{"error": {"code", "message"}}`; the code is the one BC gives the
/// matching exception (ExceptionExtensions: NavODataBadRequestException BadRequest, NavODataNotFoundException
/// BadRequest_NotFound, NavODataMethodNotAllowedException BadRequest_MethodNotAllowed, a permission error 403
/// Internal_ServerError as measured on BC 29.1, ...). BC appends "  CorrelationId:  <guid>." to the message;
/// bc-rust has no correlation ids.
fn err(code: StatusCode, msg: impl Into<String>) -> ApiErr {
    let c = match code.as_u16() {
        400 => "BadRequest",
        403 | 500 => "Internal_ServerError",
        404 => "BadRequest_NotFound",
        405 => "BadRequest_MethodNotAllowed",
        409 => "Internal_EntityWithSameKeyExists",
        503 => "Application_ServiceUnavailable",
        _ => "Unknown",
    };
    ApiErr(code, msg.into(), Some(c))
}

/// "Resource not found for the segment '<name>'." with error code BadRequest_ResourceNotFound
/// [src:X02] (the 404 status is not verified): a GET names the entity set, a write the entity
/// (EntityType.Name).
fn not_found(p: &PageDef, write: bool) -> ApiErr {
    let prop = if write { "entityname" } else { "entitysetname" };
    let name = p.props.get(prop).or_else(|| p.props.get("entitysetname")).map(|n| n.trim_matches(['\'', '"']).to_string()).unwrap_or_default();
    ApiErr(StatusCode::NOT_FOUND, format!("Resource not found for the segment '{name}'."), Some("BadRequest_ResourceNotFound"))
}

/// BC's OData error code `Request_EntityChanged`, with HTTP 409 [src:X03].
const ENTITY_CHANGED: &str = "Request_EntityChanged";

fn al(e: al_runtime::Flow) -> ApiErr {
    match e {
        // the record changed between this request's read and its write (a commit of another
        // session after the If-Match check): 409 like a stale If-Match, with the AL message.
        // BC's own status for this case is not verified (I15 W3).
        al_runtime::Flow::Err(e) if e.code == "DB:RecordChanged" => ApiErr(StatusCode::CONFLICT, e.msg, Some(ENTITY_CHANGED)),
        // a permission error raised by AL code: 403 Internal_ServerError, as BC 29.1 answers its permission errors
        al_runtime::Flow::Err(e) if e.code == "Permission" => ApiErr(StatusCode::FORBIDDEN, e.msg, Some("Internal_ServerError")),
        // BC's codes by exception (ExceptionExtensions): Error() and FieldError are NavNCLDialogException,
        // TestField NavTestFieldException, a failed Get NavCSideRecordNotFoundException, other database
        // errors NavCSideException (Internal_ServerError)
        al_runtime::Flow::Err(e) => {
            let code = match e.code {
                "TestField" => "Application_FieldValidationException",
                "DB:RecordNotFound" => "Internal_RecordNotFound",
                c if c.starts_with("DB:") => "Internal_ServerError",
                _ => "Application_DialogException",
            };
            ApiErr(StatusCode::BAD_REQUEST, e.msg, Some(code))
        }
        _ => err(StatusCode::INTERNAL_SERVER_ERROR, "unexpected control flow"),
    }
}

/// API v2.0 pages by entity set name.
fn api_page(ctx: &Ctx, set: &str) -> Option<&'static PageDef> {
    ctx.rt.cat.pages.values().find(|p| {
        p.page_type.eq_ignore_ascii_case("api")
            && p.props.get("apiversion").is_some_and(|v| v.contains("v2.0"))
            && !p.props.contains_key("apipublisher")
            && p.props.get("entitysetname").is_some_and(|v| v.eq_ignore_ascii_case(set))
    })
}

/// (json name, field index, control) for every Rec-bound field of the page, in page order.
fn api_fields(p: &PageDef) -> Vec<(&str, usize, &Control)> {
    api_controls(p).into_iter().filter_map(|(n, fi, c)| fi.map(|fi| (n, fi, c))).collect()
}

/// (json name, field index or None for a page variable, control) for every field control of
/// the page, in page order: Rec-bound fields and fields on a page variable
/// (`field(currencyCode; CurrencyCodeTxt)`), which BC's OData serves like table fields.
fn api_controls(p: &PageDef) -> Vec<(&str, Option<usize>, &Control)> {
    fn walk<'a>(cs: &'a [Control], out: &mut Vec<(&'a str, Option<usize>, &'a Control)>) {
        for c in cs {
            if c.kind == "field" {
                if let Some(fi) = c.field {
                    out.push((c.name.as_str(), Some(fi), c));
                } else if matches!(c.source_expr, Some(Expr::Ident(_))) {
                    out.push((c.name.as_str(), None, c));
                }
            } else if c.kind != "part" {
                walk(&c.children, out);
            }
        }
    }
    let mut v = vec![];
    walk(&p.layout, &mut v);
    v
}

/// The page variable behind a field control on a page variable: (slot, declared type).
fn page_var(s: &mut Session, inst: &std::rc::Rc<Instance>, c: &Control) -> Option<(al_runtime::value::Slot, Option<&'static TypeRef>)> {
    let Some(Expr::Ident(var)) = &c.source_expr else { return None };
    let g = s.instance_globals(inst);
    g.vars.iter().find(|x| x.name.eq_ignore_ascii_case(var)).map(|x| (x.slot.clone(), x.ty))
}

fn guid_text(g: &[u8; 16]) -> String {
    al_runtime::value::format_guid(g).trim_matches(['{', '}']).to_lowercase()
}

fn to_json(s: &Session, fd: &FieldDef, v: &Val) -> J {
    match v {
        Val::Bool(b) => json!(b),
        Val::Int(i) => json!(i),
        Val::BigInt(i) => json!(i),
        Val::Dec(d) => serde_json::from_str(&d.normalize().to_string()).unwrap_or(json!(0)),
        Val::Guid(g) => json!(guid_text(g)),
        Val::Date(_) | Val::DateTime(_) => json!(al_runtime::builtins::format_val(v, 9)),
        Val::Opt { .. } => json!(s.format_field_val(fd, v).replace(' ', "_x0020_")),
        other => json!(other.to_text()),
    }
}

fn entity_json(s: &mut Session, p: &PageDef, inst: &std::rc::Rc<Instance>, h: &RecHandle) -> Result<J, ApiErr> {
    let mut o = Map::new();
    for (name, fi, c) in api_controls(p) {
        let Some(fi) = fi else {
            // a page variable (set by the page's OnAfterGetRecord, e.g. SetCalculatedFields)
            let v = page_var(s, inst, c).map(|(sl, _)| sl.borrow().clone()).unwrap_or(Val::Text(String::new()));
            o.insert(name.to_string(), val_json(&v));
            continue;
        };
        if h.borrow().field(fi).class == FieldClass::FlowField {
            s.calc_field(h, fi).map_err(al)?;
        }
        let (fd, v) = {
            let r = h.borrow();
            (r.field(fi), r.get_val(fi))
        };
        o.insert(name.to_string(), to_json(s, fd, &v));
    }
    o.insert("@odata.etag".into(), json!(format!("W/\"{}\"", row_version(h))));
    Ok(J::Object(o))
}

/// The entity's version for its ETag: the record's SystemRowVersion (0 without one).
fn row_version(h: &RecHandle) -> i64 {
    let r = h.borrow();
    r.tdef.field("SystemRowVersion").map(|i| r.get_val(i).as_i64()).unwrap_or(0)
}

/// The `If-Match` header of a PATCH or DELETE.
#[derive(Debug, PartialEq)]
enum IfMatch {
    Absent,
    /// `*`: whatever version the entity has
    Any,
    /// `W/"<version>"` or `"<version>"`, as `@odata.etag` gave it
    Version(i64),
}

/// "Could not validate the client concurrency token ...": HTTP 400, error code BadRequest_InvalidToken
/// [src:X03].
fn invalid_token() -> ApiErr {
    ApiErr(StatusCode::BAD_REQUEST, "Could not validate the client concurrency token required by the service. Please provide a valid token in the client request.".into(), Some("BadRequest_InvalidToken"))
}

/// Read `If-Match` as BC's PageDataProvider.EnsureConcurrenyTokenSpecified checks it: `*`, or a
/// well-formed ETag with a valid value (here: the row version `@odata.etag` carries); anything
/// else is the invalid-token error.
fn if_match(headers: &HeaderMap) -> Result<IfMatch, ApiErr> {
    let Some(v) = headers.get(axum::http::header::IF_MATCH) else { return Ok(IfMatch::Absent) };
    let t = v.to_str().map_err(|_| invalid_token())?.trim();
    if t == "*" {
        return Ok(IfMatch::Any);
    }
    let quoted = t.strip_prefix("W/").unwrap_or(t);
    let inner = quoted.strip_prefix('"').and_then(|x| x.strip_suffix('"')).ok_or_else(invalid_token)?;
    inner.parse::<i64>().map(IfMatch::Version).map_err(|_| invalid_token())
}

/// The entity read for a PATCH or DELETE against the request's If-Match: a version other than
/// the entity's is "Another user has already changed the record.", 409 Request_EntityChanged
/// [src:X03].
fn check_version(h: &RecHandle, m: &IfMatch) -> Result<(), ApiErr> {
    match m {
        IfMatch::Version(v) if *v != row_version(h) => Err(ApiErr(StatusCode::CONFLICT, "Another user has already changed the record.".into(), Some(ENTITY_CHANGED))),
        _ => Ok(()),
    }
}

fn json_input(s: &Session, fd: &FieldDef, v: &J) -> Result<Val, ApiErr> {
    let txt = json_text(v);
    let txt = if matches!(fd.ty, FieldType::Date) && txt.len() == 10 && txt.as_bytes()[4] == b'-' { txt } else { txt };
    let evf = |n: &str| s.enum_ordinal_for_field(fd, n);
    let sv = text_to_store(fd, &txt, &evf, s.work_date).ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("'{txt}' is not a valid value for {}", fd.name)))?;
    Ok(to_val(fd, &sv))
}

/// Run a page trigger. OnInsertRecord / OnModifyRecord / OnDeleteRecord return true unless
/// they exit(false), as in BC: the journal-line page's OnInsertRecord re-initializes Rec and has
/// no exit, so the platform inserts it (the default false stored nothing and still answered 201).
fn page_trigger(s: &mut Session, p: &PageDef, inst: &std::rc::Rc<Instance>, h: &RecHandle, name: &str, args: Vec<al_runtime::interp::Arg>) -> Result<Option<Val>, ApiErr> {
    let co = &s.cat.code[p.code as usize];
    match co.trigger(name) {
        Some(t) => {
            s.ret_true = matches!(name, "oninsertrecord" | "onmodifyrecord" | "ondeleterecord");
            let v = s.invoke(al_model::ProcRef { code: p.code, proc: t }, args, Some(inst.clone()), Some(h.clone()), None);
            s.ret_true = false;
            Ok(Some(v.map_err(al)?))
        }
        None => Ok(None),
    }
}

/// The page's first record (FindRecord "-"): its OnFindRecord trigger if it has one (temporary
/// source tables loaded by the trigger, journals filtered by template), else FindFirst. Then
/// OnAfterGetRecord, as BC's page data provider reads every record through the form.
fn page_find_first(s: &mut Session, p: &PageDef, inst: &std::rc::Rc<Instance>, h: &RecHandle) -> Result<bool, ApiErr> {
    let found = match page_trigger(s, p, inst, h, "onfindrecord", vec![al_runtime::interp::Arg::Val(Val::text("-"))])? {
        Some(v) => v.as_bool(),
        None => s.find_first(h, false).map_err(al)?,
    };
    if found {
        page_trigger(s, p, inst, h, "onaftergetrecord", vec![])?;
    }
    Ok(found)
}

/// The page's next record (NextRecord 1): OnNextRecord if the page has one, else Next; then OnAfterGetRecord.
fn page_next(s: &mut Session, p: &PageDef, inst: &std::rc::Rc<Instance>, h: &RecHandle) -> Result<bool, ApiErr> {
    let found = match page_trigger(s, p, inst, h, "onnextrecord", vec![al_runtime::interp::Arg::Val(Val::Int(1))])? {
        Some(v) => v.as_i64() != 0,
        None => s.next(h, 1).map_err(al)? != 0,
    };
    if found {
        page_trigger(s, p, inst, h, "onaftergetrecord", vec![])?;
    }
    Ok(found)
}

/// The text of a JSON property value.
fn json_text(v: &J) -> String {
    match v {
        J::String(x) => x.replace("_x0020_", " "),
        J::Bool(b) => b.to_string(),
        J::Number(n) => n.to_string(),
        J::Null => String::new(),
        other => other.to_string(),
    }
}

/// Assign JSON properties like BC's OData page provider (NavFormODataExtension.SetValuesAsync):
/// in the order of the page's controls, not the request's, entity key fields first. A table field
/// is validated (then the control's OnValidate); a page variable is assigned, then its control's
/// OnValidate runs. The order matters: the APIV2 document pages record each field in a field
/// buffer and their aggregators replay it in that order, so `discountPercent` or
/// `directUnitCost` sent before `lineObjectNumber` was validated before "No.", and lost.
fn apply_props(s: &mut Session, p: &PageDef, inst: &std::rc::Rc<Instance>, h: &RecHandle, body: &Map<String, J>) -> Result<(), ApiErr> {
    let ctls = api_controls(p);
    // unknown and read-only properties fail before anything is assigned
    for k in body.keys() {
        if k.starts_with('@') {
            continue;
        }
        let Some((_, _, c)) = ctls.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(k)) else {
            // own wording [src:X27]
            return Err(err(StatusCode::BAD_REQUEST, format!("This entity has no property '{k}'.")));
        };
        if c.props.get("editable").is_some_and(|e| e.eq_ignore_ascii_case("false")) {
            return Err(err(StatusCode::BAD_REQUEST, format!("The property '{k}' is read-only.")));
        }
    }
    let key = key_field(p, h).ok();
    let (keys, rest): (Vec<_>, Vec<_>) = ctls.iter().partition(|(_, fi, _)| fi.is_some() && *fi == key);
    for (name, fi, c) in keys.into_iter().chain(rest) {
        let Some((_, v)) = body.iter().find(|(k, _)| !k.starts_with('@') && k.eq_ignore_ascii_case(name)) else { continue };
        // the control's OnValidate sees xRec = the record before this change
        let xrec = al_runtime::record::copy_record(h);
        match fi {
            Some(fi) => {
                let fd = h.borrow().field(fi);
                let val = json_input(s, fd, v)?;
                let fid = fd.id as i32;
                s.rec_validate(h, fi, Some(val), fid).map_err(al)?;
            }
            None => {
                let Some((slot, ty)) = page_var(s, inst, c) else {
                    // own wording [src:X27]
                    return Err(err(StatusCode::BAD_REQUEST, format!("Property '{name}' of this entity cannot be assigned.")));
                };
                let cur = slot.borrow().clone();
                let txt = json_text(v);
                let val = match cur {
                    Val::Text(_) => Val::Text(txt),
                    _ => s.text_to_var(&txt, &cur, ty).ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("'{txt}' is not a valid value for {name}")))?,
                };
                s.write_lval(&al_runtime::interp::LVal::Slot(slot, ty), val).map_err(al)?;
            }
        }
        if let Some(pr) = c.triggers.get("onvalidate") {
            s.invoke(*pr, vec![], Some(inst.clone()), Some(h.clone()), Some(xrec)).map_err(al)?;
        }
    }
    Ok(())
}

/// The JSON value of an action's return value.
fn val_json(v: &Val) -> J {
    match v {
        Val::Bool(b) => json!(b),
        Val::Int(i) => json!(i),
        Val::BigInt(i) => json!(i),
        Val::Dec(d) => serde_json::from_str(&d.normalize().to_string()).unwrap_or(json!(0)),
        Val::Guid(g) => json!(guid_text(g)),
        Val::Date(_) | Val::DateTime(_) => json!(al_runtime::builtins::format_val(v, 9)),
        Val::Void => J::Null,
        other => json!(other.to_text()),
    }
}

/// A bound action's parameter from the request body (ODataActionParameters), by AL type.
fn json_param(v: &J, ty: &TypeRef) -> Result<Val, ApiErr> {
    let name = match ty {
        TypeRef::Simple { name, .. } => name.to_ascii_lowercase(),
        _ => String::new(),
    };
    let text = || match v {
        J::String(x) => x.clone(),
        J::Null => String::new(),
        other => other.to_string(),
    };
    let bad = || err(StatusCode::BAD_REQUEST, format!("'{}' is not a valid value for a parameter of type {name}", text()));
    Ok(match name.as_str() {
        "boolean" => Val::Bool(match v {
            J::Bool(b) => *b,
            _ => text().eq_ignore_ascii_case("true"),
        }),
        "integer" => Val::Int(text().parse().map_err(|_| bad())?),
        "biginteger" => Val::BigInt(text().parse().map_err(|_| bad())?),
        "decimal" => Val::Dec(text().parse().map_err(|_| bad())?),
        "guid" => Val::Guid(al_runtime::value::parse_guid(&text()).ok_or_else(bad)?),
        "datetime" => {
            let t = text();
            if t.is_empty() {
                Val::DateTime(bc_store::DATETIME_UNDEFINED)
            } else {
                Val::DateTime(chrono::DateTime::parse_from_rfc3339(&t).map_err(|_| bad())?.timestamp_millis())
            }
        }
        "date" => {
            let t = text();
            let d = chrono::NaiveDate::parse_from_str(&t, "%Y-%m-%d").map_err(|_| bad())?;
            Val::Date(al_runtime::value::date_from_ymd(chrono::Datelike::year(&d), chrono::Datelike::month(&d), chrono::Datelike::day(&d)).unwrap_or(0))
        }
        _ => Val::Text(text()),
    })
}

fn is_action_context(ty: &TypeRef) -> bool {
    matches!(ty, TypeRef::Simple { name, .. } if name.eq_ignore_ascii_case("webserviceactioncontext"))
}

/// A bound action, `POST .../<entitySet>(<id>)/Microsoft.NAV.<name>`: the API page's
/// `[ServiceEnabled]` procedure of that name, run on the page with its record, as BC's OData
/// layer does it (Microsoft.Dynamics.Nav.Service.OData.V4: GenericODataController.InvokeBoundAction,
/// PageDataProvider.InvokeAsync, CodeunitInvocationHelper). The key filter is set on the record,
/// OnOpenPage runs, the record is read (OnAfterGetRecord), and the procedure gets a new
/// WebServiceActionContext plus its other parameters from the JSON body by name. The response
/// (GetBoundActionContextActionResult):
/// - result code Deleted and no entity keys: 204;
/// - entity keys that name an entity of the API page the context names: the result code's
///   status (Get/Updated 200, Created 201, Deleted 204) with a Location header and no body;
/// - otherwise, and for procedures without a context: 204;
/// - a procedure with a return value: 200 `{"value": ...}`.
/// Returns (status, body, Location).
#[allow(clippy::too_many_arguments)]
fn bound_action(s: &mut Session, ctx: &Ctx, p: &PageDef, inst: &std::rc::Rc<Instance>, h: &RecHandle, id: &str, name: &str, body: Option<&J>, base: &str) -> Result<(StatusCode, J, Option<String>), ApiErr> {
    let cat = s.cat;
    let co = &cat.code[p.code as usize];
    let pi = co
        .find(name)
        .iter()
        .copied()
        .find(|&i| {
            let pr = &co.procs[i as usize];
            pr.kind == ProcKind::Procedure && pr.attributes.iter().any(|a| a.name.eq_ignore_ascii_case("serviceenabled"))
        })
        .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("The bound action 'Microsoft.NAV.{name}' does not exist on this entity.")))?;
    let pr = co.procs[pi as usize].clone();
    // the page with its record: key filter, OnOpenPage, read, OnAfterGetRecord
    key_filter(p, h, id)?;
    page_trigger(s, p, inst, h, "onopenpage", vec![])?;
    if !page_find_first(s, p, inst, h)? {
        return Err(not_found(p, true));
    }
    // parameters: the action context, the rest from the body by name
    let params = match body {
        Some(J::Object(m)) => Some(m),
        _ => None,
    };
    let mut ctx_slot = None;
    let mut args = Vec::with_capacity(pr.params.len());
    for prm in &pr.params {
        if is_action_context(&prm.ty) {
            let sl = al_runtime::value::slot(al_runtime::platobj::new_action_context());
            ctx_slot = Some(sl.clone());
            args.push(al_runtime::interp::Arg::Ref(al_runtime::interp::LVal::Slot(sl, None)));
            continue;
        }
        let v = params
            .and_then(|m| m.get(&prm.name).or_else(|| m.iter().find(|(k, _)| k.eq_ignore_ascii_case(&prm.name)).map(|(_, v)| v)))
            .ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("The bound action expects the parameter '{}'.", prm.name)))?;
        args.push(al_runtime::interp::Arg::Val(json_param(v, &prm.ty)?));
    }
    let ret = s.invoke(al_model::ProcRef { code: p.code, proc: pi }, args, Some(inst.clone()), Some(h.clone()), None).map_err(al)?;
    if pr.ret.is_some() {
        return Ok((StatusCode::OK, json!({ "value": val_json(&ret) }), None));
    }
    let Some(sl) = ctx_slot else { return Ok((StatusCode::NO_CONTENT, J::Null, None)) };
    let c = match &*sl.borrow() {
        Val::Plat(pc) => match &*pc.borrow() {
            al_runtime::platobj::Plat::ActionContext(c) => c.clone(),
            _ => Default::default(),
        },
        _ => Default::default(),
    };
    let status = al_runtime::platobj::action_result_status(c.result_code);
    if status == 204 && c.keys.is_empty() {
        return Ok((StatusCode::NO_CONTENT, J::Null, None));
    }
    // ObjectType::Page = 8: the API v2.0 page with that id, if it is one
    let target = (c.object_type == 8).then(|| ctx.rt.cat.pages.get(&c.object_id)).flatten().filter(|t| {
        t.page_type.eq_ignore_ascii_case("api") && t.props.get("apiversion").is_some_and(|v| v.contains("v2.0")) && !t.props.contains_key("apipublisher")
    });
    let nkeys = target.map(|t| t.props.get("odatakeyfields").map(|k| k.split(',').count()).unwrap_or(1)).unwrap_or(0);
    let (Some(t), Some(set)) = (target, target.and_then(|t| t.props.get("entitysetname"))) else {
        return Ok((StatusCode::NO_CONTENT, J::Null, None));
    };
    let _ = t;
    if c.keys.len() != nkeys || status == 0 {
        return Ok((StatusCode::NO_CONTENT, J::Null, None));
    }
    let key = c
        .keys
        .iter()
        .map(|(_, v)| match v {
            Val::Guid(g) => guid_text(g),
            Val::Int(_) | Val::BigInt(_) | Val::Dec(_) => v.to_text(),
            other => format!("'{}'", other.to_text().replace('\'', "''")),
        })
        .collect::<Vec<_>>()
        .join(",");
    let location = format!("{base}/companies({COMPANY_ID})/{}({key})", set.trim_matches('\''));
    Ok((StatusCode::from_u16(status).unwrap_or(StatusCode::NO_CONTENT), J::Null, Some(location)))
}

fn new_instance(s: &mut Session, p: &PageDef, h: &RecHandle) -> std::rc::Rc<Instance> {
    let inst = Instance::new(p.code);
    *inst.rec.borrow_mut() = Some(h.clone());
    let _ = s;
    inst
}

/// The record field behind the entity key: the page's ODataKeyFields (one field; the APIV2
/// pages use SystemId, or Id on the buffer-table pages such as salesOrders), else SystemId.
fn key_field(p: &PageDef, h: &RecHandle) -> Result<usize, ApiErr> {
    let name = p.props.get("odatakeyfields").map(|k| k.split(',').next().unwrap_or("").trim().trim_matches('"').to_string()).filter(|k| !k.is_empty()).unwrap_or_else(|| "SystemId".into());
    h.borrow().tdef.field(&name).ok_or_else(|| err(StatusCode::BAD_REQUEST, format!("the entity's key field {name} does not exist")))
}

/// Filter the record on the entity key `id` (a Guid).
fn key_filter(p: &PageDef, h: &RecHandle, id: &str) -> Result<(), ApiErr> {
    let g = al_runtime::value::parse_guid(id).ok_or_else(|| err(StatusCode::BAD_REQUEST, "invalid id"))?;
    let si = key_field(p, h)?;
    // with its filter text: the pages' OnFindRecord read it back (Rec.GetFilter(SystemId))
    h.borrow_mut().set_filter(si, Some(al_runtime::filter::FNode::Eq(bc_store::Value::Guid(g))), al_runtime::value::format_guid(&g));
    Ok(())
}

/// The entity `id`, read like BC's page data provider does: key filter, OnOpenPage, FindRecord
/// through the page (OnFindRecord: the line pages load their temporary source table there, so
/// PATCH purchaseInvoiceLines(<id>) found nothing before), OnAfterGetRecord.
fn by_id(s: &mut Session, p: &PageDef, inst: &std::rc::Rc<Instance>, h: &RecHandle, id: &str, write: bool) -> Result<(), ApiErr> {
    key_filter(p, h, id)?;
    page_trigger(s, p, inst, h, "onopenpage", vec![])?;
    if !page_find_first(s, p, inst, h)? {
        return Err(not_found(p, write));
    }
    Ok(())
}

/// `$filter=number eq '10000' and displayName eq 'x'` (eq / ne / gt / lt / ge / le joined by `and`).
fn apply_filter(s: &mut Session, p: &PageDef, h: &RecHandle, f: &str) -> Result<(), ApiErr> {
    let fields = api_fields(p);
    for clause in f.split(" and ") {
        let parts: Vec<&str> = clause.trim().splitn(3, ' ').collect();
        if parts.len() != 3 {
            return Err(err(StatusCode::BAD_REQUEST, format!("unsupported $filter clause '{clause}'")));
        }
        let Some((_, fi, _)) = fields.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(parts[0])) else {
            return Err(err(StatusCode::BAD_REQUEST, format!("unknown property '{}' in $filter", parts[0])));
        };
        let lit = parts[2].trim().trim_matches('\'').replace("''", "'");
        let op = match parts[1] {
            "eq" => "",
            "ne" => "<>",
            "gt" => ">",
            "lt" => "<",
            "ge" => ">=",
            "le" => "<=",
            o => return Err(err(StatusCode::BAD_REQUEST, format!("unsupported operator '{o}'"))),
        };
        let quoted = format!("{op}'{}'", lit.replace('\'', "''"));
        let fd = h.borrow().field(*fi);
        let node = s.parse_field_filter(fd, &quoted).map_err(al)?;
        h.borrow_mut().set_filter(*fi, Some(node), quoted);
    }
    Ok(())
}

pub async fn handler(
    State(ctx): State<&'static Ctx>,
    method: axum::http::Method,
    Path(rest): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    raw: axum::body::Bytes,
) -> Result<Response, ApiErr> {
    let body: Option<J> = if raw.iter().all(|b| b.is_ascii_whitespace()) {
        None
    } else {
        Some(serde_json::from_slice(&raw).map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON body: {e}")))?)
    };
    let segs: Vec<&str> = rest.trim_matches('/').split('/').collect();
    // companies
    if segs == ["companies"] {
        return Ok((StatusCode::OK, Json(json!({"value": [{"id": COMPANY_ID, "name": ctx.company, "displayName": ctx.company}]}))).into_response());
    }
    let company = segs.first().and_then(|s| s.strip_prefix("companies(")).and_then(|s| s.strip_suffix(')')).ok_or_else(|| err(StatusCode::NOT_FOUND, "expected companies({id})/..."))?;
    if !company.eq_ignore_ascii_case(COMPANY_ID) && !company.eq_ignore_ascii_case(&ctx.company) {
        return Err(err(StatusCode::NOT_FOUND, "unknown company"));
    }
    let ent = segs.get(1).ok_or_else(|| err(StatusCode::NOT_FOUND, "missing entity set"))?;
    // a bound action: .../<entitySet>(<id>)/Microsoft.NAV.<name>
    let action: Option<String> = match segs.get(2) {
        None => None,
        Some(a) if segs.len() == 3 && a.len() > "Microsoft.NAV.".len() && a[.."Microsoft.NAV.".len()].eq_ignore_ascii_case("Microsoft.NAV.") => Some(a["Microsoft.NAV.".len()..].to_string()),
        Some(a) => return Err(err(StatusCode::NOT_FOUND, format!("unsupported path segment '{a}'"))),
    };
    // the request's own base URL, for the Location header of bound actions
    let host = headers.get("x-forwarded-host").or_else(|| headers.get(axum::http::header::HOST)).and_then(|v| v.to_str().ok()).unwrap_or("localhost").to_string();
    let proto = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).unwrap_or("http").to_string();
    let base = format!("{proto}://{host}/api/v2.0");
    let (set, id) = match ent.find('(') {
        Some(i) => (&ent[..i], Some(ent[i + 1..].trim_end_matches(')').to_string())),
        None => (*ent, None),
    };
    let p = api_page(ctx, set).ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no API v2.0 page for '{set}'")))?;
    let tid = p.source_table.ok_or_else(|| err(StatusCode::NOT_FOUND, "API page has no source table"))?;
    crate::auth::spawn_blocking(move || {
        let mut s = Session::try_new(ctx.rt, &ctx.company).map_err(|e| err(StatusCode::SERVICE_UNAVAILABLE, e.to_string()))?;
        // an API v2.0 request is an API client session in BC (ClientType::Api): the Base App branches on it,
        // e.g. Item's Base Unit of Measure validation inserts the item first for API and OData clients
        s.client_type = "api";
        // the authenticated user (BC_AUTH, crate::auth), else the default user as before
        crate::auth::bind(&mut s);
        // permissions, only with BC_PERMISSIONS=enforce (al_runtime::permissions, docs/permissions.md):
        // the request's user and their permission on the page's source table, as BC's page data
        // provider checks it; bound actions run AL code, which record_method checks
        if al_runtime::permissions::enforce() {
            // the Basic user name unchecked only while BC_AUTH is off; with it, bind() set the verified user
            if let Some(u) = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(al_runtime::permissions::basic_auth_user).filter(|_| !crate::auth::enabled()) {
                s.user_id = u;
                al_runtime::userauth::resolve_session_user(&mut s);
            }
            s.login_permissions();
            use al_runtime::permissions::{DELETE, INSERT, MODIFY, READ};
            let op = match method.as_str() {
                _ if action.is_some() => 0,
                "GET" => READ,
                "POST" => INSERT,
                "PATCH" => MODIFY,
                "DELETE" => DELETE,
                _ => 0,
            };
            if op != 0 {
                let code = ctx.rt.cat.code_by_key.get(&(al_syntax::ObjectKind::Page, p.id)).copied().unwrap_or(0);
                if let Err(al_runtime::Flow::Err(e)) = s.check_table_op(code, tid, op) {
                    return Err(err(StatusCode::FORBIDDEN, e.msg.clone()));
                }
            }
        }
        // SourceTableTemporary pages (the document line pages) work on a temporary record
        let temp = p.props.get("sourcetabletemporary").is_some_and(|v| v.eq_ignore_ascii_case("true"));
        let h = s.new_record(tid, temp);
        let inst = new_instance(&mut s, p, &h);
        let mut location: Option<String> = None;
        let res: Result<(StatusCode, J), ApiErr> = (|| match (method.as_str(), &id) {
            ("POST", Some(id)) if action.is_some() => {
                let (code, out, loc) = bound_action(&mut s, ctx, p, &inst, &h, id, action.as_deref().unwrap_or_default(), body.as_ref(), &base)?;
                location = loc;
                Ok((code, out))
            }
            _ if action.is_some() => Err(err(StatusCode::METHOD_NOT_ALLOWED, "bound actions are invoked with POST on an entity")),
            ("GET", None) => {
                if let Some(f) = q.get("$filter") {
                    apply_filter(&mut s, p, &h, f)?;
                }
                let top = q.get("$top").and_then(|t| t.parse().ok()).unwrap_or(usize::MAX);
                let skip = q.get("$skip").and_then(|t| t.parse().ok()).unwrap_or(0usize);
                // through the page: OnOpenPage, OnFindRecord / OnNextRecord, OnAfterGetRecord
                page_trigger(&mut s, p, &inst, &h, "onopenpage", vec![])?;
                let mut out = vec![];
                let mut ok = page_find_first(&mut s, p, &inst, &h)?;
                let mut i = 0;
                while ok && out.len() < top {
                    if i >= skip {
                        out.push(entity_json(&mut s, p, &inst, &h)?);
                    }
                    i += 1;
                    ok = page_next(&mut s, p, &inst, &h)?;
                }
                Ok((StatusCode::OK, json!({ "value": out })))
            }
            ("GET", Some(id)) => {
                by_id(&mut s, p, &inst, &h, id, false)?;
                Ok((StatusCode::OK, entity_json(&mut s, p, &inst, &h)?))
            }
            ("POST", None) => {
                let Some(J::Object(m)) = &body else { return Err(err(StatusCode::BAD_REQUEST, "expected a JSON object")) };
                page_trigger(&mut s, p, &inst, &h, "onnewrecord", vec![al_runtime::interp::Arg::Val(Val::Bool(false))])?;
                apply_props(&mut s, p, &inst, &h, m)?;
                let r = page_trigger(&mut s, p, &inst, &h, "oninsertrecord", vec![al_runtime::interp::Arg::Val(Val::Bool(false))])?;
                if r.is_none_or(|v| v.as_bool()) {
                    if let Err(m) = s.rec_insert(&h, true).map_err(al)? {
                        return Err(err(StatusCode::CONFLICT, m));
                    }
                }
                if let Some(row) = s.rec_get_pk(&h) {
                    h.borrow_mut().row = row.into();
                }
                Ok((StatusCode::CREATED, entity_json(&mut s, p, &inst, &h)?))
            }
            ("PATCH", Some(id)) => {
                // BC refuses a PATCH without If-Match before it reads anything
                let im = if_match(&headers)?;
                if im == IfMatch::Absent {
                    return Err(invalid_token());
                }
                let Some(J::Object(m)) = &body else { return Err(err(StatusCode::BAD_REQUEST, "expected a JSON object")) };
                by_id(&mut s, p, &inst, &h, id, true)?;
                check_version(&h, &im)?;
                apply_props(&mut s, p, &inst, &h, m)?;
                let r = page_trigger(&mut s, p, &inst, &h, "onmodifyrecord", vec![])?;
                if r.is_none_or(|v| v.as_bool()) {
                    if let Err(m) = s.rec_modify(&h, true).map_err(al)? {
                        return Err(err(StatusCode::BAD_REQUEST, m));
                    }
                }
                Ok((StatusCode::OK, entity_json(&mut s, p, &inst, &h)?))
            }
            ("DELETE", Some(id)) => {
                // If-Match is optional on DELETE; one that is there must be valid and current
                let im = if_match(&headers)?;
                by_id(&mut s, p, &inst, &h, id, true)?;
                check_version(&h, &im)?;
                let r = page_trigger(&mut s, p, &inst, &h, "ondeleterecord", vec![])?;
                if r.is_none_or(|v| v.as_bool()) {
                    if let Err(m) = s.rec_delete(&h, true).map_err(al)? {
                        return Err(err(StatusCode::BAD_REQUEST, m));
                    }
                }
                Ok((StatusCode::NO_CONTENT, J::Null))
            }
            _ => Err(err(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")),
        })();
        match res {
            Ok((code, j)) => {
                s.commit().map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                let mut r = if code == StatusCode::NO_CONTENT || (j.is_null() && location.is_some()) { code.into_response() } else { (code, Json(j)).into_response() };
                if let Some(l) = location.and_then(|l| axum::http::HeaderValue::from_str(&l).ok()) {
                    r.headers_mut().insert(axum::http::header::LOCATION, l);
                }
                Ok(r)
            }
            Err(e) => {
                s.rollback();
                Err(e)
            }
        }
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

pub fn _unused(_: &dyn Fn(&FieldDef, &Val) -> Result<bc_store::Value, String>) {
    let _ = to_store;
}
