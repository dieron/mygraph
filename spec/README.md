# MyGraph specification

Spec version: see [VERSION](VERSION) (`0.1.0-draft`) · storage format version: `1`

This folder is the **normative definition** of MyGraph: what any implementation, in any language,
must do and store. It never refers to a particular implementation. Rationale and design history
live in [`../docs/`](../docs/); executable checks live in [`../conformance/`](../conformance/).

## 1. Normative language

The key words MUST, MUST NOT, REQUIRED, SHALL, SHALL NOT, SHOULD, SHOULD NOT, RECOMMENDED, MAY and
OPTIONAL are to be interpreted as described in BCP 14 (RFC 2119, RFC 8174) when, and only when,
they appear in all capitals.

- Text is normative unless a section or paragraph is marked *(informative)*.
- Examples are informative. **Test vectors in `conformance/vectors/` are normative**: if prose and a
  vector disagree, the vector is right and the prose has a bug to fix.

## 2. Rule IDs

Every normative rule carries a stable ID in brackets, e.g. **[ID-CODEC-3]**.

- Format: `AREA-TOPIC-N`. `N` starts at 1 per topic.
- IDs are never renumbered or reused. A rule that is removed stays in place, marked *withdrawn*.
- Conformance cases and test vectors list the rule IDs they check; implementations cite rule IDs in
  code comments at the implementing site.

| Area | Topic | File |
|---|---|---|
| `ID`, `KEY`, `REF`, `GRAPH`, `ACTOR`, `SEQ` | Object IDs, natural keys, batch references, graph and actor names, change-log sequence numbers | [model/identifiers.md](model/identifiers.md) |
| `NORM` | Text input handling and name matching keys | [model/normalization.md](model/normalization.md) |
| `VOCAB` | Canonical spelling of node types, link types and property keys | [model/normalization.md](model/normalization.md) |
| `VAL` | Property values: kinds, JSON forms, limits, ordering, storage encoding | [model/values.md](model/values.md) |
| `STORE` | Storage format | [storage/schema.sql](storage/schema.sql) (comments are normative) |
| `OBJ` | Objects, lifecycle, evidence policy, merge | [model/objects.md](model/objects.md) |
| `WRITE` | Write processing order, matching, field combination | [algorithms/upsert-matching.md](algorithms/upsert-matching.md) |
| `VSIM` | Similar existing vocabulary for `NEW_VOCABULARY` warnings | [algorithms/vocabulary-similarity.md](algorithms/vocabulary-similarity.md) |
| `RANK` | Search matching and ranking: name keys, trigrams, match kinds, order | [algorithms/search-ranking.md](algorithms/search-ranking.md) |
| `OP` | Operations: requests, responses, errors | [api/operations/](api/operations/) |
| `FIND`, `PATH`, `API`, `ERR`, `ADMIN` | Planned (M2–M3) | — |

## 3. Common definitions

- **String**: a sequence of Unicode scalar values (code points excluding surrogates), exchanged as
  UTF-8. Lengths stated in *bytes* are UTF-8 bytes measured **after** NFC normalization; lengths
  stated in *characters* are code points.
- **Code point order**: comparison of strings code point by code point; identical to comparing
  their UTF-8 bytes.
- **JSON**: RFC 8259, restricted to I-JSON (RFC 7493): no duplicate member names, no unpaired
  surrogate escapes. A payload that violates this is invalid.
- **Reason code**: a dotted lowercase identifier for one specific failure, e.g. `id.malformed`.
  The spec and the test vectors use reason codes; the API reports them inside its error objects
  (error format: planned `api/errors.md`).

## 4. Unicode data

- Implementations MUST use Unicode Character Database data of version **15.1.0 or later** for:
  normalization forms NFC / NFKD (UAX #15), `General_Category` (UnicodeData.txt),
  `White_Space` (PropList.txt), simple case folding (CaseFolding.txt, status `C` and `S`) and
  simple case mappings (UnicodeData.txt fields 12–14).
- Conformance vectors only use characters whose relevant properties have not changed since
  Unicode 9.0, so any data set from 15.1.0 on passes them. Behaviour on characters assigned in
  later versions may differ between implementations that use different data versions; derived
  values that depend on it are never stored as the source of truth.

## 5. Versioning

| What | Scheme | Where it is visible |
|---|---|---|
| This specification | Semantic versioning. Before 1.0.0, a minor version may contain breaking changes. | `VERSION`; reported by the API (`overview`, `/health`) |
| Storage format | Integer, incremented on any change to the stored data layout | SQLite `PRAGMA user_version`; `meta` table |
| HTTP API | Major version in the path (`/v1/…`) | URLs |

## 6. Layout and status

| File | Status | Milestone |
|---|---|---|
| [model/identifiers.md](model/identifiers.md) | Draft | M0 |
| [model/normalization.md](model/normalization.md) | Draft | M0 |
| [model/values.md](model/values.md) | Draft | M0 |
| [storage/schema.sql](storage/schema.sql), [storage/migrations/](storage/migrations/) | Draft | M0 |
| [model/objects.md](model/objects.md) — graph, node, link, evidence, citation, lifecycle, merge | Draft | M1 |
| [storage/storage-format.md](storage/storage-format.md) — column encodings, FTS sync, change-log diff format, blob store | Draft | M1 |
| [algorithms/upsert-matching.md](algorithms/upsert-matching.md), [api/operations/write.md](api/operations/write.md), [retract.md](api/operations/retract.md), [merge.md](api/operations/merge.md) | Draft | M1 |
| [algorithms/vocabulary-similarity.md](algorithms/vocabulary-similarity.md) | Draft | M1 |
| [api/operations/search.md](api/operations/search.md), [get.md](api/operations/get.md), [algorithms/search-ranking.md](algorithms/search-ranking.md) | Draft | M2 |
| `api/operations/{resolve,neighbors,paths,find,history}.md`, other `algorithms/*` | Planned | M2 |
| `api/conventions.md`, `api/errors.md`, `api/schemas/`, `api/openapi.yaml`, `api/mcp-tools.json`, `admin/` | Planned | M3 |

Status values: **Draft** (open for change) → **Review** (complete, being checked against the
implementation and conformance suite) → **Stable** (changes only through a new spec version).

## 7. Changing the specification

Follow the workflow in [docs/10-portability-and-spec.md §5](../docs/10-portability-and-spec.md):
decision (ADR) → spec text and schemas → conformance cases and vectors → implementation. A change
to observable behaviour or stored data that skips the spec is a defect.
