# 01 — Existing systems: what is out there and what to borrow

Status: **Draft for review** · researched 2026-10-24 · sources in [research/sources.md](research/sources.md)

Question: is there already a fast, compiled graph store with first-class, multi-source evidence
and an agent-first API (no query language)? Short answer: **no**. Pieces exist in four families,
and each family gives us ideas worth stealing.

Claims marked *(verify)* come from secondary sources or memory and should be checked before we
rely on them.

---

## 1. General-purpose graph databases

| System | Lang | Model / query | License | Status (Oct 2026) | Relevance |
|---|---|---|---|---|---|
| **Neo4j** | Java | Property graph, Cypher | Community GPLv3 / Enterprise commercial | Mature, market leader | The model R1 refers to. Heavy JVM server; provenance has to be modelled by hand (evidence nodes + relationships). |
| **Memgraph** | C++ | Property graph, in-memory, Cypher | BSL 1.1 *(verify)* | Active | Shows the speed of an all-in-RAM graph with on-disk durability — the same approach we propose. |
| **FalkorDB** | C (Rust rewrite reported in 2026, *verify*) | Property graph on GraphBLAS sparse matrices, Cypher, Redis module | SSPLv1 (not OSI) | Active, GraphRAG-focused | Sparse-matrix traversal is very fast; licence and Redis coupling rule it out as a base. |
| **Kuzu** | C++ | Embedded columnar property graph, Cypher | MIT | **Archived Oct 2025**, last release 0.11.3; community fork RyuGraph | Was the best embedded graph DB. A warning about depending on a small vendor's engine. |
| **ArangoDB** | C++ | Multi-model, AQL | BSL 1.1 since 3.12 *(verify)* | Active | Not a fit (licence, query language). |
| **Dgraph** | Go | Graph, DQL/GraphQL | Apache 2.0 | Changed owners twice (Hypermode 2023, Istari Digital 2025, *verify*) | Proof Go can do it; ownership churn is a risk signal. |
| **NebulaGraph, JanusGraph, TigerGraph** | C++/Java | Distributed graph | Mixed | Active | Built for distributed scale; far beyond PoC needs. |
| **Apache AGE** | C | openCypher inside PostgreSQL | Apache 2.0 | Active | Interesting future "Postgres backend" option. |

## 2. Rust-native graph / multi-model engines (closest technically)

| System | What it is | License | Status | Take-away |
|---|---|---|---|---|
| **HelixDB** | Graph + vector + BM25 in one embedded store; LMDB via `heed`; compiled query DSL (HelixQL) | AGPL-3.0 | Active, declared GA in 2026 | Proves the "graph + vector + text in one Rust process on LMDB" design. Marketing benchmarks (1000× Neo4j) are unsourced. AGPL and a query-language-first API make it a reference, not a base. |
| **SurrealDB** | Multi-model (documents, graph relations, vectors, FTS), SurrealQL; embeddable | BSL 1.1 *(verify current)* | Active | Everything in one, but SurrealQL in the middle, vendor-controlled licence, graph traversal not its strongest part. |
| **TypeDB 3** | Strongly typed knowledge DB, rewritten from Java to Rust (3.0, Dec 2024), TypeQL | MPL-2.0 | Active | **n-ary relations** and a type system with inheritance. Worth borrowing for the schema registry later. |
| **CozoDB** | Embedded relational-graph DB with Datalog, graph algorithms | MPL-2.0 | Inactive since 2024 | Good ideas (recursive aggregation, built-in PageRank), dead project. |
| **IndraDB** | TAO-inspired graph library + server | MPL-2.0 | Small, low activity | Simple adjacency design, nothing beyond that. |
| **Oxigraph** | RDF/SPARQL store on RocksDB | MIT/Apache | Active | Reference for quad indexing (SPO/POS/OSP) if we ever support RDF export. |
| **TerminusDB** | Rust + Prolog, git-like versioned graph | Apache 2.0 | Active | Branch/commit history for data: idea for a later "versioned graph" feature. |

## 3. Agent-memory systems (closest conceptually)

| System | Model | Provenance | Notes |
|---|---|---|---|
| **Graphiti / Zep** (Python, Apache 2.0) | Temporal knowledge graph: **episodes** (raw text/JSON = ground truth) → entities (with evolving summary) → fact edges with validity windows (`valid_at`, `invalid_at`) | Every fact edge lists the episodes it came from; new evidence can invalidate contradicting facts | **Closest to our idea.** But: a Python library on top of an external graph DB (Neo4j, FalkorDB, Kuzu, Neptune); ingestion calls an LLM for extraction; one evidence type (episode); no fragments, no evidence versions, no blob store. |
| **MCP reference "memory" server** (TypeScript, MIT) | Entities (name, type, list of string observations), relations (from, to, type), stored as a JSONL file | None | The de-facto baseline agents already know. Its tool set (`create_entities`, `create_relations`, `add_observations`, `search_nodes`, `open_nodes`, `read_graph`, deletes) is a useful starting vocabulary, but it has no properties, no evidence, linear-scan search. |
| **Memento MCP** (Neo4j) | Entities/relations + embeddings in Neo4j; relation confidence with time decay; versioned updates | Free-form metadata field on relations (e.g. source URL) | Shows demand for confidence + temporal + provenance; provenance is unstructured. |
| **Mem0** | Vector store + optional graph (graph in paid tier) | Weak | Personalisation memory, not a knowledge graph. |
| **Cognee**, **LightRAG**, **Microsoft GraphRAG** | LLM-built knowledge graphs for retrieval | Chunks/documents linked to entities | Pipelines, not databases. Their "chunk → entity" link is exactly our citation. |

