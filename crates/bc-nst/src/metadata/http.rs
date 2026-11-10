//! REST endpoints under `<service base URL>client/metadata/`.

use super::{
    homepage_versioned, master_navigation_versioned, master_page_versioned, meta_table_versioned, navigation, page_with_dependencies_versioned,
    system_part_versioned, MetadataState,
};
use axum::body::Bytes;
use axum::extract::Request;
use axum::extract::{Path, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::Value;
use std::collections::HashMap;

type Params = Path<HashMap<String, String>>;

fn token_header(token: i64) -> [(HeaderName, HeaderValue); 1] {
    [(HeaderName::from_static("metadatatoken"), HeaderValue::from_str(&token.to_string()).unwrap_or(HeaderValue::from_static("0")))]
}

fn reply(st: &MetadataState, v: Value) -> Response {
    if v.is_null() {
        return (StatusCode::NOT_FOUND, token_header(st.token), "not found").into_response();
    }
    (StatusCode::OK, token_header(st.token), axum::Json(v)).into_response()
}

/// HTTP 521 with a `ServerFault` body: what the real NST answers for a page or table id
/// that does not exist. A plain 404 made the web server drop the whole session.
fn not_found(st: &MetadataState, object: &str) -> Response {
    let f = crate::wire::Fault::new(crate::wire::WireException::metadata_not_found(object));
    (StatusCode::from_u16(521).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), token_header(st.token), axum::Json(f.server_fault())).into_response()
}

/// Metadata of object `kind` `id`, or the not-found fault.
fn reply_object(st: &MetadataState, kind: &str, id: Option<i64>, v: Value) -> Response {
    if v.is_null() {
        return not_found(st, &format!("{kind} {}", id.map(|i| i.to_string()).unwrap_or_default()));
    }
    reply(st, v)
}

fn id(p: &HashMap<String, String>, k: &str) -> Option<i64> {
    p.get(k).and_then(|s| s.parse().ok())
}

async fn page(State(st): State<MetadataState>, Path(p): Params) -> Response {
    let v = id(&p, "pageId").map(|i| master_page_versioned(st.catalog, i, st.token)).unwrap_or(Value::Null);
    reply_object(&st, "Page", id(&p, "pageId"), v)
}

async fn page_deps(State(st): State<MetadataState>, Path(p): Params, body: Bytes) -> Response {
    // body: JSON array of table ids the web server already has cached (ints or numeric strings)
    let cached: Vec<i64> = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
        .collect();
    let v = id(&p, "pageId").map(|i| page_with_dependencies_versioned(st.catalog, i, &cached, st.token)).unwrap_or(Value::Null);
    reply_object(&st, "Page", id(&p, "pageId"), v)
}

async fn table(State(st): State<MetadataState>, Path(p): Params) -> Response {
    let v = id(&p, "tableId").map(|i| meta_table_versioned(st.catalog, i, st.token)).unwrap_or(Value::Null);
    reply_object(&st, "Table", id(&p, "tableId"), v)
}

async fn system_part(State(st): State<MetadataState>, Path(p): Params) -> Response {
    let v = p.get("partType").map(|t| system_part_versioned(t, st.token)).unwrap_or(Value::Null);
    reply(&st, v)
}

async fn nav(State(st): State<MetadataState>) -> Response {
    reply(&st, master_navigation_versioned(st.catalog, &st.profile, st.token))
}

async fn product_overview(State(st): State<MetadataState>) -> Response {
    reply(&st, navigation::product_overview(st.catalog, &st.profile, st.token))
}

async fn homepage(State(st): State<MetadataState>) -> Response {
    reply(&st, homepage_versioned(st.catalog, &st.profile, st.token))
}

/// One line per metadata call in the server log (method, path, status, time).
async fn log_call(req: Request, next: Next) -> Response {
    let (method, uri) = (req.method().clone(), req.uri().clone());
    let t0 = std::time::Instant::now();
    let res = next.run(req).await;
    eprintln!("bc-nst metadata: {method} {uri} -> {} ({:.1} ms)", res.status().as_u16(), t0.elapsed().as_secs_f64() * 1000.0);
    res
}

async fn accept(State(st): State<MetadataState>) -> Response {
    (StatusCode::OK, token_header(st.token)).into_response()
}

/// Routes relative to `/{instance}/client/metadata`. Path parameters are read by name, so
/// the router also works when nested under a prefix that has its own parameters.
pub fn router(state: MetadataState) -> Router {
    Router::new()
        .route("/pagewithdependencies/{pageId}", post(page_deps).get(page_deps))
        .route("/pages/{pageId}", get(page))
        .route("/tables/{tableId}", get(table))
        .route("/systemparts/{partType}", get(system_part))
        .route("/navigation", get(nav))
        .route("/productoverview", get(product_overview))
        .route("/homepage", get(homepage))
        .route("/configuration", post(accept))
        .route("/pagedatapersonalization", post(accept))
        .with_state(state)
        .layer(axum::middleware::from_fn(log_call))
}

/// [`router`] nested at `/{instance}/client/metadata`, ready to `merge` into the NST router.
pub fn mounted_router(state: MetadataState) -> Router {
    Router::new().nest("/{instance}/client/metadata", router(state))
}
