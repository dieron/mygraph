# Operation: `stats`

Status: **Draft** · spec 0.1.0-draft · design: [docs/07-admin-api.md §2](../../../docs/07-admin-api.md)

Base statistics of a graph: how much it holds, how connected it is, how it grew and how well
its facts are sourced. Agents use it to judge a graph before relying on it; the admin API
([admin.md §2](../admin.md)) serves the same sections plus `actors` and `storage`. Everything is
computed from the in-memory indexes, so the answer reflects the graph at the moment of the call.

---

## 1. Request

```json
{ "sections": ["counts", "degree"], "days": 30, "top": 10 }
```

| Field | Meaning | Default |
|---|---|---|
| `sections` | Non-empty array of distinct section names: `counts`, `degree`, `growth`, `evidence` | all four |
| `days` | 1–366: length of the `growth` window in days | 30 |
| `top` | 1–100: length of each top list (types, hubs, domains, most cited) | 10 |

**[OP-STATS-1]** Invalid fields, an unknown or repeated section, and a section the caller may not
request (`actors` and `storage` are admin-only) fail with `stats.invalid_field`.

## 2. Response

```json
{ "graph": "research", "last_seq": 412230, "computed_at": "2026-10-26T10:00:00.000Z",
  "counts": {…}, "degree": {…}, "growth": {…}, "evidence": {…} }
```

**[OP-STATS-2]** The response holds `graph`, `last_seq` (as in [OP-OVER-3]), `computed_at` (the
request time) and one member per requested section, and no other sections.

### 2.1 `counts`

```json
{ "active":    { "nodes": 18432, "links": 61210, "evidence": 9120, "citations": 88014 },
  "retracted": { "nodes": 12, "links": 40, "evidence": 1, "citations": 55 },
  "merged":    { "nodes": 7 },
  "actors": 3, "node_types": 14, "link_types": 22,
  "top_node_types": [ { "type": "Person", "count": 5120 } ],
  "top_link_types": [ { "type": "WORKS_AT", "count": 4410 } ],
  "evidence_kinds": { "text": 4600, "url": 4100 } }
```

**[OP-STATS-3]** `active`, `retracted` and `merged` count the graph's objects by status (only
nodes can be merged); `active` equals `counts` of [OP-OVER-2]. `actors` is the number of known
actors. `node_types` and `link_types` are the numbers of distinct types among active nodes and
links; `top_node_types` and `top_link_types` list the `top` most used, by count descending, then
type in code point order. `evidence_kinds` is as in [OP-OVER-2].

### 2.2 `degree`

```json
{ "nodes": 18432, "min": 0, "max": 812, "mean": 6.64, "p50": 3, "p90": 14, "p99": 61,
  "orphans": 210,
  "hubs": [ { "id": "n_1", "type": "Person", "name": "Ada Lovelace", "status": "active", "degree": 812 } ] }
```

**[OP-STATS-4]** The degree of an active node is the number of active links that have it as an
endpoint; a link from a node to itself counts twice. Over the `nodes` active nodes: `min`, `max`,
`mean` (rounded to 2 decimals) and the percentiles `p50`, `p90`, `p99`, where pX is the degree at
position ⌈X / 100 × nodes⌉ − 1 (from 0) of all degrees sorted ascending. `orphans` counts nodes
with degree 0. `hubs` lists the `top` nodes with the highest degree above 0, by degree
descending, then internal number, each as its brief form ([OP-GET-3]) plus `degree`. With no
active nodes every number is 0 and `hubs` is empty.

### 2.3 `growth`

```json
{ "days": 30, "from": "2026-09-27", "to": "2026-10-26",
  "per_day": [ { "day": "2026-10-25",
                 "created":   { "nodes": 40, "links": 92, "evidence": 6, "citations": 180 },
                 "retracted": { "nodes": 0, "links": 1, "evidence": 0, "citations": 2 } } ],
  "totals": { "created": { … }, "retracted": { … } } }
```

**[OP-STATS-5]** The window is the `days` UTC calendar days ending with the day of the request
(`to`); `from` is its first day. For each day of the window, oldest first, on which anything was
created or retracted, `per_day` has an entry: `created` counts objects of each kind, whatever
their status now, whose `created_at` falls on that day; `retracted` counts objects of each kind
whose status is retracted and whose `updated_at` falls on that day (a retracted object can only
change again by being restored, so its last update is its retraction). All four kinds are always
present. `totals` sums the entries.

### 2.4 `evidence`

```json
{ "active": 9120, "kinds": { "text": 4600, "url": 4100, "document": 420 },
  "uncited": 31,
  "domains": [ { "domain": "en.wikipedia.org", "count": 2210 } ],
  "most_cited": [ { "id": "e_7", "kind": "url", "title": "…", "uri": "https://…", "status": "active", "citations": 940 } ],
  "blobs": { "evidence": 12, "distinct": 11, "bytes": 5242880 } }
```

**[OP-STATS-6]** Over active evidence:

- `active` and `kinds` as in [OP-STATS-3].
- `uncited`: evidence that no active citation cites, either directly or through one of its
  active fragments (evidence whose `parent` chain leads to it).
- `domains`: for evidence whose `uri` starts with `http://` or `https://`, the URI's host in
  lower case, without a port and without a leading `www.`; the `top` most frequent, by count
  descending, then domain in code point order.
- `most_cited`: the `top` evidence with the most active citations (direct only, at least one),
  by that number descending, then internal number, each as its brief form plus `citations`.
- `blobs`: `evidence` = evidence with a blob; `distinct` = distinct blob hashes among them;
  `bytes` = the sum of the sizes of the distinct blobs.

## 3. Errors

`stats.invalid_field` ([OP-STATS-1]); the common errors of [errors.md](../errors.md).
