//! The agent REST API under `/v1` — spec/api/conventions.md.

use crate::App;
use crate::auth::Caller;
use crate::config::Role;
use crate::error::ApiError;
use crate::registry::{G, now_ms};
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mygraph_core::ids::{ObjectId, ObjectKind};
use mygraph_core::store::Store;
use serde_json::{Map, Value as Json, json};
use std::sync::{Arc, PoisonError};
use std::time::Instant;

/// [API-OP-1] Operations that only read, and those that change the graph.
const READS: [&str; 8] = [
    "overview",
    "search",
    "resolve",
    "get",
    "neighbors",
    "paths",
    "find",
    "history",
];
const WRITES: [&str; 3] = ["write", "retract", "merge"];

type Params = Result<Query<Vec<(String, String)>>, QueryRejection>;
pub type Body = Result<Bytes, BytesRejection>;

pub fn json_body(v: &Json) -> impl IntoResponse + use<> {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        serde_json::to_vec(v).unwrap_or_default(),
    )
}

/// Records the request in the metrics, logs slow ones, and builds the response.
pub fn finish(
    app: &App,
    op: &str,
    graph: Option<&str>,
    start: Instant,
    r: Result<Response, ApiError>,
) -> Response {
    let resp = r.unwrap_or_else(IntoResponse::into_response);
    let took = start.elapsed();
    let status = resp.status().as_u16();
    app.metrics.record(op, status, took);
    let ms = took.as_secs_f64() * 1000.0;
    if ms >= app.config.slow_ms as f64 {
        tracing::warn!(op, graph, status, ms, "slow request");
    } else {
        tracing::debug!(op, graph, status, ms, "request");
    }
    resp
}

fn ok(v: &Json) -> Result<Response, ApiError> {
    Ok(json_body(v).into_response())
}

/// [API-HTTP-1] [API-HTTP-3] The body as JSON; empty means `{}`.
pub fn parse_body(body: Body) -> Result<Json, ApiError> {
    let bytes = body.map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::new("api.body_too_large", "the request body is too large")
        } else {
            ApiError::new("json.malformed", e.body_text())
        }
    })?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| ApiError::new("text.invalid_unicode", "the body is not UTF-8"))?;
    serde_json::from_str(text).map_err(|e| {
        let code = if text.contains("\\ud") || text.contains("\\uD") {
            "text.invalid_unicode"
        } else {
            "json.malformed"
        };
        ApiError::new(code, e.to_string())
    })
}

/// Checks `role`, then runs `f` on the graph under the shared lock, or the exclusive one when
/// `write`, on a blocking thread ([API-GRAPH-3]).
pub async fn with_graph<T: Send + 'static>(
    app: &Arc<App>,
    caller: &Caller,
    graph: &str,
    role: Role,
    write: bool,
    f: impl FnOnce(&mut GraphAccess) -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    caller.require(role)?;
    caller.check_graph(graph)?;
    let handle = app.registry.get(graph).await?;
    let name = graph.to_owned();
    tokio::task::spawn_blocking(move || {
        let gone = || crate::registry::not_found(&name);
        if write {
            let mut guard = handle.write().unwrap_or_else(PoisonError::into_inner);
            let g = guard.as_mut().ok_or_else(gone)?;
            f(&mut GraphAccess::Write(g))
        } else {
            let guard = handle.read().unwrap_or_else(PoisonError::into_inner);
            let g = guard.as_ref().ok_or_else(gone)?;
            f(&mut GraphAccess::Read(g))
        }
    })
    .await
    .map_err(|e| ApiError::new("internal", e.to_string()))?
}

pub enum GraphAccess<'a> {
    Read(&'a G),
    Write(&'a mut G),
}

impl GraphAccess<'_> {
    pub fn graph(&self) -> &G {
        match self {
            Self::Read(g) => g,
            Self::Write(g) => g,
        }
    }
}

