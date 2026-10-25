//! Property values — spec/model/values.md.

use crate::Reason;
use crate::text::nfc;
use crate::ucd::gc;
use serde_json::{Map, Number, Value as Json};
use std::cmp::Ordering;
use unicode_general_category::GeneralCategory as Gc;

const MS_PER_DAY: i64 = 86_400_000;

/// A stored property value (§1).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Date(Date),
    DateTime(DateTime),
    /// Non-empty, one scalar kind ([VAL-LIST-2]).
    List(Vec<Value>),
}

/// Result of parsing a value in a write: a value, or "remove this property".
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Value(Value),
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Precision {
    Year,
    Month,
    Day,
}

/// A calendar date of year, month or day precision ([VAL-DATE-1] .. [VAL-DATE-6]).
/// `month` and `day` are 0 when absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

/// An instant: milliseconds since 1970-01-01T00:00:00Z ([VAL-DT-2]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime {
    pub ms: i64,
}

impl Value {
    /// Kind name as used by the API: `string`, `int`, …, `list<date>`.
    pub fn kind_name(&self) -> String {
        match self {
            Self::List(items) => format!("list<{}>", items[0].kind_name()),
            other => other.scalar_kind().to_owned(),
        }
    }

    fn scalar_kind(&self) -> &'static str {
        match self {
            Self::String(_) => "string",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Bool(_) => "bool",
            Self::Date(_) => "date",
            Self::DateTime(_) => "datetime",
            Self::List(_) => "list",
        }
    }

    /// Canonical JSON form (§1, [VAL-STORE-1]).
    pub fn to_json(&self) -> Json {
        match self {
            Self::String(s) => Json::String(s.clone()),
            Self::Int(n) => Json::Number((*n).into()),
            Self::Float(x) => Number::from_f64(*x).map_or(Json::Null, Json::Number),
            Self::Bool(b) => Json::Bool(*b),
            Self::Date(d) => wrapper("$date", d.canonical()),
            Self::DateTime(t) => wrapper("$datetime", t.canonical()),
            Self::List(items) => Json::Array(items.iter().map(Self::to_json).collect()),
        }
    }
}

fn wrapper(name: &str, s: String) -> Json {
    let mut m = Map::new();
    m.insert(name.to_owned(), Json::String(s));
    Json::Object(m)
}

/// Parses raw JSON text of a value. Unpaired surrogate escapes give `text.invalid_unicode`
/// ([NORM-TEXT-1]).
pub fn parse_json_text(text: &str) -> Result<Parsed, Reason> {
    let json: Json = serde_json::from_str(text).map_err(|_| {
        if has_unpaired_surrogate_escape(text) {
            Reason("text.invalid_unicode")
        } else {
            Reason("json.malformed")
        }
    })?;
    parse(&json)
}

/// True when a JSON string escape `\uD800`–`\uDFFF` is not part of a high+low surrogate pair.
fn has_unpaired_surrogate_escape(text: &str) -> bool {
    let b = text.as_bytes();
    let hex = |at: usize| -> Option<u16> {
        let s = std::str::from_utf8(b.get(at..at + 4)?).ok()?;
        u16::from_str_radix(s, 16).ok()
    };
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
            continue;
        }
        if b.get(i + 1) != Some(&b'u') {
            i += 2; // other escape, including an escaped backslash
            continue;
        }
        let Some(unit) = hex(i + 2) else { return false };
        match unit {
            0xD800..=0xDBFF => {
                let low = (b.get(i + 6) == Some(&b'\\') && b.get(i + 7) == Some(&b'u'))
                    .then(|| hex(i + 8))
                    .flatten();
                if !matches!(low, Some(0xDC00..=0xDFFF)) {
                    return true;
                }
                i += 12;
            }
            0xDC00..=0xDFFF => return true,
            _ => i += 6,
        }
    }
    false
}

/// Parses a property value from its JSON form (§1 – §8).
pub fn parse(json: &Json) -> Result<Parsed, Reason> {
    match json {
        Json::Null => Ok(Parsed::Remove), // [VAL-KIND-2]
        Json::Array(items) if items.is_empty() => Ok(Parsed::Remove), // [VAL-LIST-4]
        Json::Array(items) => parse_list(items).map(Parsed::Value),
        other => parse_scalar(other).map(Parsed::Value),
    }
}

