# Operation: `search`

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §4.2](../../../docs/06-agent-api.md)

Ranked search over node names and aliases, node text, link text and evidence text, by spelling
and, where the server has a semantic index, by meaning. Matching and order:
[algorithms/search-ranking.md](../../algorithms/search-ranking.md).

---

## 1. Request

```json
{ "query": "babbage engine", "in": ["nodes", "evidence"], "types": ["Person", "Machine"],
  "mode": "auto", "limit": 10, "include_retracted": false }
```

| Field | Meaning | Default |
|---|---|---|
| `query` | Text, 1–512 bytes after NFC ([NORM-TEXT-1]) | required |
| `in` | Any of `nodes`, `links`, `evidence` | `["nodes"]` |
| `types` | Node types or link types, each acceptable as one of them ([VOCAB-1]); filters node and link hits | all |
| `mode` | `auto` (all match kinds), `exact` (`*:exact`), `prefix` (`*:exact`, `*:prefix`), `fuzzy` (`*:fuzzy`), `text` (`text` only: words), `semantic` (`semantic` only: meaning) | `auto` |
| `limit` | 1–500 | 20 |
| `include_retracted` | Also return retracted nodes, links and evidence (merged nodes never). Name matching covers active nodes only, so retracted nodes are found by full text | `false` |

**[OP-SEARCH-1]** Invalid fields fail with `write.invalid_field`-style reason `search.invalid_field`
naming the field; an empty or too long `query` fails with `search.query_invalid`.

**[OP-SEARCH-2]** Evidence and links have no names; for them every mode except `exact` and
`prefix` uses full text, and `auto` and `semantic` also meaning for evidence ([RANK-8]). Links
are found by full text only.

**[OP-SEARCH-5]** Matching by meaning needs the graph's semantic index ([RANK-8]), which a server
may lack (semantic search turned off, or no embedding model) or may still be building. Then `auto`
matches by spelling only, and `semantic` fails with `search.semantic_unavailable` (HTTP 503).
Whether a graph matches by meaning, and its semantic threshold, are in `overview` ([OP-OVER-6]);
for the server as a whole, in `GET /v1/graphs` ([API-GRAPH-3]).

## 2. Response

```json
{
  "hits": [
    { "kind": "node", "id": "n_3kr", "type": "Person", "name": "Charles Babbage",
      "score": 0.85, "matched": "name:prefix" },
    { "kind": "evidence", "id": "e_2", "evidence_kind": "text", "title": "Ada Lovelace",
      "score": 0.4, "matched": "text", "snippet": "…notes on Babbage's «Analytical Engine»…" },
    { "kind": "link", "id": "l_9", "type": "WROTE_ABOUT", "from": "n_1", "to": "n_3",
      "score": 0.4, "matched": "text" },
    { "kind": "evidence", "id": "e_5", "evidence_kind": "text", "title": "Letter, 1843",
      "score": 0.35, "matched": "semantic", "snippet": "I have been working very hard for you…" }
  ],
  "truncated": false
}
```

**[OP-SEARCH-3]** `hits` holds at most `limit` hits in the order of [RANK-7]. `truncated` is `true`
when more hits were found than returned. Retracted hits carry `"status": "retracted"`.

**[OP-SEARCH-4]** An evidence hit's `snippet` is up to about 12 words of the matching title or
content with matched words between `«` and `»` and elisions shown as `…`; the exact cut is
implementation-defined. For a hit matched by meaning only, it is the start of the passage closest
to the query ([RANK-8]), without marks.
