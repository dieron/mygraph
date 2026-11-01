-- =====================================================================================
-- Migration 0002 — storage format version 1 → 2                spec 0.1.0-draft · Draft
--
-- NORMATIVE. Full-text search matches English word forms: the word tables use the `porter`
-- stemmer over `unicode61` ("clothes" and "clothing" are both indexed as "cloth", "opened"
-- as "open"), and queries are stemmed the same way. The rows are carried over: node and link
-- rows are copied from the old tables, evidence rows are rebuilt from the evidence table they
-- index (STORE-FTS-3). Applied after 0001_init.sql, the result is identical to
-- spec/storage/schema.sql. Runs in one transaction with the version change (STORE-DIR-3).
-- =====================================================================================

ALTER TABLE fts_node RENAME TO fts_node_v1;
CREATE VIRTUAL TABLE fts_node USING fts5 (
  name, aliases, summary, props_text,
  tokenize = 'porter unicode61 remove_diacritics 2',
  prefix = '2 3'
);
INSERT INTO fts_node (rowid, name, aliases, summary, props_text)
  SELECT rowid, name, aliases, summary, props_text FROM fts_node_v1;
DROP TABLE fts_node_v1;

ALTER TABLE fts_link RENAME TO fts_link_v1;
CREATE VIRTUAL TABLE fts_link USING fts5 (
  type_text, props_text,
  tokenize = 'porter unicode61 remove_diacritics 2'
);
INSERT INTO fts_link (rowid, type_text, props_text)
  SELECT rowid, type_text, props_text FROM fts_link_v1;
DROP TABLE fts_link_v1;

DROP TABLE fts_evidence;
CREATE VIRTUAL TABLE fts_evidence USING fts5 (
  title, content,
  content = 'evidence', content_rowid = 'idx',
  tokenize = 'porter unicode61 remove_diacritics 2'
);
INSERT INTO fts_evidence (fts_evidence) VALUES ('rebuild');

UPDATE meta SET v = '2' WHERE k = 'format_version';
PRAGMA user_version = 2;
