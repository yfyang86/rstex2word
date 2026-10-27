//! OMML -> LaTeX, the round-trip math reader (port of `mathml/omml_reader.py`).
//!
//! Parses an `m:oMath` element back into the math AST of [`crate::latex_math`]
//! and serializes that AST to LaTeX. It is the inverse of [`crate::omml`] for
//! the constructs that writer emits.

use tex2word_ir::pystr;
use tex2word_ooxml::xml::Element;

use crate::latex_math::{Lit, MNode, Nary, Row};
use crate::symbols;

const M: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

/// lxml's `ValueError` from `etree.QName(e)` on a comment or processing
/// instruction: the reference raises it for any non-element child it walks.
pub const QNAME_ERROR: &str =
    "Invalid input tag of type <class '_cython_3_2_9.cython_function_or_method'>";

/// `etree.QName(e).localname`, failing like lxml on a comment or PI.
pub fn localname(e: &Element) -> Result<&str, String> {
    if e.is_element() {
        Ok(&e.local)
    } else {
        Err(QNAME_ERROR.to_string())
    }
}

fn mq(name: &str) -> String {
    format!("{{{M}}}{name}")
}

/// The first `(cmd, glyph)` entry whose glyph is `glyph` (a reversed dict where
/// the first definition wins).
fn reverse(table: &'static [(&'static str, &'static str)], glyph: &str) -> Option<&'static str> {
    table.iter().find(|(_, g)| *g == glyph).map(|(c, _)| *c)
}

fn children_to_row(parent: Option<&Element>) -> Result<Row, String> {
    let mut items = Vec::new();
    if let Some(p) = parent {
        for child in &p.children {
            if let Some(node) = parse(child)? {
                items.push(node);
            }
        }
    }
    Ok(Row(items))
}

fn flat(parent: Option<&Element>) -> Result<MNode, String> {
    let mut row = children_to_row(parent)?;
    Ok(if row.0.len() == 1 {
        row.0.pop().unwrap()
    } else {
        MNode::Group(row)
    })
}

fn find<'a>(e: &'a Element, name: &str) -> Option<&'a Element> {
    e.find(&mq(name))
}

fn chr_val(pr: Option<&Element>, name: &str, default: &str) -> String {
    let Some(pr) = pr else {
        return default.to_string();
    };
    match pr.find(&mq(name)) {
        Some(el) => el.get(&mq("val")).unwrap_or(default).to_string(),
        None => default.to_string(),
    }
}

/// A non-empty child element list (`len(el)` counts every child node).
fn has_children(e: Option<&Element>) -> Option<&Element> {
    e.filter(|e| !e.children.is_empty())
}

