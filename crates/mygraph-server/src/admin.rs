//! The admin API under `/admin` — spec/api/admin.md.

use crate::App;
use crate::api::{Body, api_not_found, finish, json_body, parse_body, with_graph};
use crate::auth::Caller;
use crate::config::Role;
use crate::error::ApiError;
use crate::registry::{SlotState, now_ms};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use mygraph_core::model::Policies;
use mygraph_core::stats::{Section, StatsRequest};
use mygraph_store_sqlite::GraphFile;
use serde_json::{Map, Value as Json, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// UTC time as `YYYYMMDDTHHMMSSmmmZ`, for file names ([ADMIN-GRAPH-4]).
pub fn stamp(ms: i64) -> String {
    let t = mygraph_core::model::time_json(ms);
    // "2026-10-25T09:12:00.000Z" -> "20261025T091200000Z"
    t.as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new("admin.invalid_field", msg)
}

/// [ADMIN-GRAPH-1]
async fn graph_info(app: &Arc<App>, caller: &Caller, name: &str) -> Result<Json, ApiError> {
    let state = app
        .registry
        .state(name)
        .ok_or_else(|| crate::registry::not_found(name))?;
    let (file_bytes, wal_bytes) = GraphFile::file_sizes(app.registry.data_dir(), name);
    let mut info = json!({
        "name": name, "status": state.name(), "file_bytes": file_bytes, "wal_bytes": wal_bytes,
    });
    if let SlotState::Ready(_) = state {
        let loaded = with_graph(app, caller, name, Role::Admin, false, |access| {
            let g = access.graph();
            let counts: Map<String, Json> = g
                .counts()
                .into_iter()
                .map(|(k, n)| (k.to_owned(), n.into()))
                .collect();
            Ok(json!({
                "counts": counts, "policies": g.policies().to_json(), "last_seq": g.last_seq(),
                "text_index": g.text_index_status(),
                "semantic_index": g.semantic_index_status(),
            }))
        })
        .await?;
        if let (Some(i), Some(l)) = (info.as_object_mut(), loaded.as_object()) {
            i.extend(l.clone());
        }
    }
    Ok(info)
}

/// `GET /admin/graphs` and `POST /admin/graphs`.
pub async fn list_graphs(State(app): State<Arc<App>>, caller: Caller) -> Response {
    let start = Instant::now();
    let r = async {
        caller.require(Role::Admin)?;
        let mut graphs = Vec::new();
        for (name, _) in app.registry.list() {
            graphs.push(graph_info(&app, &caller, &name).await?);
        }
        Ok(json_body(&json!({ "graphs": graphs })).into_response())
    }
    .await;
    finish(&app, "admin.list_graphs", None, start, r)
}

/// Creates a graph from `{"name", "policies"}` and returns its `GraphInfo` ([ADMIN-GRAPH-2]);
/// shared by `POST /admin/graphs` and the MCP `graphs` tool.
pub async fn create(app: &Arc<App>, caller: &Caller, req: &Json) -> Result<Json, ApiError> {
    caller.require(Role::Admin)?;
    let req = req
        .as_object()
        .ok_or_else(|| bad("request: expected an object"))?;
    {
        for k in req.keys() {
            if k != "name" && k != "policies" {
                return Err(bad(format!("{k}: no such field")));
            }
        }
        let name = req
            .get("name")
            .and_then(Json::as_str)
            .ok_or_else(|| bad("name: expected a string"))?;
        mygraph_core::ids::validate_graph_name(name)
            .map_err(|r| ApiError::new(r.code(), format!("name {name:?}: {r}")))?;
        let mut policies = Policies::default().to_json();
        if let Some(p) = req.get("policies") {
            let p = p
                .as_object()
                .ok_or_else(|| bad("policies: expected an object"))?;
            for (k, v) in p {
                if policies.get(k).is_none() {
                    return Err(bad(format!("policies.{k}: no such policy")));
                }
                policies[k] = v.clone();
            }
        }
        let policies = Policies::from_json(&policies)
            .filter(|p| p.validate().is_ok())
            .ok_or_else(|| ApiError::new("graph.policy_invalid", "invalid policies"))?;
        app.registry.create(name, &policies)?;
        graph_info(app, caller, name).await
    }
}

pub async fn create_graph(State(app): State<Arc<App>>, caller: Caller, body: Body) -> Response {
    let start = Instant::now();
    let r = async {
        let info = create(&app, &caller, &parse_body(body)?).await?;
        let name = info["name"].as_str().unwrap_or_default().to_owned();
        let mut resp = (StatusCode::CREATED, json_body(&info)).into_response();
        if let Ok(loc) = HeaderValue::from_str(&format!("/admin/graphs/{name}")) {
            resp.headers_mut().insert(header::LOCATION, loc);
        }
        Ok(resp)
    }
    .await;
    finish(&app, "admin.create_graph", None, start, r)
}

/// `GET /admin/graphs/{graph}`
pub async fn get_graph(
    State(app): State<Arc<App>>,
    Path(graph): Path<String>,
    caller: Caller,
) -> Response {
    let start = Instant::now();
    let r = async {
        caller.require(Role::Admin)?;
        Ok(json_body(&graph_info(&app, &caller, &graph).await?).into_response())
    }
    .await;
    finish(&app, "admin.get_graph", Some(&graph), start, r)
}

/// `DELETE /admin/graphs/{graph}?confirm={graph}` ([ADMIN-GRAPH-3])
pub async fn delete_graph(
    State(app): State<Arc<App>>,
    Path(graph): Path<String>,
    caller: Caller,
    params: Result<Query<HashMap<String, String>>, QueryRejection>,
) -> Response {
    let start = Instant::now();
    let r = async {
        caller.require(Role::Admin)?;
        let confirm = params.ok().and_then(|Query(q)| q.get("confirm").cloned());
        if confirm.as_deref() != Some(graph.as_str()) {
            return Err(ApiError::new(
                "admin.confirm_required",
                format!("add ?confirm={graph} to delete graph {graph}"),
            ));
        }
        let reg = app.registry.clone();
        let name = graph.clone();
        let moved = tokio::task::spawn_blocking(move || reg.delete(&name))
            .await
            .map_err(|e| ApiError::new("internal", e.to_string()))??;
        Ok(json_body(&json!({
            "deleted": graph, "moved_to": moved.display().to_string(),
        }))
        .into_response())
    }
    .await;
    finish(&app, "admin.delete_graph", Some(&graph), start, r)
}

/// `POST /admin/graphs/{graph}/{snapshot|check|export|import|restore}`
pub async fn graph_action(
    State(app): State<Arc<App>>,
    Path((graph, action)): Path<(String, String)>,
    caller: Caller,
    body: Body,
) -> Response {
    let start = Instant::now();
    let label = match action.as_str() {
        "snapshot" => "admin.snapshot",
        "check" => "admin.check",
        "export" => "admin.export",
        "import" => "admin.import",
        "restore" => "admin.restore",
        _ => "unknown",
    };
    let r = async {
        caller.require(Role::Admin)?;
        let data_dir = app.registry.data_dir().to_owned();
        let name = graph.clone();
        let body = match action.as_str() {
            // [ADMIN-GRAPH-4] Under the shared lock: reads go on, writes wait.
            "snapshot" => {
                with_graph(&app, &caller, &graph, Role::Admin, false, move |access| {
                    let g = access.graph();
                    let dir = data_dir.join("snapshots");
                    std::fs::create_dir_all(&dir)
                        .map_err(|e| ApiError::new("internal", e.to_string()))?;
                    let path = dir.join(format!("{name}-{}.db", stamp(now_ms())));
                    g.store()
                        .snapshot_to(&path)
                        .map_err(|e| ApiError::new("storage.failure", e.to_string()))?;
                    let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
                    Ok(json!({
                        "path": path.display().to_string(), "bytes": bytes,
                        "last_seq": g.last_seq(),
                    }))
                })
                .await?
            }
            // [ADMIN-GRAPH-5]
            "check" => {
                with_graph(&app, &caller, &graph, Role::Admin, false, |access| {
                    let problems = access
                        .graph()
                        .store()
                        .sqlite_check()
                        .map_err(|e| ApiError::new("storage.failure", e.to_string()))?;
                    Ok(json!({ "ok": problems.is_empty(), "problems": problems }))
                })
                .await?
            }
            // [ADMIN-GRAPH-7]
            "export" => with_graph(&app, &caller, &graph, Role::Admin, false, move |access| {
                let g = access.graph();
                let dir = data_dir.join("exports");
                let internal = |e: &dyn std::fmt::Display| ApiError::new("internal", e.to_string());
                std::fs::create_dir_all(&dir).map_err(|e| internal(&e))?;
                let path = dir.join(format!("{name}-{}.jsonl", stamp(now_ms())));
                let file = std::fs::File::create(&path).map_err(|e| internal(&e))?;
                let mut out = std::io::BufWriter::new(file);
                let counts = g
                    .export(&mut out, now_ms())
                    .map_err(|e| ApiError::new("storage.failure", e.to_string()))?;
                drop(out);
                let bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
                Ok(json!({ "path": path.display().to_string(), "bytes": bytes, "counts": counts }))
            })
            .await?,
            // [ADMIN-GRAPH-8]
            "import" => {
                let req = parse_body(body)?;
                let file = req
                    .get("file")
                    .and_then(Json::as_str)
                    .ok_or_else(|| bad("file: expected a file name in exports/"))?;
                let path = app.registry.data_file("exports", file)?;
                if app.registry.state(&graph).is_none() {
                    let header = std::io::BufRead::lines(std::io::BufReader::new(
                        std::fs::File::open(&path)
                            .map_err(|e| ApiError::new("internal", e.to_string()))?,
                    ))
                    .next()
                    .and_then(Result::ok)
                    .and_then(|l| serde_json::from_str::<Json>(&l).ok())
                    .unwrap_or(Json::Null);
                    let policies = Policies::from_json(&header["policies"]).unwrap_or_default();
                    app.registry.create(&graph, &policies)?;
                }
                let actor = caller.actor.clone();
                let imported =
                    with_graph(&app, &caller, &graph, Role::Admin, true, move |access| {
                        let crate::api::GraphAccess::Write(g) = access else {
                            unreachable!("write access")
                        };
                        let file = std::fs::File::open(&path)
                            .map_err(|e| ApiError::new("internal", e.to_string()))?;
                        let mut input = std::io::BufReader::new(file);
                        Ok(g.import(&actor, &mut input, now_ms())?)
                    })
                    .await?;
                let mut info = graph_info(&app, &caller, &graph).await?;
                info["imported"] = imported;
                info
            }
            // [ADMIN-GRAPH-6]
            "restore" => {
                let req = parse_body(body)?;
                let snapshot = req
                    .get("snapshot")
                    .and_then(Json::as_str)
                    .ok_or_else(|| bad("snapshot: expected a file name in snapshots/"))?;
                let replace = req.get("replace").and_then(Json::as_bool).unwrap_or(false);
                let path = app.registry.data_file("snapshots", snapshot)?;
                let reg = app.registry.clone();
                let name = graph.clone();
                tokio::task::spawn_blocking(move || reg.restore(&name, &path, replace))
                    .await
                    .map_err(|e| ApiError::new("internal", e.to_string()))??;
                app.registry.get(&graph).await?;
                graph_info(&app, &caller, &graph).await?
            }
            _ => return Err(api_not_found()),
        };
        Ok(json_body(&body).into_response())
    }
    .await;
    finish(&app, label, Some(&graph), start, r)
}

/// Total size of the files under `dir` (0 when it does not exist).
fn dir_size(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|e| match e.file_type() {
                Ok(t) if t.is_dir() => dir_size(&e.path()),
                _ => e.metadata().map_or(0, |m| m.len()),
            })
            .sum()
    })
}

