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
  `Comment <id>`, the rest their LDBC name. The other LDBC columns are properties, with LDBC's
  column names (a column that is the whole name is not repeated; persons keep `firstName` and
  `lastName`); dates become `$date`, timestamps `$datetime`. A person's emails and languages are list
  properties `email` and `speaks`. Empty values are left out.
- **Link types** use LDBC's names: `KNOWS` (undirected), `HAS_CREATOR`, `CONTAINER_OF`,
  `HAS_MODERATOR`, `HAS_MEMBER`, `HAS_TAG`, `HAS_INTEREST`, `IS_LOCATED_IN`, `REPLY_OF`, `LIKES`,
  `STUDY_AT`, `WORK_AT`, `IS_PART_OF`, `HAS_TYPE`, `IS_SUBCLASS_OF`. Link attributes
  (`creationDate`, `joinDate`, `classYear`, `workFrom`) are link properties.
- **Validity:** `KNOWS` and `HAS_MEMBER` links are valid from their `creationDate` / `joinDate`,
  `WORK_AT` from the start of its `workFrom` year (`valid_from`, [TRAV-2]), so `as_of` has
  something to filter. The properties stay as well; LDBC has no end dates.
- **Naming:** graphs are created with `naming: free` ([VOCAB-1]), so names stay exactly as in
  LDBC and in Neo4j (`locationIP`, not the `location_ip` that `naming: styled` would ask for).
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
| Load: nodes / links | 41 s / 99 s | 53 s / 177 s | |
| Write throughput: nodes/s, links/s | 7 961 / 14 954 | 6 133 / 8 336 | |
| Load total | 140 s | 231 s | +65 % |
| File size | 1 218 MiB | 2 036 MiB | +67 % |
| RAM after opening (heap) | 650 MiB | 1 008 MiB | +55 % |
| Open time (warm cache) | 4.9 s | 6.2 s | |

(Rerun on a quiet machine with the current mapping; an earlier run alongside other work took
145 s and 262 s. Adding `valid_from` to 140 654 links later made the `none` load 156 s and the
file 1 227 MiB.)

**SF1** (`--provenance file`): 3 181 724 nodes, 17 256 038 links, 20 437 762 citations; every
count equals its CSV rows, nothing dangling. Load 3 612 s (nodes 638 s at 4 986/s, links 2 973 s
at 5 803/s), file 23.0 GiB, 10.9 GiB of RAM after opening, which takes 123 s. This run shared the
machine with builds, the Neo4j import and other loads, and used the earlier mapping (names
repeated as properties, keys renamed to lower_snake), so its times are an upper bound. Per object
it is close to SF0.1 (heap ~270 bytes per node, link or citation on average), so memory grows
linearly; at this size a 16 GB machine is the minimum.

Per object in memory: node 917 bytes (posts and comments carry their text as a property), link
254 bytes, citation 207 bytes. Findings so far:

- **Citations dominate the cost of evidence**, not evidence itself: one citation per fact
  roughly doubles the object count. A cheaper representation for "everything in this batch cites
  this one source" (a batch-level citation) would cut most of it.
- **The file is large**: about 650 bytes per object without evidence (1.2 GB for 1.8 M objects),
  compared with 59 MiB of CSV tables (17 MB compressed). Where the bytes go (change log, FTS5 tables,
  indexes) is the next thing to measure.

### Neo4j with the same graph

`mygraph-ldbc neo4j-csv` writes the graph as input for `neo4j-admin database import full`, from the
same mapping code as the MyGraph load: label = MyGraph type plus subtype labels, properties `key`,
`name` and the same properties with the same values. Neo4j relationships are always directed, so
`KNOWS` is stored once per pair and queried without a direction.

Setup on Windows (no Docker or WSL needed): Neo4j Community 2026.10.1 (zip from
`dist.neo4j.org`) and the Temurin 21 JRE, unpacked next to the data, outside synced folders. In
`conf/neo4j.conf`: `dbms.security.auth_enabled=false` (it listens on localhost only),
`dbms.usage_report.enabled=false`, heap 4 GB, page cache 4 GB.

