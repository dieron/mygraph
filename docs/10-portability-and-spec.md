# 10 — Portability: specification first

Status: **Accepted approach** · 2026-10-24 · covers R12, N14 · decision ADR-012 in [decisions.md](decisions.md)

Goal: Rust is the implementation, but **the specification is the product definition**. Someone
who has never seen the Rust code should be able to build a compatible implementation in Go, C# or
anything else from two folders — `spec/` and `conformance/` — and prove it by passing the same tests.

---

## 1. Three layers of documentation

| Layer | Folder | Nature | Answers |
|---|---|---|---|
| **Design** | `docs/` (this folder) | Informative: rationale, research, trade-offs, plans | *Why* is it like this? |
| **Specification** | `spec/` | **Normative**: exact behaviour, formats, schemas, algorithms | *What* must any implementation do? |
| **Conformance** | `conformance/` | Executable, data-driven, black-box | *How* do we know an implementation does it? |

The Rust code is the fourth layer: one implementation of the spec. When code and spec disagree,
the spec wins and the code has a bug (or the spec is changed first, deliberately).

## 2. What makes a rewrite easy — the five contracts

| # | Contract | Form | Why it is language-neutral |
|---|---|---|---|
| 1 | **API contract** | JSON Schema (draft 2020-12) per operation request/response, OpenAPI 3.1, MCP tool definitions (JSON), server-instructions text | Every language has JSON Schema validators and OpenAPI tooling |
| 2 | **Storage format** | SQLite DDL + numbered SQL migrations; column encodings; blob-store layout; hashing | Any language can open the same `.db` files — a rewrite can take over existing data unchanged |
| 3 | **Behaviour rules** | Markdown with numbered rules using RFC 2119 words (MUST / SHOULD / MAY) | Plain text, precise, referenceable |
| 4 | **Algorithms** | Pseudocode + formulas + **test vectors** (input → expected output as JSON) for every non-trivial function: ID codec, name normalization, vocabulary casing, date parsing, ranking, resolve scoring, path ordering, aggregates | Vectors pin down edge cases that prose cannot |
| 5 | **Conformance suite** | YAML test cases (setup → request → expected response/matchers) run over HTTP against a base URL | Tests any implementation as a black box |

## 3. Planned layout

```
spec/
  README.md                  how to read the spec, RFC 2119 keywords, versioning rules
  VERSION                    spec version (semver), also reported by the API (overview, /health)
  model/
    objects.md               graph, node, link, evidence, citation — fields, invariants   (from docs/04)
    values.md                property value encoding, partial dates, date-times
    identifiers.md           ID prefixes, base32 alphabet, key rules, reference forms
    normalization.md         name normalization, vocabulary casing rules
  api/
    conventions.md           graph param, references, detail/fields, limits, cursors, truncation
    operations/              one file per operation: overview, search, resolve, get, write, retract,
                             merge, neighbors, paths, find, history — semantics, rules, errors
    errors.md                error and warning codes with exact meaning
    compact-format.md        grammar (EBNF) of the compact text output
    schemas/                 *.schema.json — requests, responses, shared definitions
    openapi.yaml             REST contract (references schemas/)
    mcp-tools.json           MCP tool names, descriptions, input schemas
    server-instructions.md   text sent to agents on connect
  admin/
    admin-api.md, schemas/   admin endpoints
  algorithms/
    upsert-matching.md       node/link matching and property-conflict rules
    search-ranking.md        exact → alias → fuzzy → text merge, score formula, tie-breaks
    resolve-scoring.md       candidate scoring and verdict thresholds
    vocabulary-similarity.md "similar existing type" detection
    traversal.md             neighbors / paths semantics, ordering, caps, k-shortest definition
    aggregation.md           find: null handling, type rules, median/percentile definitions
    pagination.md            cursor format guarantees and stability
  storage/
    storage-format.md        column encodings, FTS sync rules, change-log diff format, blob layout
    schema.sql               storage format v1 (normative)
    migrations/0001_init.sql
conformance/
  README.md                  how to run the suite against any implementation (base URL + API key)
  vectors/                   pure-function test vectors (JSON): ids, normalization, dates, casing, ranking
  cases/                     data-driven API cases (YAML), one folder per operation
  bench/
    dataset.md               generator version + seed + SHA-256 of the JSONL output = the dataset
    scenarios.yaml           benchmark requests and performance targets
  agent-evals/               agent scenarios (tasks + expected facts) — language-independent
```

The runner for `conformance/` is a small tool in this repository, but it only speaks HTTP, so it
tests a Go or C# implementation exactly as it tests the Rust one.

## 4. Rules for writing the spec

1. **Every normative rule has a stable ID**, e.g. `WRITE-UPSERT-3`, `NORM-NAME-2`, `FIND-AGG-7`.
   Rule IDs are never renumbered; removed rules are marked *withdrawn*.
2. **Each rule says one thing** and uses MUST / SHOULD / MAY deliberately.
3. **Every rule that can be tested has a conformance case or test vector** that cites its ID.
4. **No reference to Rust** in `spec/` — no crate names, no Rust types. Implementation notes go to
   `docs/` or code comments.
5. **Pin what varies between platforms**: Unicode version and normalization form, case-folding
   method, floating-point rounding for aggregates, sort collation, timestamp precision (ms, UTC),
   minimum SQLite version and required features (FTS5, `trigram` tokenizer, JSON functions).
