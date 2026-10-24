# 00 — Requirements

Status: **Draft for review** · 2026-10-24

This file records what was asked for (R-items, in the original words where possible) and the
requirements derived from them (N-items). Every design document refers back to these IDs.

## Stated requirements

| ID  | Requirement |
|-----|-------------|
| R1  | Property-graph structure similar to Neo4j: nodes (entities) and links (relations), both carrying properties. Links have a direction, and a link can be bidirectional ("both sides"). |
| R2  | Every node and link references the data it was built from (its evidence). |
| R3  | Evidence is stored: text, a reference (URL), a picture, anything. External sources may be referenced only; short text is stored locally. A fact can have several pieces of evidence (e.g. a link to an article **and** a text fragment of it). Evidence has attributes such as date and version. |
| R4  | API access only. The primary user is an AI agent (e.g. Claude) that keeps its knowledge there and uses it. An agent-oriented API is a must. |
| R5  | A second API for admin pages, management, statistics and maintenance. |
| R6  | Existing storage implementations may be reused (SQLite, Redis, relational DBs, …). |
| R7  | Compiled, fast language preferred; compare Rust, Go and C#, with emphasis on libraries. |
| R8  | Research first, check existing solutions, make a plan, keep all material. No code until approved. |
| R9  | No query language. The API itself provides everything the agent needs, including aggregations and calculations. |
| R10 | Focus on speed, especially search speed. |
| R11 | Advanced features and big parts are planned for later; a proof of concept comes first. |
| R12 | Implement in **Rust**, but keep everything documented so the system can easily be re-written in another language. *(added 2026-10-24)* |

## Derived requirements

| ID  | Requirement | Derived from |
|-----|-------------|--------------|
| N1  | **Token economy.** Responses are compact, paginated and field-selectable; the MCP tool set stays small (target ≤ 12 tools) because every tool definition costs context. | R4 |
| N2  | **Round trips matter more than microseconds.** An agent's wall-clock cost is dominated by the number of tool calls. One call must be able to write a whole batch (evidence + nodes + links + citations), and one read must be able to return a node with its neighbourhood and evidence. | R4, R10 |
| N3  | **Idempotent writes.** Agents retry and re-read; upserts by natural key must not create duplicates. | R4 |
| N4  | **Entity resolution support.** Before writing, the agent can ask "does something like this already exist?" and get ranked candidates. Duplicates can be merged later. | R4, R10 |
| N5  | **Mistakes are recoverable.** Deletes are soft by default (retract), every change is in an append-only change log, and history is queryable. Hard purge is admin-only. | R4, R5 |
| N6  | **Vocabulary discovery.** The agent can list the node types, link types and property keys in use (with counts), so it reuses `WORKS_AT` instead of inventing `EMPLOYED_BY`. | R4, R9 |
| N7  | **Helpful errors.** Errors say what to do next (e.g. ambiguous name → list of candidates with IDs). | R4 |
| N8  | **Single binary.** Runs locally (MCP over stdio, launched by Claude Code / Claude Desktop) and as a network server (MCP over Streamable HTTP + REST). | R4, R7 |
| N9  | **Durability.** Crash-safe writes; backup is "copy a snapshot". | R6 |
| N10 | **PoC scale.** Comfortable on a laptop at 1 M nodes / 5 M links / 1 M evidence items. The design must not block 100 M+ later. | R10, R11 |
| N11 | **Attribution.** Every change records which actor (agent, client, user) made it. | R2, R5 |
| N12 | **Security baseline.** Local-only by default; HTTP requires an API key. Fine-grained permissions come later. | R4, R5 |
| N13 | **Observability.** Request metrics and slow-operation logging from day one, so speed claims are measured, not assumed. | R10 |
| N14 | **Language-neutral contract.** API schemas, storage format, algorithms and conformance tests live outside the code, in plain files any language can use; nothing durable is stored in a Rust-specific format. See [10-portability-and-spec.md](10-portability-and-spec.md). | R12 |

## Glossary

| Term | Meaning |
|------|---------|
| **Graph** | An isolated workspace (one knowledge base). A server can host several. |
| **Node** | An entity: a person, place, concept, document, event… Has a type, a name, aliases, properties. |
| **Link** | A relation between two nodes. Has a type, properties, and is either *directed* or *undirected* (symmetric, "both sides"). |
| **Evidence** | A piece of source material: inline text, a URL, a file or image in the blob store, an agent observation. Has dates, version, author, etc. |
| **Citation** | The connection between a fact (node, link, or one property of either) and a piece of evidence, with a role (supports / contradicts / mentions), a confidence and an optional quote. |
| **Fact** | Collective name for nodes, links and their properties — anything that can be cited. |
| **Actor** | Whoever makes a change: an agent session, a script, an admin. |
| **Key** | Optional natural identifier chosen by the client (e.g. `person/ada-lovelace`), unique within a graph, used for idempotent upserts. |
