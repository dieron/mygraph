# Objects

Status: **Draft** · spec 0.1.0-draft · conformance: HTTP cases planned under `conformance/cases/write/`

Defines the graph and the four object kinds — node, link, evidence, citation — with their fields,
limits, lifecycle and invariants. How writes create and match objects is in
[algorithms/upsert-matching.md](../algorithms/upsert-matching.md). Storage columns are in
[storage/schema.sql](../storage/schema.sql). Design rationale: [docs/04-data-model.md](../../docs/04-data-model.md).

Byte limits are measured after NFC ([NORM-TEXT-2]); all text fields except evidence `content`
are NFC-normalized and must not contain `Cc` code points other than tab, LF and CR
(reason code `text.control_char`).

---

## 1. Graph

**[OBJ-GRAPH-1]** A graph has a name ([GRAPH-NAME-1]), an optional description (≤ 4096 bytes) and
these policies:

| Policy | Values | Default |
|---|---|---|
| `require_evidence` | `enforce`, `warn`, `off` | `enforce` |
| `inline_text_limit` | integer bytes, 1024 – 1 048 576 | 65 536 |
| `normalize_types` | `true`, `false` | `true` |

**[OBJ-GRAPH-2]** Policies apply to writes made after they change; existing objects are not
re-validated.

## 2. Fields every object has

**[OBJ-COMMON-1]** Nodes, links, evidence and citations have: `id` ([ID-FORM-1]), `status`,
`version`, `created_at`, `created_by`, `updated_at`, `updated_by`. Times are record time
(date-time values, [VAL-DT-3]); `*_by` are actor names ([ACTOR-NAME-1]).

**[OBJ-COMMON-2]** `version` is 1 on creation and increases by exactly 1 with every batch that
changes the object, including status changes. A batch that changes nothing about an object leaves
its `version`, `updated_at` and `updated_by` unchanged.

## 3. Node

| Field | Rules |
|---|---|
| `key` | Optional ([KEY-1]..[KEY-4]) |
| `type` | Required; node type ([VOCAB]) |
| `labels` | 0–32 items, each spelled like a node type; duplicates removed, first occurrence order kept |
| `name` | Required; 1–512 bytes; name key not empty ([NORM-NAME-5]) |
| `aliases` | 0–64 items, each 1–512 bytes with a non-empty name key. An alias whose name key equals the name key of the node's name or of an earlier alias is dropped |
| `summary` | Optional; ≤ 4096 bytes |
| `props` | Property map ([VAL]) |
| `status` | `active`, `retracted` or `merged` |
| `merged_into` | Node ID; present exactly when `status` is `merged` |

Reason codes: `node.name_too_long`, `node.alias_too_long`, `node.too_many_aliases`,
`node.too_many_labels`, `node.summary_too_long`, plus those of the referenced specs.

## 4. Link

| Field | Rules |
|---|---|
| `key` | Optional |
| `type` | Required; link type ([VOCAB]) |
| `from`, `to` | Required node references; both nodes must be `active` when the link is created or restored. `from` and `to` may be the same node |
| `directed` | `true` or `false`. When omitted on creation: `false` if the link type is declared symmetric in the vocabulary notes, otherwise `true` |
| `multi` | `true` creates a parallel link that is never matched by endpoints ([WRITE-MATCH-LINK-3]); default `false`; fixed at creation |
| `props` | Property map |
| `valid_from`, `valid_to` | Optional `date` or `datetime` values (§4.1) |
| `status` | `active` or `retracted` |

**[OBJ-LINK-1]** For an undirected link the order of `from` and `to` carries no meaning;
implementations MAY store and return them in either order, and every operation treats the pair as
unordered.

**[OBJ-LINK-2]** `type`, `from`, `to`, `directed` and `multi` never change after creation, except
that `from`/`to` are re-pointed by a merge ([OBJ-MERGE-2]).

