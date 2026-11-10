//! HTTP API used by the web client. Every data operation runs real AL
//! (table triggers, field validation, events) inside a short-lived session.

use crate::app::flow_err;
use al_model::{CondKind, Control, FieldClass, FieldDef, FieldType, PageDef, TableDef};
use al_runtime::record::{text_to_store, to_val};
use al_runtime::{RecHandle, Runtime, Session, Val};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Map, Value as J};
use std::collections::HashMap;
use std::time::Instant;

pub struct Ctx {
    pub rt: &'static Runtime,
    pub company: String,
    pub boot_ms: u128,
}

type S = State<&'static Ctx>;

pub fn router(ctx: &'static Ctx) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(|| async { ([("content-type", "text/javascript")], include_str!("../../../web/app.js")) }))
        .route("/app.css", get(|| async { ([("content-type", "text/css")], include_str!("../../../web/app.css")) }))
        .route("/api/stats", get(stats))
        .route("/api/home", get(home))
        .route("/api/pages", get(search_pages))
        .route("/api/page/{id}", get(page_meta))
        .route("/api/page/{id}/rows", get(page_rows))
        .route("/api/page/{id}/record", get(get_record).post(new_record).patch(patch_record).delete(delete_record))
        .route("/api/page/{id}/action/{name}", axum::routing::post(run_action))
        .route("/api/v2.0/{*rest}", axum::routing::any(crate::odata::handler))
        // BC_AUTH: every route needs credentials (crate::auth; a pass-through while off)
        .layer(axum::middleware::from_fn_with_state(ctx, crate::auth::layer))
        .with_state(ctx)
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../../../web/index.html"))
}

/// An error answer: `{"error": message}`, or with a BC OData error code
/// `{"error": {"code": code, "message": message}}` as BC's OData layer writes it
/// (NavODataErrorContainer; codes from ExceptionExtensions, e.g. Request_EntityChanged).
pub struct ApiErr(pub StatusCode, pub String, pub Option<&'static str>);
impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        match self.2 {
            None => (self.0, Json(json!({ "error": self.1 }))).into_response(),
            Some(code) => (self.0, Json(json!({ "error": { "code": code, "message": self.1 } }))).into_response(),
        }
    }
}
fn bad(m: impl Into<String>) -> ApiErr {
    ApiErr(StatusCode::BAD_REQUEST, m.into(), None)
}
fn al(e: al_runtime::Flow) -> ApiErr {
    match e {
        // the record changed after this request read it ("Another user has modified the record"):
        // 409, as BC's OData layer answers a write on a changed record (I15 W3)
        al_runtime::Flow::Err(e) if e.code == "DB:RecordChanged" => ApiErr(StatusCode::CONFLICT, e.msg, None),
        al_runtime::Flow::Err(e) => ApiErr(StatusCode::UNPROCESSABLE_ENTITY, e.msg, None),
        _ => ApiErr(StatusCode::INTERNAL_SERVER_ERROR, "unexpected control flow".into(), None),
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, ApiErr> + Send + 'static) -> Result<T, ApiErr> {
    crate::auth::spawn_blocking(f).await.map_err(|e| ApiErr(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None))?
}

// ------------------------------------------------------------------ metadata

fn page(ctx: &Ctx, id: i64) -> Result<&'static PageDef, ApiErr> {
    ctx.rt.cat.pages.get(&id).ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, format!("page {id} not found"), None))
}

fn literal_false(c: &Control, prop: &str) -> bool {
    c.props.get(prop).is_some_and(|v| v.eq_ignore_ascii_case("false"))
}

