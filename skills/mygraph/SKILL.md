---
name: mygraph
description: How to use MyGraph, a knowledge graph where every fact cites its evidence, through its MCP tools (graphs, overview, search, resolve, get, write, retract, merge, neighbors, paths, find, history, stats) or its REST API. Use this skill whenever the user mentions MyGraph, "the graph", "my knowledge graph", or asks to record, remember, store or ingest facts from an article, web page, document, transcript or conversation; to look up what is known about a person, organization, place or topic; to explain how two things are connected; to count, group or compare stored entities; to find the sources behind a claim; or to correct, merge or retract stored facts — even when they don't name the tools.
---

# MyGraph

MyGraph stores **nodes** (entities: people, organizations, works, places, concepts…) and **links**
(typed relations between nodes, directed or not), both with properties. Every fact is backed by
**evidence** (a source: URL, text fragment, document, file, or your own observation) through
**citations** (evidence → node or link, with a role such as `supports` or `contradicts`). The point
of the graph is that every answer can be traced to a source, so recording and citing evidence
carefully is the core of using it well.

One server holds several **graphs** (separate databases). Nothing is ever deleted: wrong facts are
*retracted* (restorable) and duplicates are *merged*.

IDs are short strings with a kind prefix: `n_3kq` node, `l_9x` link, `e_2` evidence, `c_1` citation,
`b_8k` batch. They are not sequential decimal numbers — never guess an ID; get it from a result.

## 1. Start of a session

1. `graphs` — which graphs exist, and which one the tools use when you pass no `graph`. If the
   user's topic has no graph yet, create one: `graphs {"action": "create", "name": "ai-history"}`
   (names: lowercase letters, digits, `-`, `_`). Pass `graph` on every call when working in a
   graph that is not the default.
2. `overview` — counts, node types, link types and property keys already in use, with their
   conventions. **Reuse that vocabulary**: if `Person` and `WORKS_AT` exist, don't invent `Human`
   or `EMPLOYED_BY`. `overview {"type": "Person"}` shows a type's property keys, fill rates, typical
   links and examples — check it before adding properties to a type.

## 2. Recording a source — one `write` per source

The best write takes one source and records everything you learned from it in a single atomic
call: the evidence, the nodes, the links, and the citations that connect them.

**Before writing**, call `resolve` with every entity you are about to create (up to 100 at once).
For each one it answers `match` (use that node's ID), `ambiguous` (look at the candidates; pick one,
or create a new node with `create_new: true` if it is really someone else), or `none` (create it).
Writes also match nodes by type + name/alias on their own, so `resolve` is mainly how you avoid
near-duplicates like "C. Babbage" vs "Charles Babbage" and catch type mismatches.

```json
{
  "evidence": [
    { "ref": "@page", "kind": "url", "uri": "https://example.org/ada", "title": "Ada Lovelace — biography",
      "source_date": {"$date": "2024-03-02"} },
    { "ref": "@quote", "kind": "text", "parent": "@page",
      "content": "In 1843 Lovelace published her notes on Babbage's Analytical Engine." }
  ],
  "nodes": [
    { "ref": "@ada", "type": "Person", "name": "Ada Lovelace", "aliases": ["Augusta Ada King"],
      "props": { "born": {"$date": "1815-12-10"} }, "cite": ["@quote"] },
    { "ref": "@bab", "id": "n_3kr" },
    { "ref": "@ae", "type": "Machine", "name": "Analytical Engine", "cite": ["@quote"] }
  ],
  "links": [
    { "type": "WROTE_ABOUT", "from": "@ada", "to": "@ae", "props": { "year": 1843 }, "cite": ["@quote"] },
    { "type": "COLLABORATED_WITH", "from": "@ada", "to": "@bab", "directed": false,
      "cite": [{ "evidence": "@quote", "confidence": 0.9 }] }
  ]
}
```

What matters here, and why:

- **Evidence first, specific and verbatim.** Put the page as a `url` evidence and the exact sentence
  that supports the facts as a `text` evidence with `parent` = the page. A precise fragment makes
  the fact checkable later; a bare URL does not say which part supports what.
- **`ref`s** (`"@ada"`) name items inside one write so links and citations can point at them; the
  response's `refs` maps each to its new or matched ID.
- **Existing nodes** are referenced by ID (`{"ref": "@bab", "id": "n_3kr"}` or directly `"from": "n_3kr"`),
  by key (`{"key": "…"}`), or by `{"type": "Person", "name": "Charles Babbage"}`.
- **Every new node and link needs a citation** (`cite`), or the write fails with
  `write.evidence_required` (the default policy). For your own inference, add an evidence of kind
  `observation` whose `content` explains the reasoning, and cite that — it keeps your conclusions
  distinguishable from what sources say.
- **Naming**: node types `PascalCase` singular (`Person`, `Organization`), link types
  `UPPER_SNAKE_CASE` verbs (`WORKS_AT`, `BORN_IN`), property keys `lower_snake_case`. The server
  canonicalizes spelling ("works at" → `WORKS_AT`) but cannot merge synonyms — that is your job.
- **Values**: strings, numbers, booleans, lists of one kind; dates as `{"$date": "1815-12-10"}`
  (also `"1815-12"` or `"1815"` when that's all the source says — don't invent precision); times as
  `{"$datetime": "2024-03-02T10:00:00Z"}`.
- **Links**: `directed` defaults to `true` (`from` → `to`); use `false` for symmetric relations
  (`MARRIED_TO`, `COLLABORATED_WITH`). Use `valid_from` / `valid_to` for relations that held for a
  period (`WORKS_AT` 1990–1995).