/// `GET /admin/graphs/{graph}/stats` ([ADMIN-GRAPH-9]).
pub async fn graph_get(
    State(app): State<Arc<App>>,
    Path((graph, what)): Path<(String, String)>,
    caller: Caller,
    params: crate::api::Params,
) -> Response {
    let start = Instant::now();
    let r = async {
        caller.require(Role::Admin)?;
        if what != "stats" {
            return Err(api_not_found());
        }
        let req = StatsRequest::parse(&crate::api::stats_query(params)?, &Section::ADMIN)?;
        let storage = req.wants(Section::Storage);
        let now = now_ms();
        let mut body = with_graph(&app, &caller, &graph, Role::Admin, false, move |access| {
            let g = access.graph();
            let mut v = g.compute_stats(&req, now)?;
            if storage {
                v["storage"] = json!({
                    "text_index": g.text_index_status(),
                    "semantic_index": g.semantic_index_status(),
                });
            }
            Ok(v)
        })
        .await?;
        if storage {
            let dir = app.registry.data_dir();
            let (file_bytes, wal_bytes) = GraphFile::file_sizes(dir, &graph);
            let text_bytes = dir_size(&mygraph_text_tantivy::index_dir(dir, &graph));
            let semantic_bytes = dir_size(&mygraph_semantic::index_dir(dir, &graph));
            let s = &mut body["storage"];
            s["file_bytes"] = file_bytes.into();
            s["wal_bytes"] = wal_bytes.into();
            s["text_index_bytes"] = text_bytes.into();
            s["semantic_index_bytes"] = semantic_bytes.into();
        }
        Ok(json_body(&body).into_response())
    }
    .await;
    finish(&app, "admin.stats", Some(&graph), start, r)
}

