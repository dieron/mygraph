# 05 — Architecture

Status: **Draft for review** · 2026-10-24 · Rust (ADR-001, accepted) and SQLite (ADR-002)

This document is informative: internal structures (RAM indexes, locking, crates) are one way to
meet the spec. Observable behaviour and stored data are defined normatively in `spec/`
(ADR-012, [10-portability-and-spec.md](10-portability-and-spec.md)).

## 1. Components

```
   Claude Code / Claude Desktop          Scripts, apps, other agents            Admin UI (later)
             │ MCP (stdio)                      │ REST/JSON  │ MCP (HTTP)              │ Admin REST
             ▼                                  ▼            ▼                         ▼
 ┌───────────────────────────────────────────── mygraph (one binary) ─────────────────────────────────────┐
 │  Transport layer                                                                                       │
 │   ├─ MCP server (rmcp): stdio  +  Streamable HTTP at /mcp                                              │
 │   ├─ Agent REST  /v1/graphs/{g}/…   (axum, OpenAPI from spec/)                                         │
 │   └─ Admin REST  /admin/…           (separate key/role; optionally separate port)                      │
 │        auth (API keys → actor + role) · request limits · metrics · tracing                             │
 │                                                                                                        │
 │  Operation layer  — typed request/response structs matching committed spec/api/schemas                 │
 │   overview · search · resolve · get · write · retract · merge · neighbors · paths · find · history …   │
 │   (MCP tools and REST endpoints are thin adapters over the same operations — no duplicated logic)      │
 │                                                                                                        │
 │  Engine (per graph)                                                                                    │
 │   ├─ Write coordinator: validate → resolve refs → one SQLite transaction → apply to RAM indexes        │
 │   ├─ Query executor: filters → bitmap/set ops → traversal → aggregation → shaping/truncation           │
 │   └─ In-memory indexes                                                                                 │
 │        records (dense Vec, u32 index) · id/key maps · name & alias map · type/label bitmaps            │
 │        adjacency out/in · citations by target / by evidence · vocabulary counts                        │
 │                                                                                                        │
 │  Storage traits                                                                                        │
 │   ├─ Store      → SQLite (WAL): records, change log, vocabulary          (PoC)                         │
 │   ├─ TextIndex  → tantivy (fallback: SQLite FTS5 tables, still written)  (PoC)                        │
 │   ├─ BlobStore  → local content-addressed files                          (PoC)  → S3 (later)           │
 │   └─ VectorIndex→ —                                                      (phase 2: usearch + ONNX)     │
 └────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

## 2. Code layout (Rust workspace)

| Crate | Responsibility |
|---|---|
| `mygraph-core` | Model types, validation, engine, in-memory indexes, query executor, storage traits. No HTTP/MCP dependencies. |
| `mygraph-store-sqlite` | `Store` implementation (with FTS5 full text), schema migrations, local blob store |
| `mygraph-text-tantivy` | `TextIndex` on tantivy: the full-text index used for search |
| `mygraph-api` | Request/response DTOs for every operation, matching `spec/api/schemas`; embeds the spec's MCP tool definitions and OpenAPI document and serves them unchanged |
| `mygraph-server` | The binary: `serve` (HTTP: REST + MCP), `mcp` (stdio), `import`, `export`, `check`, `bench-data` subcommands; config; auth; metrics |
| `mygraph-bench` | Synthetic data generator, load scenarios, latency reports |
| `mygraph-conformance` | Black-box runner for `conformance/` (vectors + HTTP cases); works against any implementation, not only this one |

Next to the crates, at the repository root: `spec/` (normative contract) and `conformance/`
(shared test data) — see [10-portability-and-spec.md §3](10-portability-and-spec.md).

The operation layer is the product. Transports stay thin so a gRPC or a native library binding
(Python/Node via FFI) can be added later without touching the engine.

**Internal seams** (informative, kept so a rewrite can reuse the same boundaries):

| Seam | Contract |
|---|---|
| `Store` | `load_into` (streams actors and rows into a `LoadSink`, which the in-memory state implements; `load` collects them into a snapshot), `counters` (last used internal numbers), `apply` (one `ChangeSet` — rows, batch, change-log entries — in one transaction), plus change-log queries for restore and retract-by-batch. The engine assigns internal numbers and change-log sequence numbers itself, so a change set is complete before it reaches the store |
| `TextIndex` | External full-text index (`mygraph-core::store::TextIndex`): `index` (replace documents; searchable at once), `commit` (make them durable and record the change-log seq they cover; slow, so the server commits in the background about once a second and writes never wait for it), `pending`, `clear`, `applied_seq`, `search` (tokens → ranked object numbers; uncommitted documents are matched from memory). A rebuildable cache ([STORE-TEXT-1]); implementation `TantivyText` in `mygraph-text-tantivy`. Without one, the store's FTS5 tables answer |
| `BlobStore` | `put` (bytes → sha256, written and synced before it returns), `size` (also answers "exists"), `get`; delete only by admin GC. Reached through `Store::blobs()`; local implementation `BlobDir` in `mygraph-store-sqlite` |
| `VectorIndex` (phase 2) | Upsert/remove vector for an object; k-nearest query with an optional allowed-set filter |

## 3. Write path

```
request ──▶ parse & validate DTO (sizes, types, required fields)
        ──▶ resolve references (ids, keys, type+name, @batch refs) against RAM indexes
        ──▶ plan: create / match / update per object; check evidence policy; normalize vocabulary
        ──▶ take the graph's writer lock (one writer per graph)
        ──▶ SQLite: BEGIN → insert/update rows → FTS rows → change-log rows → COMMIT (fsync, WAL)
        ──▶ apply the same changes to the RAM indexes (write lock held for microseconds)
        ──▶ release locks, return result: ref→id map, created/matched/updated, warnings
