//! MCP: the agent operations as tools — spec/api/mcp.md. The protocol part is shared by the
//! Streamable HTTP endpoint (`POST /mcp`) and the stdio command (`mygraph mcp`).

use crate::App;
use crate::auth::Caller;
use crate::error::{ApiError, hint_of};
use serde_json::{Map, Value as Json, json};
use std::sync::{Arc, OnceLock};

const TOOLS: &str = include_str!("../../../spec/api/mcp-tools.json");
const INSTRUCTIONS: &str = include_str!("../../../spec/api/server-instructions.md");
/// [MCP-2] Newest first.
const VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

/// Where tool calls go.
pub enum Backend {
    /// In this process, as `caller` (the HTTP endpoint, or stdio with `--data-dir`).
    Local { app: Arc<App>, caller: Caller },
    /// A running server's REST API (stdio with `--url`).
    Remote {
        url: String,
        key: Option<String>,
        graph: Option<String>,
    },
}

/// [MCP-TOOL-1] Tool definitions with the common arguments added.
fn tools() -> &'static Json {
    static T: OnceLock<Json> = OnceLock::new();
    T.get_or_init(|| {
        let spec: Json = serde_json::from_str(TOOLS).expect("mcp-tools.json is valid");
        let common = spec["common"].as_object().cloned().unwrap_or_default();
        let tools: Vec<Json> = spec["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| {
                let mut t = t.clone();
                let props = t["inputSchema"]["properties"]
                    .as_object_mut()
                    .expect("properties");
                props.extend(common.clone());
                t
            })
            .collect();
        Json::Array(tools)
    })
}

fn rpc_error(id: &Json, code: i64, message: &str) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn rpc_result(id: &Json, result: Json) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Handles one JSON-RPC message; `None` for notifications and responses ([MCP-1]).
pub async fn handle(backend: &Backend, msg: Json) -> Option<Json> {
    let Some(m) = msg.as_object() else {
        return Some(rpc_error(
            &Json::Null,
            -32600,
            "expected one JSON-RPC message",
        ));
    };
    let id = m.get("id").cloned();
    let Some(method) = m.get("method").and_then(Json::as_str) else {
        // A response from the client, or garbage without an ID.
        return id.map(|id| rpc_error(&id, -32600, "missing method"));
    };
    let id = id?; // notifications get no answer
    let params = m.get("params").cloned().unwrap_or(json!({}));
    Some(match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let version = VERSIONS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&VERSIONS[0]);
            rpc_result(
                &id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "mygraph", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => rpc_result(&id, json!({})),
        "tools/list" => rpc_result(&id, json!({ "tools": tools() })),
        "tools/call" => match call(backend, &params).await {
            Ok(result) => rpc_result(&id, result),
            Err((code, message)) => rpc_error(&id, code, &message),
        },
        _ => rpc_error(&id, -32601, &format!("method not found: {method}")),
    })
}

