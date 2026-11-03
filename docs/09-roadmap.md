# 09 — Roadmap (after the PoC)

Status: **Draft for review** · 2026-10-24 · covers R11

Order within a phase is a suggestion; the PoC report and real usage will re-rank it.

## Phase 2 — Useful every day

| Area | Items |
|---|---|
| **Semantic search** | Local embeddings (ONNX via `ort`/`fastembed`, small multilingual model), HNSW index (`usearch`), hybrid ranking with BM25 (reciprocal rank fusion); `resolve` and `search` use meaning, not just spelling. **Done 2026-11-02** (`mygraph-semantic`, multilingual-e5-small; `resolve` lists nodes close in meaning as `similar` without changing its verdict) |
| **Search engine upgrades** | tantivy and/or fst if the M4 benchmark gate asks for them |
| **Schema registry** | Optional strict types (required props, property kinds, allowed link endpoints), type inheritance, inverse link names (`PARENT_OF` ↔ `CHILD_OF`) |
| **Truth handling** | Weighted support score (confidence × source reliability × recency); multi-valued properties with per-value citations and ranks (Wikidata-style preferred / normal / deprecated); "disputed" status and a review list |
| **Time** | Bi-temporal queries ("what did the graph say on date X") from the change log; validity on node properties |
| **Evidence** | URL snapshots into the blob store; link-rot checks; text extraction from HTML/PDF into fragment evidence; OCR for images |
| **Access** | Per-graph permissions; OAuth for remote MCP; read-only agent keys; per-actor views |
| **Operations** | Fast startup from binary index snapshots; snapshot (MVCC) reads; group commit; purge; undo of a batch; vocabulary rename |
| **Feedback** | Reports about MyGraph itself (bugs, wrong hints, slow operations, missing features) from agents and users go to one central place: the project's GitHub repository (Issues, with an issue form and labels by category), not a store in each server. Likely shape: an MCP `feedback` tool drafts the report — server and spec version, operation, error code, request shape without values, latency — and returns a prefilled "new issue" link for the user to review and submit (no token on any server); `gh issue create` where the GitHub CLI is set up; the agent searches existing issues first to avoid duplicates. Graph content (names, evidence text) never goes into a report without the user's approval. Open: whether a hosted relay with a bot token should file reports automatically |
| **Admin UI v1** | Dashboard, evidence viewer, duplicate review, vocabulary manager, editing in the graph explorer (the stats page and a read-only graph viewer come earlier, in PoC M5) |
| **Interop** | Import: MCP memory-server JSONL, Neo4j CSV, generic CSV. Export: GraphML, RDF-star, W3C PROV-O |
| **Packaging and setup** | In this order (2026-10-28): **1. Docker and compose** (built 2026-10-29: `Dockerfile`, `compose.yaml`, `.github/workflows/docker.yml`) — multi-stage Dockerfile on a small runtime image, multi-arch image (amd64, arm64) on GitHub's container registry, `compose.yaml` with a data volume, health check and an admin API key (a container listens beyond loopback, which the server only allows with keys), for running MyGraph as a service on a home server or NAS. **2. Release executables** built by GitHub Actions on a version tag (Windows x64, macOS arm64 / x64, Linux x64 / arm64) with checksums, likely through cargo-dist (one-line shell / PowerShell installers, MSI, Homebrew). **3. `mygraph setup` and `mygraph doctor`** — data folder in the OS's standard place and a first graph, autostart (Windows scheduled task, macOS launchd agent, Linux systemd user service), detect Claude Desktop, Claude Code, Cursor, VS Code and Codex and connect each (MCP config + skill), open the stats page; `doctor` checks it all. **4. Claude Desktop extension** (`.mcpb`, one-click): the binary plus a manifest; possibly local mode without a separate server that also serves the stats page and viewer while Claude runs. **5. Claude Code plugin** bundling the skill and the MCP registration. **6. Code signing** (Windows certificate, Apple notarization) once there are users. Smaller: zero-config defaults, a "Getting started" panel on the stats page, `mygraph update` or winget / Homebrew packages |

## Phase 3 — Scale and depth

| Area | Items |
|---|---|
| **Graph analytics** | `analyze` tool: PageRank, betweenness, communities (Louvain/Leiden), connected components, node similarity, link-prediction suggestions |
| **Storage engine** | Graphs larger than RAM: on-disk CSR adjacency, memory-mapped indexes, KV engine (redb / LMDB) or a purpose-built engine; columnar properties (Arrow) for analytics |
| **Versioned graphs** | Branches and commits of the data (TerminusDB idea): an agent works on a branch, a human reviews the diff and merges |
| **Distribution** | Change-log shipping replication, read replicas; PostgreSQL (+ Apache AGE) backend for hosted multi-tenant use; S3-compatible blob store |
| **Eventing** | Change streams, webhooks, subscriptions ("tell me when anything about X changes") |
| **Richer modelling** | n-ary relations / hyperedges (until then: model events as nodes — `Meeting` linked to its participants, place and date); quantities with units; geo values and spatial search |
| **Ingestion helper** | Optional LLM-based extraction pipeline for bulk documents (off the hot path; the agent remains the primary writer) |
| **Embedding** | Use the engine as a library from Python / Node (FFI bindings) |
| **Federation** | Cross-graph links and queries across graphs |

## Explicitly not planned

- A general-purpose query language (R9). New needs become new parameters, operators or tools.
- Distributed sharding — only if real usage ever demands it.
