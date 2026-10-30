# Operation: `find`

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §4.10](../../../docs/06-agent-api.md)

Structured filters, computed values, group-by, aggregates and sorting over nodes, links, evidence
or citations — the API's answer to "no query language" (R9). This file also holds the rules the
values spec points to for filters on partial dates and for missing values.

---

## 1. Request

```json
{ "target": "nodes",
  "where": { "all": [ { "field": "type", "op": "eq", "value": "Person" },
                      { "field": "prop:born", "op": "lt", "value": {"$date": "1850"} },
                      { "field": "prop:country", "op": "in", "value": ["Britain", "Ireland"] } ] },
  "group_by": ["prop:country", "prop:born.decade"],
  "aggregate": [ {"fn": "count"}, {"fn": "avg", "of": "degree(KNOWS)"} ],
  "sort": [ {"by": "count", "order": "desc"} ],
  "limit": 20 }
```

| Field | Meaning | Default |
|---|---|---|
| `target` | `nodes`, `links`, `evidence`, `citations` | required |
| `where` | A condition ([OP-FIND-3]) | everything |
| `include_retracted` | Also consider retracted objects (never merged nodes) | `false` |
| `fields` | Up to 10 field expressions added to each row as `values` | — |
| `group_by` | Up to 3 field expressions ([OP-FIND-5]) | — |
| `aggregate` | Up to 10 `{fn, of}` ([OP-FIND-6]) | `[{"fn": "count"}]` with `group_by` |
| `sort` | Up to 3 `{by, order}`; `order` `asc` (default) or `desc` ([OP-FIND-7]) | — |
| `limit` | 1–1000 rows or groups | 20 |
| `cursor` | `next_cursor` of a previous response with the same request | — |

**[OP-FIND-1]** Invalid fields, unknown field expressions or operators fail with
`find.invalid_field`.

## 2. Field expressions

**[OP-FIND-2]** A field expression names a value of an object; it may be missing.

