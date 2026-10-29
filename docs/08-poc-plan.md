# 08 — Proof-of-concept plan

Status: **In progress — M0 spec work started** · 2026-10-24 · Rust approved (ADR-001);
spec-first workflow (ADR-012); all open questions answered

## 1. What the PoC must prove

1. **The model works for real agent tasks**: evidence, fragments, citations, undirected links and
   properties capture what an agent learns from real sources, without workarounds.
2. **The agent API is usable**: Claude, connected over MCP, builds and queries a graph with few
   calls, few duplicates, and cites sources in its answers.
3. **It is fast**: the performance budget in [05-architecture.md §11](05-architecture.md) holds at
   1 M nodes / 5 M links / 1 M evidence on a laptop.
4. **It can grow**: storage, text search and transports are behind traits, so phase-2 upgrades are
   contained changes.
5. **It can be re-written** (R12): `spec/` and `conformance/` are complete enough to build a
   compatible implementation in another language without reading the Rust code.

## 2. Scope

| In the PoC | Not in the PoC (see [09-roadmap.md](09-roadmap.md)) |
|---|---|
| Graphs, nodes, links (directed + undirected), evidence (inline text, URL, local blobs, fragments, versions), citations (node / link / property level) | Semantic (vector) search, embeddings |
| All 11 agent operations over MCP (stdio + Streamable HTTP) and REST | Strict schemas, type inheritance, inverse link names |
| Upserts, resolve, merge, retract/restore, change log, history | Bi-temporal "as of record time" queries |
| Search: exact, alias, prefix, fuzzy (FTS5 trigram), full text (FTS5) | tantivy / fst (unless the benchmark gate demands them) |
| `find` with filters, group-by, aggregates, computed values | Graph algorithms (PageRank, communities…) |
| Admin API subset marked PoC in [07-admin-api.md](07-admin-api.md); internal stats page, read-only graph viewer (M5) | Admin UI with editing; feedback to the project (central, through GitHub) |
| API keys from config, limits, metrics, slow-op log | Per-graph permissions, OAuth |
| JSONL export/import | Other import/export formats, URL snapshots, OCR |
| Bench harness + synthetic data generator, agent trial | Replication, Postgres backend, S3 |
| Normative `spec/` and `conformance/` suite for everything above (R12) | Spec for phase-2 features (written when each is built) |

## 3. Milestones

Sizes are relative (S < M < L), not calendar promises.

Every milestone follows the spec-first workflow of
[10-portability-and-spec.md §5](10-portability-and-spec.md): the **Spec** items of a milestone
(rule text, schemas, test vectors, conformance cases) are written and reviewed before its Rust code.

### M0 — Foundations (S)
- **Spec:** `spec/README.md` (keywords, rule IDs, versioning), `VERSION`, `model/identifiers.md`,
  `model/normalization.md`, `model/values.md` with test vectors; `storage/schema.sql` +
  `0001_init.sql`; `conformance/README.md`.
- Git repository, Rust workspace with the crates from [05-architecture.md §2](05-architecture.md).
- CI: `cargo fmt`, `clippy -D warnings`, tests on Windows and Linux.
- Config file (`server.toml`), logging/tracing, error type; ID codec, name normalization and value
  parsing implemented against the shared vectors.
- Conformance runner skeleton (runs vectors now, HTTP cases from M1).
- **Done when:** `mygraph serve` starts, answers `/health`, the vectors pass, and CI is green.
- *Progress 2026-10-24:* spec items written — `spec/README.md`, `VERSION`, identifiers,
  normalization, values, `schema.sql` + `0001_init.sql`, `conformance/README.md` and 339 vector
  cases. Vectors and schema were cross-checked with a throwaway independent script (not kept in the
  repo). Remaining in M0: Rust workspace, CI, config, conformance runner.
- *Progress 2026-10-24 (later):* Rust workspace (`mygraph-core`, `mygraph-server`), ID codec, name
  keys, vocabulary and values implemented; all 339 vectors pass as a Rust test; `mygraph serve`
  answers `/health`; config file with local-only defaults; CI workflow (fmt, clippy, tests on
  Linux and Windows). Remaining in M0: standalone conformance runner crate (the vectors already
  run as tests; the HTTP runner is needed from M1).

