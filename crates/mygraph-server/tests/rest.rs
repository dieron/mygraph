//! The REST and admin API end to end, through the router (spec/api/conventions.md, errors.md,
//! admin.md).

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use mygraph_server::App;
use mygraph_server::config::{Config, KeyConfig, Role};
use serde_json::{Value as Json, json};
use std::sync::Arc;
use tower::ServiceExt;

const WRITER: &str = "writer-key";
const READER: &str = "reader-key";
const ADMIN: &str = "admin-key";
const OTHER: &str = "other-key";

struct Server {
    app: Arc<App>,
    router: Router,
    dir: tempfile::TempDir,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl Reply {
    fn json(&self) -> Json {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }

    fn code(&self) -> String {
        self.json()["error"]["code"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

fn key(key: &str, actor: &str, role: Role, graphs: &[&str]) -> KeyConfig {
    KeyConfig {
        key: Some(key.to_owned()),
        key_sha256: None,
        actor: actor.to_owned(),
        role,
        graphs: graphs.iter().map(|g| g.to_string()).collect(),
    }
}

fn server_with(edit: impl FnOnce(&mut Config)) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config {
        data_dir: dir.path().to_owned(),
        preload: false,
        keys: vec![
            key(WRITER, "agent-a", Role::AgentWrite, &[]),
            key(READER, "agent-r", Role::AgentRead, &[]),
            key(ADMIN, "ops", Role::Admin, &[]),
            key(OTHER, "agent-o", Role::AgentWrite, &["other"]),
        ],
        ..Config::default()
    };
    edit(&mut config);
    config.validate().unwrap();
    let app = App::new(config);
    Server {
        router: mygraph_server::router(app.clone()),
        app,
        dir,
    }
}

fn server() -> Server {
    server_with(|_| {})
}

impl Server {
    async fn call(
        &self,
        method: Method,
        uri: &str,
        key: Option<&str>,
        body: Option<Json>,
    ) -> Reply {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(k) = key {
            req = req.header("authorization", format!("Bearer {k}"));
        }
        let body = match body {
            Some(b) => Body::from(serde_json::to_vec(&b).unwrap()),
            None => Body::empty(),
        };
        let resp = self
            .router
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        Reply {
            status,
            headers,
            body,
        }
    }

    async fn get(&self, uri: &str, key: &str) -> Reply {
        self.call(Method::GET, uri, Some(key), None).await
    }

    async fn post(&self, uri: &str, key: &str, body: Json) -> Reply {
        self.call(Method::POST, uri, Some(key), Some(body)).await
    }

    async fn create_graph(&self, name: &str) {
        let r = self
            .post("/admin/graphs", ADMIN, json!({ "name": name }))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    }
}

fn ada_batch() -> Json {
    json!({
        "evidence": [
            { "ref": "@art", "kind": "url", "uri": "https://example.org/ada", "title": "Ada" },
            { "ref": "@frag", "kind": "text", "parent": "@art",
              "content": "In 1843 Lovelace published her notes on Babbage's Analytical Engine." }
        ],
        "nodes": [
            { "ref": "@ada", "type": "Person", "name": "Ada Lovelace", "aliases": ["Augusta Ada King"],
              "props": { "born": {"$date": "1815-12-10"} }, "cite": ["@frag"] },
            { "ref": "@bab", "type": "Person", "name": "Charles Babbage", "cite": ["@frag"] },
            { "ref": "@ae", "type": "Machine", "name": "Analytical Engine", "cite": ["@frag"] }
        ],
        "links": [
            { "type": "COLLABORATED_WITH", "from": "@ada", "to": "@bab", "directed": false, "cite": ["@frag"] },
            { "type": "WROTE_ABOUT", "from": "@ada", "to": "@ae", "props": { "year": 1843 }, "cite": ["@frag"] }
        ]
    })
}

#[tokio::test]
async fn service_routes_and_unknown_routes() {
    let s = server();
    let r = s.call(Method::GET, "/health", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["status"], "ok");

    let r = s.call(Method::GET, "/openapi.yaml", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.text().starts_with("openapi: 3.1.0"));

    // [ERR-1] Unknown routes and methods answer the error object too.
    let r = s.get("/v2/nothing", WRITER).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "api.not_found")
    );
    assert!(r.json()["error"]["hint"].is_string());
    let r = s.call(Method::PUT, "/v1/graphs", Some(WRITER), None).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(r.code(), "api.method_not_allowed");

    // No graphs found at start: ready.
    let r = s.call(Method::GET, "/ready", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn keys_and_roles_are_enforced() {
    let s = server();
    s.create_graph("research").await;
    s.create_graph("other").await;

    // [API-AUTH-2]
    let r = s.call(Method::GET, "/v1/graphs", None, None).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNAUTHORIZED, "auth.missing_key")
    );
    assert_eq!(r.headers["www-authenticate"], "Bearer");
    let r = s.get("/v1/graphs", "nope").await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNAUTHORIZED, "auth.invalid_key")
    );

    let r = s
        .post("/v1/graphs/research/write", READER, ada_batch())
        .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "auth.forbidden")
    );
    let r = s.get("/admin/graphs", WRITER).await;
    assert_eq!(r.code(), "auth.forbidden");

    // A key limited to some graphs sees and uses only those.
    let r = s.get("/v1/graphs", OTHER).await;
    assert_eq!(
        r.json()["graphs"],
        json!([{ "name": "other", "status": "ready" }])
    );
    let r = s.get("/v1/graphs/research", OTHER).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "auth.graph_forbidden")
    );

    // [API-AUTH-3] Changes are recorded under the key's actor.
    let r = s
        .post("/v1/graphs/research/write", WRITER, ada_batch())
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = s.get("/v1/graphs/research", READER).await;
    assert_eq!(r.json()["recent"][0]["actor"], "agent-a");
}