fn parse(e: &Element) -> Result<Option<MNode>, String> {
    let node = match localname(e)? {
        "r" => MNode::Lit(parse_run(e)),
        "f" => MNode::Frac(
            children_to_row(find(e, "num"))?,
            children_to_row(find(e, "den"))?,
        ),
        "sSup" => MNode::Sup(
            Box::new(flat(find(e, "e"))?),
            Box::new(flat(find(e, "sup"))?),
        ),
        "sSub" => MNode::Sub(
            Box::new(flat(find(e, "e"))?),
            Box::new(flat(find(e, "sub"))?),
        ),
        "sSubSup" => MNode::SubSup(
            Box::new(flat(find(e, "e"))?),
            Box::new(flat(find(e, "sub"))?),
            Box::new(flat(find(e, "sup"))?),
        ),
        "rad" => {
            let pr = find(e, "radPr");
            let deg_hidden = pr.is_some() && chr_val(pr, "degHide", "") == "1";
            let deg = find(e, "deg");
            match deg {
                Some(d) if !deg_hidden && !d.children.is_empty() => {
                    MNode::Root(children_to_row(Some(d))?, children_to_row(find(e, "e"))?)
                }
                _ => MNode::Sqrt(children_to_row(find(e, "e"))?),
            }
        }
        "nary" => {
            let pr = find(e, "naryPr");
            let op = chr_val(pr, "chr", "∫");
            let sub = has_children(find(e, "sub"))
                .map(|s| flat(Some(s)))
                .transpose()?;
            let sup = has_children(find(e, "sup"))
                .map(|s| flat(Some(s)))
                .transpose()?;
            let body = has_children(find(e, "e"))
                .map(|s| flat(Some(s)))
                .transpose()?;
            MNode::Nary(Nary {
                op,
                body: body.map(Box::new),
                sub: sub.map(Box::new),
                sup: sup.map(Box::new),
                limloc: "undOvr".into(),
            })
        }
        "acc" => MNode::Accent(
            children_to_row(find(e, "e"))?,
            chr_val(find(e, "accPr"), "chr", "\u{0302}"),
        ),
        "d" => {
            let pr = find(e, "dPr");
            MNode::Fenced(
                chr_val(pr, "begChr", "("),
                chr_val(pr, "endChr", ")"),
                children_to_row(find(e, "e"))?,
            )
        }
        "limLow" => MNode::LimLow(
            Box::new(flat(find(e, "e"))?),
            Box::new(flat(find(e, "lim"))?),
        ),
        "limUpp" => MNode::LimUpp(
            Box::new(flat(find(e, "e"))?),
            Box::new(flat(find(e, "lim"))?),
        ),
        "sPre" => MNode::SPre(
            Box::new(flat(find(e, "e"))?),
            has_children(find(e, "sub"))
                .map(|s| flat(Some(s)).map(Box::new))
                .transpose()?,
            has_children(find(e, "sup"))
                .map(|s| flat(Some(s)).map(Box::new))
                .transpose()?,
        ),
        "borderBox" => MNode::BorderBox(children_to_row(find(e, "e"))?),
        "groupChr" => {
            let pr = find(e, "groupChrPr");
            MNode::GroupChr(
                children_to_row(find(e, "e"))?,
                chr_val(pr, "chr", "⏞"),
                chr_val(pr, "pos", "top"),
            )
        }
        "m" => {
            let mut rows = Vec::new();
            for mr in e.findall(&mq("mr")) {
                let mut cells = Vec::new();
                for cell in mr.findall(&mq("e")) {
                    cells.push(children_to_row(Some(cell))?);
                }
                rows.push(cells);
            }
            MNode::Matrix(rows, false)
        }
        "oMath" | "oMathPara" => MNode::Group(children_to_row(Some(e))?),
        _ => return Ok(None),
    };
    Ok(Some(node))
}

fn parse_run(e: &Element) -> Lit {
    let text = e
        .find(&mq("t"))
        .and_then(|t| t.text.clone())
        .unwrap_or_default();
    let rpr = e.find(&mq("rPr"));
    let upright = rpr.is_some_and(|r| r.find(&mq("nor")).is_some());
    let mut script = None;
    let mut bold = false;
    if let Some(rpr) = rpr {
        if let Some(scr) = rpr.find(&mq("scr")) {
            script = scr.get(&mq("val")).map(str::to_string);
        }
        if let Some(sty) = rpr.find(&mq("sty")) {
            if matches!(sty.get(&mq("val")), Some("b" | "bi")) {
                bold = true;
            }
        }
    }
    Lit {
        text,
        upright,
        bold,
        script,
    }
}

// ---- math AST -> LaTeX ---------------------------------------------------------

fn glyphs_to_latex(text: &str) -> String {
    let mut out = String::new();
    let mut buf = [0u8; 4];
    for ch in text.chars() {
        match reverse(symbols::SYMBOLS, ch.encode_utf8(&mut buf)) {
            Some(cmd) => {
                out.push('\\');
                out.push_str(cmd);
                out.push(' ');
            }
            None => out.push(ch),
        }
    }
    out
}

fn script_cmd(script: &str) -> Option<&'static str> {
    Some(match script {
        "double-struck" => "mathbb",
        "script" => "mathcal",
        "fraktur" => "mathfrak",
        "sans-serif" => "mathsf",
        "monospace" => "mathtt",
        _ => return None,
    })
}

fn lit_latex(lit: &Lit) -> String {
    let text = &lit.text;
    if text.is_empty() {
        return String::new();
    }
    if let Some(cmd) = lit.script.as_deref().and_then(script_cmd) {
        return format!("\\{cmd}{{{text}}}");
    }
    // genuine multi-letter upright text -> a function name or \mathrm{...}
    if lit.upright && pystr::char_len(text) > 1 && pystr::is_all_alpha(text) {
        if symbols::is_function(text) {
            return format!("\\{text} ");
        }
        return format!("\\mathrm{{{text}}}");
    }
    let body = glyphs_to_latex(text);
    if lit.bold {
        return format!("\\mathbf{{{body}}}");
    }
    body
}

