//! LaTeX math AST -> OMML (port of `mathml/omml.py`).
//!
//! The direct LaTeX->OMML path (texmath-style): output lives under the `m:`
//! namespace and is embedded as native math runs in `document.xml`.

use std::sync::LazyLock;

use tex2word_ir::pystr;
use tex2word_ir::re::Regex;
use tex2word_ooxml::xml::{el, preserve_space, qn, sub, Element};

use crate::latex_math::{self as L, MNode, MathError, Row};

// strip equation-numbering / label noise before parsing a math line
static LABEL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\(?:label|tag|nonumber|notag)\b\s*(?:\{[^{}]*\})?"));

/// CJK ranges (Unified Ideographs + ext A, compat ideographs, kana, Hangul,
/// CJK symbols/punctuation and fullwidth forms).
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

/// Set an East-Asian font on math runs (`m:r`) whose `m:t` holds CJK, so Word
/// uses the document's CJK font for those glyphs. The `w:rPr` goes before
/// `m:t` (after any `m:rPr`); `w:rFonts` is first in it.
pub fn tag_cjk_runs(root: &mut Element, east_asia_font: &str) {
    let (m_t, m_r) = (qn("m:t"), qn("m:r"));
    let (w_rpr, w_rfonts, w_ea) = (qn("w:rPr"), qn("w:rFonts"), qn("w:eastAsia"));
    root.for_each_mut(&mut |r| {
        if !r.is(&m_r) {
            return;
        }
        let mut i = 0;
        while i < r.children.len() {
            let c = &r.children[i];
            if c.is(&m_t) && c.text.as_deref().is_some_and(|t| t.chars().any(is_cjk)) {
                let t_index = i;
                if r.find(&w_rpr).is_none() {
                    r.insert(t_index, el("w:rPr", &[])); // before m:t, after any m:rPr
                    i += 1;
                }
                let wrpr = r.find_mut(&w_rpr).unwrap();
                if wrpr.find(&w_rfonts).is_none() {
                    wrpr.insert(0, el("w:rFonts", &[])); // rFonts is first in CT_RPr
                }
                wrpr.find_mut(&w_rfonts).unwrap().set(&w_ea, east_asia_font);
            }
            i += 1;
        }
    });
}

// --------------------------------------------------------------------------- //
// Leaf runs
// --------------------------------------------------------------------------- //

fn run(text: &str, upright: bool, bold: bool, script: Option<&str>) -> Element {
    let mut r = el("m:r", &[]);
    if upright || bold || script.is_some() {
        let rpr = sub(&mut r, "m:rPr", &[]);
        if upright {
            sub(rpr, "m:nor", &[]);
        }
        if let Some(s) = script {
            sub(rpr, "m:scr", &[("m:val", s)]);
        }
        if bold {
            sub(rpr, "m:sty", &[("m:val", "b")]);
        } else if script.is_some() {
            sub(rpr, "m:sty", &[("m:val", "p")]);
        }
    }
    let t = sub(&mut r, "m:t", &[]);
    t.text = Some(text.to_string());
    preserve_space(t);
    r
}

// --------------------------------------------------------------------------- //
// Node emission
// --------------------------------------------------------------------------- //

/// Render a node to a list of sibling OMML elements.
fn emit(node: &MNode) -> Vec<Element> {
    match node {
        MNode::Lit(l) => {
            if l.text.is_empty() {
                return Vec::new();
            }
            vec![run(&l.text, l.upright, l.bold, l.script.as_deref())]
        }
        MNode::Row(r) | MNode::Group(r) => emit_row(r),
        other => vec![emit_single(other)],
    }
}

fn emit_row(r: &Row) -> Vec<Element> {
    r.0.iter().flat_map(emit).collect()
}

/// An OMML container element filled with the rendered node.
fn fill(tag: &str, node: Option<&MNode>) -> Element {
    let mut c = el(tag, &[]);
    if let Some(n) = node {
        for child in emit(n) {
            c.append(child);
        }
    }
    c
}

fn fill_row(tag: &str, row: &Row) -> Element {
    let mut c = el(tag, &[]);
    for child in emit_row(row) {
        c.append(child);
    }
    c
}

