# Export format (JSONL)

Status: **Draft** · spec 0.1.0-draft · design: [docs/07-admin-api.md §1](../../docs/07-admin-api.md)

A whole graph as one UTF-8 text file, one JSON value per line: a portable, implementation-neutral
copy that another implementation, or a later storage format, can import with every ID intact.
Snapshots ([ADMIN-GRAPH-4]) copy the database file instead; they are faster but tied to the
storage format.

---

## 1. Lines

**[EXP-1]** The first line is the header:

```json
{ "mygraph_export": 1, "graph": "research", "spec_version": "0.1.0-draft",
  "exported_at": "2026-10-25T20:00:00.000Z", "last_seq": 412230,
  "policies": { "require_evidence": "enforce", "inline_text_limit": 65536, "naming": "styled" },
  "counts": { "actors": 3, "nodes": 18432, "links": 61210, "evidence": 9120, "citations": 88014 } }
```

`mygraph_export` is the format version (this document: 1). `counts` are the numbers of lines of
each kind that follow, of any status.

**[EXP-2]** Then one line per actor, `{"actor": {"name": …, "kind": …}}` ([ACTOR-NAME-1]; kind as
in the `actor` table), then one line per node, link, evidence and citation, in that order and each
kind by internal number. An object line is the object's API record form with all fields
([STORE-LOG-3], as in a `create` diff, including `version`, `created_at`, `created_by`,
`updated_at`, `updated_by`, `status` and `merged_into`); an evidence line also has `content`: its
inline text, or `null`. Objects of every status are included.

**[EXP-3]** Not included: the change log, batches, vocabulary notes (descriptions, symmetric link
types) and blob bytes. An evidence's `blob` names its SHA-256; importing into a server whose blob
store lacks it is allowed, but downloading that blob then fails with `blob.not_found`.

## 2. Import

**[EXP-4]** Import reads such a file into a graph without objects or history (`last_seq` 0). Every
object keeps its ID, status, version, field values and record times; actors are created by name
when the graph does not know them. The importing actor's batches have `op` `import`, and each
object gets one change-log `create` entry whose `after` is the object's record (so `history` shows
when it was imported, while `created_at` keeps the original time).

**[EXP-5]** Import checks the header version (`import.unsupported_version`), every line's JSON
and kind (`import.invalid_line`, with the line number), value forms ([VAL]) and that every
reference — link endpoints, `merged_into`, evidence `parent` and `supersedes`, citation evidence
and target — names an object in the file (`import.dangling_reference`), and that an evidence's
`content` matches its `content_sha256` (`import.content_mismatch`). Field limits are not checked
again. A failed import leaves the batches it already committed; delete the graph and retry.
Objects may refer to objects later in the file; import orders them.
