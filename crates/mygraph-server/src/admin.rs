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

pub async fn create_graph(State(app): State<Arc<App>>, caller: Caller, body: Body) -> Response {
    let start = Instant::now();
    let r = async {
        caller.require(Role::Admin)?;
        let req = parse_body(body)?;
        let req = req
            .as_object()
            .ok_or_else(|| bad("request: expected an object"))?;
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
        let info = graph_info(&app, &caller, name).await?;
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

/// `POST /admin/graphs/{graph}/{snapshot|check}`
pub async fn graph_action(
    State(app): State<Arc<App>>,
    Path((graph, action)): Path<(String, String)>,
    caller: Caller,
) -> Response {
    let start = Instant::now();
    let label = match action.as_str() {
        "snapshot" => "admin.snapshot",
        "check" => "admin.check",
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
            _ => return Err(api_not_found()),
        };
        Ok(json_body(&body).into_response())
    }
    .await;
    finish(&app, label, Some(&graph), start, r)
}

/// `GET /admin/stats`, `/admin/keys`, `/admin/config` ([ADMIN-SRV-1]).
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
                    "uptime_s": app.started.elapsed().as_secs(), "graphs": graphs,
                    "requests": app.metrics.summary(),
                })
            }
            "keys" => app.keys.redacted(),
            "config" => app.config.redacted(),
            _ => return Err(api_not_found()),
        };
        Ok(json_body(&body).into_response())
    });
    finish(&app, "admin.server", None, start, r)
}
