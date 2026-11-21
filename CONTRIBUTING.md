# Contributing

Thanks for looking. Bug reports, questions and ideas are welcome as
[issues](https://github.com/dieron/mygraph/issues). For anything bigger than a small fix, open an
issue before a pull request, so we can agree on the change first.

Report security problems privately, as [SECURITY.md](SECURITY.md) describes, not in an issue.

## How the project is made

MyGraph is written by AI coding agents (Claude, in Claude Code), with a human setting the goals and
reviewing the result. Pull requests written with an agent are fine. Whoever opens one is
responsible for it: read the diff, run the checks below, and make sure the description says what
changed and why.

It is written specification-first:

- [spec/](spec/README.md) is the contract. Every normative rule has an ID such as
  **[ADMIN-GRAPH-6]**, and the code cites that ID where it implements the rule.
- A change in behavior changes the spec in the same pull request, adding a new rule ID or editing
  an existing rule. Rule IDs are never renumbered or reused.
- [conformance/vectors/](conformance/README.md) holds the language-neutral test vectors. If the
  prose and a vector disagree, the vector is right.
- Tests name the rule they check, e.g. `// [ADMIN-GRAPH-6]`.
- Design history and the reasons behind decisions are in [docs/](docs/), with the decision records
  in [docs/decisions.md](docs/decisions.md). Open work is in [TODO.md](TODO.md).

## Building and testing

You need stable Rust. CI runs these on Linux and Windows, and a pull request should pass all
three:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- Add a test for a bug fix. It should fail without the fix.
- To run a server against scratch data: `cargo run -p mygraph-server -- serve --data-dir <folder>`.
  Don't point development runs at a data folder you care about.
- To build the Docker image: `docker build -t mygraph .`
- On Windows, if linking fails with `cannot open file 'msvcrt.lib'`, build from a developer prompt
  of a Visual Studio installation that has the x64 desktop libraries (for example after running
  `vcvars64.bat` from VS 2022 Build Tools).

## Pull requests

- Keep one change per pull request, and keep the diff to what the change needs.
- Update the README, `skills/mygraph/` or the spec when what users or agents see changes.
- The MCP tools are shaped for agents: compact text results, a hint with every error, and
  idempotent writes. Keep new tools and errors that way.

## License

Contributions are accepted under the project's terms: either [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at the user's option.
