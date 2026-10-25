# MyGraph (working name)

A fast graph database with evidence: every node and link points to the source data it was built
from. Designed for AI agents (MCP and REST APIs, no query language), with a separate admin API.

**Status:** 2026-10-25 · language **Rust**, specification-first so it can be re-written in another
language · **M0 done, M1 (core model and storage) done**: write, retract/restore, merge, blobs and
vocabulary hints on SQLite, with property-based and kill tests; synthetic data generator and
memory measurements · **M2 in progress**: `search` (names in RAM, full text on tantivy) and `get` done, all search budgets met at 1 M nodes. See
[docs/08-poc-plan.md](docs/08-poc-plan.md).

## In one paragraph

A property graph like Neo4j (typed nodes and links with properties; links directed or undirected)
plus two first-class provenance objects: **Evidence** (inline text, URL, file/image blob, agent
observation — with dates, version, fragments of larger sources) and **Citation** (connects a node,
a link or a single property to evidence, with role supports/contradicts and confidence). One Rust
binary keeps the whole graph topology and search indexes in RAM, stores everything durably in
SQLite, and exposes 11 agent operations (search, resolve, get, write, retract, merge, neighbors,
paths, find with aggregations, history, overview) over MCP and REST.

## Building

```bash
cargo test --workspace
```

```bash
cargo run -p mygraph-server -- serve
```

The server listens on `127.0.0.1:7420`; `GET /health` reports the spec and storage format
versions. `crates/mygraph-core/tests/vectors.rs` runs every vector in `conformance/vectors/`.

Synthetic data and memory measurements (keep the data directory outside synced folders):

```bash
cargo run --release -p mygraph-bench -- generate --data ../mygraph-data --graph g100k --nodes 100000
```

```bash
cargo run --release -p mygraph-bench -- load --data ../mygraph-data --graph g100k --breakdown
```

Windows note: if linking fails with `cannot open file 'msvcrt.lib'`, the newest Visual Studio
lacks the x64 desktop libraries; build from a developer prompt of an installation that has them
(e.g. run `vcvars64.bat` from VS 2022 Build Tools first).

## Documents

| # | Document | What it answers |
|---|---|---|
| 00 | [Requirements](docs/00-requirements.md) | What was asked (R1–R11), derived needs (N1–N13), glossary |
| 01 | [Existing systems](docs/01-research-existing-systems.md) | Is there something already? (No.) What to borrow; build-vs-reuse |
| 02 | [Language comparison](docs/02-language-comparison.md) | Rust vs Go vs C# with libraries; recommendation: Rust |
| 03 | [Storage and search](docs/03-storage-and-search.md) | Storage engines, search components, SQLite schema sketch |
| 04 | [Data model](docs/04-data-model.md) | Nodes, links, evidence, citations, IDs, vocabulary, lifecycle, worked example |
| 05 | [Architecture](docs/05-architecture.md) | Components, write/read paths, RAM indexes, concurrency, performance budget |
| 06 | [Agent API](docs/06-agent-api.md) | The 11 operations with request/response examples, errors, compact format |
| 07 | [Admin API](docs/07-admin-api.md) | Management, statistics, maintenance, access, operations |
| 08 | [PoC plan](docs/08-poc-plan.md) | Milestones M0–M4, exit criteria, tests, risks |
| 09 | [Roadmap](docs/09-roadmap.md) | Phase 2 and 3 features |
| 10 | [Portability: specification first](docs/10-portability-and-spec.md) | How `spec/` + `conformance/` keep a rewrite in Go, C# or anything else easy |
| — | [Decisions (ADRs)](docs/decisions.md) | ADR-001…012; Rust and spec-first accepted, the rest *Proposed* |
| — | [Sources](docs/research/sources.md) | Everything consulted, with links |
| — | [spec/](spec/README.md) | Normative contract (M0 and M1 parts written: identifiers, normalization, values, storage, objects, write/retract/merge, vocabulary similarity) |
| — | [conformance/](conformance/README.md) | Shared test vectors, later HTTP cases, benchmarks, agent evals |

## Key recommendations

1. **Build our own core** on commodity storage. Nothing existing combines speed, multi-source
   evidence and an agent-first API; the closest ideas come from Graphiti/Zep (episodes as ground
   truth), Wikidata (references with their own dates), and HelixDB (graph + search in one Rust process).
2. **Rust** (decided), mainly for its native search/storage libraries and memory density — with a
   language-neutral spec, storage format and conformance suite so a rewrite stays easy.
3. **SQLite (WAL) as the durable store, everything searchable in RAM**; FTS5 for text in the PoC,
   tantivy only if benchmarks demand it.
4. **Evidence required by default**; agent conclusions cite an `observation` evidence.
5. **API shaped for agents**: one batch write per source, one rich read per entity, `resolve`
   before writing, short IDs, compact output, errors with hints.
6. **Semantic (vector) search in phase 2**, after the PoC proves the model, API and speed.

## Decisions

All decided on 2026-10-24: **1. Language — Rust**, specification-first (ADR-001, ADR-012), and
the defaults below were accepted as proposed.

| # | Question | Decision |
|---|---|---|
| 2 | Evidence policy | `enforce` (ADR-005): every fact cites evidence; the agent's own conclusions cite an `observation` |
| 3 | Where it runs in the PoC | Local: Claude Code over stdio, plus HTTP on localhost. The same binary can later run on a home server/NAS unchanged |
| 4 | Data size and content languages | Design for up to ~1 M nodes; word search for space-separated scripts (Latin, Cyrillic, Greek…), trigram matching for others. Say so if Chinese/Japanese or other scripts matter |
| 5 | Clients | Claude Code first; REST for custom agents; Claude Desktop through the same MCP server |
| 6 | Repository | Initialize git here, local only until you choose a remote |
| 7 | Name | Keep "MyGraph" as the working name |
