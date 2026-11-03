//! The MyGraph HTTP server: the agent REST API (`/v1`, spec/api/conventions.md), the admin API
//! (`/admin`, spec/api/admin.md) and the service routes. MCP will be a further transport over
//! the same operations.

pub mod admin;
pub mod api;
pub mod auth;
pub mod compact;
pub mod config;
pub mod error;
pub mod mcp;
pub mod metrics;
pub mod registry;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use config::Config;
use error::ApiError;
use registry::{Registry, SlotState};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Shared server state.
pub struct App {
    pub config: Config,
    pub registry: Arc<Registry>,
    pub keys: auth::Keys,
    pub metrics: metrics::Metrics,
    pub started: Instant,
}

impl App {
    /// Finds the graphs in the data folder; call [`App::preload`] to start loading them.
    pub fn new(config: Config) -> Arc<Self> {
        let registry = Arc::new(Registry::new(
            &config.data_dir,
            config.text_index,
            load_model(&config),
            Duration::from_millis(config.warmup_wait_ms),
        ));
        Arc::new(Self {
            keys: auth::Keys::new(&config),
            config,
            registry,
            metrics: metrics::Metrics::default(),
            started: Instant::now(),
        })
    }

    /// Starts loading every graph in the background (needs a tokio runtime).
    pub fn preload(&self) {
        self.registry.preload();
    }
}

/// The embedding model for semantic search, when enabled and found; without it search matches
/// words only.
fn load_model(config: &Config) -> Option<Arc<dyn mygraph_semantic::Embed>> {
    if !config.semantic {
        return None;
    }
    let dir = config
        .model_dir
        .clone()
        .unwrap_or_else(|| mygraph_semantic::default_model_dir(&config.data_dir));
    if !dir.join("model.onnx").is_file() {
        tracing::info!(
            "no embedding model in {}: search matches words only",
            dir.display()
        );
        return None;
    }
    match mygraph_semantic::Model::load(&dir) {
        Ok(m) => Some(Arc::new(m)),
        Err(e) => {
            tracing::warn!("embedding model unavailable, search matches words only: {e}");
            None
        }
    }
}

pub fn versions() -> Value {
    json!({
        "server_version": env!("CARGO_PKG_VERSION"),
        "spec_version": mygraph_core::SPEC_VERSION,
        "storage_format_version": mygraph_core::STORAGE_FORMAT_VERSION,
    })
}

/// Every route ([API-OP-1], [API-SHORT-1], admin.md, conventions.md §6).
pub fn router(app: Arc<App>) -> Router {
    let limit = app.config.max_body_bytes;
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics))
        .route("/openapi.yaml", get(openapi))
        .route("/mcp", post(mcp_http))
        .route("/v1/graphs", get(api::list_graphs))
        .route("/v1/graphs/{graph}", get(api::get_graph))
        .route(
            "/v1/graphs/{graph}/{name}",
            get(api::get_named).post(api::post_op),
        )
        .route(
            "/v1/graphs/{graph}/{collection}/{id}",
            get(api::get_object).delete(api::delete_object),
        )
        .route(
            "/v1/graphs/{graph}/{collection}/{id}/{part}",
            get(api::get_object_part).post(api::post_object_part),
        )
        .route(
            "/admin/graphs",
            get(admin::list_graphs).post(admin::create_graph),
        )
        .route(
            "/admin/graphs/{graph}",
            get(admin::get_graph).delete(admin::delete_graph),
        )
        .route(
            "/admin/graphs/{graph}/{action}",
            get(admin::graph_get).post(admin::graph_action),
        )
        .route("/admin/ui", get(stats_page))
        .route("/admin/ui/graph", get(graph_page))
        .route("/admin/ui/cytoscape.min.js", get(cytoscape_js))
        .route("/admin/{what}", get(admin::server_info))
        .fallback(|| async { api::api_not_found() })
        .method_not_allowed_fallback(|| async {
            ApiError::new(
                "api.method_not_allowed",
                "this route does not take that method",
            )
        })
        .layer(DefaultBodyLimit::max(limit))
        .with_state(app)
}

/// Liveness. No authentication.
async fn health() -> Json<Value> {
    let mut body = versions();
    body["status"] = json!("ok");
    Json(body)
}

/// Readiness: every graph found at start is loaded.
async fn ready(State(app): State<Arc<App>>) -> Response {
    let graphs = app.registry.list();
    let all = graphs.iter().all(|(_, s)| matches!(s, SlotState::Ready(_)));
    let states: serde_json::Map<String, Value> = graphs
        .into_iter()
        .map(|(n, s)| (n, s.name().into()))
        .collect();
    let status = if all {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(json!({ "ready": all, "graphs": states }))).into_response()
}

/// Resident memory of this process, where the platform reports it.
pub fn resident_bytes() -> Option<u64> {
    memory_stats::memory_stats().map(|m| m.physical_mem as u64)
}

