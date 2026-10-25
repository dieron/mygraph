# Operation: `get`

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §4.4](../../../docs/06-agent-api.md)

Fetches nodes, links, evidence and citations by reference, with a node's neighbourhood, the
support of a fact and an evidence's content, so most questions about one object take one call.

---

## 1. Request

```json
{ "refs": ["n_3kq", {"key": "person/babbage"}, {"type": "Person", "name": "Ada Lovelace"}, "e_2"],
  "detail": "normal",
  "include": { "links": {"limit": 10, "types": ["WORKS_AT"]}, "support": true,
               "evidence": {"limit": 3}, "content": {"from": 0, "max_chars": 4000} } }
```

| Field | Meaning | Default |
|---|---|---|
| `refs` | 1–100 references: an ID string; `{"key": …, "kind": …}` (`kind` one of `node`, `link`, `evidence`; default `node`); `{"type": …, "name": …}` for a node ([WRITE-MATCH-NODE-1] step 4) | required |
| `detail` | `brief`, `normal` or `full` ([OP-GET-3]) | `normal` |
| `include.links` | Nodes: neighbourhood ([OP-GET-4]); `false` turns it off. `limit` 0–500 (default 10), `types` (link types) | on, except for `brief` |
| `include.support` | Nodes and links: support summary ([OP-GET-5]) | `true`, except for `brief` |
| `include.evidence` | Nodes and links: the citing evidence ([OP-GET-6]); `limit` 0–100 (default 3) | on, except for `brief` |
| `include.content` | Evidence: `true` for the whole inline content, or `{from, max_chars}` in code points | `false` |
| `include_retracted` | Neighbourhood and evidence lists also show retracted links and citations | `false` |

**[OP-GET-1]** Invalid fields fail the call with `get.invalid_field`. A reference that does not
resolve does not fail the call: its item is `{"ref": <the reference>, "error": {"code":
"ref.not_found" | "write.ambiguous_match", "message": …}}`. A node ID of a merged node resolves to
the surviving node ([OBJ-REF-1]) and the item says `"merged_from": <the requested ID>`.

## 2. Response

```json
{ "items": [ { "id": "n_1", "type": "Person", "name": "Ada Lovelace", "…": "…",
               "support": { "supports": 2, "contradicts": 0, "sources": 2 },
               "links": { "counts": [ {"type": "WROTE_ABOUT", "direction": "out", "count": 1} ],
                          "items": [ {"id": "l_2", "type": "WROTE_ABOUT", "direction": "out",
                                      "other": {"id": "n_3", "type": "Machine", "name": "Analytical Engine"}} ],
                          "truncated": false },
               "evidence": { "items": [ {"id": "e_2", "kind": "text", "title": null, "uri": null,
                                         "role": "supports", "citation": "c_1"} ], "truncated": false } } ] }
```

**[OP-GET-2]** `items` has one entry per reference, in request order.

**[OP-GET-3]** Fields by `detail`, taken from the API record form also used in change-log diffs ([STORE-LOG-3]
and [model/objects.md](../../model/objects.md)):

| Kind | `brief` | `normal` | `full` |
|---|---|---|---|
| node | `id`, `type`, `name`, `status` | all record fields except record-time ones | all, with `version`, `created_at`, `created_by`, `updated_at`, `updated_by` |
| link | `id`, `type`, `from`, `to`, `directed`, `status` | as for nodes | as for nodes |
| evidence | `id`, `kind`, `title`, `uri`, `status` | as for nodes | as for nodes |
| citation | `id`, `evidence`, `target`, `role`, `status` | as for nodes | as for nodes |

**[OP-GET-4]** A node's `links`: `counts` lists, for each link type and direction, the number of
active links (`direction` is `out` or `in` from this node's side for directed links, `both` for
undirected ones), ordered by type then direction. `items` lists up to `limit` of those links
ordered by type, then direction, then internal number, each with the node at the other end
(`id`, `type`, `name`); a link from the node to itself has `other` = the node. `types` restricts
both. `truncated` is `true` when `items` does not show every counted link.

**[OP-GET-5]** `support` counts the target's active citations whose evidence is active, by role
(`supports`, `contradicts`, `mentions`, `derived_from`; roles with zero are omitted), and
`sources`: the number of distinct evidence among them.

**[OP-GET-6]** `evidence.items` lists up to `limit` of the target's active citations (all
citations with `include_retracted`), by internal number, each with the evidence's `id`, `kind`,
`title`, `uri`, and the citation's `role`, `prop` (when set), `quote` (when set) and `citation` ID.

**[OP-GET-7]** Evidence `content`: with `include.content`, the item gains `content` (the inline
text, or the range `[from, from + max_chars)` of it in code points), `content_length` (total code
points) and `content_truncated` (`true` when the returned text is shorter than the rest of the
content from `from`). Evidence without inline content gets `content: null`.