**[OBJ-LINK-3]** If both `valid_from` and `valid_to` are present, the interval start of
`valid_from` MUST NOT be later than the interval end of `valid_to` ([VAL-DATE-5]); reason code
`link.invalid_validity`.

Reason codes: `link.endpoint_not_active`.

## 5. Evidence

| Field | Rules | Changeable |
|---|---|---|
| `key` | Optional | set once |
| `kind` | Required: `text`, `url`, `file`, `image`, `document`, `conversation`, `observation`, `dataset`, `other` | no |
| `content` | Optional inline text, ≤ the graph's `inline_text_limit` bytes, stored exactly as received ([NORM-TEXT-2]) | no |
| `uri` | Optional; ≤ 4096 bytes; an absolute URI: a scheme (`[A-Za-z][A-Za-z0-9+.-]*`) followed by `:` and at least one more character, no whitespace | no |
| `blob` | Optional `{sha256, size}` of a blob in the blob store ([STORE-BLOB-1]); its MIME type goes in `mime` | no |
| `parent` | Optional evidence reference: this evidence is a fragment of the parent | no |
| `locator` | Optional JSON object, ≤ 4096 bytes serialized; informative keys: `page`, `char_start`, `char_end`, `selector`, `timestamp_ms`, `bbox` | yes |
| `title` | ≤ 1024 bytes | yes |
| `mime` | ≤ 255 bytes, `type/subtype` | yes |
| `source_date` | `date` or `datetime` value | yes |
| `source_version` | ≤ 256 bytes | yes |
| `supersedes` | Evidence reference: this is a newer version of that evidence | yes |
| `captured_at` | `datetime` value; default: the batch time | yes |
| `author`, `publisher`, `license` | each ≤ 512 bytes | yes |
| `language` | BCP 47 tag, syntax `[A-Za-z]{2,8}(-[A-Za-z0-9]{1,8})*` | yes |
| `reliability` | number 0–1 | yes |
| `meta` | JSON object, ≤ 16 384 bytes serialized | yes |
| `status` | `active` or `retracted` | — |

**[OBJ-EV-1]** At least one of `content`, `uri`, `blob` is required; reason code
`evidence.empty`.

**[OBJ-EV-2]** Fields marked "no" never change after creation ([STORE-EV-3]); an update that tries
to change one fails with `evidence.immutable_field`. A changed source is recorded as new evidence
with `supersedes`.

**[OBJ-EV-3]** `parent` and `supersedes` must reference existing evidence (any status). Because
they must exist before the referencing evidence, chains cannot form cycles; an update of
`supersedes` that would create a cycle fails with `evidence.supersedes_cycle`.

**[OBJ-EV-4]** A blob referenced by `blob.sha256` must exist in the blob store when the evidence is
created; reason code `evidence.blob_missing`. Uploading blobs: [OP-WRITE-7] .. [OP-WRITE-9] in
`api/operations/write.md`.

Reason codes: `evidence.content_too_long`, `evidence.uri_invalid`, `evidence.field_too_long`,
`evidence.language_invalid`, `evidence.reliability_range`.

## 6. Citation

| Field | Rules |
|---|---|
| `evidence` | Required evidence reference; the evidence must be `active` when the citation is created or restored |
| `target` | Required: a node or a link, which must be `active` when the citation is created or restored |
| `prop` | Optional property key: the citation backs this one property. The property need not currently exist |
| `role` | `supports` (default), `contradicts`, `mentions`, `derived_from` |
| `confidence` | Optional number 0–1 |
| `quote` | Optional; ≤ 1024 bytes |
| `note` | Optional; ≤ 2048 bytes |
| `status` | `active` or `retracted` |

**[OBJ-CIT-1]** At most one active citation exists per (evidence, target, prop, role)
([STORE-CIT-1]).

**[OBJ-CIT-2]** `evidence`, `target`, `prop` and `role` never change after creation (except the
target re-pointing of a merge); `confidence`, `quote` and `note` may change.

