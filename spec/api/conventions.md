# REST API conventions

Status: **Draft** · spec 0.1.0-draft · design: [docs/05-architecture.md §8–§9](../../docs/05-architecture.md),
[docs/06-agent-api.md](../../docs/06-agent-api.md)

The HTTP transport of the agent API. Every operation in [operations/](operations/) is reachable
with its JSON request and response unchanged; resource-style `GET` and `DELETE` routes are
shorthands that map onto those operations. Errors are in [errors.md](errors.md), the admin API in
[admin.md](admin.md), and [openapi.yaml](openapi.yaml) describes every route.

---

## 1. Transport

**[API-HTTP-1]** HTTP/1.1 (HTTP/2 MAY be offered). Request and response bodies are JSON
(`application/json; charset=utf-8`, I-JSON as in spec/README.md §3) unless a route says
otherwise. A server MUST accept a `POST` without `Content-Type`; any `Content-Type` is ignored
and the body is parsed as JSON. An empty `POST` body means `{}`.

**[API-HTTP-2]** The API major version is the first path segment (`/v1`). Routes outside `/v1`
and `/admin` are the unauthenticated service routes of §6.

**[API-HTTP-3]** Successful operations answer `200 OK` with the operation's response; `POST
/admin/graphs` answers `201 Created`. A body that is not valid JSON fails with `json.malformed`
(or `text.invalid_unicode`); a body larger than the configured limit (default 16 MiB) fails with
`api.body_too_large`.

## 2. Authentication and roles

**[API-AUTH-1]** Requests to `/v1` and `/admin` carry `Authorization: Bearer <key>`. A key is
configured with an **actor** name ([ACTOR-NAME-1]), a **role**, and optionally the list of graphs
it may use (default: all).

| Role | May call |
|---|---|
| `agent-read` | `/v1` reads: `overview`, `search`, `resolve`, `get`, `neighbors`, `paths`, `find`, `history`, `stats`, and their `GET` shorthands |
| `agent-write` | everything `agent-read` may, plus `write`, `retract`, `merge` and the `DELETE` / restore shorthands |
| `admin` | everything, including `/admin` |

**[API-AUTH-2]** A missing `Authorization` header fails with `auth.missing_key`, an unknown key
with `auth.invalid_key` (both `401`, with `WWW-Authenticate: Bearer`). A role that does not allow
the route fails with `auth.forbidden`, a graph outside the key's list with
`auth.graph_forbidden` (both `403`).

**[API-AUTH-3]** Changes are recorded under the key's actor name; requests cannot choose another
actor.

**[API-AUTH-4]** A server configured with **no keys** treats every request as the actor `local`
with role `admin`. It MUST then listen on loopback addresses only, and MUST refuse to start
otherwise.

## 3. Graphs

**[API-GRAPH-1]** Every `/v1` route except `GET /v1/graphs` names its graph in the path. An
unknown graph fails with `graph.not_found`.

**[API-GRAPH-2]** A graph loads its in-memory indexes when the server starts or on first use.
While it loads, a request waits up to a configured time (default 2 s); if the graph is still not
ready it fails with `graph.warming_up` (`503`, with `Retry-After` in seconds). A graph that failed
to load fails every request with `graph.load_failed`.

**[API-GRAPH-3]** Reads see every write that answered before them (read-your-writes). Writes on one
graph are applied one at a time; reads run in parallel with each other.

## 4. Routes

### 4.1 Operations

**[API-OP-1]** `POST /v1/graphs/{graph}/{operation}` runs an operation with the request body as its
request, for `operation` one of `overview`, `search`, `resolve`, `get`, `write`, `retract`,
`merge`, `neighbors`, `paths`, `find`, `history`, `stats`. Any other name fails with `api.not_found`. A
`graph` member in the body, if present, MUST equal the path's graph (otherwise
`api.graph_mismatch`) and is removed before the operation sees the request.

### 4.2 Shorthands

**[API-SHORT-1]** These routes build an operation request from the path and query string, run it,
and answer what is stated. Query parameters not listed fail with `api.invalid_parameter`; list
values are comma-separated; integers and booleans (`true`/`false`) are parsed from their text.

| Route | Operation request | Answer |
|---|---|---|
| `GET /v1/graphs` | — | `{"graphs": [{"name", "status"}]}`: graphs the key may use, by name; `status` is `ready`, `loading`, `unloaded` or `failed` |
| `GET /v1/graphs/{graph}` | `overview` with `type`, `recent`, `types_limit` from the query | the response |
| `GET /v1/graphs/{graph}/search?q=…` | `search` with `query` = `q`, and `in`, `types` (lists), `mode`, `limit`, `include_retracted` | the response |
| `GET /v1/graphs/{graph}/{collection}/{id}` | `get` with `refs: [id]`, and `detail`; `include` = list of `links`, `support`, `evidence`, `content` (others off; default: the operation's defaults); `links_limit`, `link_types`, `evidence_limit`, `include_retracted` | the single item (not wrapped in `items`) |
| `GET /v1/graphs/{graph}/{collection}/{id}/history` | `history` with `target` = id, and `limit`, `before_seq` | the response |
| `GET /v1/graphs/{graph}/stats` | `stats` with `sections` (list), `days`, `top` | the response |
| `GET /v1/graphs/{graph}/changes?since_seq=…` | `history` with `since_seq` (default 0) and `limit` | the response |
| `GET /v1/graphs/{graph}/evidence/{id}/content` | — | the inline content as `text/plain; charset=utf-8`; `from`, `max_chars` select a range in code points as in [OP-GET-7] |
| `GET /v1/graphs/{graph}/evidence/{id}/blob` | — | the blob's bytes ([STORE-BLOB-1]) with the evidence's `mime` as `Content-Type` (default `application/octet-stream`) and `ETag: "<sha256>"` |
| `DELETE /v1/graphs/{graph}/{collection}/{id}` | `retract` with `targets: [id]` and `reason` from the query | the response |
| `POST /v1/graphs/{graph}/{collection}/{id}/restore` | `retract` with `action: restore`, `targets: [id]` | the response |

`collection` is `nodes`, `links`, `evidence` or `citations`.

**[API-SHORT-2]** In shorthand routes, `id` MUST be an object ID ([ID-CODEC-*]) of the
collection's kind; anything else, or an object that does not exist, fails with `ref.not_found`
(`404`). A node ID of a merged node answers the surviving node with `merged_from`, as `get` does.
Evidence without inline content answers `404` `content.not_found` on `/content`, and without a
blob, or with a blob missing from the blob store, `404` `blob.not_found` on `/blob`.

## 5. Limits

**[API-LIMIT-1]** Each operation enforces its own item and result limits (see its file). The
transport adds the body-size limit of [API-HTTP-3]. Per-request time budgets are planned.

## 6. Service routes

| Route | Auth | Answer |
|---|---|---|
| `GET /health` | none | `200` `{"status": "ok", "server_version", "spec_version", "storage_format_version"}` while the process runs |
| `GET /ready` | none | `200` when every graph found at start is ready, else `503`; body `{"ready": bool, "graphs": {name: status}}` |
| `GET /metrics` | none | Prometheus text format ([ADMIN-MET-1]) |
| `GET /admin/ui` | none (the page reads the admin API with the user's key) | The stats page ([ADMIN-UI-1]) |
| `GET /admin/ui/graph` | none (the page reads the agent API with the user's key) | The graph viewer ([ADMIN-UI-2]) |
| `GET /admin/ui/cytoscape.min.js` | none | The viewer's drawing library |
| `GET /openapi.yaml` | none | [openapi.yaml](openapi.yaml) |
