# 07 — Admin API

Status: **Draft for review** · 2026-10-24 · covers R5, N5, N9, N11–N13

REST under `/admin/…`, requires an API key with the `admin` role. It can be bound to a separate
port. The future admin UI is a pure client of this API. **PoC** marks what is in the proof of
concept; everything else is planned.

## 1. Graph management

| Endpoint | Purpose | PoC |
|---|---|:-:|
| `GET /admin/graphs` | List graphs with counts, file sizes, status (ready / warming / error) | ✅ |
| `POST /admin/graphs` | Create `{name, description, policies}` | ✅ |
| `PATCH /admin/graphs/{g}` | Change description or policies (`require_evidence`, `inline_text_limit`, …) | ✅ |
| `DELETE /admin/graphs/{g}?confirm={g}` | Drop a graph (moved to a trash folder first) | ✅ |
| `POST /admin/graphs/{g}/snapshot` | Online, consistent backup (SQLite backup API) | ✅ |
| `POST /admin/graphs/{g}/restore` | Restore from a snapshot into a new or existing graph | |
| `POST /admin/graphs/{g}/export` | JSONL export (nodes, links, evidence, citations, vocabulary); later GraphML, CSV, RDF-star / PROV-O | ✅ JSONL |
| `POST /admin/graphs/{g}/import` | JSONL import; later: MCP memory-server JSONL, Neo4j CSV, RDF | ✅ JSONL |

## 2. Statistics

`GET /admin/graphs/{g}/stats?sections=…&days=…&top=…` — each section can be requested
separately ([spec/api/admin.md](../spec/api/admin.md) [ADMIN-GRAPH-9]). Built (M5): `counts`,
`degree`, `growth`, `evidence`, `actors`, `storage`; the graph-content sections are also the agent
operation `stats` ([spec/api/operations/stats.md](../spec/api/operations/stats.md)). Still to
build: `quality` and `usage`.

| Section | Contents | PoC |
|---|---|:-:|
| `counts` | Objects by kind, status, type, link type, evidence kind | ✅ |
| `growth` | Created/updated/retracted per day (from the change log) | ✅ |
| `degree` | Degree distribution (min, max, mean, percentiles), top hubs, orphan nodes | ✅ |
| `quality` | Facts without evidence; facts supported only by `observation`; contradicted facts; duplicate suspects (same type, fuzzy-similar names); vocabulary drift (similar type names) | ✅ |
| `evidence` | By kind, by URL domain, most-cited evidence, evidence citing nothing, blob totals | ✅ |
| `actors` | Activity per actor (batches, objects created, retractions) | ✅ |
| `storage` | DB, WAL, FTS and blob sizes; RAM per index | ✅ |
| `usage` | Calls per tool/endpoint, latency percentiles, slowest operations | ✅ |

`GET /admin/stats` — server-wide: uptime, memory, graphs, request rates.

## 3. Maintenance

| Endpoint | Purpose | PoC |
|---|---|:-:|
| `POST /admin/graphs/{g}/check` | Integrity: SQLite `integrity_check`, foreign keys, RAM-vs-disk consistency, blob hash verification (sample or full) | ✅ |
| `POST /admin/graphs/{g}/reindex` | Rebuild FTS and/or RAM indexes | ✅ |
| `POST /admin/graphs/{g}/optimize` | WAL checkpoint, FTS optimize, `VACUUM` | ✅ |
| `POST /admin/graphs/{g}/purge` | Hard-delete retracted objects older than N days (`dry_run` first) | |
| `POST /admin/graphs/{g}/undo` | Revert a batch by ID (`dry_run` first) | |
| `GET /admin/graphs/{g}/changes` | Browse the change log by actor, op, object, time range | ✅ |
| `POST /admin/blobs/gc` | Delete blobs no evidence references | |
| `POST /admin/graphs/{g}/vocabulary/rename` | Rename a type, link type or property key everywhere (e.g. `EMPLOYED_BY` → `WORKS_AT`) | |
| `PUT /admin/graphs/{g}/vocabulary/{kind}/{name}` | Description, symmetric flag | ✅ |
| `GET /admin/graphs/{g}/duplicates` | Review queue of duplicate suspects; merge via the agent `merge` operation | |
| `POST /admin/graphs/{g}/evidence/check-links` | Check external URIs for link rot; optionally snapshot them | |

## 4. Access and audit

| Endpoint | Purpose | PoC |
|---|---|:-:|
| `GET /admin/keys` | API keys (redacted) with actor, role, default and allowed graphs | ✅ read-only (keys live in config) |
| `POST/DELETE /admin/keys` | Manage keys at runtime | |
| `GET /admin/actors` | Known actors and their activity | ✅ |
| `GET /admin/audit/export` | Change log export for audit | |

## 5. Operations

| Endpoint | Purpose | PoC |
|---|---|:-:|
| `GET /health` | Liveness (no auth) | ✅ |
| `GET /ready` | All graphs warmed up | ✅ |
| `GET /metrics` | Prometheus metrics | ✅ |
| `GET /admin/config` | Effective configuration, secrets redacted | ✅ |
| `POST /admin/log-level` | Change log level at runtime | |
| `GET /admin/slow-ops` | Recent slow operations with request shapes | ✅ |

## 6. Feedback (planned)

Reports about MyGraph itself are not kept by each server: they go to one central place, the
project's GitHub repository, so there are no feedback endpoints in this API. The plan is in
[09-roadmap.md](09-roadmap.md) (Phase 2, Feedback).

## 7. Admin UI

**PoC (M5):** an internal stats page (`GET /admin/ui`, server-rendered, embedded in the binary) and
a read-only graph viewer (neighbourhood view with Cytoscape.js or Sigma.js, properties and sources
in a side panel).

**Later (Admin UI v1):** dashboard over all stats sections, evidence viewer (content, fragments in
context, blobs), duplicate review queue, vocabulary manager, change-log browser with undo, editing
in the graph viewer. Built on this API only.