fn field_json(t: &TableDef, fd: &FieldDef) -> J {
    let ty = match &fd.ty {
        FieldType::Code(n) => format!("Code[{n}]"),
        FieldType::Text(n) => format!("Text[{n}]"),
        FieldType::Enum(e) => format!("Enum {e}"),
        other => format!("{other:?}"),
    };
    let options: Vec<String> = match &fd.ty {
        FieldType::Option => fd.option_captions.iter().map(|s| s.trim().to_string()).collect(),
        FieldType::Enum(e) => {
            let cat = al_runtime::record::CATALOG.get().unwrap();
            cat.enum_by_name(e).map(|en| en.values.iter().map(|v| v.caption.clone()).collect()).unwrap_or_default()
        }
        _ => vec![],
    };
    let pk = t.pk().fields.iter().any(|&i| t.fields[i].id == fd.id);
    json!({
        "name": fd.name, "caption": fd.caption, "type": ty, "kind": format!("{:?}", fd.class),
        "editable": fd.editable && fd.class == FieldClass::Normal && !fd.is_system(), "pk": pk,
        "options": options, "tooltip": fd.tooltip, "relation": fd.relation.first().map(|r| r.table.clone()),
    })
}

fn control_json(ctx: &Ctx, t: Option<&TableDef>, c: &Control) -> Option<J> {
    if literal_false(c, "visible") {
        return None;
    }
    match c.kind.as_str() {
        "field" => {
            let (fd, caption) = match (t, c.field) {
                (Some(t), Some(i)) => (Some(&t.fields[i]), c.caption.clone().unwrap_or_else(|| t.fields[i].caption.clone())),
                _ => (None, c.caption.clone().unwrap_or_else(|| c.name.clone())),
            };
            let mut o = Map::new();
            o.insert("kind".into(), json!("field"));
            o.insert("name".into(), json!(c.name));
            o.insert("caption".into(), json!(caption));
            o.insert("importance".into(), json!(c.props.get("importance")));
            o.insert("tooltip".into(), json!(c.props.get("tooltip").or(fd.and_then(|f| f.tooltip.as_ref()))));
            match (t, fd) {
                (Some(t), Some(fd)) => {
                    o.insert("field".into(), field_json(t, fd));
                    o.insert("editable".into(), json!(!literal_false(c, "editable") && fd.editable && fd.class == FieldClass::Normal));
                }
                _ => {
                    o.insert("source".into(), json!(c.source));
                    o.insert("editable".into(), json!(false));
                }
            }
            Some(J::Object(o))
        }
        "part" => {
            let p = c.part_page.as_ref().and_then(|n| ctx.rt.cat.page_by_name(n))?;
            Some(json!({
                "kind": "part", "name": c.name, "caption": c.caption.clone().unwrap_or_else(|| p.caption.clone()), "page": p.id,
                "link": c.sub_page_link.iter().map(|l| json!({"field": l.field, "kind": format!("{:?}", l.kind), "value": l.value})).collect::<Vec<_>>(),
            }))
        }
        "systempart" | "usercontrol" | "chartpart" => None,
        _ => {
            let kids: Vec<J> = c.children.iter().filter_map(|k| control_json(ctx, t, k)).collect();
            if kids.is_empty() {
                return None;
            }
            Some(json!({ "kind": c.kind, "name": c.name, "caption": c.caption.clone().unwrap_or_else(|| c.name.clone()), "children": kids }))
        }
    }
}

fn actions_json(cs: &[Control], out: &mut Vec<J>) {
    for c in cs {
        if literal_false(c, "visible") {
            continue;
        }
        match c.kind.as_str() {
            "action" => out.push(json!({"name": c.name, "caption": strip_amp(&c.caption.clone().unwrap_or_else(|| c.name.clone())), "runnable": c.triggers.contains_key("onaction"),
                "image": c.props.get("image"), "runobject": c.props.get("runobject"), "tooltip": c.props.get("tooltip")})),
            _ => actions_json(&c.children, out),
        }
    }
}

/// BC captions mark access keys with '&' ("Co&mments"); "&&" is a literal ampersand.
fn strip_amp(s: &str) -> String {
    s.replace("&&", "\u{0}").replace('&', "").replace('\u{0}', "&")
}