/// Runs operation `op` with `request` as `caller` ([API-OP-1]).
pub async fn run(
    app: &Arc<App>,
    caller: &Caller,
    graph: &str,
    op: &'static str,
    request: Json,
) -> Result<Json, ApiError> {
    let actor = caller.actor.clone();
    let write = WRITES.contains(&op);
    let role = if write {
        Role::AgentWrite
    } else {
        Role::AgentRead
    };
    with_graph(app, caller, graph, role, write, move |access| {
        let r = match access {
            GraphAccess::Write(g) => match op {
                "write" => g.write(&actor, &request, now_ms()),
                "retract" => g.retract(&actor, &request, now_ms()),
                "merge" => g.merge(&actor, &request, now_ms()),
                _ => unreachable!("write operations"),
            },
            GraphAccess::Read(g) => match op {
                "overview" => g.overview(&request),
                "search" => g.search(&request),
                "resolve" => g.resolve(&request),
                "get" => g.get(&request),
                "neighbors" => g.neighbors(&request),
                "paths" => g.paths(&request),
                "find" => g.find(&request),
                "history" => g.history(&request),
                _ => unreachable!("read operations"),
            },
        };
        r.map_err(ApiError::from)
    })
    .await
}

pub fn operation(name: &str) -> Option<&'static str> {
    READS
        .iter()
        .chain(WRITES.iter())
        .find(|o| **o == name)
        .copied()
}

/// [API-OP-1] Removes a `graph` member equal to the path's graph.
fn strip_graph(mut req: Json, graph: &str) -> Result<Json, ApiError> {
    if let Some(m) = req.as_object_mut()
        && let Some(g) = m.remove("graph")
        && g.as_str() != Some(graph)
    {
        return Err(ApiError::new(
            "api.graph_mismatch",
            format!("graph in the body differs from the path's graph {graph}"),
        ));
    }
    Ok(req)
}

// ---------------------------------------------------------------------------------------------
// Query strings ([API-SHORT-1])

struct Q(Vec<(String, String)>);

impl Q {
    fn new(p: Params, allowed: &[&str]) -> Result<Self, ApiError> {
        let Query(pairs) = p.map_err(|e| ApiError::new("api.invalid_parameter", e.body_text()))?;
        for (k, _) in &pairs {
            if !allowed.contains(&k.as_str()) {
                return Err(ApiError::new(
                    "api.invalid_parameter",
                    format!("unknown query parameter {k}"),
                ));
            }
        }
        Ok(Self(pairs))
    }

    fn str(&self, k: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    }

    fn int(&self, k: &str) -> Result<Option<Json>, ApiError> {
        self.str(k)
            .map(|v| {
                v.parse::<i64>().map(Json::from).map_err(|_| {
                    ApiError::new("api.invalid_parameter", format!("{k}: expected an integer"))
                })
            })
            .transpose()
    }

    fn bool(&self, k: &str) -> Result<Option<Json>, ApiError> {
        self.str(k)
            .map(|v| match v {
                "true" => Ok(Json::Bool(true)),
                "false" => Ok(Json::Bool(false)),
                _ => Err(ApiError::new(
                    "api.invalid_parameter",
                    format!("{k}: expected true or false"),
                )),
            })
            .transpose()
    }

    fn list(&self, k: &str) -> Option<Vec<String>> {
        self.str(k).map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
    }