fn emit_single(node: &MNode) -> Element {
    match node {
        MNode::Frac(num, den) => {
            let mut f = el("m:f", &[]);
            f.append(fill_row("m:num", num));
            f.append(fill_row("m:den", den));
            f
        }
        MNode::Sup(base, s) => {
            let mut e = el("m:sSup", &[]);
            e.append(fill("m:e", Some(base)));
            e.append(fill("m:sup", Some(s)));
            e
        }
        MNode::Sub(base, s) => {
            let mut e = el("m:sSub", &[]);
            e.append(fill("m:e", Some(base)));
            e.append(fill("m:sub", Some(s)));
            e
        }
        MNode::SubSup(base, b, p) => {
            let mut e = el("m:sSubSup", &[]);
            e.append(fill("m:e", Some(base)));
            e.append(fill("m:sub", Some(b)));
            e.append(fill("m:sup", Some(p)));
            e
        }
        MNode::Sqrt(radicand) => {
            let mut r = el("m:rad", &[]);
            let rpr = sub(&mut r, "m:radPr", &[]);
            sub(rpr, "m:degHide", &[("m:val", "1")]);
            r.append(fill("m:deg", None));
            r.append(fill_row("m:e", radicand));
            r
        }
        MNode::Root(index, radicand) => {
            let mut r = el("m:rad", &[]);
            r.append(fill_row("m:deg", index));
            r.append(fill_row("m:e", radicand));
            r
        }
        MNode::Nary(n) => {
            let mut e = el("m:nary", &[]);
            let npr = sub(&mut e, "m:naryPr", &[]);
            sub(npr, "m:chr", &[("m:val", &n.op)]);
            sub(npr, "m:limLoc", &[("m:val", &n.limloc)]);
            if n.sub.is_none() {
                sub(npr, "m:subHide", &[("m:val", "1")]);
            }
            if n.sup.is_none() {
                sub(npr, "m:supHide", &[("m:val", "1")]);
            }
            e.append(fill("m:sub", n.sub.as_deref()));
            e.append(fill("m:sup", n.sup.as_deref()));
            e.append(fill("m:e", n.body.as_deref()));
            e
        }
        MNode::Accent(base, ch) => {
            let mut a = el("m:acc", &[]);
            let apr = sub(&mut a, "m:accPr", &[]);
            sub(apr, "m:chr", &[("m:val", ch)]);
            a.append(fill_row("m:e", base));
            a
        }
        MNode::Fenced(open, close, body) => {
            let mut d = el("m:d", &[]);
            let dpr = sub(&mut d, "m:dPr", &[]);
            sub(dpr, "m:begChr", &[("m:val", open)]);
            sub(dpr, "m:endChr", &[("m:val", close)]);
            d.append(fill_row("m:e", body));
            d
        }
        MNode::LimLow(base, lim) => {
            let mut ll = el("m:limLow", &[]);
            ll.append(fill("m:e", Some(base)));
            ll.append(fill("m:lim", Some(lim)));
            ll
        }
        MNode::GroupChr(body, ch, pos) => {
            let mut g = el("m:groupChr", &[]);
            let gpr = sub(&mut g, "m:groupChrPr", &[]);
            sub(gpr, "m:chr", &[("m:val", ch)]);
            sub(gpr, "m:pos", &[("m:val", pos)]);
            sub(
                gpr,
                "m:vertJc",
                &[("m:val", if pos == "top" { "bot" } else { "top" })],
            );
            g.append(fill_row("m:e", body));
            g
        }
        MNode::Matrix(rows, aligned) => {
            let mut m = el("m:m", &[]);
            let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
            if *aligned && ncols > 0 {
                add_alignment_mcs(&mut m, ncols);
            }
            for row in rows {
                let mr = sub(&mut m, "m:mr", &[]);
                // pad short rows to the declared column count (aligned)
                let count = if *aligned { ncols } else { row.len() };
                for c in 0..count {
                    mr.append(match row.get(c) {
                        Some(cell) => fill_row("m:e", cell),
                        None => fill("m:e", None),
                    });
                }
            }
            m
        }
        MNode::Lit(_) | MNode::Row(_) | MNode::Group(_) => unreachable!("handled by emit"),
    }
}

// --------------------------------------------------------------------------- //
// Public API
// --------------------------------------------------------------------------- //

fn omath(row: &Row) -> Element {
    let mut o = el("m:oMath", &[]);
    for child in emit_row(row) {
        o.append(child);
    }
    o
}

