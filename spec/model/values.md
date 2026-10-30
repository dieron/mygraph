# Property values

Status: **Draft** · spec 0.1.0-draft · vectors: [conformance/vectors/values.json](../../conformance/vectors/values.json)

Nodes and links carry properties: a map from property key to value. This file defines the value
kinds, their JSON form in the API, their limits, how they compare and sort, and how they are
encoded in storage.

Property keys follow [normalization.md §3](normalization.md#3-vocabulary-naming) (lower_snake
under the default `naming: styled`).

---

## 1. Kinds

| Kind | JSON form in requests and responses | Example |
|---|---|---|
| `string` | JSON string | `"British"` |
| `int` | JSON number without fraction and exponent | `1815` |
| `float` | JSON number with a fraction or an exponent | `3.14`, `1.0`, `6.02e23` |
| `bool` | `true` / `false` | `true` |
| `date` | `{"$date": "<date>"}` | `{"$date": "1815-12-10"}`, `{"$date": "1815"}` |
| `datetime` | `{"$datetime": "<date-time>"}` | `{"$datetime": "2026-10-24T08:15:00Z"}` |
| `list` | JSON array of values of one scalar kind | `["en", "fr"]` |

The *scalar* kinds are all kinds except `list`. A list's kind is written `list<k>`, e.g.
`list<string>`, `list<date>`.

**[VAL-KIND-1]** The kind of a value is determined only by its JSON form, as in the table above.
No string is ever re-interpreted as a number, date or anything else.

**[VAL-KIND-2]** `null` as a property value in a write means "remove this property". It is not a
stored value.

## 2. Strings

**[VAL-STR-1]** A string value is converted to NFC ([NORM-TEXT-2]).

**[VAL-STR-2]** A string value MUST NOT contain U+0000 or any other code point of General_Category
`Cc` except U+0009 (tab), U+000A (line feed) and U+000D (carriage return). Reason code:
`value.string.control_char`.

**[VAL-STR-3]** A string value is at most 8192 bytes. Reason code: `value.string.too_long`.
*(informative: long text belongs in evidence, not in properties.)*

**[VAL-STR-4]** The empty string is a valid value, distinct from an absent property.

## 3. Numbers

**[VAL-NUM-1]** A JSON number token that contains none of `.`, `e`, `E` is an `int`. Its value MUST
lie in −9 223 372 036 854 775 808 … 9 223 372 036 854 775 807 (64-bit signed). Reason code:
`value.int.out_of_range`.

**[VAL-NUM-2]** Any other JSON number token is a `float`: the IEEE 754 binary64 value nearest to
the decimal (round half to even). If the magnitude is too large to be finite, reason code
`value.float.out_of_range`. Values too small to be represented become 0.0 (or the nearest subnormal).

**[VAL-NUM-3]** Negative zero is stored and returned as `0.0`.

**[VAL-NUM-4]** When a `float` is written as JSON (API responses, storage), the text MUST contain
`.`, `e` or `E`, and MUST parse back to exactly the same binary64 value. *(informative: e.g. `1.0`,
not `1`; the shortest round-trip representation is recommended.)*

## 4. Booleans

**[VAL-BOOL-1]** `true` and `false` are values of kind `bool`.

## 5. Dates

A date names a calendar year, month or day, without time zone. Partial dates are normal in
knowledge work (`born ~1450` is a year, not a day).

**[VAL-DATE-1]** The `$date` string matches `^-?[0-9]{4}(-[0-9]{2}(-[0-9]{2})?)?$`:
a 4-digit year, optionally preceded by `-`, optionally followed by month, optionally followed by
day. Anything else: reason code `value.date.malformed`.

**[VAL-DATE-2]** Years use **astronomical numbering**: `0000` is 1 BC, `-0001` is 2 BC,
`-0427` is 428 BC. The range is `-9999` to `9999`. The string `-0000` is invalid:
reason code `value.date.malformed`.

**[VAL-DATE-3]** The calendar is the proleptic Gregorian calendar. A year is a leap year when it
is divisible by 4 and (not divisible by 100 or divisible by 400) — "divisible" meaning the
remainder is zero, which also holds for negative years (`0000`, `-0004`, `-0400` are leap years;
`-0100` is not).

**[VAL-DATE-4]** Month must be `01`–`12` (reason code `value.date.invalid_month`); day must be
`01` up to the number of days in that month of that year (reason code `value.date.invalid_day`).

**[VAL-DATE-5]** The **precision** of a date is `year`, `month` or `day`, from its form. A date
denotes a closed interval of days: a year from its 1 January to its 31 December, a month from its
first to its last day, a day only itself. Interval *start* is 00:00:00.000 UTC on the first day;
interval *end* is the last millisecond of the last day.

**[VAL-DATE-6]** The canonical form of a valid date is its input string (the grammar admits no
alternative spellings).

## 6. Date-times

**[VAL-DT-1]** The `$datetime` string matches:

```
YYYY-MM-DD T hh:mm:ss [ .f{1,9} ] ( Z | +hh:mm | -hh:mm )
```

with no spaces (shown above only for readability): year `0001`–`9999`, month, day as for dates,
hour `00`–`23`, minute and second `00`–`59` (no leap second), 1 to 9 fraction digits, offset hours
`00`–`23` and minutes `00`–`59`. `T` and `Z` may be lowercase. The offset is required.

Reason codes, checked in this order:

