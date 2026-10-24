//! Identifiers — spec/model/identifiers.md.

use crate::Reason;
use crate::ucd::is_white_space;
use unicode_general_category::GeneralCategory;
use unicode_normalization::UnicodeNormalization;

/// [ID-FORM-2] Crockford base32, lowercase.
const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";

/// [ID-FORM-3]
pub const MAX_NUMBER: u64 = i64::MAX as u64;

/// Object kinds that have IDs ([ID-FORM-1]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    Node,
    Link,
    Evidence,
    Citation,
    Batch,
}

impl ObjectKind {
    pub fn prefix(self) -> char {
        match self {
            Self::Node => 'n',
            Self::Link => 'l',
            Self::Evidence => 'e',
            Self::Citation => 'c',
            Self::Batch => 'b',
        }
    }

    pub fn from_prefix(p: &str) -> Option<Self> {
        match p.to_ascii_lowercase().as_str() {
            "n" => Some(Self::Node),
            "l" => Some(Self::Link),
            "e" => Some(Self::Evidence),
            "c" => Some(Self::Citation),
            "b" => Some(Self::Batch),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Link => "link",
            Self::Evidence => "evidence",
            Self::Citation => "citation",
            Self::Batch => "batch",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        [
            Self::Node,
            Self::Link,
            Self::Evidence,
            Self::Citation,
            Self::Batch,
        ]
        .into_iter()
        .find(|k| k.name() == s)
    }
}

/// A decoded object ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectId {
    pub kind: ObjectKind,
    pub number: u64,
}

impl ObjectId {
    pub fn new(kind: ObjectKind, number: u64) -> Self {
        assert!(
            (1..=MAX_NUMBER).contains(&number),
            "object number out of range"
        );
        Self { kind, number }
    }

    /// [ID-CODEC-1] [ID-CODEC-2] canonical form.
    pub fn encode(self) -> String {
        let mut digits = Vec::with_capacity(13);
        let mut n = self.number;
        while n > 0 {
            digits.push(ALPHABET[(n % 32) as usize]);
            n /= 32;
        }
        let mut s = String::with_capacity(digits.len() + 2);
        s.push(self.kind.prefix());
        s.push('_');
        s.extend(digits.iter().rev().map(|&d| d as char));
        s
    }

    /// [ID-CODEC-3] .. [ID-CODEC-5]; checks apply in the order the spec lists them.
    pub fn decode(s: &str) -> Result<Self, Reason> {
        const MALFORMED: Reason = Reason("id.malformed");
        if s.chars().any(char::is_whitespace) || s.matches('_').count() != 1 {
            return Err(MALFORMED);
        }
        let (prefix, body) = s.split_once('_').ok_or(MALFORMED)?;
        if prefix.is_empty() || body.is_empty() {
            return Err(MALFORMED);
        }
        // [ID-CODEC-4] look-alike letters
        let mut values = Vec::with_capacity(body.len());
        for c in body.chars() {
            let c = match c.to_ascii_lowercase() {
                'i' | 'l' => '1',
                'o' => '0',
                c => c,
            };
            let v = ALPHABET
                .iter()
                .position(|&a| a as char == c)
                .ok_or(MALFORMED)?;
            values.push(v as u64);
        }
        let kind = ObjectKind::from_prefix(prefix).ok_or(Reason("id.unknown_prefix"))?;
        if values[0] == 0 {
            return Err(Reason("id.non_canonical"));
        }
        if values.len() > 13 {
            return Err(Reason("id.too_long"));
        }
        let number = values.iter().fold(0u128, |n, &v| n * 32 + u128::from(v));
        if number > u128::from(MAX_NUMBER) {
            return Err(Reason("id.out_of_range"));
        }
        Ok(Self {
            kind,
            number: number as u64,
        })
    }

    /// [ID-CODEC-6]
    pub fn decode_of_kind(s: &str, expected: ObjectKind) -> Result<Self, Reason> {
        let id = Self::decode(s)?;
        if id.kind != expected {
            return Err(Reason("id.wrong_kind"));
        }
        Ok(id)
    }
}

/// [KEY-1] .. [KEY-3]: returns the key in NFC.
pub fn validate_key(s: &str) -> Result<String, Reason> {
    let key: String = s.nfc().collect();
    if key.is_empty() {
        return Err(Reason("key.empty"));
    }
    if key.len() > 1024 {
        return Err(Reason("key.too_long"));
    }
    if key
        .chars()
        .any(|c| crate::ucd::gc(c) == GeneralCategory::Control)
    {
        return Err(Reason("key.control_char"));
    }
    let edge = |c: Option<char>| c.is_some_and(is_white_space);
    if edge(key.chars().next()) || edge(key.chars().next_back()) {
        return Err(Reason("key.edge_whitespace"));
    }
    Ok(key)
}

/// [REF-1]
pub fn validate_batch_ref(s: &str) -> Result<&str, Reason> {
    let ok = s.strip_prefix('@').is_some_and(|rest| {
        (1..=64).contains(&rest.len())
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    });
    if ok {
        Ok(s)
    } else {
        Err(Reason("ref.malformed"))
    }
}

/// [GRAPH-NAME-1] [GRAPH-NAME-2]
pub fn validate_graph_name(s: &str) -> Result<&str, Reason> {
    let b = s.as_bytes();
    let well_formed = (1..=63).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-');
    if !well_formed {
        return Err(Reason("graph_name.malformed"));
    }
    let reserved = matches!(s, "con" | "prn" | "aux" | "nul")
        || (s.len() == 4
            && (s.starts_with("com") || s.starts_with("lpt"))
            && b[3].is_ascii_digit());
    if reserved {
        return Err(Reason("graph_name.reserved"));
    }
    Ok(s)
}

/// [ACTOR-NAME-1]
pub fn validate_actor_name(s: &str) -> Result<&str, Reason> {
    let b = s.as_bytes();
    let ok = (1..=128).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'@' | b'-'));
    if ok {
        Ok(s)
    } else {
        Err(Reason("actor_name.malformed"))
    }
}