    /// Copies parameters into a request object: (query name, request member, kind).
    fn to_request(&self, map: &[(&str, &str, Kind)]) -> Result<Map<String, Json>, ApiError> {
        let mut req = Map::new();
        for &(q, member, kind) in map {
            let v = match kind {
                Kind::Str => self.str(q).map(Json::from),
                Kind::Int => self.int(q)?,
                Kind::Bool => self.bool(q)?,
                Kind::List => self.list(q).map(Json::from),
            };
            if let Some(v) = v {
                req.insert(member.to_owned(), v);
            }
        }
        Ok(req)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Str,
    Int,
    Bool,
    List,
}

/// [API-SHORT-2] The object ID of a shorthand path, of the collection's kind.
fn object_id(collection: &str, id: &str) -> Result<(ObjectKind, i64), ApiError> {
    let kind = match collection {
        "nodes" => ObjectKind::Node,
        "links" => ObjectKind::Link,
        "evidence" => ObjectKind::Evidence,
        "citations" => ObjectKind::Citation,
        _ => return Err(api_not_found()),
    };
    match ObjectId::decode(id) {
        Ok(o) if o.kind == kind => Ok((kind, o.number as i64)),
        _ => Err(ApiError::new(
            "ref.not_found",
            format!("no {} with ID {id}", kind.name()),
        )),
    }
}

pub fn api_not_found() -> ApiError {
    ApiError::new("api.not_found", "no such route")
}

/// The single item of a `get` response, or its per-item error ([OP-GET-1]).
fn single_item(mut resp: Json) -> Result<Json, ApiError> {
    let item = resp["items"]
        .as_array_mut()
        .and_then(|a| a.pop())
        .unwrap_or(Json::Null);
    if let Some(e) = item.get("error") {
        let mut err = ApiError::new(
            e["code"].as_str().unwrap_or("ref.not_found"),
            e["message"].as_str().unwrap_or(""),
        );
        err.details = e.get("details").cloned().unwrap_or(Json::Null);
        return Err(err);
    }
    Ok(item)
}

// ---------------------------------------------------------------------------------------------
// Handlers

/// `POST /v1/graphs/{graph}/{operation}`
pub async fn post_op(
    State(app): State<Arc<App>>,
    Path((graph, name)): Path<(String, String)>,
    caller: Caller,
    headers: axum::http::HeaderMap,
    body: Body,
) -> Response {
    let start = Instant::now();
    let Some(op) = operation(&name) else {
        return finish(&app, "unknown", Some(&graph), start, Err(api_not_found()));
    };
    let r = async {
        let req = strip_graph(parse_body(body)?, &graph)?;
        let resp = run(&app, &caller, &graph, op, req).await?;
        // [compact-format.md] on request.
        let text = headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.starts_with("text/plain"));
        if text {
            return Ok((
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                )],
                crate::compact::render(op, &resp),
            )
                .into_response());
        }
        ok(&resp)
    }
    .await;
    finish(&app, op, Some(&graph), start, r)
}

/// `GET /v1/graphs`
pub async fn list_graphs(State(app): State<Arc<App>>, caller: Caller) -> Response {
    let start = Instant::now();
    let r = caller.require(Role::AgentRead).and_then(|()| {
        let graphs: Vec<Json> = app
            .registry
            .list()
            .into_iter()
            .filter(|(n, _)| caller.may_use(n))
            .map(|(n, s)| json!({"name": n, "status": s.name()}))
            .collect();
        ok(&json!({ "graphs": graphs }))
    });
    finish(&app, "list_graphs", None, start, r)
}

/// `GET /v1/graphs/{graph}`: `overview`.
pub async fn get_graph(
    State(app): State<Arc<App>>,
    Path(graph): Path<String>,
    caller: Caller,
    params: Params,
) -> Response {
    let start = Instant::now();
    let r = async {
        let q = Q::new(params, &["type", "recent", "types_limit"])?;
        let req = q.to_request(&[
            ("type", "type", Kind::Str),
            ("recent", "recent", Kind::Int),
            ("types_limit", "types_limit", Kind::Int),
        ])?;
        ok(&run(&app, &caller, &graph, "overview", Json::Object(req)).await?)
    }
    .await;
    finish(&app, "overview", Some(&graph), start, r)
}

/// `GET /v1/graphs/{graph}/search` and `GET /v1/graphs/{graph}/changes`.
pub async fn get_named(
    State(app): State<Arc<App>>,
    Path((graph, name)): Path<(String, String)>,
    caller: Caller,
    params: Params,
) -> Response {
    let start = Instant::now();
    let (label, r) = match name.as_str() {
        "search" => ("search", search_get(&app, &caller, &graph, params).await),
        "changes" => ("history", changes_get(&app, &caller, &graph, params).await),
        _ => ("unknown", Err(api_not_found())),
    };
    finish(&app, label, Some(&graph), start, r)
}

