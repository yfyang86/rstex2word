//! Python `str` semantics the port relies on.
//!
//! Rust's `str`/`char` methods differ from Python's in small ways that change
//! output (Python's whitespace set includes `\x1c`–`\x1f`; `isalpha` excludes
//! `Nl` letters and combining marks; `str.split()` collapses runs). The
//! character predicates here use range tables generated from CPython's own
//! Unicode database (`tools/golden/gen_walker_tables.py`), so ported code can
//! be translated literally.

use crate::unicode_tables::{ALNUM, ALPHA, DECIMAL, SPACE};

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

/// `c.isdecimal()` — the regex `\d` class.
pub fn is_decimal(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    in_ranges(DECIMAL, c)
}

/// Regex `\w` (Unicode): `isalnum()` or `_`.
pub fn is_word(c: char) -> bool {
    c == '_' || is_alnum(c)
}

/// `s.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `s.lstrip()`.
pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

/// `s.rstrip()`.
pub fn rstrip(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `s.strip(chars)`.
pub fn strip_chars<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_matches(|c| chars.contains(c))
}

/// `s.lstrip(chars)`.
pub fn lstrip_chars<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_start_matches(|c| chars.contains(c))
}

/// `s.rstrip(chars)`.
pub fn rstrip_chars<'a>(s: &'a str, chars: &str) -> &'a str {
    s.trim_end_matches(|c| chars.contains(c))
}

/// `s.split()` — split on runs of whitespace, dropping empty fields.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|t| !t.is_empty()).collect()
}

/// `s.isspace()` — non-empty and all whitespace.
pub fn is_all_space(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_space)
}

/// `s.isdigit()` for the ASCII digits tex2word tests — non-empty and all `0`–`9`.
pub fn is_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `s.isalpha()` — non-empty and all letters.
pub fn is_all_alpha(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_alpha)
}

/// `s[:n]` by characters (Python slices code points, not bytes).
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `s[n:]` by characters.
pub fn skip_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

/// `len(s)` — the length in code points.
pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// `" ".join(s.split())` — collapse whitespace runs to single spaces.
pub fn collapse_ws(s: &str) -> String {
    split_ws(s).join(" ")
}

/// `s.lower()`.
pub fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// `s.upper()`.
pub fn upper(s: &str) -> String {
    s.to_uppercase()
}

/// `s.capitalize()` — first char upper-cased, the rest lower-cased.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_includes_ascii_separators() {
        assert_eq!(strip("\u{1c} a b \u{1f}\n"), "a b");
        assert_eq!(split_ws("  a\t b\n\nc "), vec!["a", "b", "c"]);
        assert_eq!(take_chars("héllo", 2), "hé");
        assert_eq!(skip_chars("héllo", 2), "llo");
    }

    #[test]
    fn predicates_match_python_spot_checks() {
        for c in ['\u{1c}', '\u{85}', '\u{a0}', '\u{2028}', '\u{3000}'] {
            assert!(is_space(c), "{c:?}");
        }
        assert!(!is_space('\u{200b}'));
        assert!(is_alpha('é') && is_alpha('中') && is_alpha('ß'));
        // Rust's is_alphabetic admits these; Python's isalpha doesn't
        assert!(!is_alpha('\u{2167}'), "ROMAN NUMERAL EIGHT is Nl");
        assert!(!is_alpha('\u{0345}'), "COMBINING YPOGEGRAMMENI is a mark");
        assert!(is_alnum('²') && is_alnum('\u{2167}') && is_word('_'));
        assert!(is_decimal('٣') && !is_decimal('²'), "Nd only");
        assert!(!is_word('-'));
    }
}