| Reason code | When |
|---|---|
| `value.datetime.missing_offset` | The string is a well-formed date-time except that it has no offset |
| `value.datetime.malformed` | Any other deviation from the grammar (digit counts, separators, missing or extra parts, a `.` without fraction digits, more than 9 fraction digits, a sign before the year) |
| `value.datetime.invalid_date` | Year `0000`, month outside `01`–`12`, or a day the month does not have |
| `value.datetime.invalid_time` | Hour, minute or second out of range, or offset hours above `23` / offset minutes above `59` |

**[VAL-DT-2]** A date-time denotes an instant. It is converted to UTC; fraction digits beyond
milliseconds are **truncated** (not rounded). If the UTC result falls outside year 0001–9999:
reason code `value.datetime.out_of_range`.

**[VAL-DT-3]** Canonical form: `YYYY-MM-DDThh:mm:ssZ` when the milliseconds are zero, otherwise
`YYYY-MM-DDThh:mm:ss.sssZ` with exactly three fraction digits; always uppercase `T` and `Z`.

## 7. Wrapper objects

**[VAL-WRAP-1]** A JSON object used as a property value MUST have exactly one member, whose name is
`$date` or `$datetime` and whose value is a JSON string. An object is checked in this order:

1. It has a member named `$date` or `$datetime`:
   - more than one member → `value.wrapper.extra_member`;
   - the member value is not a string → `value.wrapper.not_string`;
   - otherwise the string is validated as a date ([VAL-DATE]) or date-time ([VAL-DT]).
2. Otherwise, it has exactly one member and that member's name starts with `$` →
   `value.wrapper.unknown`.
3. Otherwise (empty object, several members, or ordinary member names) →
   `value.object_not_allowed`.

*(informative)* `$qty` (quantity with unit) and `$geo` (point) are reserved for later versions
and are rejected as `value.wrapper.unknown` until then. Objects are not allowed as values because
structure belongs in links and evidence; flatten with keys like `address_city`.

## 8. Lists

**[VAL-LIST-1]** A list is a JSON array of 1 to 1000 scalar values. Reason code:
`value.list.too_long`.

**[VAL-LIST-2]** All elements must be of the same kind, except that `int` and `float` may be mixed;
such a list is a `list<float>` and its `int` elements are converted to the nearest binary64 value.
Dates of different precision are the same kind. Reason codes: `value.list.mixed_kinds`,
`value.list.nested` (an element is an array), `value.list.null_element`.

**[VAL-LIST-3]** A list is checked in this order: first its length ([VAL-LIST-1]); then each element
in turn — `null` → `value.list.null_element`, an array → `value.list.nested`, otherwise the element
is validated as a scalar value (its own reason code on failure), and then its kind is compared with
the kinds seen so far (`value.list.mixed_kinds`). The first failure is reported.

**[VAL-LIST-4]** An empty array in a write means "remove this property", like `null`.

**[VAL-LIST-5]** Order and duplicates are preserved.

## 9. Limits per object

**[VAL-LIMIT-1]** An object has at most 256 properties. Reason code: `value.props.too_many`.

**[VAL-LIMIT-2]** The storage encoding (§11) of an object's whole property map is at most 65 536
bytes. Reason code: `value.props.too_large`.

## 10. Equality and order

**[VAL-EQ-1]** Two values are **equal** when:

- both are numbers (`int` or `float`) with the same numeric value (`1` equals `1.0`); or
- both are `bool`, `string` (code point equality) or `datetime` and identical; or
- both are `date` with the same precision and the same canonical form; or
- both are lists of equal length whose elements are pairwise equal.

**[VAL-ORDER-1]** The **total order** used wherever values are sorted:

1. By kind rank: `bool` < numbers (`int`, `float`) < temporal (`date`, `datetime`) < `string` < `list`.
2. `bool`: `false` < `true`.
3. Numbers: by numeric value; for equal numeric values, `int` before `float`.
4. Temporal: by interval start ([VAL-DATE-5]; a date-time's start is its instant); for equal
   starts, by precision in the order `year` < `month` < `day` < `datetime`.
5. `string`: code point order.
6. Lists: element by element using this order; a list that is a prefix of another comes first.

*(informative)* Filter operators on partial dates (`born < 1850` when `born` is `1849-12`) are
defined with interval semantics in [api/operations/find.md](../api/operations/find.md) [OP-FIND-4];
where missing values sort is defined there too ([OP-FIND-7]).

## 11. Storage encoding

**[VAL-STORE-1]** A property map is stored as the text of a JSON object (UTF-8) whose members are
sorted by key in code point order, with each value in its canonical JSON form:

| Kind | Stored as |
|---|---|
| `string` | JSON string of the NFC text |
| `int` | JSON number token without `.`, `e`, `E` |
| `float` | JSON number token per [VAL-NUM-4] |
| `bool` | `true` / `false` |
| `date` | `{"$date":"<canonical>"}` |
| `datetime` | `{"$datetime":"<canonical UTC>"}` |
| `list` | JSON array of the element encodings |

**[VAL-STORE-2]** An object without properties stores SQL `NULL` in its properties column.
Readers MUST also accept `{}`.

**[VAL-STORE-3]** Writers SHOULD write non-ASCII characters as UTF-8 rather than `\u` escapes and
SHOULD NOT write insignificant whitespace. Readers MUST accept any valid JSON for the same values.

**[VAL-STORE-4]** Reading a stored map and writing it again with the rules above MUST yield the
same values (round trip), regardless of which implementation wrote it.