```powershell
mygraph-ldbc neo4j-csv --archive <archive> --out <dir>
$env:JAVA_HOME = '<jre folder>'
$a = Get-Content <dir>\import.args
& <neo4j>\bin\neo4j-admin.bat database import full neo4j @a
& <neo4j>\bin\neo4j.bat console
```

The database name goes before the arguments (the last `--relationships` would take it as a file),
and the Windows script does not expand `@file`, hence `Get-Content`. Then one unique constraint on
`key` per label (`CREATE CONSTRAINT Person_key FOR (n:Person) REQUIRE n.key IS UNIQUE`, …), and
`CALL db.awaitIndexes()` in a separate transaction.

**SF0.1 in Neo4j**: 327 588 nodes and 1 477 965 relationships; every count per label and per
relationship type equals MyGraph's.

| | Neo4j 2026.10.1 (offline import) | MyGraph (`write`, no evidence) |
|---|---|---|
| Load | 5.6 s (10.5 s with JVM start) | 140 s |
| On disk | 141 MiB, 151 MiB with the key indexes | 1 218 MiB |

The load times are not like for like: `neo4j-admin import` builds the store offline from sorted
input, while MyGraph applies every batch as a transaction with matching, validation, the change log
and the search indexes.

### Comparing results with Neo4j

`mygraph-ldbc diff` opens the MyGraph graph in-process and sends Cypher to Neo4j's HTTP API. Each
case is a random MyGraph request plus a Cypher query meant to return the same thing; results are
normalized (node IDs to keys, order removed, one form for temporal values and numbers) and compared.
It exits with status 1 when any case differs.

```bash
mygraph-ldbc diff --data <data dir> --graph ldbc-sf01-none --cases 700 --seed 1
```

| Family | MyGraph request | Compared |
|---|---|---|
| `neighbors_1` | 1 hop from a random node along a link its type has in the LDBC schema (sometimes a second link type, `both` or the other direction, a node type filter); `as_of` in a quarter of the cases | node keys, link count |
| `neighbors_2` | the same, 2 hops | node keys |
| `neighbors_steps` | 2–3 different `steps` that follow the schema from type to type, sometimes `as_of`; Neo4j expands hop by hop from the nodes first reached at the previous hop ([OP-NEIGH-2]) | node keys, link count |
| `paths_shortest` | a pair where the second node is mostly reached by a random walk, one of seven link-type sets, `direction` `both` / `out` / `in`, sometimes `as_of`, `max_length` 4 | length, and that the path is one of Neo4j's shortest paths |
| `paths_all_shortest` | the same | every path (keys and link types), or length and count when there are more than 100 |
| `paths_k_shortest` | the same with `k` 1–10, `max_length` 3 | every path of each length before the last one returned, then the count of the last length and that each is one of Neo4j's |
| `find_count` | type plus one condition on a property, `name` or `labels`, operand taken from a real node: every operator on strings, ints, dates and date-times (as partial dates), lists | count |
| `find_degree` | `degree(T, dir)` with `gt` / `eq` / `lte` | count, `max(degree)` |
| `find_group` | 9 fixed group-bys (`prop:…`, `.year`, `.month`, `.decade`, `labels`) with count, count of, sum, avg, median, min, count_distinct | every group |
| `find_group_order` | 5 group-bys (one with missing keys, one by `degree`) × 4 orders (default, key ascending / descending, count ascending), read in pages of 3, 25 or 1000 | the groups in order |
| `find_links` | links by type and endpoint types; date and int filters on link properties | groups, counts |
| `find_linked` | persons with a `linked` condition | node keys |
| `find_text` | `contains` / `starts_with` on a name, string or list property, sometimes under `not`; the operand cut from a real value, sometimes upper- or lower-cased | node keys, all pages |
| `find_sort` | rows sorted by one or two of 17 keys (names, dates, strings, degrees, properties some posts lack) in either order, sometimes with a condition, limit 7–1000, up to 4 pages | the total, no row twice, and the sequence of sort values against `ORDER BY` with missing values last |