/// [VAL-LIST-1] .. [VAL-LIST-3]
fn parse_list(items: &[Json]) -> Result<Value, Reason> {
    if items.len() > 1000 {
        return Err(Reason("value.list.too_long"));
    }
    let mut out = Vec::with_capacity(items.len());
    let mut kind: Option<&'static str> = None;
    let mut mixed_numbers = false;
    for item in items {
        let v = match item {
            Json::Null => return Err(Reason("value.list.null_element")),
            Json::Array(_) => return Err(Reason("value.list.nested")),
            other => parse_scalar(other)?,
        };
        let k = v.scalar_kind();
        match kind {
            None => kind = Some(k),
            Some(seen) if seen == k => {}
            Some(seen) if is_number_kind(seen) && is_number_kind(k) => mixed_numbers = true,
            Some(_) => return Err(Reason("value.list.mixed_kinds")),
        }
        out.push(v);
    }
    if mixed_numbers {
        for v in &mut out {
            if let Value::Int(n) = *v {
                *v = Value::Float(n as f64);
            }
        }
    }
    Ok(Value::List(out))
}

fn is_number_kind(k: &str) -> bool {
    k == "int" || k == "float"
}

fn parse_scalar(json: &Json) -> Result<Value, Reason> {
    match json {
        Json::String(s) => parse_string(s),
        Json::Bool(b) => Ok(Value::Bool(*b)),
        Json::Number(n) => parse_number(&n.to_string()),
        Json::Object(m) => parse_object(m),
        Json::Null | Json::Array(_) => unreachable!("handled by the caller"),
    }
}

/// [VAL-STR-1] .. [VAL-STR-4]
fn parse_string(s: &str) -> Result<Value, Reason> {
    let s = nfc(s);
    if s.chars()
        .any(|c| gc(c) == Gc::Control && !matches!(c, '\t' | '\n' | '\r'))
    {
        return Err(Reason("value.string.control_char"));
    }
    if s.len() > 8192 {
        return Err(Reason("value.string.too_long"));
    }
    Ok(Value::String(s))
}

/// [VAL-NUM-1] .. [VAL-NUM-3], on the number's JSON text.
fn parse_number(text: &str) -> Result<Value, Reason> {
    if text.contains(['.', 'e', 'E']) {
        let x: f64 = text.parse().map_err(|_| Reason("json.malformed"))?;
        if !x.is_finite() {
            return Err(Reason("value.float.out_of_range"));
        }
        Ok(Value::Float(if x == 0.0 { 0.0 } else { x }))
    } else {
        text.parse::<i64>()
            .map(Value::Int)
            .map_err(|_| Reason("value.int.out_of_range"))
    }
}

/// [VAL-WRAP-1]
fn parse_object(m: &Map<String, Json>) -> Result<Value, Reason> {
    let known = m
        .get("$date")
        .map(|v| ("$date", v))
        .or_else(|| m.get("$datetime").map(|v| ("$datetime", v)));
    if let Some((name, v)) = known {
        if m.len() > 1 {
            return Err(Reason("value.wrapper.extra_member"));
        }
        let Json::String(s) = v else {
            return Err(Reason("value.wrapper.not_string"));
        };
        return if name == "$date" {
            Date::parse(s).map(Value::Date)
        } else {
            DateTime::parse(s).map(Value::DateTime)
        };
    }
    if m.len() == 1 && m.keys().next().is_some_and(|k| k.starts_with('$')) {
        return Err(Reason("value.wrapper.unknown"));
    }
    Err(Reason("value.object_not_allowed"))
}

// ----------------------------------------------------------------------------------------
// Property maps
// ----------------------------------------------------------------------------------------

/// Shared vocabulary text: node and link types and property keys. The in-memory state interns
/// them, so records point at one copy instead of owning their own.
pub type Sym = std::sync::Arc<str>;

/// Property map kept sorted by key in code point order ([VAL-STORE-1]). A sorted vector rather
/// than a tree: objects usually have a handful of properties, and memory per object matters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PropMap(Vec<(Sym, Value)>);

