//! Name keys — spec/model/normalization.md §2.

use crate::Reason;
use crate::ucd::{gc, is_white_space, simple_fold};
use unicode_general_category::GeneralCategory as Gc;
use unicode_normalization::UnicodeNormalization;

/// [NORM-NAME-1] The matching form of a name or alias.
pub fn name_key(s: &str) -> String {
    // Steps 1-4: NFKD, drop format and non-space control characters, simple case folding, NFKD.
    let folded: String = s
        .nfkd()
        .filter(|&c| match gc(c) {
            Gc::Format => false,
            Gc::Control => is_white_space(c),
            _ => true,
        })
        .map(simple_fold)
        .collect();

    // Steps 5-8 in one pass over the re-decomposed text.
    let mut out = String::with_capacity(folded.len());
    let mut base: Option<char> = None;
    for c in folded.nfkd() {
        if gc(c) == Gc::NonspacingMark {
            if base.is_some_and(|b| removes_mark(b, c)) {
                continue; // step 5
            }
            out.push(c);
            continue;
        }
        base = Some(c);
        match c {
            c if is_quote(c) => {}                                 // step 7
            c if is_dash(c) || is_white_space(c) => out.push(' '), // steps 7-8
            c => match extra_fold(c) {
                Some(rep) => out.push_str(rep), // step 6
                None => out.push(c),
            },
        }
    }

    // Step 8: collapse and trim spaces; step 9: NFC.
    out.split(' ')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .nfc()
        .collect()
}

/// [NORM-NAME-5] Validates a node name or alias by its name key.
pub fn check_name(s: &str) -> Result<(), Reason> {
    if name_key(s).is_empty() {
        Err(Reason("name.empty_key"))
    } else {
        Ok(())
    }
}

/// [NORM-NAME-2]
fn removes_mark(base: char, mark: char) -> bool {
    matches!(base, '\u{0000}'..='\u{02AF}' | '\u{0370}'..='\u{03FF}' | '\u{1E00}'..='\u{1FFF}')
        || (mark == '\u{0308}' && base == '\u{0435}')
}

/// [NORM-NAME-3]
fn extra_fold(c: char) -> Option<&'static str> {
    Some(match c {
        '\u{00DF}' => "ss",
        '\u{00E6}' => "ae",
        '\u{00F0}' | '\u{0111}' => "d",
        '\u{00F8}' => "o",
        '\u{00FE}' => "th",
        '\u{0127}' => "h",
        '\u{0131}' => "i",
        '\u{0142}' => "l",
        '\u{0153}' => "oe",
        '\u{0167}' => "t",
        _ => return None,
    })
}

/// [NORM-NAME-4] quote table
fn is_quote(c: char) -> bool {
    matches!(
        c,
        '\u{0022}'
            | '\u{0027}'
            | '\u{0060}'
            | '\u{00AB}'
            | '\u{00BB}'
            | '\u{02B9}'
            | '\u{02BB}'
            | '\u{02BC}'
            | '\u{02BD}'
            | '\u{2018}'..='\u{201F}' | '\u{2032}' | '\u{2039}' | '\u{203A}'
    )
}

/// [NORM-NAME-4] dash table
fn is_dash(c: char) -> bool {
    matches!(
        c,
        '\u{002D}' | '\u{005F}' | '\u{2010}' | '\u{2012}'
            ..='\u{2015}' | '\u{2212}' | '\u{2E3A}' | '\u{2E3B}'
    )
}
