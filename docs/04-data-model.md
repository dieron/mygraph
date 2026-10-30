# 04 — Data model

Status: **Draft for review** · 2026-10-24 · covers R1, R2, R3 · decisions ADR-004…ADR-007 in [decisions.md](decisions.md)

This is the logical model, independent of storage. The physical SQLite layout is in
[03-storage-and-search.md](03-storage-and-search.md).

```
                      ┌──────────────────────────── Graph (workspace, policies) ───────────────────────────┐
                      │                                                                                     │
                      │   ┌────────┐   Link (type, props, directed?)   ┌────────┐                           │
                      │   │  Node  │ ───────────────────────────────▶ │  Node  │                           │
                      │   └───┬────┘                                   └────────┘                           │
                      │       │  ▲                     ▲                                                     │
                      │       │  │ target              │ target (node, link, or one property of either)     │
                      │       │  └──────┐      ┌───────┘                                                     │
                      │       │       ┌─┴──────┴─┐   role: supports | contradicts | mentions | derived_from  │
                      │       │       │ Citation │   confidence, quote, note                                 │
                      │       │       └────┬─────┘                                                           │
                      │       │            │ evidence                                                        │
                      │       │       ┌────▼─────┐  parent (fragment of)   ┌──────────┐                      │
                      │       │       │ Evidence │ ──────────────────────▶ │ Evidence │  (e.g. the article)  │
                      │       │       └────┬─────┘                         └──────────┘                      │
                      │       │            │ content: inline text | blob (sha256) | external URI            │
                      │                                                                                     │
                      │   Change log: every create/update/retract/merge, with actor, batch and diff         │
                      └─────────────────────────────────────────────────────────────────────────────────────┘
```

---

## 1. Graph (workspace)

