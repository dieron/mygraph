# Operation: `history`

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §4.11](../../../docs/06-agent-api.md)

Change-log entries ([STORE-LOG-2], [STORE-LOG-3]) of one object, or of the whole graph since a
sequence number — "what changed since I last looked?".

---

## 1. Request

```json
{ "target": "n_3kq", "limit": 20, "before_seq": 9000 }
{ "since_seq": 88120, "limit": 100 }
```

| Field | Meaning | Default |
|---|---|---|
| `target` | An object reference (as in `get`); an ID of a merged node means that node itself, not its survivor | — |
| `since_seq` | Integer ≥ 0: entries after this sequence number | — |
| `before_seq` | With `target`: entries before this sequence number (paging) | — |
| `limit` | 1–500 | 20 with `target`, 100 with `since_seq` |

**[OP-HIST-1]** Exactly one of `target` and `since_seq` is required; otherwise, or with invalid
fields, the call fails with `history.invalid_field`. An unresolvable `target` fails with
`ref.not_found`.

## 2. Response

```json
{ "entries": [ { "seq": 88121, "at": "2026-10-24T09:12:00.000Z", "actor": "claude-research",
                 "batch": "b_8k2", "op": "update", "object": "n_3kq",
                 "diff": { "before": {"summary": null}, "after": {"summary": "…"} } } ],
  "last_seq": 88140, "more": true }
```

**[OP-HIST-2]** With `target`: the object's entries, newest first. With `since_seq`: all entries
with a greater sequence number, oldest first. At most `limit` entries; `more` is `true` when
further entries match. `last_seq` is the graph's latest sequence number, so a caller can resume
with `since_seq: <last seq it saw>`.

**[OP-HIST-3]** `object` is the ID of the changed node, link, evidence or citation, or `null` for
vocabulary and graph metadata entries (identified inside `diff`, [STORE-LOG-3]). `at` is the
batch time as a `datetime` ([VAL-DT]); `actor` is the actor's name.
