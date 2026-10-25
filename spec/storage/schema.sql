-- =====================================================================================
-- MyGraph storage format, version 1                          spec 0.1.0-draft · Draft
--
-- NORMATIVE. This file is the complete schema of one graph database file. It is equal to
-- applying spec/storage/migrations/ in order (currently only 0001_init.sql).
-- Comments marked [STORE-…] are normative rules; other comments are informative.
-- Column encodings not yet detailed here (change-log diff format, derivation of the
-- full-text columns) are specified in storage/storage-format.md (planned, M1).
-- =====================================================================================

-- [STORE-FILE-1] Each graph is one SQLite database file named "<graph name>.db"
--                (graph names: spec/model/identifiers.md, GRAPH-NAME-1/2).
-- [STORE-FILE-2] Requires SQLite 3.45.0 or later with FTS5 and the built-in JSON functions.
-- [STORE-FILE-3] The database header carries application_id 0x4D594752 ("MYGR") and
--                user_version = storage format version. Readers MUST refuse a file whose
--                application_id differs, and MUST NOT write to a file whose user_version is
--                higher than the version they implement.
-- [STORE-FILE-4] Writers use journal_mode = WAL and enable foreign_keys on every connection
--                (connection settings, not stored by this script).
-- [STORE-TXN-1]  All rows changed by one batch — records, full-text rows and change-log
--                entries — are written in one transaction.
-- [STORE-TIME-1] Columns named *_at and changelog.ts hold record time: INTEGER milliseconds
--                since 1970-01-01T00:00:00Z.
-- [STORE-ID-1]   "idx" columns hold the internal numbers of object IDs (identifiers.md).
--                AUTOINCREMENT guarantees they are never reused (ID-LIFE-2).
-- [STORE-TEXT-1] Text is stored as received after NFC normalization (NORM-TEXT-2), except
--                evidence.content, which is stored exactly as received.
-- [STORE-JSON-1] JSON columns hold UTF-8 JSON text and are NULL when empty. "props" columns
--                follow spec/model/values.md VAL-STORE-1..4. "aliases" and "labels" are
--                JSON arrays of strings. "meta" and "locator" are JSON objects.
-- [STORE-ENUM-1] Small-integer enumerations are listed next to their columns; values not
--                listed are invalid.

PRAGMA application_id = 1297696594;   -- 0x4D594752 "MYGR"
PRAGMA user_version = 1;

-- -------------------------------------------------------------------------------------
-- Graph metadata
-- [STORE-META-1] Required rows: format = 'mygraph', format_version = '1' (inserted below),
--                and, written when the graph is created: graph_name, created_at (decimal
--                milliseconds), spec_version (of the creating implementation), policies (JSON
--                object: require_evidence, inline_text_limit, normalize_types).
--                Optional: description.
-- -------------------------------------------------------------------------------------
CREATE TABLE meta (
  k TEXT PRIMARY KEY NOT NULL,
  v TEXT NOT NULL
) WITHOUT ROWID;

INSERT INTO meta (k, v) VALUES ('format', 'mygraph'), ('format_version', '1');

