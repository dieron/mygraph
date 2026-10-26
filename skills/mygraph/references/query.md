# Querying MyGraph — full reference

Contents: 1. search · 2. resolve · 3. get · 4. neighbors · 5. paths · 6. find · 7. history ·
8. overview · 9. Tips

Every tool takes an optional `graph` and `format` (`compact` text, default, or `json`).

## 1. search

```json
{ "query": "babbage engine", "in": ["nodes", "evidence"], "types": ["Person"], "mode": "auto", "limit": 20 }
```

- `in`: any of `nodes` (default), `links`, `evidence`.
- `mode`: `auto` (exact name → alias → prefix → fuzzy → full text, merged), `exact`, `prefix`,
  `fuzzy` (typos), `text` (full text over names, summaries, string properties, evidence title and
  content).
- `types`: node or link types to keep. `include_retracted: true` also returns retracted objects.
- Scores: exact name 1.0, alias 0.95, prefix ~0.8–0.9, fuzzy 0.4–0.7, full text 0.1–0.4. Evidence
  hits carry a snippet with the matched words in «».

## 2. resolve

```json
{ "candidates": [ { "name": "Charles Babbage", "type": "Person", "hints": { "born": {"$date": "1791"} } },
                  { "name": "Analytical Engine" } ] }
```

Up to 100 candidates. Each result: `match` (one clear existing node), `ambiguous` (several plausible —
see the alternatives), or `none`, with scores and reasons (`name exact`, `alias exact`,
`name fuzzy 0.62`, `born matches`, `type differs`). `hints` are property values that should agree;
they raise or lower scores. Use it before every write that creates nodes.

## 3. get

```json
{ "refs": ["n_3kq", {"key": "wikidata:Q7259"}, {"type": "Person", "name": "Ada Lovelace"}, "e_2"],
  "detail": "normal",
  "include": { "links": {"limit": 20, "types": ["WORKS_AT"]}, "support": true,
               "evidence": {"limit": 5}, "content": {"from": 0, "max_chars": 4000} },
  "include_retracted": false }
```

- `detail`: `brief` (id, type, name, status), `normal` (all fields; default), `full` (plus version
  and who/when created and updated).
- `include.links` (nodes): link counts per type and direction plus up to `limit` links with the node
  at the other end; `false` turns it off. `include.support`: citation counts by role and the number
  of distinct sources. `include.evidence`: the citing evidence with role, quote and citation ID.
  `include.content` (evidence): the inline text, or a range of it.
- A link's result names its two ends. A merged node's old ID returns the survivor with
  `merged_from`. Unknown references give a per-item error instead of failing the call.

## 4. neighbors

```json
{ "start": ["n_1"], "depth": 2, "link_types": ["KNOWS"], "direction": "both", "node_types": ["Person"],
  "limit_per_node": 25, "limit": 200, "include": {"support": true} }
```

Or per-hop filters: `"steps": [ {"link_types": ["WORKS_AT"], "direction": "out"},
{"link_types": ["WORKS_AT"], "direction": "in", "node_types": ["Person"]} ]` (colleagues).
`depth` 1–5; `direction` `out`/`in`/`both` from the current node; `as_of: {"$date": "1990"}` keeps
only links valid at that date. Returns nodes (with `hop`), links, `truncated` and notes about nodes
whose links were cut by `limit_per_node`.

## 5. paths

```json
{ "from": "n_1", "to": {"type": "Person", "name": "Alan Turing"}, "max_length": 4,
  "mode": "k_shortest", "k": 3, "link_types": ["KNOWS", "WORKED_WITH"], "direction": "both" }
```