An isolated knowledge base. A server hosts any number of graphs; an agent works in one at a time
(passed on every call, defaulting to the server's default graph).

| Field | Notes |
|---|---|
| `name` | `[a-z0-9_-]+`, unique on the server |
| `description` | Shown to the agent in the overview |
| `policies.require_evidence` | `enforce` (default) · `warn` · `off` — see §7 |
| `policies.inline_text_limit` | Default 64 KB; longer text goes to the blob store |
| `policies.naming` | `styled` (default) · `free` — see §9 |

## 2. Node

| Field | Type | Notes |
|---|---|---|
| `id` | string, server-assigned | Short, prefixed, immutable: `n_3kq` |
| `key` | string, optional | Client-chosen natural key, unique in the graph (e.g. `person/ada-lovelace`). Enables idempotent upsert. |
| `type` | string, required | Primary type, e.g. `Person`. Exactly one. |
| `labels` | string[] | Extra Neo4j-style labels, e.g. `["Mathematician","Writer"]` |
| `name` | string, required | Canonical display name; indexed for exact, fuzzy and full-text search |
| `aliases` | string[] | Other names; indexed like `name` |
| `summary` | string, optional | Short description (a sentence or two); full-text indexed |
| `props` | map | Typed property values (§5) |
| `status` | enum | `active` · `retracted` · `merged` (with `merged_into`) |
| `version` | int | Increments on every change; used for optimistic concurrency |
| `created_at/by`, `updated_at/by` | | Record time and actor |

Why `name`, `aliases`, `summary` are first-class instead of ordinary properties: they are what
search and entity resolution run on, and agents always need them. Neo4j leaves them to convention;
we make them part of the contract.

## 3. Link

| Field | Type | Notes |
|---|---|---|
| `id` | string | `l_9x` |
| `key` | string, optional | For links that need their own identity (e.g. two separate employment periods) |
| `type` | string, required | e.g. `WORKS_AT`, `MARRIED_TO` |
| `from`, `to` | node references | |
| `directed` | bool, default `true` | `false` = **undirected / symmetric** ("both sides"): `MARRIED_TO`, `SIBLING_OF`, `COLLABORATED_WITH` |
| `props` | map | Typed values, e.g. `role`, `since` |
| `valid_from`, `valid_to` | partial dates, optional | When the relation held in the real world |
| `status`, `version`, `created_*`, `updated_*` | | As for nodes |

**Direction semantics**
- A directed link `A —WORKS_AT→ B` is found from A with `direction: out` and from B with `direction: in`.
- An undirected link is found from either end with `out`, `in` or `both`; it is returned with
  `directed: false` and its stored `from`/`to` order carries no meaning.
- A link type can be declared symmetric in the vocabulary (`type_info.symmetric`); new links of
  that type default to `directed: false`.
- Later (roadmap): inverse names (`PARENT_OF` ↔ `CHILD_OF`) so a link can be shown from the other
  end under its inverse name.

**Multiplicity** — by default a link is identified by `(type, from, to)` (for undirected links the
pair is unordered): upserting the same triple updates the existing link instead of adding a
duplicate. Supplying a `key`, or `multi: true`, creates a parallel link.

## 4. Evidence

The stored or referenced source material a fact was built from (R2, R3).

| Field | Notes |
|---|---|
| `id` | `e_7a` |
| `key` | Optional natural key, e.g. the canonical URL, for idempotent upsert |
| `kind` | `text` · `url` · `file` · `image` · `document` · `conversation` · `observation` · `dataset` · `other` |
| `title` | Human-readable title |
| `content` | Inline text up to the graph's limit; full-text indexed |
| `uri` | External reference (web page, file path, DOI, message link…) |
| `blob` | `{sha256, size, mime}` — file in the local content-addressed blob store (images, PDFs, long text) |
| `parent` | Evidence this one is a **fragment of** (e.g. a quote from an article) |
| `locator` | Where in the parent: `{page, char_start, char_end, selector, timestamp_ms, bbox}` |
| `source_date` | When the source was published or authored (partial dates allowed) |
| `captured_at` | When it was obtained / retrieved (default: now) |
| `source_version` | Source revision, ETag, document version (named so it is not confused with the record's own `version` counter) |
| `supersedes` | Earlier evidence this is a newer version of |
| `author`, `publisher`, `language`, `license` | Optional descriptive attributes |
| `reliability` | Optional 0..1 assessment of the source |
| `content_hash` | sha256 of content/blob; identical content is detected and reused |
| `meta` | Free-form map for anything else |

At least one of `content`, `uri`, `blob` is required.

**The R3 example — "a link to an article plus a text fragment of it"** is two evidence records:

```
e_1  kind=url   uri=https://example.org/article   title="…"  source_date=2024-03-02  captured_at=2026-10-24
e_2  kind=text  parent=e_1  content="…exact paragraph…"  locator={char_start: 1840, char_end: 2215}
```

A fact can cite `e_2` (the precise fragment) and still be traced to the article through `parent`.

**Immutability** — evidence content never changes after creation; only descriptive metadata can be
corrected. A changed source is a new evidence record with `supersedes` pointing at the old one, so
facts keep pointing at exactly what they were built from.

## 5. Property values

Properties are a map from key to value. JSON natives map directly; dates are explicit so a string
is never silently re-typed.

| Value | JSON form | Example |
|---|---|---|
| string | `"text"` | `"British"` |
| integer / float | number | `1815`, `3.14` |
| boolean | `true` / `false` | |
| list | array of scalars | `["en","fr"]` |
| date (partial allowed) | `{"$date": "1815-12-10"}`, `{"$date": "1815-12"}`, `{"$date": "1815"}` | Precision is kept: year, month or day |
| date-time | `{"$datetime": "2026-10-24T10:15:00Z"}` | |
| remove a property | `null` in an update | |
| *later:* quantity with unit, geo point | `{"$qty": 12.5, "unit": "kg"}`, `{"$geo": [lat, lon]}` | Roadmap |

Property keys are `lower_snake_case`. References to other nodes are **links**, never property
values.

## 6. Citation

Connects a fact to evidence. Many-to-many: a fact can have any number of citations, and one piece
of evidence can support many facts.

| Field | Notes |
|---|---|
| `id` | `c_19` |
| `evidence` | Evidence reference |
| `target` | A node, a link, **or one property of either** (`{node: "n_3kq", prop: "born"}`) |
| `role` | `supports` (default) · `contradicts` · `mentions` · `derived_from` |
| `confidence` | Optional 0..1: how strongly this evidence backs the target |
| `quote` | Optional short excerpt (≤ 1 KB) when no separate fragment evidence is created |
| `note` | Agent's remark, e.g. "inferred from the infobox birth date" |
| `created_at/by` | Who cited it and when |

Every read of a fact can include a **support summary** computed from its citations:
`{supports: 3, contradicts: 1, best_confidence: 0.9, sources: ["e_1","e_4","e_9"]}`. A weighted
support score (confidence × source reliability × recency) is a roadmap item.

## 7. Evidence policy (R2 enforcement)

`require_evidence = enforce` (default): creating a node or link without at least one citation is
rejected with a hint. This keeps R2 true for every fact in the graph.

For facts the agent concluded itself (no external source), it cites an evidence of kind
`observation` — a short note of its reasoning plus the conversation or task it came from. The
invariant still holds, and queries can tell sourced facts from agent-inferred ones
(`evidence.kind != observation`).

`warn` accepts the write and returns a warning; `off` disables the check (bulk imports).
Admin statistics always report facts without evidence.

## 8. Identity and references

**External IDs** are short and prefixed (`n_`, `l_`, `e_`, `c_` + base32 of an internal integer).
Short IDs matter: an agent reads and writes them thousands of times, and a 26-character UUID costs
several times the tokens of `n_3kq`.

Anywhere the API needs a node, it accepts any of:

| Form | Example | Notes |
|---|---|---|
| ID | `"n_3kq"` | Exact |
| Key | `{"key": "person/ada-lovelace"}` | Exact |
| Type + name | `{"type": "Person", "name": "Ada Lovelace"}` | Matches name or alias, normalized; an ambiguous match is an error that lists candidates |
| Batch reference | `"@ada"` | A temporary name given to an object created earlier in the same write batch |

## 9. Vocabulary (types, link types, property keys)

Schemaless by default, with an **implicit vocabulary** the engine maintains: every node type, link
type and property key in use, with counts and optional descriptions.

- Names are never rewritten. With `naming: styled` (the default) the engine requires one style:
  node types `PascalCase`, link types `UPPER_SNAKE`, property keys `lower_snake`; `"person"` or
  `"works at"` fails with `vocab.not_styled` and the expected spelling (`Person`, `WORKS_AT`).
  With `naming: free` names are stored as given (for imports that must keep source names).
  *(Changed 2026-10-29: earlier drafts rewrote names silently, which surprised users when
  `locationIP` came back as `location_ip`.)*
- When a write introduces a **new** type or link type, the response lists similar existing ones
  (`"new link type EMPLOYED_BY; similar: WORKS_AT (1,204 links)"`), so the agent can correct itself.
- Agents can attach descriptions to types (`type_info`) — these appear in the overview and act as
  shared conventions across sessions.
- Later (roadmap): optional strict schema per type (required props, allowed link endpoints,
  property types), type inheritance, n-ary relations.

## 10. Lifecycle, history and time

- **Retract** (soft delete): status → `retracted`; hidden from reads by default; citations kept;
  reversible. **Purge** (hard delete) is admin-only.
- **Merge** two duplicate nodes: links and citations move to the survivor, the loser's name becomes
  an alias, the loser gets `status = merged, merged_into = survivor`. References to the old ID keep
  resolving to the survivor.
- **Change log**: append-only record of every operation (`seq`, time, actor, batch, op, object,
  diff). Powers `history`, undo of a batch, audit, "what changed since seq N" (an agent resuming
  work), and later replication.
- **Two kinds of time**: *valid time* (when a fact is true in the world: `valid_from`/`valid_to` on
  links, partial dates in props) and *record time* (when we learned it: `created_at`, `updated_at`,
  change log). PoC filters on valid time; full bi-temporal "as of" queries are a roadmap item.

## 11. Invariants

1. Every active link connects two existing nodes (retracting a node retracts its links, reversibly).
2. With `enforce`, every active node and link has ≥ 1 active citation.
3. Evidence content and blobs are immutable; `content_hash` matches the stored bytes.
4. `key` is unique per object kind within a graph; IDs are never reused.
5. Every change has an actor and a change-log entry, written in the same transaction as the change.

## 12. Worked example

The agent reads a web article and records what it learned — **one** write call:

```json
{
  "graph": "research",
  "evidence": [
    { "ref": "@art", "kind": "url", "key": "https://example.org/ada",
      "uri": "https://example.org/ada", "title": "Ada Lovelace, the first programmer",
      "source_date": {"$date": "2024-03-02"}, "author": "J. Smith" },
    { "ref": "@frag", "kind": "text", "parent": "@art",
      "content": "In 1843 Lovelace published her notes on Babbage's Analytical Engine…",
      "locator": {"char_start": 1840, "char_end": 2215} }
  ],
  "nodes": [
    { "ref": "@ada", "type": "Person", "name": "Ada Lovelace",
      "aliases": ["Augusta Ada King"], "props": {"born": {"$date": "1815-12-10"}},
      "cite": ["@frag"] },
    { "ref": "@bab", "type": "Person", "name": "Charles Babbage", "cite": ["@frag"] },
    { "ref": "@ae",  "type": "Machine", "name": "Analytical Engine", "cite": ["@frag"] }
  ],
  "links": [
    { "type": "COLLABORATED_WITH", "from": "@ada", "to": "@bab", "directed": false,
      "cite": [{"evidence": "@frag", "confidence": 0.9}] },
    { "type": "WROTE_ABOUT", "from": "@ada", "to": "@ae",
      "props": {"year": 1843}, "cite": ["@frag"] }
  ]
}
```

Nodes are upserted by `(type, name/alias)` unless a `key` is given, so running the same call twice
changes nothing. The response maps each `@ref` to its ID and says what was created, matched or
updated. See [06-agent-api.md](06-agent-api.md).
