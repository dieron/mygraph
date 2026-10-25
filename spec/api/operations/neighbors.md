# Operation: `neighbors`

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §4.8](../../../docs/06-agent-api.md)

k-hop expansion from one or more nodes, returning a subgraph. Link rules:
[algorithms/traversal.md](../../algorithms/traversal.md).

---

## 1. Request

```json
{ "start": ["n_3kq"],
  "steps": [ { "link_types": ["COLLABORATED_WITH", "WORKS_AT"], "direction": "both" },
             { "link_types": ["WORKS_AT"], "direction": "out", "node_types": ["Organization"] } ],
  "limit_per_node": 25, "limit": 200, "as_of": {"$date": "1845"}, "include": { "support": true } }
```

| Field | Meaning | Default |
|---|---|---|
| `start` | 1–50 node references (as in `get`) | required |
| `steps` | 1–5 hops, each `{link_types, direction, node_types}` | — |
| `depth`, `link_types`, `direction`, `node_types` | Instead of `steps`: `depth` (1–5) equal hops | `depth` 1, `direction` `both` |
| `limit_per_node` | 1–1000 links taken per node and hop | 25 |
| `limit` | 1–5000 nodes in the result, start nodes included | 200 |
| `as_of` | [TRAV-2] | — |
| `include.support` | Add `sources` (distinct active evidence, [OP-GET-5]) to nodes and links | `false` |

**[OP-NEIGH-1]** `steps` and `depth` are exclusive; invalid fields fail with
`neighbors.invalid_field`; a start reference that does not resolve fails with `ref.not_found`.

## 2. Expansion

**[OP-NEIGH-2]** Hop 0 is the start nodes (resolved, merged nodes to their survivors, duplicates
removed). Hop `i` expands every node reached at hop `i − 1`, in node order ([TRAV-3]); for each,
it takes the followable links ([TRAV-1]) of step `i` whose other end has a type in the step's
`node_types` (when given), in link order, at most `limit_per_node` of them. A taken link is added
to the result; its other end is added at hop `i` unless already in the result. When adding a node
would exceed `limit`, the link and the node are left out and the expansion stops.

**[OP-NEIGH-3]** Each link appears once, even when taken from both ends.

## 3. Response

```json
{ "nodes": [ { "id": "n_3kq", "type": "Person", "name": "Ada Lovelace", "hop": 0 } ],
  "links": [ { "id": "l_9x", "type": "COLLABORATED_WITH", "from": "n_3kq", "to": "n_3kr", "directed": false } ],
  "truncated": false,
  "notes": [ { "node": "n_3kr", "hop": 1, "links_taken": 25, "links_total": 412 } ] }
```

**[OP-NEIGH-4]** `nodes` are in order of addition, `links` in order of taking. `notes` lists
every node whose followable links exceeded `limit_per_node` (`hop` is the hop being expanded).
`truncated` is `true` when `limit` stopped the expansion or any note exists.