type PropIter<'a> = std::iter::Map<
    std::slice::Iter<'a, (Sym, Value)>,
    fn(&'a (Sym, Value)) -> (&'a Sym, &'a Value),
>;

impl PropMap {
    pub fn new() -> Self {
        Self::default()
    }

    fn find(&self, k: &str) -> Result<usize, usize> {
        self.0.binary_search_by(|(x, _)| (**x).cmp(k))
    }

    pub fn get(&self, k: &str) -> Option<&Value> {
        self.find(k).ok().map(|i| &self.0[i].1)
    }

    pub fn contains_key(&self, k: &str) -> bool {
        self.find(k).is_ok()
    }

    /// Sets a property, returning the previous value; an existing key keeps its stored text.
    pub fn insert(&mut self, k: impl AsRef<str> + Into<Sym>, v: Value) -> Option<Value> {
        match self.find(k.as_ref()) {
            Ok(i) => Some(std::mem::replace(&mut self.0[i].1, v)),
            Err(i) => {
                self.0.insert(i, (k.into(), v));
                None
            }
        }
    }

    pub fn remove(&mut self, k: &str) -> Option<Value> {
        self.find(k).ok().map(|i| self.0.remove(i).1)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> PropIter<'_> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub fn keys(&self) -> impl Iterator<Item = &Sym> {
        self.0.iter().map(|(k, _)| k)
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.0.iter().map(|(_, v)| v)
    }

    /// Replaces every key with `f(key)` (used for interning) and drops spare capacity.
    pub fn intern_keys(&mut self, mut f: impl FnMut(&Sym) -> Sym) {
        for (k, _) in &mut self.0 {
            *k = f(k);
        }
        self.0.shrink_to_fit();
    }
}

impl<'a> IntoIterator for &'a PropMap {
    type Item = (&'a Sym, &'a Value);
    type IntoIter = PropIter<'a>;
    fn into_iter(self) -> PropIter<'a> {
        self.iter()
    }
}

// ----------------------------------------------------------------------------------------
// Storage encoding (§11)
// ----------------------------------------------------------------------------------------

/// [VAL-STORE-1] [VAL-STORE-2] Property map as stored text; `None` when empty.
pub fn encode_props(props: &PropMap) -> Option<String> {
    if props.is_empty() {
        return None;
    }
    let map: Map<String, Json> = props
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_json()))
        .collect();
    Some(Json::Object(map).to_string())
}

/// Reads a stored property map ([VAL-STORE-4]).
pub fn decode_props(text: Option<&str>) -> Result<PropMap, Reason> {
    let mut out = PropMap::new();
    let Some(text) = text else { return Ok(out) };
    let Json::Object(m) =
        serde_json::from_str(text).map_err(|_| Reason("storage.props_invalid"))?
    else {
        return Err(Reason("storage.props_invalid"));
    };
    for (k, v) in m {
        if let Parsed::Value(v) = parse(&v)? {
            out.insert(k, v);
        }
    }
    Ok(out)
}

/// Canonical text of a `date` or `datetime` value ([STORE-COL-3]).
pub fn temporal_text(v: &Value) -> Option<String> {
    match v {
        Value::Date(d) => Some(d.canonical()),
        Value::DateTime(t) => Some(t.canonical()),
        _ => None,
    }
}

/// Parses [`temporal_text`] output: text containing `T` is a date-time.
pub fn parse_temporal_text(s: &str) -> Result<Value, Reason> {
    if s.contains('T') {
        DateTime::parse(s).map(Value::DateTime)
    } else {
        Date::parse(s).map(Value::Date)
    }
}

/// Interval start and end in UTC milliseconds ([VAL-DATE-5]; a date-time is a point).
pub fn temporal_bounds(v: &Value) -> Option<(i64, i64)> {
    match v {
        Value::DateTime(t) => Some((t.ms, t.ms)),
        Value::Date(d) => {
            let start = d.start_ms();
            let next = match d.precision() {
                Precision::Year => Date {
                    year: d.year + 1,
                    month: 1,
                    day: 1,
                }
                .start_ms(),
                Precision::Month if d.month == 12 => Date {
                    year: d.year + 1,
                    month: 1,
                    day: 1,
                }
                .start_ms(),
                Precision::Month => Date {
                    year: d.year,
                    month: d.month + 1,
                    day: 1,
                }
                .start_ms(),
                Precision::Day => start + MS_PER_DAY,
            };
            Some((start, next - 1))
        }
        _ => None,
    }
}

// ----------------------------------------------------------------------------------------
// Dates and date-times
// ----------------------------------------------------------------------------------------

/// [VAL-DATE-3]
fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_month(y: i64, m: u8) -> u8 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (astronomical years).
fn days_from_civil(y: i64, m: u8, d: u8) -> i64 {
    let (m, d) = (i64::from(m), i64::from(d));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u8, u8) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn digits(s: &[u8]) -> Option<u32> {
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(s.iter().fold(0, |n, &b| n * 10 + u32::from(b - b'0')))
}

