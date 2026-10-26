//! MCP over Streamable HTTP and stdio, and the compact format (spec/api/mcp.md,
//! spec/api/compact-format.md).

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use mygraph_server::App;
use mygraph_server::config::{Config, KeyConfig, Role};
use serde_json::{Value as Json, json};
use std::io::{BufRead, BufReader, Write};
use tower::ServiceExt;

fn app(dir: &std::path::Path) -> axum::Router {
    let config = Config {
        data_dir: dir.to_owned(),
        preload: false,
        default_graph: "research".into(),
        keys: vec![KeyConfig {
            key: Some("k".into()),
            key_sha256: None,
            actor: "claude".into(),
            role: Role::Admin,
            graphs: vec![],
        }],
        ..Config::default()
    };
    mygraph_server::router(App::new(config))
}

fn new_graph(dir: &std::path::Path, name: &str) {
    mygraph_store_sqlite::GraphFile::create(dir, name, &Default::default(), 1).unwrap();
}

async fn rpc(router: &axum::Router, msg: Json) -> (StatusCode, Json) {
    let req = Request::post("/mcp")
        .header("authorization", "Bearer k")
        .header("content-type", "application/json")
        .body(Body::from(msg.to_string()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = if bytes.is_empty() {
        Json::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, json)
}

async fn tool(router: &axum::Router, name: &str, args: Json) -> (bool, String) {
    let (_, r) = rpc(
        router,
        json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": name, "arguments": args}}),
    )
    .await;
    let res = &r["result"];
    (
        res["isError"] == true,
        res["content"][0]["text"].as_str().unwrap_or("").to_owned(),
    )
}

const BATCH: &str = r#"{
  "evidence": [{ "ref": "@e", "kind": "text", "content": "Ada Lovelace wrote notes on the Analytical Engine." }],
  "nodes": [{ "ref": "@ada", "type": "Person", "name": "Ada Lovelace", "props": { "born": {"$date": "1815-12-10"} }, "cite": ["@e"] },
            { "ref": "@ae", "type": "Machine", "name": "Analytical Engine", "cite": ["@e"] }],
  "links": [{ "type": "WROTE_ABOUT", "from": "@ada", "to": "@ae", "cite": ["@e"] }]
}"#;

#[tokio::test]
async fn mcp_over_http() {
    let dir = tempfile::tempdir().unwrap();
    new_graph(dir.path(), "research");
    let r = app(dir.path());

    // [MCP-2]
    let (s, init) = rpc(&r, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("resolve")
    );
    let (s, _) = rpc(
        &r,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);

    // [MCP-TOOL-1]
    let (_, list) = rpc(
        &r,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 13);
    assert!(
        tools
            .iter()
            .filter(|t| t["name"] != "graphs")
            .all(|t| t["inputSchema"]["properties"]["graph"].is_object())
    );

    // [MCP-TOOL-4]
    let (_, text) = tool(&r, "graphs", json!({})).await;
    assert_eq!(
        text,
        "graphs: research (unloaded, default)
"
    );
    let (err, text) = tool(&r, "graphs", json!({"action": "create", "name": "lab"})).await;
    assert!(!err && text.starts_with("created graph lab"), "{text}");
    let (_, text) = tool(&r, "graphs", json!({"format": "json"})).await;
    let list: Json = serde_json::from_str(&text).unwrap();
    assert_eq!(list["graphs"][0]["name"], "lab");
    assert_eq!(list["default"], "research");

    // [MCP-TOOL-2] the default graph; [MCP-TOOL-3] compact text.
    let batch: Json = serde_json::from_str(BATCH).unwrap();
    let (err, text) = tool(&r, "write", batch).await;
    assert!(!err, "{text}");
    assert!(text.starts_with("batch b_1 · created "), "{text}");
    assert!(text.contains("refs: @ada=n_1"), "{text}");
    let (_, text) = tool(&r, "get", json!({"refs": ["n_1"]})).await;
    assert!(
        text.starts_with(
            "n_1 Person \"Ada Lovelace\" · born=1815-12-10 · sources 1 (supports 1)\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("  -> WROTE_ABOUT n_2 Machine \"Analytical Engine\" (l_1)"),
        "{text}"
    );
    let (_, text) = tool(&r, "paths", json!({"from": "n_2", "to": "n_1"})).await;
    assert!(
        text.starts_with("1: n_2 \"Analytical Engine\" <-[WROTE_ABOUT]- n_1 \"Ada Lovelace\""),
        "{text}"
    );
    let (_, text) = tool(&r, "overview", json!({})).await;
    assert!(text.contains("node types: Machine 1 · Person 1"), "{text}");
    let (_, text) = tool(&r, "stats", json!({"sections": ["counts", "degree"]})).await;
    assert!(
        text.contains(
            "# counts
active: nodes 2 · links 1"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "# degree
nodes 2 · min 1 · max 1"
        ),
        "{text}"
    );
    assert!(!text.contains("# growth"), "{text}");
    let (_, text) = tool(&r, "search", json!({"query": "lovelace", "format": "json"})).await;
    assert_eq!(
        serde_json::from_str::<Json>(&text).unwrap()["hits"][0]["id"],
        "n_1"
    );

    // Errors are tool results with a hint, not protocol errors.
    let (err, text) = tool(&r, "get", json!({"refs": ["n_1"], "graph": "nope"})).await;
    assert!(err);
    assert!(text.starts_with("error graph.not_found:"), "{text}");
    assert!(text.contains("\nhint: Call the graphs tool"), "{text}");
    let (err, text) = tool(
        &r,
        "write",
        json!({"nodes": [{"type": "Person", "name": "Nobody"}]}),
    )
    .await;
    assert!(
        err && text.contains("write.evidence_required") && text.contains("observation"),
        "{text}"
    );
    let (_, unknown) = rpc(
        &r,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "drop_all"}}),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32602);
    let (_, unknown) = rpc(
        &r,
        json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32601);

    // REST answers in the compact format on request.
    let req = Request::post("/v1/graphs/research/search")
        .header("authorization", "Bearer k")
        .header("accept", "text/plain")
        .body(Body::from(r#"{"query": "ada lovelace"}"#))
        .unwrap();
    let resp = r.clone().oneshot(req).await.unwrap();
    let text = String::from_utf8(
        to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert_eq!(text, "1.00 n_1 Person \"Ada Lovelace\" (name:exact)\n");
}

#[test]
fn mcp_over_stdio_with_a_data_folder() {
    let dir = tempfile::tempdir().unwrap();
    new_graph(dir.path(), "solo");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mygraph"))
        .args([
            "mcp",
            "--graph",
            "solo",
            "--actor",
            "stdio-agent",
            "--data-dir",
        ])
        .arg(dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut send = |msg: Json| -> Json {
        writeln!(stdin, "{msg}").unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "1999-01-01"}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    let batch: Json = serde_json::from_str(BATCH).unwrap();
    let w = send(
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "write", "arguments": batch}}),
    );
    assert!(
        w["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("batch b_1"),
        "{w}"
    );
    let o = send(
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "overview", "arguments": {}}}),
    );
    let text = o["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("stdio-agent write"), "{text}");
    drop(send);
    child.kill().ok();
    child.wait().ok();
}
