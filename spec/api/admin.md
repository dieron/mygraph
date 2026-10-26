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
| `POST /admin/graphs/{graph}/restore` | `{"snapshot": file, "replace": bool}` | `GraphInfo` |
| `POST /admin/graphs/{graph}/export` | — | `{"path": …, "bytes": n, "counts": {…}}` |
| `POST /admin/graphs/{graph}/import` | `{"file": name}` | `GraphInfo` plus `imported` (counts) |
| `GET /admin/graphs/{graph}/stats?sections=…&days=…&top=…` | — | `GraphStats` ([ADMIN-GRAPH-9]) |

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
while it is taken. It can be opened by any implementation like the original file, and restored
with [ADMIN-GRAPH-6].

**[ADMIN-GRAPH-5]** A check runs the storage engine's own integrity checks and reports each
problem found as text; `ok` is `true` when there are none.

**[ADMIN-GRAPH-6]** Restoring copies a snapshot file — a name in `<data_dir>/snapshots/` — to the
graph `{graph}` and loads it. An existing graph of that name is first deleted as in
[ADMIN-GRAPH-3] when `replace` is `true`, otherwise the call fails with `graph.exists`. A missing
snapshot fails with `admin.snapshot_not_found`. The full-text cache of the name is rebuilt.

**[ADMIN-GRAPH-7]** Export writes the graph in the format of [export-format.md](export-format.md) to
`<data_dir>/exports/<graph>-<UTC time as in [ADMIN-GRAPH-4]>.jsonl`. Writes wait while it runs.

**[ADMIN-GRAPH-8]** Import reads `file` — a name in `<data_dir>/exports/` — into `{graph}`
([EXP-4], [EXP-5]). When the graph does not exist it is created with the header's policies; an
existing graph must have no objects and no history (`import.graph_not_empty`). A missing file fails
with `admin.file_not_found`.

**[ADMIN-GRAPH-9]** Statistics are the response of the `stats` operation
([stats.md](operations/stats.md)) for the same `sections` (comma-separated), `days` and `top`,
where `sections` may also name the admin-only sections below and defaults to all six. Invalid
parameters fail with `stats.invalid_field`.

- `actors`: `[{"actor", "kind", "batches", "last_batch_at", "created": {"nodes", "links",
  "evidence", "citations"}, "retracted"}]`, one entry per known actor: the batches it wrote and
  the time of its newest one (`null` when none), the objects it created whatever their status
  now (by `created_by`), and the objects whose status is retracted and that it last updated (by
  `updated_by`); by `batches` descending, then actor name.
- `storage`: `{"file_bytes", "wal_bytes", "text_index_bytes", "text_index"}`: database and WAL
  sizes as in [ADMIN-GRAPH-1], the total size of the files of the graph's full-text cache (0 when
  there is none), and `text_index` as in [ADMIN-GRAPH-1].

## 2. Server

| Route | Answer |
|---|---|
| `GET /admin/stats` | `{"uptime_s", "graphs": {name: status}, "requests": {operation: {"count", "errors", "mean_ms", "max_ms"}}}` |
| `GET /admin/keys` | `{"keys": [{"key_id", "actor", "role", "graphs"}]}` — `key_id` is the first 8 hex digits of the key's SHA-256; keys themselves are never shown |
| `GET /admin/config` | The effective configuration without key material |

**[ADMIN-SRV-1]** Request statistics count since the server started, by operation name (or route
name for shorthands and admin routes). They are informative.
