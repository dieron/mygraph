# 06 — Agent API

Status: **Draft for review** · 2026-10-24 · covers R4, R9, N1–N7

This is the design and rationale. The normative contract (exact rules with IDs, JSON Schemas,
OpenAPI, MCP tool definitions) will be written from it into `spec/api/` (ADR-012,
[10-portability-and-spec.md](10-portability-and-spec.md)).

The agent API is the product. It is exposed twice with identical semantics:

- **MCP tools** — for Claude and other MCP clients (stdio or Streamable HTTP).
- **REST** — `POST /v1/graphs/{graph}/{operation}` with the same JSON bodies, for scripts and apps.

---

## 1. Design principles

1. **Few, composable tools.** Eleven tools. Every tool definition costs context tokens in every
   conversation, so breadth comes from parameters, not from more tools.
2. **One call per intention.** Reading an article should be *one* `write` (evidence + nodes +
   links + citations). Looking something up should be *one* `get` that returns the node, its
   neighbourhood and its sources. Round trips cost the agent far more time than server latency.
3. **Idempotent by default.** Writes upsert: the same call twice changes nothing.
4. **Search before write.** `resolve` answers "does this already exist?" for many names at once.
5. **Token-cheap output.** Short IDs, detail levels, field selection, a compact text format, hard
   caps with explicit truncation notices.
6. **Errors that teach.** Every error has a `code`, a `hint` with the next step, and candidates
   when a reference is ambiguous.
7. **No query language (R9).** Filters, traversals and aggregations are structured JSON
   parameters with a closed set of operators — the agent fills in a form, it does not write code.

## 2. Tools at a glance

| # | MCP tool | REST | Purpose |
|---|---|---|---|
| 1 | `overview` | `POST …/overview` | What is in this graph: counts, vocabulary, conventions, recent activity |
| 2 | `search` | `POST …/search` | Ranked search over names, aliases, summaries, properties and evidence text |
| 3 | `resolve` | `POST …/resolve` | For a list of candidate entities: existing matches with scores (dedup before write) |
| 4 | `get` | `POST …/get`, `GET …/nodes/{id}` | Fetch nodes, links, evidence by ID/key/name, with neighbourhood and sources |
| 5 | `write` | `POST …/write` | Atomic batch: evidence, nodes, links, citations, updates |
| 6 | `retract` | `POST …/retract` | Soft-delete or restore facts, citations, evidence, or everything a batch created |
| 7 | `merge` | `POST …/merge` | Merge duplicate nodes |
| 8 | `neighbors` | `POST …/neighbors` | k-hop expansion with per-hop filters → subgraph |
| 9 | `paths` | `POST …/paths` | Shortest / k-shortest paths between two nodes |
| 10 | `find` | `POST …/find` | Structured filters + group-by + aggregations + sorting |
| 11 | `history` | `POST …/history` | Change history of an object, or all changes since a sequence number |

## 3. Common conventions

| Parameter | Meaning |
|---|---|
| `graph` | Graph name. Optional when the server or API key has a default. |
| References | `"n_3kq"` · `{"key": "…"}` · `{"type": "Person", "name": "Ada Lovelace"}` · `"@ref"` (inside one `write`) |
| `detail` | `brief` (id, type, name) · `normal` (+ summary, props, support summary) · `full` (+ timestamps, versions, actors) |
| `fields` | Explicit field list, overrides `detail`: `["name", "props.born", "support"]` |
| `limit`, `cursor` | Default limit 20, hard cap 500. Responses include `next_cursor` when more exist. |
| `format` | `compact` (default for MCP text output) or `json` (default for REST) — see §6 |
| `include_retracted` | Default `false` |
| `as_of` | Valid-time filter for links (`{"$date": "1843"}`): only links valid at that date |

Every response carries `truncated: true/false`, and when true, a `hint` such as
`"412 more links of type CITES; narrow with link_types or page with cursor"`.

## 4. Tools

### 4.1 `overview`

Call first in a session. Small by default.

```json
{ "graph": "research", "detail": "normal" }
```

```json
{
  "graph": "research", "description": "History of computing",
  "counts": { "nodes": 18432, "links": 61210, "evidence": 9120, "citations": 88014 },
  "node_types": [ { "type": "Person", "count": 5120, "description": "A human being",
                    "props": [ {"key": "born", "kind": "date", "fill": 0.81}, {"key": "nationality", "kind": "string", "fill": 0.64} ] } ],
  "link_types": [ { "type": "WORKS_AT", "count": 1204, "directed": true,
                    "endpoints": [ {"from": "Person", "to": "Organization", "count": 1180} ] },
                  { "type": "MARRIED_TO", "count": 210, "directed": false } ],
  "evidence_kinds": { "url": 4100, "text": 4600, "observation": 420 },
  "policies": { "require_evidence": "enforce" },
  "recent": [ { "batch": "b_8k2", "at": "2026-10-24T09:12:00Z", "actor": "claude-research", "created": 34 } ],
  "conventions": "Node types PascalCase, link types UPPER_SNAKE, props lower_snake. Search or resolve before creating nodes."
}
```