**[OBJ-CIT-3]** A fact is **supported** when it has at least one active citation whose role is not
`contradicts` and whose evidence is active.

Reason codes: `citation.evidence_not_active`, `citation.target_not_active`,
`citation.confidence_range`.

## 7. Evidence policy

**[OBJ-POLICY-1]** With `require_evidence = enforce`, a batch fails with reason code
`write.evidence_required` if, at its end, a node or link **created or restored** by the batch is
not supported ([OBJ-CIT-3]). The error lists every such object.

**[OBJ-POLICY-2]** With `warn`, the same condition produces warning `UNSUPPORTED_FACT` for each
object instead of an error. With `off`, nothing is checked.

**[OBJ-POLICY-3]** Retracting citations or evidence is never refused by the policy. Facts that
lose their support in such a batch are reported with warning `UNSUPPORTED_FACT`.

## 8. Lifecycle

**[OBJ-LIFE-1]** Allowed status changes:

| Object | Changes |
|---|---|
| Node | `active` → `retracted` → `active`; `active` → `merged` (final) |
| Link, evidence, citation | `active` ↔ `retracted` |

Any other change fails with `lifecycle.invalid_transition`.

**[OBJ-LIFE-2]** Retracting a node also retracts its active links in the same batch. Restoring a
node restores exactly those links that were retracted by that cascade and are still retracted,
provided their other endpoint is active and no active link has taken their identity
([STORE-LINK-1]); links that cannot be restored are reported with warning `NOT_RESTORED`.

**[OBJ-LIFE-3]** Retracting evidence also retracts its active citations in the same batch.
Restoring evidence restores exactly those citations retracted by that cascade whose target is
active and whose identity is free ([OBJ-CIT-1]).

**[OBJ-LIFE-4]** Retracting a node or link does not change its citations; they become active again
in effect when the target is restored. Retracted objects are hidden from reads unless the request
asks for them.

**[OBJ-LIFE-5]** A retracted object cannot be modified except by restoring it; reason code
`lifecycle.retracted`.

## 9. Merge

**[OBJ-MERGE-1]** Merging node L ("merged") into node S ("survivor") requires both to be active and
distinct. After the merge, L has `status = merged` and `merged_into = S`; every reference to L
(ID or key) resolves to S, following chains of merges.

**[OBJ-MERGE-2]** Every active link with L as an endpoint is re-pointed to S. If a re-pointed,
unkeyed, non-multi link then has the same identity ([STORE-LINK-1]) as an active link of S, the
two are combined: the existing link of S stays, the re-pointed link is retracted, its citations
move to the remaining link (duplicates by [OBJ-CIT-1] are retracted), and properties combine as in
[OBJ-MERGE-4]. A link between L and S becomes a link from S to itself.

**[OBJ-MERGE-3]** Every active citation targeting L is re-targeted to S; citations that would then
duplicate an active citation of S are retracted.

**[OBJ-MERGE-4]** S receives: L's name and aliases as aliases (the alias rules of §3 apply), L's
labels (union), L's summary if S has none, and L's properties for keys S does not have. For keys
both have with values that are not equal ([VAL-EQ-1]), the merge request's `prop_conflicts`
decides: `keep_into` (default, S's value stays) or `prefer_merged` (L's value replaces it).
Each conflict is reported with warning `PROPERTY_CONFLICT`.

**[OBJ-MERGE-5]** L keeps its key on its own record; key lookups that find L follow `merged_into`.

## 10. Keys and references

**[OBJ-KEY-1]** A key may be given when an object is created or added later to an object that has
none. Changing or removing an existing key is an admin operation (planned); through the agent API
it fails with `key.immutable`. A key already used by another object of the same kind fails with
`key.taken`.

**[OBJ-REF-1]** A reference to a merged node resolves to the end of its `merged_into` chain.
A reference that resolves to no object fails with `ref.not_found`.
