# Operation: `write`

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §4.5](../../../docs/06-agent-api.md)

One atomic batch of evidence, nodes, links, citations and updates. Matching and field combination:
[algorithms/upsert-matching.md](../../algorithms/upsert-matching.md). Objects:
[model/objects.md](../../model/objects.md). JSON Schemas for request and response are planned in
`api/schemas/` (M3); this file defines their meaning.

---

## 1. Request

```json
{
  "graph": "research",
  "evidence":  [ EvidenceItem, … ],
  "nodes":     [ NodeItem, … ],
  "links":     [ LinkItem, … ],
  "citations": [ CitationItem, … ],
  "updates":   [ UpdateItem, … ],
  "options":   { "on_conflict": "keep", "dry_run": false }
}
```

All sections are optional. A request with no items is valid and changes nothing.

**[OP-WRITE-1]** Limits: at most 1000 items in total over all sections, counting each `cite` entry
as an item; reason code `write.too_many_items`.

### Item fields

| Item | Fields |
|---|---|
| EvidenceItem | `ref`, `id`, `key`, `kind`, `content`, `uri`, `blob` (`{sha256}` of an uploaded blob), `blob_base64` (≤ 10 MiB decoded; stored in the blob store), `parent` (evidence reference), `locator`, `title`, `mime`, `source_date`, `source_version`, `supersedes`, `captured_at`, `author`, `publisher`, `license`, `language`, `reliability`, `meta` |
| NodeItem | `ref`, `id`, `key`, `type`, `name`, `aliases`, `labels`, `summary`, `props`, `create_new`, `cite` |
| LinkItem | `ref`, `id`, `key`, `type`, `from`, `to`, `directed`, `multi`, `props`, `valid_from`, `valid_to`, `cite` |
| CitationItem | `evidence`, `target` (`{"node": ref}` or `{"link": ref}`), `prop`, `role`, `confidence`, `quote`, `note` |
| UpdateItem | `target` (`{"node"\|"link"\|"evidence"\|"citation": ref}`), `if_version`, and the changes below; plus `cite` for node and link targets |

`source_date`, `captured_at`, `valid_from`, `valid_to` use the value forms `{"$date": …}` /
`{"$datetime": …}` ([VAL]).

**References** to existing objects are: an ID string, `{"key": "…"}`, a batch reference `"@name"`,
and for nodes also `{"type": "…", "name": "…"}` (resolved by name matching,
[WRITE-MATCH-NODE-1] step 4: zero matches → `ref.not_found`, several → `write.ambiguous_match`).

**[OP-WRITE-2]** Item requirements on creation: EvidenceItem needs `kind` and one of `content`,
`uri`, `blob`, `blob_base64`; NodeItem needs `type` and `name`; LinkItem needs `type`, `from`, `to`;
CitationItem needs `evidence` and `target`. A missing required field fails with
`write.missing_field` naming it. A matched item may omit them.

### UpdateItem changes

| Target | Changes |
|---|---|
| node | `set` (map of properties; `null` removes), `unset` (list of keys), `rename` (new name; the old name becomes an alias unless `keep_old_name_as_alias: false`), `summary` (`null` removes), `add_aliases`, `remove_aliases` (matched by name key), `add_labels`, `remove_labels`, `set_key` |
| link | `set`, `unset`, `valid_from`, `valid_to` (`null` removes), `set_key` |
| evidence | any changeable field (`null` removes), `set_key` |
| citation | `confidence`, `quote`, `note` (`null` removes) |

**[OP-WRITE-3]** Updates are explicit: they overwrite regardless of `on_conflict`. If `if_version`
is given and differs from the object's current version, the batch fails with
`write.version_conflict` (the error carries the current version).

## 2. Response

```json
{
  "batch": "b_8k3",
  "dry_run": false,
  "refs": { "@frag": "e_2", "@men": "n_51a" },
  "items": [
    { "section": "nodes", "index": 0, "outcome": "created", "id": "n_51a" }
  ],
  "counts": { "created": { "node": 1 }, "updated": {}, "matched": {} },
  "warnings": [ { "code": "NEW_VOCABULARY", "message": "…", "details": { } } ]
}
```

**[OP-WRITE-4]** `batch` is the new batch's ID, or `null` when nothing changed or on a dry run.
`items` lists every item (including `cite` entries, with `section: "cite"` and the index of their
owner) with `outcome` ([WRITE-RESULT-1]). `counts` sums outcomes by object kind.

**[OP-WRITE-5]** Warnings defined so far: `PROPERTY_CONFLICT` ([WRITE-FIELD-2]),
`UNSUPPORTED_FACT` ([OBJ-POLICY-2]), `NEW_VOCABULARY` (the batch introduced a node type, link type
or property key not used before in the graph; `details.similar` lists up to 5 existing items by a
similarity defined in `algorithms/vocabulary-similarity.md`, planned).

## 3. Errors

**[OP-WRITE-6]** A failing batch returns one error with the reason code, a message, the failing
item (`section`, `index`), and where defined, details (candidates, current version, list of
unsupported facts). Nothing is written.