Optional `type: "Person"` returns the full description of one type (all property keys, value
kinds, fill rates, link types it participates in, sample nodes).

### 4.2 `search`

```json
{ "query": "babbage engine", "in": ["nodes", "evidence"], "types": ["Person", "Machine"],
  "mode": "auto", "limit": 10 }
```

`mode`: `auto` (exact name → alias → fuzzy → full text, merged and ranked), `exact`, `prefix`,
`fuzzy`, `text`. `in` may include `nodes`, `links` (type + string props), `evidence`
(title + content).

```json
{
  "hits": [
    { "kind": "node", "id": "n_3kr", "type": "Person", "name": "Charles Babbage",
      "score": 0.97, "matched": "name:fuzzy", "support": {"supports": 4} },
    { "kind": "node", "id": "n_3ks", "type": "Machine", "name": "Analytical Engine",
      "score": 0.91, "matched": "name:text" },
    { "kind": "evidence", "id": "e_2", "title": "Ada Lovelace, the first programmer",
      "score": 0.66, "snippet": "…notes on Babbage's «Analytical Engine»…" }
  ],
  "truncated": false
}
```

### 4.3 `resolve`

Entity resolution for many candidates in one call, typically right before a `write`.

```json
{ "candidates": [
    { "name": "C. Babbage", "type": "Person", "hints": { "born": {"$date": "1791"} } },
    { "name": "Analytical engine" },
    { "name": "Luigi Menabrea", "type": "Person" } ] }
```

```json
{ "results": [
    { "verdict": "match", "best": { "id": "n_3kr", "name": "Charles Babbage", "score": 0.93,
                                    "why": ["alias fuzzy 0.88", "born matches"] } },
    { "verdict": "match", "best": { "id": "n_3ks", "name": "Analytical Engine", "score": 0.99, "why": ["name exact (case-insensitive)"] } },
    { "verdict": "none" } ] }
```

`verdict` is `match`, `ambiguous` (with up to 5 `candidates`) or `none`. Scoring uses exact/alias,
fuzzy, full text, type agreement and hint properties; semantic similarity joins in phase 2.

### 4.4 `get`

The workhorse read. Accepts any mix of references.

```json
{ "refs": ["n_3kq", {"key": "person/babbage"}],
  "detail": "normal",
  "include": { "links": {"limit": 20, "types": null}, "support": true, "evidence": {"limit": 3} } }
```

For a node, `include.links` returns a neighbourhood summary — counts per link type and direction —
and the top links with the other end's name, so most questions about an entity are answered in
one call. For evidence, `include.content: true` returns the inline text (or a range of it:
`{"content": {"from": 0, "max_chars": 4000}}`); blobs are returned as a download URL (REST) or,
for small images, as MCP image content.

### 4.5 `write`

Atomic batch. Sections are processed in order: `evidence`, `nodes`, `links`, `citations`,
`updates`. Any object can carry `"ref": "@name"` and be referenced later in the same batch.

```json
{
  "evidence": [ { "ref": "@frag", "kind": "text", "content": "…", "parent": "e_1" } ],
  "nodes":    [ { "ref": "@men", "type": "Person", "name": "Luigi Menabrea",
                  "summary": "Italian engineer and statesman", "cite": ["@frag"] } ],
  "links":    [ { "type": "TRANSLATED_WORK_OF", "from": "n_3kq", "to": "@men", "cite": ["@frag"] } ],
  "citations":[ { "evidence": "@frag", "target": {"node": "n_3kq", "prop": "born"}, "role": "supports" } ],
  "updates":  [ { "ref": "n_3kr", "set": {"died": {"$date": "1871-10-18"}}, "add_aliases": ["Babbage"],
                  "if_version": 7, "cite": ["@frag"] } ],
  "options":  { "dry_run": false, "on_conflict": "keep" }
}
```

**Node upsert rules** (no `id` given):
1. `key` given → upsert by key.
2. Otherwise match active nodes of the same `type` by normalized name or alias:
   none → **create**; exactly one → **match** (merge new aliases/props, add citations);
   several → `AMBIGUOUS_REFERENCE` error with candidates, unless `"create_new": true`.

