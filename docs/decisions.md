# Decision log (ADRs)

Status values: **Proposed** (awaiting approval) · **Accepted** · **Superseded**.
Each entry: context → decision → alternatives → consequences.

---

## ADR-001 — Implementation language: Rust
**Status:** Accepted 2026-10-24, on the condition that everything stays documented for a rewrite in
another language (R12 → ADR-012) · details in [02-language-comparison.md](02-language-comparison.md)

- **Context:** R7 (compiled, fast), R10 (search speed first). The engine keeps a large graph in RAM
  and runs text/fuzzy/graph searches.
- **Decision:** Rust for the engine, both APIs and the CLI — one binary.
- **Alternatives:** C# (.NET 10) — productive, best ONNX, but weakest embedded search/KV libraries;
  Go — fastest to write, best MCP SDK, but GC pressure with large in-RAM graphs and a weaker search stack.
- **Consequences:** best native search stack (tantivy, fst, roaring, redb/LMDB) and memory density;
  slower development, partly offset by AI-assisted coding under a strict compiler.

## ADR-002 — SQLite as the durable store; all read indexes in RAM
**Status:** Proposed · details in [03-storage-and-search.md](03-storage-and-search.md)

- **Context:** R6 allows reusing storage; R10 demands fast reads; PoC should minimize moving parts.
- **Decision:** One SQLite file (WAL) per graph is the source of truth (records, change log, FTS).
  Topology, names, types, properties and citation maps live in RAM, rebuilt at startup, updated
  after each commit. Access through `Store`, `TextIndex`, `BlobStore` traits.
- **Alternatives:** redb / fjall / LMDB (faster raw KV, but we build every secondary index and FTS
  ourselves); Redis (separate process, RAM-bound, no gain over in-process indexes); PostgreSQL
  (network hop, operational weight — future backend).
- **Consequences:** reads never touch disk for topology; durability and backup are SQLite's;
  RAM ≈ 1 GB at PoC scale (to be measured); startup cost grows with graph size (snapshot loading in phase 2).

## ADR-003 — Text search: SQLite FTS5 in the PoC, tantivy behind a trait
**Status:** Proposed

- **Context:** search speed matters (R10), but a second store adds sync complexity.
- **Decision:** FTS5 (`unicode61` with diacritics removal, plus `trigram` for fuzzy/substring
  names), written in the same transaction as the data. Swap to tantivy (and/or fst for names) only
  if the M4 benchmark gate shows FTS5 misses targets.
- **Alternatives:** tantivy from day one (faster, richer; near-real-time, separate index to keep consistent).
- **Consequences:** simplest correct PoC; a measured, contained upgrade path.

## ADR-004 — Evidence and Citation are separate first-class objects
**Status:** Proposed · details in [04-data-model.md](04-data-model.md)

- **Context:** R2, R3 — every fact references its source data; several sources per fact; evidence
  has dates and versions; an article and a fragment of it can both be stored.
- **Decision:** `Evidence` (immutable content: inline text, blob or URI; dates, version,
  `parent` + `locator` for fragments, `supersedes` for versions) and `Citation` (fact ↔ evidence,
  with role, confidence, quote; can target one property).
- **Alternatives:** source fields as properties on nodes/links (cannot hold many sources with their
  own attributes); evidence as ordinary nodes with links (mixes provenance with domain data, slower
  to query, no property-level targets).
- **Consequences:** many-to-many provenance, contradiction tracking, property-level citations;
  one more object kind in the API.

## ADR-005 — Evidence required by default; agent conclusions cite an `observation`
**Status:** Accepted 2026-10-24

- **Context:** R2 says every node and link references its data.
- **Decision:** graph policy `require_evidence = enforce` by default. Facts the agent inferred cite
  an evidence of kind `observation` (its reasoning and context). Policies `warn` / `off` exist for imports.
- **Alternatives:** `warn` by default (easier, but the graph fills with unsourced facts).
- **Consequences:** the invariant holds everywhere; sourced vs inferred facts are distinguishable.