```

- **Atomic batches**: the whole write call is one transaction — all or nothing.
- **Durability first**: RAM is changed only after the commit succeeds, so RAM never holds
  something the disk does not. If applying to RAM ever fails, the engine marks the graph dirty and
  rebuilds its indexes from SQLite.
- **Read-your-writes**: the response is sent after the RAM update, so the next call from the same
  agent sees its write.
- Later: group commit (several concurrent batches share one fsync) for higher write throughput.

## 4. Read path

All topology, names, types, properties and citation lookups are served from RAM. Disk is touched
only for full-text search (FTS5) and for evidence content/blobs when they are requested.

```
request ──▶ candidate set:  ids/keys → direct  |  name → hash map  |  text → FTS5  |  type → bitmap  |  linked → that node's adjacency
        ──▶ filters:        bitmap intersection, then property predicates on the survivors
        ──▶ traversal:      adjacency walk with per-hop filters and caps (BFS / bidirectional BFS for paths);
                            a compact type number per link skips other link types without reading records
        ──▶ aggregation:    single pass (parallel with rayon above a size threshold)
        ──▶ shaping:        field selection, detail level, support summaries, truncation + cursor
```

Every response states when it was truncated and how to get more, so the agent never mistakes a
partial answer for a complete one.

## 5. In-memory index structures

| Structure | Shape | Purpose |
|---|---|---|
| Node records | `Vec<NodeRec>` indexed by `u32` | Type id, status, name/aliases (string arena), props (compact encoded), version, timestamps |
| Link records | `Vec<LinkRec>` | type id, src, dst, directed flag, props, validity, version |
| Adjacency | per node: out-list and in-list of link indexes (undirected links in both) | O(degree) neighbourhood |
| ID / key maps | `HashMap<key, u32>` per kind | Lookups by key; IDs are arithmetic (base32 ↔ integer) |
| Name map | `HashMap<normalized name or alias, SmallVec<u32>>` | Exact, case/accent-insensitive lookup and entity resolution |
| Type sets | Sorted id sets of active nodes per node type and active links per link type (M2; labels not yet) | `find` starts from them; counts per type without a scan |
| Citations | by target (node/link/prop) and by evidence | "Why do we believe this?" and "what came from this source?" |
| Evidence metadata | `Vec<EvidenceRec>` without content | Dates, kind, parent, hash — content fetched from SQLite on demand |
| Vocabulary | type / link type / property key → count, description | Overview, normalization, similar-name hints |
| String interner | type names, property keys | 2-byte ids instead of strings in every record |

### Memory estimate at PoC scale (to be measured in M4)

| Item | Count | ≈ bytes each | ≈ total |
|---|---:|---:|---:|
| Nodes (with names, ~5 props) | 1 M | 250 | 250 MB |
| Links (incl. adjacency entries) | 5 M | 56 | 280 MB |
| Evidence metadata | 1 M | 100 | 100 MB |
| Citations | 3 M | 24 | 72 MB |
| Hash maps, bitmaps, overhead | | | 150–250 MB |
| **Total** | | | **≈ 0.9 – 1 GB** |

If that becomes too much, properties can stay on disk with only indexed properties in RAM — a
contained change inside the engine.

### Measured in M1 (2026-10-25)

`mygraph-bench` generates graphs through the engine's own `write` and measures exact heap bytes
per object (a counting allocator). Shape per node: 1 evidence, 5 links, 6 citations (every node and
link cites evidence), about 5 properties per node, 30 % of links with properties.

| Bytes per object (heap, in the loaded state) | Estimate above | First PoC layout | After M1 compaction |
|---|---:|---:|---:|
| Node (with name index entry) | 250 | 1 417 | 631 |
| Link (with adjacency) | 56 | 620 | 259 |
| Evidence metadata | 100 | 1 407 | 686 |
| Citation | 24 | 719 | 201 |

Full PoC dataset (1 M nodes, 5 M links, 1 M evidence, 6 M citations; generator v1, seed 42;
8.3 GB SQLite file; generated in 59 min, links at ~1 400/s):

| Layout | Graph state in RAM | Load time (warm / cold OS cache) |
|---|---:|---:|
| First PoC layout | 9.5 GB (+ 5.5 GB row snapshot held during load) | 38.6 s / — |
| After M1 compaction | 3.8 GB (rows streamed, no snapshot) | 25.1 s / 36.9 s |

M1 compaction, in order of effect: records in dense tables indexed by internal number instead of
hash maps; index sets that keep one element inline; the link, citation and evidence identity maps
replaced by lookups through adjacency and per-target citation lists (evidence identity keeps only a
64-bit hash); type names and property keys interned; properties as a sorted vector instead of a
tree map; rows streamed from SQLite into the indexes (no full copy of the rows during load).

Still above the estimate, and why: records keep every field in RAM (record-time metadata, quotes,
evidence titles and authors) in general-purpose structs (`Option<String>` = 24 bytes, metadata 40
bytes, 8-byte numbers). Next steps, in expected order of gain, when the M4 benchmark asks for
them: (1) a hot/cold split — keep in RAM what search, traversal, matching and filters need, read
the rest (record metadata, quotes, notes, most evidence fields) from SQLite on `get`; (2) 32-bit
internal numbers and actor ids; (3) the packed layout of the table above (string arena, encoded
properties).

## 6. Concurrency

- One **writer lock per graph** serializes writes (SQLite allows one writer anyway).
- Readers take a **read lock** on the RAM indexes; writers hold the write lock only while applying an
  already-committed change (microseconds).
- Long reads (big aggregations) are bounded by per-request limits and time budgets. If they start
  blocking writes noticeably, phase 2 moves to snapshot reads (versioned records or persistent
  data structures) so readers never block writers.
- Graphs are independent: different graphs never contend.

## 7. Startup and recovery

1. Open each graph's SQLite file; check schema version; run migrations.
2. Stream active records into RAM and build indexes (parallel parsing).
3. Mark the graph ready; until then, calls get a "warming up" error with progress.

Target: < 30 s for 1 M nodes / 5 M links. Later: persist a binary index snapshot (`rkyv`) with the
change-log sequence it covers, load it with zero-copy, and replay only newer change-log entries.
The snapshot is a rebuildable cache, never the format of record (ADR-012): deleting it only costs
a slower start.

SQLite's WAL gives crash safety; the RAM indexes are always rebuildable, so they never need their
own recovery logic.

## 8. Transports and protocol details

- **MCP over stdio**: `mygraph mcp --data <dir> --graph <name>` — Claude Code / Desktop launch it
  as a local server. No network, no key.
- **MCP over Streamable HTTP** at `/mcp`: targets the stateless 2026-07-28 protocol revision (every
  tool call self-contained, which suits us: the graph and all context travel in the call), and
  negotiates down to 2025-11-25 for older clients — the SDKs handle this.
- **MCP server instructions + prompts**: the server ships its own usage guide (how to record facts
  with evidence, how to search before writing, ID formats) so any agent learns the conventions on
  connect.
- **REST**: the same operations as `POST /v1/graphs/{graph}/{operation}` with JSON bodies, plus
  conventional `GET` for single objects; the OpenAPI document from `spec/api/openapi.yaml` is
  served at `/openapi.yaml`.

## 9. Security (PoC baseline)

- stdio: trusted local process.
- HTTP: binds to `127.0.0.1` by default; every request needs `Authorization: Bearer <key>`; keys
  are configured with an actor name and a role (`agent-read`, `agent-write`, `admin`).
- Admin endpoints require `admin`.
- Request size limits, result caps, and per-request time budgets protect against runaway calls.
- Later: per-graph permissions, OAuth for remote MCP, TLS termination guidance, audit export.

## 10. Observability

- `tracing` span per operation with graph, actor, sizes and timings.
- Prometheus `/metrics`: request counts and latency histograms per operation, index sizes, RAM,
  SQLite page cache hits, write batch sizes.
- Slow-operation log (configurable threshold) with the request shape (not content).
- Per-tool usage counters — shows how agents actually use the API, which guides API changes.

## 11. Performance budget (server-side, PoC dataset 1 M / 5 M / 1 M, laptop)

| Operation | Target p99 |
|---|---|
| `get` by id/key (with support summary) | < 1 ms |
| Exact name/alias lookup | < 0.5 ms |
| Full-text search, top 20 | < 10 ms |
| Fuzzy name search, top 20 | < 15 ms |
| 2-hop neighbourhood, ≤ 200 results | < 5 ms |
| Shortest path between two nodes | < 50 ms |
| Count/group-by over all nodes of a type (1 M) | < 50 ms |
| Write batch of 50 objects (with fsync) | < 20 ms |
| Cold start | < 30 s |

These are targets to verify in milestone M4, not promises. The bench harness reports p50/p95/p99
for each.
