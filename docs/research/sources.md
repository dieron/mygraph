# Research sources

All accessed 2026-10-24. Many are secondary (directories, blogs, package indexes); claims taken
from them are marked *(verify)* in the design docs where they matter. Statements not listed here
come from prior knowledge and should be re-checked before they drive a decision.

## Graph databases

| Topic | Source |
|---|---|
| Kuzu archived (Oct 2025) | [The Register — KuzuDB abandoned](https://www.theregister.com/2025/10/14/kuzudb_abandoned/) |
| Kuzu final release / no further releases | [PyPI — kuzu](https://pypi.org/project/kuzu/) |
| RyuGraph fork | [Predictable Labs — Introducing RyuGraph](https://blog.ryugraph.io/posts/introducing-predictable-labs-and-ryugraph) |
| Kuzu overview, acquisition claim (unconfirmed) | [PuppyGraph — What is KuzuDB](https://www.puppygraph.com/blog/what-is-kuzudb) |
| GitLab replaced Kuzu | [GitLab handbook commit](https://gitlab.com/gitlab-com/content-sites/handbook/commit/d8a6afc1) |
| HelixDB GA, overview | [feedbagel — HelixDB GA](https://feedbagel.com/post/helixdb-open-source-oltp-graph-vector-database-built-in-rust-now-generally-avail), [rustutils — helix-db](https://rustutils.com/tools/helix-db/), [aiindigo — HelixDB deep dive](https://aiindigo.com/blog/helixdb-deep-dive-technical-review) |
| HelixDB architecture (LMDB/heed), licence (AGPL) | [GitHub mirror — helix-db](https://github.com/meta-introspector/helix-db), [DBDB.io — HelixDB](https://dbdb.io/db/helixdb/revisions/2), [Gitee mirror](https://gitee.com/mirrors/HelixDB) |
| HelixDB conflicting benchmark claims (not relied on) | [dailyaiworld — HelixDB deep dive](https://dailyaiworld.com/blogs/helixdb-deep-dive-open-source-vector-graph-hybrid-database) |
| IndraDB | [docs.rs — indradb](https://docs.rs/indradb) |
| CozoDB | [rustfinity — cozo](https://rustfinity.com/open-source/cozo), [DBDB.io — CozoDB](https://dbdb.io/db/cozodb), [Snyk — cozo-embedded](https://snyk.io/advisor/python/cozo-embedded) |
| SurrealDB features, embedded mode | [SurrealDB docs](https://surrealdb.com/docs/start), [SurrealDB llms.txt](https://surrealdb.com/llms.txt), [DBDB.io — SurrealDB](https://dbdb.io/db/surrealdb/revisions/22) |
| TypeDB 3.0 Rust rewrite, MPL-2.0 | [Wikipedia — TypeDB](https://en.wikipedia.org/wiki/TypeDB), [TypeDB install docs](https://typedb.com/docs/home/install/overview) |
| FalkorDB licence (SSPLv1), GraphBLAS | [FalkorDB — third-party components](https://docs.falkordb.com/design/third-party.html), [Atlan — FalkorDB for GraphRAG](https://atlan.com/know/ai-agent/knowledge-graph/falkordb-graph-database-for-graphrag/) |
| Memgraph licensing (MemGQL only) | [Memgraph — licensing](https://memgraph.com/docs/preview/memgql/licensing), [openalternative — FalkorDB vs Memgraph](https://openalternative.co/compare/falkordb/vs/memgraph) |

## Agent memory systems

| Topic | Source |
|---|---|
| Graphiti model (episodes, validity windows, provenance) | [Graphiti README](https://cdn.jsdelivr.net/gh/getzep/graphiti@main/README.md), [PyPI — graphiti-core](https://pypi.org/project/graphiti-core/0.29.3/), [D. Vaughan — Graphiti for agent memory](https://codex.danielvaughan.com/2026/03/30/graphiti-agent-memory-store/) |
| Zep paper | arXiv 2501.13956 — "Zep: A Temporal Knowledge Graph Architecture for Agent Memory" |
| MCP reference memory server and forks | [playbooks — Knowledge Graph Memory](https://playbooks.com/mcp/modelcontextprotocol-knowledge-graph-memory), [mcp-memory-py](https://github.com/edobez/mcp-memory-py), [s2005/mcp-memory-server](https://github.com/s2005/mcp-memory-server), [glama — dustinspace217/mcp-memory-server](https://glama.ai/mcp/servers/dustinspace217/mcp-memory-server), [sourcepulse — mcp-knowledge-graph](https://www.sourcepulse.org/projects/1868383) |
| Memento MCP (Neo4j, confidence decay, temporal) | [Memento MCP README](https://cdn.jsdelivr.net/gh/gannonh/memento-mcp@main/README.md), [mcpservers.org — memento-mcp](https://mcpservers.org/servers/gannonh/memento-mcp) |
| Mem0 vs Cognee vs Zep comparisons (vendor-authored) | [vectorize.io — Mem0 vs Cognee](https://vectorize.io/articles/mem0-vs-cognee), [cognee — vs mem0](https://www.cognee.ai/cognee-vs-mem0), [mcp.directory — Mem0 vs Letta vs Zep vs Cognee](https://mcp.directory/blog/mem0-vs-letta-vs-zep-vs-cognee-2026), [dasroot — Cognee vs Mem0](https://dasroot.net/posts/2025/12/cognee-vs-mem0-memory-layer-comparison-llm-agents/) |

## Provenance models

| Topic | Source |
|---|---|
| Nanopublications | [arXiv 1809.06532](https://arxiv.org/pdf/1809.06532), [arXiv 2006.06348](https://arxiv.org/pdf/2006.06348), [Dublin Core — nanopublication](https://msi.dublincore.org/standards/nanopublication) |
| Knowledge provenance for multi-source claims (2025) | [Springer IJDL 2025](https://link.springer.com/article/10.1007/s00799-025-00431-x), [Padua paper PDF](https://www.dei.unipd.it/~marches1/papers/mmg_etal-ijdl-2025.pdf) |
| Semantic units for KGs | [arXiv 2301.01227](https://arxiv.org/pdf/2301.01227) |
| Edge-labelled vs property graphs | [arXiv 2204.06277](https://arxiv.org/pdf/2204.06277), [arXiv 2110.13348](https://arxiv.org/pdf/2110.13348) |
| RDF-star provenance | [metaphacts — provenance with RDF-star](https://blog.metaphacts.com/citation-needed-provenance-with-rdf-star), [W3C RDF-star WG list (Jan 2024)](https://lists.w3.org/Archives/Public/public-rdf-star-wg/2024Jan/0040.html), [W3C RDF-star WG list (Apr 2024)](https://lists.w3.org/Archives/Public/public-rdf-star-wg/2024Apr/0028.html) |
| Statement-level metadata overhead in Neo4j | [Semantic Web Journal swj4028](https://semantic-web-journal.net/system/files/swj4028.pdf) |
| Edge properties vs named graphs (Stardog) | [Stardog community thread](https://community.stardog.com/t/rdf-or-property-graph-in-stardog/2168) |

## MCP protocol and SDKs

| Topic | Source |
|---|---|
| MCP 2026-07-28 (stateless, `server/discover`) | [Composio — MCP 2026-07-28 update](https://composio.dev/blog/mcp-2026-07-28-update-statelessness-apps-auth), [Better Stack — stateless MCP](https://betterstack.com/community/guides/ai/mcp-stateless/), [Solo.io — stateless spec details](https://www.solo.io/blog/mcp-stateless-spec-changes-the-engineering-details), [GitLab issue — support 2026-07-28](https://gitlab.com/gitlab-org/gitlab/-/work_items/627825), [notesbylex — MCP is stateless](https://notesbylex.com/mcp-is-now-a-stateless-protocol) |
| Rust SDK `rmcp` 3.x | [docs.rs — rmcp 3.1.0](https://docs.rs/crate/rmcp/3.1.0/source/README.md) |
| Go SDK v1.7 | [GitHub releases — go-sdk](https://github.com/modelcontextprotocol/go-sdk/releases), [pkg.go.dev — go-sdk v1.7.0-pre.3](https://pkg.go.dev/github.com/modelcontextprotocol/go-sdk@v1.7.0-pre.3) |
| C# SDK 1.0 / 1.1 | [NuGet — ModelContextProtocol.AspNetCore](https://www-1.nuget.org/packages/ModelContextProtocol.AspNetCore), [I Programmer — MCP C# SDK 1.0](https://www.i-programmer.info/news/89-net/18731-mcp-c-sdk-10-released.html), [NuGet profile](https://www.nuget.org/profiles/ModelContextProtocol) |

## Storage and search libraries

| Topic | Source |
|---|---|
| fjall 3.0 | [Phoronix — Fjall 3.0](https://www.phoronix.com/news/Fjall-3.0-Released), [fjall blog](https://fjall-rs.github.io/post/fjall-3) |
| redb and its benchmark table | [docs.rs — redb 4.4.0](https://docs.rs/crate/redb/4.4.0) |
| tantivy vs Lucene | [quickwit search-benchmark-game](https://github.com/quickwit-oss/search-benchmark-game), [ParadeDB — interview with Paul Masurel](https://paradedb.com/blog/tantivy-interview), [lib.rs — tantivy](https://lib.rs/crates/tantivy) |
| Lucene.NET 4.8 beta status | [Lucene.NET beta00018](https://lucenenet.apache.org/download/version-4.8.0-beta00018.html) |
| bleve v2 (vector, hybrid) | [bleve overview](https://upd.dev/blevesearch/bleve), [goja-bleve (vector build notes)](https://pkg.go.dev/github.com/go-go-golems/goja-bleve) |
| Pebble, Badger, bbolt | [builder.io — pebble](https://best-of-web.builder.io/library/cockroachdb/pebble), [builder.io — badger](https://best-of-web.builder.io/library/dgraph-io/badger), [builder.io — bbolt](https://best-of-web.builder.io/library/etcd-io/bbolt) |
| ZoneTree (C#) | [NuGet — ZoneTree](https://feed.nuget.org/packages/ZoneTree) |
| usearch bindings | [usearch README](https://cdn.jsdelivr.net/npm/usearch@2.26.0/README.md), [PyPI — usearch](https://pypi.org/project/usearch/) |
| fastembed-rs, ort | [docs.rs — ort 2.0.0-rc.13](https://docs.rs/crate/ort/2.0.0-rc.13/source/README.md), [sourcepulse — fastembed-rs](https://www.sourcepulse.org/projects/2337587), [rust-embedding-bench](https://awesome.ecosyste.ms/projects/github.com%2Fjerrythomas%2Frust-embedding-bench) |

## To verify before relying on

- Memgraph core licence (believed BSL 1.1), ArangoDB BSL since 3.12, SurrealDB current licence.
- Dgraph ownership history (Hypermode 2023, Istari Digital 2025).
- FalkorDB Rust rewrite (single source).
- C# MCP SDK support for spec 2026-07-28.
- Maturity of Roaring bitmap ports for .NET.