fn meta_json(ctx: &Ctx, p: &PageDef) -> J {
    let t = p.source_table.and_then(|id| ctx.rt.cat.tables.get(&id));
    let layout: Vec<J> = p.layout.iter().filter_map(|c| if c.name.eq_ignore_ascii_case("factboxes") { None } else { control_json(ctx, t, c) }).collect();
    let mut actions = vec![];
    actions_json(&p.actions, &mut actions);
    let card = p.card_page.as_ref().and_then(|n| ctx.rt.cat.page_by_name(n)).map(|cp| cp.id);
    json!({
        "id": p.id, "name": p.name, "caption": p.caption, "type": p.page_type,
        "table": t.map(|t| json!({"id": t.id, "name": t.name, "caption": t.caption,
            "pk": t.pk().fields.iter().map(|&i| t.fields[i].name.clone()).collect::<Vec<_>>()})),
        "cardPage": card,
        "editable": !p.props.get("editable").is_some_and(|v| v.eq_ignore_ascii_case("false")),
        "insertAllowed": !p.props.get("insertallowed").is_some_and(|v| v.eq_ignore_ascii_case("false")),
        "deleteAllowed": !p.props.get("deleteallowed").is_some_and(|v| v.eq_ignore_ascii_case("false")),
        "layout": layout, "actions": actions,
        "autoSplitKey": p.props.get("autosplitkey").is_some_and(|v| v.eq_ignore_ascii_case("true")),
        "about": p.props.get("abouttext"),
    })
}

async fn page_meta(State(ctx): S, Path(id): Path<i64>) -> Result<Json<J>, ApiErr> {
    let p = page(ctx, id)?;
    Ok(Json(meta_json(ctx, p)))
}

#[derive(serde::Deserialize)]
pub struct SearchQ {
    q: Option<String>,
}

async fn search_pages(State(ctx): S, Query(q): Query<SearchQ>) -> Json<J> {
    let needle = q.q.unwrap_or_default().to_lowercase();
    let mut hits: Vec<&PageDef> = ctx
        .rt
        .cat
        .pages
        .values()
        .filter(|p| matches!(p.page_type.to_ascii_lowercase().as_str(), "list" | "card" | "document" | "worksheet" | "listplus"))
        .filter(|p| p.source_table.is_some())
        .filter(|p| needle.is_empty() || p.caption.to_lowercase().contains(&needle) || p.name.to_lowercase().contains(&needle))
        .collect();
    hits.sort_by_key(|p| (!p.caption.to_lowercase().starts_with(&needle), p.page_type.to_ascii_lowercase() != "list", p.caption.len(), p.id));
    Json(json!(hits.iter().take(40).map(|p| json!({"id": p.id, "caption": p.caption, "type": p.page_type})).collect::<Vec<_>>()))
}

async fn home(State(ctx): S) -> Result<Json<J>, ApiErr> {
    blocking(move || {
        // the web client opened: the company-open triggers of an interactive session (crate::companyopen)
        crate::companyopen::web_client_open(ctx);
        Ok(Json(home_json(ctx)))
    })
    .await
}

fn home_json(ctx: &Ctx) -> J {
    let groups: &[(&str, &[i64])] = &[
        ("Sales", &[22, 9305, 9301, 9300, 143]),
        ("Purchasing", &[27, 9307, 9308, 146]),
        ("Inventory", &[31, 15, 5700]),
        ("Finance", &[16, 20, 25, 29, 251, 312]),
        ("Setup", &[459, 460, 456, 118, 1, 571]),
    ];
    let cat = ctx.rt.cat;
    let mut tiles = vec![];
    for (g, ids) in groups {
        let pages: Vec<J> = ids
            .iter()
            .filter_map(|id| cat.pages.get(id))
            .map(|p| {
                let n = p.source_table.map(|t| ctx.rt.db.table_count(&ctx.company, &ctx.rt.schemas[&t])).unwrap_or(0);
                json!({"id": p.id, "caption": p.caption, "count": n, "type": p.page_type})
            })
            .collect();
        tiles.push(json!({"group": g, "pages": pages}));
    }
    json!({ "company": ctx.company, "groups": tiles })
}