Key observation: these systems put an LLM **inside** the memory layer to extract entities.
In our design the agent *is* the LLM: it reads, decides what the facts are, and writes them
with their evidence. The database stays deterministic, fast and cheap. (An optional extraction
helper can come later — roadmap.)

## 4. Provenance models and standards

| Model | Idea | What we borrow |
|---|---|---|
| **Wikidata statement model** | Each statement has *qualifiers* (properties on the edge), *references* (each reference is a set of claims such as "stated in", "reference URL", "retrieved on"), and a *rank* (preferred / normal / deprecated) | The closest proven match to R2/R3: many references per fact, each with its own dates. Rank ≈ our citation role and fact status. |
| **W3C PROV-O** | Entity / Activity / Agent with `wasDerivedFrom`, `wasGeneratedBy`, `wasAttributedTo` | Our Evidence ≈ prov:Entity, the agent session ≈ prov:Agent, a write batch ≈ prov:Activity. We can export to PROV-O later. |
| **Nanopublications** | Assertion graph + provenance graph + publication-info graph; 2025 work adds "knowledge provenance" for multi-source claims that support **or conflict** | Separate "where the claim comes from" (citation) from "who recorded it and when" (change log). Model *contradicting* sources explicitly. |
| **RDF reification / RDF 1.2 (RDF-star) / named graphs** | Ways to make statements about statements | Confirms the property-graph choice: in an LPG, a link is already an addressable object, so citing it is natural. Multiple sources on the same fact need a separate citation object, not just link properties. |

---

## 5. Gap analysis

| Need | Neo4j-class DBs | HelixDB / SurrealDB | Graphiti / Zep | MCP memory server | **This project** |
|---|---|---|---|---|---|
| Property graph, properties on links (R1) | ✅ | ✅ | partial | ❌ | ✅ |
| Undirected / bidirectional links (R1) | via query | via query | ❌ | ❌ | ✅ first-class |
| Multi-evidence per fact, typed evidence store with versions, fragments, blobs (R2, R3) | manual modelling | manual modelling | single episode type | ❌ | ✅ core feature |
| Property-level provenance | ❌ | ❌ | ❌ | ❌ | ✅ (optional) |
| Agent-first API, no query language (R4, R9) | ❌ Cypher | ❌ DSL | Python API | ✅ (very basic) | ✅ |
| Aggregations without a query language (R9) | via Cypher | via DSL | ❌ | ❌ | ✅ |
| Compiled single binary, embedded storage (R7) | ❌ JVM server | ✅ | ❌ Python + external DB | ❌ Node.js | ✅ |
| Search speed as the primary goal (R10) | good | good | depends on backend | linear scan | ✅ in-RAM indexes |
| Permissive licence for our own use | GPL/commercial | AGPL/BSL | Apache | MIT | ours |

## 6. Build vs reuse

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **A. Agent API layer over Neo4j** | Mature engine; fastest path to a demo | JVM server to run; provenance becomes extra nodes/edges and Cypher we must generate; each agent call becomes several DB round trips; licence | Rejected for core; possible future backend |
| **B. Agent API over SurrealDB or HelixDB (embedded)** | Rust, one process, FTS + vectors included | We would still build the whole provenance model and agent API on top; vendor DSL and licence (BSL/AGPL) in the middle; engine direction not ours | Rejected; keep as references |
| **C. Own core on commodity storage** (SQLite durable store + in-memory graph indexes + proven search libraries) | The valuable parts (provenance model, agent API, speed) are exactly the parts we must build anyway; storage underneath is commodity and swappable; full control over performance | More work than A/B for the first demo | **Recommended** |

Option C keeps a `Store` trait so a Postgres/AGE or Neo4j backend could be added later without
touching the API.

## 7. Ideas adopted from the research

1. **Episodes → Evidence** (Graphiti): raw source material is the ground truth; facts point to it.
2. **References with their own dates + rank** (Wikidata): many citations per fact; status preferred/normal/deprecated.
3. **Support *and* conflict** (nanopublication knowledge provenance): citation role `contradicts`.
4. **Validity windows** (Graphiti, Memento): optional `valid_from` / `valid_to` on links.
5. **Tiny, familiar tool vocabulary** (MCP memory server): agents already know `search_nodes` / `open_nodes`-style tools.
6. **All-in-RAM graph + durable log** (Memgraph): reads never touch disk for topology.
7. **One process for graph + text + vector** (HelixDB): no network hops between engines.
8. **n-ary relations and typed schema** (TypeDB): roadmap item for the schema registry.
9. **Commit history of data** (TerminusDB): roadmap item for "as of" queries and undo.
