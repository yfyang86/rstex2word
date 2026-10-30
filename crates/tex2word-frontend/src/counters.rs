//! Evaluate user-declared LaTeX counters in document order (port of
//! `frontend/counters.py`).
//!
//! `\newcounter{c}` declares a counter; `\setcounter`, `\addtocounter`,
//! `\stepcounter` and `\refstepcounter` change it; `\arabic{c}`,
//! `\roman`, `\Roman`, `\alph`, `\Alph`, `\fnsymbol`, `\value{c}`
//! and `\thec` print it. This pass runs after macro expansion (so a user macro
//! that steps and prints a counter has been inlined at each use), replaces the
//! printing commands with their value at that point, and removes the changing
//! commands. A `\newcounter{c}[p]` resets when the user counter `p` steps.
//!
//! Only counters the document declares are touched: LaTeX's own (section,
//! figure, enumi, page, ...) are numbered by Word and left as they are.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

use tex2word_ir::dict::Dict;
use tex2word_ir::pystr;
use tex2word_ir::re::Regex;

use crate::conditionals::{mask, unmask};

macro_rules! lazy_re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat));
    };
}

lazy_re!(
    DECL_RE,
    r"\\newcounter\s*\{([A-Za-z@]+)\}(?:\s*\[([A-Za-z@]+)\])?"
);
lazy_re!(
    TOKEN_RE,
    concat!(
        r"\\(newcounter|setcounter|addtocounter|stepcounter|refstepcounter|arabic|roman|Roman|",
        r"alph|Alph|fnsymbol|value)\s*\{\s*([A-Za-z@]+)\s*\}",
        r"|\\the([A-Za-z@]+)(?![A-Za-z@])"
    )
);
lazy_re!(OPT_PARENT_RE, r"\s*\[[A-Za-z@]+\]");
lazy_re!(VALUE_RE, r"\\value\s*\{\s*([A-Za-z@]+)\s*\}");
lazy_re!(TAIL_CS_RE, r"\\[A-Za-z@]+$");
const FNSYMBOLS: [&str; 10] = ["", "*", "†", "‡", "§", "¶", "‖", "**", "††", "‡‡"];

pub fn resolve_counters(source: &str) -> String {
    if !source.contains("\\newcounter") {
        return source.to_string();
    }
    let (masked, saved) = mask(source);
    let mut parents: Dict<Option<String>> = Dict::new();
    for m in DECL_RE.find_iter(&masked) {
        parents.insert(m.group_str(1), m.group(2).map(str::to_string));
    }
    let mut values: HashMap<String, i64> = parents.keys().map(|name| (name.clone(), 0)).collect();
    let mut out: Vec<Cow<str>> = Vec::new();
    let mut i = 0;
    loop {
        let Some(found) = TOKEN_RE.search_from(&masked, i) else {
            out.push(Cow::Borrowed(&masked[i..]));
            break;
        };
        let m = found;
        let (cmd, name, the) = (m.group_str(1), m.group_str(2), m.group(3));
        if let Some(the) = the {
            let Some(&v) = values.get(the) else {
                out.push(Cow::Borrowed(&masked[i..m.end()]));
                i = m.end();
                continue;
            };
            out.push(Cow::Borrowed(&masked[i..m.start()]));
            let text = glue(&out, v.to_string());
            out.push(Cow::Owned(text));
            i = m.end();
            continue;
        }
        if !values.contains_key(name) {
            out.push(Cow::Borrowed(&masked[i..m.end()]));
            i = m.end();
            continue;
        }
        out.push(Cow::Borrowed(&masked[i..m.start()]));
        let mut j = m.end();
        if cmd == "newcounter" {
            j = OPT_PARENT_RE
                .match_at(&masked, j)
                .map_or(j, |opt| opt.end());
        } else if matches!(cmd, "setcounter" | "addtocounter") {
            let Some((text, end)) = group(&masked, j) else {
                out.push(Cow::Borrowed(&masked[m.start()..j]));
                i = j;
                continue;
            };
            j = end;
            if let Some(n) = number(text, &values) {
                let v = values.get_mut(name).unwrap();
                // Python ints are unbounded; saturate instead of overflowing
                *v = if cmd == "setcounter" {
                    n
                } else {
                    v.saturating_add(n)
                };
            }
        } else if matches!(cmd, "stepcounter" | "refstepcounter") {
            let v = values.get_mut(name).unwrap();
            *v = v.saturating_add(1);
            for (child, parent) in parents.iter() {
                if parent.as_deref() == Some(name) {
                    values.insert(child.clone(), 0);
                }
            }
        } else {
            let text = glue(&out, format(values[name], cmd));
            out.push(Cow::Owned(text));
        }
        i = j;
    }
    unmask(&out.concat(), &saved)
}

