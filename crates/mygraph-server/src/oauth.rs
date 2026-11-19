//! OAuth 2.1 for remote MCP clients (spec/api/oauth.md): the server is its own authorization
//! server. A person signs in on its page with one of the server's API keys; the client then gets
//! tokens that call `/mcp` as that key's actor, with its role and graphs (read-only if they chose
//! so), until the grant is revoked or the key is removed from the configuration.
//!
//! Clients are identified by a metadata document URL (CIMD) or register themselves (DCR, RFC
//! 7591). Codes need PKCE with S256. Refresh tokens rotate, and a used one coming back revokes
//! the grant ([API-OAUTH-7]).

// Failures here are ready answers (OAuth error JSON, a page or a redirect), returned as is.
#![allow(clippy::result_large_err)]

use crate::App;
use crate::auth::{Caller, Keys};
use crate::config::{OauthConfig, Role};
use crate::registry::now_ms;
use crate::server_db::ServerDb;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mygraph_core::store::sha256_hex;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use url::Url;

/// How long an authorization code may wait for the token request.
const CODE_TTL: Duration = Duration::from_secs(60);
/// A refresh token used again this soon after its rotation is a retry, not a theft: the client
/// may have lost the answer. Later, it revokes the grant.
const REUSE_GRACE_MS: i64 = 120_000;
/// Failed sign-ins allowed per minute, for the whole server.
const MAX_FAILURES: usize = 10;
/// Client registrations allowed per minute, for the whole server.
const MAX_REGISTRATIONS: usize = 60;
const DOCUMENT_TTL: Duration = Duration::from_secs(3600);
const DOCUMENT_MAX_BYTES: u64 = 64 << 10;
const SCOPES: [&str; 2] = ["mcp", "offline_access"];

pub struct OAuth {
    base: String,
    config: OauthConfig,
    db: Arc<ServerDb>,
    codes: Mutex<HashMap<String, Code>>,
    documents: Mutex<HashMap<String, (Instant, Client)>>,
    failures: Mutex<VecDeque<Instant>>,
    registrations: Mutex<VecDeque<Instant>>,
}

#[derive(Clone, Debug, PartialEq)]
struct Client {
    id: String,
    name: String,
    redirect_uris: Vec<String>,
    /// Identified by its metadata document (CIMD), not registered here.
    document: bool,
}

struct Code {
    client_id: String,
    client_name: String,
    redirect_uri: String,
    challenge: String,
    scope: String,
    key_hash: String,
    read_only: bool,
    expires: Instant,
}

/// A checked authorization request.
struct AuthRequest {
    client: Client,
    redirect_uri: String,
    state: Option<String>,
    challenge: String,
    scope: String,
    resource: Option<String>,
}

/// Why an authorization request was refused: on the page itself while the redirect URI is not
/// yet trusted, else back at the client.
enum Refusal {
    Page(String),
    Redirect {
        uri: String,
        state: Option<String>,
        error: &'static str,
        description: String,
    },
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system's random source works");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// PKCE S256 (RFC 7636 §4.6).
fn pkce_ok(verifier: &str, challenge: &str) -> bool {
    let ok_chars = verifier
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
    (43..=128).contains(&verifier.len())
        && ok_chars
        && URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == challenge
}

fn loopback(u: &Url) -> bool {
    u.scheme() == "http" && matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

/// A requested redirect URI against a registered one: the same, or for a loopback address the
/// same but for the port, which a local client picks when it starts (RFC 8252 §7.3).
fn redirect_matches(registered: &str, requested: &str) -> bool {
    if registered == requested {
        return true;
    }
    match (Url::parse(registered), Url::parse(requested)) {
        (Ok(a), Ok(b)) => {
            loopback(&a)
                && loopback(&b)
                && a.host_str() == b.host_str()
                && a.path() == b.path()
                && a.query() == b.query()
        }
        _ => false,
    }
}

/// [API-OAUTH-3] A redirect URI against the server's `oauth.redirect_uris`.
fn allowed_redirect(allowlist: &[String], uri: &str) -> bool {
    let Ok(u) = Url::parse(uri) else {
        return false;
    };
    if u.fragment().is_some() || !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    allowlist
        .iter()
        .any(|entry| match entry.strip_suffix("/*") {
            Some(pattern) => {
                Url::parse(pattern).is_ok_and(|p| loopback(&u) && u.host_str() == p.host_str())
            }
            None => entry == uri,
        })
}

/// Whether an address may be fetched for a client metadata document: not this machine, the
/// local network or any other address the public internet cannot route to.
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (64..128).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 198 && (18..20).contains(&b)))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(v4));
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8))
        }
    }
}

/// Resolves names to public addresses only, so a metadata URL cannot reach inside the network,
/// whatever its DNS answers later.
struct PublicOnly;

impl ureq::Resolver for PublicOnly {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        let addrs: Vec<SocketAddr> = netloc
            .to_socket_addrs()?
            .filter(|a| public_ip(a.ip()))
            .collect();
        if addrs.is_empty() {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{netloc} has no public address"),
            ))
        } else {
            Ok(addrs)
        }
    }
}