/// Render inline math to a single `m:oMath` element.
pub fn render_inline(latex: &str) -> Result<Element, MathError> {
    let cleaned = LABEL_RE.sub("", latex);
    let row = L::parse(pystr::strip(&cleaned))?;
    Ok(omath(&row))
}

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

/// Remove top-level `&` alignment markers from an align/aligned line (a `&`
/// inside a nested environment is left intact).
pub fn strip_align_markers(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let (mut depth, mut env): (i64, i64) = (0, 0);
    let mut i = 0;
    while i < line.len() {
        let (denv, tlen) = env_token(line, i);
        if tlen != 0 {
            env = (env + denv).max(0);
            out.push_str(&line[i..i + tlen]);
            i += tlen;
            continue;
        }
        let c = line[i..].chars().next().unwrap();
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
        }
        if c == '&' && depth == 0 && env == 0 {
            out.push(' ');
        } else {
            out.push(c);
        }
        i += c.len_utf8();
    }
    out
}

/// True if `line` has a top-level `&` alignment marker.
pub fn has_top_amp(line: &str) -> bool {
    let (mut depth, mut env): (i64, i64) = (0, 0);
    let mut i = 0;
    while i < line.len() {
        let (denv, tlen) = env_token(line, i);
        if tlen != 0 {
            env = (env + denv).max(0);
            i += tlen;
            continue;
        }
        let c = line[i..].chars().next().unwrap();
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
        } else if c == '&' && depth == 0 && env == 0 {
            return true;
        }
        i += c.len_utf8();
    }
    false
}

/// `m:mPr/m:mcs` column properties for the classic `array{rl}` look.
fn add_alignment_mcs(m: &mut Element, ncols: usize) {
    let mpr = sub(m, "m:mPr", &[]);
    let mcs = sub(mpr, "m:mcs", &[]);
    for i in 0..ncols {
        let mc = sub(mcs, "m:mc", &[]);
        let mcpr = sub(mc, "m:mcPr", &[]);
        sub(mcpr, "m:count", &[("m:val", "1")]);
        sub(
            mcpr,
            "m:mcJc",
            &[("m:val", if i % 2 == 0 { "right" } else { "left" })],
        );
    }
}

/// `&`-aligned lines -> one `m:oMath` holding a column-justified matrix.
fn aligned_matrix(lines: &[String]) -> Result<Element, MathError> {
    let rows: Vec<Vec<String>> = lines
        .iter()
        .map(|line| {
            split_top_level(line, "&")
                .iter()
                .map(|c| pystr::strip(c).to_string())
                .collect()
        })
        .collect();
    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut m = el("m:m", &[]);
    add_alignment_mcs(&mut m, ncols);
    for cells in &rows {
        let mut mr = el("m:mr", &[]);
        for i in 0..ncols {
            let cell = cells.get(i).map_or("", String::as_str);
            if cell.is_empty() {
                mr.append(fill("m:e", None));
            } else {
                mr.append(fill_row("m:e", &L::parse(cell)?));
            }
        }
        m.append(mr);
    }
    let mut o = el("m:oMath", &[]);
    o.append(m);
    Ok(o)
}

/// Render display math (possibly multi-line) to a list of `m:oMath`: one per
/// `\\` line, or -- with `collapse_align` and `&` markers -- a single
/// column-aligned matrix.
pub fn render_block_lines(latex: &str, collapse_align: bool) -> Result<Vec<Element>, MathError> {
    let cleaned = LABEL_RE.sub("", latex);
    let cleaned = pystr::strip(&cleaned);
    let lines: Vec<String> = split_top_level(cleaned, "\\\\")
        .iter()
        .map(|s| pystr::strip(s).to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return Err(MathError::unsupported("empty", "no math content"));
    }
    if collapse_align && lines.iter().any(|l| has_top_amp(l)) {
        return Ok(vec![aligned_matrix(&lines)?]);
    }
    lines
        .iter()
        .map(|line| {
            let stripped = strip_align_markers(line);
            Ok(omath(&L::parse(pystr::strip(&stripped))?))
        })
        .collect()
}

/// `latex_to_omml(latex, display)`: an `m:oMathPara` of lines, or an inline
/// `m:oMath`.
pub fn latex_to_omml(latex: &str, display: bool) -> Result<Element, MathError> {
    if display {
        let mut para = el("m:oMathPara", &[]);
        for line in render_block_lines(latex, true)? {
            para.append(line);
        }
        return Ok(para);
    }
    render_inline(latex)
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