async fn search_get(
    app: &Arc<App>,
    caller: &Caller,
    graph: &str,
    params: Params,
) -> Result<Response, ApiError> {
    let q = Q::new(
        params,
        &["q", "in", "types", "mode", "limit", "include_retracted"],
    )?;
    let req = q.to_request(&[
        ("q", "query", Kind::Str),
        ("in", "in", Kind::List),
        ("types", "types", Kind::List),
        ("mode", "mode", Kind::Str),
        ("limit", "limit", Kind::Int),
        ("include_retracted", "include_retracted", Kind::Bool),
    ])?;
    ok(&run(app, caller, graph, "search", Json::Object(req)).await?)
}

async fn changes_get(
    app: &Arc<App>,
    caller: &Caller,
    graph: &str,
    params: Params,
) -> Result<Response, ApiError> {
    let q = Q::new(params, &["since_seq", "limit"])?;
    let mut req = q.to_request(&[
        ("since_seq", "since_seq", Kind::Int),
        ("limit", "limit", Kind::Int),
    ])?;
    req.entry("since_seq").or_insert(json!(0));
    ok(&run(app, caller, graph, "history", Json::Object(req)).await?)
}

/// `GET /v1/graphs/{graph}/{collection}/{id}`: one object via `get`.
pub async fn get_object(
    State(app): State<Arc<App>>,
    Path((graph, collection, id)): Path<(String, String, String)>,
    caller: Caller,
    params: Params,
) -> Response {
    let start = Instant::now();
    let r = async {
        object_id(&collection, &id)?;
        let q = Q::new(
            params,
            &[
                "detail",
                "include",
                "links_limit",
                "link_types",
                "evidence_limit",
                "include_retracted",
            ],
        )?;
        let mut req = q.to_request(&[
            ("detail", "detail", Kind::Str),
            ("include_retracted", "include_retracted", Kind::Bool),
        ])?;
        req.insert("refs".into(), json!([id]));
        let listed = q.list("include");
        let mut include = Map::new();
        for part in ["links", "support", "evidence", "content"] {
            if let Some(l) = &listed {
                include.insert(part.into(), l.iter().any(|x| x == part).into());
            }
        }
        for (part, opts) in [
            (
                "links",
                vec![
                    ("limit", q.int("links_limit")?),
                    ("types", q.list("link_types").map(Json::from)),
                ],
            ),
            ("evidence", vec![("limit", q.int("evidence_limit")?)]),
        ] {
            let opts: Map<String, Json> = opts
                .into_iter()
                .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v)))
                .collect();
            if !opts.is_empty() && include.get(part) != Some(&Json::Bool(false)) {
                include.insert(part.into(), Json::Object(opts));
            }
        }
        if !include.is_empty() {
            req.insert("include".into(), Json::Object(include));
        }
        let resp = run(&app, &caller, &graph, "get", Json::Object(req)).await?;
        ok(&single_item(resp)?)
    }
    .await;
    finish(&app, "get", Some(&graph), start, r)
}

/// `DELETE /v1/graphs/{graph}/{collection}/{id}`: retract it.
pub async fn delete_object(
    State(app): State<Arc<App>>,
    Path((graph, collection, id)): Path<(String, String, String)>,
    caller: Caller,
    params: Params,
) -> Response {
    let start = Instant::now();
    let r = async {
        object_id(&collection, &id)?;
        let q = Q::new(params, &["reason"])?;
        let mut req = q.to_request(&[("reason", "reason", Kind::Str)])?;
        req.insert("targets".into(), json!([id]));
        ok(&run(&app, &caller, &graph, "retract", Json::Object(req)).await?)
    }
    .await;
    finish(&app, "retract", Some(&graph), start, r)
}

/// `GET /v1/graphs/{graph}/{collection}/{id}/{history|content|blob}`
pub async fn get_object_part(
    State(app): State<Arc<App>>,
    Path((graph, collection, id, part)): Path<(String, String, String, String)>,
    caller: Caller,
    params: Params,
) -> Response {
    let start = Instant::now();
    let (label, r) = match part.as_str() {
        "history" => (
            "history",
            object_history(&app, &caller, &graph, &collection, &id, params).await,
        ),
        "content" if collection == "evidence" => (
            "content",
            evidence_content(&app, &caller, &graph, &id, params).await,
        ),
        "blob" if collection == "evidence" => (
            "blob",
            evidence_blob(&app, &caller, &graph, &id, params).await,
        ),
        _ => ("unknown", Err(api_not_found())),
    };
    finish(&app, label, Some(&graph), start, r)
}