/// [ADMIN-MET-1] A loaded graph busy with a long write (an import) is not waited for.
async fn metrics(State(app): State<Arc<App>>) -> Response {
    let mut graphs = Vec::new();
    for (name, state) in app.registry.list() {
        let loaded = match &state {
            SlotState::Ready(h) => {
                let h = h.clone();
                tokio::task::spawn_blocking(move || {
                    let g = match h.try_read() {
                        Ok(g) => g,
                        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
                        Err(std::sync::TryLockError::WouldBlock) => return None,
                    };
                    g.as_ref().map(|g| metrics::LoadedMetrics {
                        counts: g.counts(),
                        last_seq: g.last_seq(),
                        text: match g.text_index_status() {
                            "none" => None,
                            status => Some((
                                g.text_handle().map_or(0, |t| t.pending()),
                                status == "stale",
                            )),
                        },
                        semantic_pending: g.semantic_handle().map(|t| t.pending()),
                    })
                })
                .await
                .ok()
                .flatten()
            }
            _ => None,
        };
        graphs.push(metrics::GraphMetrics {
            file_bytes: mygraph_store_sqlite::GraphFile::file_sizes(app.registry.data_dir(), &name),
            run: app.registry.run(&name),
            state: state.name(),
            name,
            loaded,
        });
    }
    let process = metrics::ProcessMetrics {
        version: env!("CARGO_PKG_VERSION"),
        spec: mygraph_core::SPEC_VERSION,
        storage_format: mygraph_core::STORAGE_FORMAT_VERSION,
        uptime_s: app.started.elapsed().as_secs_f64(),
        resident_bytes: resident_bytes(),
    };
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4"),
        )],
        app.metrics.prometheus(&process, &graphs),
    )
        .into_response()
}

/// A static page of the server's own: no data in it, nothing from other origins.
fn page(html: &'static str) -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(
                    "default-src 'none'; script-src 'self' 'unsafe-inline'; \
                     style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; \
                     base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
                ),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        html,
    )
        .into_response()
}

/// [ADMIN-UI-1] The stats page; its script reads the admin API.
async fn stats_page() -> Response {
    page(include_str!("../ui/stats.html"))
}

/// [ADMIN-UI-2] The graph viewer; its script reads the agent API.
async fn graph_page() -> Response {
    page(include_str!("../ui/graph.html"))
}

/// The viewer's drawing library, vendored (ui/vendor/README.md).
async fn cytoscape_js() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/javascript; charset=utf-8"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            ),
        ],
        include_str!("../ui/vendor/cytoscape-3.30.4.min.js"),
    )
        .into_response()
}

async fn openapi() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/yaml"),
        )],
        include_str!("../../../spec/api/openapi.yaml"),
    )
        .into_response()
}

/// Serves until Ctrl-C.
pub async fn serve(app: Arc<App>) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(app.config.listen).await?;
    tracing::info!(
        listen = %listener.local_addr()?,
        data_dir = %app.config.data_dir.display(),
        keys = app.config.keys.len(),
        "mygraph serving"
    );
    if app.config.keys.is_empty() {
        tracing::warn!("no API keys configured: every local request is the admin `local`");
    }
    if app.config.preload {
        app.preload();
    }
    spawn_maintenance(&app);
    let registry = app.registry.clone();
    let result = axum::serve(listener, router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    tokio::task::spawn_blocking(move || {
        registry.maintain();
        registry.flush_semantic();
    })
    .await
    .ok();
    result
}

/// [MCP-HTTP-1] Streamable HTTP: one JSON-RPC message per POST, answered as JSON.
async fn mcp_http(State(app): State<Arc<App>>, caller: auth::Caller, body: api::Body) -> Response {
    let start = Instant::now();
    let msg = match api::parse_body(body) {
        Ok(m) => m,
        Err(_) => json!(null),
    };
    let backend = mcp::Backend::Local {
        app: app.clone(),
        caller,
    };
    let reply = if msg.is_null() {
        Some(
            json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } }),
        )
    } else {
        mcp::handle(&backend, msg).await
    };
    let resp = match reply {
        Some(r) => Json(r).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    };
    api::observe(&app, "mcp", None, resp.status().as_u16(), start, None);
    resp
}

/// Background maintenance (full-text commits, WAL checkpoints) every `maintenance_ms`.
pub fn spawn_maintenance(app: &App) {
    let registry = app.registry.clone();
    let every = Duration::from_millis(app.config.maintenance_ms.max(10));
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            let r = registry.clone();
            let _ = tokio::task::spawn_blocking(move || r.maintain()).await;
        }
    });
}

/// [STORE-DIR-4] Locks the data folder for this process; keep the file open while running.
pub fn lock_data_dir(dir: &std::path::Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join("mygraph.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(format!(
            "{} is in use by another MyGraph process (lock {})",
            dir.display(),
            path.display()
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(format!("cannot lock {}: {e}", path.display())),
    }
}
