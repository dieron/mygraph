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

/// [MCP-TOOL-1] Tool definitions with the common arguments added; [MCP-TOOL-6] without
/// semantic search, each tool's `without_semantic` variant.
fn tools(semantic: bool) -> &'static Json {
    static WITH: OnceLock<Json> = OnceLock::new();
    static WITHOUT: OnceLock<Json> = OnceLock::new();
    (if semantic { &WITH } else { &WITHOUT }).get_or_init(|| {
        let spec: Json = serde_json::from_str(TOOLS).expect("mcp-tools.json is valid");
        let common = spec["common"].as_object().cloned().unwrap_or_default();
        let tools: Vec<Json> = spec["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|t| {
                let mut t = t.clone();
                let variant = t.as_object_mut().and_then(|o| o.remove("without_semantic"));
                if let (false, Some(v)) = (semantic, variant) {
                    if let Some(d) = v.get("description") {
                        t["description"] = d.clone();
                    }
                    for (k, p) in v["properties"].as_object().into_iter().flatten() {
                        t["inputSchema"]["properties"][k] = p.clone();
                    }
                }
                let graphs_tool = t["name"] == "graphs";
                let props = t["inputSchema"]["properties"]
                    .as_object_mut()
                    .expect("properties");
                // `graphs` works on the server, not on one graph.
                props.extend(
                    common
                        .iter()
                        .filter(|(k, _)| !(graphs_tool && *k == "graph"))
                        .map(|(k, v)| (k.clone(), v.clone())),
                );
                t
            })
            .collect();
        Json::Array(tools)
    })
}

/// [MCP-TOOL-6] The server instructions, without the mention of meaning when there is no
/// semantic search.
fn instructions(semantic: bool) -> String {
    if semantic {
        INSTRUCTIONS.to_owned()
    } else {
        INSTRUCTIONS.replace(
            "alias, text or meaning",
            "alias or text (words, not meaning)",
        )
    }
}

/// Whether the server behind `backend` searches by meaning: in this process, whether the
/// embedding model is loaded; for a remote server, what `GET /v1/graphs` says (asked once; when
/// it cannot be asked, the full tool set is listed).
async fn semantic(backend: &Backend) -> bool {
    static REMOTE: OnceLock<bool> = OnceLock::new();
    match backend {
        Backend::Local { app, .. } => app.registry.semantic_threshold().is_some(),
        Backend::Remote { url, key, .. } => {
            if let Some(s) = REMOTE.get() {
                return *s;
            }
            let (url, key) = (url.clone(), key.clone());
            let asked = tokio::task::spawn_blocking(move || {
                let u = format!("{}/v1/graphs", url.trim_end_matches('/'));
                answer(request(&u, key.as_deref(), "GET").call())
                    .ok()
                    .and_then(|r| r["search"]["semantic"].as_bool())
            })
            .await
            .ok()
            .flatten();
            match asked {
                Some(s) => *REMOTE.get_or_init(|| s),
                None => true,
            }
        }
    }
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
                    "instructions": instructions(semantic(backend).await),
                }),
            )
        }
        "ping" => rpc_result(&id, json!({})),
        "tools/list" => rpc_result(&id, json!({ "tools": tools(semantic(backend).await) })),
        "tools/call" => match call(backend, &params).await {
            Ok(result) => rpc_result(&id, result),
            Err((code, message)) => rpc_error(&id, code, &message),
        },
        _ => rpc_error(&id, -32601, &format!("method not found: {method}")),
    })
}

/// [MCP-TOOL-2] [MCP-TOOL-3]
/// [MCP-TOOL-5] Top-level arguments that are `null`, `""`, `[]` or `{}` count as absent.
fn given(args: &mut Map<String, Json>) {
    args.retain(|_, v| match v {
        Json::Null => false,
        Json::String(s) => !s.is_empty(),
        Json::Array(a) => !a.is_empty(),
        Json::Object(o) => !o.is_empty(),
        _ => true,
    });
}

