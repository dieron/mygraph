# 02 — Language comparison: Rust vs Go vs C#

Status: **Decided — Rust** (2026-10-24), with the condition that everything stays documented for a
rewrite (R12, ADR-012) · decision recorded as ADR-001 in [decisions.md](decisions.md) · the
component-by-component library equivalents for Go and C# are in
[10-portability-and-spec.md §7](10-portability-and-spec.md)

The engine will spend its life doing four things: keeping a large graph in RAM, running
text/name/fuzzy searches, walking adjacency lists, and serving many small API calls. The
language choice is therefore mostly a choice of **search and storage libraries** and of
**memory layout control**. HTTP frameworks and MCP SDKs are good in all three.

---

## 1. Libraries by need

### Rust

| Need | Libraries | Notes |
|---|---|---|
| HTTP API | **axum** (tokio/tower), actix-web | Top-tier throughput, low latency |
| OpenAPI | utoipa, aide | Generated from types |
| MCP server | **rmcp** — official `modelcontextprotocol/rust-sdk`, 3.x | Targets the current MCP spec (2026-07-28, stateless); stdio + Streamable HTTP; tool macros |
| SQLite | **rusqlite** (`bundled`, FTS5 included), sqlx | Mature, zero-cost bindings |
| Embedded KV | **redb** 4.x (pure-Rust copy-on-write B-tree, ACID), **fjall** 3.0 (pure-Rust LSM, Jan 2026), **heed** (LMDB; used by Meilisearch and HelixDB), rust-rocksdb | Best embedded-KV choice of the three languages |
| Full-text search | **tantivy** (Lucene-class engine behind Quickwit and ParadeDB), Meilisearch `milli` | ~2× Lucene on the Quickwit search benchmark (gap reportedly narrowed since) |
| Prefix / fuzzy names | **fst** (finite-state transducers + Levenshtein automata) | Microsecond fuzzy lookup over millions of names |
| Vector ANN (phase 2) | usearch, hnsw_rs, arroy (LMDB-backed, Meilisearch), LanceDB | |
| Local embeddings (phase 2) | ort (ONNX Runtime), fastembed-rs, candle | ort is a community wrapper, but widely used |
| Bitmaps | roaring-rs, croaring | For type/label sets and filter intersection |
| Graph algorithms | petgraph, rustworkx-core | PageRank, components, shortest paths |
| Concurrency | tokio, rayon, parking_lot, dashmap, arc-swap | rayon gives parallel aggregations for free |
| Serialization | serde (+ sonic-rs / simd-json), rkyv (zero-copy snapshots), postcard | |
| Analytics (later) | arrow-rs, DataFusion, Polars | If aggregations outgrow hand-written code |
| Observability / bench | tracing, metrics + Prometheus, OpenTelemetry · criterion, divan, flamegraph | |

### Go

