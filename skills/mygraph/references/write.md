# Writing to MyGraph — full reference

Contents: 1. Request shape · 2. Evidence · 3. Nodes · 4. Links · 5. Citations · 6. Updates ·
7. How items are matched · 8. Values · 9. Options, limits, policy · 10. Recipes

## 1. Request shape

```json
{ "evidence": [ … ], "nodes": [ … ], "links": [ … ], "citations": [ … ], "updates": [ … ],
  "options": { "on_conflict": "keep", "dry_run": false }, "graph": "optional-graph-name" }
```

All sections are optional and processed in that order, as one atomic batch: if anything fails,
nothing is stored. The response has `batch` (ID, or `null` when nothing changed), `refs` (each
`@ref` → ID), per-item outcomes (`created`, `matched`, `updated`), counts, and warnings.

**References** to objects: an ID string (`"n_3kq"`), `{"key": "…"}`, a batch reference `"@name"`
(any item with `"ref": "@name"` in the same write), and for nodes `{"type": "Person", "name": "Ada Lovelace"}`
(name matching: no match → `ref.not_found`, several → `write.ambiguous_match` with candidates).

## 2. Evidence items

Needs `kind` and at least one of `content`, `uri`, `blob`, `blob_base64`.

| Field | Meaning |
|---|---|
| `kind` | `text`, `url`, `file`, `image`, `document`, `conversation`, `observation`, `dataset`, `other` |
| `content` | Inline text, stored exactly (default limit 64 KiB). Quote the source verbatim |
| `uri` | Absolute URI of the source |
| `parent` | Evidence reference: this is a fragment of the parent (e.g. a quote of a page) |
| `title`, `author`, `publisher`, `language` (BCP 47), `license` | Descriptive fields |
| `source_date` | When the source was published/written (`{"$date": …}`) |
| `source_version`, `supersedes` | Versions of a changing source; `supersedes` = the older evidence |
| `locator` | Where in the parent: `{"page": 3}`, `{"char_start": 120, "char_end": 410}`, `{"timestamp_ms": 61000}` |
| `reliability` | 0–1, your judgement of the source |
| `blob_base64` / `blob` | A file (≤ 10 MiB, base64 without line breaks) / `{"sha256": …}` of one already stored; set `mime` |
| `captured_at`, `meta`, `key`, `id` | Capture time (default now), free JSON, natural key, existing ID |

Patterns: page + quotes (`url` parent, one `text` child per supporting sentence, with `parent`;
not one quote of the whole page); a conversation or meeting
(`conversation` with the transcript excerpt); your own reasoning (`observation` whose `content`
says what you concluded and from what).

## 3. Node items

Needs `type` and `name` when created.

| Field | Meaning |
|---|---|
| `type` | Node type, PascalCase singular (`Person`); another spelling fails with `vocab.not_styled` and the expected one |
| `name` | Display name, ≤ 512 bytes |
| `aliases` | Other names (≤ 64); also used for matching and search |
| `labels` | Extra classifications spelled like types (`["Mathematician", "Writer"]`) |
| `summary` | One or two sentences, ≤ 4 KiB |
| `props` | Property map (§8); keys lower_snake_case |
| `cite` | Evidence references or `{evidence, prop, role, confidence, quote, note}` objects |
| `key` | Your stable natural key (`"wikidata:Q7259"`); a node with this key is reused, otherwise created |
| `id` | Refer to an existing node (to add citations, props or aliases) |
| `create_new` | `true`: always create, even if a node with this name exists (a different person with the same name) |

## 4. Link items

Needs `type`, `from`, `to` when created.

| Field | Meaning |
|---|---|
| `type` | UPPER_SNAKE_CASE verb phrase (`WORKS_AT`) |
| `from`, `to` | Node references (often `@ref`s) |
| `directed` | Default `true`; `false` for symmetric relations (order of from/to then means nothing) |
| `props` | Property map |
| `valid_from`, `valid_to` | Period when the relation held (`{"$date": "1990"}`) |
| `multi` | `true` creates a parallel link even if one with the same type/endpoints exists |
| `cite`, `key`, `id` | As for nodes |

## 5. Citations

Usually created by `cite` on nodes and links. The `citations` section cites existing objects or
single properties:

```json
{ "evidence": "@src2", "target": {"node": "n_1"}, "prop": "born", "role": "contradicts",
  "confidence": 0.6, "quote": "born in 1816", "note": "conflicts with the parish record" }
```

