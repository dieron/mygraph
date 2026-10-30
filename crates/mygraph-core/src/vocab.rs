//! Vocabulary naming — spec/model/normalization.md §3.

use crate::Reason;
use crate::engine::OpError;
use crate::model::Naming;
use crate::ucd::{
    gc, is_letter, is_mark, is_number, is_upper_or_title, simple_lower, simple_upper,
};
use serde_json::json;
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

/// A name the graph's naming policy refuses ([VOCAB-1], [VOCAB-5]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameError {
    pub reason: Reason,
    pub given: String,
    /// For `vocab.not_styled`: the spelling the policy expects.
    pub styled: Option<String>,
}

impl NameError {
    /// The error for the request field that held the name.
    pub fn at(&self, field: &str) -> OpError {
        let message = match &self.styled {
            Some(s) => format!(
                "{field}: {:?} does not follow this graph's naming style; use {s:?}",
                self.given
            ),
            None => format!("{field}: {:?}: {}", self.given, self.reason.0),
        };
        OpError::new(self.reason.0, message)
            .details(json!({ "field": field, "given": self.given, "expected": self.styled }))
    }
}

/// [VOCAB-1] the name as stored and looked up, if the policy accepts it: `input` in NFC, never
/// rewritten otherwise.
pub fn check(kind: VocabKind, input: &str, naming: Naming) -> Result<String, NameError> {
    let err = |reason, styled| NameError {
        reason,
        given: input.to_owned(),
        styled,
    };
    match naming {
        Naming::Free => validate_verbatim(input).map_err(|r| err(r, None)),
        Naming::Styled => {
            let styled = canonicalize(kind, input).map_err(|r| err(r, None))?;
            let given: String = input.nfc().collect();
            if given == styled {
                Ok(given)
            } else {
                Err(err(Reason("vocab.not_styled"), Some(styled)))
            }
        }
    }
}

/// [VOCAB-2] .. [VOCAB-4] the styled spelling of a name: what `check` asks for under
/// `naming: styled`.
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

/// [VOCAB-5] validation of a name taken as given (`naming: free`); returns the NFC form.
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

// ----------------------------------------------------------------------------------------
// Similarity — spec/algorithms/vocabulary-similarity.md
// ----------------------------------------------------------------------------------------

/// [VSIM-1] lowercase words.
fn comparison_words(s: &str) -> Vec<Vec<char>> {
    split_words(s)
        .iter()
        .map(|w| w.chars().map(simple_lower).collect())
        .collect()
}

/// Levenshtein distance in code points.
fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// [VSIM-2] score from 0 to 1000.
pub fn similarity(a: &str, b: &str) -> u32 {
    let (wa, wb) = (comparison_words(a), comparison_words(b));
    let (ja, jb): (Vec<char>, Vec<char>) = (wa.concat(), wb.concat());
    let len = ja.len().max(jb.len());
    let chars = (1000 * (len - levenshtein(&ja, &jb)))
        .checked_div(len)
        .unwrap_or(0);
    let (sa, sb): (std::collections::BTreeSet<_>, std::collections::BTreeSet<_>) =
        (wa.into_iter().collect(), wb.into_iter().collect());
    let union = sa.union(&sb).count();
    let words = (1000 * sa.intersection(&sb).count())
        .checked_div(union)
        .unwrap_or(0);
    let (short, long) = if ja.len() <= jb.len() {
        (&ja, &jb)
    } else {
        (&jb, &ja)
    };
    let prefix = if short.len() >= 3 && long.starts_with(short) {
        700
    } else {
        0
    };
    chars.max(words).max(prefix) as u32
}

/// [VSIM-3] [VSIM-4] up to 5 similar items with their usage counts, in report order.
pub fn similar<'a>(
    new: &str,
    vocabulary: impl IntoIterator<Item = (&'a str, usize)>,
) -> Vec<(&'a str, usize)> {
    let mut found: Vec<(u32, &str, usize)> = vocabulary
        .into_iter()
        .filter(|(name, count)| *count > 0 && *name != new)
        .map(|(name, count)| (similarity(new, name), name, count))
        .filter(|(score, _, _)| *score >= 600)
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then(b.2.cmp(&a.2)).then(a.1.cmp(b.1)));
    found
        .into_iter()
        .take(5)
        .map(|(_, name, count)| (name, count))
        .collect()
}