- **Writes are idempotent**: repeating the same write changes nothing, so it's safe to retry.
  Existing property values are kept when a write disagrees (warning `PROPERTY_CONFLICT`); use
  `options: {"on_conflict": "overwrite"}` or an explicit update to change them.
- **Read the warnings.** `NEW_VOCABULARY` with `similar` names means you probably meant an existing
  type — fix it before more data piles up (retract the batch, write again). `new vocabulary: …`
  lines without similar names are expected in a young graph.
- Unsure about a big write? Add `"options": {"dry_run": true}` to see the outcome without storing.

Full field reference (all item fields, updates, citations with roles and quotes, matching rules,
limits): **read `references/write.md`** before writes that go beyond the pattern above.

## 3. Answering questions from the graph

Answer from the graph, not from memory, when the user asks about what is recorded — and **cite
evidence IDs** (e.g. "born 1815 [e_2]") so the user can check. Pick the tool by the question:

| Question | Tool |
|---|---|
| "What do we know about X?" | `search` (or `resolve`) for the ID, then `get` with `include: {"links": {"limit": 20}, "evidence": {"limit": 5}}` |
| "Is X in the graph?" / dedupe | `resolve` (many names at once) |
| Find by words in names, summaries or source text | `search` with `in: ["nodes", "evidence"]`, `mode: "text"` for full text |
| "Who/what is around X?" | `neighbors` (`depth` 1–2, filter `link_types`, `direction`) |
| "How is X connected to Y?" | `paths` (`mode: "k_shortest"`, `k: 3` for alternatives) |
| Lists, counts, statistics | `find` (filters, `group_by`, aggregates, `sort`) |
| "Where does this claim come from?" | `get` the link or node with `include: {"evidence": {"limit": 10}, "support": true}`, then `get` the evidence with `include: {"content": true}` |
| "What changed?" | `history` (`target`, or `since_seq` from a previous `last_seq`) |
| "How big / connected / well sourced is the graph?" | `stats` (counts, degree and hubs, growth per day, evidence domains, uncited and most-cited evidence) |

`find` example — people born before 1850 per country:

```json
{ "target": "nodes",
  "where": { "all": [ { "field": "type", "op": "eq", "value": "Person" },
                      { "field": "prop:born", "op": "lt", "value": {"$date": "1850"} } ] },
  "group_by": ["prop:country"], "aggregate": [{"fn": "count"}], "sort": [{"by": "count", "order": "desc"}] }
```

Query details (all search modes, `get` includes, `neighbors` steps, `paths` modes, every `find`
field, operator and aggregate, date semantics, paging): **read `references/query.md`**.

## 4. Correcting the graph

- **Change a field**: `write` with `updates`: `{"target": {"node": "n_1"}, "set": {"born": {"$date": "1815"}}}`
  (also `unset`, `rename`, `summary`, `add_aliases`, `remove_aliases`; links: `set`, `valid_from`…).
- **Wrong fact**: `retract` `{"targets": ["l_9x"], "reason": "source was satire"}`. Retracting a node
  also retracts its links, and retracting evidence its citations; `{"action": "restore", "targets": [...]}`
  brings back the object and exactly what the cascade took. A retracted object keeps its citations
  (so it may still show `sources 1`) — that records why it was believed, not that it is current.
  Undo a whole write: `retract {"batch": "b_8k3"}`.
- **Duplicates**: `merge {"into": "n_3kr", "merge": ["n_77f"]}` — links, citations, aliases and
  properties move to the survivor; the old ID keeps resolving to it.
- **Contradicting sources**: don't overwrite; cite the second source with role `contradicts`
  (`citations: [{"evidence": "@src2", "target": {"node": "n_1"}, "prop": "born", "role": "contradicts"}]`)
  so both views stay visible.

## 5. Reading results

Results are compact text, one object per line:
`n_1 Person "Ada Lovelace" · born=1815-12-10 · sources 2 (supports 2)`,
`-> WROTE_ABOUT n_3 Machine "Analytical Engine" (l_2)`, `[retracted]` marks inactive objects.
`truncated: true` or `… more` means there is more — narrow the query or page with `cursor`. Pass
`format: "json"` when you need exact structure (for example the full `refs` map of a large write).

Errors come back as `error <code>: <message>` plus a `hint:` with the next step — follow the hint.
Common ones: `ref.not_found` (find the ID with `search`/`resolve`), `write.ambiguous_match` (pass an
ID from the candidates or `create_new`), `write.evidence_required` (cite evidence),
`graph.not_found` (call `graphs`), `*.invalid_field` (the message names the field; see the
references). A failed write stores nothing, so fix and resend the whole call.

## 6. Without the MCP tools

If the tools are not connected, use the REST API — it has the same operations with the same JSON:
`POST $MYGRAPH_URL/v1/graphs/{graph}/{operation}` with `Authorization: Bearer $MYGRAPH_KEY`. Take
the server address and key from the environment variables `MYGRAPH_URL` and `MYGRAPH_KEY`
(without `MYGRAPH_URL`, try `http://127.0.0.1:7420`); never print the key or write it into files.
Graph administration (create, snapshot, export/import) is under `/admin`. See
**`references/rest.md`**.

```bash
curl -s "${MYGRAPH_URL:-http://127.0.0.1:7420}/v1/graphs" -H "Authorization: Bearer $MYGRAPH_KEY"
```

To connect a client (Claude Code, Claude desktop, Cursor, VS Code Copilot, Codex) or install this
skill elsewhere, see **`references/setup.md`**.