### M1 — Core model and storage (L)
- **Spec:** `model/objects.md` (fields, invariants), `algorithms/upsert-matching.md`,
  `storage/storage-format.md`, operations `write`, `retract`, `merge`; their schemas; conformance
  cases for every rule.
- DTOs and validation for nodes, links, evidence, citations, updates.
- SQLite schema, migrations, `Store` trait and implementation; blob store.
- Write path: reference resolution (`id`, `key`, type+name, `@ref`), upsert rules, property
  conflicts, evidence policy, vocabulary normalization + similar-name warnings, change log, one
  transaction per batch.
- RAM indexes; rebuild on startup; retract/restore (with cascades); merge.
- Synthetic data generator started here (not at M4), so RAM usage and load time are measured early.
- **Tests:** unit tests; property-based tests (`proptest`) running random write/retract/merge
  sequences against a simple reference model and checking the invariants in
  [04-data-model.md §11](04-data-model.md); restart test (state identical after reload); kill-during-write test.
- **Done when:** invariants hold under property tests; 1 M-node synthetic graph loads and its RAM
  footprint is measured.
- *Progress 2026-10-24:* spec written — `model/objects.md`, `algorithms/upsert-matching.md`,
  `storage/storage-format.md`, `api/operations/{write,retract,merge}.md`. Code: crate
  `mygraph-store-sqlite` creates and opens graph files (migrations from `spec/`, header checks,
  WAL, policies), 4 tests including "created schema equals spec schema". Next: record writes and
  change log, RAM indexes, the write path, then retract/merge and property tests.
- *Progress 2026-10-25:* engine implemented in `mygraph-core` (`engine.rs`, `state.rs`, `write.rs`,
  `lifecycle.rs`, `store.rs`) and the `Store` trait on SQLite (`records.rs`): write with batch
  references, matching, field combination, evidence policy, dry run; retract/restore with
  cascades; merge; change log with cascade causes. Tests: 11 engine integration tests, a
  randomized test (24 cases by default, also passed at 200) checking invariants, SQLite integrity
  and memory-equals-disk after every step, and a kill-during-write test. Spec fixes found while
  implementing: citations gained a `version` column; [OP-RETRACT-7] restore order.
  **Known gaps (still M1):** blob upload (`evidence.blob_unsupported` for now); vocabulary
  similarity for `NEW_VOCABULARY` (`similar` is empty until `algorithms/vocabulary-similarity.md`
  exists); HTTP conformance cases; synthetic data generator and the 1 M-node load measurement.
  **Deviations from docs/05 (PoC simplifications):** the graph's write lock is held for the whole
  batch, not only while applying to memory; in-memory records are in hash maps rather than dense
  vectors.
- *Progress 2026-10-25 (later):* M1 gaps closed. **Blobs:** `blob_base64` upload and `blob`
  references ([OP-WRITE-7] .. [OP-WRITE-9]), local content-addressed blob store (`BlobDir`,
  [STORE-BLOB-5] added). **Vocabulary hints:** `algorithms/vocabulary-similarity.md` (integer
  scores, so every implementation lists the same items) with 12 vector cases; `NEW_VOCABULARY`
  now fills `similar`. **Generator and measurements:** crate `mygraph-bench` (deterministic
  SplitMix64 generator writing through the engine's own `write`; `load --breakdown` reports exact
  heap bytes per object). The first measurement showed about 10× the RAM estimate, so the in-memory
  layout was compacted (dense tables, inline one-element sets, identity maps replaced by lookups,
  interned vocabulary, vector property maps, streamed loading): 2.3–2.8× less memory per object.
  **Done-when check:** the 1 M / 5 M / 1 M / 6 M graph loads in 25 s (warm cache; 37 s cold) into
  3.8 GB of RAM (first layout: 9.5 GB plus a 5.5 GB transient snapshot). The RAM estimate of
  ~1 GB is missed by about 4×; numbers and next steps in [05-architecture.md §5](05-architecture.md).
  Write throughput fell from ~3 400 to ~1 400 links/s as the file grew to 8 GB (suspected: SQLite's
  default 2 MB page cache); to be measured in M4. In-memory records now live
  in dense tables, which removes the second deviation above. **HTTP conformance cases** move to M3,
  where the REST transport exists; the vectors run as Rust tests until then.

