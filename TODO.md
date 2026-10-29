# TODO

Deferred work, kept here so it is not lost. The plan with milestones is
[docs/08-poc-plan.md](docs/08-poc-plan.md); later features are in [docs/09-roadmap.md](docs/09-roadmap.md).

## Distribution and setup (in this order; docs/09-roadmap.md, Packaging and setup)

- [x] **Docker and compose** — `Dockerfile` (distroless, non-root, 45 MB), `compose.yaml`,
      `.env.example`, `MYGRAPH_*` environment settings, `mygraph health`; the multi-arch image
      workflow `.github/workflows/docker.yml` runs once the repository is on GitHub.
- [ ] Run it as a service on the chosen home machine (README, "Running as a service with Docker").
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
