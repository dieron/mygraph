# Conformance suite

Language-neutral tests that any MyGraph implementation must pass. The specification is in
[`../spec/`](../spec/README.md); this folder turns its rules into data.

| Folder | Contents | Status |
|---|---|---|
| `vectors/` | Test vectors for pure functions (IDs, normalization, values) | M0 — written |
| `cases/` | Black-box HTTP cases per API operation (setup → request → expected) | Planned (M1–M3) |
| `bench/` | Benchmark dataset definition and scenarios with performance targets | Planned (M4) |
| `agent-evals/` | Agent tasks with expected facts, for usability regression | Planned (M4) |

## 1. Using the vectors

An implementation's unit tests load the files in `vectors/` **from this folder** (do not copy them
into the implementation), call the named function for each case, and compare the result. Every
case passes or the implementation is not conformant. The vectors are normative
([spec/README.md §1](../spec/README.md#1-normative-language)).

## 2. Vector file format (`mygraph-vectors/1`)

```json
{
  "format": "mygraph-vectors/1",
  "spec_version": "0.1.0-draft",
  "spec": "spec/model/identifiers.md",
  "groups": [
    {
      "function": "decode_id",
      "rules": ["ID-CODEC-3", "ID-CODEC-4"],
      "description": "…",
      "cases": [
        { "input": "n_3kq", "output": { "kind": "node", "number": 3703 } },
        { "input": "n_01",  "error": "id.non_canonical", "note": "leading zero" }
      ]
    }
  ]
}
```

A case has exactly one input field and exactly one of `output` / `error`:

| Field | Meaning |
|---|---|
| `input` | The input value as JSON |
| `input_repeat` | A string input built as `prefix + unit × count` (`prefix` optional), for long inputs. For `parse_value` it is the string *value*, not JSON text |
| `input_json` | The input as raw JSON **text** (a string to be parsed by the implementation's JSON parser), used where the lexical form matters, e.g. `1815` vs `1815.0` |
| `input_list_repeat` | A JSON array built from `element_json` repeated `count` times |
| `output` | The expected result |
| `output_repeat` | Expected string built like `input_repeat` |
| `value_repeat`, `value_list_repeat` | Inside an `output` object: the `value` member built by repetition |
| `error` | The expected reason code (the function must fail with exactly this code) |
| `note` | Informative explanation |

**Comparison rules.** Strings: exact, code point by code point. Integers: exact; they are 64-bit
signed, so parse vector files with a JSON reader that keeps 64-bit integers exact. Floats (values
of kind `float`): numeric equality of binary64 values; `0.0` expected from `-0.0` input also
requires a positive sign bit. Objects and arrays: member-wise and element-wise with the same rules.

Non-ASCII characters in vector files are written as `\u` escapes so that invisible and combining
characters are unambiguous; `note` shows the readable form.

## 3. Function catalogue

Functions are named by what they compute; implementations map them to their own API.

| File | Function | Input | Output |
|---|---|---|---|
| [identifiers.json](vectors/identifiers.json) | `encode_id` | `{kind, number}` | ID string |
| | `decode_id` | ID string | `{kind, number}` |
| | `decode_id_of_kind` | `{id, expect_kind}` | `{kind, number}` |
| | `validate_key` | string | the key in NFC |
| | `validate_batch_ref` | string | the same string |
| | `validate_graph_name` | string | the same string |
| | `validate_actor_name` | string | the same string |
| [name-normalization.json](vectors/name-normalization.json) | `name_key` | string | name key |
| [vocabulary.json](vectors/vocabulary.json) | `canonical_node_type`, `canonical_link_type`, `canonical_prop_key` | string | canonical form |
| | `validate_vocabulary_verbatim` | `{kind, value}` (kind: `node_type`, `link_type`, `prop_key`) | the value in NFC |
| [values.json](vectors/values.json) | `parse_value` | JSON text of a property value | `{kind, value}` (+ `precision` for dates) or `{remove: true}` |
| | `compare_values` | JSON text of `[a, b]` | `-1`, `0` or `1` |
| | `values_equal` | JSON text of `[a, b]` | `true` / `false` |

Object kinds in `encode_id` / `decode_id`: `node`, `link`, `evidence`, `citation`, `batch`.

## 4. Coverage

Each group lists the rule IDs it checks in `rules`. A spec-coverage report (planned with the
runner in M0/M1) lists testable rules that no vector or case cites.