### M2 — Reads, search and analytics (L)
- **Spec:** operations `get`, `search`, `resolve`, `neighbors`, `paths`, `find`, `history`;
  `algorithms/search-ranking.md`, `resolve-scoring.md`, `traversal.md`, `aggregation.md`,
  `pagination.md`; `api/compact-format.md`; schemas, vectors (ranking, aggregates) and cases.
- `get` (with neighbourhood summary, support summaries, evidence content ranges).
- `search` (exact → alias → fuzzy → text, merged ranking, snippets) and `resolve`.
- `neighbors` (steps or uniform depth, per-hop caps), `paths` (bidirectional BFS; k-shortest via
  Yen's algorithm), `as_of` valid-time filter.
- `find`: filter operators, structural filters, computed values, group-by, aggregates, sorting,
  cursors.
- `history` by object and `since_seq`.
- Compact text formatter.
- **Done when:** every operation has golden-response tests; micro-benchmarks exist for each.
- *Progress 2026-10-25:* **`search`** and **`get`** specified (`api/operations/search.md`,
  `get.md`, `algorithms/search-ranking.md` + 11 vector cases) and implemented. Name matching runs in
  RAM: an ordered name-key index (exact, prefix) and a trigram index (fuzzy, pg_trgm-style
  similarity with prefix filtering); full text uses SQLite FTS5. `mygraph-bench search` times them
  on the 1 M-node graph (300 queries each, ms):

  | Query | p50 | p99 | Budget (p99) |
  |---|---:|---:|---:|
  | exact name | 0.007 | 0.014 | 0.5 ✓ |
  | prefix (4 characters) | 0.63 | 2.6 | — |
  | fuzzy (one typo) | 3.4 | 11.2 | 15 ✓ |
  | auto (full name / with a typo) | 3.6 / 2.9 | 12.0 / 10.7 | 15 ✓ |
  | full text, one word (nodes) | 1.2 | 71 | 10 ✗ |
  | full text, one word (evidence) | 2.7 | 671 | 10 ✗ |
  | `get` node with default includes | 0.06 | 0.09 | 1 ✓ |
  | `get` of the 20 largest hubs (≤ 9 833 links) | 0.6 | 3.7 | 1 ✗ |

  Full text misses on frequent words: FTS5 computes BM25 for every matching row before taking the
  top 20. Hub `get` sorts all of a hub's links per call; caching per-node type counts would fix it
  if needed.
- *Progress 2026-10-25 (later):* the tantivy decision of the M4 gate was taken early: full-text
  queries now go to **tantivy** (crate `mygraph-text-tantivy`) behind the new `TextIndex` seam.
  The index is a rebuildable cache ([STORE-TEXT-1]) in `<data_dir>/text/<graph>/`; each commit
  records the change-log sequence it covers, so attaching catches up from the change log or
  rebuilds (53 s for the 1 M / 5 M / 1 M graph, 196 MB). Writes update it after the SQLite commit;
  if that fails, search falls back to FTS5 until the next attach. FTS5 tables are still written
  (format v1 unchanged). Design choices that mattered for speed: one text field per object kind
  (a one-word query is a bare term query, so block-max skipping applies) and prefix expansion of
  the last word only when whole-word hits do not fill the limit. Results (ms):

  | Full text, one word | FTS5 p50 / p99 | tantivy p50 / p99 | Budget (p99) |
  |---|---:|---:|---:|
  | nodes | 1.2 / 71 | 5.6 / 9.8 | 10 ✓ |
  | evidence (with snippets) | 2.7 / 671 | 5.5 / 7.5 | 10 ✓ |

  All `search` budget lines are now met. Not yet measured: the write-latency cost of the tantivy
  commit per batch.
- *Progress 2026-10-25 (resolve):* **`resolve`** specified (`api/operations/resolve.md`,
  `algorithms/resolve-scoring.md`, 17 vector cases for hint agreement and verdicts) and
  implemented: candidates from exact name/alias and trigram similarity ≥ 0.4, name score, type
  factor, hint agreement (partial dates compare at the coarser precision), verdict thresholds. The
  first draft scored every weak trigram neighbour (similarity 0.3) as plausible, so on a large
  graph almost nothing would come back `none`; the thresholds were raised before release (fuzzy
  name score `min(0.9, s + 0.2)`, plausible ≥ 0.6). On the 1 M graph a call with 20 candidates
  takes ~130 ms p50 (exact names) and ~90 ms (names with a typo), measured while two file-sync
  clients kept the CPU ~77 % busy, which roughly doubled every timing that day. Keeping `target/`
  and benchmark data outside synced folders is needed for clean numbers.
- *Progress 2026-10-25 (rest of M2):* **`neighbors`**, **`paths`**, **`history`** and **`find`**
  specified (`algorithms/traversal.md`, `api/operations/{neighbors,paths,history,find}.md`; `find`
  also defines interval semantics for partial dates and where missing values sort, as
  `model/values.md` promised) and implemented with engine tests. `paths` uses bidirectional BFS
  for the distance, then walks the shortest-path layers in a defined path order; `k_shortest` goes
  on to longer simple paths within a work budget. `find` evaluates fields as borrowed views of the
  in-memory records, and starts from new per-type indexes of active nodes and links when a
  condition fixes the type. Timings on the 1 M graph (laptop **on battery**, so CPU-throttled; all
  of this day's numbers are worst-case), ms:

  | Query | p50 | p99 | Budget (p99) |
  |---|---:|---:|---:|
  | `neighbors` 1 hop / 2 hops (≤ 200 nodes) | 0.03 / 0.42 | 0.39 / 1.07 | 5 ✓ |
  | `paths` shortest / k = 3, max length 4 | 0.12 / 0.14 | 2.4 / 2.6 | 50 ✓ |
  | `find` count of one node type (400 k nodes) | 1.3 | 1.8 | 50 ✓ |
  | `find` group 5 M links / 1 M nodes by type | 0.016 / 0.009 | 0.02 / 0.44 | 50 ✓ |
  | `find` group 400 k Persons by birth decade + avg | 111 | 128 | 50 ✗ |
  | `find` filter + sort Organizations by a property | 97 | 112 | — |

  The misses read a property of every candidate (~250 ns per object, memory-bound with records of
  ~230 bytes). Options for the M4 gate: parallel scans (rayon, as docs/05 §4 planned) or columns
  for frequently filtered properties. **M2 is complete** except the compact text format, which moves
  to M3 with the MCP transport that uses it.
  Next: `resolve`, `neighbors`, `paths`, `find`, `history`.

### M3 — APIs (M)
- **Spec:** `api/conventions.md`, `errors.md`, complete `schemas/`, `openapi.yaml`,
  `mcp-tools.json`, `server-instructions.md`, `admin/admin-api.md` with schemas.
- MCP server via `rmcp`: 11 tools whose definitions are embedded from `spec/api/mcp-tools.json`,
  server instructions, stdio and Streamable HTTP.
- REST endpoints + generated OpenAPI; API-key auth with roles; request size, result and time limits.
- Admin API (PoC subset); Prometheus metrics; slow-op log.
- **Done when:** Claude Code connects over stdio and over HTTP, lists the tools, and completes a
  scripted smoke scenario; OpenAPI validates.
- *Progress 2026-10-25 (REST first):* REST comes first and MCP will be built on top of it once
  REST does what agents need. **Spec:** `api/conventions.md` ([API-*]: transport, Bearer keys with
  roles `agent-read` / `agent-write` / `admin` and optional graph lists, keyless local mode bound
  to loopback, warm-up, read-your-writes, routes and shorthands), `api/errors.md` ([ERR-*]: error
  object, HTTP status by reason code, hints for agents), `api/admin.md` ([ADMIN-*]), the new
  operation `api/operations/overview.md` ([OP-OVER-*]) and `api/openapi.yaml` (validated with
  Redocly; served at `/openapi.yaml`). **Server:** every operation at
  `POST /v1/graphs/{graph}/{operation}`; shorthands `GET /v1/graphs/{graph}` (overview),
  `…/search?q=`, `…/changes?since_seq=`, `GET`/`DELETE …/{collection}/{id}`, `…/history`,
  `…/restore`, evidence `…/content` (text) and `…/blob` (bytes, media type, ETag); admin create /
  list / drop (to trash) / snapshot (`VACUUM INTO`) / check, stats, keys, config; `/health`,
  `/ready`, Prometheus `/metrics`, slow-request log. Graphs load in the background at start (or
  on first use); requests wait up to 2 s, then get `503 graph.warming_up` with `Retry-After`.
  Reads run in parallel under a per-graph `RwLock`; writes are exclusive (the SQLite connection
  sits behind a mutex, so only reads that reach SQLite take turns). 8 end-to-end tests drive the
  router. Not yet: per-request time budgets, full JSON Schemas per operation, policy changes and
  vocabulary notes over the admin API, JSONL export/import.

  **Smoke run over HTTP** on the 1 M / 5 M / 1 M graph (release build, `curl` from the same
  laptop, median of 20 calls, HTTP and JSON included; a bare `/health` takes ~1.4 ms this way):

  | Call | Median ms |
  |---|---:|
  | `GET /v1/graphs/g1m` (overview) / `?type=Person` | 1.6 / 54 |
  | search: exact name (GET) / fuzzy / evidence full text | 6.5 / 6.9 / 2.3 |
  | `GET …/nodes/{id}` default / brief | 4.9 / 1.4 |
  | resolve (1 candidate) | 6.7 |
  | neighbors depth 2 / paths (2 hops) / paths k-shortest 3 | 4.1 / 6.2 / 21 |
  | find (group by type) / history of a node / changes | 1.4 / 1.6 / 3.5 |
  | write: unchanged (idempotent) / new (9 objects, fsync) | 1.5 / 4.1 |

  Three problems found by this run and fixed: (1) `overview` scanned all evidence and citations
  (105 ms) — active counts are now kept in memory (1.6 ms); type statistics sampled the first
  10 000 objects, which are the hubs, and now sample at equal steps. (2) Every write committed the
  tantivy index (50–150 ms per write at 1 M). Writes now only queue documents; searches match the
  queue from memory, and the server commits the index in the background once a second without
  holding the graph lock. (3) SQLite's automatic WAL checkpoint ran inside whichever commit crossed
  the threshold, stalling it on the 8.7 GB file; the server turns it off and runs passive
  checkpoints from a second connection. Sustained writes (3 nodes/links + evidence each, 300 in a
  row, warm): p50 4.0 ms, p90 7.2 ms, p99 16 ms (before: p99 95 ms); the first writes after a
  start still see cold-cache outliers of up to ~0.9 s.
- *Progress 2026-10-25 (MCP):* **Spec:** `api/mcp.md` ([MCP-*]), `api/mcp-tools.json` (11 tools, the
  operation requests as arguments plus `graph` and `format`), `api/server-instructions.md`,
  `api/compact-format.md` ([CMP-*]). **Server:** MCP is a thin layer over the REST operations: a
  hand-written JSON-RPC handler (initialize, ping, tools/list, tools/call; stateless) instead of
  `rmcp`, served as Streamable HTTP at `POST /mcp` (same keys) and over stdio by `mygraph mcp`,
  which forwards tool calls to a running server's REST API (`--url`, so one process owns the
  data) or opens a data folder itself (`--data-dir`). Tool results are compact text by default;
  REST returns it with `Accept: text/plain`. Tests: protocol, tools, compact output and errors over
  HTTP; stdio with a data folder; forwarding checked by hand against a running server.
  **Claude Code smoke run:** registered with `claude mcp add mygraph -- mygraph mcp --url … --graph
  research` (connected), then a headless session given one short source and three questions used
  only the tools: overview → resolve (10 names) → one write (11 nodes, 13 links, 24 citations, 2
  evidence) → paths, find, get → retract + get; 12 turns, 60 s, every call succeeded and every
  answer cited evidence IDs. From its feedback the compact format now says `no path found` /
  `no hits` instead of a bare `ok`, repeats candidate names in resolve lines, and folds
  new-vocabulary warnings without similar names into one line; the server instructions now show
  the date format. Still open from the feedback: link lines show endpoint IDs without names;
  support counts on a retracted link can look like it is still supported.
- *Progress 2026-10-25 (data safety):* **Spec:** `api/export-format.md` ([EXP-*]: JSONL with a
  header, actors and every object in record form, any status; import keeps IDs, statuses,
  versions and record times) and [ADMIN-GRAPH-6..8] (restore a snapshot, export, import). **Server:**
  `POST /admin/graphs/{g}/restore | export | import`; import creates the graph from the header's
  policies when needed and writes 10 000-object `import` batches, holding back objects whose
  references come later in the file. `get` now names a link's ends (`ends`), which the compact
  format shows (`l_2 WROTE_ABOUT n_1 "Ada Lovelace" -> n_3 "Analytical Engine"`). **1 M graph:**
  export 74 s (4.9 GB); import into a new graph 33 min (~6 600 objects/s; 8.8 GB, like the
  original); exporting the copy gives byte-identical object lines (same SHA-256). Two problems found
  on the way, fixed: background maintenance waited for the graph lock that the import held, so no
  checkpoint ran and the WAL reached 17 GB; and even with background checkpoints the WAL outgrew
  the database during a bulk load. Maintenance now skips a busy graph's text commit (checkpoints
  need no graph lock), and import runs a truncating checkpoint every 20 batches (WAL stays < 2.5 GB).
