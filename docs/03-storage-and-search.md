# 03 — Storage and search options

Status: **Draft for review** · 2026-10-24 · decisions ADR-002, ADR-003 in [decisions.md](decisions.md)

Principle: **separate the durable store from the read indexes.**
The durable store only has to be correct and crash-safe. Speed comes from in-memory indexes that
are rebuilt from the store on startup and updated on every write. That makes the store
swappable, and lets us pick the easiest correct option for the PoC.

---

## 1. Durable store candidates

| Option | Kind | Pros | Cons | Verdict |
|---|---|---|---|---|
| **SQLite** (WAL mode) | Embedded SQL, B-tree | Most battle-tested storage on earth; one file per graph; transactions; **FTS5 in the same transaction as the data**; JSON functions; online backup API; any tool can open the file for debugging; admin statistics are plain SQL | Single writer (fine for our write rates); slower raw KV than LMDB/redb; C dependency (bundled, no install) | **PoC choice** |
| **redb** | Embedded KV, CoW B-tree, pure Rust | ACID; fastest random reads in its own benchmark; no C deps | We must build every secondary index ourselves; slow deletes in its benchmark; no FTS | Candidate for phase 3 custom engine |
| **fjall** 3.0 | Embedded LSM, pure Rust | Fast batch writes; good for large datasets | Young; same "build everything yourself" cost | Candidate if write-heavy |
| **LMDB** (heed) | Embedded KV, mmap B+tree | Extremely fast reads; proven (Meilisearch, HelixDB) | Map-size management; single writer; C dependency | Candidate for phase 3 |
| **RocksDB** | Embedded LSM (C++) | Industry standard at scale | Heavy C++ build, many tuning knobs; overkill | No |
| **Redis / Valkey** | In-memory server | Fast | Separate process; durability through AOF/RDB; everything must fit in RAM including evidence text; licence churn (Redis vs Valkey) | No — our in-process indexes already give RAM speed without a network hop |
| **PostgreSQL** (+ Apache AGE) | Client-server SQL | Multi-user, replication, managed hosting everywhere | Network round trip per operation; operational weight | Future backend for hosted multi-tenant deployments |

## 2. Search components

| Search need | PoC solution | Upgrade path (benchmark-driven) |
|---|---|---|
| Get by ID / key | In-RAM hash maps (key → internal index), dense `Vec` of records | — |
| Exact name / alias (case- and accent-insensitive) | In-RAM hash map: normalized name → node indexes | — |
| Prefix and fuzzy (typos) on names | SQLite FTS5 with the `trigram` tokenizer | `fst` crate: Levenshtein automata over a sorted name set, rebuilt in background, plus a small delta map for recent writes |
| Full-text (words, BM25 ranking) over names, summaries, string properties, evidence text | SQLite **FTS5** (`porter` stemming over the `unicode61` tokenizer, remove diacritics; stemming since storage format 2), transactional with the data | **tantivy** if FTS5 misses targets: richer scoring, faster on large corpora, but near-real-time and a second store to keep in sync |
| Neighbourhood, k-hop, paths | In-RAM adjacency lists (out / in per node, undirected links in both) | Compressed sparse row (CSR) base + delta for very large graphs |
| Filter by type / label | In-RAM roaring bitmaps: type → node set, link type → link set | — |
| Property filters | Scan of the type-filtered set (RAM, parallel with rayon) | Per-property sorted indexes, created on demand or automatically for hot properties |
| Aggregations (count, sum, avg, min, max, group-by, histogram) | Same scan, aggregated in one pass | Columnar property storage (Arrow) / DataFusion for heavy analytics |
| Evidence ↔ fact lookups | In-RAM maps: target → citations, evidence → citations | — |
| Semantic (meaning) search | Server-side embeddings (multilingual-e5-small, int8 ONNX via `fastembed`/`ort`) + HNSW index (`usearch`, f16 vectors), fused with BM25 by reciprocal rank fusion; crate `mygraph-semantic` (since 2026-11-02) | A larger or better model when benchmarks show what it gains |

Semantic search came after the PoC: the agent cannot produce embeddings itself, so the server
embeds text with a local model. That costs a model download (135 MB, once, never at run time), CPU
(tens of passages a second; writes only queue texts, background commits embed them) and a second
index, a cache like the tantivy one ([STORE-VEC-1]).

## 3. Evidence content storage

| Content | Where |
|---|---|
| Short text (≤ 64 KB by default, configurable per graph) | Inline in the `evidence` table, full-text indexed |
| Long text, PDFs, images, any binary | **Content-addressed blob store**: `data/blobs/sha256/ab/cd/<hash>`; metadata (hash, size, MIME) in SQLite. Identical content is stored once. |
| External sources | URI + metadata only (title, dates, version); optionally a captured snapshot in the blob store (later: automatic snapshotting and link-rot checks) |

Later: S3-compatible blob backend; text extraction from PDFs/images (OCR) into child "fragment"
evidence so it becomes searchable.

## 4. PoC physical layout

```
data/
  graphs/
    <graph-name>.db          SQLite file: all records, FTS5 tables, change log
    <graph-name>.db-wal
  blobs/
    sha256/ab/cd/<hash>      content-addressed, shared by all graphs
  server.toml                configuration
```

One SQLite file per graph gives isolation, trivial backup/restore/delete of a graph, and no
cross-graph locking.

### SQLite schema sketch (PoC)