impl Date {
    /// [VAL-DATE-1] .. [VAL-DATE-4]
    pub fn parse(s: &str) -> Result<Self, Reason> {
        const MALFORMED: Reason = Reason("value.date.malformed");
        let b = s.as_bytes();
        let (neg, rest) = match b.first() {
            Some(b'-') => (true, &b[1..]),
            _ => (false, b),
        };
        if !matches!(rest.len(), 4 | 7 | 10) {
            return Err(MALFORMED);
        }
        let year = digits(&rest[..4]).ok_or(MALFORMED)?;
        if neg && year == 0 {
            return Err(MALFORMED);
        }
        let mut parts = [0u32; 2];
        for (i, part) in parts.iter_mut().enumerate().take((rest.len() - 4) / 3) {
            let at = 4 + i * 3;
            if rest[at] != b'-' || rest[at + 1..at + 3].len() != 2 {
                return Err(MALFORMED);
            }
            *part = digits(&rest[at + 1..at + 3]).ok_or(MALFORMED)?;
        }
        let year = if neg { -(year as i32) } else { year as i32 };
        let (month, day) = (parts[0] as u8, parts[1] as u8);
        if rest.len() >= 7 && !(1..=12).contains(&month) {
            return Err(Reason("value.date.invalid_month"));
        }
        if rest.len() == 10 && !(1..=days_in_month(year.into(), month)).contains(&day) {
            return Err(Reason("value.date.invalid_day"));
        }
        Ok(Self { year, month, day })
    }

    /// [VAL-DATE-5]
    pub fn precision(self) -> Precision {
        match (self.month, self.day) {
            (0, _) => Precision::Year,
            (_, 0) => Precision::Month,
            _ => Precision::Day,
        }
    }

    /// Interval start in UTC milliseconds ([VAL-DATE-5]).
    pub fn start_ms(self) -> i64 {
        days_from_civil(self.year.into(), self.month.max(1), self.day.max(1)) * MS_PER_DAY
    }

    /// [VAL-DATE-6]
    pub fn canonical(self) -> String {
        let sign = if self.year < 0 { "-" } else { "" };
        let mut s = format!("{sign}{:04}", self.year.unsigned_abs());
        if self.month > 0 {
            s += &format!("-{:02}", self.month);
        }
        if self.day > 0 {
            s += &format!("-{:02}", self.day);
        }
        s
    }
}

impl DateTime {
    /// [VAL-DT-1] [VAL-DT-2]
    pub fn parse(s: &str) -> Result<Self, Reason> {
        let b = s.as_bytes();
        let (core_len, frac) = Self::split_core(b).ok_or(Reason("value.datetime.malformed"))?;
        let offset = &b[core_len..];
        let (sign, oh, om) = match offset {
            [] => return Err(Reason("value.datetime.missing_offset")),
            [b'Z' | b'z'] => (1, 0, 0),
            [sg @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
                let h = digits(&[*h1, *h2]).ok_or(Reason("value.datetime.malformed"))?;
                let m = digits(&[*m1, *m2]).ok_or(Reason("value.datetime.malformed"))?;
                (if *sg == b'-' { -1 } else { 1 }, h, m)
            }
            _ => return Err(Reason("value.datetime.malformed")),
        };
        let num = |r: std::ops::Range<usize>| i64::from(digits(&b[r]).unwrap_or(0));
        let (y, mo, d) = (num(0..4), num(5..7) as u8, num(8..10) as u8);
        let (hh, mi, ss) = (num(11..13), num(14..16), num(17..19));
        if y == 0 || !(1..=12).contains(&mo) || !(1..=days_in_month(y, mo)).contains(&d) {
            return Err(Reason("value.datetime.invalid_date"));
        }
        if hh > 23 || mi > 59 || ss > 59 || oh > 23 || om > 59 {
            return Err(Reason("value.datetime.invalid_time"));
        }
        // Truncate the fraction to milliseconds.
        let ms_frac = frac
            .iter()
            .chain(b"000")
            .take(3)
            .fold(0i64, |n, &c| n * 10 + i64::from(c - b'0'));
        let local =
            (days_from_civil(y, mo, d) * 86_400 + hh * 3600 + mi * 60 + ss) * 1000 + ms_frac;
        let ms = local - sign * (i64::from(oh) * 3600 + i64::from(om) * 60) * 1000;
        let (uy, _, _) = civil_from_days(ms.div_euclid(MS_PER_DAY));
        if !(1..=9999).contains(&uy) {
            return Err(Reason("value.datetime.out_of_range"));
        }
        Ok(Self { ms })
    }

