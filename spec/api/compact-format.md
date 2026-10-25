# Compact text format

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §6](../../docs/06-agent-api.md)

A line-oriented rendering of operation responses for agents: the same content as the JSON
response with far fewer tokens. MCP tool results use it by default ([MCP-TOOL-3]); REST returns
it for `POST /v1/graphs/{graph}/{operation}` when the request has `Accept: text/plain`.

The format is for reading, not parsing: clients that need structure use JSON. Rules below fix
what is shown; spacing and the exact layout of the examples are informative.

---

## 1. Values and objects

**[CMP-1]** Values: strings in JSON quotes (escapes as in JSON); numbers and booleans as JSON;
`{"$date": d}` and `{"$datetime": t}` as `d` / `t` unquoted; lists as `[a, b]`; `null` and empty lists are
omitted together with their label.

**[CMP-2]** An object's brief form, followed by ` [retracted]` or ` [merged]` when its status is
not active:

| Kind | Form |
|---|---|
| node | `n_3kq Person "Ada Lovelace"` |
| link | `l_9x WROTE_ABOUT n_1 -> n_3` (undirected: `n_1 -- n_3`) |
| evidence | `e_2 text "title"` (or the URI when there is no title; neither: kind only) |
| citation | `c_1 supports e_2 -> n_1` |

**[CMP-3]** Properties as `key=value` separated by spaces, in the response's order.

**[CMP-5]** Lists that come back empty say so (`no hits`, `no path found`), and resolve lines repeat
the candidate's name from the request. `NEW_VOCABULARY` warnings without similar existing names are
collected on one line (`new vocabulary: …`); the others keep a line each.

**[CMP-4]** Every member of the JSON response that a layout below does not show is rendered by the
generic layout: `label: value` for scalars and lists of scalars, nested objects indented by two
spaces, arrays of objects one item per line with `- `. Nothing in the response is dropped
silently; in particular `truncated: true`, `next_cursor`, `more` and warnings always appear.

## 2. Layouts (informative examples)

```
# get — one block per item
n_1 Person "Ada Lovelace" aka "Augusta Ada King" · born=1815-12-10 · support 2 (supports 2)
  summary: "Mathematician"
  links: COLLABORATED_WITH both 1 · WROTE_ABOUT out 1
  -- COLLABORATED_WITH n_2 Person "Charles Babbage" (l_1)
  -> WROTE_ABOUT n_3 Machine "Analytical Engine" (l_2)
  evidence: e_2 text supports (c_1) "quote…"
# search
0.85 n_3kr Person "Charles Babbage" (name:prefix)
0.40 e_2 text "Ada Lovelace" (text) …notes on Babbage's «Analytical Engine»…
# resolve — one line per candidate entity, alternatives indented
1 match n_3kr Person "Charles Babbage" 0.80 (name fuzzy 0.50; born matches)
2 none
# write
batch b_8k3 · created node 1 · refs @men=n_51a
warning NEW_VOCABULARY: …
# neighbors
n_3kq Person "Ada Lovelace" (hop 0)
l_9x COLLABORATED_WITH n_3kq -- n_3kr
# paths — one line per path
2: n_1 "Ada Lovelace" -[COLLABORATED_WITH]- n_2 "Charles Babbage" -[KNOWS]-> n_3 "…"
# find
n_3kq Person "Ada Lovelace" · prop:born=1815-12-10
{prop:country="Britain"} count=12 avg(degree(KNOWS))=3.5
# history
88121 2026-10-24T09:12:00.000Z claude-research b_8k2 update n_3kq {"before":…,"after":…}
```

`overview` lists counts, then node and link types as `Type count` separated by ` · `, policies,
recent batches one per line, conventions, and with `type` the type's details.
