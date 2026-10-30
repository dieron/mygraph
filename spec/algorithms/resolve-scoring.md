# Resolve scoring

Status: **Draft** · spec 0.1.0-draft · used by [api/operations/resolve.md](../api/operations/resolve.md)

How `resolve` scores existing nodes against a described entity and turns the scores into a
verdict. Everything here is defined exactly, so implementations agree on verdicts. Name keys and
trigram similarity: [search-ranking.md](search-ranking.md) §1. Test vectors:
`conformance/vectors/resolve-scoring.json`.

---

## 1. Candidates

**[RES-1]** The candidate nodes for a described entity with name key `q` are the active nodes that
match `q` by `name:exact`, `alias:exact` or trigram similarity ≥ 0.4 with their name or an alias
([RANK-3]; no prefix or full-text matching). At most 200 are scored: those with the highest
**name score** ([RES-2]), ties by internal number (ascending).

## 2. Score

**[RES-2]** **Name score**: 1.0 when the name key equals `q`; else 0.95 when an alias key equals
`q`; else `min(0.9, s + 0.2)`, where `s` is the highest trigram similarity of `q` with the name
key or any alias key (a typo in a full name scores about 0.9; an initial instead of a first name
about 0.7).

**[RES-3]** **Type factor**: 1.0 when the entity gives no `type` or the node has that type
(checked as in a write, [VOCAB-1]); 0.7 otherwise.

**[RES-4]** **Hints**: for each hint property the node also has, the hint **agrees** or
**conflicts** ([RES-5]); properties the node lacks count for neither.

**[RES-5]** A hint agrees with a stored value when:
- both are `date`: the year is equal, and the month (day) is equal wherever both give a month
  (day) — `1791` agrees with `1791-12-26`, `1791-11` does not;
- both are `string`: their name keys ([NORM-NAME-7]) are equal;
- otherwise: they are equal by [VAL-EQ] (`values_equal`).

Otherwise it conflicts.

**[RES-6]** **Score** = `name score × type factor + 0.1 × agreeing − 0.2 × conflicting`, clamped
to [0, 1].

## 3. Verdict

**[RES-7]** Nodes scoring at least 0.6 are **plausible**, ordered by score (highest first), then
internal number. The verdict is:
- `none` when no node is plausible;
- `match` when the first scores at least 0.8 and no other plausible node scores within 0.1 of
  it (second score < first − 0.1);
- `ambiguous` otherwise.

**[RES-8]** `why` lists the reasons for a node's score in this order: the name part (`name
exact`, `alias exact`, or `name fuzzy <s>` / `alias fuzzy <s>` with `s` to 2 decimals), then
`type differs (<node type>)` when the factor is 0.7, then `<key> matches` / `<key> differs` for
each hint in key order.

Comparisons of scores use exact arithmetic on the values above (implementations using binary
floating point must compare with a tolerance of 1e-9).