-- -------------------------------------------------------------------------------------
-- Actors and batches
-- -------------------------------------------------------------------------------------
CREATE TABLE actor (
  idx        INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT    NOT NULL UNIQUE,                 -- ACTOR-NAME-1
  kind       TEXT    NOT NULL CHECK (kind IN ('agent', 'user', 'admin', 'system', 'import')),
  meta       TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE batch (                                   -- "b_" IDs
  idx        INTEGER PRIMARY KEY AUTOINCREMENT,
  actor      INTEGER NOT NULL REFERENCES actor (idx),
  op         TEXT    NOT NULL CHECK (op IN ('write', 'retract', 'restore', 'merge', 'admin', 'import')),
  created_at INTEGER NOT NULL,
  note       TEXT
);

-- -------------------------------------------------------------------------------------
-- Nodes                                                                 ("n_" IDs)
-- status: 0 active, 1 retracted, 2 merged (merged_into is set exactly when status = 2)
-- -------------------------------------------------------------------------------------
CREATE TABLE node (
  idx         INTEGER PRIMARY KEY AUTOINCREMENT,
  key         TEXT    UNIQUE,                          -- KEY-1..4, NULL when none
  type        TEXT    NOT NULL,                        -- canonical node type (VOCAB)
  name        TEXT    NOT NULL,
  aliases     TEXT,
  labels      TEXT,                                    -- canonical, PascalCase like node types
  summary     TEXT,
  props       TEXT,
  status      INTEGER NOT NULL DEFAULT 0 CHECK (status IN (0, 1, 2)),
  merged_into INTEGER REFERENCES node (idx),
  version     INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
  created_at  INTEGER NOT NULL,
  created_by  INTEGER NOT NULL REFERENCES actor (idx),
  updated_at  INTEGER NOT NULL,
  updated_by  INTEGER NOT NULL REFERENCES actor (idx),
  CHECK ((status = 2) = (merged_into IS NOT NULL))
);

CREATE INDEX node_type ON node (type);

-- -------------------------------------------------------------------------------------
-- Links                                                                 ("l_" IDs)
-- directed: 1 directed (src -> dst), 0 undirected (src/dst order carries no meaning)
-- multi:    1 = created as a parallel link, never matched by (type, src, dst)
-- status:   0 active, 1 retracted
-- valid_from / valid_to: canonical date (VAL-DATE-6) or date-time (VAL-DT-3) text, or NULL
-- -------------------------------------------------------------------------------------
CREATE TABLE link (
  idx        INTEGER PRIMARY KEY AUTOINCREMENT,
  key        TEXT    UNIQUE,
  type       TEXT    NOT NULL,                         -- canonical link type (VOCAB)
  src        INTEGER NOT NULL REFERENCES node (idx),
  dst        INTEGER NOT NULL REFERENCES node (idx),
  directed   INTEGER NOT NULL DEFAULT 1 CHECK (directed IN (0, 1)),
  multi      INTEGER NOT NULL DEFAULT 0 CHECK (multi IN (0, 1)),
  props      TEXT,
  valid_from TEXT,
  valid_to   TEXT,
  status     INTEGER NOT NULL DEFAULT 0 CHECK (status IN (0, 1)),
  version    INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
  created_at INTEGER NOT NULL,
  created_by INTEGER NOT NULL REFERENCES actor (idx),
  updated_at INTEGER NOT NULL,
  updated_by INTEGER NOT NULL REFERENCES actor (idx)
);

CREATE INDEX link_src  ON link (src);
CREATE INDEX link_dst  ON link (dst);
CREATE INDEX link_type ON link (type);

-- [STORE-LINK-1] At most one active, non-multi, unkeyed link exists per (type, directed,
--                endpoints), where the endpoints of an undirected link are an unordered pair.
CREATE UNIQUE INDEX link_identity ON link (
  type,
  directed,
  CASE WHEN directed = 1 THEN src ELSE min(src, dst) END,
  CASE WHEN directed = 1 THEN dst ELSE max(src, dst) END
) WHERE status = 0 AND multi = 0 AND key IS NULL;

-- -------------------------------------------------------------------------------------
-- Evidence                                                              ("e_" IDs)
-- status: 0 active, 1 retracted
-- [STORE-EV-1] At least one of content, uri, blob_sha256 is present.
-- [STORE-EV-2] content_sha256 is the SHA-256 of the UTF-8 bytes of content, as 64 lowercase
--              hex digits; NULL when content is NULL. blob_sha256 is the SHA-256 of the blob
--              bytes, same format.
-- [STORE-EV-3] Content and blob of an evidence never change after creation; corrections to
--              descriptive columns (title, author, …) increment version.
-- [STORE-BLOB-1] A blob is a file in the server's blob store at
--              blobs/sha256/<hex 1-2>/<hex 3-4>/<all 64 hex digits>, shared by all graphs.
-- source_date: canonical date or date-time text, or NULL. language: BCP 47 tag.
-- -------------------------------------------------------------------------------------
CREATE TABLE evidence (
  idx            INTEGER PRIMARY KEY AUTOINCREMENT,
  key            TEXT    UNIQUE,
  kind           TEXT    NOT NULL CHECK (kind IN ('text', 'url', 'file', 'image', 'document',
                                                  'conversation', 'observation', 'dataset', 'other')),
  title          TEXT,
  content        TEXT,
  content_sha256 TEXT    CHECK (content_sha256 IS NULL OR
                                (length(content_sha256) = 64 AND content_sha256 NOT GLOB '*[^0-9a-f]*')),
  uri            TEXT,
  blob_sha256    TEXT    CHECK (blob_sha256 IS NULL OR
                                (length(blob_sha256) = 64 AND blob_sha256 NOT GLOB '*[^0-9a-f]*')),
  blob_size      INTEGER CHECK (blob_size IS NULL OR blob_size >= 0),
  mime           TEXT,
  parent         INTEGER REFERENCES evidence (idx),    -- fragment of
  locator        TEXT,
  source_date    TEXT,
  source_version TEXT,                                 -- revision / ETag / document version
  supersedes     INTEGER REFERENCES evidence (idx),    -- newer version of
  captured_at    INTEGER NOT NULL,
  author         TEXT,
  publisher      TEXT,
  language       TEXT,
  license        TEXT,
  reliability    REAL    CHECK (reliability IS NULL OR (reliability >= 0.0 AND reliability <= 1.0)),
  meta           TEXT,
  status         INTEGER NOT NULL DEFAULT 0 CHECK (status IN (0, 1)),
  version        INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
  created_at     INTEGER NOT NULL,
  created_by     INTEGER NOT NULL REFERENCES actor (idx),
  updated_at     INTEGER NOT NULL,
  updated_by     INTEGER NOT NULL REFERENCES actor (idx),
  CHECK (content IS NOT NULL OR uri IS NOT NULL OR blob_sha256 IS NOT NULL),
  CHECK ((content IS NULL) = (content_sha256 IS NULL)),
  CHECK ((blob_sha256 IS NULL) = (blob_size IS NULL))
);

CREATE INDEX evidence_parent         ON evidence (parent);
CREATE INDEX evidence_content_sha256 ON evidence (content_sha256);
CREATE INDEX evidence_blob_sha256    ON evidence (blob_sha256);

-- -------------------------------------------------------------------------------------
-- Citations                                                             ("c_" IDs)
-- target_kind: 0 node, 1 link             (target = node.idx or link.idx)
-- prop:        canonical property key when the citation backs one property, else NULL
-- role:        0 supports, 1 contradicts, 2 mentions, 3 derived_from
-- status:      0 active, 1 retracted
-- -------------------------------------------------------------------------------------
CREATE TABLE citation (
  idx         INTEGER PRIMARY KEY AUTOINCREMENT,
  evidence    INTEGER NOT NULL REFERENCES evidence (idx),
  target_kind INTEGER NOT NULL CHECK (target_kind IN (0, 1)),
  target      INTEGER NOT NULL,
  prop        TEXT,
  role        INTEGER NOT NULL DEFAULT 0 CHECK (role IN (0, 1, 2, 3)),
  confidence  REAL    CHECK (confidence IS NULL OR (confidence >= 0.0 AND confidence <= 1.0)),
  quote       TEXT,
  note        TEXT,
  status      INTEGER NOT NULL DEFAULT 0 CHECK (status IN (0, 1)),
  version     INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
  created_at  INTEGER NOT NULL,
  created_by  INTEGER NOT NULL REFERENCES actor (idx),
  updated_at  INTEGER NOT NULL,
  updated_by  INTEGER NOT NULL REFERENCES actor (idx)
);

CREATE INDEX citation_target   ON citation (target_kind, target);
CREATE INDEX citation_evidence ON citation (evidence);

-- [STORE-CIT-1] At most one active citation per (evidence, target, property, role).
CREATE UNIQUE INDEX citation_identity ON citation (evidence, target_kind, target, ifnull(prop, ''), role)
  WHERE status = 0;

-- -------------------------------------------------------------------------------------
-- Vocabulary notes (only items someone described; usage counts are computed, not stored)
-- kind: 0 node type, 1 link type, 2 property key            symmetric: link types only
-- -------------------------------------------------------------------------------------
CREATE TABLE type_info (
  kind        INTEGER NOT NULL CHECK (kind IN (0, 1, 2)),
  name        TEXT    NOT NULL,                        -- canonical form
  description TEXT,
  symmetric   INTEGER CHECK (symmetric IS NULL OR (kind = 1 AND symmetric IN (0, 1))),
  updated_at  INTEGER NOT NULL,
  updated_by  INTEGER NOT NULL REFERENCES actor (idx),
  PRIMARY KEY (kind, name)
) WITHOUT ROWID;

-- -------------------------------------------------------------------------------------
-- Change log (append-only)
-- op:       create, update, retract, restore, merge, purge
-- obj_kind: 0 node, 1 link, 2 evidence, 3 citation, 4 type_info, 5 graph metadata
-- obj:      idx of the object; NULL for type_info and graph metadata (identified in diff)
-- diff:     JSON, format in storage-format.md (M1)
-- [STORE-LOG-1] Rows are never updated. Rows are deleted only by an explicit admin purge of
--               history (not part of version 1).
-- -------------------------------------------------------------------------------------
CREATE TABLE changelog (
  seq      INTEGER PRIMARY KEY AUTOINCREMENT,          -- SEQ-1
  ts       INTEGER NOT NULL,
  actor    INTEGER NOT NULL REFERENCES actor (idx),
  batch    INTEGER NOT NULL REFERENCES batch (idx),
  op       TEXT    NOT NULL CHECK (op IN ('create', 'update', 'retract', 'restore', 'merge', 'purge')),
  obj_kind INTEGER NOT NULL CHECK (obj_kind IN (0, 1, 2, 3, 4, 5)),
  obj      INTEGER,
  diff     TEXT
);

CREATE INDEX changelog_obj   ON changelog (obj_kind, obj);
CREATE INDEX changelog_batch ON changelog (batch);

-- -------------------------------------------------------------------------------------
-- Full-text indexes
-- [STORE-FTS-1] Full-text tables hold one row per object that has not been purged, whatever
--               its status, with rowid = the object's idx. Filtering by status happens in
--               the engine.
-- [STORE-FTS-2] fts_node and fts_name: "aliases" is the node's aliases joined with U+000A.
--               fts_node.props_text and fts_link.props_text are derived from string property
--               values (storage-format.md, M1). fts_link.type_text is the link type with "_"
--               replaced by U+0020.
-- [STORE-FTS-3] fts_evidence is an external-content table over evidence(title, content):
--               its rows are inserted with the evidence and re-indexed when title changes.
-- [STORE-FTS-4] Tokenizer settings below are part of the format; changing them requires a new
--               storage format version.
-- -------------------------------------------------------------------------------------
CREATE VIRTUAL TABLE fts_node USING fts5 (
  name, aliases, summary, props_text,
  tokenize = 'unicode61 remove_diacritics 2',
  prefix = '2 3'
);

CREATE VIRTUAL TABLE fts_name USING fts5 (       -- fuzzy and substring matching on names
  name, aliases,
  tokenize = 'trigram remove_diacritics 1'
);

CREATE VIRTUAL TABLE fts_link USING fts5 (
  type_text, props_text,
  tokenize = 'unicode61 remove_diacritics 2'
);

CREATE VIRTUAL TABLE fts_evidence USING fts5 (
  title, content,
  content = 'evidence', content_rowid = 'idx',
  tokenize = 'unicode61 remove_diacritics 2'
);
