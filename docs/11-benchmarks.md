# 11 — External benchmarks and tests

Status: **Draft** · 2026-10-29 · extends M4 in [08-poc-plan.md](08-poc-plan.md)

MyGraph's own benchmarks use a synthetic dataset ([crates/mygraph-bench](../crates/mygraph-bench)).
This plan adds published datasets, workloads and benchmarks, so MyGraph is measured against
something others measure too.

Most graph database benchmarks test a **query language** (Cypher, GQL, Gremlin) or **whole-graph
algorithms**. MyGraph has neither: it has 11 agent operations plus evidence. So we do not "pass"
those benchmarks; we take their data, their workloads and their reference answers, and map them to
our operations. Results are reported as *derived from* a benchmark, never as official results
(LDBC's fair use policy reserves "LDBC SNB result" for audited runs).

## 1. Groups

| # | Group | What it tells us | Benchmarks |
|---|---|---|---|
| 1 | Correctness against a reference database | Traversal, filters and aggregates return what a mature engine returns | Same data in MyGraph and Neo4j; random `neighbors` / `paths` / `find` requests translated to Cypher; results compared (the idea behind GDsmith and GDBMeter) |
| 2 | Performance | Latency, throughput, RAM and load time on realistic, skewed data; the cost of provenance | LDBC SNB Interactive (short reads IS1–7, inserts, a subset of complex reads), LinkBench-style mixed load |
| 3 | Agent benchmarks | Whether an agent with the skill stores and retrieves knowledge well, and cites the right evidence | HotpotQA / MuSiQue (graded on supporting facts), LongMemEval, LoCoMo, MemoryAgentBench, CypherBench-style QA (our tools versus Cypher on Neo4j) |
| 4 | Components | Precision and recall of `resolve` | Leipzig entity-resolution datasets (Abt-Buy, Amazon-Google, DBLP-ACM) |

Not planned: LDBC Graphalytics and Graph500 (whole-graph algorithms; no `analyze` operation yet,
[09-roadmap.md](09-roadmap.md) phase 3) and the openCypher TCK (no query language).

**Order:** (1) LDBC SNB load plus the Neo4j comparison — one dataset serves groups 1 and 2;
(2) HotpotQA / MuSiQue with citation grading; (3) LongMemEval and LoCoMo; (4) `resolve` on the ER
datasets; (5) tools-versus-Cypher QA. Groups 1 and 4 run in CI on small slices; groups 3 and 5 cost
model tokens and run on demand.

## 2. Step 1: LDBC SNB Interactive

### Data

LDBC SNB Interactive v1, Datagen v1.0.0, format `CsvMergeForeign` with `StringDateFormatter`
(one file per entity with its foreign keys as columns; `|`-separated; ISO dates), from
<https://ldbcouncil.org/benchmarks/snb/datasets/>:

| Scale | Archive | Bytes | SHA-256 |
|---|---|---|---|
| SF0.1 | `social_network-sf0.1-CsvMergeForeign-StringDateFormatter.tar.zst` | 17 274 753 | `c2b07d8c24735dcf3be168948c80936b11da14b77905578513d479890d65a4f6` |
| SF1 | `social_network-sf1-CsvMergeForeign-StringDateFormatter.tar.zst` | 222 302 549 | `bfa76cb798e507e931e1bc46830ade8f22d0096bcd7ec820856252c5ec17f6c2` |

The loader reads the `.tar.zst` archive directly; nothing is unpacked. Keep archives and graphs
outside synced folders.

### Mapping

Crate [`mygraph-ldbc`](../crates/mygraph-ldbc) writes through the engine's own `write`, in batches
of at most 1000 items ([OP-WRITE-1]): all nodes first, then all links.

- **Node types** follow the LDBC schema: `Person`, `Forum`, `Post`, `Comment`, `Organisation`,
  `Place`, `Tag`, `TagClass`. LDBC subtypes become labels (`Company` / `University`,
  `City` / `Country` / `Continent`).
- **Keys** are `<table>/<LDBC id>` (`person/933`), because LDBC ids repeat across tables and
  person names repeat within one. A key also skips name matching ([WRITE-MATCH-NODE-1]).
- **Names:** persons `firstName lastName`, forums their title, posts and comments `Post <id>` /
  `Comment <id>`, the rest their LDBC name. All LDBC columns are properties, with LDBC's column
  names; dates become `$date`, timestamps `$datetime`. A person's emails and languages are list
  properties `email` and `speaks`. Empty values are left out.
- **Link types** use LDBC's names: `KNOWS` (undirected), `HAS_CREATOR`, `CONTAINER_OF`,
  `HAS_MODERATOR`, `HAS_MEMBER`, `HAS_TAG`, `HAS_INTEREST`, `IS_LOCATED_IN`, `REPLY_OF`, `LIKES`,
  `STUDY_AT`, `WORK_AT`, `IS_PART_OF`, `HAS_TYPE`, `IS_SUBCLASS_OF`. Link attributes
  (`creationDate`, `joinDate`, `classYear`, `workFrom`) are link properties.
- **Provenance** (`--provenance`): `none` (policy `require_evidence = off`, no citations) or `file`
  (one evidence per CSV file, every node and link cites the evidence of the file it came from). The
  difference between the two runs is the measured cost of evidence.

### Running it

```bash
cargo run --release -p mygraph-ldbc -- load --archive <dir>/social_network-sf0.1-CsvMergeForeign-StringDateFormatter.tar.zst --data <data dir> --graph ldbc-sf01-file --provenance file
```

```bash
cargo run --release -p mygraph-bench -- load --data <data dir> --graph ldbc-sf01-file --breakdown
```

### Results

2026-10-29, Windows 11, 8 cores, 32 GB RAM, NVMe SSD; one loader thread; data outside synced
folders.

**SF0.1**: 327 588 nodes and 1 477 965 links. Every node and link count equals the number of CSV
rows it comes from; no row points to a missing node; no row matched an earlier object.

| | `--provenance none` | `--provenance file` | Cost of evidence |
|---|---|---|---|
| Evidence / citations | 0 / 0 | 18 / 1 805 553 | |
| Load: nodes / links | 41 s / 104 s | 65 s / 197 s | |
| Write throughput: nodes/s, links/s | 8 006 / 14 228 | 5 069 / 7 488 | ~1.8× slower |
| Load total | 145 s | 262 s | +81 % |
| File size | 1 224 MiB | 2 042 MiB | +67 % |
| RAM after opening (heap) | 653 MiB | 1 011 MiB | +55 % |
| Open time (warm cache) | 5.1 s | 6.7 s | |

Per object in memory: node 917 bytes (posts and comments carry their text as a property), link
254 bytes, citation 207 bytes. Findings so far:

- **Citations dominate the cost of evidence**, not evidence itself: one citation per fact
  roughly doubles the object count. A cheaper representation for "everything in this batch cites
  this one source" (a batch-level citation) would cut most of it.
- **The file is large**: about 650 bytes per object without evidence (1.2 GB for 1.8 M objects),
  compared with 59 MiB of CSV tables (17 MB compressed). Where the bytes go (change log, FTS5 tables,
  indexes) is the next thing to measure.

### Then

1. Load SF1 (3.2 M nodes, ~17 M links); RAM is expected around 10 GB with evidence.
2. Neo4j on the same data (LDBC's own Neo4j loader) and the comparison harness for group 1.
3. Workload driver: IS1–IS7, the inserts of the update streams, and IC1 / IC13-style reads, with
   p50 / p95 / p99 for MyGraph and Neo4j on the same machine.
