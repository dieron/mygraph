# Operation: `resolve`

Status: **Draft** · spec 0.1.0-draft · design: [docs/06-agent-api.md §4.3](../../../docs/06-agent-api.md)

Entity resolution for many described entities in one call, typically right before a `write`: for
each one, does a node for it already exist? Scoring and verdicts:
[algorithms/resolve-scoring.md](../../algorithms/resolve-scoring.md).

---

## 1. Request

```json
{ "candidates": [
    { "name": "C. Babbage", "type": "Person", "hints": { "born": {"$date": "1791"} } },
    { "name": "Analytical engine" },
    { "name": "Luigi Menabrea", "type": "Person" } ] }
```

| Field | Meaning |
|---|---|
| `candidates` | 1–100 described entities |
| `candidates[].name` | Required; a node name ([NORM-NAME]) |
| `candidates[].type` | Optional node type |
| `candidates[].hints` | Optional map of property key → value in write form ([VAL]); keys canonicalized as in a write |

**[OP-RESOLVE-1]** An invalid field fails the whole call with `resolve.invalid_field`, naming the
candidate index and field.

## 2. Response

```json
{ "results": [
    { "verdict": "match",
      "best": { "id": "n_3kr", "type": "Person", "name": "Charles Babbage", "score": 0.8,
                "why": ["name fuzzy 0.50", "born matches"] } },
    { "verdict": "ambiguous",
      "best": { "id": "n_7", "type": "Machine", "name": "Analytical Engine", "score": 1.0, "why": ["name exact"] },
      "candidates": [ { "id": "n_7", "…": "…" }, { "id": "n_9", "…": "…" } ] },
    { "verdict": "none" } ] }
```

**[OP-RESOLVE-2]** `results` has one entry per candidate, in request order. `best` is the first
plausible node ([RES-7]), absent for `none`. For `ambiguous`, `candidates` lists up to 5 plausible
nodes in order, `best` first. Scores are rounded to 3 decimals.
