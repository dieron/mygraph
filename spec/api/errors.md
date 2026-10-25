# Errors

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §5](../../docs/06-agent-api.md)

How the API reports a failed request: one error object with a reason code (spec/README.md §3), an
HTTP status derived from it, and a hint that tells an agent what to do next.

---

## 1. Error object

**[ERR-1]** A failed request answers with this body and the status of §2:

```json
{ "error": { "code": "ref.not_found", "message": "nothing matches n_9zz",
             "section": null, "index": null, "details": null,
             "hint": "Find the object's ID with search or resolve." } }
```

| Member | Meaning |
|---|---|
| `code` | The reason code of the specific failure |
| `message` | Human-readable text; its wording is not normative |
| `section`, `index` | For `write`: the request section (`evidence`, `nodes`, …) and the 0-based item index that failed ([OP-WRITE-6]); otherwise `null` |
| `details` | Code-specific data (e.g. `candidates` for `write.ambiguous_match`, the stored version for `write.version_conflict`), or `null` |
| `hint` | The next step for the caller (§3, [ERR-4]), or `null` |

**[ERR-2]** A request fails as a whole: no partial effects ([OP-WRITE-6]). Problems that do not
fail a request are reported as warnings inside the operation's response (for example
`NEW_VOCABULARY`), or as per-item errors where an operation says so (`get`, [OP-GET-1]).

## 2. HTTP status

**[ERR-3]** The status follows from the code by this table; every code not listed is a problem
with the request itself and answers `400 Bad Request`.

| Status | Codes |
|---|---|
| `401` | `auth.missing_key`, `auth.invalid_key` |
| `403` | `auth.forbidden`, `auth.graph_forbidden` |
| `404` | `ref.not_found`, `graph.not_found`, `api.not_found`, `content.not_found`, `blob.not_found` |
| `405` | `api.method_not_allowed` |
| `409` | Conflicts with the stored state — the same request could succeed against another state: `write.version_conflict`, `write.property_conflict`, `write.ambiguous_match`, `write.type_mismatch`, `write.link_mismatch`, `key.taken`, `lifecycle.identity_taken`, `lifecycle.retracted`, `lifecycle.invalid_transition`, `link.endpoint_not_active`, `citation.evidence_not_active`, `citation.target_not_active`, `evidence.blob_conflict`, `graph.exists` |
| `413` | `api.body_too_large` |
| `500` | `graph.load_failed`, `internal`, and every `storage.*` code |
| `503` | `graph.warming_up` |

## 3. Hints

**[ERR-4]** A server SHOULD set `hint` for these codes (wording informative):

| Code | Hint |
|---|---|
| `ref.not_found` | Find the object's ID with `search` or `resolve`. |
| `write.ambiguous_match` | Several objects match; pass an ID or a key from `details.candidates`, or set `create_new: true`. |
| `write.evidence_required` | Cite evidence for each new fact; for your own conclusion, add an `observation` evidence and cite it. |
| `write.version_conflict` | The object changed since you read it; `get` it again and retry with its current version. |
| `write.property_conflict` | A stored value differs; use `on_conflict: overwrite` to replace it, or `keep` to leave it and get a warning. |
| `lifecycle.retracted` | The object is retracted; restore it with `retract` and `action: restore` first. |
| `graph.not_found` | List the available graphs with `GET /v1/graphs`. |
| `graph.warming_up` | The graph is loading; retry after the number of seconds in `Retry-After`. |
| `auth.missing_key` | Send `Authorization: Bearer <key>`. |
| `api.not_found` | See `GET /openapi.yaml` for the available routes. |
| `json.malformed` | The body must be one JSON object. |
| any `*.invalid_field` | The message names the field; see the operation's specification. |
