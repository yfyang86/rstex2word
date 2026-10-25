//! LaTeX math AST -> OMML (port of `mathml/omml.py`).

use tex2word_ir::pystr;

/// Detect a `\begin{`/`\end{` environment delimiter starting at byte `i`.
///
/// Returns `(env_depth_change, token_length)` -- `(1, 6)` for `\begin`,
/// `(-1, 4)` for `\end`, `(0, 0)` otherwise. The next char must exist and be
/// non-alphabetic, which distinguishes `\begin{x}` from macros like
/// `\beginner`.
pub fn env_token(s: &str, i: usize) -> (i64, usize) {
    let next_alpha = |k: usize| s[k..].chars().next().map(pystr::is_alpha);
    if s[i..].starts_with("\\begin") && next_alpha(i + 6) == Some(false) {
        return (1, 6);
    }
    if s[i..].starts_with("\\end") && next_alpha(i + 4) == Some(false) {
        return (-1, 4);
    }
    (0, 0)
}

/// Split `latex` on `sep` at brace depth 0 and outside `\begin…\end`.
pub fn split_top_level(latex: &str, sep: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth: i64 = 0;
    // \begin..\end nesting: a separator inside an environment isn't top-level
    let mut env: i64 = 0;
    let n = latex.len();
    let mut i = 0;
    let mut buf = String::new();
    while i < n {
        let (denv, tlen) = env_token(latex, i);
        if tlen != 0 {
            env = (env + denv).max(0);
            buf.push_str(&latex[i..i + tlen]);
            i += tlen;
            continue;
        }
        let c = latex[i..].chars().next().unwrap();
        if c == '{' {
            depth += 1;
            buf.push(c);
        } else if c == '}' {
            depth -= 1;
            buf.push(c);
        } else if depth == 0 && env == 0 && latex[i..].starts_with(sep) {
            parts.push(std::mem::take(&mut buf));
            i += sep.len();
            continue;
        } else {
            buf.push(c);
        }
        i += c.len_utf8();
    }
    parts.push(buf);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_outside_groups_and_envs() {
        assert_eq!(
            split_top_level(r"a\\{b\\c}\\\begin{x}d\\e\end{x}\\f", r"\\"),
            vec!["a", r"{b\\c}", r"\begin{x}d\\e\end{x}", "f"]
        );
        assert_eq!(env_token(r"\beginner", 0), (0, 0));
        assert_eq!(env_token(r"\begin", 0), (0, 0));
    }
}