async fn stats(State(ctx): S) -> Result<Json<J>, ApiErr> {
    let st = blocking(move || Ok(ctx.rt.db.stats())).await?;
    let c = &ctx.rt.cat;
    Ok(Json(json!({
        "files": c.stats.files, "lines": c.stats.lines, "objects": c.stats.objects, "parseMs": c.stats.parse_ms, "buildMs": c.stats.build_ms,
        "tables": c.tables.len(), "pages": c.pages.len(), "codeunits": c.code.iter().filter(|x| x.kind == al_syntax::ObjectKind::Codeunit).count(),
        "enums": c.enums.len(), "subscribers": c.subscribers.values().map(|v| v.len()).sum::<usize>(),
        "storeTables": st.tables, "storeRows": st.rows, "bootMs": ctx.boot_ms, "database": ctx.rt.db.describe(),
    })))
}

// ------------------------------------------------------------------ data

fn display(s: &Session, fd: &FieldDef, v: &Val) -> J {
    match v {
        Val::Bool(b) => json!(b),
        Val::Dec(d) => json!(al_runtime::value::format_decimal(d.round_dp(2), true)),
        Val::Int(i) if fd.blank_zero && *i == 0 => json!(""),
        Val::Opt { .. } => json!(s.format_field_val(fd, v)),
        other => json!(other.to_text()),
    }
}

fn row_json(s: &mut Session, h: &RecHandle, fields: &[usize]) -> J {
    let mut o = Map::new();
    let r = h.borrow();
    for &i in fields {
        let fd = r.field(i);
        o.insert(fd.name.clone(), display(s, fd, &r.get_val(i)));
    }
    let pk: Vec<J> = r.tdef.pk().fields.iter().map(|&i| json!(r.get_val(i).to_text())).collect();
    o.insert("$key".into(), J::Array(pk));
    J::Object(o)
}

fn collect_fields(t: &TableDef, cs: &[Control], out: &mut Vec<usize>) {
    for c in cs {
        if let Some(i) = c.field {
            if !out.contains(&i) {
                out.push(i);
            }
        }
        if c.kind != "part" {
            collect_fields(t, &c.children, out);
        }
    }
}

fn page_fields(t: &TableDef, p: &PageDef) -> Vec<usize> {
    let mut v = vec![];
    for &i in &t.pk().fields {
        v.push(i);
    }
    collect_fields(t, &p.layout, &mut v);
    v
}

fn set_text_filter(s: &mut Session, h: &RecHandle, fd_i: usize, text: &str) -> Result<(), ApiErr> {
    let fd = h.borrow().field(fd_i);
    let node = s.parse_field_filter(fd, text).map_err(al)?;
    h.borrow_mut().set_filter(fd_i, Some(node), text.to_string());
    Ok(())
}