6. **Examples are informative, vectors are normative.** If an example and a vector disagree, the
   vector wins.

## 5. Workflow — definition of done for any behaviour change

```
1. ADR (if it is a decision)          docs/decisions.md
2. Spec text + schema change           spec/…              (rule IDs added or changed)
3. Conformance cases / test vectors    conformance/…       (fail against the current build)
4. Rust implementation                 crates/…            (code comments cite rule IDs)
5. CI green:
   - conformance suite passes against the freshly built server
   - every response in the suite validates against spec/api/schemas
   - spec coverage report: rules without a test do not increase
   - storage: a database created by the build matches spec/storage/schema.sql
```

A pull request that changes behaviour without steps 2–3 is incomplete.

## 6. Rust coding rules that keep a rewrite easy

| Rule | Reason |
|---|---|
| The MCP tool schemas and OpenAPI document are **embedded from `spec/` files** at build time and served as-is | The published contract *is* the spec; it cannot drift |
| Rust request/response types are hand-written to match the schemas and **tested against them** (every conformance response is schema-validated) | No "the Rust types are the truth" |
| Business logic lives in `mygraph-core` as plain data structures and functions; transports stay thin | The logic reads like the spec, module by module |
| Module and file names mirror spec sections (`write/upsert.rs` ↔ `spec/algorithms/upsert-matching.md`) | Easy to find the code for a rule, and the rule for the code |
| Code comments cite rule IDs at the implementing site (`// WRITE-UPSERT-3`) | Traceability in both directions |
| No logic hidden in macros or type-level tricks in the core; generics/traits only at the documented seams (`Store`, `TextIndex`, `BlobStore`, `VectorIndex`) | Ports cleanly to languages without Rust's type system |
| **Nothing durable in Rust-specific formats** (no bincode/rkyv/serde-only encodings in SQLite or files of record). RAM-index snapshots (phase 2) are rebuildable caches, never the format of record | Data outlives the implementation |
| Internal seams (traits) are described in `docs/05-architecture.md` with their contracts | A rewrite can keep the same component boundaries |
| Pure functions (ID codec, normalization, date parsing, scoring) are tested from the shared `conformance/vectors` files, not from Rust-only fixtures | The same vectors test every implementation |

## 7. Component map: Rust choice and equivalents

For a rewrite, each component has a direct counterpart. The storage engine (SQLite + FTS5) is the
same in every language, which removes the riskiest part of a port.

| Component | Rust (chosen) | Go | C# / .NET |
|---|---|---|---|
| HTTP server | axum | net/http, chi | ASP.NET Core minimal APIs |
| MCP server | rmcp | modelcontextprotocol/go-sdk | ModelContextProtocol(.AspNetCore) |
| SQLite binding | rusqlite (bundled) | mattn/go-sqlite3 (`sqlite_fts5`) | Microsoft.Data.Sqlite |
| Full-text search | SQLite FTS5 | SQLite FTS5 | SQLite FTS5 |
| Bitmaps | roaring | RoaringBitmap/roaring | community port or custom bitset |
| JSON | serde_json | encoding/json | System.Text.Json |
| JSON Schema validation (tests) | jsonschema | santhosh-tekuri/jsonschema | JsonSchema.Net |
| Unicode normalization | unicode-normalization | golang.org/x/text/unicode/norm | `string.Normalize` |
| SHA-256 | sha2 | crypto/sha256 | System.Security.Cryptography |
| Parallel scans | rayon | goroutines + errgroup | `Parallel.For` / PLINQ |
| Reader/writer lock | parking_lot `RwLock` | `sync.RWMutex` | `ReaderWriterLockSlim` |
| Metrics | metrics + Prometheus exporter | prometheus/client_golang | OpenTelemetry / prometheus-net |
| CLI | clap | cobra | System.CommandLine |
| Property-based tests | proptest | rapid | CsCheck / FsCheck |
| Benchmarks | criterion | testing.B | BenchmarkDotNet |
| *Phase 2:* text engine | tantivy | bleve | Lucene.NET (beta) |
| *Phase 2:* vectors | usearch | usearch (cgo) / coder/hnsw | usearch / HNSW.Net |
| *Phase 2:* embeddings | ort / fastembed | onnxruntime_go | Microsoft.ML.OnnxRuntime |

## 8. Rewrite checklist (what a porting team does)

1. Read `spec/README.md`, then `spec/model/` and `spec/storage/`.
2. Implement the storage layer against `schema.sql`; verify by opening a database produced by the
   Rust build.
3. Implement pure functions; pass `conformance/vectors/`.
4. Implement operations one by one; pass `conformance/cases/<operation>/`.
5. Serve `spec/api/openapi.yaml` and `mcp-tools.json` unchanged.
6. Run `conformance/bench/` on the shared dataset to compare speed with the Rust build.
7. Run `conformance/agent-evals/` to compare agent usability.

## 9. Costs and how we keep them small

- **More writing per feature.** Mitigation: the spec replaces most of what would otherwise be code
  comments and ad-hoc design notes; conformance cases double as integration tests.
- **Two sources to keep in sync (spec and code).** Mitigation: CI checks above; schemas embedded
  from `spec/`; coverage report of rule IDs.
- **Spec can over-constrain internals.** Mitigation: the spec defines observable behaviour and
  stored data only; internal data structures (RAM indexes, locking) stay informative in `docs/`.
