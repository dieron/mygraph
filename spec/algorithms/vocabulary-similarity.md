# Vocabulary similarity

Status: **Draft** · spec 0.1.0-draft · design: [docs/04-data-model.md §9](../../docs/04-data-model.md)

When a write introduces a node type, link type or property key that the graph has not used before,
the `NEW_VOCABULARY` warning lists similar existing items ([OP-WRITE-5]), so an agent notices
`WORKS_FOR` next to an established `WORKS_AT` and can correct itself. The comparison is lexical
only; meaning-based similarity is phase 2. All arithmetic is on integers, so every implementation
produces the same lists. Test vectors: `conformance/vectors/vocabulary-similarity.json`.

---

## 1. Comparison form

**[VSIM-1]** A vocabulary item is compared through its **words**: split it by [VOCAB-2]
([model/normalization.md](../model/normalization.md)), then map every code point of every word
with its simple lowercase mapping ([VOCAB-3]). `joined(x)` is the concatenation of the words of
`x` without separators; `words(x)` is the set of distinct words.

Examples: `WORKS_AT` → words `works`, `at`, joined `worksat`; `ResearchPaper` → `research`,
`paper`, joined `researchpaper`; `born_on` → `born`, `on`, joined `bornon`.

## 2. Score

**[VSIM-2]** The score of two items `a`, `b` is an integer from 0 to 1000, the largest of:

1. **Characters:** `floor(1000 × (L − d) / L)`, where `d` is the Levenshtein distance between
   `joined(a)` and `joined(b)` counted in code points (insertion, deletion and substitution each
   cost 1) and `L` is the length in code points of the longer one; 0 when `L` is 0.
2. **Words:** `floor(1000 × |words(a) ∩ words(b)| / |words(a) ∪ words(b)|)`; 0 when both sets
   are empty.
3. **Prefix:** 700 when the shorter of `joined(a)`, `joined(b)` has at least 3 code points and is
   a prefix of the longer one (catches abbreviations such as `Org` → `Organization`); otherwise 0.

## 3. Selection

**[VSIM-3]** Candidates are the items of the same kind (node types, link types, or property keys)
whose usage count is above zero at the moment the new item is processed, earlier items of the same
batch included, except an item exactly equal to the new one. The usage count is the number of
objects of any status that use the item: nodes of a node type, links of a link type, nodes and
links having a property key. A candidate is **similar** when its score is at least 600.

**[VSIM-4]** Similar candidates are ordered by score (highest first), then usage count (highest
first), then name (ascending code point order). The first 5 are reported.

**[VSIM-5]** The warning's `details.similar` is a list of `{"name": <item>, "count": <usage
count>}` in that order; an empty list when nothing is similar.
