# Traversal

Status: **Draft** · spec 0.1.0-draft · used by [neighbors](../api/operations/neighbors.md) and
[paths](../api/operations/paths.md)

Which links a traversal may follow, and in which order, so `neighbors` and `paths` return the same
subgraphs and paths in every implementation.

---

## 1. Following a link

**[TRAV-1]** From node `n`, a link `l` can be followed to its other end when all hold:
- `l` is active;
- `l`'s type is in the step's `link_types` (canonicalized as in a write, [VOCAB-1]), or no
  `link_types` are given;
- direction: `out` follows directed links from `n` (`n` = `from`), `in` follows directed links
  into `n` (`n` = `to`), `both` follows either; undirected links are followed in every direction;
- with `as_of` ([TRAV-2]), `l` is valid at that time.

A link from a node to itself leads back to the same node.

**[TRAV-2]** `as_of` is a `date` or `datetime` value. A link is valid at `as_of` when its
validity interval overlaps the interval `as_of` denotes (a partial date denotes its whole year or
month): `valid_from` absent or its start ≤ the end of `as_of`, and `valid_to` absent or its end ≥
the start of `as_of` ([VAL-DATE]).

**[TRAV-3]** **Link order**: links of one node are taken by link type (code point order), then
internal number. **Node order**: nodes of one frontier are taken by internal number.

## 2. Path order

**[TRAV-4]** A path is an alternating sequence `n0, l1, n1, …, lk, nk` without repeated nodes;
its length is `k`. Paths are ordered by length, then position by position: first by the internal
number of `n1`, then of `l1`, then of `n2`, then of `l2`, and so on.
