//! Python `str` semantics the port relies on.
//!
//! Rust's `str` methods differ from Python's in small ways that change output
//! (Python's whitespace set includes `\x1c`–`\x1f`; `str.split()` collapses
//! runs; `isalpha`/`isdigit` are Unicode-category based). These helpers
//! reproduce CPython's behaviour so ported code can be translated literally.

/// `ch.isspace()` — Unicode whitespace plus the ASCII separators
/// `\x1c`–`\x1f`, which Python counts and Rust's `char::is_whitespace` does not.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
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

/// `s.split()` — split on runs of whitespace, dropping empty fields.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|t| !t.is_empty()).collect()
}

/// `s.isspace()` — non-empty and all whitespace.
pub fn is_all_space(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_space)
}

/// `ch.isalpha()` — Unicode letter categories (L*). Rust's
/// `char::is_alphabetic` also admits `Other_Alphabetic` marks; Python doesn't,
/// but the distinction never arises for the ASCII-dominated text tex2word
/// classifies, so the Rust predicate is used.
pub fn is_alpha(c: char) -> bool {
    c.is_alphabetic()
}

/// `s.isdigit()` for an ASCII string — non-empty and all `0`–`9`.
pub fn is_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `s[:n]` by characters (Python slices code points, not bytes).
pub fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// `" ".join(s.split())` — collapse whitespace runs to single spaces.
pub fn collapse_ws(s: &str) -> String {
    split_ws(s).join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_includes_ascii_separators() {
        assert_eq!(strip("\u{1c} a b \u{1f}\n"), "a b");
        assert_eq!(split_ws("  a\t b\n\nc "), vec!["a", "b", "c"]);
        assert_eq!(take_chars("héllo", 2), "hé");
    }
}