| Target | Expressions |
|---|---|
| all | `id` (the object's ID string), `status`, `created_at`, `updated_at` (`datetime`), `prop:<key>` (nodes and links; key checked as in a write, [VOCAB-1]), `evidence_count` (nodes and links: distinct active evidence, [OP-GET-5]) |
| nodes | `type`, `name`, `labels` (list), `degree`, `degree(<LINK_TYPE>)`, `degree(<LINK_TYPE>, <out\|in\|both>)` — number of active links, an undirected link counting for every direction, a self-link once |
| links | `type`, `from`, `to` (node IDs), `from.type`, `to.type`, `directed` |
| evidence | `kind`, `title`, `uri`, `author`, `publisher`, `language`, `source_date`, `captured_at`, `citations` (number of active citations) |
| citations | `evidence`, `target` (IDs), `target_kind` (`node`/`link`), `prop`, `role`, `confidence`, `evidence.kind` |

Suffixes on a `date` or `datetime` value (UTC): `.year`, `.month` (missing for a year-precision
date), `.decade` (`floor(year / 10) × 10`), `.century` (`floor(year / 100) × 100`), all `int`.
`bucket(<expr>, <width>)` maps a number `x` to `floor(x / width) × width` (`width` > 0).
`degree(...)` link types, and type names compared with `type` by `eq`, `ne`, `in`, `not_in`, are checked as in a write ([VOCAB-1]).

## 3. Conditions

**[OP-FIND-3]** A condition is `{"field": <expr>, "op": <op>, "value": <value>}`, or a combinator
`{"all": [conditions]}`, `{"any": [conditions]}`, `{"not": condition}`, or
`{"linked": {"link_type": …, "direction": …, "node": <node reference>}}` (nodes only: the node has
an active link of that type and direction, as in [TRAV-1], to the referenced node). Values are in
write form ([VAL]); `in`, `not_in` take a list, `between` a list `[low, high]`; `exists` and
`missing` take no value.

**[OP-FIND-4]** Operators on a field value `v`. `ne` and `not_in` are always the negations of
`eq` and `in`. When `v` is a list, every other comparison holds when it holds for some element, so
`ne` / `not_in` hold when no element is equal / in the operand list (`speaks ne "en"` fails for
`["en", "si"]`). `exists` / `missing` look at the field itself.

| Op | Holds when |
|---|---|
| `exists` / `missing` | the field has a value / has none |
| `eq`, `ne` | equal ([VAL-EQ-1]; temporal values: below) / not `eq` |
| `lt`, `lte`, `gt`, `gte` | by [VAL-ORDER-1] within the same kind rank; temporal: below |
| `between` | `gte low` and `lte high` |
| `in`, `not_in` | `eq` some element / no element of the list |
| `contains`, `starts_with` | strings: the name key ([NORM-NAME-7]) of `v` contains / starts with the name key of the operand |

Temporal values are compared as intervals ([VAL-DATE-5]; a date-time is an instant): with `v` =
[s, e] and the operand [S, E]: `eq` ⇔ S ≤ s and e ≤ E (inside); `lt` ⇔ e < S; `lte` ⇔ e ≤ E;
`gt` ⇔ s > E; `gte` ⇔ s ≥ S. So `born lt 1850` holds for `1849-12` and fails for `1850-03`, and
`born eq 1815` holds for `1815-12-10`.

A missing field, or a value of another kind rank than the operand, satisfies only `missing`, `ne`
and `not_in`.

## 4. Groups and aggregates

**[OP-FIND-5]** With `group_by` or `aggregate`, matching objects are grouped by the values of the
`group_by` expressions (a missing value forms its own group, shown as `null`; a list value is one
group key, not one group per element). Without `group_by`, all matching objects form one group,
also when no object matches: that group has count 0 and its other aggregates follow
[OP-FIND-6] (as SQL and Cypher aggregate without grouping). With `group_by`, no matching objects
means no groups.

**[OP-FIND-6]** Aggregates over a group, skipping objects where `of` is missing:

| `fn` | Result | Name in the output |
|---|---|---|
| `count` (no `of`) | number of objects | `count` |
| `count` with `of` | objects where `of` exists | `count(<of>)` |
| `count_distinct` | number of distinct values ([VAL-EQ-1]) | `count_distinct(<of>)` |
| `sum`, `avg` | numbers only (others skipped); `avg` is a float, `sum` an int when all are ints | `sum(<of>)`, `avg(<of>)` |
| `min`, `max` | by [VAL-ORDER-1] | `min(<of>)`, `max(<of>)` |
| `median` | numbers only; the mean of the two middle values for an even count | `median(<of>)` |

An aggregate over no values is `null`, except the counts (`count`, `count` with `of`,
`count_distinct`), which are 0.

## 5. Order, paging, response

**[OP-FIND-7]** Rows are ordered by the `sort` keys (field expressions), then by internal number;
groups by the `sort` keys (aggregate names or `group_by` expressions), then by `count` descending,
then by the group key ([VAL-ORDER-1], element by element). Missing values sort last in both
orders.

**[OP-FIND-8]** The response has `total` (matching objects, or groups), up to `limit` `rows` or
`groups`, and `next_cursor` when more follow. A cursor is only valid for the same request; results
are consistent across pages only when the graph does not change in between.

```json
{ "rows": [ { "id": "n_3kq", "type": "Person", "name": "Ada Lovelace", "values": {"prop:born": {"$date": "1815-12-10"}} } ],
  "total": 1, "next_cursor": null }
{ "groups": [ { "key": {"prop:country": "Britain", "prop:born.decade": 1810}, "count": 12, "avg(degree(KNOWS))": 3.5 } ],
  "total": 31, "next_cursor": "20" }
```

Rows show the brief form of [OP-GET-3] plus `values`.