The Cypher side of `paths` is a quantified path pattern whose inner `WHERE` applies [TRAV-1] per
step (direction, and validity for `as_of`). Neo4j's `ALL SHORTEST` returns shortest trails, which
never repeat a node, so they are [TRAV-4] paths; for `k_shortest` Neo4j lists the paths of each
length filtered to distinct nodes. Which of several equally long paths comes first, and which rows
tie in a sort, depend on internal numbers ([TRAV-4], [OP-FIND-7]), so those are compared as sets
or by their sort values. Cypher has no name key ([NORM-NAME-1] folds case and removes accents),
so for `find_text` the expected nodes are worked out from Neo4j's values with the spec's name key.

A case is skipped when MyGraph's result is truncated (more than 1000 links per node or 5000
nodes, or the `k_shortest` work budget), or when Neo4j hits its 30 s transaction timeout
(`db.transaction.timeout=30s`).

**SF0.1, seeds 1–4, 700 random cases each** (the group-by cases are the same in every run; about
40 s per run):

| Family | Pass | Fail | Skipped |
|---|---|---|---|
| `neighbors_1` | 557 | 0 | 3 |
| `neighbors_2` | 278 | 0 | 2 |
| `neighbors_steps` | 259 | 0 | 21 |
| `paths_shortest` | 108 | 0 | 0 |
| `paths_all_shortest` | 98 | 0 | 0 |
| `paths_k_shortest` | 74 | 0 | 0 |
| `find_count` | 560 | 0 | 0 |
| `find_degree` | 280 | 0 | 0 |
| `find_group` | 36 | 0 | 0 |
| `find_group_order` | 80 | 0 | 0 |
| `find_links` | 32 | 0 | 0 |
| `find_linked` | 280 | 0 | 0 |
| `find_text` | 280 | 0 | 0 |
| `find_sort` | 280 | 0 | 0 |

**Are the cases worth anything?** `--verbose` prints every compared result. In these runs 201 of
280 `paths` cases have a path (lengths 1–4, all three directions, 77 with `as_of`); two fifths of
the `neighbors` cases reach only the start node (a link the node happens not to have, the other
direction, or an `as_of` before the link); the `find_text` results have a median of 218 nodes;
223 of 280 `find_sort` cases read more than one page. The first version of the generator picked
link types at random, and two thirds of its `neighbors` cases and four fifths of its `paths` cases
were empty, which is why steps now follow the schema.

Each comparison was also broken on purpose, one at a time, on a 700-case run:

| Broken on the Cypher side | Failures |
|---|---|
| `as_of` uses the start of the period instead of its end | 11 (`neighbors_*`, `paths_*`) |
| `out` follows links into the node | 10 (`paths_*`) |
| missing sort values first in descending order | 4 (`find_sort`) |
| plain lower-casing instead of the name key | 15 (`find_text`) |
| later hops expand every node so far, not the new ones | 18 (`neighbors_steps`) |
| default group order with the key descending | 1 (`find_group_order`) |
| `k_shortest` paths may repeat a node | 0 |

The last one is not caught: with these link sets a path of length ≤ 3 that repeats a node needs
two links between the same pair (a person liking their own post), which the cases did not meet.

**Findings, fixed on 2026-10-29:**

1. **`find` without `group_by` over no matching objects returned no group** when an aggregate
   other than `count` was asked for. Fixed in the code; [OP-FIND-5] now says outright that there
   is always one group (count 0, other aggregates null), as SQL and Cypher do.
2. **`ne` and `not_in` on a list property**: the code treated them as the negations of `eq` /
   `in` (no element matches), while [OP-FIND-4] said a comparison on a list holds for any element,
   which would make `speaks ne "en"` true for `["en", "si"]`. The spec now says `ne` / `not_in`
   are always the negations, which is what the code did.
3. **Names were rewritten silently**: `normalize_types` turned `locationIP` into `location_ip`, so
   agents saw other names than they wrote. Replaced by the policy `naming` ([VOCAB-1]): `styled`
   (the default) refuses a name not in its style with `vocab.not_styled` and the expected spelling
   in `details.expected`; `free` stores names as given. Names are never rewritten, and reads check
   names the same way, so a misspelled type fails instead of matching nothing. Existing graphs
   read `normalize_types: true` as `styled`.