**Property conflicts** on match/update (`on_conflict`): `keep` (default — existing value stays,
response warns and shows both values), `overwrite`, or `error`. The agent can then record the
disagreement explicitly with a `contradicts` citation.

**Links** upsert by `(type, from, to)` unless `key` or `multi: true` is given.

**Blobs**: evidence may carry `blob_base64` (≤ 10 MB) or, for the local stdio server only,
`file_path` (the server reads the file). REST also supports `POST …/blobs` (multipart) returning a
hash to reference.

**`dry_run: true`** returns the full plan (what would be created / matched / updated, warnings)
without writing — useful before large batches.

Response:

```json
{
  "batch": "b_8k3",
  "refs": { "@frag": "e_2", "@men": "n_51a" },
  "created": { "evidence": 1, "nodes": 1, "links": 1, "citations": 4 },
  "matched": { "nodes": 0 }, "updated": { "nodes": 1 },
  "warnings": [
    { "code": "NEW_VOCABULARY", "message": "New link type TRANSLATED_WORK_OF",
      "similar": [ {"type": "TRANSLATED", "count": 12} ] } ]
}
```

### 4.6 `retract`

```json
{ "targets": ["l_9x", "c_19"], "reason": "Source was a satire site", "action": "retract" }
{ "batch": "b_8k3", "action": "retract" }
{ "targets": ["l_9x"], "action": "restore" }
```

Retracting a node retracts its links (reversibly). Retracting evidence retracts its citations;
facts left without support are reported (and, under `enforce`, listed for the agent to fix).

### 4.7 `merge`

```json
{ "into": "n_3kr", "merge": ["n_77f"], "prop_conflicts": "keep_into" }
```

Moves links and citations, adds the merged names as aliases, marks the merged node
`merged_into`. Old IDs keep resolving to the survivor.

### 4.8 `neighbors`

```json
{ "start": ["n_3kq"],
  "steps": [ { "link_types": ["COLLABORATED_WITH", "WORKS_AT"], "direction": "both" },
             { "link_types": ["WORKS_AT"], "direction": "out", "node_types": ["Organization"] } ],
  "limit_per_node": 25, "limit": 200, "as_of": {"$date": "1845"},
  "include": { "support": true } }
```

Instead of explicit `steps`, a uniform expansion can be requested with `depth`, `link_types`,
`direction`, `node_types`. Returns a subgraph (`nodes`, `links`) with per-hop truncation notes.

### 4.9 `paths`

```json
{ "from": "n_3kq", "to": {"type": "Person", "name": "Alan Turing"},
  "max_length": 5, "link_types": null, "direction": "both", "mode": "k_shortest", "k": 3 }
```

`mode`: `shortest`, `all_shortest`, `k_shortest`. Returns each path as alternating nodes and links,
with support summaries so the agent can judge the weakest link of a chain.

### 4.10 `find` — filters, aggregations, calculations

The no-query-language answer to R9. A closed set of filter operators, computed values and
aggregate functions.

```json
{
  "target": "nodes",
  "where": {
    "type": "Person",
    "text": "mathematician",
    "props": [ { "key": "born", "op": "lt", "value": {"$date": "1850"} },
               { "key": "nationality", "op": "in", "value": ["British", "Irish"] } ],
    "linked": [ { "link_type": "MEMBER_OF", "direction": "out",
                  "node": {"type": "Organization", "name": "Royal Society"} } ],
    "evidence": { "min_sources": 2, "exclude_kinds": ["observation"], "contradicted": false }
  },
  "group_by": ["prop:nationality", "prop:born.decade"],
  "aggregate": [ {"fn": "count"},
                 {"fn": "avg", "of": "prop:born.year"},
                 {"fn": "avg", "of": "degree(COLLABORATED_WITH, both)"} ],
  "sort": [ {"by": "count", "order": "desc"} ],
  "limit": 20
}
```

| Piece | Allowed values |
|---|---|
| `target` | `nodes`, `links`, `evidence`, `citations` |
| Filter ops | `eq`, `ne`, `lt`, `lte`, `gt`, `gte`, `between`, `in`, `not_in`, `exists`, `missing`, `contains`, `starts_with`, `text` · combinators `all`, `any`, `not` |
| Structural filters | `type`, `labels`, `linked` (has a neighbour matching…), `evidence` (min sources, kinds, contradicted, cited by a given evidence), `created_after/before`, `updated_after/before`, `actor` |
| Computed values | `prop:x`, `prop:date.year/.month/.decade/.century`, `degree(link_type?, direction?)`, `evidence_count`, `contradictions`, `name.length`, `bucket(prop:x, width)` |
| Link-specific | `from`, `to`, `from.type`, `to.type`, `link_type` (for `group_by` and filters) |
| Aggregates | `count`, `count_distinct`, `sum`, `avg`, `min`, `max`, `median`, `percentile(p)`, `stddev`, `histogram(bucket)`, `top(k)` |
| Output | rows (when no `group_by`/`aggregate`), or groups with aggregate values |

