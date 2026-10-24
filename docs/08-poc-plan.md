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
| Admin API subset marked PoC in [07-admin-api.md](07-admin-api.md) | Admin UI |
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

### M3 — APIs (M)
- **Spec:** `api/conventions.md`, `errors.md`, complete `schemas/`, `openapi.yaml`,
  `mcp-tools.json`, `server-instructions.md`, `admin/admin-api.md` with schemas.
- MCP server via `rmcp`: 11 tools whose definitions are embedded from `spec/api/mcp-tools.json`,
  server instructions, stdio and Streamable HTTP.
- REST endpoints + generated OpenAPI; API-key auth with roles; request size, result and time limits.
- Admin API (PoC subset); Prometheus metrics; slow-op log.
- **Done when:** Claude Code connects over stdio and over HTTP, lists the tools, and completes a
  scripted smoke scenario; OpenAPI validates.

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