/// [MCP-TOOL-2] [MCP-TOOL-3]
async fn call(backend: &Backend, params: &Json) -> Result<Json, (i64, String)> {
    let name = params["name"].as_str().unwrap_or("");
    let op = crate::api::operation(name).ok_or((-32602, format!("unknown tool: {name}")))?;
    let mut args: Map<String, Json> = match params.get("arguments") {
        None | Some(Json::Null) => Map::new(),
        Some(Json::Object(m)) => m.clone(),
        Some(_) => return Err((-32602, "arguments must be an object".into())),
    };
    let graph = args.remove("graph");
    let json_format = match args.remove("format") {
        None => false,
        Some(f) if f == "compact" => false,
        Some(f) if f == "json" => true,
        Some(_) => {
            return Ok(tool_error(&ApiError::new(
                "mcp.invalid_format",
                "format: expected compact or json",
            )));
        }
    };
    let graph = match graph {
        Some(Json::String(g)) => Some(g),
        None => None,
        Some(_) => {
            return Ok(tool_error(&ApiError::new(
                "mcp.invalid_graph",
                "graph: expected a string",
            )));
        }
    };
    let result = match backend {
        Backend::Local { app, caller } => {
            let graph = graph.unwrap_or_else(|| match &caller.graphs {
                Some(gs) if gs.len() == 1 => gs[0].clone(),
                _ => app.config.default_graph.clone(),
            });
            crate::api::run(app, caller, &graph, op, Json::Object(args)).await
        }
        Backend::Remote {
            url,
            key,
            graph: default,
        } => {
            let (url, key, default) = (url.clone(), key.clone(), default.clone());
            tokio::task::spawn_blocking(move || {
                let graph = match graph.or(default) {
                    Some(g) => g,
                    None => only_graph(&url, key.as_deref())?,
                };
                remote(&url, key.as_deref(), &graph, op, &Json::Object(args))
            })
            .await
            .map_err(|e| (-32603, e.to_string()))?
        }
    };
    Ok(match result {
        Ok(resp) => {
            let text = if json_format {
                resp.to_string()
            } else {
                crate::compact::render(op, &resp)
            };
            json!({ "content": [{ "type": "text", "text": text }] })
        }
        Err(e) => tool_error(&e),
    })
}

fn tool_error(e: &ApiError) -> Json {
    let mut text = format!("error {}: {}", e.code, e.message);
    if let Some(h) = hint_of(&e.code) {
        text.push_str("\nhint: ");
        text.push_str(h);
    }
    if !e.details.is_null() {
        text.push_str("\ndetails: ");
        text.push_str(&e.details.to_string());
    }
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

// ---------------------------------------------------------------------------------------------
// Forwarding to a running server ([MCP-STDIO-1])

fn request(url: &str, key: Option<&str>, method: &str) -> ureq::Request {
    let r = ureq::request(method, url).timeout(std::time::Duration::from_secs(120));
    match key {
        Some(k) => r.set("Authorization", &format!("Bearer {k}")),
        None => r,
    }
}

fn answer(r: Result<ureq::Response, ureq::Error>) -> Result<Json, ApiError> {
    let parse = |resp: ureq::Response| -> Result<Json, ApiError> {
        let text = resp
            .into_string()
            .map_err(|e| ApiError::new("mcp.server_unreachable", e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| ApiError::new("internal", e.to_string()))
    };
    match r {
        Ok(resp) => parse(resp),
        Err(ureq::Error::Status(_, resp)) => {
            let body = parse(resp)?;
            let e = &body["error"];
            let mut err = ApiError::new(
                e["code"].as_str().unwrap_or("internal"),
                e["message"].as_str().unwrap_or(""),
            );
            err.details = e.get("details").cloned().unwrap_or(Json::Null);
            Err(err)
        }
        Err(e) => Err(ApiError::new(
            "mcp.server_unreachable",
            format!("cannot reach the MyGraph server: {e}"),
        )),
    }
}

fn remote(
    url: &str,
    key: Option<&str>,
    graph: &str,
    op: &str,
    req: &Json,
) -> Result<Json, ApiError> {
    let u = format!("{}/v1/graphs/{graph}/{op}", url.trim_end_matches('/'));
    answer(
        request(&u, key, "POST")
            .set("Content-Type", "application/json")
            .send_string(&req.to_string()),
    )
}

/// The default graph of a forwarded call: the only graph the key may use.
fn only_graph(url: &str, key: Option<&str>) -> Result<String, ApiError> {
    let list = answer(
        request(
            &format!("{}/v1/graphs", url.trim_end_matches('/')),
            key,
            "GET",
        )
        .call(),
    )?;
    match list["graphs"].as_array().map(Vec::as_slice) {
        Some([one]) => Ok(one["name"].as_str().unwrap_or("default").to_owned()),
        _ => Err(ApiError::new(
            "mcp.graph_required",
            "several graphs are available: pass graph (or start mygraph mcp with --graph)",
        )),
    }
}
