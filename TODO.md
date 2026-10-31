# TODO

Deferred work, kept here so it is not lost. The plan with milestones is
[docs/08-poc-plan.md](docs/08-poc-plan.md); later features are in [docs/09-roadmap.md](docs/09-roadmap.md).

## Distribution and setup (in this order; docs/09-roadmap.md, Packaging and setup)

- [x] **Docker and compose** — `Dockerfile` (distroless, non-root, 45 MB), `compose.yaml`,
      `.env.example`, `MYGRAPH_*` environment settings, `mygraph health`; the multi-arch image
      workflow `.github/workflows/docker.yml` runs once the repository is on GitHub.
- [x] Run it as a service on a home machine; `deploy/docker/deploy.sh <ssh-destination>` deploys
      new versions.
- [ ] Prune old snapshots on the server (each deploy adds one per graph).
- [ ] Release executables built by GitHub Actions on a version tag (Windows, macOS, Linux; x64 and
      arm64) with checksums; one-line installers (likely cargo-dist).
- [ ] `mygraph setup` / `mygraph doctor`: data folder, first graph, autostart on each OS, detect
      and connect Claude Desktop, Claude Code, Cursor, VS Code and Codex, install the skill.
- [ ] Claude Desktop extension (`.mcpb`) for one-click install.
- [ ] Claude Code plugin with the skill and the MCP registration.
- [ ] Code signing for Windows and macOS.

## Publishing on GitHub

- [ ] README for a public audience: current status, quickstart, limits, bundled Cytoscape.js.
- [ ] `SECURITY.md`: without API keys every request is admin, so bind to loopback; how to report.
- [ ] Final name; `repository` and `description` in `Cargo.toml`.
- [ ] Create the empty repository and push `main` with its history.

## Next milestone (M4)

- [ ] Benchmark run on the 1 M / 5 M / 1 M dataset with the budget table; record the dataset
      (generator version, seed, SHA-256) in `conformance/bench/`.
- [ ] Agent trial: an agent ingests about 30 real sources on one topic, then answers about 20
      questions from the graph only; measure calls per source, duplicates, vocabulary drift and
      the share of answers that cite evidence. Turn it into a repeatable eval suite.
- [ ] Portability cold read: a fresh session implements the pure functions in another language
      from `spec/` and `conformance/` only.

## External benchmarks (docs/11-benchmarks.md)

- [x] LDBC SNB loader (`mygraph-ldbc`); SF0.1 loads with exact counts, with and without evidence.
- [x] SF1 load numbers: 60 min with evidence on a busy machine, 23 GiB file, 10.9 GiB RAM.
- [ ] Where the graph file's bytes go: ~650 bytes per object without evidence, about 21× the raw
      CSV (Neo4j: about 2×).
- [ ] Faster large loads: an offline bulk import (like `neo4j-admin import`), larger
      transactions for trusted loaders, one batch-level citation instead of one per fact.
      Published SF1 loads: Neo4j 71 s, TypeGraph/SQLite 587 s; ours took 60 min with evidence.
- [x] Neo4j Community on Windows with the same data (`mygraph-ldbc neo4j-csv`); SF0.1 counts match.
- [x] The harness that compares `neighbors` / `paths` / `find` results with Cypher
      (`mygraph-ldbc diff`); 3 212 SF0.1 cases agree with Neo4j.
- [x] Fix what it found: one group when nothing matches (code), `ne` / `not_in` on lists (spec),
      no silent renaming of names (policy `naming: styled | free`).
- [x] Extend the harness: `k_shortest`, directed `paths`, `steps`, `as_of` (links get
      `valid_from`), `contains` / `starts_with`, sorting, paging, group order; 3 228 cases agree,
      and six of seven deliberate breaks are caught.
- [ ] Run the harness on SF1 (needs the SF1 Neo4j import and the reload with `valid_from`).
- [ ] Rerun the SF1 load on a quiet machine with the current mapping.
- [ ] Deployed instance: after this lands, agents get `vocab.not_styled` for names the server used
      to rewrite; check the skill and tools in use there still send styled names.
- [x] Workload driver (`mygraph-ldbc bench`): IS1–IS7, IC1-style, IC13 and update-stream
      inserts against Neo4j, in-process and over REST; every answer agrees.
- [ ] Make `find` with a `linked` condition start from the linked node's links (IS2: 32 ms).
- [ ] Faster multi-hop `neighbors` over one link type: skip other links cheaply, and maybe an
      option to leave out links (IC1: 50 ms in-process, 87 ms over REST).
- [ ] Find the ~10 ms p99 stall on inserts (SQLite checkpoint?).
- [ ] Run the driver on SF1.
- [ ] HotpotQA / MuSiQue with citation grading; then LongMemEval and LoCoMo.
- [ ] `resolve` on the Leipzig entity-resolution datasets.
- [ ] Agent with MyGraph tools versus agent writing Cypher (CypherBench-style questions).

## Observability

- [ ] Example Grafana dashboard for `/metrics` under `deploy/`.
- [ ] Latency percentiles (p50 / p95 / p99) per operation on the stats page and in `/admin/stats`
      (the histogram buckets are already there).
- [ ] Graph statistics sections `quality` (duplicate suspects, observation-only facts,
      contradicted facts, similar type names) and `usage` (docs/07-admin-api.md §2).

## API gaps

- [ ] `GET /v1/graphs/{g}/search` ignores `Accept: text/plain` (the compact format).
- [ ] Per-request time limits (spec/api/conventions.md [API-LIMIT-1]).
- [ ] Export leaves out the change log and vocabulary notes.
- [ ] Admin `PATCH` for graph policies and vocabulary notes; JSON Schemas per operation in
      `spec/api/schemas/`.

## Graph viewer

- [ ] When browsing a large graph by expanding nodes, a link between two nodes already on screen
      appears only after one of them is expanded; fetch the links among shown nodes.

## Clients

- [ ] Check the `graphs` and `stats` tools in Claude Desktop after a restart.
- [ ] claude.ai (web and mobile) needs remote MCP with OAuth and HTTPS.
- [ ] Feedback from agents to central GitHub issues (docs/09-roadmap.md, Feedback).
