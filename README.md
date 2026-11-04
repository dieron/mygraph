# MyGraph

**Built by AI, for AI.** A knowledge graph that AI agents use as their long-term memory, where
every fact points to the source it came from.

MyGraph was designed and written by AI coding agents (Claude, in Claude Code), with a human
setting the goals and making the decisions. Its users are agents too: they read and write it
through [MCP](https://modelcontextprotocol.io) tools shaped for how agents work, not through a
query language made for people. Humans install it, connect their agents, and look at what the
agents have built (a stats page and a graph viewer).

## Why an agent would want it

- **Every fact cites its evidence.** A node, a link or a single property is backed by a source:
  a URL, a verbatim sentence, a document, a file, or the agent's own observation. By default a write
  without a citation is refused, so an answer can always be traced back and checked.
- **Tools made for agents.** 13 MCP tools and no query language. One `write` records a whole
  source (evidence, nodes, links, citations) atomically; `resolve` checks 100 names at once
  before writing, so agents don't create duplicates; results are compact text that costs few
  tokens; every error comes with a hint saying what to do next; writes are idempotent, so
  retrying is safe.
- **Search by words and by meaning.** Full-text search with stemming, plus semantic search
  with a small multilingual embedding model that runs locally, so a question in Ukrainian finds an
  English source. No calls to an outside API.
- **Nothing is deleted.** Wrong facts are retracted (and can be restored), duplicates are merged,
  and `history` shows every change, so an agent's mistakes can be undone.
- **One small server.** A single Rust binary keeps the graph and its indexes in memory and stores
  everything in SQLite. It runs on a laptop or a home server, with several graphs per server.

## Status

A working prototype, used daily, before its first release. No prebuilt binaries yet: build it
from source or with Docker. Tested on Windows and Linux; macOS has not been tried.

Known limits:
- One server per data folder; no replication or clustering.
- About 450–550 writes per second per graph, and reads wait while a write commits
  ([TODO.md](TODO.md), "Concurrency").
- The server speaks plain HTTP with API keys. Over a network, use it on a network you trust or
  behind a reverse proxy with TLS. Clients that need remote MCP with OAuth (claude.ai on the web
  and mobile) can't connect yet.
- The specification is a draft (0.1.0-draft); storage migrates itself on upgrade, but the API may
  still change.

## Quick start

With Docker (the image includes the embedding model):

```bash
git clone https://github.com/dieron/mygraph.git && cd mygraph
```

```bash
cp .env.example .env    # then set MYGRAPH_ADMIN_KEY, and MYGRAPH_AGENT_KEY for agents
```

```bash
docker compose up -d --build
```

Or from source, with [Rust](https://rustup.rs) installed:

```bash
cargo build --release -p mygraph-server
```

```bash
target/release/mygraph serve
```

From source the server listens on `127.0.0.1:7420` and, without API keys, accepts local requests
as an admin. It matches words only until you download the embedding model (135 MB, once):

```bash
deploy/fetch-model.sh <data_dir>/models/multilingual-e5-small
```

Then connect an agent (next section) and ask it to create a graph and remember something.

## Using it over MCP

MCP is the main way in. An agent gets these tools:

| Tool | What it does |
|---|---|
| `graphs` | List the graphs on the server, or create one |
| `overview` | What a graph holds: counts, node and link types in use, conventions to follow |
| `search` | Find nodes and evidence by name, alias, words or meaning |
| `resolve` | Check up to 100 names before writing: match, ambiguous, or none (with similar nodes) |
| `get` | One object with its properties, links, citations and sources |
| `write` | Record one source: evidence, nodes, links and citations, all at once |
| `retract` | Mark facts as wrong (restorable), or undo a whole write |
| `merge` | Merge duplicate nodes; the old IDs keep resolving |
| `neighbors` | The subgraph around a node, filtered by link type and direction |
| `paths` | How two nodes are connected (shortest path or several) |
| `find` | Filtered lists, counts, group-by and aggregates |
| `history` | What changed, and since when |
| `stats` | Size, connectivity, growth and how well facts are sourced |

### Connecting a client

There are three ways to connect, and all of them use the same tools:

1. **stdio bridge to a running server** (the usual one). The client starts
   `mygraph mcp --url http://<host>:7420 --graph <default graph>`, which forwards every call to the
   server. Pass the key with `--key` or the `MYGRAPH_KEY` environment variable.
2. **Streamable HTTP**, for clients that support it: `http://<host>:7420/mcp`, with the header
   `Authorization: Bearer <key>`.
3. **No server.** `mygraph mcp --data-dir <folder>` opens a data folder in the client's own
   process. This works only while no other process is using that folder.

Claude Code, for every project (`-s user`):

```bash
claude mcp add mygraph -s user -e MYGRAPH_KEY=<key> -- mygraph mcp --url http://127.0.0.1:7420 --graph research
```

Or over HTTP, with no local program:

```bash
claude mcp add --transport http mygraph http://<host>:7420/mcp --header "Authorization: Bearer <key>"
```

Claude Desktop, in `claude_desktop_config.json` (Settings → Developer → Edit Config). Use the
full path of the `mygraph` program, then quit and reopen the app:

```json
{ "mcpServers": { "mygraph": { "command": "/path/to/mygraph", "args": ["mcp", "--url", "http://127.0.0.1:7420", "--graph", "research"],
                               "env": { "MYGRAPH_KEY": "<key>" } } } }
```

Cursor, VS Code with GitHub Copilot and OpenAI Codex use the same command;
[skills/mygraph/references/setup.md](skills/mygraph/references/setup.md) has the snippet for each.
To check the connection, ask the agent to call `graphs`.

On Windows, `deploy/windows/deploy.ps1 -BridgeOnly` installs just the `mygraph` program, for a
server that runs elsewhere. The bridge lists the tools itself, so update it when the server gains
new ones.

### The skill: teach the agent to use it well

The tools work on their own, but agents do much better with the skill in
[skills/mygraph/](skills/mygraph/SKILL.md). It covers:
- picking or creating a graph, and reusing its vocabulary;
- one `write` per source, with each fact's own sentence as its evidence;
- `resolve` before creating anything;
- which tool answers which kind of question;
- citing evidence IDs in answers;
- what to do when searches miss;
- fixing mistakes.

The skill is one folder in the Agent Skills format, which Claude, Codex, Cursor and GitHub Copilot
read. Install it in `~/.claude/skills/mygraph/` (Claude Code) or `~/.agents/skills/mygraph/`
(Codex, Cursor, Copilot), or upload it zipped in the Claude apps. `deploy/install-skill.ps1` does
all three on Windows.

### What a session looks like

```text
graphs                                         → research (ready, default)
overview                                       → types in use: Person, Work, Machine…
resolve  [{"type": "Person", "name": "Ada Lovelace"}]   → none
write    { evidence: [the page, the sentence], nodes: [Ada, Analytical Engine],
           links: [WROTE_ABOUT], cite: the sentence }   → refs @ada = n_3kq …
search   "who wrote about early computers"     → n_3kq Person "Ada Lovelace" (semantic)
get      n_3kq with evidence                   → born 1815-12-10 · sources: e_2 "In 1843…"
```

Results are compact text ([spec/api/compact-format.md](spec/api/compact-format.md)); any tool
returns JSON with `format: "json"`.

## How well agents do with it

Agents were measured on published benchmarks. They answered once from MyGraph, using the tools
and the skill, and once by reading the whole source text in their prompt ("direct"). Answers
were graded on correctness and on whether they cite the right evidence. All runs used GPT-6.1
Terra. Details and every finding are in
[docs/11-benchmarks.md](docs/11-benchmarks.md).

| Benchmark | Questions | From MyGraph | Direct | Notes |
|---|---|---|---|---|
| HotpotQA (multi-hop, Wikipedia) | 50 | answer F1 0.80, citation F1 0.85 | 0.82, 0.88 | The agent stored the passages itself |
| MuSiQue (2–4 hops) | 50 | answer F1 0.75, supporting paragraphs F1 0.87 | 0.76, 0.88 | |
| LoCoMo (long conversations) | 7 × 20 | 0.85 correct, citation F1 0.77 | 0.84, 0.74 | Half the cost per question |
| LongMemEval (multi-session, temporal) | 50 | 0.90 correct, citation F1 0.87 | 0.86, 0.91 | $0.015 against $0.095 per question |

MyGraph doesn't answer better than an agent that reads everything. It answers about as well while
reading a small part of the text, and every answer points to its sources. That matters once
the memory no longer fits in a prompt.

The engine was also compared with Neo4j on the LDBC Social Network Benchmark (scale factor 0.1, about
330 000 nodes and 1.5 million links, results derived from the benchmark, not audited). In 3 228 `neighbors`, `paths` and `find`
cases, both returned the same results. Point reads (IS1 and IS4–IS7) took 0.2–0.3 ms at the
median over HTTP, against 0.4–0.5 ms for Neo4j. A 3-hop friend search (IC1-style) is still much
slower: 64 ms against 1.6 ms.

## Running it as a service

For a home server or NAS that other machines and agents use, run it with Docker
([Quick start](#quick-start)):

- Data lives in the `mygraph-data` volume (`/data` in the container). The server restarts with the
  machine and reports its health to Docker (`mygraph health`).
- The container listens on all interfaces, which MyGraph allows only with API keys. Every request
  needs `Authorization: Bearer <key>`. Set `MYGRAPH_BIND=127.0.0.1` in `.env` to keep it on that
  machine only.
- Settings are `MYGRAPH_*` environment variables:
  - listen address, data folder, default graph and log level;
  - the admin, agent and read-only keys, each also as `<NAME>_FILE` for Docker secrets;
  - semantic search: `MYGRAPH_SEMANTIC` and `MYGRAPH_SEMANTIC_THRESHOLD`.

  See `Config::apply_env` in [crates/mygraph-server/src/config.rs](crates/mygraph-server/src/config.rs).
- To update on the machine, run `git pull && docker compose up -d --build`. From your own computer,
  `deploy/docker/deploy.sh <ssh-destination>` does it over SSH without a clone on the server. It
  sends the committed `HEAD`, snapshots every graph, rebuilds, restarts and waits until the server
  is healthy. `.env` and the data are left alone.
- [.github/workflows/docker.yml](.github/workflows/docker.yml) publishes a multi-arch image (amd64,
  arm64) to `ghcr.io/dieron/mygraph`. `:main` follows the main branch, and version tags will add
  `:latest`. To use it instead of building, set `MYGRAPH_IMAGE=ghcr.io/dieron/mygraph:main` in
  `.env`, then run `docker compose pull && docker compose up -d`.

**API keys.** Without a configuration file, keys come from the environment. In a `server.toml`
(`mygraph serve --config server.toml`), each key gets an actor name, a role and optionally its
graphs:

```toml
data_dir = "../mygraph-data"

[[keys]]
key_sha256 = "…"          # `mygraph hash-key <key>` prints it; or `key = "…"`
actor = "claude-research"
role = "agent-write"      # agent-read, agent-write or admin
graphs = ["research"]     # optional; default all graphs
```

**Semantic search** settings, in `server.toml` or the environment:

```toml
[search]
semantic = true            # MYGRAPH_SEMANTIC: also match by meaning (false: words only)
semantic_threshold = 0.80  # MYGRAPH_SEMANTIC_THRESHOLD: lowest similarity for a hit found by meaning
model_dir = "..."          # MYGRAPH_MODEL_DIR: default <data_dir>/models/multilingual-e5-small
```

Each graph's vectors are a cache in `<data_dir>/vectors/<graph>/`. When the cache is missing, it is
rebuilt in the background, and search matches words only until that finishes. Agents see the
settings in `overview`. Without semantic search, the tools stop offering it.

**On Windows without Docker**, `cargo build --release -p mygraph-server`, then
`powershell -ExecutionPolicy Bypass -File deploy/windows/deploy.ps1`. This installs the server in
`%LOCALAPPDATA%\MyGraph` and starts it at logon (data in `%USERPROFILE%\mygraph-data`). Run both
again to upgrade; `-Uninstall` removes it and keeps the data.

## Watching and backing up

- `/admin/ui` is the stats page: graphs, request statistics, slow requests, and per graph its
  counts, growth, connectivity, evidence and recent batches.
- `/admin/ui/graph` is the graph viewer. It draws a graph as a network, with search, type filters
  and the properties, links and evidence of whatever you click.
- `GET /metrics` serves Prometheus metrics, listed in [spec/api/admin.md](spec/api/admin.md).
- To back up, `POST /admin/graphs/{g}/snapshot` copies a graph online into `<data_dir>/snapshots/`.
  `POST /admin/graphs/{g}/restore` brings one back.
- To move a graph, `POST /admin/graphs/{g}/export` writes it as JSONL
  ([spec/api/export-format.md](spec/api/export-format.md)). `POST /admin/graphs/{new}/import`
  reads it back with every ID intact. Copy `<data_dir>/blobs/` along when moving to another
  machine.

## REST API

Agents with a shell, and programs, can use REST, which has the same operations as the tools. Every
operation is `POST /v1/graphs/{graph}/{operation}`, with the JSON its spec file describes
([spec/api/operations/](spec/api/operations/)). The server serves
[spec/api/openapi.yaml](spec/api/openapi.yaml) at `/openapi.yaml`. Conventions are in
[spec/api/conventions.md](spec/api/conventions.md) and errors in
[spec/api/errors.md](spec/api/errors.md).

```bash
curl -s -X POST localhost:7420/admin/graphs -d '{"name": "research"}'
```

```bash
curl -s -X POST localhost:7420/v1/graphs/research/write -d '{"evidence": [{"ref": "@e", "kind": "text", "content": "Ada Lovelace wrote about the Analytical Engine."}], "nodes": [{"ref": "@ada", "type": "Person", "name": "Ada Lovelace", "cite": ["@e"]}, {"ref": "@ae", "type": "Machine", "name": "Analytical Engine", "cite": ["@e"]}], "links": [{"type": "WROTE_ABOUT", "from": "@ada", "to": "@ae", "cite": ["@e"]}]}'
```

```bash
curl -s "localhost:7420/v1/graphs/research/search?q=lovelace"
```

Add `-H "Authorization: Bearer <key>"` when the server has keys.

## Building and testing

```bash
cargo test --workspace
```

`crates/mygraph-core/tests/vectors.rs` runs every shared test vector in `conformance/vectors/`.
Benchmarks:
- `crates/mygraph-bench`: synthetic data and memory;
- `crates/mygraph-ldbc`: LDBC SNB and the Neo4j comparison;
- `crates/mygraph-agentbench`: the agent benchmarks.

On Windows, if linking fails with `cannot open file 'msvcrt.lib'`, the newest Visual Studio lacks
the x64 desktop libraries. Build from a developer prompt of an installation that has them, for
example after running `vcvars64.bat` from VS 2022 Build Tools.

## How it is built

MyGraph is written specification-first. [spec/](spec/README.md) is the normative contract: data
model, operations, ranking, storage format, errors. [conformance/](conformance/README.md) holds
the test vectors that any implementation must pass. The Rust code is one implementation of the
spec, and the goal is that an agent could rewrite it in another language from `spec/` and
`conformance/` alone.

| Document | What it answers |
|---|---|
| [Requirements](docs/00-requirements.md) | What was asked, derived needs, glossary |
| [Existing systems](docs/01-research-existing-systems.md) | What exists already, what was borrowed, build or reuse |
| [Language comparison](docs/02-language-comparison.md) | Rust, Go or C#, and why Rust |
| [Storage and search](docs/03-storage-and-search.md) | Storage engines, search components, the SQLite schema |
| [Data model](docs/04-data-model.md) | Nodes, links, evidence, citations, IDs, vocabulary, lifecycle |
| [Architecture](docs/05-architecture.md) | Components, write and read paths, memory indexes, concurrency |
| [Agent API](docs/06-agent-api.md) | The operations with examples, errors, the compact format |
| [Admin API](docs/07-admin-api.md) | Management, statistics, maintenance, access |
| [PoC plan](docs/08-poc-plan.md) | Milestones, exit criteria, tests, risks |
| [Roadmap](docs/09-roadmap.md) | What comes after |
| [Portability](docs/10-portability-and-spec.md) | How the spec and conformance suite keep a rewrite easy |
| [Benchmarks](docs/11-benchmarks.md) | LDBC SNB against Neo4j, and the agent benchmarks |
| [Decisions](docs/decisions.md) | The architecture decision records |

Open work is in [TODO.md](TODO.md).

## Security

See [SECURITY.md](SECURITY.md): what the server protects, how to run it safely, and how to report
a vulnerability.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
Contributions are accepted under the same terms. The bundled
[Cytoscape.js](crates/mygraph-server/ui/vendor/) is MIT-licensed. The embedding model,
[multilingual-e5-small](https://huggingface.co/intfloat/multilingual-e5-small), is MIT-licensed
and is downloaded at build time, not stored in this repository.
