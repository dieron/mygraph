# TODO

Deferred work, kept here so it is not lost. The plan with milestones is
[docs/08-poc-plan.md](docs/08-poc-plan.md); later features are in [docs/09-roadmap.md](docs/09-roadmap.md).

## Before promoting it

- [ ] TLS: serve HTTPS directly (certificate and key files in the configuration), so a server on a
      network needs no reverse proxy.
- [ ] OAuth for remote MCP, so claude.ai (web and mobile) and other remote clients can connect.
- [x] Tag the first release (v0.1.0) and update the README's quick start for it.

## Distribution and setup (in this order; docs/09-roadmap.md, Packaging and setup)

- [x] **Docker and compose** — `Dockerfile` (distroless, non-root, 45 MB), `compose.yaml`,
      `.env.example`, `MYGRAPH_*` environment settings, `mygraph health`; the multi-arch image
      workflow `.github/workflows/docker.yml` runs once the repository is on GitHub.
- [x] Run it as a service on a home machine; `deploy/docker/deploy.sh <ssh-destination>` deploys
      new versions.
- [ ] Prune old snapshots on the server (each deploy adds one per graph).
- [x] Release executables built by GitHub Actions on a version tag (`.github/workflows/release.yml`):
      Linux x64 and arm64, Windows x64, macOS arm64, each smoke-tested with the model; the model as
      its own archive, found next to the binary; SHA256SUMS; a draft release to publish by hand.
- [ ] Windows arm64 and macOS x64 builds, if anyone asks.
- [ ] One-line installers (shell and PowerShell) that fetch the right archive and the model.
- [ ] `mygraph setup` / `mygraph doctor`: data folder, first graph, autostart on each OS, detect
      and connect Claude Desktop, Claude Code, Cursor, VS Code and Codex, install the skill.
- [ ] Claude Desktop extension (`.mcpb`) for one-click install.
- [ ] Claude Code plugin with the skill and the MCP registration.
- [ ] Code signing for Windows and macOS.

## Publishing on GitHub

- [x] README for a public audience, agents first ("Built by AI, for AI"): status, limits,
      quickstart, using it over MCP, benchmark results, bundled Cytoscape.js and the model.
- [x] `SECURITY.md`: without API keys every request is admin, so bind to loopback; how to report.
- [x] Name: MyGraph (no other graph database or agent memory project uses it).
- [x] `repository` in `Cargo.toml`; private vulnerability reporting enabled (SECURITY.md points
      to it).
- [x] Public repository github.com/dieron/mygraph, `main` pushed with its history (2026-11-04).

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
- [x] `find` with a `linked` condition starts from the linked node's links (IS2: 32 ms → 0.55 ms).
- [x] Traversals skip links of other types by a per-link type number (3-hop walk 50 ms → 6 ms).
- [ ] An option for `neighbors` to leave out links: IC1 still spends ~19 ms building ~13 000
      link entries it does not need (64 ms over REST).
- [ ] Find the ~10 ms p99 stall on inserts (SQLite checkpoint?); Neo4j's p99 is 3–5 ms.
- [ ] IS3 over REST: one `get` per 100 links; a way to read link properties with the links.
- [ ] Run the driver on SF1.
- [x] HotpotQA runner with citation grading (`mygraph-agentbench hotpot`); a 5-question run.
- [x] Skill: one `text` evidence per sentence instead of one per page (citation precision
      0.26 → 0.80 on 5 HotpotQA questions).
- [x] OpenAI models in the HotpotQA runner (`--provider openai`: own agent loop over the MCP tools).
- [x] HotpotQA, 5 questions on GPT-6.1 Terra: same graph-mode scores as Sonnet at ~40 % of the cost.
- [x] HotpotQA, 50 questions on Terra: graph-backed answers and citations within 0.01–0.05 of
      reading the passages (answer F1 0.785 vs 0.788, citation F1 0.83 vs 0.84); $4.56.
- [ ] HotpotQA, the same 50 questions on Sonnet.
- [x] Tool calls treat empty top-level values (`"type": ""`, `[]`, `{}`, `null`) as absent
      ([MCP-TOOL-5]); agents on OpenAI models send them.
- [ ] Agents sometimes send one 20+ KB `write` that arrives as invalid JSON; suggest smaller
      writes in the skill?
- [x] MuSiQue in the same runner (`mygraph-agentbench musique`); 5 questions on Terra. Both modes
      now share the answer rules: with `unknown` offered only to the graph agent, it abstained on
      the 3- and 4-hop questions.
- [x] MuSiQue on 50 questions on Terra: graph matches direct (answer F1 0.747 vs 0.763,
      supporting-paragraph F1 0.868 vs 0.877); the 5 `unknown` answers, the same in both modes,
      are questions whose paragraphs do not support the key; $6.99.
- [x] HotpotQA again with the shared answer rules (`--reanswer` on the same graphs): graph 0.799
      vs direct 0.815 answer F1, 0.853 vs 0.879 citation F1; no change to the conclusion.
- [x] LoCoMo runner (`mygraph-agentbench locomo`): ingest session by session, a model as judge,
      citations graded against the evidence messages.
- [x] LoCoMo pilot, conv-30 on Terra: graph 0.83 correct vs direct 0.91 (categories 1–4);
      96.5 % of evidence messages stored; losses in search, hedged dates and lists across
      sessions; $3.60.