fn group(s: &str, mut i: usize) -> Option<(&str, usize)> {
    // byte-wise: the characters tested are ASCII, which a UTF-8 continuation
    // byte never equals, so this matches Python's per-character loop
    let b = s.as_bytes();
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n') {
        i += 1;
    }
    if i >= b.len() || b[i] != b'{' {
        return None;
    }
    let mut depth = 0i64;
    for (k, &c) in b.iter().enumerate().skip(i) {
        if c == b'{' {
            depth += 1;
        } else if c == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some((&s[i + 1..k], k + 1));
            }
        }
    }
    None
}

fn number(text: &str, values: &HashMap<String, i64>) -> Option<i64> {
    let t = pystr::strip(text);
    if let Some(m) = VALUE_RE.fullmatch(t) {
        return values.get(m.group_str(1)).copied();
    }
    pystr::parse_int(t)
}

fn format(n: i64, cmd: &str) -> String {
    if matches!(cmd, "roman" | "Roman") {
        let s = roman(n);
        return if cmd == "roman" { s.to_lowercase() } else { s };
    }
    if matches!(cmd, "alph" | "Alph") {
        let s = letters(n);
        return if cmd == "alph" { s.to_lowercase() } else { s };
    }
    if cmd == "fnsymbol" {
        return if (0..FNSYMBOLS.len() as i64).contains(&n) {
            FNSYMBOLS[n as usize].to_string()
        } else {
            n.to_string()
        };
    }
    n.to_string()
}

