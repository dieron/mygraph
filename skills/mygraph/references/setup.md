# Connecting a client to MyGraph

MyGraph runs as a server, on the same machine (default `http://127.0.0.1:7420`) or on another one
(for example in Docker: `http://graphhost:7420`; below, `URL` stands for that address). Clients
reach it through MCP in one of two ways, and agents with a shell can also use the REST API:

- **stdio bridge** — the client starts `mygraph mcp --url URL --graph <default graph>`;
  works in every client that supports local MCP servers. Add `--key <key>` (or set `MYGRAPH_KEY`)
  when the server has API keys. An `https://` URL works too; for a self-signed or private-CA
  certificate add `--ca-cert <file.pem>` (or set `MYGRAPH_CA_CERT`).
- **Streamable HTTP** — the client connects to `URL/mcp` directly (header
  `Authorization: Bearer <key>` when keys are configured).

No server yet? The installers set one up on this machine and start it at login:
`curl -fsSL https://raw.githubusercontent.com/dieron/mygraph/main/install.sh | sh` (Linux, macOS)
or `irm https://raw.githubusercontent.com/dieron/mygraph/main/install.ps1 | iex` (Windows).

Below, `MYGRAPH` stands for the full path of the `mygraph` program (after the Windows installer:
`%LOCALAPPDATA%\MyGraph\bin\mygraph.exe`, written out in full; in JSON write backslashes as `\\`;
after `install.sh`: `~/.local/share/mygraph/bin/mygraph`).

## Claude Code

The plugin connects over Streamable HTTP and installs this skill:

```bash
claude plugin marketplace add dieron/mygraph
```

```bash
claude plugin install mygraph@mygraph
```

Its options (`/plugin`, or `--config server_url=URL --config api_key=<key>` at install) are the
server address, default `http://127.0.0.1:7420`, and the API key. Or connect by hand; `-s user`
makes the tools available in every project (`-s local` only in the current folder):

```bash
claude mcp add mygraph -s user -e MYGRAPH_KEY=<key> -- MYGRAPH mcp --url URL --graph research
```

For the REST fallback (this skill's `references/rest.md`), give Claude Code's shell the address and
key in `~/.claude/settings.json`, which is personal and not part of any repository:

```json
{ "env": { "MYGRAPH_URL": "URL", "MYGRAPH_KEY": "<key>" } }
```

## Claude desktop app (chat)

`claude_desktop_config.json` (Settings → Developer → Edit Config), then fully quit and reopen:

```json
{ "mcpServers": { "mygraph": { "command": "MYGRAPH", "args": ["mcp", "--url", "URL", "--graph", "research"],
                               "env": { "MYGRAPH_KEY": "<key>" } } } }
```

The app may rewrite this file; if the tools disappear after an update, check the entry is still
there.

## The Claude API and the OpenAI API

A program can give the model MyGraph's tools when the server is reachable from the internet over
HTTPS (its public listener, `[oauth] listen`, serves `/mcp`). Use a key of its own, limited with
each graph's access.
- Claude API (Messages, beta `mcp-client-2025-11-20`): `"mcp_servers": [{"type": "url", "url":
  "https://<public host>/mcp", "name": "mygraph", "authorization_token": "<key>"}]` and
  `"tools": [{"type": "mcp_toolset", "mcp_server_name": "mygraph"}]`.
- OpenAI Responses API: `"tools": [{"type": "mcp", "server_label": "mygraph", "server_url":
  "https://<public host>/mcp", "authorization": "<key>", "require_approval": "never"}]`.

## claude.ai, Claude Desktop and the phone apps (connector)

When the server has OAuth on (`[oauth] public_url`) and is reachable from the internet over
HTTPS (best through its public listener, `[oauth] listen`, which serves nothing else): Settings → Connectors → Add custom connector, URL `https://<public host>/mcp`. MyGraph's
sign-in page asks for an API key; **Read only** limits the connector to reading. One connector
works in every Claude app on that account. If it stops working, remove and add it again to sign
in anew.

## Cursor

`~/.cursor/mcp.json` (all projects) or `.cursor/mcp.json` (one project):

```json
{ "mcpServers": { "mygraph": { "command": "MYGRAPH", "args": ["mcp", "--url", "URL", "--graph", "research"] } } }
```

## VS Code with GitHub Copilot (agent mode)

`.vscode/mcp.json` in a workspace, or the user-level MCP configuration (command "MCP: Open User
Configuration"):

```json
{ "servers": { "mygraph": { "type": "stdio", "command": "MYGRAPH", "args": ["mcp", "--url", "URL", "--graph", "research"] } } }
```

## OpenAI Codex

`~/.codex/config.toml`:

```toml
[mcp_servers.mygraph]
command = "MYGRAPH"
args = ["mcp", "--url", "URL", "--graph", "research"]
```

## Installing this skill

The skill is one folder in the Agent Skills format (`SKILL.md` + `references/`). Each tool looks for
it in its own place:

| Tool | Personal folder | Per-project folder |
|---|---|---|
| Claude Code | `~/.claude/skills/mygraph/` | `.claude/skills/mygraph/` |
| Codex, Cursor, GitHub Copilot | `~/.agents/skills/mygraph/` | `.agents/skills/mygraph/` |
| Claude desktop / claude.ai | upload the zipped folder (Settings → Capabilities → Skills) | — |

From the MyGraph repository, `deploy/install-skill.ps1` copies it to both personal folders and
builds the zip for upload.

## Checking the connection

Call the `graphs` tool: it lists the graphs and the default one. `error … cannot reach the MyGraph
server` means the server is not running or the address is wrong (or, with `invalid peer
certificate`, that its certificate is not trusted: pass `--ca-cert`): check `curl URL/health` (on
Windows a local server starts at logon as the scheduled task "MyGraph server"; a Docker server
with `docker compose ps`). Over REST, `curl -s "$MYGRAPH_URL/v1/graphs" -H "Authorization: Bearer
$MYGRAPH_KEY"` does the same check.