No differences were found by the families added on 2026-10-29 (`neighbors_steps`, the three
`paths` families with direction and `as_of`, `find_text`, `find_sort`, `find_group_order`).

Not covered yet: SF1; the order of `neighbors` nodes and of equally long paths (internal numbers);
`include_retracted`; `find` over evidence and citations, `fields` beyond sort keys, `bucket()` and
`.century`; `search`, `resolve` and `history`, which have no Cypher counterpart.

### Workload driver

`mygraph-ldbc bench` runs LDBC SNB Interactive v1's short reads IS1–IS7, an IC1-style read, IC13
and the inserts of the update streams against MyGraph and Neo4j with the same parameters, times
each query on both and compares the two answers, so both did the same work. It exits with status
1 when any answer differs or fails.

```bash
mygraph-ldbc bench --data <data dir> --graph ldbc-sf01-none --runs 200 --warmup 20
mygraph-ldbc bench --mygraph-url http://127.0.0.1:7499 --graph ldbc-sf01-none
mygraph-ldbc bench --data <data dir> --graph ldbc-sf01-none --queries "" --updates 20000 --archive <archive>
```

MyGraph answers through its operations, as an agent would, so a query may take several calls;
the driver counts them and times each operation. It runs in the driver's process or over the REST
API of a running server (`--mygraph-url`). Neo4j answers one Cypher statement per query, with
parameters, over its HTTP API. Parameters (persons, messages, first names) are drawn from Neo4j
with a seed, and the order of the two sides alternates between runs.

| Query | What it returns | MyGraph calls |
|---|---|---|
| IS1 | a person's profile and city | `get` with `IS_LOCATED_IN` links |
| IS2 | a person's 10 latest messages, each with its thread's post and that post's author | `find` (type, `linked` creator, sorted by date, limit 10); `neighbors` up `REPLY_OF` to the posts; `get` the posts with `HAS_CREATOR` |
| IS3 | a person's friends with the friendship's date | `get` with `KNOWS` links; `get` those links for `creationDate` |
| IS4 | a message's date and content | `get` |
| IS5 | a message's author | `get` with `HAS_CREATOR` links |
| IS6 | the forum holding a message's thread and its moderator | `neighbors` up `REPLY_OF`; `neighbors` with steps `CONTAINER_OF` in, `HAS_MODERATOR` out |
| IS7 | replies to a message, their authors, and whether each knows the message's author | `get` with `REPLY_OF` / `HAS_CREATOR` links; `get` the replies; `get` the author's `KNOWS` |
| IC1-style | up to 20 persons with a given first name within 3 `KNOWS` hops, by distance and last name | `find` by `firstName`; `neighbors` 3 hops |
| IC13 | length of the shortest `KNOWS` path (at most 10) | `paths` |
| INS1–INS8 | add a person, a like of a post / comment, a forum, a membership, a post, a comment, a friendship | one `write` |

Names stand in for LDBC ids where a MyGraph brief result has no key (posts and comments are named
`Post <id>` / `Comment <id>`, persons `firstName lastName`). Rows that tie on the sort key are
compared as sets. IC1 compares (distance, last name) pairs, as the tie-break on the LDBC id is not
visible in MyGraph's rows. An insert is one MyGraph `write` holding the node and its links, ends
by key, and on Neo4j one transaction of `MERGE` statements, so both are idempotent; the driver
checks that both created the same number of nodes and links. Inserts change both databases;
reload MyGraph and re-import Neo4j to start again.

**SF0.1, seed 1, 20 warmup and 200 measured runs per query, milliseconds** (2026-10-30, the
machine above, Neo4j 2026.10.1 with heap 4 GB and page cache 4 GB). Neo4j's floor, `RETURN 1` over
HTTP, is 0.39 ms at p50.

