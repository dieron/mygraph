# Search ranking

Status: **Draft** · spec 0.1.0-draft · used by [api/operations/search.md](../api/operations/search.md)

How `search` finds and orders hits. Name matching (exact, prefix, fuzzy) is fully defined here so
every implementation returns the same name hits; full-text relevance within its tier is left to
the full-text engine. Test vectors: `conformance/vectors/search-ranking.json`.

---

## 1. Name keys and trigrams

**[RANK-1]** Name matching compares **name keys** ([NORM-NAME-7]): the query's name key against the
name key of each active node's `name` and of each of its `aliases`.

**[RANK-2]** The **trigrams** of a name key: split it on U+0020 into words (empty words dropped);
for each word `w`, take every run of 3 consecutive code points of `"␠␠" + w + "␠"` (␠ = U+0020).
The trigram set is the union over all words, duplicates removed. Example: `ada` →
`{"␠␠a", "␠ad", "ada", "da␠"}`.

**[RANK-3]** **Trigram similarity** of two name keys is `|A ∩ B| / |A ∪ B|` over their trigram sets
(0 when both are empty).

## 2. Match kinds

**[RANK-4]** A node matches the query `q` (its name key) in one or more of these ways; each way has
a score, and the node's score is the highest of them:

| `matched` | Condition (on the name, or on any alias) | Score |
|---|---|---|
| `name:exact` | name key = `q` | 1.0 |
| `alias:exact` | an alias key = `q` | 0.95 |
| `name:prefix` | name key starts with `q` and is longer | 0.8 + 0.1 × len(q) / len(key) |
| `alias:prefix` | an alias key starts with `q` and is longer | 0.75 + 0.1 × len(q) / len(key) |
| `name:fuzzy` / `alias:fuzzy` | trigram similarity with `q` ≥ 0.3 | 0.4 + 0.3 × similarity |
| `text` | the full-text query ([RANK-6]) matches the node's name, aliases, summary or string properties | 0.1 + 0.3 × (n − i) / n |

Lengths are in code points. For `text`, `i` is the 0-based position of the node among the `n`
text hits the full-text engine returned, in its relevance order (BM25 for SQLite FTS5).

**[RANK-5]** Prefix matching considers name keys in ascending code point order and stops after
1000 keys; fuzzy matching only runs when `q` has at least 3 code points.

**[RANK-6]** The **full-text query** is built from the raw query text: split it into tokens, each a
maximal run of letters (L*), marks (M*) and numbers (N*); all tokens must match (AND), and the last
token also matches as a prefix of a word. No tokens → no text hits. Implementations may rank documents that contain the
whole last word before documents that only contain a longer word starting with it. Evidence and links are found
only by full text: evidence by `title` and `content`, links by their type (with `_` read as a
space) and string properties.

## 3. Order

**[RANK-7]** Hits are ordered by score (highest first), then by kind (`node`, `link`, `evidence`),
then by internal number (ascending). Scores in responses are rounded to 3 decimals; ordering uses
the unrounded values.