#[tokio::test]
async fn agent_workflow_over_rest() {
    let s = server();
    s.create_graph("research").await;
    let g = "/v1/graphs/research";

    // Empty graph: overview works.
    let r = s.get(g, READER).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["counts"]["nodes"], 0);
    assert_eq!(r.json()["last_seq"], 0);

    // write, with a matching graph member in the body ([API-OP-1]).
    let mut batch = ada_batch();
    batch["graph"] = json!("research");
    let r = s.post(&format!("{g}/write"), WRITER, batch).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let w = r.json();
    assert_eq!(w["refs"]["@ada"], "n_1");
    let frag = w["refs"]["@frag"].as_str().unwrap().to_owned();

    // overview ([OP-OVER-2]) and one type ([OP-OVER-5]).
    let o = s.get(g, READER).await.json();
    assert_eq!(
        o["counts"],
        json!({ "nodes": 3, "links": 2, "evidence": 2, "citations": 5 })
    );
    assert_eq!(o["node_types"][0], json!({ "type": "Person", "count": 2 }));
    assert_eq!(o["evidence_kinds"], json!({ "text": 1, "url": 1 }));
    assert_eq!(o["recent"][0]["changes"], 12);
    let o = s.get(&format!("{g}?type=person"), READER).await.json();
    let d = &o["type_detail"];
    assert_eq!(
        (d["kind"].as_str(), d["count"].as_u64()),
        (Some("node"), Some(2))
    );
    assert_eq!(
        d["props"],
        json!([{ "key": "born", "kinds": ["date"], "fill": 0.5 }])
    );
    assert_eq!(d["links"][0]["type"], "COLLABORATED_WITH");
    let o = s.get(&format!("{g}?type=wrote_about"), READER).await.json();
    assert_eq!(
        o["type_detail"]["endpoints"],
        json!([{ "from": "Person", "to": "Machine", "count": 1 }])
    );
    let r = s.get(&format!("{g}?type=Planet"), READER).await;
    assert_eq!(r.code(), "ref.not_found");

    // search, as GET and POST.
    let r = s
        .get(&format!("{g}/search?q=lovelace&in=nodes,evidence"), READER)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["hits"][0]["id"], "n_1");
    let r = s
        .post(
            &format!("{g}/search"),
            READER,
            json!({ "query": "babage", "mode": "fuzzy" }),
        )
        .await;
    assert_eq!(r.json()["hits"][0]["id"], "n_2");
    // Full text sees the write at once, before the index commits ([API-GRAPH-3]).
    let r = s
        .get(&format!("{g}/search?q=notes&in=evidence&mode=text"), READER)
        .await;
    assert_eq!(r.json()["hits"][0]["id"], frag.as_str());

    // One object, unwrapped ([API-SHORT-1]).
    let n = s
        .get(&format!("{g}/nodes/n_1?include=links,support"), READER)
        .await
        .json();
    assert_eq!(n["name"], "Ada Lovelace");
    assert_eq!(n["support"]["sources"], 1);
    assert_eq!(n["links"]["items"].as_array().unwrap().len(), 2);
    assert!(n.get("evidence").is_none());
    let n = s
        .get(
            &format!("{g}/nodes/n_1?links_limit=1&link_types=wrote_about&detail=full"),
            READER,
        )
        .await
        .json();
    assert_eq!(n["links"]["items"][0]["type"], "WROTE_ABOUT");
    assert_eq!(n["created_by"], "agent-a");
    let r = s.get(&format!("{g}/nodes/n_99"), READER).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "ref.not_found")
    );
    // [API-SHORT-2] An ID of another kind is not found in this collection.
    let r = s.get(&format!("{g}/links/n_1"), READER).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = s.get(&format!("{g}/nodes/n_1?colour=red"), READER).await;
    assert_eq!(r.code(), "api.invalid_parameter");

    // Evidence content as text, whole and as a range.
    let r = s.get(&format!("{g}/evidence/{frag}/content"), READER).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/plain")
    );
    assert!(r.text().starts_with("In 1843 Lovelace"));
    let r = s
        .get(
            &format!("{g}/evidence/{frag}/content?from=3&max_chars=4"),
            READER,
        )
        .await;
    assert_eq!(r.text(), "1843");
    let r = s.get(&format!("{g}/evidence/e_1/content"), READER).await;
    assert_eq!(r.code(), "content.not_found");

    // resolve, find, neighbors, paths.
    let r = s
        .post(
            &format!("{g}/resolve"),
            READER,
            json!({ "candidates": [{ "name": "ada lovelace", "type": "Person" }] }),
        )
        .await;
    assert_eq!(r.json()["results"][0]["verdict"], "match");
    let r = s
        .post(
            &format!("{g}/find"),
            READER,
            json!({
                "target": "nodes", "group_by": ["type"], "sort": [{ "by": "type" }]
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["groups"][0]["key"], json!({ "type": "Machine" }));
    let r = s
        .post(
            &format!("{g}/neighbors"),
            READER,
            json!({ "start": ["n_1"] }),
        )
        .await;
    assert_eq!(r.json()["nodes"].as_array().unwrap().len(), 3);
    let r = s
        .post(
            &format!("{g}/paths"),
            READER,
            json!({ "from": "n_2", "to": "n_3" }),
        )
        .await;
    assert_eq!(r.json()["paths"][0]["nodes"].as_array().unwrap().len(), 3);

    // DELETE retracts, restore brings back; history and changes show both.
    let r = s
        .call(
            axum::http::Method::DELETE,
            &format!("{g}/links/l_2?reason=unsure"),
            Some(WRITER),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["items"][0],
        json!({ "id": "l_2", "outcome": "retracted" })
    );
    let r = s
        .post(&format!("{g}/links/l_2/restore"), WRITER, json!(null))
        .await;
    assert_eq!(r.json()["items"][0]["outcome"], "restored");
    let h = s
        .get(&format!("{g}/links/l_2/history?limit=2"), READER)
        .await
        .json();
    let ops: Vec<&str> = h["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["op"].as_str().unwrap())
        .collect();
    assert_eq!(ops, ["restore", "retract"]);
    assert_eq!(h["more"], true);
    let c = s
        .get(&format!("{g}/changes?since_seq=12"), READER)
        .await
        .json();
    assert_eq!(c["entries"].as_array().unwrap().len(), 2);
    assert_eq!(c["last_seq"], 14);

    // merge: the old ID answers the survivor.
    s.post(&format!("{g}/write"), WRITER, json!({
        "evidence": [{ "ref": "@o", "kind": "observation", "content": "Same person" }],
        "nodes": [{ "type": "Person", "name": "C. Babbage", "create_new": true, "cite": ["@o"] }]
    })).await;
    let r = s
        .post(
            &format!("{g}/merge"),
            WRITER,
            json!({ "into": "n_2", "merge": ["n_4"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let n = s
        .get(&format!("{g}/nodes/n_4?detail=brief"), READER)
        .await
        .json();
    assert_eq!(
        (n["id"].as_str(), n["merged_from"].as_str()),
        (Some("n_2"), Some("n_4"))
    );
}

#[tokio::test]
async fn request_errors_map_to_statuses() {
    let s = server_with(|c| c.max_body_bytes = 4096);
    s.create_graph("research").await;
    let g = "/v1/graphs/research";

    let r = s.get("/v1/graphs/nope", READER).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "graph.not_found")
    );
    let r = s.post(&format!("{g}/teleport"), WRITER, json!({})).await;
    assert_eq!(r.code(), "api.not_found");

    // [API-HTTP-3]
    let req = Request::post(format!("{g}/write"))
        .header("authorization", format!("Bearer {WRITER}"))
        .body(Body::from("{\"nodes\": ["))
        .unwrap();
    let resp = s.router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: Json =
        serde_json::from_slice(&to_bytes(resp.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "json.malformed");
    let big = json!({ "query": "x".repeat(5000) });
    let r = s.post(&format!("{g}/search"), READER, big).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "api.body_too_large")
    );

    let r = s
        .post(
            &format!("{g}/search"),
            READER,
            json!({ "graph": "elsewhere", "query": "x" }),
        )
        .await;
    assert_eq!(r.code(), "api.graph_mismatch");
    let r = s
        .post(
            &format!("{g}/search"),
            READER,
            json!({ "query": "x", "limit": 0 }),
        )
        .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "search.invalid_field")
    );

    // [ERR-1] section and index of a failed write item, and the evidence policy's hint.
    let r = s
        .post(
            &format!("{g}/write"),
            WRITER,
            json!({ "nodes": [{ "type": "Person", "name": "Nobody" }] }),
        )
        .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "write.evidence_required")
    );
    assert!(
        r.json()["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("observation")
    );
    let r = s
        .post(
            &format!("{g}/write"),
            WRITER,
            json!({ "links": [{ "type": "X", "from": "n_9", "to": "n_8" }] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json()["error"]["section"], "links");
    assert_eq!(r.json()["error"]["index"], 0);

    // 409: a conflict with the stored state.
    s.post(&format!("{g}/write"), WRITER, ada_batch()).await;
    let r = s
        .post(
            &format!("{g}/write"),
            WRITER,
            json!({
                "updates": [{ "target": { "node": "n_1" }, "if_version": 7, "summary": "x" }]
            }),
        )
        .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "write.version_conflict")
    );
}