async fn object_history(
    app: &Arc<App>,
    caller: &Caller,
    graph: &str,
    collection: &str,
    id: &str,
    params: Params,
) -> Result<Response, ApiError> {
    object_id(collection, id)?;
    let q = Q::new(params, &["limit", "before_seq"])?;
    let mut req = q.to_request(&[
        ("limit", "limit", Kind::Int),
        ("before_seq", "before_seq", Kind::Int),
    ])?;
    req.insert("target".into(), json!(id));
    ok(&run(app, caller, graph, "history", Json::Object(req)).await?)
}

async fn evidence_content(
    app: &Arc<App>,
    caller: &Caller,
    graph: &str,
    id: &str,
    params: Params,
) -> Result<Response, ApiError> {
    object_id("evidence", id)?;
    let q = Q::new(params, &["from", "max_chars"])?;
    let range = q.to_request(&[
        ("from", "from", Kind::Int),
        ("max_chars", "max_chars", Kind::Int),
    ])?;
    let content = if range.is_empty() {
        json!(true)
    } else {
        Json::Object(range)
    };
    let req = json!({ "refs": [id], "detail": "brief", "include": { "content": content } });
    let item = single_item(run(app, caller, graph, "get", req).await?)?;
    let Some(text) = item.get("content").and_then(Json::as_str) else {
        return Err(ApiError::new(
            "content.not_found",
            format!("{id} has no inline content"),
        ));
    };
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        text.to_owned(),
    )
        .into_response())
}

async fn evidence_blob(
    app: &Arc<App>,
    caller: &Caller,
    graph: &str,
    id: &str,
    params: Params,
) -> Result<Response, ApiError> {
    let (_, idx) = object_id("evidence", id)?;
    Q::new(params, &[])?;
    let id = id.to_owned();
    let (bytes, sha, mime) =
        with_graph(app, caller, graph, Role::AgentRead, false, move |access| {
            let g = access.graph();
            let missing = || ApiError::new("blob.not_found", format!("{id} has no blob"));
            let e = g.state().evidence.get(&idx).ok_or_else(|| {
                ApiError::new("ref.not_found", format!("no evidence with ID {id}"))
            })?;
            let sha = e.blob_sha256.clone().ok_or_else(missing)?;
            let bytes = g
                .store()
                .blobs()
                .get(&sha)
                .map_err(|e| ApiError::new("storage.failure", e.to_string()))?
                .ok_or_else(missing)?;
            Ok((bytes, sha, e.mime.clone()))
        })
        .await?;
    let mime = mime
        .and_then(|m| HeaderValue::from_str(&m).ok())
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));
    let etag = HeaderValue::from_str(&format!("\"{sha}\"")).expect("hex digits");
    Ok(([(header::CONTENT_TYPE, mime), (header::ETAG, etag)], bytes).into_response())
}

/// `POST /v1/graphs/{graph}/{collection}/{id}/restore`
pub async fn post_object_part(
    State(app): State<Arc<App>>,
    Path((graph, collection, id, part)): Path<(String, String, String, String)>,
    caller: Caller,
    params: Params,
) -> Response {
    let start = Instant::now();
    let r = async {
        if part != "restore" {
            return Err(api_not_found());
        }
        object_id(&collection, &id)?;
        let q = Q::new(params, &["reason"])?;
        let mut req = q.to_request(&[("reason", "reason", Kind::Str)])?;
        req.insert("action".into(), json!("restore"));
        req.insert("targets".into(), json!([id]));
        ok(&run(&app, &caller, &graph, "retract", Json::Object(req)).await?)
    }
    .await;
    finish(&app, "retract", Some(&graph), start, r)
}