/// Apply SourceTableView and an optional parent link (SubPageLink) to a record.
fn apply_view(s: &mut Session, p: &PageDef, h: &RecHandle, link: Option<&J>) -> Result<(), ApiErr> {
    let t = h.borrow().tdef;
    if !p.view.sorting.is_empty() {
        let idx: Vec<usize> = p.view.sorting.iter().filter_map(|f| t.field(f)).collect();
        let k = t.keys.iter().position(|k| k.fields.len() >= idx.len() && k.fields[..idx.len()] == idx[..]);
        if let Some(k) = k {
            let kf = t.keys[k].fields.clone();
            let mut r = h.borrow_mut();
            r.key = r.schema.keys.iter().position(|sk| *sk == kf).unwrap_or(0);
        }
    }
    h.borrow_mut().ascending = !p.view.descending;
    for c in &p.view.conds {
        if let Some(fi) = t.field(&c.field) {
            let txt = match c.kind {
                CondKind::Const => {
                    if c.value.contains(['|', '&', '.', '<', '>', '*']) {
                        format!("'{}'", c.value)
                    } else {
                        c.value.clone()
                    }
                }
                _ => c.value.clone(),
            };
            let _ = set_text_filter(s, h, fi, &txt);
        }
    }
    if let Some(J::Object(m)) = link {
        for (k, v) in m {
            if let Some(fi) = t.field(k) {
                let txt = v.as_str().map(|x| x.to_string()).unwrap_or_else(|| v.to_string());
                let fd = &t.fields[fi];
                let evf = |n: &str| s.enum_ordinal_for_field(fd, n);
                let sv = text_to_store(fd, &txt, &evf, s.work_date).ok_or_else(|| bad(format!("bad link value {txt}")))?;
                let text = txt.clone();
                h.borrow_mut().set_filter(fi, Some(al_runtime::filter::FNode::Eq(sv)), text);
            }
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
pub struct RowsQ {
    search: Option<String>,
    top: Option<usize>,
    link: Option<String>,
    filter: Option<String>,
}

async fn page_rows(State(ctx): S, Path(id): Path<i64>, Query(q): Query<RowsQ>) -> Result<Json<J>, ApiErr> {
    let p = page(ctx, id)?;
    let tid = p.source_table.ok_or_else(|| bad("page has no source table"))?;
    blocking(move || {
        let t0 = Instant::now();
        let mut s = session(ctx)?;
        let t = &ctx.rt.cat.tables[&tid];
        let h = s.new_record(tid, false);
        let link: Option<J> = q.link.as_deref().and_then(|l| serde_json::from_str(l).ok());
        apply_view(&mut s, p, &h, link.as_ref())?;
        if let Some(J::Object(m)) = q.filter.as_deref().and_then(|f| serde_json::from_str::<J>(f).ok()) {
            for (k, v) in m {
                if let (Some(fi), Some(txt)) = (t.field(&k), v.as_str()) {
                    if !txt.is_empty() {
                        set_text_filter(&mut s, &h, fi, txt)?;
                    }
                }
            }
        }
        let fields = page_fields(t, p);
        let flow: Vec<usize> = fields.iter().copied().filter(|&i| t.fields[i].class == FieldClass::FlowField).collect();
        let needle = q.search.unwrap_or_default().to_lowercase();
        let top = q.top.unwrap_or(200).min(2000);
        let mut rows = vec![];
        let mut ok = s.find_first(&h, false).map_err(al)?;
        let mut scanned = 0usize;
        while ok && rows.len() < top {
            scanned += 1;
            for &fi in &flow {
                s.calc_field(&h, fi).map_err(al)?;
            }
            let rj = row_json(&mut s, &h, &fields);
            let keep = needle.is_empty()
                || rj.as_object().unwrap().values().any(|v| v.as_str().is_some_and(|x| x.to_lowercase().contains(&needle)));
            if keep {
                rows.push(rj);
            }
            ok = s.next(&h, 1).map_err(al)? != 0;
        }
        Ok(Json(json!({ "rows": rows, "scanned": scanned, "more": ok, "ms": t0.elapsed().as_secs_f64() * 1000.0 })))
    })
    .await
}

#[derive(serde::Deserialize)]
pub struct KeyQ {
    key: Option<String>,
}

fn load_by_key(s: &mut Session, h: &RecHandle, key: &str) -> Result<(), ApiErr> {
    let parts: Vec<String> = serde_json::from_str(key).map_err(|_| bad("key must be a JSON array"))?;
    let t = h.borrow().tdef;
    for (j, &fi) in t.pk().fields.iter().enumerate() {
        let txt = parts.get(j).cloned().unwrap_or_default();
        let fd = &t.fields[fi];
        let evf = |n: &str| s.enum_ordinal_for_field(fd, n);
        let sv = text_to_store(fd, &txt, &evf, s.work_date).ok_or_else(|| bad(format!("bad key value {txt}")))?;
        h.borrow_mut().row[fi] = sv;
    }
    match s.rec_get_pk(h) {
        Some(row) => {
            let row: al_runtime::record::CowRow = row.into();
            h.borrow_mut().xrow = Some(row.clone());
            h.borrow_mut().row = row;
            Ok(())
        }
        None => Err(ApiErr(StatusCode::NOT_FOUND, "record not found".into(), None)),
    }
}

fn card_json(s: &mut Session, p: &PageDef, h: &RecHandle) -> Result<J, ApiErr> {
    let t = h.borrow().tdef;
    let fields = page_fields(t, p);
    for &fi in &fields {
        if t.fields[fi].class == FieldClass::FlowField {
            s.calc_field(h, fi).map_err(al)?;
        }
    }
    Ok(row_json(s, h, &fields))
}

async fn get_record(State(ctx): S, Path(id): Path<i64>, Query(q): Query<KeyQ>) -> Result<Json<J>, ApiErr> {
    let p = page(ctx, id)?;
    let tid = p.source_table.ok_or_else(|| bad("page has no source table"))?;
    blocking(move || {
        let mut s = session(ctx)?;
        let h = s.new_record(tid, false);
        match q.key.as_deref() {
            Some(k) => load_by_key(&mut s, &h, k)?,
            None => {
                if !s.find_first(&h, false).map_err(al)? {
                    return Err(ApiErr(StatusCode::NOT_FOUND, "no records".into(), None));
                }
            }
        }
        Ok(Json(card_json(&mut s, p, &h)?))
    })
    .await
}

fn parse_input(s: &Session, fd: &FieldDef, v: &J) -> Result<Val, ApiErr> {
    let txt = match v {
        J::String(x) => x.clone(),
        J::Bool(b) => b.to_string(),
        J::Number(n) => n.to_string(),
        J::Null => String::new(),
        other => other.to_string(),
    };
    let evf = |n: &str| s.enum_ordinal_for_field(fd, n);
    let sv = text_to_store(fd, &txt, &evf, s.work_date).ok_or_else(|| bad(format!("'{txt}' is not a valid value for {}", fd.caption)))?;
    Ok(to_val(fd, &sv))
}

fn finish(s: &mut Session) -> Result<(), ApiErr> {
    s.commit().map_err(|e| ApiErr(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None))
}

fn session<'a>(ctx: &'a Ctx) -> Result<Session<'a>, ApiErr> {
    let mut s = Session::try_new(ctx.rt, &ctx.company).map_err(|e| ApiErr(StatusCode::SERVICE_UNAVAILABLE, e.to_string(), None))?;
    crate::auth::bind(&mut s);
    Ok(s)
}

fn with_rollback<T>(s: &mut Session, r: Result<T, ApiErr>) -> Result<T, ApiErr> {
    if r.is_err() {
        s.rollback();
    }
    r
}

/// New records on a page get the page's fixed filters (SourceTableView const / link values),
/// and AutoSplitKey pages get the next line number.
fn apply_defaults(s: &mut Session, p: &PageDef, h: &RecHandle, body: &HashMap<String, J>) -> Result<(), ApiErr> {
    let t = h.borrow().tdef;
    for c in &p.view.conds {
        // const(X), or filter(X) with a single plain value, becomes the new record's default
        let single = c.kind == CondKind::Const || (c.kind == CondKind::Filter && !c.value.contains(['|', '&', '.', '<', '>', '*', '=']));
        if single {
            if let Some(fi) = t.field(&c.field) {
                let fd = &t.fields[fi];
                let evf = |n: &str| s.enum_ordinal_for_field(fd, n);
                if let Some(v) = text_to_store(fd, &c.value, &evf, s.work_date) {
                    h.borrow_mut().row[fi] = v;
                }
            }
        }
    }
    for &fi in &t.pk().fields {
        if let Some(v) = body.get(&t.fields[fi].name) {
            let val = parse_input(s, &t.fields[fi], v)?;
            let sv = al_runtime::record::to_store(&t.fields[fi], &val).map_err(bad)?;
            h.borrow_mut().row[fi] = sv;
        }
    }
    if p.props.get("autosplitkey").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
        let last = *t.pk().fields.last().unwrap();
        if matches!(t.fields[last].ty, FieldType::Integer) && !body.contains_key(&t.fields[last].name) {
            let probe = s.new_record(t.id, false);
            {
                let src = h.borrow();
                let mut pr = probe.borrow_mut();
                for &fi in &t.pk().fields[..t.pk().fields.len() - 1] {
                    let v = src.row[fi].clone();
                    pr.set_filter(fi, Some(al_runtime::filter::FNode::Eq(v)), String::new());
                }
            }
            let mut max = 0i64;
            if s.find_first(&probe, true).map_err(al)? {
                max = probe.borrow().get_val(last).as_i64();
            }
            h.borrow_mut().row[last] = bc_store::Value::Int((max + 10000) as i32);
        }
    }
    Ok(())
}

