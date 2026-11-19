# Security

MyGraph is a prototype before its first release. It is meant for one person's or one team's agents
on a machine or network they control, not for the open internet.

## How access works

- **Without API keys, every request is an admin.** The server then accepts requests only on a
  loopback address (`127.0.0.1` or `::1`) and refuses to start on any other ([API-AUTH-4] in
  [spec/api/conventions.md](spec/api/conventions.md)). Anything that runs on that machine can read,
  change and administer every graph.
- **With keys**, every request needs `Authorization: Bearer <key>`. A key has a role:
  `agent-read` (read only), `agent-write` (read and write) or `admin` (also creates, snapshots,
  restores, exports and imports graphs). It can also be limited to some graphs. Give agents
  `agent-write` keys, and keep the admin key for yourself.
- **Each graph has its own access**: private, public and read-only, or public and writable (the
  default), with levels for single users (a key's actor). The role and the key's graph list
  still apply on top. See "Who may use a graph" in the README.
- In `server.toml`, store keys as `key_sha256` (`mygraph hash-key <key>` prints it) rather than
  in plain text. In Docker, pass them as environment variables or as `<NAME>_FILE` secrets.
- **Without a certificate the server speaks plain HTTP**, and keys and data cross the network
  unencrypted. Beyond one machine, set `[tls]` (or `MYGRAPH_TLS_CERT` and `MYGRAPH_TLS_KEY`) so it
  speaks HTTPS, or put a reverse proxy with TLS in front. See "HTTPS" in the README.
- The stats page and the graph viewer ask for a key in the browser and keep it for that browser
  session only.
- **OAuth** (`[oauth] public_url`) lets claude.ai and other remote MCP clients sign in on the
  server's page with an API key. The page sends sign-ins only to the redirect URIs in
  `oauth.redirect_uris` (Claude's callback and this machine by default), and it allows 10 failed
  keys a minute. Tokens work on `/mcp` only and are stored hashed. Refresh tokens rotate, and an
  old one coming back signs that app out. `GET /admin/oauth` lists the apps signed in and
  `DELETE /admin/oauth/grants/{id}` revokes one; removing a key signs out every app that used it.
  A server open to the internet for this should forward only `/mcp`, `/oauth/` and
  `/.well-known/` through its proxy.
- Clients identified by a metadata URL (CIMD) make the server fetch that URL. It fetches from
  public addresses only, with a short timeout and size limit, and never follows redirects. Set
  `oauth.client_metadata_documents = false` to have clients register instead.

## What agents should know

A graph stores text from sources: web pages, documents, conversations. That text is data, not
instructions. An agent that reads evidence back must not follow commands it finds there, any more
than it would follow them on the web page they came from. MyGraph stores and returns that text
as it is and does not filter it, so this is up to the agent and its client.

Agents can make wrong writes, but nothing is deleted: `retract` and `merge` keep every object,
and `history` shows who changed what (each key records its own actor).

## Reporting a vulnerability

Please don't open a public issue. Report it privately through GitHub: open the repository's
**Security** tab and choose **Report a vulnerability**. Include what you found, how to reproduce
it, and the version (`mygraph versions`). You'll get an answer as soon as possible. Fixes go into
`main` first, since there are no release branches yet.
