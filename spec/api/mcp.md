# MCP transport

Status: **Draft** · spec 0.1.0-draft · design: [docs/05-architecture.md §8](../../docs/05-architecture.md)

The agent API as Model Context Protocol tools: one tool per operation, with the operation's
request as the tool's arguments. Semantics are those of the REST API ([conventions.md](conventions.md));
the tool definitions are [mcp-tools.json](mcp-tools.json) and the instructions sent to agents are
[server-instructions.md](server-instructions.md).

---

## 1. Protocol

**[MCP-1]** JSON-RPC 2.0 messages. The server implements `initialize`, `ping`, `tools/list`,
`tools/call` and accepts the notification `notifications/initialized`; other methods fail with
JSON-RPC error `-32601`, malformed messages with `-32700` / `-32600`.

**[MCP-2]** `initialize` answers the client's `protocolVersion` when the server supports it
(`2025-11-25`, `2025-06-18`, `2025-03-26`), otherwise its newest, with capabilities
`{"tools": {}}`, `serverInfo` `{"name": "mygraph", "version": …}` and `instructions` = the text
of server-instructions.md. The server keeps no session state: every tool call stands alone.

## 2. Tools

**[MCP-TOOL-1]** `tools/list` returns the tools of mcp-tools.json: `graphs` ([MCP-TOOL-4]) and one
tool per operation — `overview`, `search`, `resolve`, `get`, `write`, `retract`, `merge`,
`neighbors`, `paths`, `find`, `history` — each taking the operation's request members plus `graph`
(optional) and `format` (optional).

**[MCP-TOOL-2]** The graph is `graph` when given; otherwise the only graph the caller's key allows,
if it allows exactly one; otherwise the server's default graph. Roles and graph lists apply as in
[API-AUTH-1].

**[MCP-TOOL-3]** A successful call returns one text content item: the response in the compact
format ([compact-format.md](compact-format.md)), or with `format: "json"` the JSON response. A
failed operation returns a result with `isError: true` and the text
`error <code>: <message>` followed by `hint: …` when there is one ([ERR-4]) and the details as
JSON when present. Hints name tools rather than REST routes (for example `graph.not_found` points to
the `graphs` tool). Unknown tools fail with JSON-RPC error `-32602`.

**[MCP-TOOL-4]** `graphs` with `action` `list` (the default) returns the graphs the caller may use,
each with its status, and `default`: the graph of [MCP-TOOL-2] when it exists, else `null`. With
`action` `create` it creates the graph `name` with optional `policies`, as `POST /admin/graphs`
([ADMIN-GRAPH-2]; admin role), and returns it as `created`. Compact text: `graphs: research (ready,
default) · lab (unloaded)`; `created graph lab · policies {…}`.

## 3. Transports

**[MCP-HTTP-1]** Streamable HTTP at `POST /mcp`: one JSON-RPC message per request, authenticated
like `/v1` ([API-AUTH-1]). Requests answer `200` with `application/json`; notifications and
responses answer `202` with no body. No session ID is issued; `GET /mcp` answers `405`.

**[MCP-STDIO-1]** stdio: one JSON-RPC message per line on stdin, one per line on stdout. The
`mygraph mcp` command either forwards each tool call to a running server's REST API (`--url`,
`--key`), so one process owns the data, or opens a data folder itself (`--data-dir`, only when no
server uses that folder), acting as the actor given by `--actor` (default `mcp`).
