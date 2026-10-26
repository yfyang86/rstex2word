//! Python `str` semantics the port relies on.
//!
//! Rust's `str`/`char` methods differ from Python's in small ways that change
//! output (Python's whitespace set includes `\x1c`–`\x1f`; `isalpha` excludes
//! `Nl` letters and combining marks; `str.split()` collapses runs). The
//! character predicates here use range tables generated from CPython's own
//! Unicode database (`tools/golden/gen_walker_tables.py`), so ported code can
//! be translated literally.

use crate::unicode_norm::{CCC, COMPOSE, DECOMP};
use crate::unicode_tables::{ALNUM, ALPHA, DECIMAL, DIGIT, SPACE};

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
        return matches!(
            c,
            ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c' | '\x1c'..='\x1f'
        );
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

/// `c.isdigit()` (decimal digits plus other digits such as `²`).
pub fn is_digit(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    in_ranges(DIGIT, c)
}

/// Regex `\w` (Unicode): `isalnum()` or `_`.
pub fn is_word(c: char) -> bool {
    c == '_' || is_alnum(c)
}

/// The value of a Unicode decimal digit (`unicodedata.decimal(c)`).
pub fn decimal_value(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return c.to_digit(10);
    }
    let cp = c as u32;
    // every Nd run in the table is a block of ten digits starting at 0
    DECIMAL
        .iter()
        .find(|&&(lo, hi)| lo <= cp && cp <= hi)
        .map(|&(lo, _)| (cp - lo) % 10)
}

/// `int(s)` for a decimal string: optional surrounding whitespace and sign,
/// any Unicode decimal digits, and `_` separators between digits.
pub fn parse_int(s: &str) -> Option<i64> {
    let t = strip(s);
    let (neg, digits) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let mut v: i64 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        v = v.checked_mul(10)?.checked_add(decimal_value(c)? as i64)?;
    }
    Some(if neg { -v } else { v })
}

/// `float(s)` for a string: surrounding whitespace, an optional sign,
/// `inf`/`infinity`/`nan` (any case), or a decimal literal whose digits may be
/// any Unicode decimal digits with single `_` separators between them.
pub fn parse_float(s: &str) -> Option<f64> {
    let t = strip(s);
    let (sign, body) = match t.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", t.strip_prefix('+').unwrap_or(t)),
    };
    let lower = body.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "infinity" | "nan") {
        return format!("{sign}{lower}").parse().ok();
    }
    // normalise digits (Unicode Nd -> ASCII), validating `_` placement
    let chars: Vec<char> = body.chars().collect();
    let mut norm = String::with_capacity(chars.len());
    for (k, &c) in chars.iter().enumerate() {
        if c == '_' {
            let digit = |o: Option<&char>| o.is_some_and(|&c| decimal_value(c).is_some());
            if !(k > 0 && digit(chars.get(k - 1)) && digit(chars.get(k + 1))) {
                return None;
            }
            continue;
        }
        match decimal_value(c) {
            Some(d) => norm.push(char::from_digit(d, 10).unwrap()),
            None if matches!(c, '.' | 'e' | 'E' | '+' | '-') => norm.push(c),
            None => return None,
        }
    }
    // grammar: (digits [. [digits]] | . digits) [(e|E) [sign] digits]
    let b = norm.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize| {
        let s = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - s
    };
    let int_len = digits(&mut i);
    let mut frac_len = 0;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        frac_len = digits(&mut i);
    }
    if int_len + frac_len == 0 {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return None;
        }
    }
    if i != b.len() {
        return None;
    }
    format!("{sign}{norm}").parse().ok()
}

/// Python's `round(x)` for a float: round half to even.
pub fn round_half_even(x: f64) -> f64 {
    x.round_ties_even()
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
        Some(c) => c
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
    }
}

// Hangul syllable algorithm constants (Unicode §3.12)
const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

