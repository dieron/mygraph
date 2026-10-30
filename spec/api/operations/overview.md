# Operation: `overview`

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §4.1](../../../docs/06-agent-api.md)

What a graph holds: counts, vocabulary in use, policies and recent activity. An agent calls it
first in a session to learn the existing types before it writes, so the vocabulary does not
drift. With `type`, it describes one node type or link type in depth.

---

## 1. Request

```json
{ "recent": 5 }
{ "type": "Person" }
```

| Field | Meaning | Default |
|---|---|---|
| `type` | A node type or link type, exactly as stored ([VOCAB-1]); a miss suggests styled spellings that exist | — |
| `recent` | 0–50: number of recent batches to list | 5 |
| `types_limit` | 1–1000: node types and link types listed (each) | 100 |

**[OP-OVER-1]** Invalid fields fail with `overview.invalid_field`. A `type` that is neither a
node type nor a link type of an active object fails with `ref.not_found`.

## 2. Response

```json
{ "graph": "research", "spec_version": "0.1.0-draft",
  "counts": { "nodes": 18432, "links": 61210, "evidence": 9120, "citations": 88014 },
  "node_types": [ { "type": "Person", "count": 5120, "description": "A human being" } ],
  "link_types": [ { "type": "MARRIED_TO", "count": 210, "symmetric": true } ],
  "types_truncated": false,
  "evidence_kinds": { "text": 4600, "url": 4100 },
  "policies": { "require_evidence": "enforce", "inline_text_limit": 65536, "naming": "styled" },
  "last_seq": 412230,
  "recent": [ { "batch": "b_8k2", "at": "2026-10-24T09:12:00.000Z", "actor": "claude-research",
                "op": "write", "note": null, "changes": 34 } ],
  "conventions": "…" }
```

**[OP-OVER-2]** `counts` are numbers of **active** objects of each kind. `node_types` and
`link_types` list the types of active nodes and links with their counts, by count descending,
then type in code point order, at most `types_limit` each; `types_truncated` is `true` when
either list was cut. An entry has `description` when a vocabulary note describes the type, and a
link type has `symmetric: true` when it is declared symmetric. `evidence_kinds` counts active
evidence by kind; kinds with zero are omitted.

**[OP-OVER-3]** `policies` are the graph's policies ([OBJ-POLICY-*]); `last_seq` is the latest
change-log sequence number (0 for an empty graph); `conventions` is a fixed informative text
about naming and the write workflow (its wording is not normative).

**[OP-OVER-4]** `recent` lists the newest batches, newest first: ID, time, actor, operation,
note, and `changes` — the number of change-log entries the batch wrote.

## 3. One type

With `type`, the response also has `type_detail`:

```json
{ "type_detail": {
    "kind": "node", "type": "Person", "count": 5120, "description": "A human being",
    "sample": 5120,
    "props": [ { "key": "born", "kinds": ["date"], "fill": 0.81 } ],
    "links": [ { "type": "WORKS_AT", "direction": "out", "other_types": [ {"type": "Organization", "count": 1180} ], "count": 1204 } ],
    "examples": [ { "id": "n_1", "type": "Person", "name": "Ada Lovelace" } ] } }
```

**[OP-OVER-5]** Statistics of a type are computed over a **sample** of its active objects in
internal-number order: all of them when there are at most 10 000, else the 10 000 at positions
floor(i × n / 10 000) for i = 0 … 9 999 (n objects, positions from 0); `sample` is its size. For a name used both as a node type and as a link type, the node type is described.

- `props`: every property key present in the sample, with the value kinds seen (sorted) and
  `fill` = objects with the key / `sample`, rounded to 2 decimals; by fill descending, then key.
- Node type, `links`: active links of the sample nodes, grouped by link type and direction
  (`out`, `in` for directed links seen from the node; `both` for undirected ones), with the
  count of such link ends and the types of the nodes at the other end (count descending, then
  type; at most 5); groups by count descending, then type, then direction; at most 20.
- Link type, `endpoints` instead of `links`: `[{"from": <type>, "to": <type>, "count": n}]`
  over the sample links (for an undirected link, `from` is the endpoint with the smaller
  internal number), by count descending, then `from`, then `to`; at most 20. Also `symmetric`.
- `examples`: the brief form ([OP-GET-3]) of the first 5 objects of the sample.