async fn call(backend: &Backend, params: &Json) -> Result<Json, (i64, String)> {
    let name = params["name"].as_str().unwrap_or("");
    let mut args: Map<String, Json> = match params.get("arguments") {
        None | Some(Json::Null) => Map::new(),
        Some(Json::Object(m)) => m.clone(),
        Some(_) => return Err((-32602, "arguments must be an object".into())),
    };
    given(&mut args);
    if name == "graphs" {
        return Ok(graphs_tool(backend, args).await);
    }
    let op = crate::api::operation(name).ok_or((-32602, format!("unknown tool: {name}")))?;
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
    let shown = Json::Object(args.clone());
    let result = match backend {
        Backend::Local { app, caller } => {
            let graph = graph.unwrap_or_else(|| match &caller.graphs {
                Some(gs) if gs.len() == 1 => gs[0].clone(),
                _ => app.config.default_graph.clone(),
            });
            let start = std::time::Instant::now();
            let r = crate::api::run(app, caller, &graph, op, Json::Object(args)).await;
            let (status, code) = match &r {
                Ok(_) => (200, None),
                Err(e) => (e.status().as_u16(), Some(e.code.as_str())),
            };
            let label = format!("mcp.{op}");
            crate::api::observe(app, &label, Some(&graph), status, start, code);
            r
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
                crate::compact::render(op, &shown, &resp)
            };
            json!({ "content": [{ "type": "text", "text": text }] })
        }
        Err(e) => tool_error(&e),
    })
}