- [x] LoCoMo, 7 conversations × 20 questions on Terra ($14): graph 0.85 vs direct 0.84 correct
      (a tie within judge noise), citation F1 0.77 vs 0.74, half the cost per question. The full
      run (about $80–100) is not planned.
- [x] Text search matches word forms (English stemming, storage format 2 with migration 0002;
      the tantivy index rebuilds itself). In the LoCoMo pilot a stored message ("clothes store is
      open") was missed by searches for "clothing store" and "store opened".
- [x] Stemming's effect: the pilot's 105 conv-30 questions answered again from its graph; the
      store question is now found and correct, graph 0.83 → 0.86 on categories 1–4 (the rest
      within noise); $1.17.
- [x] LongMemEval runner (`mygraph-agentbench longmemeval`): the history stored by a script,
      not an agent (an agent ingest would cost $5–10 per question); a judge rule per question
      type.
- [x] LongMemEval, 20 questions on Terra ($2.35): graph 0.85 vs direct 0.90 correct, the one
      difference a reading of the question; the same answer messages cited; $0.010 vs $0.107 per
      question.
- [x] LongMemEval, 50 multi-session and temporal questions (`--types`; $5.30): graph 0.82 vs
      direct 0.86, citation F1 0.81 vs 0.91; the graph's misses are messages worded unlike the
      question ("kitchen appliance" vs "a smoker"), which word search cannot reach.
- [x] Skill hint: search matches words, not meaning; after a few misses, narrow down (dates,
      types, links) and read the evidence. LongMemEval 50 in graph mode: citation F1 0.81 → 0.88,
      correct 41 → 42 of 50; $0.59.
- [x] Semantic search (crate `mygraph-semantic`: multilingual-e5-small, usearch, reciprocal rank
      fusion with full text; `resolve` lists `similar` nodes). LongMemEval 50 in graph mode,
      against the hint: correct 42 → 41, citation F1 0.88 → 0.83, 13 % more tokens; $0.78.
      Words already showed 103 of 107 answer messages; meaning-only hits fill every search and
      push some word matches out (docs/11-benchmarks.md, findings 44–47).
- [x] Word matches before meaning-only hits ([RANK-9]). LongMemEval 50 in graph mode: correct
      45 of 50 (hint 42, interleaved 41, direct 43), citation F1 0.87 (hint 0.88), 9 % more
      tokens than the hint; $0.78. The gains are answering, not finding (findings 48–50).
- [x] Settings: `search.semantic` (default on) and `search.semantic_threshold` (default 0.80,
      from 0.77); `overview` and `GET /v1/graphs` report them; without semantic search the MCP
      tools drop mode `semantic`. Replayed on the 160 searches: 0.80 halves the meaning-only hits,
      ~6 % fewer input tokens; Ukrainian, Russian and German queries find English messages only
      by meaning (findings 51–54).
- [ ] LongMemEval 50 again at 0.80 with the skill's "search with the whole question" advice
      (about $0.78): the tokens saved for real.
- [ ] A stemmer or meaning for Ukrainian and Russian word endings (word search misses
      «коптильню» for «коптильня»).
- [ ] A test where words fail more often (extracting ingest, other languages, questions written
      without the history's words), to see whether meaning earns its cost.
- [ ] Docker image with the model: build and check it (not built yet: no Docker on the
      development machine); the image grows by about 165 MB (model 135 MB, ONNX Runtime in the
      binary about 28 MB).
- [ ] `resolve` on the Leipzig entity-resolution datasets.
- [ ] Agent with MyGraph tools versus agent writing Cypher (CypherBench-style questions).

## Concurrency (many users on one graph)

A load test on a small graph (8 cores, NVMe, client on the same machine) gave about 6 800 reads/s
with 8 readers, but only about 1 700 reads/s once 2 writers joined, and writes topped out at
about 450–550/s per graph however many writers ran. The cause: a write holds the graph's
exclusive lock through the SQLite commit and its full sync, so every reader waits for the disk.
docs/05-architecture.md §6 meant the lock to cover only the in-memory apply.

- [ ] Commit to SQLite outside the exclusive lock (a separate writer mutex for validation and
      the transaction), then take the write lock only to apply the change in memory.
- [ ] Group commit: concurrent writes share one transaction and sync.
- [ ] A small pool of read connections for the reads that reach SQLite (evidence text).
- [ ] Per-key rate limits or quotas.
- [ ] A concurrency test in the suite, and a mixed read/write benchmark in
      docs/11-benchmarks.md, including a large graph where long reads delay writers.

## Observability

- [ ] Example Grafana dashboard for `/metrics` under `deploy/`.
- [ ] Latency percentiles (p50 / p95 / p99) per operation on the stats page and in `/admin/stats`
      (the histogram buckets are already there).
- [ ] Graph statistics sections `quality` (duplicate suspects, observation-only facts,
      contradicted facts, similar type names) and `usage` (docs/07-admin-api.md §2).

## API gaps

- [ ] `GET /v1/graphs/{g}/search` ignores `Accept: text/plain` (the compact format).
- [ ] Per-request time limits (spec/api/conventions.md [API-LIMIT-1]); with many users this also
      keeps one heavy `find` or `paths` from holding a graph up.
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
