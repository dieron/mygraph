# Operation: `paths`

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §4.9](../../../docs/06-agent-api.md)

Shortest or k-shortest paths between two nodes. Link rules and path order:
[algorithms/traversal.md](../../algorithms/traversal.md).

---

## 1. Request

```json
{ "from": "n_3kq", "to": {"type": "Person", "name": "Alan Turing"},
  "max_length": 5, "link_types": null, "direction": "both", "mode": "k_shortest", "k": 3,
  "as_of": {"$date": "1950"} }
```

| Field | Meaning | Default |
|---|---|---|
| `from`, `to` | Node references (as in `get`) | required |
| `max_length` | 1–10 | 4 |
| `link_types`, `direction`, `as_of` | [TRAV-1], [TRAV-2]; `direction` is read from `from` towards `to` | all, `both`, — |
| `mode` | `shortest`, `all_shortest`, `k_shortest` | `shortest` |
| `k` | 1–20, for `k_shortest` | 3 |

**[OP-PATHS-1]** Invalid fields fail with `paths.invalid_field`; unresolvable references with
`ref.not_found`.

## 2. Result

**[OP-PATHS-2]** Among the paths ([TRAV-4]) from `from` to `to` of length ≤ `max_length` whose
links are followable ([TRAV-1]) in the walking direction:
- `shortest`: the first path in path order;
- `all_shortest`: all paths of the smallest length, in path order, at most 100;
- `k_shortest`: the first `k` paths in path order.

When `from` = `to` the only path has length 0.

**[OP-PATHS-3]** Implementations may stop searching for longer paths (`k_shortest`) after a work
budget of their choosing; they then return the paths found so far, which are still the first ones
in path order, with `truncated: true`. Shortest paths are never cut by the budget.

## 3. Response

```json
{ "paths": [ { "length": 2,
               "nodes": [ {"id": "n_1", "type": "Person", "name": "Ada Lovelace"}, "…" ],
               "links": [ {"id": "l_9", "type": "COLLABORATED_WITH", "from": "n_1", "to": "n_2",
                           "directed": false, "sources": 1}, "…" ],
               "min_sources": 1 } ],
  "truncated": false }
```

**[OP-PATHS-4]** `nodes` has `length + 1` entries, `links` `length`. Every link carries `sources`
(distinct active evidence citing it, [OP-GET-5]); `min_sources` is the smallest of them (absent
for length 0), so an agent can judge the weakest step of a chain.