/// The canonical combining class of `c` (`unicodedata.combining`).
pub fn combining(c: char) -> u8 {
    let cp = c as u32;
    match CCC.binary_search_by(|&(lo, hi, _)| {
        if hi < cp {
            std::cmp::Ordering::Less
        } else if lo > cp {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => CCC[i].2,
        Err(_) => 0,
    }
}

fn decompose_into(cp: u32, out: &mut Vec<u32>) {
    if (S_BASE..S_BASE + S_COUNT).contains(&cp) {
        let s = cp - S_BASE;
        out.push(L_BASE + s / N_COUNT);
        out.push(V_BASE + (s % N_COUNT) / T_COUNT);
        if s % T_COUNT != 0 {
            out.push(T_BASE + s % T_COUNT);
        }
        return;
    }
    match DECOMP.binary_search_by_key(&cp, |&(c, _)| c) {
        Ok(i) => {
            for &p in DECOMP[i].1 {
                decompose_into(p, out);
            }
        }
        Err(_) => out.push(cp),
    }
}

fn compose_pair(a: u32, b: u32) -> Option<u32> {
    if (L_BASE..L_BASE + L_COUNT).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        return Some(S_BASE + ((a - L_BASE) * V_COUNT + (b - V_BASE)) * T_COUNT);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&a)
        && (a - S_BASE) % T_COUNT == 0
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&b)
    {
        return Some(a + (b - T_BASE));
    }
    COMPOSE
        .binary_search_by(|&(x, y, _)| (x, y).cmp(&(a, b)))
        .ok()
        .map(|i| COMPOSE[i].2)
}

/// `unicodedata.normalize("NFC", s)`.
pub fn nfc(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    // canonical decomposition
    let mut d: Vec<u32> = Vec::with_capacity(s.len());
    for c in s.chars() {
        decompose_into(c as u32, &mut d);
    }
    let ccc = |cp: u32| char::from_u32(cp).map_or(0, combining);
    // canonical ordering: stable-sort each run of non-starters by class
    let mut i = 0;
    while i < d.len() {
        if ccc(d[i]) == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < d.len() && ccc(d[i]) != 0 {
            i += 1;
        }
        d[start..i].sort_by_key(|&cp| ccc(cp));
    }
    // canonical composition
    let mut out: Vec<u32> = Vec::with_capacity(d.len());
    let mut starter: Option<usize> = None;
    let mut last_ccc = 0u8;
    for cp in d {
        let cc = ccc(cp);
        if let Some(si) = starter {
            let unblocked = out.len() - 1 == si || (last_ccc != 0 && last_ccc < cc);
            if unblocked {
                if let Some(comp) = compose_pair(out[si], cp) {
                    out[si] = comp;
                    continue;
                }
            }
        }
        if cc == 0 {
            starter = Some(out.len());
        }
        last_ccc = cc;
        out.push(cp);
    }
    out.into_iter().filter_map(char::from_u32).collect()
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
        assert!(is_digit('²') && is_digit('٣') && !is_digit('½'));
        assert!(!is_word('-'));
    }

    #[test]
    fn nfc_matches_python() {
        assert_eq!(crate::unicode_norm::UNICODE_VERSION, "15.0.0");
        assert_eq!(nfc("e\u{301}"), "é");
        assert_eq!(nfc("a\u{323}\u{302}"), "ậ", "reordered then composed");
        assert_eq!(nfc("a\u{302}\u{323}"), "ậ");
        assert_eq!(nfc("\u{1100}\u{1161}\u{11A8}"), "각", "Hangul LVT");
        assert_eq!(nfc("\u{212B}"), "Å", "singleton decomposes");
        assert_eq!(
            nfc("\u{0344}"),
            "\u{308}\u{301}",
            "non-starter decomposition"
        );
        assert_eq!(nfc("\u{301}e"), "\u{301}e", "no starter before the mark");
        assert_eq!(nfc("i\u{307}\u{301}"), "i\u{307}\u{301}");
    }

    #[test]
    fn float_parsing_matches_python() {
        assert_eq!(parse_float(" 1_000.5e-1 "), Some(100.05));
        assert_eq!(parse_float("5."), Some(5.0));
        assert_eq!(parse_float(".5"), Some(0.5));
        assert_eq!(parse_float("-Infinity"), Some(f64::NEG_INFINITY));
        assert_eq!(parse_float("٣"), Some(3.0));
        for bad in ["", ".", "1__0", "_1", "1e", "0x10", "1.2.3", "e5", "in f"] {
            assert_eq!(parse_float(bad), None, "{bad:?}");
        }
        assert_eq!(round_half_even(2.5), 2.0);
        assert_eq!(round_half_even(3.5), 4.0);
    }
}