| Need | Libraries | Notes |
|---|---|---|
| HTTP API | net/http (pattern routing since 1.22), chi, echo, gin | Excellent standard library |
| OpenAPI | huma, ogen, oapi-codegen | |
| MCP server | **official `modelcontextprotocol/go-sdk`** v1.7 (co-maintained with Google), mark3labs/mcp-go | Full 2026-07-28 support; the most polished SDK of the three |
| SQLite | mattn/go-sqlite3 (cgo, `sqlite_fts5` tag), modernc.org/sqlite (pure Go, slower), ncruces/go-sqlite3 (WASM, pure Go) | Fast option needs cgo |
| Embedded KV | **Pebble** (CockroachDB, RocksDB-style LSM, well maintained), Badger (maintenance tied to Dgraph's changing owners), bbolt (etcd, B+tree) | Good choices |
| Full-text search | **bleve v2** (active; text, numeric, geo, vector, hybrid RRF) | Slower and larger indexes than tantivy/Lucene is the common experience — no public benchmark found; vector search needs FAISS via cgo |
| Prefix / fuzzy names | vellum (FST, from the bleve project) | |
| Vector ANN | coder/hnsw (pure Go), usearch (cgo), bleve + FAISS (cgo) | |
| Local embeddings | onnxruntime_go (cgo), hugot | Every ML path goes through cgo |
| Bitmaps | **RoaringBitmap/roaring** | Reference-quality |
| Graph algorithms | gonum/graph | |
| Observability / bench | pprof (built in), OpenTelemetry, Prometheus client · testing.B, benchstat | Best built-in profiling of the three |

### C# / .NET 10

| Need | Libraries | Notes |
|---|---|---|
| HTTP API | **ASP.NET Core / Kestrel** minimal APIs | Top-tier throughput |
| OpenAPI | Built-in `Microsoft.AspNetCore.OpenApi` (.NET 9+) | |
| MCP server | **official `ModelContextProtocol` + `.AspNetCore`** (Microsoft + Anthropic), 1.0 Mar 2026, 1.1 since | 1.0 shipped for spec 2025-11-25; support for 2026-07-28 *(verify)* |
| SQLite | **Microsoft.Data.Sqlite** (bundled e_sqlite3 includes FTS5), Dapper, EF Core | Mature |
| Embedded KV | LMDB via Lightning.NET (community), RocksDbSharp (community wrapper), FASTER KV (Microsoft Research; effort moved to Garnet/Tsavorite), ZoneTree (pure C# LSM, single maintainer, self-reported benchmarks), LiteDB | **Weakest area** — no widely adopted, actively maintained, high-performance embedded KV |
| Full-text search | Lucene.NET 4.8 (**still beta** — beta00018, June 2026 — and based on Lucene 4.8 from 2014) | **Weakest area** — no modern native engine; options are SQLite FTS5, an external server, or an FFI bridge to tantivy |
| Prefix / fuzzy names | Nothing mainstream outside Lucene.NET internals | Would be hand-written |
| Vector ANN | HNSW.Net, usearch (C# binding), Microsoft.Extensions.VectorData (abstraction only) | |
| Local embeddings | **Microsoft.ML.OnnxRuntime** (first-party, GPU/DirectML), Microsoft.ML.Tokenizers, Microsoft.Extensions.AI | **Best of the three** |
| Bitmaps | Small community Roaring ports / CRoaring wrappers *(verify)* | Thin |
| Graph algorithms | QuikGraph | |
| Observability / bench | OpenTelemetry .NET, dotnet-counters/trace · **BenchmarkDotNet** (best in class) | |

---

## 2. Pros and cons

### Rust
**Pros**
- Fastest and most predictable: no GC, so no pause-driven tail latency on a heap holding millions of graph records.
- Full control of memory layout: a node record can be ~40–60 bytes plus properties (u32 indexes, packed enums, arenas), roughly 2–4× denser than an object-per-node design in a GC language. RAM density decides how big a graph fits in memory, which decides search speed.
- **Best native search stack**: tantivy + fst + roaring + redb/fjall/heed + usearch, all in-process, no FFI.
- Most of the relevant engines (HelixDB, SurrealDB, TypeDB 3, Oxigraph, Meilisearch, Quickwit) are Rust, so their code is a readable reference.
- Official MCP SDK is current with the latest spec.
- Strict compiler is a strong safety net when much of the code is written by an AI agent: whole classes of bugs (data races, use-after-free, null) do not compile.

**Cons**
- Slowest to write and to learn; graph structures fight the borrow checker unless designed with index-based arenas (which we would do anyway for density).
- Longer compile times.
- Fewer people can maintain it than Go or C#.

### Go
**Pros**
- Fastest to write and to read; tiny language, fast compiles, great tooling (pprof, race detector).
- Excellent HTTP and concurrency story; single static binary (when cgo-free).
- Official MCP SDK is the most mature of the three.
- Good KV stores (Pebble, bbolt) and the reference Roaring library.

**Cons**
- GC scans pointer-heavy heaps: a big in-RAM graph must be built as flat slices of structs and integer IDs to keep GC cost down — doable, but it works against the language's natural style.
- Search ecosystem is the weak spot: bleve is slower than tantivy/Lucene; serious vector or ML work needs cgo, which breaks easy cross-compilation and adds call overhead.
- Less control over layout and no portable SIMD; peak speed is usually below Rust and recent .NET.

### C# (.NET 10)
**Pros**
- Very productive with excellent IDE tooling and BenchmarkDotNet.
- Modern .NET is genuinely fast: Span/Memory, value types, SIMD `Vector<T>`, frozen collections, Kestrel among the fastest HTTP servers; Native AOT gives a single binary.
- First-party ONNX Runtime: the easiest path to local embeddings and semantic search (phase 2).
- Official MCP SDK backed by Microsoft and Anthropic.

**Cons**
- **Search and storage libraries are the weakest**: Lucene.NET is a long-running beta of a 2014 design; embedded KV options are community or research projects. We would rely on SQLite FTS5 or bridge to tantivy over FFI — losing much of the "use .NET" benefit.
- GC heap of millions of objects needs struct-of-arrays design to stay fast (same discipline as Go, with better tools for it).
- Native AOT restricts reflection-heavy libraries.

---

## 3. Weighted scoring (opinion, weights follow R10 "speed, especially search")

| Criterion | Weight | Rust | Go | C# |
|---|---:|---:|---:|---:|
| Native search & storage libraries | 5 | 5 | 3 | 2 |
| Raw speed & tail latency | 4 | 5 | 3.5 | 4 |
| Memory density of a large in-RAM graph | 4 | 5 | 3 | 3.5 |
| API + MCP ecosystem | 3 | 4.5 | 5 | 4.5 |
| Development speed / ergonomics | 3 | 2.5 | 5 | 4.5 |
| Local embeddings / ML (phase 2) | 2 | 4 | 2.5 | 5 |
| Single-binary deployment | 2 | 5 | 5 | 4 |
| Safety net for AI-written code | 2 | 5 | 3 | 4 |
| **Weighted average** | | **4.6** | **3.7** | **3.7** |

If development speed were weighted 5 and search libraries 3, the three would be within ~0.3
of each other. The ranking is driven by R10.

## 4. Recommendation

**Rust** for the engine and both APIs (one binary).

- The one thing this project must be best at — search speed over a large in-RAM graph — is where
  Rust's ecosystem is strongest and where the other two would need FFI or hand-written code.
- Its main cost, development speed, is reduced when an AI agent writes much of the code under a
  compiler that rejects whole categories of mistakes.

**What would change this:**
- If the people maintaining this long-term are C# developers and speed targets can be met with
  SQLite FTS5 + in-RAM indexes → **C#** is a sound second choice (benchmark in the PoC would tell).
- If time-to-first-demo dominates everything → **Go**, accepting a weaker search stack.

A neutral detail: the admin UI (later) can be any web stack; it only talks to the admin REST API.