- *Progress 2026-10-25 (local deployment):* `deploy/windows/deploy.ps1` installs or upgrades a
  local instance (binary in `%LOCALAPPDATA%\MyGraph\bin`, own `server.toml`, data in
  `%USERPROFILE%\mygraph-data`, log file) and registers a hidden logon task; upgrades rename the
  running binary aside so MCP bridges keep working. A data folder is now locked by the process
  that uses it ([STORE-DIR-4]); a second server on the same folder refuses to start. Claude Code's
  `mygraph` MCP server points at the deployed binary. New config option `log_file`.
- *Progress 2026-10-25 (agent skill):* using MyGraph from the Claude desktop chat showed two gaps:
  the MCP tools could not list or create graphs, and the `graph.not_found` hint pointed at a REST
  route, so the agent went looking for REST. Added the MCP tool `graphs` (list with the default
  graph, create; [MCP-TOOL-4]) and MCP-specific hints. Wrote the **`mygraph` skill**
  (`skills/mygraph/`: SKILL.md with the session start, the one-write-per-source pattern, a
  question → tool table, corrections, reading results; references for writes, queries, REST and
  client setup) in the Agent Skills format shared by Claude, Codex, Cursor and Copilot, with
  `deploy/install-skill.ps1`. Test: a headless Claude session with only the skill and the tools,
  given a short Lviv guide text, created its own graph, recorded the source in one write (page +
  one quote per fact, 7 nodes, 6 links, 13 citations), answered both questions with evidence IDs
  and flagged where the source was vague — 10 turns, 42 s, no errors.

