# Operation: `merge`

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §4.7](../../../docs/06-agent-api.md)

Merges duplicate nodes into one. Semantics: [model/objects.md §9](../../model/objects.md#9-merge).

## 1. Request

```json
{ "graph": "research", "into": "n_3kr", "merge": ["n_77f", "n_80a"], "prop_conflicts": "keep_into" }
```

**[OP-MERGE-1]** `into` is a node reference; `merge` lists 1–100 node references. All must resolve
to distinct active nodes (a reference that resolves to `into` itself fails with
`merge.self`). `prop_conflicts` is `keep_into` (default) or `prefer_merged`.

**[OP-MERGE-2]** The nodes in `merge` are merged into `into` one after another in list order, all
in one batch. Nodes of a different `type` than `into` may be merged; the survivor keeps its type
and warning `TYPE_DIFFERS` is reported.

## 2. Response

```json
{
  "batch": "b_8k5",
  "into": "n_3kr",
  "merged": ["n_77f", "n_80a"],
  "moved": { "links": 12, "citations": 30 },
  "combined": { "links": 2, "citations": 1 },
  "warnings": [ { "code": "PROPERTY_CONFLICT", "details": { "prop": "born", "kept": {"$date": "1791"}, "dropped": {"$date": "1792"} } } ]
}
```

**[OP-MERGE-3]** `moved` counts re-pointed links and re-targeted citations; `combined` counts links
and citations retracted as duplicates ([OBJ-MERGE-2], [OBJ-MERGE-3]).