> **Superseded** by the normative [spec/storage/schema.sql](../spec/storage/schema.sql)
> (2026-10-24). Main differences: a `batch` table; `source_date`, `valid_from`, `valid_to` stored as
> canonical date text (partial dates); evidence `version` renamed `source_version`, plus a record
> `version`; `content_hash` → `content_sha256`, `blob_hash` → `blob_sha256`; unique indexes for
> link and citation identity; FTS tables named `fts_node`, `fts_name`, `fts_link`, `fts_evidence`.
> The sketch below is kept as design history.

```sql
-- internal dense integer ids; external ids are prefix + base32(idx), e.g. "n_3kq"
CREATE TABLE node (
  idx         INTEGER PRIMARY KEY AUTOINCREMENT,   -- never reused
  key         TEXT UNIQUE,                         -- optional natural key
  type        TEXT NOT NULL,
  name        TEXT NOT NULL,
  aliases     TEXT,            -- JSON array
  labels      TEXT,            -- JSON array
  summary     TEXT,
  props       TEXT,            -- JSON object with typed values
  status      INTEGER NOT NULL DEFAULT 0,          -- 0 active, 1 retracted, 2 merged
  merged_into INTEGER,
  version     INTEGER NOT NULL DEFAULT 1,
  created_at  INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  created_by  INTEGER, updated_by INTEGER
);

CREATE TABLE link (
  idx        INTEGER PRIMARY KEY AUTOINCREMENT,
  key        TEXT UNIQUE,
  type       TEXT NOT NULL,
  src        INTEGER NOT NULL REFERENCES node(idx),
  dst        INTEGER NOT NULL REFERENCES node(idx),
  directed   INTEGER NOT NULL DEFAULT 1,           -- 0 = undirected ("both sides")
  props      TEXT,
  valid_from INTEGER, valid_to INTEGER,             -- real-world validity (optional)
  status     INTEGER NOT NULL DEFAULT 0,
  version    INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  created_by INTEGER, updated_by INTEGER
);

CREATE TABLE evidence (
  idx          INTEGER PRIMARY KEY AUTOINCREMENT,
  key          TEXT UNIQUE,
  kind         TEXT NOT NULL,     -- text | url | file | image | document | conversation | observation | dataset | other
  title        TEXT,
  content      TEXT,              -- inline text (<= limit)
  uri          TEXT,              -- external reference
  blob_hash    TEXT, blob_size INTEGER, mime TEXT,
  parent       INTEGER REFERENCES evidence(idx),   -- fragment-of
  locator      TEXT,              -- JSON: page, char range, selector, timestamp, bbox
  source_date  INTEGER,           -- when the source was published / authored
  captured_at  INTEGER NOT NULL,  -- when we obtained it
  version      TEXT,              -- source revision / etag / document version
  supersedes   INTEGER REFERENCES evidence(idx),
  author TEXT, publisher TEXT, language TEXT, license TEXT,
  reliability  REAL,              -- 0..1, optional assessment of the source
  content_hash TEXT,              -- sha256 of content or blob, for dedup
  meta         TEXT,              -- JSON
  status       INTEGER NOT NULL DEFAULT 0,
  created_at   INTEGER NOT NULL, created_by INTEGER
);

CREATE TABLE citation (
  idx          INTEGER PRIMARY KEY AUTOINCREMENT,
  evidence     INTEGER NOT NULL REFERENCES evidence(idx),
  target_kind  INTEGER NOT NULL,  -- 0 node, 1 link
  target       INTEGER NOT NULL,
  prop         TEXT,              -- optional: cites one property of the target
  role         INTEGER NOT NULL,  -- 0 supports, 1 contradicts, 2 mentions, 3 derived_from
  confidence   REAL,
  quote        TEXT,
  note         TEXT,
  status       INTEGER NOT NULL DEFAULT 0,
  created_at   INTEGER NOT NULL, created_by INTEGER
);

CREATE TABLE actor     (idx INTEGER PRIMARY KEY, name TEXT UNIQUE, kind TEXT, meta TEXT);
CREATE TABLE changelog (seq INTEGER PRIMARY KEY, ts INTEGER, actor INTEGER, batch TEXT,
                        op TEXT, obj_kind INTEGER, obj INTEGER, diff TEXT);
CREATE TABLE type_info (kind INTEGER, name TEXT, description TEXT, symmetric INTEGER,
                        PRIMARY KEY (kind, name));          -- optional vocabulary notes
CREATE TABLE meta      (k TEXT PRIMARY KEY, v TEXT);        -- schema version, graph policies

-- Full-text (external-content FTS5 tables kept in sync by the engine in the same transaction)
CREATE VIRTUAL TABLE fts_node     USING fts5(name, aliases, summary, props_text,
                                             tokenize = 'porter unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE fts_evidence USING fts5(title, content,
                                             tokenize = 'porter unicode61 remove_diacritics 2');
CREATE VIRTUAL TABLE fts_name_tri USING fts5(name, tokenize = 'trigram');
```

SQLite settings: `journal_mode=WAL`, `synchronous=FULL` by default (configurable to `NORMAL`),
`foreign_keys=ON`, large `cache_size` and `mmap_size`.

## 5. Recommendation

- **PoC:** SQLite (WAL) as the durable store and FTS engine, behind a `Store` trait and a
  `TextIndex` trait; topology, names, types and properties served from RAM.
- **Benchmark gate at the end of the PoC** decides whether to bring in tantivy (text), fst
  (fuzzy names) or a KV engine (redb/LMDB) — each is a contained swap behind its trait.
