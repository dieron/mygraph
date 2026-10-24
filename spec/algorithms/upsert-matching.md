# Upsert matching

Status: **Draft** · spec 0.1.0-draft · conformance: HTTP cases planned under `conformance/cases/write/`

How each item of a `write` request is resolved to an existing object (**match**) or a new one
(**create**), and how incoming fields combine with stored ones. The request format is in
`api/operations/write.md` (planned, M1); objects are defined in
[model/objects.md](../model/objects.md).

Goal *(informative)*: the same write sent twice changes nothing the second time, and an agent that
does not know IDs can still write without creating duplicates.

---

## 1. Processing order

**[WRITE-ORDER-1]** A write is processed in this section order: `evidence`, `nodes`, `links`,
`citations`, `updates`; within a section, in array order. All of it is one atomic batch
([STORE-TXN-1]): if any item fails, nothing is written.

**[WRITE-ORDER-2]** An item may define a batch reference with `"ref": "@name"` ([REF-1]). A batch
reference can be used only by items processed later; using one that is not yet defined fails with
`ref.undefined`; defining one twice fails with `ref.duplicate`. A matched item binds its reference
to the matched object.

**[WRITE-ORDER-3]** Objects created earlier in the same batch are visible to matching of later
items (a second item with the same name matches the node created by the first).

## 2. Common rule for combining a field

**[WRITE-FIELD-1]** When a matched item carries a field, the outcome is:

| Stored | Incoming | Result |
|---|---|---|
| absent | present | set |
| equal ([VAL-EQ-1]; text: identical) | present | no change |
| different | present | decided by the request's `on_conflict` |
| any | absent | no change |

**[WRITE-FIELD-2]** `on_conflict` is `keep` (default: stored value stays, warning
`PROPERTY_CONFLICT` with both values), `overwrite` (incoming value replaces it) or `error`
(the batch fails with `write.property_conflict`). It applies to properties and to single-valued
fields that may change (node `summary`; link `valid_from`/`valid_to`; evidence changeable fields;
citation `confidence`, `quote`, `note`).

**[WRITE-FIELD-3]** A property value `null` (or `[]`) in a matched item removes the property
([VAL-KIND-2]) regardless of `on_conflict`.

## 3. Nodes

**[WRITE-MATCH-NODE-1]** A node item is resolved in this order:

1. `id` given → that node (following merges, [OBJ-REF-1]). Not found → `ref.not_found`.
2. `key` given → the node with that key (following merges). If none exists, the item **creates** a
   node with that key — no name matching happens.
3. `create_new: true` → **create**.
4. Otherwise, **name matching**: the active nodes whose canonical `type` equals the item's
   canonical type and whose name or any alias has the same name key ([NORM-NAME-7]) as the item's
   `name`:
   - none → **create**;
   - exactly one → **match**;
   - more than one → fail with `write.ambiguous_match`, listing the candidates (ID, name,
     summary).

Only the item's `name` is used for matching, not its aliases.

**[WRITE-MATCH-NODE-2]** If steps 1 or 2 find a node that is `retracted`, the batch fails with
`lifecycle.retracted` (hint: restore it first). Name matching ignores retracted nodes.

**[WRITE-MATCH-NODE-3]** A matched node combines:

- `name`: the stored name stays. If the incoming name is not identical (after NFC) to the stored
  name, it is added as an alias (alias rules of [model/objects.md §3](../model/objects.md#3-node)).
- `aliases`, `labels`: union, using the de-duplication rules of objects.md §3.
- `summary`, `props`: [WRITE-FIELD-1].
- `key`: set if the node has none ([OBJ-KEY-1]); a different existing key fails with `key.immutable`.
- `type` is never changed by a match (with `id` or `key`, a different incoming type fails with
  `write.type_mismatch`).

## 4. Links

**[WRITE-MATCH-LINK-1]** `from` and `to` are resolved first; each must be an active node
(`link.endpoint_not_active`). `type` is canonicalized; `directed` defaults as in
[model/objects.md §4](../model/objects.md#4-link).

**[WRITE-MATCH-LINK-2]** A link item is resolved in this order:

1. `id` given → that link.
2. `key` given → the link with that key; none → **create** with that key.
3. `multi: true` → **create** (a parallel link).
4. Otherwise the active, non-multi, unkeyed link with the same identity — (`type`, `directed`,
   endpoints, unordered when undirected) ([STORE-LINK-1]) — is **matched**; if none, **create**.

**[WRITE-MATCH-LINK-3]** With `id` or `key`, incoming `type`, `from`, `to`, `directed` that differ
from the stored link fail with `write.link_mismatch`. A retracted link found by `id` or `key` fails
with `lifecycle.retracted`; a retracted link with the same identity is ignored by step 4 (a new link
is created).

**[WRITE-MATCH-LINK-4]** A matched link combines `props`, `valid_from` and `valid_to` by
[WRITE-FIELD-1].

## 5. Evidence

**[WRITE-MATCH-EV-1]** An evidence item is resolved in this order:

1. `id` given → that evidence.
2. `key` given → the evidence with that key; none → **create** with that key.
3. Otherwise **content matching**: the active evidence with the same `kind`, the same `uri`, the
   same blob SHA-256, the same `parent` and the same SHA-256 of `content` (each "same" including
   both absent) is **matched**; if none, **create**. If several match, the one with the lowest
   internal number is used.

**[WRITE-MATCH-EV-2]** A matched evidence combines its changeable fields
([model/objects.md §5](../model/objects.md#5-evidence)) by [WRITE-FIELD-1]. With `id` or `key`,
an incoming immutable field that differs fails with `evidence.immutable_field`.

## 6. Citations

Citations come from the `citations` section and from the `cite` shorthand on node, link and update
items, which is processed right after its item and cites the item's object.

**[WRITE-MATCH-CIT-1]** A citation item is matched by its identity (evidence, target, prop, role)
among active citations ([OBJ-CIT-1]); if none, it is **created**. A matched citation combines
`confidence`, `quote` and `note` by [WRITE-FIELD-1].

**[WRITE-MATCH-CIT-2]** The `cite` shorthand accepts an evidence reference (string) or an object
`{evidence, prop?, role?, confidence?, quote?, note?}`.

## 7. Result reporting

**[WRITE-RESULT-1]** For every item the result states its outcome — `created`, `matched` (no
change), or `updated` (matched and changed) — and the object's ID; batch references map to IDs.

**[WRITE-RESULT-2]** With `dry_run: true`, all checks and matching run and the result is reported
exactly as for a real write, but nothing is stored; IDs of objects that would be created are
reported as `null`.
