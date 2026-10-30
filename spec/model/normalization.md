# Normalization

Status: **Draft** · spec 0.1.0-draft · vectors:
[conformance/vectors/name-normalization.json](../../conformance/vectors/name-normalization.json),
[conformance/vectors/vocabulary.json](../../conformance/vectors/vocabulary.json)

Three things are defined here:

1. **Text input** — how every incoming string is cleaned before it is stored ([NORM-TEXT]).
2. **Name keys** — the matching form of names and aliases, used for exact lookup, upsert matching
   and the exact stage of entity resolution ([NORM-NAME]).
3. **Vocabulary naming** — which spellings of node types, link types and property keys a graph
   accepts; names are never rewritten ([VOCAB]).

Full-text search tokenization is not defined here; it is configured in the storage format
([storage/schema.sql](../storage/schema.sql)).

Unicode data requirements are in [spec/README.md §4](../README.md#4-unicode-data).

---

## 1. Text input

**[NORM-TEXT-1]** Every string received through the API MUST be valid Unicode (no unpaired
surrogates). Otherwise the request is invalid with reason code `text.invalid_unicode`.

**[NORM-TEXT-2]** Every string received through the API MUST be converted to NFC before it is
validated, compared or stored — with one exception: the `content` of an evidence is stored exactly
as received (it is the evidence) and is not normalized.

**[NORM-TEXT-3]** Field-specific limits (lengths, allowed characters) are measured on the NFC form
and are defined with each field (identifiers, values, objects).

## 2. Name keys

The **name key** of a string is the form used to decide whether two names are "the same name".
It folds case, accents in Latin and Greek script, typographic variants and spacing, but keeps
everything that can distinguish two different entities (digits, symbols like `#` and `+`,
punctuation like `.` and `/`, letters of other scripts with their marks).

**[NORM-NAME-1]** `name_key(s)` is computed by these steps, in order:

| Step | Operation |
|---|---|
| 1 | Convert `s` to **NFKD**. |
| 2 | Remove every code point whose General_Category is `Cf` (format characters: soft hyphen, zero-width space and joiners, BOM, bidirectional marks…), and every code point whose General_Category is `Cc` and that is not `White_Space`. |
| 3 | Replace every code point by its **simple case folding** (CaseFolding.txt entries with status `C` or `S`; code points without such an entry are unchanged). |
| 4 | Convert to **NFKD** again. |
| 5 | Remove marks (see [NORM-NAME-2]). |
| 6 | Replace code points listed in the **extra folding table** ([NORM-NAME-3]). |
| 7 | Remove the code points in the **quote table**; replace the code points in the **dash table** by U+0020 SPACE ([NORM-NAME-4]). |
| 8 | Replace every `White_Space` code point by U+0020; replace each run of consecutive U+0020 by one U+0020; remove U+0020 at the start and end. |
| 9 | Convert to **NFC**. |

**[NORM-NAME-2]** Mark removal (step 5). For each code point `m` with General_Category `Mn`, let
its **base** be the nearest code point before `m` whose General_Category is not `Mn`. Remove `m` if:

- its base lies in one of these ranges: U+0000–U+02AF, U+0370–U+03FF, U+1E00–U+1FFF
  (Latin and Greek blocks, including Latin Extended Additional and Greek Extended); **or**
- `m` is U+0308 COMBINING DIAERESIS and its base is U+0435 CYRILLIC SMALL LETTER IE
  (so that `ё` folds to `е`).

A mark with no base (at the start of the string) is kept. All other marks are kept — for example
the breve of Cyrillic `й`, the diaeresis of Ukrainian `ї`, and Devanagari or Hebrew marks.

**[NORM-NAME-3]** Extra folding table (step 6). Letters that have no decomposition but are
commonly written without their stroke or as two letters:

| Code point | Character | Replacement |
|---|---|---|
| U+00DF | ß | `ss` |
| U+00E6 | æ | `ae` |
| U+00F0 | ð | `d` |
| U+00F8 | ø | `o` |
| U+00FE | þ | `th` |
| U+0111 | đ | `d` |
| U+0127 | ħ | `h` |
| U+0131 | ı (dotless i) | `i` |
| U+0142 | ł | `l` |
| U+0153 | œ | `oe` |
| U+0167 | ŧ | `t` |

Only lowercase forms are listed: step 3 has already folded the capitals (Æ, Ø, Ł, ẞ…) to them.

**[NORM-NAME-4]** Quote and dash tables (step 7).

Quote table — removed:

| Code points |
|---|
| U+0022 `"`, U+0027 `'`, U+0060 `` ` ``, U+00AB `«`, U+00BB `»`, U+02B9 `ʹ`, U+02BB `ʻ`, U+02BC `ʼ`, U+02BD `ʽ`, U+2018 `‘`, U+2019 `’`, U+201A `‚`, U+201B `‛`, U+201C `“`, U+201D `”`, U+201E `„`, U+201F `‟`, U+2032 `′`, U+2039 `‹`, U+203A `›` |

Dash table — replaced by a space:

| Code points |
|---|
| U+002D `-`, U+005F `_`, U+2010 `‐`, U+2012 `‒`, U+2013 `–`, U+2014 `—`, U+2015 `―`, U+2212 `−`, U+2E3A `⸺`, U+2E3B `⸻` |

*(informative)* Other dash and quote variants (fullwidth forms, non-breaking hyphen, small forms)
are already turned into one of the listed characters by NFKD in step 1.

**[NORM-NAME-5]** A node name or alias whose name key is the empty string is invalid, reason code
`name.empty_key` (for example a name consisting only of quotes or spaces).

**[NORM-NAME-6]** Name keys are derived values. They MUST NOT be the only stored copy of a name;
an implementation MAY cache them, but MUST be able to recompute them from the stored names.

**[NORM-NAME-7]** Two names *match exactly* when their name keys are equal in code point order.

*(informative)* What the name key deliberately does **not** do:

- No transliteration: `Moscow` and `Москва` do not match. Aliases, fuzzy search and (phase 2)
  semantic search cover this.
- No removal of `.`, `,`, `/`, `#`, `+`, `&`: `J.R.R. Tolkien` and `JRR Tolkien` do not match
  exactly (fuzzy search finds them), but `C#` and `C` stay different.
- No word reordering: `Lovelace, Ada` and `Ada Lovelace` do not match exactly.
- No folding of Cyrillic `й`/`и`, Ukrainian `ї`/`і`, or any mark outside Latin and Greek script,
  except `ё` → `е`, which is how Russian is commonly written.

## 3. Vocabulary naming

A graph's naming policy keeps one spelling per vocabulary item, so `works at`, `worksAt` and
`WORKS_AT` do not become three link types. It does so by refusing names, never by rewriting them:
what a client sends is what is stored and returned, or the request fails and says what to send.

| Item | Style | Example |
|---|---|---|
| Node type (also labels) | PascalCase | `ResearchPaper` |
| Link type | UPPER_SNAKE | `WORKS_AT` |
| Property key | lower_snake | `birth_date` |

**[VOCAB-1]** The graph policy `naming` is one of:

- `styled` (the default): a name is accepted only when it already is in its style, that is, when
  its NFC form equals its styled form ([VOCAB-2] .. [VOCAB-4]) and the styled form passes
  [VOCAB-5]. Otherwise the request fails with `vocab.not_styled`; the error's `details` hold
  `field`, `given` and `expected` (the styled form), so the client can resend.
- `free`: any name that passes [VOCAB-5] as given (after NFC) is accepted unchanged.

The rule applies to every name in a request, in writes and in reads alike (`find` field
expressions and type values, `neighbors` and `paths` link and node types, `search` types,
`resolve` types and hint keys, `get` link types), so a misspelled name fails instead of quietly
matching nothing. NFC ([NORM-TEXT-2]) is the only change ever made to a name.

*(informative)* Graphs created before this policy stored `normalize_types`; `true` reads as
`styled` (their names are styled already), `false` as `free`.

**[VOCAB-2]** Word splitting. Convert the input to **NFKC**, then split it into words:

1. *Word characters* are code points whose General_Category starts with `L` (letters), `M`
   (marks) or `N` (numbers). Every other code point is a separator; runs of separators separate
   words and are discarded.
2. Inside a run of word characters, a word boundary is placed between two adjacent code points
   `a` and `b` when:
   1. `a` is `Ll` (lowercase letter) and `b` is `Lu` or `Lt` (uppercase or titlecase letter) —
      `worksAt` → `works` | `At`; or
   2. `a` is `Lu` or `Lt`, `b` is `Lu` or `Lt`, and the code point after `b` exists and is `Ll` —
      `HTTPServer` → `HTTP` | `Server`; or
   3. `a` is `Nd`, `Nl` or `No` (a number) and `b` is `Lu` or `Lt` — `Covid19Case` → `Covid19` | `Case`.
3. Marks (`M`) never start a boundary: when checking rules 1–3, a mark is skipped and the
   code point before it is used as `a`.

**[VOCAB-3]** Case mapping in this section uses **simple** case mappings (UnicodeData.txt), one
code point to one code point. A code point without a mapping is unchanged.

**[VOCAB-4]** Joining:

- **Node type (PascalCase):** for each word, map its first code point to uppercase and the rest to
  lowercase; concatenate the words without separator.
- **Link type (UPPER_SNAKE):** map every code point of every word to uppercase; join the words
  with `_`.
- **Property key (lower_snake):** map every code point of every word to lowercase; join the words
  with `_`.

**[VOCAB-5]** Validation of a name: under `styled`, of its styled form; under `free`, of the name
as given, after NFC:

| Reason code | When |
|---|---|
| `vocab.empty` | No words (`styled`) or the empty string (`free`) |
| `vocab.too_long` | More than 64 code points |
| `vocab.bad_start` | The first code point is not a letter (General_Category `L*`) |
| `vocab.bad_char` | Only under `free`: a code point that is not `L*`, `M*`, `N*` or `_` |
| `vocab.not_styled` | Only under `styled`: the name passes the checks above but is not its own styled form ([VOCAB-1]) |

The checks apply in the order listed.

*(informative)* Known effects of these rules: under `styled`, acronyms lose their capitals in
PascalCase (`TVShow` is refused with `expected` `TvShow`), because one predictable spelling matters
more than preserving acronyms; a graph that must keep source spellings (`locationIP`, `TVShow`)
uses `free`. Letters of any script are allowed (`Человек`).