#[tokio::test]
async fn blobs_download_with_their_media_type() {
    let s = server();
    s.create_graph("files").await;
    let r = s.post("/v1/graphs/files/write", WRITER, json!({
        "evidence": [{ "ref": "@f", "kind": "file", "blob_base64": "aGVsbG8=", "mime": "text/plain" }]
    })).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = s.get("/v1/graphs/files/evidence/e_1/blob", READER).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(&r.body[..], b"hello");
    assert_eq!(r.headers["content-type"], "text/plain");
    assert_eq!(
        r.headers["etag"],
        "\"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\""
    );
    let r = s
        .post(
            "/v1/graphs/files/write",
            WRITER,
            json!({
                "evidence": [{ "kind": "observation", "content": "no blob here" }]
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = s.get("/v1/graphs/files/evidence/e_2/blob", READER).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "blob.not_found")
    );
}

#[tokio::test]
async fn admin_manages_graphs() {
    let s = server();
    // [ADMIN-GRAPH-2]
    let r = s
        .post(
            "/admin/graphs",
            ADMIN,
            json!({ "name": "lab", "policies": { "require_evidence": "warn" } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.headers["location"], "/admin/graphs/lab");
    let info = r.json();
    assert_eq!(info["status"], "ready");
    assert_eq!(info["policies"]["require_evidence"], "warn");
    assert_eq!(info["text_index"], "current");
    let r = s
        .post("/admin/graphs", ADMIN, json!({ "name": "lab" }))
        .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "graph.exists")
    );
    let r = s
        .post("/admin/graphs", ADMIN, json!({ "name": "Bad Name" }))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = s
        .post(
            "/admin/graphs",
            ADMIN,
            json!({ "name": "x", "policies": { "colour": 1 } }),
        )
        .await;
    assert_eq!(r.code(), "admin.invalid_field");

    // Under `warn`, a fact without evidence is accepted.
    let r = s
        .post(
            "/v1/graphs/lab/write",
            WRITER,
            json!({ "nodes": [{ "type": "Thing", "name": "Widget" }] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let info = s.get("/admin/graphs/lab", ADMIN).await.json();
    assert_eq!(info["counts"]["nodes"], 1);
    assert_eq!(info["last_seq"], 1);

    // [ADMIN-GRAPH-4] [ADMIN-GRAPH-5]
    let r = s
        .post("/admin/graphs/lab/snapshot", ADMIN, json!(null))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let snap = std::path::PathBuf::from(r.json()["path"].as_str().unwrap());
    assert!(snap.starts_with(s.dir.path().join("snapshots")));
    assert!(snap.is_file());
    let r = s.post("/admin/graphs/lab/check", ADMIN, json!(null)).await;
    assert_eq!(r.json(), json!({ "ok": true, "problems": [] }));

    let stats = s.get("/admin/stats", ADMIN).await.json();
    assert_eq!(stats["graphs"]["lab"], "ready");
    assert_eq!(stats["requests"]["write"]["count"], 1);
    let keys = s.get("/admin/keys", ADMIN).await;
    assert!(!keys.text().contains(WRITER));
    assert_eq!(keys.json()["keys"].as_array().unwrap().len(), 4);
    let cfg = s.get("/admin/config", ADMIN).await.json();
    assert_eq!(cfg["keys"], 4);
    let m = s.call(Method::GET, "/metrics", None, None).await.text();
    assert!(
        m.contains("mygraph_requests_total{op=\"write\",status=\"2xx\"} 1"),
        "{m}"
    );
    assert!(
        m.contains("mygraph_objects{graph=\"lab\",kind=\"nodes\"} 1"),
        "{m}"
    );
    // [ADMIN-MET-1]
    for line in [
        "mygraph_graph_state{graph=\"lab\",state=\"ready\"} 1",
        "mygraph_graph_last_seq{graph=\"lab\"} ",
        "mygraph_graph_file_bytes{graph=\"lab\",file=\"db\"} ",
        "mygraph_graph_load_seconds{graph=\"lab\"} ",
        "mygraph_build_info{version=\"",
        "mygraph_uptime_seconds ",
        "mygraph_checkpoints_total{graph=\"lab\",outcome=\"ok\"} ",
        "mygraph_errors_total{code=\"graph.exists\"} 1",
    ] {
        assert!(m.contains(line), "{line} in {m}");
    }
    assert!(stats["versions"]["spec_version"].is_string());
    assert!(stats["slow_ms"].is_u64());

    // [ADMIN-GRAPH-3]
    let r = s
        .call(Method::DELETE, "/admin/graphs/lab", Some(ADMIN), None)
        .await;
    assert_eq!(r.code(), "admin.confirm_required");
    let r = s
        .call(
            Method::DELETE,
            "/admin/graphs/lab?confirm=lab",
            Some(ADMIN),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let moved = std::path::PathBuf::from(r.json()["moved_to"].as_str().unwrap());
    assert!(moved.is_file());
    assert!(!s.dir.path().join("graphs/lab.db").exists());
    let r = s.get("/v1/graphs/lab", READER).await;
    assert_eq!(r.code(), "graph.not_found");
    // The name is free again.
    s.create_graph("lab").await;
    let info = s.get("/admin/graphs/lab", ADMIN).await.json();
    assert_eq!(info["counts"]["nodes"], 0);
}

#[tokio::test]
async fn graphs_on_disk_load_on_first_use() {
    let dir = tempfile::tempdir().unwrap();
    {
        let file = mygraph_store_sqlite::GraphFile::create(
            dir.path(),
            "old",
            &mygraph_core::model::Policies::default(),
            1,
        )
        .unwrap();
        let mut g = mygraph_core::engine::Graph::open(file, "old", Default::default()).unwrap();
        g.write("seed", &ada_batch(), 1).unwrap();
    }
    let s = server_with(|c| c.data_dir = dir.path().to_owned());
    assert_eq!(s.app.registry.state("old").unwrap().name(), "unloaded");
    let r = s.call(Method::GET, "/ready", None, None).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    let r = s.get("/v1/graphs/old", READER).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json()["counts"]["nodes"], 3);
    // The tantivy index was built on load and answers full-text queries.
    let r = s
        .get(
            "/v1/graphs/old/search?q=published&in=evidence&mode=text",
            READER,
        )
        .await;
    assert_eq!(r.json()["hits"][0]["id"], "e_2");
    let r = s.call(Method::GET, "/ready", None, None).await;
    assert_eq!(
        r.json(),
        json!({ "ready": true, "graphs": { "old": "ready" } })
    );
}

#[tokio::test]
async fn without_keys_requests_are_the_local_admin() {
    let s = server_with(|c| c.keys.clear());
    let r = s
        .call(
            Method::POST,
            "/admin/graphs",
            None,
            Some(json!({ "name": "solo" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = s
        .call(
            Method::POST,
            "/v1/graphs/solo/write",
            None,
            Some(ada_batch()),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = s.call(Method::GET, "/v1/graphs/solo", None, None).await;
    assert_eq!(r.json()["recent"][0]["actor"], "local");

    // [API-AUTH-4]
    let mut c = Config {
        listen: "0.0.0.0:7420".parse().unwrap(),
        ..Config::default()
    };
    assert!(c.validate().is_err());
    c.keys.push(key("k", "a", Role::AgentRead, &[]));
    assert!(c.validate().is_ok());
}

/// Object lines of an export file (the header and actors differ between copies).
fn object_lines(path: &str) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|l| l.starts_with("{\"id\"") || l.contains("\"id\":"))
        .filter(|l| !l.contains("\"mygraph_export\""))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn export_import_and_restore_keep_everything() {
    let s = server();
    s.create_graph("src").await;
    let g = "/v1/graphs/src";
    s.post(&format!("{g}/write"), WRITER, ada_batch()).await;
    s.post(&format!("{g}/write"), WRITER, json!({
        "evidence": [{ "ref": "@o", "kind": "observation", "content": "Same person" }],
        "nodes": [{ "type": "Person", "name": "C. Babbage", "create_new": true, "cite": ["@o"] }]
    })).await;
    s.post(
        &format!("{g}/merge"),
        WRITER,
        json!({ "into": "n_2", "merge": ["n_4"] }),
    )
    .await;
    s.call(
        Method::DELETE,
        &format!("{g}/links/l_2"),
        Some(WRITER),
        None,
    )
    .await;

    // [ADMIN-GRAPH-7]
    let r = s.post("/admin/graphs/src/export", ADMIN, json!(null)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let exp = r.json();
    assert_eq!(exp["counts"]["nodes"], 4);
    let path = exp["path"].as_str().unwrap().to_owned();
    let file = std::path::Path::new(&path)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let first: Json = serde_json::from_str(
        std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["mygraph_export"], 1);

    // [ADMIN-GRAPH-8] into a new graph: identical objects, IDs and statuses.
    let r = s
        .post("/admin/graphs/copy/import", ADMIN, json!({ "file": file }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["imported"],
        json!({ "nodes": 4, "links": 2, "evidence": 3, "citations": 6 })
    );
    let copy = s
        .post("/admin/graphs/copy/export", ADMIN, json!(null))
        .await
        .json();
    assert_eq!(
        object_lines(&path),
        object_lines(copy["path"].as_str().unwrap())
    );
    let n = s
        .get("/v1/graphs/copy/nodes/n_4?detail=brief", READER)
        .await
        .json();
    assert_eq!(n["merged_from"], "n_4");
    let l = s.get("/v1/graphs/copy/links/l_2", READER).await.json();
    assert_eq!(l["status"], "retracted");
    assert_eq!(l["ends"]["to"]["name"], "Analytical Engine");
    let h = s
        .get("/v1/graphs/copy/nodes/n_1/history", READER)
        .await
        .json();
    assert_eq!(h["entries"][0]["op"], "create");
    // Searchable and writable after the import; new IDs continue after the imported ones.
    let r = s.get("/v1/graphs/copy/search?q=lovelace", READER).await;
    assert_eq!(r.json()["hits"][0]["id"], "n_1");
    let r = s
        .get(
            "/v1/graphs/copy/search?q=published&in=evidence&mode=text",
            READER,
        )
        .await;
    assert_eq!(r.json()["hits"][0]["id"], "e_2", "{}", r.text());
    let r = s
        .post(
            "/v1/graphs/copy/write",
            WRITER,
            json!({
                "evidence": [{ "ref": "@o", "kind": "observation", "content": "new" }],
                "nodes": [{ "type": "Person", "name": "Grace Hopper", "cite": ["@o"] }]
            }),
        )
        .await;
    assert_eq!(r.json()["refs"]["@o"], "e_4");

    // A graph with objects is refused; a file outside exports/ cannot be named.
    let r = s
        .post("/admin/graphs/copy/import", ADMIN, json!({ "file": file }))
        .await;
    assert_eq!(r.code(), "import.graph_not_empty");
    let r = s
        .post(
            "/admin/graphs/x/import",
            ADMIN,
            json!({ "file": "../graphs/src.db" }),
        )
        .await;
    assert_eq!(r.code(), "admin.file_not_found");

    // [ADMIN-GRAPH-6]
    let snap = s
        .post("/admin/graphs/src/snapshot", ADMIN, json!(null))
        .await
        .json();
    let snap_file = std::path::Path::new(snap["path"].as_str().unwrap())
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let r = s
        .post(
            "/admin/graphs/copy/restore",
            ADMIN,
            json!({ "snapshot": snap_file }),
        )
        .await;
    assert_eq!(r.code(), "graph.exists");
    let r = s
        .post(
            "/admin/graphs/copy/restore",
            ADMIN,
            json!({ "snapshot": snap_file, "replace": true }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json()["counts"]["nodes"],
        3,
        "the snapshot, without Grace Hopper"
    );
    let r = s.get("/v1/graphs/copy/search?q=lovelace", READER).await;
    assert_eq!(r.json()["hits"][0]["id"], "n_1");
}

#[test]
fn one_process_per_data_folder() {
    // [STORE-DIR-4]
    let dir = tempfile::tempdir().unwrap();
    let first = mygraph_server::lock_data_dir(dir.path()).unwrap();
    let second = mygraph_server::lock_data_dir(dir.path()).unwrap_err();
    assert!(
        second.contains("in use by another MyGraph process"),
        "{second}"
    );
    drop(first);
    mygraph_server::lock_data_dir(dir.path()).unwrap();
}

#[tokio::test]
async fn stats_for_agents_and_admins() {
    let s = server();
    s.create_graph("research").await;
    let g = "/v1/graphs/research";

    // Empty graph ([OP-STATS-4]): every number 0.
    let r = s.get(&format!("{g}/stats"), READER).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let e = r.json();
    assert_eq!(e["degree"]["nodes"], 0);
    assert_eq!(e["degree"]["hubs"], json!([]));
    assert_eq!(e["growth"]["per_day"], json!([]));

    let w = s
        .post(&format!("{g}/write"), WRITER, ada_batch())
        .await
        .json();
    let frag = w["refs"]["@frag"].as_str().unwrap().to_owned();
    let r = s
        .post(
            &format!("{g}/write"),
            WRITER,
            json!({ "nodes": [{ "type": "Place", "name": "London", "cite": [frag] }] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = s
        .call(
            Method::DELETE,
            &format!("{g}/links/l_2"),
            Some(WRITER),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let st = s
        .post(&format!("{g}/stats"), READER, json!({}))
        .await
        .json();
    assert_eq!(st["graph"], "research");
    // [OP-STATS-3]
    let c = &st["counts"];
    assert_eq!(c["active"]["nodes"], 4);
    assert_eq!(c["active"]["links"], 1);
    assert_eq!(c["retracted"]["links"], 1);
    assert_eq!(c["merged"]["nodes"], 0);
    assert_eq!(c["node_types"], 3);
    assert_eq!(
        c["top_node_types"][0],
        json!({ "type": "Person", "count": 2 })
    );
    // [OP-STATS-4] Degrees 1, 1, 0, 0 after the retraction.
    let d = &st["degree"];
    assert_eq!(
        (&d["nodes"], &d["max"], &d["mean"], &d["orphans"]),
        (&json!(4), &json!(1), &json!(0.5), &json!(2))
    );
    assert_eq!((&d["p50"], &d["p90"]), (&json!(0), &json!(1)));
    assert_eq!(d["hubs"][0]["id"], "n_1");
    assert_eq!(d["hubs"][0]["degree"], 1);
    assert_eq!(d["hubs"].as_array().unwrap().len(), 2);
    // [OP-STATS-5] Everything happened today.
    let gr = &st["growth"];
    assert_eq!(gr["per_day"].as_array().unwrap().len(), 1);
    assert_eq!(gr["per_day"][0]["day"], gr["to"]);
    assert_eq!(gr["totals"]["created"]["nodes"], 4);
    assert_eq!(gr["totals"]["created"]["links"], 2);
    assert_eq!(gr["totals"]["retracted"]["links"], 1);
    // [OP-STATS-6] The page is cited through its fragment.
    let ev = &st["evidence"];
    assert_eq!(ev["active"], 2);
    assert_eq!(ev["uncited"], 0);
    assert_eq!(
        ev["domains"],
        json!([{ "domain": "example.org", "count": 1 }])
    );
    assert_eq!(ev["most_cited"][0]["id"], frag.as_str());
    assert_eq!(ev["most_cited"].as_array().unwrap().len(), 1);

    // Shorthand with sections and top; admin-only sections are refused ([OP-STATS-1]).
    let r = s
        .get(&format!("{g}/stats?sections=degree&top=1"), READER)
        .await
        .json();
    assert!(r.get("counts").is_none());
    assert_eq!(r["degree"]["hubs"].as_array().unwrap().len(), 1);
    for bad in [
        json!({ "sections": ["actors"] }),
        json!({ "sections": [] }),
        json!({ "sections": ["degree", "degree"] }),
        json!({ "days": 0 }),
        json!({ "top": 101 }),
        json!({ "colour": 1 }),
    ] {
        let r = s.post(&format!("{g}/stats"), READER, bad.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(r.code(), "stats.invalid_field", "{bad}");
    }

    // [ADMIN-GRAPH-9] All six sections for admins only.
    let r = s.get("/admin/graphs/research/stats", READER).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let a = s.get("/admin/graphs/research/stats", ADMIN).await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.text());
    let a = a.json();
    let agent = a["actors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["actor"] == "agent-a")
        .unwrap()
        .clone();
    assert_eq!(agent["batches"], 3);
    assert_eq!(agent["created"]["nodes"], 4);
    assert!(agent["retracted"].as_u64().unwrap() >= 1);
    assert_eq!(a["actors"][0]["actor"], "agent-a");
    assert!(a["storage"]["file_bytes"].as_u64().unwrap() > 0);
    assert_eq!(a["storage"]["text_index"], st_text_index(&s).await);
    let r = s
        .get("/admin/graphs/research/stats?sections=storage", ADMIN)
        .await
        .json();
    assert!(r.get("degree").is_none() && r.get("storage").is_some());
    let r = s.get("/admin/graphs/research/nope", ADMIN).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

async fn st_text_index(s: &Server) -> Json {
    s.get("/admin/graphs/research", ADMIN).await.json()["text_index"].clone()
}

#[tokio::test]
async fn slow_requests_and_the_stats_page() {
    // Every request is slow with slow_ms = 0 ([ADMIN-SRV-2]).
    let s = server_with(|c| c.slow_ms = 0);
    s.create_graph("lab").await;
    s.get("/v1/graphs/nope", READER).await;
    let r = s.get("/admin/slow-ops", ADMIN).await.json();
    assert_eq!(r["slow_ms"], 0);
    let ops = r["ops"].as_array().unwrap();
    assert_eq!(ops[0]["op"], "overview");
    assert_eq!(ops[0]["graph"], "nope");
    assert_eq!(ops[0]["status"], 404);
    assert!(ops.iter().any(|o| o["op"] == "admin.create_graph"));
    let m = s.call(Method::GET, "/metrics", None, None).await.text();
    assert!(
        m.contains("mygraph_slow_requests_total{op=\"overview\"} 1"),
        "{m}"
    );
    assert!(
        m.contains("mygraph_errors_total{code=\"graph.not_found\"} 1"),
        "{m}"
    );
    let r = s.get("/admin/slow-ops", READER).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // [ADMIN-UI-1] The page needs no key and loads nothing from elsewhere.
    let r = s.call(Method::GET, "/admin/ui", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    let csp = r.headers["content-security-policy"].to_str().unwrap();
    assert!(
        csp.contains("default-src 'none'") && csp.contains("connect-src 'self'"),
        "{csp}"
    );
    let page = r.text();
    assert!(page.contains("/admin/graphs"));
    for external in ["src=\"http", "href=\"http", "@import", "fetch(\"http"] {
        assert!(!page.contains(external), "{external}");
    }

    // [ADMIN-UI-2] The viewer and its library, served by the server itself.
    let r = s
        .call(Method::GET, "/admin/ui/graph?graph=lab", None, None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.headers.contains_key("content-security-policy"));
    let page = r.text();
    assert!(page.contains("<script src=\"/admin/ui/cytoscape.min.js\"></script>"));
    for external in ["src=\"http", "href=\"http", "@import", "fetch(\"http"] {
        assert!(!page.contains(external), "{external}");
    }
    let r = s
        .call(Method::GET, "/admin/ui/cytoscape.min.js", None, None)
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/javascript")
    );
    assert!(r.text().contains("Cytoscape Consortium"));
}