fn tool_error(e: &ApiError) -> Json {
    let mut text = format!("error {}: {}", e.code, e.message);
    if let Some(h) = mcp_hint(&e.code).or_else(|| hint_of(&e.code)) {
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
    let r = crate::tls::agent()
        .request(method, url)
        .timeout(std::time::Duration::from_secs(120));
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
            format!(
                "cannot reach the MyGraph server: {}",
                crate::tls::explain(&e.to_string())
            ),
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

/// [MCP-TOOL-3] Hints that name tools, not REST routes.
fn mcp_hint(code: &str) -> Option<&'static str> {
    Some(match code {
        "graph.not_found" => {
            "Call the graphs tool to list the graphs, or graphs with action \"create\" to make one."
        }
        "auth.missing_key" | "auth.invalid_key" => {
            "The MCP server needs a valid API key (mygraph mcp --key, or MYGRAPH_KEY)."
        }
        "auth.forbidden" => {
            "This key's role does not allow it; creating graphs needs an admin key."
        }
        "graph.warming_up" => "The graph is loading; call the tool again in a few seconds.",
        _ => return None,
    })
}

/// [MCP-TOOL-4] The `graphs` tool: list the graphs, or create one.
async fn graphs_tool(backend: &Backend, mut args: Map<String, Json>) -> Json {
    let json_format = args.remove("format").is_some_and(|f| f == "json");
    let action = match args.remove("action") {
        None => "list".to_owned(),
        Some(Json::String(a)) => a,
        Some(_) => String::new(),
    };
    let result: Result<Json, ApiError> = match (action.as_str(), backend) {
        ("list", Backend::Local { app, caller }) => (|| {
            caller.require(crate::config::Role::AgentRead)?;
            let graphs: Vec<Json> = app
                .registry
                .list()
                .into_iter()
                .filter(|(n, _)| caller.may_use(n))
                .map(|(n, s)| json!({ "name": n, "status": s.name() }))
                .collect();
            let default = match &caller.graphs {
                Some(gs) if gs.len() == 1 => gs[0].clone(),
                _ => app.config.default_graph.clone(),
            };
            let mut r = with_default(graphs, Some(default));
            r["search"] = app.search_info();
            Ok(r)
        })(),
        ("create", Backend::Local { app, caller }) => {
            crate::admin::create(app, caller, &Json::Object(args))
                .await
                .map(|info| json!({ "created": info }))
        }
        (action @ ("list" | "create"), Backend::Remote { url, key, graph }) => {
            let (url, key, graph) = (url.clone(), key.clone(), graph.clone());
            let action = action.to_owned();
            tokio::task::spawn_blocking(move || {
                let base = url.trim_end_matches('/');
                if action == "create" {
                    let r = request(&format!("{base}/admin/graphs"), key.as_deref(), "POST")
                        .set("Content-Type", "application/json")
                        .send_string(&Json::Object(args).to_string());
                    answer(r).map(|info| json!({ "created": info }))
                } else {
                    let list = answer(
                        request(&format!("{base}/v1/graphs"), key.as_deref(), "GET").call(),
                    )?;
                    let graphs = list["graphs"].as_array().cloned().unwrap_or_default();
                    let default = graph.or_else(|| match graphs.as_slice() {
                        [one] => one["name"].as_str().map(str::to_owned),
                        _ => None,
                    });
                    let mut r = with_default(graphs, default);
                    if let Some(s) = list.get("search") {
                        r["search"] = s.clone();
                    }
                    Ok(r)
                }
            })
            .await
            .unwrap_or_else(|e| Err(ApiError::new("internal", e.to_string())))
        }
        _ => Err(ApiError::new(
            "mcp.invalid_action",
            "action: expected \"list\" or \"create\"",
        )),
    };
    match result {
        Err(e) => tool_error(&e),
        Ok(r) => {
            let text = if json_format {
                r.to_string()
            } else {
                graphs_text(&r)
            };
            json!({ "content": [{ "type": "text", "text": text }] })
        }
    }
}

/// The list with `default`: the graph tools use when a call names none (only if it exists).
fn with_default(graphs: Vec<Json>, default: Option<String>) -> Json {
    let default = default.filter(|d| graphs.iter().any(|g| g["name"] == d.as_str()));
    json!({ "graphs": graphs, "default": default })
}

fn graphs_text(r: &Json) -> String {
    if let Some(info) = r.get("created") {
        return format!(
            "created graph {} · policies {}\n",
            info["name"].as_str().unwrap_or("?"),
            info["policies"]
        );
    }
    let default = r["default"].as_str();
    let list: Vec<String> = r["graphs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| {
            let name = g["name"].as_str().unwrap_or("?");
            let mark = if Some(name) == default {
                ", default"
            } else {
                ""
            };
            format!("{name} ({}{mark})", g["status"].as_str().unwrap_or("?"))
        })
        .collect();
    let mut out = if list.is_empty() {
        "no graphs yet: create one with action \"create\"\n".to_owned()
    } else {
        format!("graphs: {}\n", list.join(" · "))
    };
    if default.is_none() && !list.is_empty() {
        out.push_str("no default graph: pass `graph` to the other tools\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{INSTRUCTIONS, instructions, tools};
    use serde_json::json;

    fn tool<'a>(semantic: bool, name: &str) -> &'a serde_json::Value {
        tools(semantic)
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()
    }

    /// [MCP-TOOL-6]
    #[test]
    fn tools_and_instructions_follow_semantic_search() {
        let modes = |s| tool(s, "search")["inputSchema"]["properties"]["mode"]["enum"].clone();
        assert_eq!(
            modes(true),
            json!(["auto", "exact", "prefix", "fuzzy", "text", "semantic"])
        );
        assert_eq!(
            modes(false),
            json!(["auto", "exact", "prefix", "fuzzy", "text"])
        );
        for s in [true, false] {
            assert!(tool(s, "search").get("without_semantic").is_none());
            assert!(tool(s, "search")["inputSchema"]["properties"]["graph"].is_object());
        }
        let similar = |s| {
            tool(s, "resolve")["description"]
                .as_str()
                .unwrap()
                .contains("similar")
        };
        assert!(similar(true) && !similar(false));
        assert_eq!(instructions(true), INSTRUCTIONS);
        assert!(INSTRUCTIONS.contains("alias, text or meaning"));
        assert!(instructions(false).contains("alias or text (words, not meaning)"));
    }
}
