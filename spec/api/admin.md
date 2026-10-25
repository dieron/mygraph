# Admin API

Status: **Draft** · spec 0.1.0-draft · design: [docs/07-admin-api.md](../../docs/07-admin-api.md)

Routes under `/admin` for operators: creating and dropping graphs, statistics, backups and
checks. They require the `admin` role ([API-AUTH-1]); conventions and errors are those of
[conventions.md](conventions.md) and [errors.md](errors.md). This is the proof-of-concept subset
of the design; the rest is planned.

---

## 1. Graphs

| Route | Request | Answer |
|---|---|---|
| `GET /admin/graphs` | — | `{"graphs": [GraphInfo]}` by name |
| `POST /admin/graphs` | `{"name": …, "policies": {…}}` | `201`, `GraphInfo`, `Location: /admin/graphs/{name}` |
| `GET /admin/graphs/{graph}` | — | `GraphInfo` |
| `DELETE /admin/graphs/{graph}?confirm={graph}` | — | `{"deleted": name, "moved_to": path}` |
| `POST /admin/graphs/{graph}/snapshot` | — | `{"path": …, "bytes": n, "last_seq": n}` |
| `POST /admin/graphs/{graph}/check` | — | `{"ok": bool, "problems": [text]}` |

**[ADMIN-GRAPH-1]** `GraphInfo` is `{"name", "status", "file_bytes", "wal_bytes"}`, plus, when the
graph is loaded (`status` `ready`): `counts` (as in [OP-OVER-2]), `policies`, `last_seq`, and
`text_index`: `"current"`, `"stale"` (an update failed; search uses the store's own full text) or
`"none"`.

**[ADMIN-GRAPH-2]** Creating: `name` follows [GRAPH-NAME-1]; `policies` is optional, each missing
member taking its default ([OBJ-POLICY-*]); invalid input fails with `admin.invalid_field`,
`graph_name.*` or `graph.policy_invalid`; an existing name with `graph.exists`. The new graph is
loaded and ready when the call answers.

**[ADMIN-GRAPH-3]** Deleting requires `confirm` equal to the graph name (otherwise
`admin.confirm_required`). The graph is closed and its database files are moved to
`<data_dir>/trash/` (never deleted outright); its full-text cache is deleted. Requests that were
running finish first; later requests fail with `graph.not_found`.

**[ADMIN-GRAPH-4]** A snapshot is a consistent copy of the graph's database taken while it stays
online, written to `<data_dir>/snapshots/<graph>-<UTC time as YYYYMMDDTHHMMSSmmmZ>.db`. Writes wait
while it is taken. It can be opened by any implementation like the original file (restore is
planned).

**[ADMIN-GRAPH-5]** A check runs the storage engine's own integrity checks and reports each
problem found as text; `ok` is `true` when there are none.

## 2. Server

| Route | Answer |
|---|---|
| `GET /admin/stats` | `{"uptime_s", "graphs": {name: status}, "requests": {operation: {"count", "errors", "mean_ms", "max_ms"}}}` |
| `GET /admin/keys` | `{"keys": [{"key_id", "actor", "role", "graphs"}]}` — `key_id` is the first 8 hex digits of the key's SHA-256; keys themselves are never shown |
| `GET /admin/config` | The effective configuration without key material |

**[ADMIN-SRV-1]** Request statistics count since the server started, by operation name (or route
name for shorthands and admin routes). They are informative.