    /// Validates `YYYY-MM-DDThh:mm:ss[.f{1,9}]`; returns its length and the fraction digits.
    fn split_core(b: &[u8]) -> Option<(usize, &[u8])> {
        if b.len() < 19 {
            return None;
        }
        let shape_ok = b[..19].iter().enumerate().all(|(i, &c)| match i {
            4 | 7 => c == b'-',
            10 => c == b'T' || c == b't',
            13 | 16 => c == b':',
            _ => c.is_ascii_digit(),
        });
        if !shape_ok {
            return None;
        }
        if b.get(19) != Some(&b'.') {
            return Some((19, &[]));
        }
        let n = b[20..].iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=9).contains(&n) {
            return None;
        }
        Some((20 + n, &b[20..20 + n]))
    }

    /// [VAL-DT-3]
    pub fn canonical(self) -> String {
        let (y, m, d) = civil_from_days(self.ms.div_euclid(MS_PER_DAY));
        let rem = self.ms.rem_euclid(MS_PER_DAY);
        let (h, mi, s, ms) = (
            rem / 3_600_000,
            rem / 60_000 % 60,
            rem / 1000 % 60,
            rem % 1000,
        );
        let frac = if ms == 0 {
            String::new()
        } else {
            format!(".{ms:03}")
        };
        format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}{frac}Z")
    }
}

// ----------------------------------------------------------------------------------------
// Equality and order (§10)
// ----------------------------------------------------------------------------------------

/// [VAL-EQ-1]
pub fn values_equal(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (Int(_) | Float(_), Int(_) | Float(_)) => cmp_numbers(a, b) == Ordering::Equal,
        (String(x), String(y)) => x == y,
        (Bool(x), Bool(y)) => x == y,
        (DateTime(x), DateTime(y)) => x == y,
        (Date(x), Date(y)) => x == y,
        (List(x), List(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| values_equal(p, q))
        }
        _ => false,
    }
}

/// [VAL-ORDER-1] total order.
pub fn compare(a: &Value, b: &Value) -> Ordering {
    use Value::*;
    let rank = |v: &Value| match v {
        Bool(_) => 0,
        Int(_) | Float(_) => 1,
        Date(_) | DateTime(_) => 2,
        String(_) => 3,
        List(_) => 4,
    };
    rank(a).cmp(&rank(b)).then_with(|| match (a, b) {
        (Bool(x), Bool(y)) => x.cmp(y),
        (Int(_) | Float(_), Int(_) | Float(_)) => {
            cmp_numbers(a, b).then_with(|| matches!(a, Float(_)).cmp(&matches!(b, Float(_))))
        }
        (String(x), String(y)) => x.cmp(y),
        (List(x), List(y)) => x
            .iter()
            .zip(y)
            .map(|(p, q)| compare(p, q))
            .find(|o| o.is_ne())
            .unwrap_or_else(|| x.len().cmp(&y.len())),
        _ => temporal_key(a).cmp(&temporal_key(b)),
    })
}

fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Float(x) => *x,
        _ => f64::NAN,
    }
}

/// Exact numeric comparison of an int or float with another (no precision loss for large ints).
fn cmp_numbers(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Int(x), Value::Float(y)) => cmp_int_float(*x, *y),
        (Value::Float(x), Value::Int(y)) => cmp_int_float(*y, *x).reverse(),
        _ => as_f64(a).total_cmp(&as_f64(b)),
    }
}

fn cmp_int_float(i: i64, f: f64) -> Ordering {
    let two_63 = (1u64 << 63) as f64; // exactly representable
    if f >= two_63 {
        Ordering::Less
    } else if f < -two_63 {
        Ordering::Greater
    } else {
        let t = f.trunc();
        (i as i128)
            .cmp(&(t as i128))
            .then_with(|| 0f64.partial_cmp(&(f - t)).unwrap_or(Ordering::Equal))
    }
}

fn temporal_key(v: &Value) -> (i64, u8) {
    match v {
        Value::Date(d) => (d.start_ms(), d.precision() as u8),
        Value::DateTime(t) => (t.ms, 3),
        _ => (0, 0),
    }
}