## ADR-006 — Short prefixed IDs, optional natural keys, name references
**Status:** Proposed

- **Context:** agents read and write IDs constantly (token cost, N1); they think in names, not UUIDs.
- **Decision:** IDs are `n_`/`l_`/`e_`/`c_` + base32 of an internal integer, never reused. Optional
  client keys for idempotent upserts. References accept ID, key, type+name, or `@ref` within a batch.
- **Alternatives:** UUID/ULID (globally unique, 26–36 chars, 3–5× the tokens).
- **Consequences:** cheap, readable IDs; IDs are unique per graph only (fine; cross-graph identity
  is a federation topic for later).

## ADR-007 — No query language: structured operations with a closed set of operators
**Status:** Accepted (user requirement R9)

- **Decision:** 11 operations; `find` covers filters, computed values, group-by and aggregates with
  enumerated operators. New needs become new parameters or tools, never a language.
- **Consequences:** predictable, validatable, cacheable requests; easy for agents to fill in;
  very complex analytics may need several calls or a future dedicated tool.

## ADR-008 — One operation layer, two transports (MCP + REST), plus admin REST
**Status:** Proposed

- **Decision:** typed request/response structs per operation; MCP tools (via `rmcp`, stdio +
  Streamable HTTP) and REST endpoints are thin adapters. The JSON Schemas, MCP tool definitions
  and OpenAPI document are **committed files in `spec/`** and are served as-is; the Rust types are
  tested against them, not the other way round (ADR-012). Admin API separate, `admin` role.
- **Consequences:** no logic duplication; future transports (gRPC, FFI) are cheap; the published
  contract cannot drift from the spec.

## ADR-009 — Semantic (vector) search deferred to phase 2
**Status:** Proposed

- **Context:** agents cannot produce embeddings themselves; the server would need a local model.
- **Decision:** PoC ships exact/alias/fuzzy/full-text search; embeddings + HNSW come in phase 2.
- **Consequences:** smaller PoC; `resolve` relies on spelling, not meaning, until phase 2.

## ADR-010 — Build our own core on commodity storage (no existing graph DB underneath)
**Status:** Proposed · details in [01-research-existing-systems.md §6](01-research-existing-systems.md)

- **Decision:** own engine (model, indexes, API) on SQLite; reference projects used for ideas only.
- **Alternatives:** agent API over Neo4j; over SurrealDB or HelixDB embedded.
- **Consequences:** full control of speed and provenance model; more initial work; no vendor or
  licence risk (cf. Kuzu archived in 2025).

## ADR-011 — Soft delete and append-only change log
**Status:** Proposed

- **Decision:** `retract` / `restore` for agents; hard `purge` admin-only; every change logged with
  actor and batch in the same transaction.
- **Consequences:** agent mistakes are reversible; history, audit, "changes since", and later
  replication come from one mechanism; storage grows (purge policy in phase 2).

## ADR-012 — Specification first: a language-neutral contract outside the code
**Status:** Accepted 2026-10-24 (user requirement R12) · details in [10-portability-and-spec.md](10-portability-and-spec.md)

- **Context:** Rust is chosen, but a rewrite in another language must stay easy.
- **Decision:** the normative specification lives in `spec/` (behaviour rules with IDs, JSON
  Schemas, OpenAPI, MCP tool definitions, SQL storage format, algorithms with test vectors) and a
  black-box, data-driven conformance suite in `conformance/` that tests any implementation over
  HTTP. Every behaviour change updates spec and conformance cases before the Rust code. Durable
  data uses only SQLite + JSON + plain blob files — never a Rust-specific format.
- **Alternatives:** docs generated from code (the code becomes the spec — a rewrite must read Rust);
  design docs only (too vague to re-implement identically).
- **Consequences:** more writing per feature; in return a rewrite needs only `spec/` +
  `conformance/`, can open existing data files unchanged, and proves itself by passing the same suite.
