//! Python `str` character predicates, from tables generated off CPython's
//! Unicode database (see `tools/golden/gen_walker_tables.py`).
//!
//! pylatexenc tokenises with `str.isspace()`/`str.isalpha()` and the regex
//! `\w` class; these differ from Rust's `char` methods at the edges, so the
//! walker uses the exact Python sets.

use crate::unicode_tables::{ALNUM, ALPHA, SPACE};

fn in_ranges(table: &[(u32, u32)], c: char) -> bool {
    let cp = c as u32;
    table
        .binary_search_by(|&(lo, hi)| {
            if hi < cp {
                std::cmp::Ordering::Less
            } else if lo > cp {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// `c.isspace()`.
pub fn is_space(c: char) -> bool {
    if c.is_ascii() {
        return matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c' | '\x1c'..='\x1f');
    }
    in_ranges(SPACE, c)
}

/// `c.isalpha()`.
pub fn is_alpha(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphabetic();
    }
    in_ranges(ALPHA, c)
}

/// `c.isalnum()`.
pub fn is_alnum(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphanumeric();
    }
    in_ranges(ALNUM, c)
}

/// Regex `\w` (Unicode): `isalnum()` or `_`.
pub fn is_word(c: char) -> bool {
    c == '_' || is_alnum(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicates_match_python_spot_checks() {
        // isspace: ASCII separators count, ZWSP does not
        for c in ['\u{1c}', '\u{85}', '\u{a0}', '\u{2028}', '\u{3000}'] {
            assert!(is_space(c), "{c:?}");
        }
        assert!(!is_space('\u{200b}'));
        // isalpha: letters yes; Nl numerals and combining marks no (Rust's
        // is_alphabetic says yes to both)
        assert!(is_alpha('é') && is_alpha('中') && is_alpha('ß'));
        assert!(!is_alpha('\u{2167}'), "ROMAN NUMERAL EIGHT is Nl");
        assert!(!is_alpha('\u{0345}'), "COMBINING YPOGEGRAMMENI is a mark");
        assert!('\u{2167}'.is_alphabetic() && '\u{0345}'.is_alphabetic());
        // isalnum includes numerics
        assert!(is_alnum('²') && is_alnum('\u{2167}') && is_word('_'));
        assert!(!is_word('-'));
    }
}