| Query | MyGraph calls | MyGraph in-process p50 / p95 / p99 | MyGraph REST p50 / p95 / p99 | Neo4j p50 / p95 / p99 |
|---|---|---|---|---|
| IS1 | 1 | 0.07 / 0.19 / 0.25 | 0.26 / 0.43 / 0.51 | 0.42 / 0.53 / 0.60 |
| IS2 | 2.9 | 32.4 / 35.3 / 36.7 | 35.6 / 40.6 / 42.0 | 1.35 / 1.83 / 2.04 |
| IS3 | 1.9 | 0.22 / 1.45 / 2.59 | 0.65 / 2.74 / 4.63 | 0.66 / 1.29 / 1.94 |
| IS4 | 1 | 0.02 / 0.03 / 0.05 | 0.19 / 0.24 / 0.25 | 0.44 / 0.62 / 0.74 |
| IS5 | 1 | 0.03 / 0.05 / 0.06 | 0.17 / 0.23 / 0.24 | 0.45 / 0.61 / 0.91 |
| IS6 | 2 | 0.04 / 0.07 / 0.08 | 0.29 / 0.38 / 0.41 | 0.51 / 0.64 / 0.71 |
| IS7 | 1.3 | 0.04 / 0.55 / 0.95 | 0.19 / 1.12 / 1.78 | 0.41 / 0.76 / 0.90 |
| IC1-style | 2 | 56.8 / 85.0 / 126.3 | 104.6 / 140.3 / 153.6 | 1.82 / 2.46 / 4.19 |
| IC13 | 1 | 0.41 / 3.29 / 4.15 | 0.58 / 2.87 / 4.15 | 0.75 / 1.53 / 5.94 |

Every answer agreed with Neo4j's in both runs (1 800 queries each), with no errors. The Neo4j
column is from the in-process run; the REST run's Neo4j numbers are within 0.2 ms of it.

**Inserts: the first 20 000 events of the update streams**, in scheduled order, applied in-process
(the first 200 had been applied in a trial and were no-ops on both sides). Both created 7 908 nodes
and 43 698 links, with no errors, and the comparison harness afterwards (seed 1, 700 cases) found
no difference.

| Operation | Events | MyGraph p50 / p95 / p99 | Neo4j p50 / p95 / p99 |
|---|---|---|---|
| INS1 person | 14 | 3.84 / 27.9 / 27.9 | 6.54 / 62.9 / 62.9 |
| INS2 like post | 2 640 | 0.74 / 1.32 / 10.2 | 1.12 / 2.33 / 3.75 |
| INS3 like comment | 2 204 | 0.74 / 1.32 / 10.1 | 1.11 / 2.31 / 3.47 |
| INS4 forum | 268 | 1.23 / 8.95 / 19.9 | 1.56 / 4.39 / 11.7 |
| INS5 membership | 6 824 | 0.75 / 1.29 / 9.98 | 1.12 / 2.39 / 3.45 |
| INS6 post | 2 905 | 1.30 / 7.65 / 16.1 | 1.74 / 3.81 / 4.86 |
| INS7 comment | 4 795 | 1.39 / 9.56 / 18.1 | 1.82 / 3.53 / 4.66 |
| INS8 friendship | 350 | 0.75 / 1.42 / 11.3 | 1.14 / 2.51 / 9.65 |

**Findings:**

1. **Point reads and inserts are on par with Neo4j or faster**, even over REST: IS1 and IS4–IS7
   take 0.2–0.3 ms at p50 over HTTP against Neo4j's 0.4–0.5 ms, and 0.02–0.07 ms in-process.
   Inserts are faster at p50, but MyGraph's p99 is about 10 ms on every insert type where Neo4j's
   is 3–5 ms: a periodic stall in the write path (a SQLite checkpoint is the first suspect).
2. **`find` with a `linked` condition scans every candidate of the type** (IS2: 32 ms, all in
   `find`): it checks the links of all 286 k posts and comments instead of starting from the
   linked node's own links, which lead to the few hundred candidates.
3. **A 3-hop `neighbors` costs 50 ms in-process and 87 ms over REST** (IC1): it examines every link
   of every person it reaches (their likes and messages too, not only `KNOWS`), testing each
   link's type against a set of strings, and the result carries every `KNOWS` link among about
   1 500 persons, which IC1 does not need and REST has to serialize.
4. **Each extra call is a round trip over REST**: IS3 for a person with many friends takes a `get`
   per 100 links (p99 4.6 ms against Neo4j's 1.9 ms).

### Then

1. Fix findings 2–4 and the insert p99 stall, then rerun the driver.
2. The same runs on SF1.