/// Run page-control OnValidate triggers for a field (after the table's Validate).
fn page_field_trigger(s: &mut Session, p: &PageDef, h: &RecHandle, fi: usize) -> Result<(), ApiErr> {
    fn find<'a>(cs: &'a [Control], fi: usize) -> Option<&'a Control> {
        for c in cs {
            if c.field == Some(fi) && c.triggers.contains_key("onvalidate") {
                return Some(c);
            }
            if let Some(x) = find(&c.children, fi) {
                return Some(x);
            }
        }
        None
    }
    if let Some(c) = find(&p.layout, fi) {
        let pr = c.triggers["onvalidate"];
        let inst = al_runtime::Instance::new(p.code);
        *inst.rec.borrow_mut() = Some(h.clone());
        s.invoke(pr, vec![], Some(inst), Some(h.clone()), None).map_err(al)?;
    }
    Ok(())
}

#[derive(serde::Deserialize)]
pub struct ActionQ {
    key: Option<String>,
}

async fn run_action(State(ctx): S, Path((id, name)): Path<(i64, String)>, Query(q): Query<ActionQ>) -> Result<Json<J>, ApiErr> {
    let p = page(ctx, id)?;
    fn find<'a>(cs: &'a [Control], n: &str) -> Option<&'a Control> {
        for c in cs {
            if c.kind == "action" && c.name.eq_ignore_ascii_case(n) {
                return Some(c);
            }
            if let Some(x) = find(&c.children, n) {
                return Some(x);
            }
        }
        None
    }
    let act = find(&p.actions, &name).ok_or_else(|| ApiErr(StatusCode::NOT_FOUND, format!("action {name} not found"), None))?;
    let pr = *act.triggers.get("onaction").ok_or_else(|| bad("action has no OnAction trigger"))?;
    let tid = p.source_table;
    blocking(move || {
        let t0 = Instant::now();
        let mut s = session(ctx)?;
        let inst = al_runtime::Instance::new(p.code);
        let rec = match tid {
            Some(tid) => {
                let h = s.new_record(tid, false);
                if let Some(k) = q.key.as_deref() {
                    load_by_key(&mut s, &h, k)?;
                }
                *inst.rec.borrow_mut() = Some(h.clone());
                Some(h)
            }
            None => None,
        };
        let r = s.invoke(pr, vec![], Some(inst), rec.clone(), None).map_err(al);
        let r = with_rollback(&mut s, r);
        r?;
        finish(&mut s)?;
        let still_exists = rec.as_ref().is_some_and(|h| s.rec_get_pk(h).is_some());
        let record = match (&rec, still_exists) {
            (Some(h), true) => {
                let row = s.rec_get_pk(h).unwrap();
                h.borrow_mut().row = row.into();
                Some(card_json(&mut s, p, h)?)
            }
            _ => None,
        };
        Ok(Json(json!({"record": record, "deleted": !still_exists && rec.is_some(), "messages": s.messages, "steps": s.steps, "ms": t0.elapsed().as_secs_f64() * 1000.0})))
    })
    .await
}

