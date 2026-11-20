//! OAuth for remote MCP clients end to end, through the router (spec/api/oauth.md): discovery,
//! registration, the sign-in page, codes with PKCE, tokens on `/mcp`, refresh rotation and
//! reuse, read-only sign-ins, revocation, and a removed key signing its apps out.

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mygraph_server::App;
use mygraph_server::config::{Config, KeyConfig, Role};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tower::ServiceExt;

const PUBLIC: &str = "https://mg.example.com";
const CALLBACK: &str = "https://claude.ai/api/mcp/auth_callback";
const AGENT: &str = "agent-key";
const ADMIN: &str = "admin-key";
const VERIFIER: &str = "a-pkce-code-verifier-that-is-long-enough-0123456789";

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
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    /// The query of the redirect this answer sends the browser to.
    fn redirect(&self) -> HashMap<String, String> {
        assert_eq!(self.status, StatusCode::SEE_OTHER, "{}", self.text());
        let to = self.headers[header::LOCATION].to_str().unwrap();
        assert!(to.starts_with(CALLBACK), "{to}");
        url::Url::parse(to)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    }
}

fn key(key: &str, actor: &str, role: Role) -> KeyConfig {
    KeyConfig {
        key: Some(key.to_owned()),
        key_sha256: None,
        actor: actor.to_owned(),
        role,
        graphs: Vec::new(),
    }
}

fn config(dir: &std::path::Path, keys: Vec<KeyConfig>) -> Config {
    let mut c = Config {
        data_dir: dir.to_owned(),
        preload: false,
        keys,
        ..Config::default()
    };
    c.oauth.public_url = Some(PUBLIC.to_owned());
    c.validate().unwrap();
    c
}

async fn call(
    router: &Router,
    method: Method,
    uri: &str,
    auth: Option<&str>,
    content_type: &str,
    body: String,
) -> Reply {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, content_type);
    if let Some(a) = auth {
        req = req.header(header::AUTHORIZATION, format!("Bearer {a}"));
    }
    let resp = router
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    Reply {
        status: resp.status(),
        headers: resp.headers().clone(),
        body: to_bytes(resp.into_body(), usize::MAX).await.unwrap(),
    }
}

async fn get(router: &Router, uri: &str, auth: Option<&str>) -> Reply {
    call(router, Method::GET, uri, auth, "text/plain", String::new()).await
}

async fn post_json(router: &Router, uri: &str, auth: Option<&str>, body: Json) -> Reply {
    call(
        router,
        Method::POST,
        uri,
        auth,
        "application/json",
        body.to_string(),
    )
    .await
}

async fn post_form(router: &Router, uri: &str, fields: &[(&str, &str)]) -> Reply {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields)
        .finish();
    call(
        router,
        Method::POST,
        uri,
        None,
        "application/x-www-form-urlencoded",
        body,
    )
    .await
}

/// A JSON-RPC call to `/mcp`.
async fn mcp(router: &Router, token: &str, method: &str, params: Json) -> Reply {
    post_json(
        router,
        "/mcp",
        Some(token),
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }),
    )
    .await
}

fn challenge() -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes()))
}

fn authorize_query(client_id: &str) -> Vec<(&'static str, String)> {
    vec![
        ("response_type", "code".into()),
        ("client_id", client_id.into()),
        ("redirect_uri", CALLBACK.into()),
        ("code_challenge", challenge()),
        ("code_challenge_method", "S256".into()),
        ("state", "st-123".into()),
        ("scope", "mcp offline_access".into()),
        ("resource", format!("{PUBLIC}/mcp")),
    ]
}

fn query(pairs: &[(&str, String)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// Signs in on the form and returns the code.
async fn sign_in(router: &Router, client_id: &str, key: &str, read_only: bool) -> String {
    let mut fields: Vec<(&str, String)> = authorize_query(client_id);
    fields.push(("key", key.into()));
    fields.push(("action", "allow".into()));
    if read_only {
        fields.push(("read_only", "1".into()));
    }
    let pairs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let q = post_form(router, "/oauth/authorize", &pairs)
        .await
        .redirect();
    assert_eq!(q["state"], "st-123");
    assert_eq!(q["iss"], PUBLIC);
    q["code"].clone()
}

async fn exchange(router: &Router, client_id: &str, code: &str, verifier: &str) -> Reply {
    post_form(
        router,
        "/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", client_id),
            ("redirect_uri", CALLBACK),
            ("code_verifier", verifier),
            ("resource", &format!("{PUBLIC}/mcp")),
        ],
    )
    .await
}