### M4 — Validation and report (M)
- **Benchmarks** on the 1 M / 5 M / 1 M synthetic dataset (power-law degrees, realistic names,
  evidence with text): p50/p95/p99 for every budget line, RAM, cold start, write throughput.
  Optional comparison runs: the same queries on SQLite-only (recursive CTE) and on Neo4j Community.
- **Benchmark gate:** decide on tantivy, fst, or a KV engine only where a target is missed.
- **Agent trial:** Claude Code with the MCP server ingests ~30 real sources on one topic, then
  answers ~20 questions using only the graph. Measured: tool calls per source, tokens, errors,
  duplicate rate, vocabulary drift, share of answers citing evidence IDs. Fix API rough edges found.
- Turn the trial into a repeatable **agent eval suite** (tasks + expected facts) for regression
  testing of future API changes.
- **Spec:** `conformance/bench/dataset.md` (generator version + seed + SHA-256) and
  `scenarios.yaml`; agent evals stored under `conformance/agent-evals/`.
- **Portability check (cold read):** a fresh agent session with only `spec/` and `conformance/`
  implements the pure functions (ID codec, normalization, value parsing, one ranking function) in
  Go or C# and runs the vectors. Every gap it hits is a spec bug to fix.
- **PoC report:** results vs targets, lessons, updated roadmap.

