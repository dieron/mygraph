//! Text input — spec/model/normalization.md §1.

use unicode_normalization::UnicodeNormalization;

/// [NORM-TEXT-2] NFC form of an incoming string (not used for evidence content).
///
/// [NORM-TEXT-1] holds by construction: a Rust `str` cannot contain unpaired surrogates, so
/// `text.invalid_unicode` is reported where JSON is decoded.
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}