/// [API-OAUTH-4] A client from its metadata document fetched from `url`.
fn client_from_document(url: &str, doc: &Json) -> Result<Client, String> {
    if doc["client_id"].as_str() != Some(url) {
        return Err("its client_id is not the URL it was fetched from".into());
    }
    let redirect_uris: Vec<String> = doc["redirect_uris"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|u| u.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if redirect_uris.is_empty() {
        return Err("it lists no redirect_uris".into());
    }
    match doc["token_endpoint_auth_method"].as_str() {
        None | Some("none") => {}
        Some(m) => {
            return Err(format!(
                "token_endpoint_auth_method {m}: this server takes public clients only"
            ));
        }
    }
    let host = Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default();
    let name = doc["client_name"]
        .as_str()
        .filter(|n| !n.trim().is_empty())
        .map(|n| n.chars().take(200).collect())
        .unwrap_or(host);
    Ok(Client {
        id: url.to_owned(),
        name,
        redirect_uris,
        document: true,
    })
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn with_params(uri: &str, pairs: &[(&str, &str)]) -> String {
    match Url::parse(uri) {
        Ok(mut u) => {
            {
                let mut q = u.query_pairs_mut();
                for (k, v) in pairs {
                    q.append_pair(k, v);
                }
            }
            u.to_string()
        }
        Err(_) => uri.to_owned(),
    }
}

fn no_store(mut r: Response) -> Response {
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    r
}

fn json_answer(status: StatusCode, body: &Json) -> Response {
    no_store((status, crate::api::json_body(body)).into_response())
}

/// An OAuth error answer (RFC 6749 §5.2).
fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    json_answer(
        status,
        &json!({ "error": error, "error_description": description }),
    )
}

fn redirect(to: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    no_store(r)
}

fn form(body: &[u8]) -> HashMap<String, String> {
    url::form_urlencoded::parse(body).into_owned().collect()
}

/// Keeps the events of the last minute and says whether another one is allowed.
fn under_limit(events: &Mutex<VecDeque<Instant>>, max: usize) -> bool {
    let mut q = lock(events);
    while q
        .front()
        .is_some_and(|t| t.elapsed() > Duration::from_secs(60))
    {
        q.pop_front();
    }
    q.len() < max
}

fn note(events: &Mutex<VecDeque<Instant>>) {
    lock(events).push_back(Instant::now());
}

impl OAuth {
    /// The authorization server, when `[oauth]` sets a public URL.
    pub fn new(config: &OauthConfig, db: Arc<ServerDb>) -> Option<Self> {
        Some(Self {
            base: config.base()?,
            config: config.clone(),
            db,
            codes: Mutex::default(),
            documents: Mutex::default(),
            failures: Mutex::default(),
            registrations: Mutex::default(),
        })
    }

    /// The protected resource: the MCP endpoint's public URL.
    pub fn resource(&self) -> String {
        format!("{}/mcp", self.base)
    }

    /// [API-OAUTH-1] The `WWW-Authenticate` of a 401 from `/mcp`.
    pub fn challenge(&self) -> String {
        format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
            self.base
        )
    }

    fn resource_ok(&self, resource: Option<&String>) -> bool {
        resource.is_none_or(|r| r.trim_end_matches('/') == self.resource())
    }

    fn protected_resource(&self) -> Json {
        json!({
            "resource": self.resource(),
            "authorization_servers": [self.base],
            "scopes_supported": ["mcp"],
            "bearer_methods_supported": ["header"],
            "resource_name": "MyGraph",
        })
    }

    fn server_metadata(&self) -> Json {
        let b = &self.base;
        json!({
            "issuer": b,
            "authorization_endpoint": format!("{b}/oauth/authorize"),
            "token_endpoint": format!("{b}/oauth/token"),
            "registration_endpoint": format!("{b}/oauth/register"),
            "revocation_endpoint": format!("{b}/oauth/revoke"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "revocation_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": SCOPES,
            "client_id_metadata_document_supported": self.config.client_metadata_documents,
            "authorization_response_iss_parameter_supported": true,
        })
    }

    // -----------------------------------------------------------------------------------------
    // Clients

    fn registered_client(&self, id: &str) -> Option<Client> {
        let conn = self.db.conn();
        conn.query_row(
            "SELECT name, redirect_uris FROM oauth_clients WHERE id = ?1",
            params![id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .ok()
        .flatten()
        .map(|(name, uris)| Client {
            id: id.to_owned(),
            name,
            redirect_uris: serde_json::from_str(&uris).unwrap_or_default(),
            document: false,
        })
    }

    /// Fetches a client metadata document (blocking).
    fn fetch_document(&self, id: &str) -> Result<Client, String> {
        if let Some((at, c)) = lock(&self.documents).get(id)
            && at.elapsed() < DOCUMENT_TTL
        {
            return Ok(c.clone());
        }
        // The sign-in page needs no key, so fetches share the registrations' limit.
        if !under_limit(&self.registrations, MAX_REGISTRATIONS) {
            return Err("too many new clients; try again in a minute".into());
        }
        note(&self.registrations);
        let url = Url::parse(id).map_err(|e| e.to_string())?;
        if url.scheme() != "https"
            || url.path() == "/"
            || url.fragment().is_some()
            || !url.username().is_empty()
        {
            return Err("a client metadata URL is an https:// URL with a path".into());
        }
        let agent = crate::tls::agent_builder(None)?
            .resolver(PublicOnly)
            .timeout(Duration::from_secs(5))
            .redirects(0)
            .build();
        let resp = agent
            .get(id)
            .set("accept", "application/json")
            .call()
            .map_err(|e| format!("fetching it failed: {e}"))?;
        let mut body = Vec::new();
        resp.into_reader()
            .take(DOCUMENT_MAX_BYTES + 1)
            .read_to_end(&mut body)
            .map_err(|e| format!("reading it failed: {e}"))?;
        if body.len() as u64 > DOCUMENT_MAX_BYTES {
            return Err("it is larger than 64 KB".into());
        }
        let doc: Json =
            serde_json::from_slice(&body).map_err(|e| format!("it is not JSON: {e}"))?;
        let client = client_from_document(id, &doc)?;
        let mut docs = lock(&self.documents);
        docs.retain(|_, (at, _)| at.elapsed() < DOCUMENT_TTL);
        docs.insert(id.to_owned(), (Instant::now(), client.clone()));
        Ok(client)
    }

    async fn client(self: &Arc<Self>, id: &str) -> Result<Client, String> {
        if id.starts_with("https://") && self.config.client_metadata_documents {
            let (me, id) = (self.clone(), id.to_owned());
            return tokio::task::spawn_blocking(move || me.fetch_document(&id))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("the client's metadata document: {e}"));
        }
        self.registered_client(id)
            .ok_or_else(|| "this client is not registered here; add the connector again".into())
    }

    // -----------------------------------------------------------------------------------------
    // Authorization requests

    async fn check_request(
        self: &Arc<Self>,
        q: &HashMap<String, String>,
    ) -> Result<AuthRequest, Refusal> {
        let client_id = q
            .get("client_id")
            .filter(|c| !c.is_empty())
            .ok_or_else(|| Refusal::Page("The request names no client_id.".into()))?;
        let client = self
            .client(client_id)
            .await
            .map_err(|e| Refusal::Page(format!("Unknown client: {e}.")))?;
        let redirect_uri = match q.get("redirect_uri") {
            Some(r) => r.clone(),
            None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
            None => return Err(Refusal::Page("The request names no redirect_uri.".into())),
        };
        if !client
            .redirect_uris
            .iter()
            .any(|r| redirect_matches(r, &redirect_uri))
        {
            return Err(Refusal::Page(format!(
                "The redirect URI {redirect_uri} is not one this client registered."
            )));
        }
        if !allowed_redirect(&self.config.redirect_uris, &redirect_uri) {
            return Err(Refusal::Page(format!(
                "This server does not send sign-ins to {redirect_uri}. An admin can add it to oauth.redirect_uris."
            )));
        }
        let state = q.get("state").cloned();
        let refuse = |error, description: &str| Refusal::Redirect {
            uri: redirect_uri.clone(),
            state: state.clone(),
            error,
            description: description.to_owned(),
        };
        if q.get("response_type").map(String::as_str) != Some("code") {
            return Err(refuse(
                "unsupported_response_type",
                "only response_type=code is supported",
            ));
        }
        let challenge = q.get("code_challenge").cloned().unwrap_or_default();
        if challenge.len() < 43
            || q.get("code_challenge_method").map(String::as_str) != Some("S256")
        {
            return Err(refuse(
                "invalid_request",
                "PKCE is required: code_challenge with code_challenge_method=S256",
            ));
        }
        if !self.resource_ok(q.get("resource")) {
            return Err(refuse(
                "invalid_target",
                &format!("the resource here is {}", self.resource()),
            ));
        }
        let asked = q.get("scope").map(String::as_str).unwrap_or("");
        let mut scope: Vec<&str> = SCOPES
            .into_iter()
            .filter(|s| asked.split(' ').any(|a| a == *s))
            .collect();
        if !scope.contains(&"mcp") {
            scope.insert(0, "mcp");
        }
        Ok(AuthRequest {
            client,
            redirect_uri,
            state,
            challenge,
            scope: scope.join(" "),
            resource: q.get("resource").cloned(),
        })
    }

    fn refuse(&self, refusal: Refusal) -> Response {
        match refusal {
            Refusal::Page(message) => self.error_page(&message),
            Refusal::Redirect {
                uri,
                state,
                error,
                description,
            } => {
                let mut pairs = vec![
                    ("error", error),
                    ("error_description", description.as_str()),
                    ("iss", self.base.as_str()),
                ];
                if let Some(s) = &state {
                    pairs.push(("state", s));
                }
                redirect(&with_params(&uri, &pairs))
            }
        }
    }

    fn html(&self, status: StatusCode, body: &str, form_target: Option<&str>) -> Response {
        let form_action = match form_target.and_then(|t| Url::parse(t).ok()) {
            Some(u) => format!("'self' {}", u.origin().ascii_serialization()),
            None => "'self'".to_owned(),
        };
        let csp = format!(
            "default-src 'none'; style-src 'unsafe-inline'; form-action {form_action}; frame-ancestors 'none'; base-uri 'none'"
        );
        let page = format!(
            r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Sign in · MyGraph</title><style>{STYLE}</style></head>
<body><main>{body}</main></body></html>"#
        );
        let mut r = (status, page).into_response();
        let h = r.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        h.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
        if let Ok(v) = HeaderValue::from_str(&csp) {
            h.insert(header::CONTENT_SECURITY_POLICY, v);
        }
        no_store(r)
    }

    fn error_page(&self, message: &str) -> Response {
        self.html(
            StatusCode::BAD_REQUEST,
            &format!(
                "<h1>MyGraph can't sign you in</h1><p class=\"error\">{}</p>\
                 <p class=\"muted\">Go back to the app you came from and try connecting again.</p>",
                html_escape(message)
            ),
            None,
        )
    }

    /// [API-OAUTH-5] The sign-in page.
    fn sign_in_page(&self, req: &AuthRequest, status: StatusCode, error: Option<&str>) -> Response {
        let back = Url::parse(&req.redirect_uri)
            .ok()
            .and_then(|u| {
                u.host_str()
                    .map(|h| u.port().map_or(h.to_owned(), |p| format!("{h}:{p}")))
            })
            .unwrap_or_default();
        let who = if req.client.document {
            let host = Url::parse(&req.client.id)
                .ok()
                .and_then(|u| u.host_str().map(String::from))
                .unwrap_or_default();
            format!(
                "<b>{}</b> <span class=\"muted\">(identified by {})</span>",
                html_escape(&req.client.name),
                html_escape(&host)
            )
        } else {
            format!(
                "an app that calls itself <b>{}</b>",
                html_escape(&req.client.name)
            )
        };
        let hidden = [
            ("client_id", Some(req.client.id.as_str())),
            ("redirect_uri", Some(req.redirect_uri.as_str())),
            ("response_type", Some("code")),
            ("code_challenge", Some(req.challenge.as_str())),
            ("code_challenge_method", Some("S256")),
            ("scope", Some(req.scope.as_str())),
            ("state", req.state.as_deref()),
            ("resource", req.resource.as_deref()),
        ]
        .into_iter()
        .filter_map(|(k, v)| {
            v.map(|v| {
                format!(
                    "<input type=\"hidden\" name=\"{k}\" value=\"{}\">",
                    html_escape(v)
                )
            })
        })
        .collect::<String>();
        let error = error
            .map(|e| format!("<p class=\"error\">{}</p>", html_escape(e)))
            .unwrap_or_default();
        let body = format!(
            r#"<h1>Connect to MyGraph</h1>
<p>{who} wants to use the knowledge graphs on <b>{server}</b>.</p>
<p>After you sign in, you go back to <b>{back}</b>. Continue only if you started this there.</p>
{error}
<form method="post" action="/oauth/authorize" autocomplete="off">{hidden}
<label for="key">API key</label>
<input id="key" name="key" type="password" autocomplete="off" autofocus>
<p class="muted">The app gets what this key may do: its role and its graphs. You can revoke it under /admin/oauth, and removing the key from the server's configuration signs the app out.</p>
<label class="check"><input type="checkbox" name="read_only" value="1"> Read only: the app may read the graphs but not change them</label>
<div class="buttons"><button name="action" value="allow" type="submit">Allow</button>
<button name="action" value="deny" type="submit" class="secondary">Deny</button></div>
</form>"#,
            server = html_escape(
                self.base
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
            ),
            back = html_escape(&back),
        );
        self.html(status, &body, Some(&req.redirect_uri))
    }

    // -----------------------------------------------------------------------------------------
    // Grants and tokens

    fn issue(&self, grant_id: &str) -> Result<Json, String> {
        let now = now_ms();
        let access = random_token();
        let refresh = random_token();
        let access_ms = self.config.access_token_minutes as i64 * 60_000;
        let refresh_ms = self.config.refresh_token_days as i64 * 86_400_000;
        let conn = self.db.conn();
        conn.execute(
            "INSERT INTO oauth_tokens (hash, grant_id, kind, expires) VALUES (?1, ?2, 'access', ?3), (?4, ?2, 'refresh', ?5)",
            params![
                sha256_hex(access.as_bytes()),
                grant_id,
                now + access_ms,
                sha256_hex(refresh.as_bytes()),
                now + refresh_ms
            ],
        )
        .map_err(|e| e.to_string())?;
        let scope: String = conn
            .query_row(
                "SELECT scope FROM oauth_grants WHERE id = ?1",
                params![grant_id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "mcp".into());
        Ok(json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": access_ms / 1000,
            "refresh_token": refresh,
            "scope": scope,
        }))
    }

    fn revoke_grant_now(&self, grant_id: &str) {
        let conn = self.db.conn();
        let _ = conn.execute(
            "UPDATE oauth_grants SET revoked = ?2 WHERE id = ?1 AND revoked IS NULL",
            params![grant_id, now_ms()],
        );
        let _ = conn.execute(
            "DELETE FROM oauth_tokens WHERE grant_id = ?1",
            params![grant_id],
        );
    }

    fn exchange_code(&self, keys: &Keys, f: &HashMap<String, String>) -> Result<Json, Response> {
        let bad = |error: &str, d: &str| oauth_error(StatusCode::BAD_REQUEST, error, d);
        let code = f
            .get("code")
            .ok_or_else(|| bad("invalid_request", "code is missing"))?;
        let code = lock(&self.codes)
            .remove(&sha256_hex(code.as_bytes()))
            .ok_or_else(|| bad("invalid_grant", "the code is unknown or already used"))?;
        if code.expires < Instant::now() {
            return Err(bad("invalid_grant", "the code expired"));
        }
        if f.get("client_id").is_some_and(|c| *c != code.client_id) {
            return Err(bad(
                "invalid_grant",
                "the code was issued to another client",
            ));
        }
        if f.get("redirect_uri") != Some(&code.redirect_uri) {
            return Err(bad(
                "invalid_grant",
                "redirect_uri differs from the authorization request",
            ));
        }
        if !f
            .get("code_verifier")
            .is_some_and(|v| pkce_ok(v, &code.challenge))
        {
            return Err(bad(
                "invalid_grant",
                "code_verifier does not match code_challenge",
            ));
        }
        if !self.resource_ok(f.get("resource")) {
            return Err(bad(
                "invalid_target",
                &format!("the resource here is {}", self.resource()),
            ));
        }
        if keys.by_hash(&code.key_hash).is_none() {
            return Err(bad("invalid_grant", "the key used to sign in was removed"));
        }
        let grant_id = random_token();
        let now = now_ms();
        self.db
            .conn()
            .execute(
                "INSERT INTO oauth_grants (id, client_id, client_name, key_sha256, read_only, scope, created, last_used)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![grant_id, code.client_id, code.client_name, code.key_hash, code.read_only, code.scope, now],
            )
            .map_err(|e| oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e.to_string()))?;
        let _ = self.db.conn().execute(
            "UPDATE oauth_clients SET last_used = ?2 WHERE id = ?1",
            params![code.client_id, now],
        );
        self.issue(&grant_id)
            .map_err(|e| oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e))
    }

    fn refresh(&self, keys: &Keys, f: &HashMap<String, String>) -> Result<Json, Response> {
        let bad = |d: &str| oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", d);
        let token = f.get("refresh_token").ok_or_else(|| {
            oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "refresh_token is missing",
            )
        })?;
        let hash = sha256_hex(token.as_bytes());
        let row = self
            .db
            .conn()
            .query_row(
                "SELECT t.grant_id, t.expires, t.used, g.client_id, g.key_sha256, g.revoked
                 FROM oauth_tokens t JOIN oauth_grants g ON g.id = t.grant_id
                 WHERE t.hash = ?1 AND t.kind = 'refresh'",
                params![hash],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()
            .ok()
            .flatten();
        let Some((grant_id, expires, used, client_id, key_hash, revoked)) = row else {
            return Err(bad(
                "the refresh token is unknown or was revoked; sign in again",
            ));
        };
        let now = now_ms();
        if revoked.is_some() || expires < now {
            return Err(bad(
                "the refresh token expired or was revoked; sign in again",
            ));
        }
        if f.get("client_id").is_some_and(|c| *c != client_id) {
            return Err(bad("the refresh token was issued to another client"));
        }
        if let Some(at) = used
            && now - at > REUSE_GRACE_MS
        {
            // [API-OAUTH-7] An old refresh token came back: someone may hold a copy.
            tracing::warn!(grant = %&grant_id[..8], "OAuth refresh token reused; grant revoked");
            self.revoke_grant_now(&grant_id);
            return Err(bad("the refresh token was already used; sign in again"));
        }
        if keys.by_hash(&key_hash).is_none() {
            self.revoke_grant_now(&grant_id);
            return Err(bad("the key used to sign in was removed; sign in again"));
        }
        {
            let conn = self.db.conn();
            let _ = conn.execute(
                "UPDATE oauth_tokens SET used = ?2 WHERE hash = ?1 AND used IS NULL",
                params![hash, now],
            );
            let _ = conn.execute(
                "UPDATE oauth_grants SET last_used = ?2 WHERE id = ?1",
                params![grant_id, now],
            );
        }
        self.issue(&grant_id)
            .map_err(|e| oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e))
    }

    /// [API-OAUTH-6] The caller of an access token on `/mcp`: the actor, role and graphs of the
    /// key it was signed in with, as configured now, and read-only if chosen at sign-in.
    pub fn authenticate(&self, token: &str, keys: &Keys) -> Option<Caller> {
        let now = now_ms();
        let conn = self.db.conn();
        let (grant_id, key_hash, read_only, last_used) = conn
            .query_row(
                "SELECT g.id, g.key_sha256, g.read_only, g.last_used
                 FROM oauth_tokens t JOIN oauth_grants g ON g.id = t.grant_id
                 WHERE t.hash = ?1 AND t.kind = 'access' AND t.expires > ?2 AND g.revoked IS NULL",
                params![sha256_hex(token.as_bytes()), now],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, bool>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()
            .ok()
            .flatten()?;
        let mut caller = keys.by_hash(&key_hash)?;
        if read_only {
            caller.role = caller.role.min(Role::AgentRead);
        }
        if now - last_used > 60_000 {
            let _ = conn.execute(
                "UPDATE oauth_grants SET last_used = ?2 WHERE id = ?1",
                params![grant_id, now],
            );
        }
        Some(caller)
    }

    // -----------------------------------------------------------------------------------------
    // Admin and upkeep

    /// [ADMIN-OAUTH-1] `GET /admin/oauth`: live grants and the number of registered clients.
    pub fn admin_list(&self, keys: &Keys) -> Json {
        let conn = self.db.conn();
        let grants: Vec<Json> = conn
            .prepare(
                "SELECT id, client_id, client_name, key_sha256, read_only, scope, created, last_used
                 FROM oauth_grants WHERE revoked IS NULL ORDER BY last_used DESC",
            )
            .and_then(|mut s| {
                s.query_map([], |r| {
                    let key: String = r.get(3)?;
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "client_id": r.get::<_, String>(1)?,
                        "client_name": r.get::<_, String>(2)?,
                        "actor": keys.by_hash(&key).map(|c| c.actor),
                        "read_only": r.get::<_, bool>(4)?,
                        "scope": r.get::<_, String>(5)?,
                        "created": mygraph_core::model::time_json(r.get(6)?),
                        "last_used": mygraph_core::model::time_json(r.get(7)?),
                    }))
                })?
                .collect()
            })
            .unwrap_or_default();
        let clients: i64 = conn
            .query_row("SELECT COUNT(*) FROM oauth_clients", [], |r| r.get(0))
            .unwrap_or(0);
        json!({ "public_url": self.base, "grants": grants, "registered_clients": clients })
    }

    /// [ADMIN-OAUTH-2] Revokes a grant: its tokens stop working at once.
    pub fn revoke_grant(&self, id: &str) -> bool {
        let live: bool = self
            .db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM oauth_grants WHERE id = ?1 AND revoked IS NULL",
                params![id],
                |r| r.get::<_, i64>(0),
            )
            .is_ok_and(|n| n > 0);
        if live {
            self.revoke_grant_now(id);
        }
        live
    }

    /// Drops expired tokens and codes, grants whose key is gone or whose tokens all expired,
    /// and registered clients without a live grant a day after they were last used (Claude
    /// registers a new client for every connection, and anyone may register).
    pub fn prune(&self, keys: &Keys) {
        let now = now_ms();
        lock(&self.codes).retain(|_, c| c.expires > Instant::now());
        let conn = self.db.conn();
        let _ = conn.execute("DELETE FROM oauth_tokens WHERE expires < ?1", params![now]);
        let orphaned: Vec<String> = conn
            .prepare("SELECT id, key_sha256 FROM oauth_grants WHERE revoked IS NULL")
            .and_then(|mut s| {
                s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, key)| keys.by_hash(key).is_none())
            .map(|(id, _)| id)
            .collect();
        for id in orphaned {
            let _ = conn.execute(
                "UPDATE oauth_grants SET revoked = ?2 WHERE id = ?1",
                params![id, now],
            );
            let _ = conn.execute("DELETE FROM oauth_tokens WHERE grant_id = ?1", params![id]);
        }
        let _ = conn.execute(
            "DELETE FROM oauth_grants WHERE id NOT IN (SELECT grant_id FROM oauth_tokens)",
            [],
        );
        let _ = conn.execute(
            "DELETE FROM oauth_clients WHERE last_used < ?1
             AND id NOT IN (SELECT client_id FROM oauth_grants)",
            params![now - 86_400_000],
        );
    }
}

