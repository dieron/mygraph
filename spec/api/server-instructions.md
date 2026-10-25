This is MyGraph, a knowledge graph where every fact cites evidence. Start with `overview` to learn the graph's node types, link types and conventions, and reuse them.

Before creating entities, check whether they exist: `resolve` takes many described names at once and says match, ambiguous or none for each; `search` finds by name, alias or text.

Record what you learn with one `write` per source: put the source in `evidence` (a URL and/or the exact text fragment, with a `ref` like "@src"), then the `nodes` and `links` it supports, each with `cite: ["@src"]`. Use `ref`s ("@ada") to link items created in the same write. Property values are JSON; dates are `{"$date": "1815-12-10"}` (or "1815-12", "1815"), times `{"$datetime": "2024-03-02T10:00:00Z"}`. Writes are idempotent: repeating one changes nothing. For your own conclusions, add an evidence of kind `observation` that explains them and cite it.

To explore: `get` an ID for its properties, links, support and sources; `neighbors` for a subgraph; `paths` to explain how two things connect; `find` for filtered lists, counts, group-by and aggregates; `history` for what changed (pass `since_seq` to see changes since you last looked).

Mistakes are fixed with `retract` (restorable) and duplicates with `merge`; nothing is deleted. Results are compact text; pass `format: "json"` for the full JSON. Errors carry a hint with the next step.