async fn refresh(router: &Router, client_id: &str, token: &str) -> Reply {
    post_form(
        router,
        "/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", token),
            ("client_id", client_id),
        ],
    )
    .await
}

#[tokio::test]
async fn a_remote_client_signs_in_and_calls_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let keys = vec![
        key(AGENT, "agent-a", Role::AgentWrite),
        key(ADMIN, "ops", Role::Admin),
    ];
    let app = App::new(config(dir.path(), keys));
    let router = mygraph_server::router(app.clone());
    let r = post_json(
        &router,
        "/admin/graphs",
        Some(ADMIN),
        json!({ "name": "default" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());

    // [API-OAUTH-1] Without a token, /mcp says where to find out how to sign in.
    let r = mcp(&router, "", "tools/list", json!({})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        r.headers[header::WWW_AUTHENTICATE],
        format!("Bearer resource_metadata=\"{PUBLIC}/.well-known/oauth-protected-resource\"")
    );
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let r = get(&router, path, None).await;
        assert_eq!(r.json()["resource"], format!("{PUBLIC}/mcp"));
        assert_eq!(r.json()["authorization_servers"], json!([PUBLIC]));
    }
    // [API-OAUTH-2]
    let m = get(&router, "/.well-known/oauth-authorization-server", None)
        .await
        .json();
    assert_eq!(m["issuer"], PUBLIC);
    assert_eq!(m["token_endpoint"], format!("{PUBLIC}/oauth/token"));
    assert_eq!(m["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(m["token_endpoint_auth_methods_supported"], json!(["none"]));
    assert_eq!(m["client_id_metadata_document_supported"], true);

    // [API-OAUTH-3] Registration, limited to the allowed redirect URIs.
    let r = post_json(
        &router,
        "/oauth/register",
        None,
        json!({ "client_name": "Claude", "redirect_uris": [CALLBACK], "token_endpoint_auth_method": "none" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let client = r.json()["client_id"].as_str().unwrap().to_owned();
    assert_eq!(r.json()["token_endpoint_auth_method"], "none");
    let r = post_json(
        &router,
        "/oauth/register",
        None,
        json!({ "client_name": "Evil", "redirect_uris": ["https://evil.example/cb"] }),
    )
    .await;
    assert_eq!(r.json()["error"], "invalid_redirect_uri");

    // [API-OAUTH-5] The sign-in page names the app and where the browser goes back to.
    let r = get(
        &router,
        &format!("/oauth/authorize?{}", query(&authorize_query(&client))),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let page = r.text();
    assert!(
        page.contains("calls itself <b>Claude</b>") && page.contains("<b>claude.ai</b>"),
        "{page}"
    );
    assert!(
        r.headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );

    // Until the client and redirect URI check out, errors stay on the page.
    let mut unknown = authorize_query("no-such-client");
    let r = get(
        &router,
        &format!("/oauth/authorize?{}", query(&unknown)),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(!r.headers.contains_key(header::LOCATION));
    unknown = authorize_query(&client);
    unknown[2].1 = "https://claude.ai/elsewhere".into();
    let r = get(
        &router,
        &format!("/oauth/authorize?{}", query(&unknown)),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Then they go back to the client.
    let mut no_pkce = authorize_query(&client);
    no_pkce.retain(|(k, _)| *k != "code_challenge_method");
    let q = get(
        &router,
        &format!("/oauth/authorize?{}", query(&no_pkce)),
        None,
    )
    .await
    .redirect();
    assert_eq!(
        (q["error"].as_str(), q["state"].as_str()),
        ("invalid_request", "st-123")
    );
    let mut other_resource = authorize_query(&client);
    other_resource[7].1 = "https://other.example/mcp".into();
    let q = get(
        &router,
        &format!("/oauth/authorize?{}", query(&other_resource)),
        None,
    )
    .await
    .redirect();
    assert_eq!(q["error"], "invalid_target");

    // A wrong key stays on the page; Deny goes back with access_denied.
    let mut fields = authorize_query(&client);
    fields.push(("key", "wrong".into()));
    fields.push(("action", "allow".into()));
    let pairs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let r = post_form(&router, "/oauth/authorize", &pairs).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.text().contains("not valid"));
    let mut fields = authorize_query(&client);
    fields.push(("action", "deny".into()));
    let pairs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let q = post_form(&router, "/oauth/authorize", &pairs)
        .await
        .redirect();
    assert_eq!(q["error"], "access_denied");

    // [API-OAUTH-6] A code needs the PKCE verifier, and works once.
    let code = sign_in(&router, &client, AGENT, false).await;
    let r = exchange(
        &router,
        &client,
        &code,
        "a-different-verifier-that-is-long-enough-012345",
    )
    .await;
    assert_eq!(r.json()["error"], "invalid_grant");
    let r = exchange(&router, &client, &code, VERIFIER).await;
    assert_eq!(r.json()["error"], "invalid_grant", "a code is used once");
    let code = sign_in(&router, &client, AGENT, false).await;
    let t = exchange(&router, &client, &code, VERIFIER).await;
    assert_eq!(t.status, StatusCode::OK, "{}", t.text());
    assert_eq!(t.headers[header::CACHE_CONTROL], "no-store");
    let t = t.json();
    assert_eq!(
        (t["token_type"].as_str(), t["expires_in"].as_i64()),
        (Some("Bearer"), Some(3600))
    );
    assert_eq!(t["scope"], "mcp offline_access");
    let access = t["access_token"].as_str().unwrap().to_owned();
    let refresh1 = t["refresh_token"].as_str().unwrap().to_owned();

    // The token calls /mcp as the key's actor, and nothing else.
    let r = mcp(&router, &access, "tools/list", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(r.json()["result"]["tools"].as_array().unwrap().len() > 5);
    let r = mcp(
        &router,
        &access,
        "tools/call",
        json!({ "name": "write", "arguments": {
            "evidence": [{ "ref": "@e", "kind": "text", "content": "Ada wrote notes." }],
            "nodes": [{ "type": "Person", "name": "Ada Lovelace", "cite": ["@e"] }] } }),
    )
    .await;
    assert_ne!(r.json()["result"]["isError"], true, "{}", r.text());
    let r = get(&router, "/v1/graphs/default", Some(ADMIN)).await;
    assert_eq!(r.json()["recent"][0]["actor"], "agent-a");
    let r = get(&router, "/v1/graphs", Some(&access)).await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "tokens are for /mcp only"
    );

    // [API-OAUTH-7] Refresh tokens rotate; a retry soon after is forgiven...
    let r = refresh(&router, &client, &refresh1).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let refresh2 = r.json()["refresh_token"].as_str().unwrap().to_owned();
    assert_ne!(refresh2, refresh1);
    let r = refresh(&router, &client, &refresh1).await;
    assert_eq!(r.status, StatusCode::OK, "a lost answer may be retried");
    let access3 = r.json()["access_token"].as_str().unwrap().to_owned();
    // ...but an old one coming back later revokes the whole grant.
    {
        let db = rusqlite::Connection::open(dir.path().join("server.db")).unwrap();
        db.execute(
            "UPDATE oauth_tokens SET used = used - 600000 WHERE used IS NOT NULL",
            [],
        )
        .unwrap();
    }
    let r = refresh(&router, &client, &refresh1).await;
    assert_eq!(r.json()["error"], "invalid_grant");
    assert_eq!(
        mcp(&router, &access3, "tools/list", json!({})).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        refresh(&router, &client, &refresh2).await.json()["error"],
        "invalid_grant"
    );

    // A read-only sign-in can read but not write.
    let code = sign_in(&router, &client, AGENT, true).await;
    let t = exchange(&router, &client, &code, VERIFIER).await.json();
    let reader = t["access_token"].as_str().unwrap().to_owned();
    let r = mcp(
        &router,
        &reader,
        "tools/call",
        json!({ "name": "search", "arguments": { "query": "Ada" } }),
    )
    .await;
    assert_ne!(r.json()["result"]["isError"], true, "{}", r.text());
    let r = mcp(
        &router,
        &reader,
        "tools/call",
        json!({ "name": "write", "arguments": {
            "evidence": [{ "ref": "@e", "kind": "text", "content": "Babbage built engines." }],
            "nodes": [{ "type": "Person", "name": "Charles Babbage", "cite": ["@e"] }] } }),
    )
    .await;
    assert_eq!(r.json()["result"]["isError"], true);
    assert!(r.text().contains("auth.forbidden"), "{}", r.text());

    // [ADMIN-OAUTH-1] Admins see the grants and [ADMIN-OAUTH-2] revoke them.
    let list = get(&router, "/admin/oauth", Some(ADMIN)).await.json();
    let grants = list["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 1, "{list}");
    assert_eq!(
        (
            grants[0]["actor"].as_str(),
            grants[0]["client_name"].as_str(),
            grants[0]["read_only"].as_bool()
        ),
        (Some("agent-a"), Some("Claude"), Some(true))
    );
    let id = grants[0]["id"].as_str().unwrap();
    let r = call(
        &router,
        Method::DELETE,
        &format!("/admin/oauth/grants/{id}"),
        Some(ADMIN),
        "text/plain",
        String::new(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        mcp(&router, &reader, "tools/list", json!({})).await.status,
        StatusCode::UNAUTHORIZED
    );

    // RFC 7009: revoking a refresh token signs the app out.
    let code = sign_in(&router, &client, AGENT, false).await;
    let t = exchange(&router, &client, &code, VERIFIER).await.json();
    let (a, rt) = (
        t["access_token"].as_str().unwrap(),
        t["refresh_token"].as_str().unwrap(),
    );
    let r = post_form(
        &router,
        "/oauth/revoke",
        &[("token", rt), ("token_type_hint", "refresh_token")],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        mcp(&router, a, "tools/list", json!({})).await.status,
        StatusCode::UNAUTHORIZED
    );

    // A key removed from the configuration signs its apps out.
    let code = sign_in(&router, &client, AGENT, false).await;
    let t = exchange(&router, &client, &code, VERIFIER).await.json();
    let token = t["access_token"].as_str().unwrap().to_owned();
    assert_eq!(
        mcp(&router, &token, "tools/list", json!({})).await.status,
        StatusCode::OK
    );
    drop(router);
    drop(app);
    let app = App::new(config(dir.path(), vec![key(ADMIN, "ops", Role::Admin)]));
    let router = mygraph_server::router(app);
    assert_eq!(
        mcp(&router, &token, "tools/list", json!({})).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn oauth_is_off_without_a_public_url() {
    let dir = tempfile::tempdir().unwrap();
    let c = Config {
        data_dir: dir.path().to_owned(),
        preload: false,
        keys: vec![key(AGENT, "agent-a", Role::AgentWrite)],
        ..Config::default()
    };
    let router = mygraph_server::router(App::new(c));
    let r = get(&router, "/.well-known/oauth-authorization-server", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = mcp(&router, "", "tools/list", json!({})).await;
    assert_eq!(r.headers[header::WWW_AUTHENTICATE], "Bearer");
}

#[test]
fn public_url_and_redirect_settings_are_checked() {
    let with = |url: &str, keys: bool| {
        let mut c = Config::default();
        if keys {
            c.keys.push(key(AGENT, "agent-a", Role::AgentWrite));
        }
        c.oauth.public_url = Some(url.to_owned());
        c.validate()
    };
    assert!(with("https://mg.example.com", true).is_ok());
    assert!(with("http://127.0.0.1:7420", true).is_ok());
    assert!(
        with("http://mg.example.com", true).is_err(),
        "plain HTTP off loopback"
    );
    assert!(
        with("https://mg.example.com/mygraph", true).is_err(),
        "a path"
    );
    assert!(
        with("https://mg.example.com", false).is_err(),
        "no keys to sign in with"
    );

    let mut c = Config::default();
    c.keys.push(key(AGENT, "agent-a", Role::AgentWrite));
    c.oauth.public_url = Some("https://mg.example.com".into());
    c.oauth.redirect_uris = vec!["https://claude.ai/*".into()];
    assert!(c.validate().is_err(), "wildcards only on loopback");
}

/// [API-OAUTH-9] The public router answers only what remote clients use.
#[tokio::test]
async fn the_public_router_serves_only_mcp_and_oauth() {
    let dir = tempfile::tempdir().unwrap();
    let keys = vec![
        key(AGENT, "agent-a", Role::AgentWrite),
        key(ADMIN, "ops", Role::Admin),
    ];
    let app = App::new(config(dir.path(), keys));
    let public = mygraph_server::public_router(app.clone());

    let r = get(&public, "/.well-known/oauth-authorization-server", None).await;
    assert_eq!(r.json()["issuer"], PUBLIC);
    let r = get(&public, "/.well-known/oauth-protected-resource/mcp", None).await;
    assert_eq!(r.json()["resource"], format!("{PUBLIC}/mcp"));
    let r = get(
        &public,
        &format!("/oauth/authorize?{}", query(&authorize_query("x"))),
        None,
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "an unknown client, shown on the page"
    );
    let r = mcp(&public, "", "tools/list", json!({})).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(
        r.headers[header::WWW_AUTHENTICATE]
            .to_str()
            .unwrap()
            .contains("resource_metadata")
    );
    // A key works on /mcp, as on the main listener.
    let r = mcp(&public, AGENT, "tools/list", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    // Everything else is not there, even with the admin key.
    for path in [
        "/health",
        "/ready",
        "/metrics",
        "/openapi.yaml",
        "/v1/graphs",
        "/admin/graphs",
        "/admin/oauth",
        "/admin/config",
        "/admin/ui",
        "/admin/ui/graph",
        "/",
    ] {
        let r = get(&public, path, Some(ADMIN)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
        assert!(r.body.is_empty(), "{path}: {}", r.text());
    }
    let r = post_json(
        &public,
        "/admin/graphs",
        Some(ADMIN),
        json!({ "name": "x" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(
        &public,
        Method::DELETE,
        "/admin/oauth/grants/1",
        Some(ADMIN),
        "text/plain",
        String::new(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(app.registry.list().is_empty(), "nothing was created");
}

/// [API-OAUTH-9] `serve` runs the public listener next to the main one.
#[test]
fn the_public_listener_runs_beside_the_main_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = Config {
        data_dir: dir.path().to_owned(),
        preload: false,
        keys: vec![key(AGENT, "agent-a", Role::AgentWrite)],
        ..Config::default()
    };
    c.search.semantic = false;
    c.oauth.public_url = Some("http://127.0.0.1:1".into());
    c.validate().unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let main = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let public = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let (main_port, public_port) = (
        main.local_addr().unwrap().port(),
        public.local_addr().unwrap().port(),
    );
    let app = {
        let _guard = rt.enter();
        App::new(c)
    };
    rt.spawn(mygraph_server::serve_listeners(app, main, Some(public)));

    let status =
        |port: u16, path: &str| match ureq::get(&format!("http://127.0.0.1:{port}{path}")).call() {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(s, _)) => s,
            Err(e) => panic!("{e}"),
        };
    assert_eq!(status(main_port, "/health"), 200);
    assert_eq!(status(main_port, "/ready"), 200);
    assert_eq!(status(public_port, "/health"), 404);
    assert_eq!(status(public_port, "/ready"), 404);
    assert_eq!(status(public_port, "/v1/graphs"), 404);
    assert_eq!(
        status(public_port, "/.well-known/oauth-authorization-server"),
        200
    );
}

#[test]
fn the_public_listener_needs_oauth_and_its_own_port() {
    let mut c = Config::default();
    c.keys.push(key(AGENT, "agent-a", Role::AgentWrite));
    c.oauth.listen = Some("0.0.0.0:7421".parse().unwrap());
    let err = c.validate().unwrap_err();
    assert!(err.contains("oauth.public_url"), "{err}");

    c.oauth.public_url = Some(PUBLIC.into());
    c.validate().unwrap();
    c.oauth.listen = Some(format!("0.0.0.0:{}", c.listen.port()).parse().unwrap());
    let err = c.validate().unwrap_err();
    assert!(err.contains("another port"), "{err}");

    let mut c = Config::default();
    c.apply_env(&|n| (n == "MYGRAPH_OAUTH_LISTEN").then(|| "0.0.0.0:7421".to_owned()))
        .unwrap();
    assert_eq!(c.oauth.listen, Some("0.0.0.0:7421".parse().unwrap()));
}
