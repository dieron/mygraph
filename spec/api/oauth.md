# OAuth for remote MCP clients

Status: **Draft** · spec 0.1.0-draft

How a server lets remote MCP clients (claude.ai on the web and on phones, Claude Desktop's
connectors, Claude Code) sign in with OAuth 2.1 and then call `/mcp`. The server is its own
authorization server, following the MCP authorization specification (2025-06-18 and 2025-11-25):
Protected Resource Metadata (RFC 9728), Authorization Server Metadata (RFC 8414), client ID
metadata documents or Dynamic Client Registration (RFC 7591), the authorization code grant with
PKCE (RFC 7636), and token revocation (RFC 7009).

A person signs in with one of the server's API keys. The client then acts as that key: the
same actor, role and graphs ([API-AUTH-1], [API-ACCESS-2]), or read-only if they choose.

OAuth is on when the server is configured with its public URL (`[oauth] public_url`, the address
clients reach it at, which may be a reverse proxy's). It needs API keys, since people sign in
with them. Below, `{base}` is that URL's origin.

---

## 1. Discovery

**[API-OAUTH-1]** A request to `/mcp` without valid credentials answers `401` with
`WWW-Authenticate: Bearer resource_metadata="{base}/.well-known/oauth-protected-resource"`.
`GET /.well-known/oauth-protected-resource` (and the same with `/mcp` appended) answers
`{"resource": "{base}/mcp", "authorization_servers": ["{base}"], "scopes_supported": ["mcp"],
"bearer_methods_supported": ["header"]}`.

**[API-OAUTH-2]** `GET /.well-known/oauth-authorization-server` answers the server's metadata:
`issuer` `{base}`; `authorization_endpoint`, `token_endpoint`, `registration_endpoint` and
`revocation_endpoint` under `{base}/oauth/`; `response_types_supported` `["code"]`;
`grant_types_supported` `["authorization_code", "refresh_token"]`;
`code_challenge_methods_supported` `["S256"]`; `token_endpoint_auth_methods_supported`
`["none"]` (public clients only); `scopes_supported` `["mcp", "offline_access"]`; and
`client_id_metadata_document_supported`. Without `[oauth]`, these routes answer `404`.

## 2. Clients

**[API-OAUTH-3]** Redirect URIs are limited to the server's allowlist (`oauth.redirect_uris`;
by default Claude's `https://claude.ai/api/mcp/auth_callback` and
`https://claude.com/api/mcp/auth_callback`, and `http://localhost/*` and `http://127.0.0.1/*` for
local clients). An entry ending in `/*` matches any port and path on that loopback host; any
other entry matches exactly. `POST /oauth/register` (JSON) registers a public client with 1 to
10 allowed `redirect_uris` and an optional `client_name`, and answers `201` with its
`client_id`. A redirect URI outside the allowlist fails with `invalid_redirect_uri`. A
registered client with no live grant is removed a day after it was last used. Registrations and
fetches of metadata documents ([API-OAUTH-4]) are limited to 60 a minute for the whole server.

**[API-OAUTH-4]** A `client_id` that is an `https://` URL with a path names the client's metadata
document. The server fetches it (from public addresses only, 5 s, 64 KB at most, no redirects,
kept for an hour). The document's `client_id` must equal its URL, it must list `redirect_uris`,
and its `token_endpoint_auth_method` must be absent or `none`.

## 3. Signing in

**[API-OAUTH-5]** `GET /oauth/authorize` takes `response_type=code`, `client_id`,
`redirect_uri` (optional when the client has only one), `code_challenge` with
`code_challenge_method=S256`, and optionally `state`, `scope` and `resource`, which must be
`{base}/mcp` when given.

Until the client and the redirect URI check out (registered or listed, and allowed), errors are
shown on the page and the browser is not redirected. Later errors go back to the redirect URI
as `error`, `error_description`, `state` and `iss`.

The page names the client and the host the browser will go back to, and asks for an API key,
with an option to sign in read-only. It is never framed, and it is never cached.

Submitting it (`POST /oauth/authorize`, form-encoded) checks the request again:
- **Allow** with a configured key redirects with a one-time `code` (valid 60 s), `state` and
  `iss`.
- **Deny** redirects with `error=access_denied`.
- A wrong key stays on the page. After 10 failures in a minute, the page refuses sign-ins until
  the minute has passed.

## 4. Tokens

**[API-OAUTH-6]** `POST /oauth/token` (form-encoded), `grant_type=authorization_code`, takes
`code`, `redirect_uri` (as in the request), `code_verifier` (PKCE S256) and optionally
`client_id` and `resource`. It answers `{"access_token", "token_type": "Bearer", "expires_in",
"refresh_token", "scope"}` with `Cache-Control: no-store`. Access tokens last
`oauth.access_token_minutes` (default 60), refresh tokens `oauth.refresh_token_days` (default 30).

An access token is accepted on `/mcp` only. It acts as the key it was signed in with, as that
key is configured now, and with at most the `agent-read` role if read-only was chosen. It stops
working when its grant is revoked or the key is removed from the configuration. Errors follow
RFC 6749 §5.2: `invalid_request`, `invalid_grant`, `invalid_target`,
`unsupported_grant_type`.

**[API-OAUTH-7]** `grant_type=refresh_token` takes `refresh_token` and optionally `client_id`,
and answers a new access token and a new refresh token: refresh tokens rotate.

A refresh token used again within 2 minutes of its rotation is treated as a retry and answered
again. Used later, it revokes the whole grant, since someone may hold a copy, and fails with
`invalid_grant`. So does a refresh token that expired or whose grant was revoked: the client
must sign in again.

**[API-OAUTH-8]** `POST /oauth/revoke` (form-encoded, RFC 7009) takes `token`. A refresh token
revokes its grant; an access token stops working by itself. It answers `200` whether or not the
token was known.

Admins list and revoke grants with `/admin/oauth` ([ADMIN-OAUTH-1], [ADMIN-OAUTH-2]).

## 5. The public listener

**[API-OAUTH-9]** With `oauth.listen` set (it needs `public_url`, and a port other than
`listen`'s), the server also listens there, with the same certificate, for a reverse proxy to
point at. That listener serves `/mcp`, `/oauth/*` and `/.well-known/*` as above, and answers
every other path `404` with an empty body: the REST and admin APIs, the pages, `/health`,
`/ready`, `/metrics` and `/openapi.yaml`. API keys still work on its `/mcp`.
