# MyGraph over REST (when the MCP tools are not available)

The server's address is in the environment variable `MYGRAPH_URL` (for example
`http://graphhost:7420`); without it, try `http://127.0.0.1:7420`. Every MCP tool is a REST
operation with the same JSON request; responses are JSON (send `Accept: text/plain` on POST
operations for the compact text). If the server has API keys, send
`Authorization: Bearer $MYGRAPH_KEY`; without keys, local requests are accepted. Never print the
key or write it into files. The full description is served at `GET /openapi.yaml`.

## Operations

`POST /v1/graphs/{graph}/{operation}` with the operation's JSON body, for `overview`, `search`,
`resolve`, `get`, `write`, `retract`, `merge`, `neighbors`, `paths`, `find`, `history`, `stats`.

```bash
MG=${MYGRAPH_URL:-http://127.0.0.1:7420}
curl -s -X POST "$MG/v1/graphs/research/search" -H "Authorization: Bearer $MYGRAPH_KEY" \
  -H 'Content-Type: application/json' -H 'Accept: text/plain' -d '{"query": "lovelace"}'
```

PowerShell: `Invoke-RestMethod -Method Post -Uri "$env:MYGRAPH_URL/v1/graphs/research/search"
-Headers @{Authorization = "Bearer $env:MYGRAPH_KEY"} -ContentType 'application/json'
-Body '{"query": "lovelace"}'`.

For a `write` with long text, put the JSON in a file and send it with `curl --data-binary @file`
(or `-InFile file`), so quoting cannot break it.

Shorthands:

| Route | Does |
|---|---|
| `GET /v1/graphs` | Graphs you may use |
| `GET /v1/graphs/{g}` | `overview` (`?type=Person`) |
| `GET /v1/graphs/{g}/search?q=…&in=nodes,evidence&mode=text&limit=10` | `search` |
| `GET /v1/graphs/{g}/nodes/{id}?include=links,support,evidence&detail=full` | `get` one object (also `links`, `evidence`, `citations`) |
| `GET /v1/graphs/{g}/{collection}/{id}/history` | the object's history |
| `GET /v1/graphs/{g}/changes?since_seq=…` | changes since |
| `GET /v1/graphs/{g}/stats?sections=degree,evidence&top=5` | `stats` |
| `GET /v1/graphs/{g}/evidence/{id}/content` | evidence text (`?from=&max_chars=`) |
| `GET /v1/graphs/{g}/evidence/{id}/blob` | the evidence file |
| `DELETE /v1/graphs/{g}/{collection}/{id}?reason=…` | retract |
| `POST /v1/graphs/{g}/{collection}/{id}/restore` | restore |

## Administration (admin role)

| Route | Does |
|---|---|
| `POST /admin/graphs` `{"name": "lab", "policies": {"require_evidence": "warn"}}` | create a graph (the MCP `graphs` tool does this too) |
| `GET /admin/graphs`, `GET /admin/graphs/{g}` | graphs with status, counts, sizes |
| `DELETE /admin/graphs/{g}?confirm={g}` | drop a graph (moved to the trash folder) |
| `POST /admin/graphs/{g}/snapshot` | online copy of the database into `snapshots/` |
| `POST /admin/graphs/{g}/restore` `{"snapshot": "<file>", "replace": true}` | restore a snapshot |
| `POST /admin/graphs/{g}/export` | JSONL export into `exports/` |
| `POST /admin/graphs/{new}/import` `{"file": "<file>"}` | import an export into a new graph, IDs kept |
| `POST /admin/graphs/{g}/check` | integrity check |
| `POST /admin/graphs/{g}/reindex` | rebuild the full-text index from the records (when `text_index` stays `stale`) |
| `GET /admin/stats`, `/admin/keys`, `/admin/config` | server information |

Service routes without a key: `GET /health`, `GET /ready`, `GET /metrics`.

## Errors

`{"error": {"code", "message", "section", "index", "details", "hint"}}` with an HTTP status from
the code: 400 invalid request, 401/403 key or role, 404 not found (`ref.not_found`,
`graph.not_found`), 409 conflict with stored data (`write.version_conflict`,
`write.ambiguous_match`, `lifecycle.retracted`…), 413 body too large, 503 graph still loading (retry
after `Retry-After` seconds).