/// A braced argument for sub/sup/frac/....
fn arg(node: &MNode) -> String {
    format!("{{{}}}", ast_to_latex(node))
}

fn row_latex(row: &Row) -> String {
    row.0.iter().map(ast_to_latex).collect()
}

/// Serialize a math AST node to LaTeX.
pub fn ast_to_latex(node: &MNode) -> String {
    match node {
        MNode::Lit(l) => lit_latex(l),
        MNode::Row(r) | MNode::Group(r) => row_latex(r),
        MNode::Frac(n, d) => format!("\\frac{{{}}}{{{}}}", row_latex(n), row_latex(d)),
        MNode::Sup(b, s) => format!("{}^{}", arg(b), arg(s)),
        MNode::Sub(b, s) => format!("{}_{}", arg(b), arg(s)),
        MNode::SubSup(b, sub, sup) => format!("{}_{}^{}", arg(b), arg(sub), arg(sup)),
        MNode::Sqrt(r) => format!("\\sqrt{{{}}}", row_latex(r)),
        MNode::Root(i, r) => format!("\\sqrt[{}]{{{}}}", row_latex(i), row_latex(r)),
        MNode::Nary(n) => {
            let mut op = format!("\\{} ", reverse(symbols::NARY, &n.op).unwrap_or("int"));
            if let Some(s) = &n.sub {
                op.push('_');
                op.push_str(&arg(s));
            }
            if let Some(s) = &n.sup {
                op.push('^');
                op.push_str(&arg(s));
            }
            if let Some(b) = &n.body {
                op.push(' ');
                op.push_str(&ast_to_latex(b));
            }
            op
        }
        MNode::Accent(base, ch) => {
            let cmd = reverse(symbols::ACCENTS, ch).unwrap_or("hat");
            format!("\\{cmd}{{{}}}", row_latex(base))
        }
        MNode::Fenced(open, close, body) => {
            let op = if open.is_empty() { "." } else { open };
            let cl = if close.is_empty() { "." } else { close };
            format!(
                "\\left{} {} \\right{}",
                delim(op),
                row_latex(body),
                delim(cl)
            )
        }
        MNode::LimLow(base, lim) => format!("{}_{}", ast_to_latex(base), arg(lim)),
        MNode::LimUpp(base, over) => {
            format!(
                "\\overset{{{}}}{{{}}}",
                ast_to_latex(over),
                ast_to_latex(base)
            )
        }
        MNode::SPre(base, sub, sup) => format!(
            "\\prescript{{{}}}{{{}}}{{{}}}",
            sub.as_deref().map(ast_to_latex).unwrap_or_default(),
            sup.as_deref().map(ast_to_latex).unwrap_or_default(),
            ast_to_latex(base)
        ),
        MNode::BorderBox(r) => format!("\\boxed{{{}}}", row_latex(r)),
        MNode::GroupChr(body, _, pos) => {
            let cmd = if pos == "top" {
                "overbrace"
            } else {
                "underbrace"
            };
            format!("\\{cmd}{{{}}}", row_latex(body))
        }
        MNode::Matrix(rows, _) => {
            let rows: Vec<String> = rows
                .iter()
                .map(|r| r.iter().map(row_latex).collect::<Vec<_>>().join(" & "))
                .collect();
            format!("\\begin{{matrix}} {} \\end{{matrix}}", rows.join(" \\\\ "))
        }
    }
}

/// A delimiter for `\left`/`\right` (Python's `ch in "()[]|/."` is a
/// substring test).
fn delim(ch: &str) -> String {
    if "()[]|/.".contains(ch) {
        return ch.to_string();
    }
    if ch == "{" || ch == "}" {
        return format!("\\{ch}");
    }
    format!("\\{}", reverse(symbols::SYMBOLS, ch).unwrap_or(ch))
}

/// Convert an `m:oMath` element to a LaTeX math string. Fails (with lxml's
/// `ValueError` text) when the math holds a comment or processing instruction.
pub fn omath_to_latex(element: &Element) -> Result<String, String> {
    let row = children_to_row(Some(element))?;
    Ok(pystr::strip(&row_latex(&row)).to_string())
}
