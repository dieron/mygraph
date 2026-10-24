# Storage format details

Status: **Draft** · spec 0.1.0-draft · storage format version 1

Completes [schema.sql](schema.sql): how API values map to columns, how the full-text tables are
kept in sync, the change-log diff format, the blob store and the data directory.

---

## 1. Data directory

**[STORE-DIR-1]** A server's data directory contains:

```
<data_dir>/graphs/<graph name>.db     one SQLite file per graph (+ -wal, -shm while open)
<data_dir>/blobs/sha256/<h1h2>/<h3h4>/<64 hex digits>
```

**[STORE-DIR-2]** Creating a graph: create the file, run the migrations in order, then insert the
`meta` rows of [STORE-META-1] in the same transaction.

**[STORE-DIR-3]** Opening a graph: check `application_id` ([STORE-FILE-3]); if `user_version` is
lower than the implementation's format version, run the missing migrations in one transaction
before use; if higher, open read-only or refuse.

## 2. Column encodings

**[STORE-COL-1]** Enumerations:

| Column | Encoding |
|---|---|
| `node.status` | 0 `active`, 1 `retracted`, 2 `merged` |
| `link.status`, `evidence.status`, `citation.status` | 0 `active`, 1 `retracted` |
| `citation.role` | 0 `supports`, 1 `contradicts`, 2 `mentions`, 3 `derived_from` |
| `citation.target_kind`, `changelog.obj_kind` | 0 node, 1 link (and for `obj_kind`: 2 evidence, 3 citation, 4 type_info, 5 graph metadata) |
| `type_info.kind` | 0 node type, 1 link type, 2 property key |
| `link.directed`, `link.multi`, `type_info.symmetric` | 0 false, 1 true |

**[STORE-COL-2]** `*_by` columns and `changelog.actor` reference `actor.idx`; the API shows actor
names. An actor row is created the first time an actor writes to the graph.

**[STORE-COL-3]** `valid_from`, `valid_to`, `source_date` hold the canonical text of a `date`
([VAL-DATE-6]) or `datetime` ([VAL-DT-3]) value; a value containing `T` is a date-time.
`captured_at` and all `*_at` columns hold milliseconds ([STORE-TIME-1]).

**[STORE-COL-4]** `aliases` and `labels`: JSON arrays of strings in stored order, NULL when empty.
`props`: [VAL-STORE-1]. `locator`, `meta`, `actor.meta`: JSON objects, NULL when absent.

**[STORE-COL-5]** `evidence.mime` holds the MIME type of the blob when there is one, otherwise the
declared MIME type of the content or URI target.

## 3. Full-text synchronization

**[STORE-FTSYNC-1]** Derived text:

- **aliases text**: the aliases joined with U+000A.
- **props text**: for each property in key order, its `string` value, or the elements of a
  `list<string>`, each followed by U+000A; values of other kinds contribute nothing.
- **type text**: the link type with every `_` replaced by U+0020.

**[STORE-FTSYNC-2]** In the same transaction as the record change:

| Event | Action |
|---|---|
| Node created | insert `fts_node(rowid, name, aliases text, summary, props text)` and `fts_name(rowid, name, aliases text)` |
| Node `name`, `aliases`, `summary` or `props` changed | delete the node's rows from both tables and insert them again |
| Link created / `props` changed | insert / replace `fts_link(rowid, type text, props text)` |
| Evidence created | insert `fts_evidence(rowid, title, content)` |
| Evidence `title` changed | `INSERT INTO fts_evidence(fts_evidence, rowid, title, content) VALUES('delete', …old values…)`, then insert the new values |
| Status change (retract, restore, merge) | nothing ([STORE-FTS-1]) |
| Purge | delete the object's rows |

## 4. Change log

**[STORE-LOG-2]** Every change to a node, link, evidence, citation, vocabulary note or graph
metadata appends one `changelog` row in the batch's transaction, with the batch's `ts` and actor.
A batch that changes nothing appends no rows and creates no `batch` row.

**[STORE-LOG-3]** `diff` is a JSON object. Field names and values use the API's JSON form (IDs as
canonical strings, values per [VAL]):

| `op` | `diff` |
|---|---|
| `create` | `{"after": {…all fields of the new object…}}` |
| `update` | `{"before": {…changed fields, old values…}, "after": {…same fields, new values…}}` (a field that did not exist is `null` on that side) |
| `retract`, `restore` | `{"reason": <text or null>, "cascade_of": <seq of the entry that caused it, or null>}` |
| `merge` | on the merged node: `{"merged_into": "<survivor id>", "prop_conflicts": "<keep_into or prefer_merged>"}`; the effects on links, citations and the survivor are separate `update` / `retract` entries with `cascade_of` set to this entry's seq |
| `purge` | `{}` |

For `obj_kind` 4 and 5 the object is identified inside `diff`: `{"kind": "link_type", "name":
"WORKS_AT", …}` and `{"key": "<meta key>", …}`.

**[STORE-LOG-4]** Cascaded retractions ([OBJ-LIFE-2], [OBJ-LIFE-3]) are recorded with
`cascade_of` = the seq of the node or evidence retraction. Restoring finds the objects to restore
through these entries.

## 5. Blob store

**[STORE-BLOB-2]** A blob file's name is the lowercase hex SHA-256 of its bytes; its folder is
`sha256/` + first two hex digits + `/` + next two hex digits ([STORE-BLOB-1]).

**[STORE-BLOB-3]** A blob is written to a temporary file in the same folder, synced, then renamed
to its final name, before the transaction that references it commits. An existing file with the
same name is not rewritten.

**[STORE-BLOB-4]** Blobs are shared by all graphs and never modified. Unreferenced blobs are removed
only by the admin blob garbage collection.