fn roman(mut n: i64) -> String {
    if n <= 0 {
        return String::new();
    }
    let mut out = String::new();
    for (v, s) in [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ] {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

fn letters(mut n: i64) -> String {
    let mut out = String::new();
    while n > 0 {
        let r = (n - 1) % 26;
        n = (n - 1) / 26;
        out.insert(0, char::from(b'A' + r as u8));
    }
    out
}

/// A value right after a control word gets a separating space.
fn glue(out: &[Cow<str>], text: String) -> String {
    if text.chars().next().is_some_and(pystr::is_alnum)
        && TAIL_CS_RE.is_match(&out[out.len().saturating_sub(2)..].concat())
    {
        return format!(" {text}");
    }
    text
}

#[cfg(test)]
mod tests {
    // expected strings are the Python reference's output
    use super::*;

    #[test]
    fn counters_step_reset_and_print() {
        assert_eq!(
            resolve_counters(
                r"\newcounter{q}\newcounter{p}[q]\stepcounter{q}Q\theq.\stepcounter{p}\stepcounter{p}\thep{} \stepcounter{q}\theq.\thep{} \setcounter{q}{4}\addtocounter{q}{1}\roman{q} \Alph{q} \value{q}"
            ),
            r"Q1.2{} 2.0{} v E 5"
        );
        assert_eq!(
            resolve_counters(
                r"\newcounter{a}\newcounter{b}[a]\newcounter{c}[a]\stepcounter{b}\stepcounter{c}\stepcounter{a}\theb\thec"
            ),
            r"00"
        );
    }

    #[test]
    fn latex_counters_and_verbatim_are_left_alone() {
        assert_eq!(
            resolve_counters(r"\newcounter{c}\thesection \verb|\thec|"),
            r"\thesection \verb|\thec|"
        );
        assert_eq!(
            resolve_counters(
                r"\newcounter{a}[section]\stepcounter{a}\thea \stepcounter{section}\thea"
            ),
            r"1 \stepcounter{section}1"
        );
        assert_eq!(
            resolve_counters(r"\begin{comment}\newcounter{a}\end{comment}\thea"),
            r"\begin{comment}\newcounter{a}\end{comment}\thea"
        );
    }

    #[test]
    fn formats() {
        assert_eq!(
            resolve_counters(r"\newcounter{c}\setcounter{c}{1994}\roman{c} \Roman{c}"),
            r"mcmxciv MCMXCIV"
        );
        assert_eq!(
            resolve_counters(
                r"\newcounter{c}\setcounter{c}{702}\alph{c} \Alph{c} \setcounter{c}{703}\alph{c}"
            ),
            r"zz ZZ aaa"
        );
        assert_eq!(
            resolve_counters(
                r"\newcounter{c}\setcounter{c}{-3}\arabic{c} \roman{c} \alph{c} \fnsymbol{c}"
            ),
            r"-3   -3"
        );
        assert_eq!(
            resolve_counters(
                r"\newcounter{c}\setcounter{c}{0}\fnsymbol{c} \setcounter{c}{1}\fnsymbol{c} \setcounter{c}{2}\fnsymbol{c} \setcounter{c}{3}\fnsymbol{c} \setcounter{c}{4}\fnsymbol{c} \setcounter{c}{5}\fnsymbol{c} \setcounter{c}{6}\fnsymbol{c} \setcounter{c}{7}\fnsymbol{c} \setcounter{c}{8}\fnsymbol{c} \setcounter{c}{9}\fnsymbol{c} \setcounter{c}{10}\fnsymbol{c} "
            ),
            r" * † ‡ § ¶ ‖ ** †† ‡‡ 10 "
        );
    }

    #[test]
    fn set_and_add() {
        assert_eq!(
            resolve_counters(
                r"\newcounter{a}\newcounter{b}\setcounter{a}{5}\setcounter{b}{\value{a}}\theb"
            ),
            r"5"
        );
        assert_eq!(
            resolve_counters(r"\newcounter{a}\addtocounter{a}{3}\addtocounter{a}{-5}\thea"),
            r"-2"
        );
        assert_eq!(
            resolve_counters(r"\newcounter{c}\setcounter{c}{x}\thec \setcounter{c}{ 7 }\thec"),
            r"0 7"
        );
        assert_eq!(
            resolve_counters(r"\newcounter{c}\setcounter{c}{٣}\thec"),
            r"3"
        );
        assert_eq!(
            resolve_counters(r"\newcounter{c}\setcounter{c}\thec"),
            r"\setcounter{c}0"
        );
    }

    #[test]
    fn the_needs_a_whole_name() {
        assert_eq!(
            resolve_counters(r"\newcounter{a}\theab \thea@ \thea1"),
            r"\theab \thea@ 01"
        );
        assert_eq!(resolve_counters(r"\thea\newcounter{a}\thea"), r"00");
    }

    #[test]
    fn value_after_a_control_word_gets_a_space() {
        assert_eq!(
            resolve_counters(r"\newcounter{c}\stepcounter{c}\foo\thec"),
            r"\foo 1"
        );
        assert_eq!(
            resolve_counters(r"\newcounter{c}\stepcounter{c}\foo\fnsymbol{c}"),
            r"\foo*"
        );
        assert_eq!(resolve_counters(r"\newcounter{c}\foo\roman{c}x"), r"\foox");
        assert_eq!(
            resolve_counters(r"é\newcounter{a}日本\stepcounter{a}ü\thea中"),
            r"é日本ü1中"
        );
    }
}
