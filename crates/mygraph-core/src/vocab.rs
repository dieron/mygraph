//! Vocabulary canonicalization — spec/model/normalization.md §3.

use crate::Reason;
use crate::ucd::{
    gc, is_letter, is_mark, is_number, is_upper_or_title, simple_lower, simple_upper,
};
use unicode_general_category::GeneralCategory as Gc;
use unicode_normalization::UnicodeNormalization;

/// The three kinds of vocabulary items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VocabKind {
    /// PascalCase
    NodeType,
    /// UPPER_SNAKE
    LinkType,
    /// lower_snake
    PropKey,
}

impl VocabKind {
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "node_type" => Some(Self::NodeType),
            "link_type" => Some(Self::LinkType),
            "prop_key" => Some(Self::PropKey),
            _ => None,
        }
    }
}

/// [VOCAB-1] [VOCAB-4] canonical form when `normalize_types` is on.
pub fn canonicalize(kind: VocabKind, input: &str) -> Result<String, Reason> {
    let words = split_words(input);
    let out = match kind {
        VocabKind::NodeType => words
            .iter()
            .flat_map(|w| {
                let mut cs = w.chars();
                cs.next()
                    .map(simple_upper)
                    .into_iter()
                    .chain(cs.map(simple_lower))
            })
            .collect(),
        VocabKind::LinkType => join(&words, simple_upper),
        VocabKind::PropKey => join(&words, simple_lower),
    };
    validate(&out, false)?;
    Ok(out)
}

/// [VOCAB-5] validation of an item taken as given (`normalize_types` off); returns the NFC form.
pub fn validate_verbatim(input: &str) -> Result<String, Reason> {
    let s: String = input.nfc().collect();
    validate(&s, true)?;
    Ok(s)
}

fn join(words: &[String], map: fn(char) -> char) -> String {
    words
        .iter()
        .map(|w| w.chars().map(map).collect::<String>())
        .collect::<Vec<_>>()
        .join("_")
}

/// [VOCAB-5]
fn validate(s: &str, verbatim: bool) -> Result<(), Reason> {
    let first = s.chars().next().ok_or(Reason("vocab.empty"))?;
    if s.chars().count() > 64 {
        return Err(Reason("vocab.too_long"));
    }
    if !is_letter(first) {
        return Err(Reason("vocab.bad_start"));
    }
    if verbatim && !s.chars().all(|c| is_word_char(c) || c == '_') {
        return Err(Reason("vocab.bad_char"));
    }
    Ok(())
}

fn is_word_char(c: char) -> bool {
    is_letter(c) || is_mark(c) || is_number(c)
}

/// [VOCAB-2] NFKC, then split on separators and case transitions.
fn split_words(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.nfkc().collect();
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None; // last non-mark code point of the current word
    for (i, &c) in chars.iter().enumerate() {
        if !is_word_char(c) {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev = None;
            continue;
        }
        if is_mark(c) {
            cur.push(c); // [VOCAB-2] 3: marks never start a boundary
            continue;
        }
        if let Some(a) = prev {
            let next = chars.get(i + 1).copied();
            let boundary = (gc(a) == Gc::LowercaseLetter && is_upper_or_title(c))
                || (is_upper_or_title(a)
                    && is_upper_or_title(c)
                    && next.is_some_and(|n| gc(n) == Gc::LowercaseLetter))
                || (is_number(a) && is_upper_or_title(c));
            if boundary {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
        prev = Some(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}
