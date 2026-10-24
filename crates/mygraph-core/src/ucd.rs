//! Unicode Character Database helpers needed by the spec (spec/README.md §4) that the standard
//! library does not provide in the required form.

use unicode_general_category::{GeneralCategory as Gc, get_general_category};

pub(crate) fn gc(c: char) -> Gc {
    get_general_category(c)
}

pub(crate) fn is_letter(c: char) -> bool {
    matches!(
        gc(c),
        Gc::UppercaseLetter
            | Gc::LowercaseLetter
            | Gc::TitlecaseLetter
            | Gc::ModifierLetter
            | Gc::OtherLetter
    )
}

pub(crate) fn is_mark(c: char) -> bool {
    matches!(
        gc(c),
        Gc::NonspacingMark | Gc::SpacingMark | Gc::EnclosingMark
    )
}

pub(crate) fn is_number(c: char) -> bool {
    matches!(
        gc(c),
        Gc::DecimalNumber | Gc::LetterNumber | Gc::OtherNumber
    )
}

pub(crate) fn is_upper_or_title(c: char) -> bool {
    matches!(gc(c), Gc::UppercaseLetter | Gc::TitlecaseLetter)
}

/// `White_Space` property (PropList.txt).
pub(crate) fn is_white_space(c: char) -> bool {
    matches!(c,
        '\u{0009}'..='\u{000D}' | '\u{0020}' | '\u{0085}' | '\u{00A0}' | '\u{1680}'
        | '\u{2000}'..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}')
}

/// Simple case folding: CaseFolding.txt entries with status C or S.
///
/// For almost every code point this equals the single-code-point lowercase mapping; the table
/// lists the C/S entries where it does not.
pub(crate) fn simple_fold(c: char) -> char {
    match c {
        '\u{00B5}' => '\u{03BC}',
        '\u{017F}' => 's',
        '\u{0345}' | '\u{1FBE}' => '\u{03B9}',
        '\u{03C2}' => '\u{03C3}',
        '\u{03D0}' => '\u{03B2}',
        '\u{03D1}' => '\u{03B8}',
        '\u{03D5}' => '\u{03C6}',
        '\u{03D6}' => '\u{03C0}',
        '\u{03F0}' => '\u{03BA}',
        '\u{03F1}' => '\u{03C1}',
        '\u{03F5}' => '\u{03B5}',
        '\u{1C80}' => '\u{0432}',
        '\u{1C81}' => '\u{0434}',
        '\u{1C82}' => '\u{043E}',
        '\u{1C83}' => '\u{0441}',
        '\u{1C84}' | '\u{1C85}' => '\u{0442}',
        '\u{1C86}' => '\u{044A}',
        '\u{1C87}' => '\u{0463}',
        '\u{1C88}' => '\u{A64B}',
        '\u{1E9B}' => '\u{1E61}',
        '\u{1E9E}' => '\u{00DF}',
        // Cherokee folds to the uppercase letters.
        '\u{13F8}'..='\u{13FD}' => char::from_u32(c as u32 - 8).unwrap_or(c),
        '\u{AB70}'..='\u{ABBF}' => char::from_u32(c as u32 - 0xAB70 + 0x13A0).unwrap_or(c),
        // No C or S entry (only F / T entries).
        '\u{0130}' => '\u{0130}',
        _ => single(c.to_lowercase()).unwrap_or(c),
    }
}

/// Simple uppercase mapping (UnicodeData.txt field 12).
pub(crate) fn simple_upper(c: char) -> char {
    single(c.to_uppercase()).unwrap_or(c)
}

/// Simple lowercase mapping (UnicodeData.txt field 13).
pub(crate) fn simple_lower(c: char) -> char {
    match c {
        '\u{0130}' => 'i',
        _ => single(c.to_lowercase()).unwrap_or(c),
    }
}

fn single(mut it: impl Iterator<Item = char>) -> Option<char> {
    let first = it.next()?;
    if it.next().is_none() {
        Some(first)
    } else {
        None
    }
}