Examples the agent can express without a query language:
- "How many people per organization?" → `target: links, where: {link_type: WORKS_AT}, group_by: ["to"], aggregate: [count]`.
- "Facts supported only by my own observations" → `where: {evidence: {only_kinds: ["observation"]}}`.
- "Which sources contributed most facts last week?" → `target: citations, where: {created_after: …}, group_by: ["evidence"], aggregate: [count]`.

Graph algorithms (PageRank, communities, centrality) come later as an `analyze` tool (roadmap).

### 4.11 `history`

```json
{ "target": "n_3kq", "limit": 20 }
{ "since_seq": 88120, "limit": 100 }
```

Returns change-log entries (`seq`, time, actor, batch, op, diff). `since_seq` lets an agent that
resumes work ask "what changed since I last looked?"

## 5. Errors and warnings

```json
{ "error": { "code": "AMBIGUOUS_REFERENCE",
             "message": "2 active Person nodes are named 'John Smith'",
             "hint": "Pass an id, a key, or set create_new: true for a different person",
             "candidates": [ {"id": "n_12", "summary": "British economist"}, {"id": "n_98", "summary": "US painter"} ] } }
```

| Error code | When |
|---|---|
| `INVALID_REQUEST` | Schema or value problem; message names the field |
| `NOT_FOUND` | Reference does not resolve; hint suggests `search` |
| `AMBIGUOUS_REFERENCE` | Name matches several; candidates listed |
| `EVIDENCE_REQUIRED` | Fact without citation under `enforce`; hint shows how to cite an `observation` |
| `VERSION_CONFLICT` | `if_version` mismatch; current version and diff returned |
| `LIMIT_EXCEEDED` | Batch or result too large; states the limit |
| `GRAPH_NOT_FOUND`, `FORBIDDEN`, `WARMING_UP`, `INTERNAL` | As named |

| Warning code | When |
|---|---|
| `NEW_VOCABULARY` | A new type / link type / property key, with similar existing ones |
| `PROPERTY_CONFLICT` | Incoming value differs from the stored one (`on_conflict: keep`) |
| `DUPLICATE_SUSPECT` | A created node is fuzzy-similar to an existing one; suggests `merge` |
| `UNSUPPORTED_FACT` | A retract left a fact without active evidence |
| `TRUNCATED` | Results capped; hint says how to narrow or page |

## 6. Compact text format

MCP tool results default to a compact, line-oriented rendering (the structured JSON is also
attached as `structuredContent`). Roughly 3–5× fewer tokens than pretty JSON for typical reads:

```
n_3kq Person "Ada Lovelace" aka "Augusta Ada King" | born=1815-12-10 | src 3, contra 0
  ↔ COLLABORATED_WITH  n_3kr Person "Charles Babbage"        [l_9x src 1 conf .9]
  → WROTE_ABOUT        n_3ks Machine "Analytical Engine" {year=1843}  [l_9y src 1]
  links: COLLABORATED_WITH 4 · WROTE_ABOUT 2 · MEMBER_OF 1   (all shown)
  sources: e_2 text "In 1843 Lovelace published…" ⊂ e_1 url example.org/ada (2024-03-02)
```

## 7. Server instructions shipped to agents (draft)

Sent as MCP server `instructions` so any connected agent learns the rules:

> This is a knowledge graph with evidence. Start with `overview`. Before creating entities, use
> `resolve` (many names at once) or `search`. Record what you learn with one `write` per source:
> add the source as evidence (URL and/or the exact text fragment), then nodes and links that cite
> it. If a fact is your own conclusion, cite an `observation` evidence explaining why. Reuse
> existing types and link types from the overview. Use `get` with `include.links` to explore an
> entity, `find` for counts and statistics, `paths` to explain connections. Retract instead of
> deleting; mistakes can be restored.

## 8. Familiarity: mapping from the MCP reference memory server

| Memory server tool | Here |
|---|---|
| `create_entities` | `write.nodes` |
| `create_relations` | `write.links` |
| `add_observations` | `write.evidence` (kind `observation`/`text`) + `cite` |
| `delete_entities` / `delete_relations` / `delete_observations` | `retract` |
| `search_nodes` | `search` |
| `open_nodes` | `get` |
| `read_graph` | `overview` + `find` (never dumps the whole graph) |

An importer for the memory server's JSONL file is planned (roadmap), so existing memories can move
over.