async fn new_record(State(ctx): S, Path(id): Path<i64>, Json(body): Json<HashMap<String, J>>) -> Result<Json<J>, ApiErr> {
    let p = page(ctx, id)?;
    let tid = p.source_table.ok_or_else(|| bad("page has no source table"))?;
    blocking(move || {
        let t0 = Instant::now();
        let mut s = session(ctx)?;
        let h = s.new_record(tid, false);
        let t = &ctx.rt.cat.tables[&tid];
        let r = (|| {
            // fixed filters + primary key values first, then Insert(true), then validate the rest
            apply_defaults(&mut s, p, &h, &body)?;
            if let Err(m) = s.rec_insert(&h, true).map_err(al)? {
                return Err(ApiErr(StatusCode::CONFLICT, m, None));
            }
            let mut changed = false;
            for (k, v) in &body {
                let Some(fi) = t.field(k) else { continue };
                if t.pk().fields.contains(&fi) {
                    continue;
                }
                let val = parse_input(&s, &t.fields[fi], v)?;
                let fid = t.fields[fi].id as i32;
                s.rec_validate(&h, fi, Some(val), fid).map_err(al)?;
                changed = true;
            }
            if changed {
                if let Err(m) = s.rec_modify(&h, true).map_err(al)? {
                    return Err(bad(m));
                }
            }
            card_json(&mut s, p, &h)
        })();
        let j = with_rollback(&mut s, r)?;
        finish(&mut s)?;
        Ok(Json(json!({"record": j, "messages": s.messages, "steps": s.steps, "ms": t0.elapsed().as_secs_f64() * 1000.0})))
    })
    .await
}