`mode`: `shortest` (one), `all_shortest`, `k_shortest` (the `k` shortest simple paths). Each path
lists nodes and links in order, with the number of sources per link and `min_sources` (the weakest
link's support) — prefer well-supported paths when explaining a connection. `no path found` means
none within `max_length` over active links.

## 6. find

Structured queries over `nodes`, `links`, `evidence` or `citations` — the replacement for a query
language.

```json
{ "target": "nodes",
  "where": { "all": [ {"field": "type", "op": "eq", "value": "Person"},
                      {"field": "prop:born", "op": "between", "value": [{"$date": "1800"}, {"$date": "1899"}]},
                      {"any": [ {"field": "prop:country", "op": "in", "value": ["Britain", "Ireland"]},
                                {"field": "labels", "op": "eq", "value": "Mathematician"} ]},
                      {"not": {"field": "summary", "op": "missing"}},
                      {"linked": {"link_type": "WORKS_AT", "direction": "out", "node": "n_40"}} ] },
  "fields": ["prop:born", "degree(KNOWS)"],
  "sort": [{"by": "prop:born", "order": "asc"}],
  "limit": 50 }
```

**Fields** — all targets: `id`, `status`, `created_at`, `updated_at`, `prop:<key>`,
`evidence_count`. Nodes: `type`, `name`, `labels`, `degree`, `degree(TYPE)`,
`degree(TYPE, out|in|both)`. Links: `type`, `from`, `to`, `from.type`, `to.type`, `directed`.
Evidence: `kind`, `title`, `uri`, `author`, `publisher`, `language`, `source_date`, `captured_at`,
`citations`. Citations: `evidence`, `target`, `target_kind`, `prop`, `role`, `confidence`,
`evidence.kind`. Date suffixes: `.year`, `.month`, `.decade`, `.century`; `bucket(<field>, <width>)`
groups numbers.

**Operators**: `eq`, `ne`, `lt`, `lte`, `gt`, `gte`, `between` (`[low, high]`), `in`, `not_in`,
`exists`, `missing`, `contains`, `starts_with` (strings, case- and accent-insensitive). On a list
field a comparison holds if any element matches.

**Dates are intervals**: `"1815"` means the whole year. `born lt 1850` holds for 1849-12 and fails
for 1850-03; `born eq 1815` holds for 1815-12-10. A missing value matches only `missing`, `ne` and
`not_in`.

**Groups and aggregates**:

```json
{ "target": "links", "where": {"field": "type", "op": "eq", "value": "WORKS_AT"},
  "group_by": ["to"], "aggregate": [{"fn": "count"}, {"fn": "min", "of": "valid_from"}],
  "sort": [{"by": "count", "order": "desc"}], "limit": 10 }
```

`fn`: `count` (no `of`: objects; with `of`: objects having it), `count_distinct`, `sum`, `avg`,
`min`, `max`, `median`. Up to 3 `group_by`, 10 aggregates, 3 sort keys; a missing group value is its
own group (`null`). `limit` 1–1000 (default 20); `next_cursor` pages (pass it back as `cursor` with
the same request). `include_retracted: true` also considers retracted objects.

Useful questions: most connected nodes (`sort` by `degree`, desc); facts without evidence
(`evidence_count` `eq` 0); sources per domain (`target: evidence`, group by `publisher`); weak links
(`target: citations`, `confidence` `lt` 0.5); type usage (`group_by: ["type"]`).

## 7. history

`{"target": "n_1", "limit": 20}` — that object's changes, newest first, with diffs, actor and
batch (`before_seq` pages back). `{"since_seq": 88120}` — every change after a sequence number,
oldest first; keep the returned `last_seq` and pass it next time to see only what is new.

## 8. overview

`{}` — counts, node and link types with counts, evidence kinds, policies, recent batches,
conventions. `{"type": "Person"}` — one type in depth: property keys with value kinds and fill rate,
link patterns with the types at the other end, examples (statistics over a sample of up to 10 000).

## 9. Tips

- Get IDs from `search`/`resolve` first; most other tools take IDs or references.
- Ask for what you need: `detail: "brief"`, small `limit`s, `include.links.types`. Results are
  capped and say `truncated` — narrow rather than raise limits blindly.
- When answering the user, cite evidence IDs and say how many sources support a claim; mention
  contradicting citations if `support` shows any.
