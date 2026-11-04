# Search ranking

Status: **Draft** · spec 0.1.0-draft · used by [api/operations/search.md](../api/operations/search.md)

How `search` finds and orders hits. Name matching (exact, prefix, fuzzy) is fully defined here so
every implementation returns the same name hits; full-text relevance within its tier is left to
the full-text engine, and closeness in meaning to the embedding model; how the two are fused is
defined. Test vectors: `conformance/vectors/search-ranking.json`.

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
| `semantic` | the node's text is close in meaning to the query ([RANK-8]), and the full-text query does not match it | 0.1 + 0.3 × (n − i) / n |

Lengths are in code points. For `text` and `semantic`, `i` is the 0-based position of the node
among the `n` hits of the fused list ([RANK-9]); without a semantic index that list is the text
hits the full-text engine returned, in its relevance order (BM25 for SQLite FTS5).

**[RANK-5]** Prefix matching considers name keys in ascending code point order and stops after
1000 keys; fuzzy matching only runs when `q` has at least 3 code points.

**[RANK-6]** The **full-text query** is built from the raw query text: split it into tokens, each a
maximal run of letters (L*), marks (M*) and numbers (N*); all tokens must match (AND), and the last
token also matches as a prefix of a word. Words of the query and of the texts are compared
lowercased, with diacritics removed and English words reduced to their stem, so other forms of a
word match ("clothing" finds "clothes", "opened" finds "open"); the stored tables use FTS5's
`porter` stemmer ([STORE-FTS-4]), and a derived index may use another English stemmer that agrees
on common words. Prefixes are matched against stems. No tokens → no text hits. Implementations may rank documents that contain the
whole last word before documents that only contain a longer word starting with it. Evidence and links have no
names: evidence is found by full text over `title` and `content` (and by meaning, [RANK-8]),
links by full text only, over their type (with `_` read as a space) and string properties.

## 3. Meaning

**[RANK-8]** A **semantic index** holds, for each node and evidence, embedding vectors of its text
(a node's name, aliases, summary and string properties, as for full text; an evidence's content,
or its title when it has no content) from a local embedding model, and answers which objects are closest in meaning
to a query by cosine similarity. Links are not in it. Long texts are embedded as **passages**: runs
of at most 120 words (split on white space), each ending after the last word in its second half
that ends with `.`, `!` or `?` when there is one; at most 32 passages per object. An object's
similarity is that of its closest passage. Hits are those at least as similar as the
**semantic threshold**, a server setting (the reference server's `search.semantic_threshold`,
default 0.80) that `overview` reports ([OP-OVER-6]); a hit below it neither re-ranks a word match
nor is added after them ([RANK-9]). The model, the threshold's default and the vector index are
implementation-defined (the reference server uses multilingual-e5-small, int8, through ONNX Runtime and a usearch HNSW index),
so semantic hits may differ between implementations; the index is a cache ([STORE-VEC-1]). A
server may turn semantic search off; then there is no semantic index and search matches words
only.

**[RANK-9]** **Fusion.** In each of `nodes` and `evidence`, the full-text hits (in the engine's
relevance order) and the semantic hits (most similar first), each at most 5 × `limit` (and at most
2500), are merged by **reciprocal rank fusion**: an object scores Σ 1 / (60 + r) over the lists it
is in, `r` being its 1-based rank there. Higher scores first; ties by the better (lower) best rank,
then the full-text list before the semantic list. Then every object the full-text query matched
comes before every object found by meaning only, each group keeping that order: meaning re-ranks
word matches and adds hits after them, but never pushes a word match down. The result is the
fused list of [RANK-4]; `matched` is `text` for objects the full-text query matched, else
`semantic`.

## 4. Order

**[RANK-7]** Hits are ordered by score (highest first), then by kind (`node`, `link`, `evidence`),
then by internal number (ascending). Scores in responses are rounded to 3 decimals; ordering uses
the unrounded values.