async fn patch_record(State(ctx): S, Path(id): Path<i64>, Query(q): Query<KeyQ>, Json(body): Json<HashMap<String, J>>) -> Result<Json<J>, ApiErr> {
    let p = page(ctx, id)?;
    let tid = p.source_table.ok_or_else(|| bad("page has no source table"))?;
    let key = q.key.ok_or_else(|| bad("key required"))?;
    blocking(move || {
        let t0 = Instant::now();
        let mut s = session(ctx)?;
        let h = s.new_record(tid, false);
        let t = &ctx.rt.cat.tables[&tid];
        let r = (|| {
            load_by_key(&mut s, &h, &key)?;
            let mut renamed = false;
            for (k, v) in &body {
                let Some(fi) = t.field(k) else { return Err(bad(format!("unknown field {k}"))) };
                let val = parse_input(&s, &t.fields[fi], v)?;
                let fid = t.fields[fi].id as i32;
                if t.pk().fields.contains(&fi) {
                    renamed = true;
                }
                s.rec_validate(&h, fi, Some(val), fid).map_err(al)?;
                page_field_trigger(&mut s, p, &h, fi)?;
            }
            if renamed {
                return Err(bad("renaming records from the web client is not supported yet"));
            }
            if let Err(m) = s.rec_modify(&h, true).map_err(al)? {
                return Err(bad(m));
            }
            card_json(&mut s, p, &h)
        })();
        let j = with_rollback(&mut s, r)?;
        finish(&mut s)?;
        Ok(Json(json!({"record": j, "messages": s.messages, "steps": s.steps, "ms": t0.elapsed().as_secs_f64() * 1000.0})))
    })
    .await
}

async fn delete_record(State(ctx): S, Path(id): Path<i64>, Query(q): Query<KeyQ>) -> Result<Json<J>, ApiErr> {
    let p = page(ctx, id)?;
    let tid = p.source_table.ok_or_else(|| bad("page has no source table"))?;
    let key = q.key.ok_or_else(|| bad("key required"))?;
    blocking(move || {
        let t0 = Instant::now();
        let mut s = session(ctx)?;
        let h = s.new_record(tid, false);
        let r = (|| {
            load_by_key(&mut s, &h, &key)?;
            if let Err(m) = s.rec_delete(&h, true).map_err(al)? {
                return Err(bad(m));
            }
            Ok(())
        })();
        with_rollback(&mut s, r)?;
        finish(&mut s)?;
        Ok(Json(json!({"deleted": true, "messages": s.messages, "steps": s.steps, "ms": t0.elapsed().as_secs_f64() * 1000.0})))
    })
    .await
}

pub fn _unused(_: &dyn Fn() -> anyhow::Error) {
    let _ = flow_err;
}