const STYLE: &str = "
:root{color-scheme:light dark;--bg:#f6f7f9;--card:#fff;--fg:#1d2330;--muted:#5d6675;--line:#d5d9e0;--accent:#2f6fde;--error:#b42318}
@media (prefers-color-scheme:dark){:root{--bg:#121520;--card:#1b2030;--fg:#e4e7ee;--muted:#9aa3b2;--line:#2c3346;--accent:#6c9bf0;--error:#f97066}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,-apple-system,Segoe UI,sans-serif}
main{max-width:30rem;margin:8vh auto;padding:1.5rem;background:var(--card);border:1px solid var(--line);border-radius:12px}
@media (max-width:34rem){main{margin:0;border-radius:0;border-width:0 0 1px}}
h1{font-size:1.3rem;margin:0 0 1rem}.muted{color:var(--muted);font-size:.9rem}.error{color:var(--error);font-weight:600}
label{display:block;font-weight:600;margin:1rem 0 .3rem}label.check{font-weight:400}
input[type=password]{width:100%;padding:.55rem .7rem;font:inherit;color:inherit;background:var(--bg);border:1px solid var(--line);border-radius:8px}
.buttons{display:flex;gap:.6rem;margin-top:1.2rem}button{font:inherit;padding:.5rem 1.2rem;border-radius:8px;border:1px solid var(--accent);background:var(--accent);color:#fff;cursor:pointer}
button.secondary{background:transparent;color:var(--fg);border-color:var(--line)}
";

// ---------------------------------------------------------------------------------------------
// Routes

fn server(app: &App) -> Result<Arc<OAuth>, Response> {
    app.oauth
        .clone()
        .ok_or_else(|| crate::api::api_not_found().into_response())
}

fn observed(app: &App, op: &str, start: Instant, r: Response) -> Response {
    crate::api::observe(app, op, None, r.status().as_u16(), start, None);
    r
}

/// `GET /.well-known/oauth-protected-resource[/mcp]` ([API-OAUTH-1], RFC 9728)
pub async fn protected_resource(State(app): State<Arc<App>>) -> Response {
    match server(&app) {
        Ok(o) => public_json(&o.protected_resource()),
        Err(r) => r,
    }
}

/// `GET /.well-known/oauth-authorization-server` ([API-OAUTH-2], RFC 8414)
pub async fn authorization_server(State(app): State<Arc<App>>) -> Response {
    match server(&app) {
        Ok(o) => public_json(&o.server_metadata()),
        Err(r) => r,
    }
}

fn public_json(body: &Json) -> Response {
    let mut r = crate::api::json_body(body).into_response();
    r.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    r
}

/// `POST /oauth/register` ([API-OAUTH-3], RFC 7591)
pub async fn register(State(app): State<Arc<App>>, body: Bytes) -> Response {
    let start = Instant::now();
    let o = match server(&app) {
        Ok(o) => o,
        Err(r) => return r,
    };
    let r = (|| {
        let invalid = |e: &str, d: String| Err(oauth_error(StatusCode::BAD_REQUEST, e, &d));
        if !under_limit(&o.registrations, MAX_REGISTRATIONS) {
            return Err(oauth_error(
                StatusCode::TOO_MANY_REQUESTS,
                "temporarily_unavailable",
                "too many registrations; try again in a minute",
            ));
        }
        if body.len() > 64 << 10 {
            return invalid(
                "invalid_client_metadata",
                "the request is larger than 64 KB".into(),
            );
        }
        let req: Json = match serde_json::from_slice(&body) {
            Ok(v @ Json::Object(_)) => v,
            _ => return invalid("invalid_client_metadata", "expected a JSON object".into()),
        };
        let uris: Vec<String> = match req["redirect_uris"].as_array() {
            Some(a) if !a.is_empty() && a.len() <= 10 && a.iter().all(Json::is_string) => a
                .iter()
                .filter_map(|u| u.as_str().map(String::from))
                .collect(),
            _ => {
                return invalid(
                    "invalid_redirect_uri",
                    "redirect_uris: expected 1 to 10 URLs".into(),
                );
            }
        };
        if let Some(u) = uris
            .iter()
            .find(|u| !allowed_redirect(&o.config.redirect_uris, u))
        {
            return invalid(
                "invalid_redirect_uri",
                format!(
                    "this server does not send sign-ins to {u}; an admin can add it to oauth.redirect_uris"
                ),
            );
        }
        let wrong = |field: &str, ok: &[&str]| {
            req[field].as_array().is_some_and(|a| {
                a.iter()
                    .any(|v| !v.as_str().is_some_and(|s| ok.contains(&s)))
            })
        };
        if wrong("grant_types", &["authorization_code", "refresh_token"])
            || wrong("response_types", &["code"])
        {
            return invalid(
                "invalid_client_metadata",
                "only the authorization_code and refresh_token grants, with response type code"
                    .into(),
            );
        }
        let name: String = req["client_name"]
            .as_str()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or("an unnamed app")
            .chars()
            .take(200)
            .collect();
        let id = random_token();
        let now = now_ms();
        o.db.conn()
            .execute(
                "INSERT INTO oauth_clients (id, name, redirect_uris, created, last_used) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![id, name, Json::from(uris.clone()).to_string(), now],
            )
            .map_err(|e| oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", &e.to_string()))?;
        note(&o.registrations);
        Ok(json_answer(
            StatusCode::CREATED,
            &json!({
                "client_id": id,
                "client_id_issued_at": now / 1000,
                "client_name": name,
                "redirect_uris": uris,
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none",
            }),
        ))
    })();
    observed(&app, "oauth.register", start, r.unwrap_or_else(|e| e))
}

/// `GET /oauth/authorize`: the sign-in page ([API-OAUTH-5]).
pub async fn authorize_page(
    State(app): State<Arc<App>>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let start = Instant::now();
    let o = match server(&app) {
        Ok(o) => o,
        Err(r) => return r,
    };
    let r = match o.check_request(&q).await {
        Ok(req) => o.sign_in_page(&req, StatusCode::OK, None),
        Err(refusal) => o.refuse(refusal),
    };
    observed(&app, "oauth.authorize", start, r)
}

/// `POST /oauth/authorize`: the sign-in form ([API-OAUTH-5]).
pub async fn authorize(State(app): State<Arc<App>>, body: Bytes) -> Response {
    let start = Instant::now();
    let o = match server(&app) {
        Ok(o) => o,
        Err(r) => return r,
    };
    let f = form(&body);
    let req = match o.check_request(&f).await {
        Ok(req) => req,
        Err(refusal) => return observed(&app, "oauth.sign_in", start, o.refuse(refusal)),
    };
    let back = |pairs: &[(&str, &str)]| {
        let mut pairs = pairs.to_vec();
        pairs.push(("iss", o.base.as_str()));
        if let Some(s) = &req.state {
            pairs.push(("state", s));
        }
        redirect(&with_params(&req.redirect_uri, &pairs))
    };
    if f.get("action").map(String::as_str) != Some("allow") {
        let r = back(&[
            ("error", "access_denied"),
            ("error_description", "the person signing in said no"),
        ]);
        return observed(&app, "oauth.sign_in", start, r);
    }
    if !under_limit(&o.failures, MAX_FAILURES) {
        let r = o.sign_in_page(
            &req,
            StatusCode::TOO_MANY_REQUESTS,
            Some("Too many failed sign-ins. Wait a minute and try again."),
        );
        return observed(&app, "oauth.sign_in", start, r);
    }
    let key = f.get("key").map(|k| k.trim()).unwrap_or("");
    let Some((key_hash, caller)) = app.keys.lookup(key) else {
        note(&o.failures);
        tracing::warn!(client = %req.client.name, "OAuth sign-in with a wrong API key");
        tokio::time::sleep(Duration::from_secs(1)).await;
        let r = o.sign_in_page(
            &req,
            StatusCode::UNAUTHORIZED,
            Some("That key is not valid on this server."),
        );
        return observed(&app, "oauth.sign_in", start, r);
    };
    let code = random_token();
    let read_only = f.contains_key("read_only");
    {
        let mut codes = lock(&o.codes);
        codes.retain(|_, c| c.expires > Instant::now());
        codes.insert(
            sha256_hex(code.as_bytes()),
            Code {
                client_id: req.client.id.clone(),
                client_name: req.client.name.clone(),
                redirect_uri: req.redirect_uri.clone(),
                challenge: req.challenge.clone(),
                scope: req.scope.clone(),
                key_hash,
                read_only,
                expires: Instant::now() + CODE_TTL,
            },
        );
    }
    tracing::info!(actor = %caller.actor, client = %req.client.name, read_only, "OAuth sign-in");
    observed(&app, "oauth.sign_in", start, back(&[("code", &code)]))
}

/// `POST /oauth/token` ([API-OAUTH-6], [API-OAUTH-7])
pub async fn token(State(app): State<Arc<App>>, body: Bytes) -> Response {
    let start = Instant::now();
    let o = match server(&app) {
        Ok(o) => o,
        Err(r) => return r,
    };
    let f = form(&body);
    let r = match f.get("grant_type").map(String::as_str) {
        Some("authorization_code") => o.exchange_code(&app.keys, &f),
        Some("refresh_token") => o.refresh(&app.keys, &f),
        _ => Err(oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "grant_type: expected authorization_code or refresh_token",
        )),
    };
    let r = match r {
        Ok(body) => json_answer(StatusCode::OK, &body),
        Err(e) => e,
    };
    observed(&app, "oauth.token", start, r)
}

/// `POST /oauth/revoke` (RFC 7009): a refresh token revokes its grant, an access token itself.
pub async fn revoke(State(app): State<Arc<App>>, body: Bytes) -> Response {
    let start = Instant::now();
    let o = match server(&app) {
        Ok(o) => o,
        Err(r) => return r,
    };
    if let Some(t) = form(&body).get("token") {
        let hash = sha256_hex(t.as_bytes());
        let found =
            o.db.conn()
                .query_row(
                    "SELECT grant_id, kind FROM oauth_tokens WHERE hash = ?1",
                    params![hash],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
                .ok()
                .flatten();
        match found {
            Some((grant, kind)) if kind == "refresh" => o.revoke_grant_now(&grant),
            Some(_) => {
                let _ =
                    o.db.conn()
                        .execute("DELETE FROM oauth_tokens WHERE hash = ?1", params![hash]);
            }
            None => {}
        }
    }
    observed(
        &app,
        "oauth.revoke",
        start,
        no_store(StatusCode::OK.into_response()),
    )
}

/// Prunes expired OAuth state every hour.
pub fn spawn_prune(app: Arc<App>) {
    if app.oauth.is_none() {
        return;
    }
    tokio::spawn(async move {
        loop {
            if let Some(o) = &app.oauth {
                let (o, a) = (o.clone(), app.clone());
                let _ = tokio::task::spawn_blocking(move || o.prune(&a.keys)).await;
            }
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256() {
        // RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(pkce_ok(verifier, challenge));
        assert!(!pkce_ok(
            verifier,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cX"
        ));
        assert!(!pkce_ok("short", challenge));
    }

    #[test]
    fn redirect_uris() {
        let allow: Vec<String> = crate::config::DEFAULT_REDIRECT_URIS
            .map(String::from)
            .to_vec();
        assert!(allowed_redirect(
            &allow,
            "https://claude.ai/api/mcp/auth_callback"
        ));
        assert!(allowed_redirect(&allow, "http://localhost:3118/callback"));
        assert!(allowed_redirect(&allow, "http://127.0.0.1/cb"));
        assert!(!allowed_redirect(
            &allow,
            "https://claude.ai/api/mcp/auth_callback/x"
        ));
        assert!(!allowed_redirect(
            &allow,
            "https://evil.example/api/mcp/auth_callback"
        ));
        assert!(!allowed_redirect(&allow, "https://localhost/callback"));
        assert!(!allowed_redirect(
            &allow,
            "http://localhost.evil.example/callback"
        ));
        assert!(!allowed_redirect(&allow, "http://user@localhost/callback"));

        assert!(redirect_matches(
            "http://localhost/callback",
            "http://localhost:51234/callback"
        ));
        assert!(!redirect_matches(
            "http://localhost/callback",
            "http://localhost:51234/other"
        ));
        assert!(!redirect_matches(
            "http://localhost/callback",
            "http://127.0.0.1:5/callback"
        ));
        assert!(!redirect_matches(
            "https://claude.ai/api/mcp/auth_callback",
            "https://claude.ai:8443/api/mcp/auth_callback"
        ));
    }

    #[test]
    fn only_public_addresses_are_fetched() {
        for ip in [
            "10.1.2.3",
            "127.0.0.1",
            "192.168.0.10",
            "172.16.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.0.10",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["160.79.104.10", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(public_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn metadata_documents() {
        let url = "https://app.example/oauth/client.json";
        let ok = json!({"client_id": url, "client_name": "App", "redirect_uris": ["http://localhost/callback"]});
        let c = client_from_document(url, &ok).unwrap();
        assert_eq!((c.name.as_str(), c.document), ("App", true));
        let mut other = ok.clone();
        other["client_id"] = json!("https://evil.example/x");
        assert!(client_from_document(url, &other).is_err());
        let mut secret = ok.clone();
        secret["token_endpoint_auth_method"] = json!("client_secret_basic");
        assert!(client_from_document(url, &secret).is_err());
        assert!(client_from_document(url, &json!({"client_id": url})).is_err());
    }

    #[test]
    fn page_text_is_escaped() {
        assert_eq!(
            html_escape(r#"<b a="x">&'"#),
            "&lt;b a=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