### M5 — Observability and viewing (M)
Added 2026-10-25. Each item follows the spec-first workflow: rule text and OpenAPI entries first.

- [x] **Graph statistics in the admin API and the agent API.** Today only `counts` exist (in
  `GET /admin/graphs`, `GET /admin/graphs/{g}` and `overview`); `GET /admin/graphs/{g}/stats` from
  [07-admin-api.md §2](07-admin-api.md) is not built. First cut: `counts`, `degree` (min / max /
  mean / percentiles, top hubs, orphans), `growth` (per day, from the change log), `evidence` (by
  kind, by URL domain, most cited, citing nothing), `actors`, `storage` (DB / WAL / text index /
  blob sizes). `quality` and `usage` follow. The agent API gets a read-only `stats` operation (and
  MCP tool) with the graph-content sections only (no storage, keys or actors' activity), in the
  compact format. Must stay fast at 1 M nodes: compute from the RAM indexes, cache per `last_seq`.
  - *Progress 2026-10-26:* done — spec `api/operations/stats.md` ([OP-STATS-1..6]) and
    [ADMIN-GRAPH-9]; agent operation `stats` (REST `POST`/`GET /v1/graphs/{g}/stats`, MCP tool,
    compact layout) with `counts`, `degree`, `growth`, `evidence`; admin
    `GET /admin/graphs/{g}/stats` adds `actors` and `storage`. `growth` is computed from record
    times (created per day; retracted per day = retracted objects by `updated_at`), not from the
    change log, so it needs no scan of the log. On the 1 M graph (13 M records) a first call with
    all four sections takes ~0.33 s (counts 10 ms, degree 90 ms, evidence 105 ms, growth 130 ms —
    single sequential passes over dense arrays; a first version with per-node adjacency walks and
    hash maps took 1.1 s), admin with all six ~0.45 s; repeated calls are served from a cache keyed
    by (`last_seq`, UTC day) in ~1 ms until the next write. `quality` and `usage` are not built.
- [x] **Prometheus metrics, complete set.** `GET /metrics` today has request counts, latency
  histograms and object counts per graph. Add: graph state (loading / ready / failed) and warm-up
  time, writes and batches per graph, text-index pending docs and commit time, WAL and DB file
  sizes, checkpoint runs, process RSS, MCP tool calls separate from REST, errors by code, build
  info. Document every metric (name, type, labels) in `spec/api/admin.md`; add an example
  Grafana dashboard under `deploy/`.
  - *Progress 2026-10-27:* done except the Grafana dashboard. [ADMIN-MET-1] lists 18 metrics:
    build info, uptime, resident memory; requests and latency per operation (MCP tool calls as
    `mcp.<tool>`, separate from REST), errors by code, slow requests; per graph: state, last load
    time, DB and WAL bytes, active objects, `last_seq` (writes show as its growth), full-text
    pending updates and stale flag, background full-text commits and WAL checkpoints (count and
    time). `/metrics` no longer waits for a graph busy with a long write ([ADMIN-MET-1]: its
    *loaded* metrics are left out for that scrape). Also built: `GET /admin/slow-ops`, the last 50
    requests over `slow_ms` ([ADMIN-SRV-2]), and `/admin/stats` now reports versions, memory and
    `slow_ms`.
- [x] **Internal stats page.** `GET /admin/ui` (admin role): a server-rendered HTML page — no
  build step, embedded in the binary — showing server info, graphs with status and counts, request
  rates and latency percentiles, slow operations and recent batches. Reads the same
  data as the admin API; auto-refreshes.
  - *Progress 2026-10-27:* done as [ADMIN-UI-1], with one change: the page is static (no data in
    it, so it needs no key) and its script reads the admin API from the browser, sending the key
    the user types (kept for the browser session). Shown: versions, uptime, memory; graphs with
    status, counts, `last_seq`, DB / WAL size, text index state (click one for details); requests
    per operation (calls, errors, mean, max — no percentiles yet); slow requests; for the chosen
    graph its `stats` (growth chart per day, connectivity and hubs, evidence, domains, most cited,
    types, actors, storage) and recent batches. Every value is inserted as text, never as HTML
    (names are written by agents); a strict Content-Security-Policy allows nothing from other
    origins. Refreshes every 10 s while the tab is visible. Checked in the browser against the
    10 k / 100 k / 1 M benchmark graphs.
- **Feedback — postponed (2026-10-26).** Reports about MyGraph itself go to one central place,
  the project's GitHub repository, instead of a store in each server instance; see
  [09-roadmap.md](09-roadmap.md) (Phase 2, Feedback).
- [x] **Graph viewer (visual, read-only).** `GET /admin/ui/graph/{g}` (or a page linked from the
  stats page): search a node, show its neighbourhood (Cytoscape.js or Sigma.js, loaded from the
  binary, no CDN), expand on click, filter by node and link type, side panel with properties,
  support and sources (evidence text and URL). Built only on existing operations (`search`,
  `neighbors`, `get`); caps the drawn subgraph so a 1 M-node graph stays responsive. Editing stays
  in Admin UI v1 ([09-roadmap.md](09-roadmap.md)).
  - *Progress 2026-10-28:* done as [ADMIN-UI-2], `GET /admin/ui/graph?graph=…&node=…`, linked from
    the stats page ("browse"). Cytoscape.js 3.30.4 (MIT) is vendored in
    `crates/mygraph-server/ui/vendor/` with its licence and npm integrity hash, and served from
    the binary; no CDN. A graph with ≤ 2 000 nodes and ≤ 6 000 links is drawn whole (paged
    `find`); a larger one starts from its top hubs and grows by double-clicking nodes
    (`neighbors`), up to 3 000 on screen. Nodes are coloured by type (colours fixed by how common
    the type is in the graph) and sized by degree; selecting a node or link fades everything but
    its neighbourhood and shows details: properties, links (click to move), support, and the
    citing evidence with a preview of its text, the source page of a fragment, and its URL (links
    only for http/https); search by name, alias or text; filters by node and link type; the URL
    keeps the graph and node. Needs only a key that may read the graph. Checked in the browser on
    copies of the `boats` (396 nodes, whole: layout ~1.5 s) and `fishing` (95 nodes) graphs and
    the 10 k benchmark graph (hub start, expand).

## 4. Exit criteria

| Area | Criterion |
|---|---|
| Correctness | Property tests pass; restart and kill tests show no lost or inconsistent data |
| Speed | All budget lines met, or each miss has a measured cause and a chosen fix |
| Agent usability | ≤ 2 write calls per source on average; duplicate rate < 5 %; ≥ 90 % of answers cite evidence; no tool-misuse loops |
| Operability | Snapshot + restore works; `check` passes on all test graphs; metrics visible |
| Docs | API reference rendered from `spec/`; "connect Claude Code" guide; ADRs updated |
| Portability | Every testable spec rule has a conformance case or vector (coverage report); all responses validate against `spec/api/schemas`; the cold-read check passes |

## 5. Test strategy

| Level | Tooling | What |
|---|---|---|
| Unit | `cargo test` | Validation, upsert rules, ID codec, aggregations |
| Property-based | `proptest` | Random operation sequences vs reference model; invariants |
| Integration | REST + MCP clients in tests | Golden responses, error codes, limits |
| Durability | Process kill harness | Kill mid-batch, restart, verify |
| Performance | `criterion` (micro), `mygraph-bench` (macro) | Budget table, regressions in CI (smaller dataset) |
| Test vectors | `conformance/vectors` (JSON) | Pure functions — the same files test any implementation |
| Conformance | `mygraph-conformance` over HTTP | Spec rules, by rule ID; schema validation of every response |
| Agent evals | Scripted Claude sessions | Usability metrics above |

## 6. Main dependencies (Rust)

`tokio`, `axum`, `rmcp`, `rusqlite` (bundled, FTS5), `serde`/`serde_json`, `schemars` (JSON Schema),
`utoipa` (OpenAPI), `roaring`, `parking_lot`, `rayon`, `smallvec`, `sha2`, `unicode-normalization`,
`tracing`, `metrics` + Prometheus exporter, `clap`, `proptest`, `criterion`, `jsonschema` (test-time
validation against `spec/`), a YAML parser for conformance cases. Go and C# equivalents for each
are listed in [10-portability-and-spec.md §7](10-portability-and-spec.md).

## 7. Risks

| Risk | Mitigation |
|---|---|
| RAM use above estimate | Measure in M1 with the generator; fallback: keep non-indexed properties on disk |
| FTS5 too slow or ranking too weak | `TextIndex` trait; tantivy swap decided at the M4 gate |
| Agents create duplicates or drift vocabulary | `resolve`, warnings, server instructions, agent evals |
| Name-based upsert merges two different entities with the same name | Ambiguity errors, keys, `create_new`, hint properties in `resolve`; measured in the trial |
| MCP protocol churn (2026-07-28 revision is a large change) | Official SDK negotiates versions; transport layer kept thin |
| Single writer per graph limits write throughput | Agent write rates are low; group commit in phase 2 |
| Scope creep | Fixed PoC scope above; everything else goes to the roadmap |

## 8. Open questions

All answered on 2026-10-24 — see the [README](../README.md#decisions).
