# Connecting a client to MyGraph

MyGraph runs as a local server (default `http://127.0.0.1:7420`). Clients reach it through MCP in
one of two ways:

- **stdio bridge** — the client starts `mygraph mcp --url http://127.0.0.1:7420 --graph <default graph>`;
  works in every client that supports local MCP servers. Add `--key <key>` (or set `MYGRAPH_KEY`)
  when the server has API keys.
- **Streamable HTTP** — the client connects to `http://127.0.0.1:7420/mcp` directly (header
  `Authorization: Bearer <key>` when keys are configured).

Below, `MYGRAPH` stands for the full path of `mygraph.exe` (after `deploy/windows/deploy.ps1`:
`%LOCALAPPDATA%\MyGraph\bin\mygraph.exe`, written out in full; in JSON write backslashes as `\\`).

## Claude Code

```bash
claude mcp add mygraph -s user -- MYGRAPH mcp --url http://127.0.0.1:7420 --graph research
```

## Claude desktop app (chat)

`claude_desktop_config.json` (Settings → Developer → Edit Config), then fully quit and reopen:

```json
{ "mcpServers": { "mygraph": { "command": "MYGRAPH", "args": ["mcp", "--url", "http://127.0.0.1:7420", "--graph", "research"] } } }
```

## Cursor

`~/.cursor/mcp.json` (all projects) or `.cursor/mcp.json` (one project):

```json
{ "mcpServers": { "mygraph": { "command": "MYGRAPH", "args": ["mcp", "--url", "http://127.0.0.1:7420", "--graph", "research"] } } }
```

## VS Code with GitHub Copilot (agent mode)

`.vscode/mcp.json` in a workspace, or the user-level MCP configuration (command "MCP: Open User
Configuration"):

```json
{ "servers": { "mygraph": { "type": "stdio", "command": "MYGRAPH", "args": ["mcp", "--url", "http://127.0.0.1:7420", "--graph", "research"] } } }
```

## OpenAI Codex

`~/.codex/config.toml`:

```toml
[mcp_servers.mygraph]
command = "MYGRAPH"
args = ["mcp", "--url", "http://127.0.0.1:7420", "--graph", "research"]
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
server` means the server is not running (on Windows it starts at logon as the scheduled task
"MyGraph server").
