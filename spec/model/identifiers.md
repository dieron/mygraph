# Identifiers

Status: **Draft** · spec 0.1.0-draft · vectors: [conformance/vectors/identifiers.json](../../conformance/vectors/identifiers.json)

Covers object IDs, natural keys, batch references, graph names, actor names and change-log
sequence numbers. How references are used inside API requests is specified in `api/conventions.md`
(planned, M3).

---

## 1. Object IDs

An object ID is a short, case-insensitive, prefixed string that encodes the object's internal
number. Example: `n_3kq` is node number 3,703 (3·32² + 19·32 + 23).

**[ID-FORM-1]** An object ID has the form `<prefix>_<body>`, where `<prefix>` names the object
kind:

| Prefix | Object kind |
|---|---|
| `n` | node |
| `l` | link |
| `e` | evidence |
| `c` | citation |
| `b` | batch (one atomic write or admin operation) |

**[ID-FORM-2]** `<body>` is the internal number written in base 32 with this alphabet, where the
digit value is the position in the string (0 to 31):

```
0123456789abcdefghjkmnpqrstvwxyz
```

(This is Crockford's Base32 alphabet in lowercase: the letters `i`, `l`, `o`, `u` are not used.)

**[ID-FORM-3]** Internal numbers are integers in the range 1 to 9 223 372 036 854 775 807
(2^63 − 1) inclusive.

### 1.1 Encoding

**[ID-CODEC-1]** To encode a number, write its base-32 digits most significant first, without
leading zeros. The body is therefore 1 to 13 characters long.

**[ID-CODEC-2]** Encoders MUST produce lowercase prefix and body. The output of encoding is the
**canonical form** of the ID; APIs MUST always return IDs in canonical form.

### 1.2 Decoding

Decoding turns a string into `(kind, number)` or fails with a reason code.

**[ID-CODEC-3]** Decoding is case-insensitive for both prefix and body.

**[ID-CODEC-4]** Before converting digits, a decoder MUST map the look-alike letters in the body:
`i` and `l` (either case) to `1`, and `o` (either case) to `0`. The letter `u` has no mapping and
is invalid.

**[ID-CODEC-5]** Decoding fails with:

| Reason code | When |
|---|---|
| `id.malformed` | The string is not exactly `<prefix>_<body>` with one underscore, or the prefix or the body is empty, or the body contains a character outside the alphabet (after the mapping of [ID-CODEC-4]), or the string contains any whitespace |
| `id.unknown_prefix` | The prefix is not one of `n`, `l`, `e`, `c`, `b` (case-insensitive) |
| `id.non_canonical` | The body (after mapping) starts with `0`. This includes the body `0` itself. |
| `id.too_long` | The body is longer than 13 characters |
| `id.out_of_range` | The decoded number is greater than 2^63 − 1 |

The checks apply in the order listed; the first failing check gives the reason code.

**[ID-CODEC-6]** A decoder that expects a particular kind (for example "a node ID") MUST reject an
ID of another kind with reason code `id.wrong_kind`.

**[ID-CODEC-7]** A well-formed ID that names no existing object is not a decoding error; the
operation reports it as "not found" (API error mapping: planned `api/errors.md`).

### 1.3 Lifetime

**[ID-LIFE-1]** An object's ID never changes.

**[ID-LIFE-2]** Within one graph, an internal number is never assigned twice for the same object
kind, not even after the object is purged. Numbers are assigned in increasing order of creation.

**[ID-LIFE-3]** IDs are unique per graph and object kind only. The same ID in two different graphs
names two unrelated objects.

**[ID-LIFE-4]** When a node is merged into another, its ID remains valid and resolves to the
surviving node (rules in `model/objects.md`, planned M1).

## 2. Natural keys

A key is an optional, client-chosen identifier for a node, link or evidence, used for idempotent
upserts: `person/ada-lovelace`, `https://example.org/ada`, `wikidata:Q7259`.

**[KEY-1]** A key is a string of 1 to 1024 bytes, measured after NFC normalization.

**[KEY-2]** The server converts a received key to NFC. Two keys are equal when their NFC forms are
equal in code point order (case-sensitive; no other normalization).

**[KEY-3]** Validation fails with:

| Reason code | When |
|---|---|
| `key.empty` | The key is the empty string |
| `key.too_long` | Longer than 1024 bytes after NFC |
| `key.control_char` | Contains a code point of General_Category `Cc` |
| `key.edge_whitespace` | Starts or ends with a `White_Space` code point |

The checks apply in the order listed; the first failing check gives the reason code.

**[KEY-4]** Keys are unique per object kind within a graph: two nodes cannot share a key, but a
node and an evidence may.

**[KEY-5]** Rules for adding or changing an object's key are in `model/objects.md` (planned M1).

*(informative)* Recommended conventions: `<type>/<slug>` for nodes (`person/ada-lovelace`), the
canonical URL for web evidence, `<source>:<id>` for external identifiers (`wikidata:Q7259`).

## 3. Batch references

Inside one write request, a newly created object can be given a temporary name and referenced by
later items of the same request: `"ref": "@ada"`, then `"from": "@ada"`.

**[REF-1]** A batch reference is `@` followed by 1 to 64 characters from `A`–`Z`, `a`–`z`, `0`–`9`,
`_` and `-`. Anything else fails with `ref.malformed`.

**[REF-2]** Batch references are case-sensitive and exist only within the request that defines
them. Rules for duplicate and undefined references are in `api/operations/write.md` (planned M1).

## 4. Graph names

**[GRAPH-NAME-1]** A graph name matches `^[a-z0-9][a-z0-9_-]{0,62}$` (1 to 63 characters,
lowercase ASCII letters, digits, `_` and `-`, not starting with `_` or `-`). Uppercase input is
rejected, not converted. Failure: `graph_name.malformed`.

**[GRAPH-NAME-2]** Because a graph name is used as a file name, these names are reserved and
rejected with `graph_name.reserved`: `con`, `prn`, `aux`, `nul`, `com0`–`com9`, `lpt0`–`lpt9`.

## 5. Actor names

An actor is whoever performs a change: an agent session, a user, an import job.

**[ACTOR-NAME-1]** An actor name matches `^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$` (1 to 128 ASCII
characters). Actor names are case-sensitive. Failure: `actor_name.malformed`.

*(informative)* Examples: `claude-code@laptop`, `import:wikidata`, `admin`.

## 6. Change-log sequence numbers

**[SEQ-1]** Every change-log entry has a sequence number: a positive integer, strictly increasing
within a graph, never reused, exchanged as a JSON integer.

**[SEQ-2]** Sequence numbers of different graphs are unrelated.