/// `GET /admin/stats`, `/admin/slow-ops`, `/admin/keys`, `/admin/config` ([ADMIN-SRV-1],
/// [ADMIN-SRV-2]).
pub async fn server_info(
    State(app): State<Arc<App>>,
    Path(what): Path<String>,
    caller: Caller,
) -> Response {
    let start = Instant::now();
    let r = caller.require(Role::Admin).and_then(|()| {
        let body = match what.as_str() {
            "stats" => {
                let graphs: Map<String, Json> = app
                    .registry
                    .list()
                    .into_iter()
                    .map(|(n, s)| (n, s.name().into()))
                    .collect();
                json!({
                    "uptime_s": app.started.elapsed().as_secs(), "versions": crate::versions(),
                    "memory_bytes": crate::resident_bytes(), "slow_ms": app.config.slow_ms,
                    "graphs": graphs, "requests": app.metrics.summary(),
                })
            }
            // [ADMIN-SRV-2]
            "slow-ops" => json!({ "slow_ms": app.config.slow_ms, "ops": app.metrics.slow_ops() }),
            "keys" => app.keys.redacted(),
            "config" => app.config.redacted(),
            _ => return Err(api_not_found()),
        };
        Ok(json_body(&body).into_response())
    });
    finish(&app, "admin.server", None, start, r)
}