`role`: `supports` (default), `contradicts`, `mentions` (the source talks about it without
asserting it), `derived_from` (computed or inferred from it). A fact counts as supported when it has
an active non-`contradicts` citation whose evidence is active. `prop` ties the citation to one
property instead of the whole object.

## 6. Updates

```json
{ "updates": [
  { "target": {"node": "n_1"}, "if_version": 3,
    "set": {"born": {"$date": "1815-12-10"}, "nationality": "British"}, "unset": ["nickname"],
    "rename": "Ada King", "summary": "English mathematician", "add_aliases": ["Ada Lovelace"],
    "add_labels": ["Writer"], "cite": ["@src"] },
  { "target": {"link": "l_4"}, "valid_to": {"$date": "1852"} },
  { "target": {"evidence": "e_2"}, "title": "Corrected title" },
  { "target": {"citation": "c_7"}, "confidence": 0.5 }
] }
```

Updates overwrite (they ignore `on_conflict`). `rename` keeps the old name as an alias unless
`keep_old_name_as_alias: false`. `set` with `null` removes a property. `if_version` makes the update
fail with `write.version_conflict` if someone changed the object since you read it (get it again).
Type, endpoints and direction of a link, and the content/URI/kind of evidence, never change: retract
and write a new one instead.

## 7. How items are matched (why writes don't duplicate)

- **Node**: `id` → `key` → `create_new` → otherwise name matching: same type and the
  same normalized name (case, accents and punctuation ignored) as the node's name or an alias. One
  match → reused (new aliases/props/citations are added); none → created; several →
  `write.ambiguous_match`.
- **Link**: same type and endpoints (either order when undirected) → reused, unless `multi`.
- **Evidence**: same kind, URI, parent, blob and exact content → reused. Writing the same page and
  quote again therefore reuses them.
- **Citation**: same (evidence, target, prop, role) → reused.

Name matching is exact after normalization — "C. Babbage" does not match "Charles Babbage". Use
`resolve` first to catch such variants, then reference the existing node by ID (and add the variant
as an alias).

## 8. Values

| Kind | JSON |
|---|---|
| string, int, float, bool | as JSON (`"Paris"`, `42`, `3.5`, `true`) |
| date | `{"$date": "1815-12-10"}`, `{"$date": "1815-12"}`, `{"$date": "1815"}` (precision = what you know) |
| datetime | `{"$datetime": "2024-03-02T10:00:00Z"}` (offset required) |
| list | `["a", "b"]` — all elements of one kind, no nesting, no nulls |
| remove (in `set`) | `null` |

Objects are not property values: model structure as nodes and links. Use one unit per key and put
it in the key name when it matters (`height_m`, `revenue_usd`).

## 9. Options, limits, policy

- `on_conflict` for properties and changeable fields when a write disagrees with a stored value:
  `keep` (default; warning `PROPERTY_CONFLICT` shows both values), `overwrite`, or `error`.
- `dry_run: true`: everything is checked and reported, nothing stored.
- At most 1000 items per write, counting each `cite` entry; split bigger sources into several writes
  that reuse the same evidence (matching makes that safe).
- Evidence policy of the graph (`overview` → `policies.require_evidence`): `enforce` (default:
  every new node/link needs a supporting citation), `warn`, or `off`.

## 10. Recipes

**A second source for known facts** — reference the existing objects and just cite:

```json
{ "evidence": [{ "ref": "@s2", "kind": "url", "uri": "https://example.com/obituary" }],
  "nodes": [{ "id": "n_1", "cite": ["@s2"] }],
  "links": [{ "id": "l_2", "cite": [{ "evidence": "@s2", "confidence": 0.8 }] }] }
```

**Your own conclusion** — an observation that says why:

```json
{ "evidence": [{ "ref": "@obs", "kind": "observation",
                 "content": "Both letters (e_4, e_7) are signed by the same secretary, so they worked together." }],
  "links": [{ "type": "WORKED_WITH", "from": "n_12", "to": "n_31", "directed": false,
              "cite": [{ "evidence": "@obs", "role": "derived_from" }] }] }
```

**Same name, different entity** — `"create_new": true` plus a distinguishing `summary`.

**Fix vocabulary after a mistake** — `retract {"batch": "b_…"}` the batch that introduced the wrong
type, then write again with the right one.
