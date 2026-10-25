//! IR -> LaTeX writer, the round-trip back end (port of
//! `backend/latex_writer.py`).
//!
//! Walks the IR and emits compilable LaTeX. Each node's retained original
//! source is spliced in where present (figures, raw passthroughs) and the
//! rest is rebuilt structurally. This is a structural round trip, not a
//! byte-identical one.

use std::collections::BTreeMap;
use std::fmt;

use tex2word_ir::{pystr, *};

/// A `KeyError` the reference raises for a value outside its lookup tables
/// (a table alignment or TOC kind it doesn't know).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatexError(pub String);

impl fmt::Display for LatexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LatexError {}

type R<T> = Result<T, LatexError>;

fn key_error(key: &str) -> LatexError {
    LatexError(json::py_str_repr(key))
}

fn heading_cmd(level: i64, book: bool) -> &'static str {
    let cmd = if book {
        match level {
            1 => "chapter",
            2 => "section",
            3 => "subsection",
            4 => "subsubsection",
            5 => "paragraph",
            _ => "",
        }
    } else {
        match level {
            1 => "section",
            2 => "subsection",
            3 => "subsubsection",
            4 => "paragraph",
            _ => "",
        }
    };
    if cmd.is_empty() {
        "section"
    } else {
        cmd
    }
}

fn emph_cmd(kind: &str) -> &'static str {
    match kind {
        "bold" => "textbf",
        "italic" => "emph",
        "underline" => "underline",
        "typewriter" => "texttt",
        "smallcaps" => "textsc",
        "superscript" => "textsuperscript",
        "subscript" => "textsubscript",
        "strike" => "sout",
        "highlight" => "hl",
        _ => "emph",
    }
}

/// `w:sz` half-points -> the LaTeX size declaration.
fn hp_size_cmd(hp: i64) -> Option<&'static str> {
    Some(match hp {
        10 => "tiny",
        14 => "scriptsize",
        16 => "footnotesize",
        18 => "small",
        24 => "large",
        29 => "Large",
        34 => "LARGE",
        41 => "huge",
        50 => "Huge",
        _ => return None,
    })
}

fn cite_cmd(mode: &str) -> &'static str {
    match mode {
        "paren" => "citep",
        "text" => "citet",
        "foot" => "footcite",
        "author" => "citeauthor",
        "year" => "citeyear",
        "num" => "citenum",
        _ => "cite",
    }
}

/// BCP-47 -> babel option (common languages).
fn babel_name(lang: &str) -> Option<&'static str> {
    Some(match lang {
        "en-US" => "english",
        "en-GB" => "british",
        "de-DE" => "ngerman",
        "de-AT" => "naustrian",
        "fr-FR" => "french",
        "fr-CA" => "canadien",
        "es-ES" => "spanish",
        "it-IT" => "italian",
        "pt-PT" => "portuguese",
        "pt-BR" => "brazilian",
        "nl-NL" => "dutch",
        "ru-RU" => "russian",
        "pl-PL" => "polish",
        "sv-SE" => "swedish",
        "da-DK" => "danish",
        "nb-NO" => "norsk",
        "fi-FI" => "finnish",
        "cs-CZ" => "czech",
        "el-GR" => "greek",
        "tr-TR" => "turkish",
        "ja-JP" => "japanese",
        "zh-CN" => "chinese",
        _ => return None,
    })
}

fn align_letter(align: &str) -> R<&'static str> {
    match align {
        "left" => Ok("l"),
        "center" => Ok("c"),
        "right" => Ok("r"),
        other => Err(key_error(other)),
    }
}

/// `\cmidrule` lines for the bordered (`border_bottom`) cells of a row.
fn partial_rule_latex(row: &TableRow) -> String {
    let mut cols: Vec<i64> = Vec::new();
    let mut col = 1;
    for cell in &row.cells {
        if cell.border_bottom {
            cols.extend(col..col + cell.colspan);
        }
        col += cell.colspan;
    }
    if cols.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let (mut start, mut prev) = (cols[0], cols[0]);
    for c in cols[1..].iter().copied().chain([-1]) {
        if c != prev + 1 {
            out.push_str(&format!("\\cmidrule{{{start}-{prev}}}"));
            start = c;
        }
        prev = c;
    }
    out
}

/// Escape LaTeX specials in plain text.
pub fn latex_escape(text: &str) -> String {
    let text = text.replace('\\', "\\textbackslash{}");
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '&' | '%' | '$' | '#' | '_' | '{' | '}') {
            out.push('\\');
        }
        out.push(c);
    }
    out.replace('~', "\\textasciitilde{}")
        .replace('^', "\\textasciicircum{}")
}

fn truthy<T>(v: &Option<Vec<T>>) -> Option<&Vec<T>> {
    v.as_ref().filter(|x| !x.is_empty())
}

fn nonempty(v: &Option<String>) -> Option<&str> {
    v.as_deref().filter(|x| !x.is_empty())
}

fn theorem_env(kind: &str) -> String {
    if kind == "Proof" {
        "proof".into()
    } else {
        pystr::lower(kind)
    }
}

/// The IR -> LaTeX writer.
#[derive(Debug, Default)]
pub struct LatexWriter {
    book: bool,
    appendix_emitted: bool,
}

impl LatexWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Serialize a document.
    pub fn write(&mut self, doc: &Document) -> R<String> {
        self.book = doc.book;
        let body = self.blocks(&doc.blocks)?;
        let preamble = self.preamble(doc);
        let meta = self.frontmatter(&doc.meta)?;
        let parts = [
            preamble.as_str(),
            "\\begin{document}",
            &meta,
            &body,
            "\\end{document}",
            "",
        ];
        Ok(parts
            .iter()
            .filter(|p| !p.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n"))
    }

    // -- preamble ---------------------------------------------------------------

    fn preamble(&self, doc: &Document) -> String {
        let mut feat = Features::default();
        scan(&doc.blocks, &mut feat);
        if let Some(a) = truthy(&doc.meta.abstract_) {
            scan(a, &mut feat);
        }
        let cls = if doc.book { "report" } else { "article" };
        let mut lines = vec![
            format!("\\documentclass{{{cls}}}"),
            "\\usepackage[utf8]{inputenc}".to_string(),
        ];
        if let Some(b) = babel_name(doc.meta.language.as_deref().unwrap_or("")) {
            lines.push(format!("\\usepackage[{b}]{{babel}}"));
        }
        let m = &doc.meta;
        if nonempty(&m.cjk_main_font).is_some()
            || nonempty(&m.cjk_sans_font).is_some()
            || nonempty(&m.cjk_mono_font).is_some()
        {
            lines.push("\\usepackage{xeCJK}".into());
        } else if nonempty(&m.main_font).is_some() {
            lines.push("\\usepackage{fontspec}".into());
        }
        if let Some(f) = nonempty(&m.main_font) {
            lines.push(format!("\\setmainfont{{{f}}}"));
        }
        if let Some(f) = nonempty(&m.cjk_main_font) {
            lines.push(format!("\\setCJKmainfont{{{f}}}"));
        }
        if let Some(f) = nonempty(&m.cjk_sans_font) {
            lines.push(format!("\\setCJKsansfont{{{f}}}"));
        }
        if let Some(f) = nonempty(&m.cjk_mono_font) {
            lines.push(format!("\\setCJKmonofont{{{f}}}"));
        }
        let mut add = |cond: bool, pkgs: &[&str]| {
            if cond {
                lines.extend(pkgs.iter().map(|p| p.to_string()));
            }
        };
        add(
            feat.math,
            &["\\usepackage{amsmath}", "\\usepackage{amssymb}"],
        );
        add(feat.graphics, &["\\usepackage{graphicx}"]);
        add(feat.subfig, &["\\usepackage{subcaption}"]);
        add(feat.booktabs, &["\\usepackage{booktabs}"]);
        add(feat.multirow, &["\\usepackage{multirow}"]);
        add(
            feat.algorithm,
            &["\\usepackage{algorithm}", "\\usepackage{algpseudocode}"],
        );
        add(feat.links || feat.refs, &["\\usepackage{hyperref}"]);
        add(feat.cleveref, &["\\usepackage{cleveref}"]);
        add(feat.cites, &["\\usepackage{natbib}"]);
        add(feat.index, &["\\usepackage{makeidx}", "\\makeindex"]);
        for (env, display) in &feat.theorem_kinds {
            if env != "proof" {
                lines.push(format!("\\newtheorem{{{env}}}{{{display}}}"));
            }
        }
        lines.join("\n")
    }

    fn frontmatter(&mut self, meta: &DocumentMeta) -> R<String> {
        let mut out: Vec<String> = Vec::new();
        if let Some(title) = truthy(&meta.title) {
            let head = match nonempty(&meta.running_head) {
                Some(rh) => format!("[{}]", latex_escape(rh)),
                None => String::new(),
            };
            out.push(format!("\\title{head}{{{}}}", self.inlines(title)));
        } else if let Some(rh) = nonempty(&meta.running_head) {
            out.push(format!("\\markright{{{}}}", latex_escape(rh)));
        }
        for author in &meta.authors {
            out.push(format!("\\author{{{}}}", self.inlines(author)));
        }
        for affil in &meta.affiliations {
            out.push(format!("\\affiliation{{{}}}", self.inlines(affil)));
        }
        if let Some(date) = truthy(&meta.date) {
            out.push(format!("\\date{{{}}}", self.inlines(date)));
        }
        if truthy(&meta.title).is_some() {
            out.push("\\maketitle".into());
        }
        if let Some(a) = truthy(&meta.abstract_) {
            out.push("\\begin{abstract}".into());
            out.push(self.blocks(a)?);
            out.push("\\end{abstract}".into());
        }
        if let Some(k) = truthy(&meta.keywords) {
            out.push(format!("\\keywords{{{}}}", self.inlines(k)));
        }
        Ok(out.join("\n"))
    }

    // -- blocks -------------------------------------------------------------------

    fn blocks(&mut self, blocks: &[Block]) -> R<String> {
        let mut parts = Vec::with_capacity(blocks.len());
        for b in blocks {
            parts.push(self.block(b)?);
        }
        Ok(parts.join("\n\n"))
    }

    fn block(&mut self, block: &Block) -> R<String> {
        Ok(match block {
            Block::Heading(h) => {
                let cmd = if h.part {
                    "part"
                } else {
                    heading_cmd(h.level, self.book)
                };
                let star = if h.numbered { "" } else { "*" };
                let label = match nonempty(&h.label) {
                    Some(l) => format!("\\label{{{l}}}"),
                    None => String::new(),
                };
                let mut prefix = "";
                if h.appendix && !self.appendix_emitted {
                    prefix = "\\appendix\n";
                    self.appendix_emitted = true;
                }
                format!(
                    "{prefix}\\{cmd}{star}{{{}}}{label}",
                    self.inlines(&h.inlines)
                )
            }
            Block::Paragraph(p) => {
                let body = self.inlines(&p.inlines);
                let env = match p.align.as_deref().unwrap_or("") {
                    "center" => "center",
                    "left" => "flushleft",
                    "right" => "flushright",
                    _ => return Ok(body),
                };
                format!("\\begin{{{env}}}\n{body}\n\\end{{{env}}}")
            }
            Block::MathBlock(mb) => math_block(mb),
            Block::ItemList(l) => self.list(l)?,
            Block::Table(t) => self.table(t)?,
            Block::Figure(f) => {
                if f.source.is_empty() {
                    self.figure(f)?
                } else {
                    f.source.clone()
                }
            }
            Block::CodeBlock(c) => format!("\\begin{{verbatim}}\n{}\n\\end{{verbatim}}", c.text),
            Block::Quote(q) => {
                let head = match truthy(&q.title) {
                    Some(t) => format!("\\textbf{{{}}}\n\n", self.inlines(t)),
                    None => String::new(),
                };
                format!(
                    "\\begin{{quote}}\n{head}{}\n\\end{{quote}}",
                    self.blocks(&q.blocks)?
                )
            }
            Block::Theorem(t) => {
                let env = theorem_env(&t.kind);
                let title = match truthy(&t.title) {
                    Some(ti) => format!("[{}]", self.inlines(ti)),
                    None => String::new(),
                };
                let label = match nonempty(&t.label) {
                    Some(l) => format!("\\label{{{l}}}\n"),
                    None => String::new(),
                };
                format!(
                    "\\begin{{{env}}}{title}\n{label}{}\n\\end{{{env}}}",
                    self.blocks(&t.blocks)?
                )
            }
            Block::Algorithm(a) => self.algorithm(a),
            Block::Bibliography(b) => bibliography(b),
            Block::TableOfContents(t) => match t.kind.as_str() {
                "contents" => "\\tableofcontents".into(),
                "figures" => "\\listoffigures".into(),
                "tables" => "\\listoftables".into(),
                other => return Err(key_error(other)),
            },
            Block::Index(_) => "\\printindex".into(),
            Block::RawPassthrough(r) => r.latex.clone(),
        })
    }

    fn list(&mut self, block: &ItemList) -> R<String> {
        let env = if block.description {
            "description"
        } else if block.ordered {
            "enumerate"
        } else {
            "itemize"
        };
        let mut rows = Vec::new();
        for item in &block.items {
            let term = match truthy(&item.term) {
                Some(t) => format!("[{}]", self.inlines(t)),
                None => String::new(),
            };
            let row = format!("\\item{term} {}", self.blocks(&item.blocks)?);
            rows.push(pystr::rstrip(&row).to_string());
        }
        Ok(format!(
            "\\begin{{{env}}}\n{}\n\\end{{{env}}}",
            rows.join("\n")
        ))
    }

    fn table(&mut self, block: &Table) -> R<String> {
        let mut colspec = String::new();
        for (i, a) in block.colspec.iter().enumerate() {
            match block
                .colwidths
                .get(i)
                .copied()
                .flatten()
                .filter(|w| *w != 0.0)
            {
                Some(w) => colspec.push_str(&format!("p{{{:.1}pt}}", w * 72.27 / 914400.0)),
                None => colspec.push_str(align_letter(a)?),
            }
        }
        if colspec.is_empty() {
            let ncols = block
                .rows
                .iter()
                .map(|r| r.cells.iter().map(|c| c.colspan).sum::<i64>())
                .max()
                .unwrap_or(1);
            colspec = "l".repeat(ncols.max(0) as usize);
        }
        let mut lines = vec![format!("\\begin{{tabular}}{{{colspec}}}")];
        if block.booktabs {
            lines.push("\\toprule".into());
        }
        for row in &block.rows {
            let mut cells = Vec::new();
            for c in &row.cells {
                cells.push(self.cell(c)?);
            }
            lines.push(cells.join(" & ") + " \\\\");
            let rule = partial_rule_latex(row);
            if !rule.is_empty() {
                lines.push(rule);
            }
            if block.booktabs && row.is_header {
                lines.push("\\midrule".into());
            }
        }
        if block.booktabs {
            lines.push("\\bottomrule".into());
        }
        lines.push("\\end{tabular}".into());
        let table = lines.join("\n");
        if block.caption.is_none() && block.label.is_none() {
            return Ok(table);
        }
        let star = if block.caption_numbered { "" } else { "*" };
        let cap = format!(
            "\\caption{star}{{{}}}",
            self.inlines(block.caption.as_deref().unwrap_or(&[]))
        );
        let label = match nonempty(&block.label) {
            Some(l) => format!("\\label{{{l}}}"),
            None => String::new(),
        };
        Ok(format!(
            "\\begin{{table}}\n\\centering\n{table}\n{cap}{label}\n\\end{{table}}"
        ))
    }

    fn cell(&mut self, cell: &TableCell) -> R<String> {
        let mut content = self.blocks(&cell.blocks)?;
        if let Some(shade) = nonempty(&cell.shade) {
            content = format!("\\cellcolor[HTML]{{{shade}}}{content}");
        }
        if cell.colspan > 1 {
            let a = align_letter(&cell.align)?;
            content = format!("\\multicolumn{{{}}}{{{a}}}{{{content}}}", cell.colspan);
        }
        if cell.rowspan > 1 {
            content = format!("\\multirow{{{}}}{{*}}{{{content}}}", cell.rowspan);
        }
        Ok(content)
    }

    fn figure(&mut self, block: &Figure) -> R<String> {
        let mut lines = vec!["\\begin{figure}".to_string(), "\\centering".to_string()];
        if let Some(img) = &block.image {
            lines.push(format!("\\includegraphics{{{}}}", img.path));
        }
        for sub in &block.subfigures {
            lines.push("\\begin{subfigure}{0.45\\linewidth}".into());
            if let Some(img) = &sub.image {
                lines.push(format!("\\includegraphics{{{}}}", img.path));
            }
            if let Some(c) = truthy(&sub.caption) {
                lines.push(format!("\\caption{{{}}}", self.inlines(c)));
            }
            if let Some(l) = nonempty(&sub.label) {
                lines.push(format!("\\label{{{l}}}"));
            }
            lines.push("\\end{subfigure}".into());
        }
        if block.image.is_none() && !block.blocks.is_empty() {
            lines.push(
                self.blocks(&block.blocks)?
                    .trim_end_matches('\n')
                    .to_string(),
            );
        }
        if let Some(c) = truthy(&block.caption) {
            let star = if block.caption_numbered { "" } else { "*" };
            lines.push(format!("\\caption{star}{{{}}}", self.inlines(c)));
        }
        if let Some(l) = nonempty(&block.label) {
            lines.push(format!("\\label{{{l}}}"));
        }
        lines.push("\\end{figure}".into());
        Ok(lines.join("\n"))
    }

    fn algorithm(&self, block: &Algorithm) -> String {
        let mut lines = vec!["\\begin{algorithm}".to_string()];
        if let Some(c) = truthy(&block.caption) {
            lines.push(format!("\\caption{{{}}}", self.inlines(c)));
        }
        if let Some(l) = nonempty(&block.label) {
            lines.push(format!("\\label{{{l}}}"));
        }
        lines.push("\\begin{algorithmic}[1]".into());
        for aline in &block.lines {
            let indent = "  ".repeat(aline.indent.max(0) as usize);
            lines.push(format!("{indent}\\State {}", self.inlines(&aline.inlines)));
        }
        lines.push("\\end{algorithmic}".into());
        lines.push("\\end{algorithm}".into());
        lines.join("\n")
    }

    // -- inline -------------------------------------------------------------------

    fn inlines(&self, inlines: &[Inline]) -> String {
        inlines.iter().map(|n| self.inline(n)).collect()
    }

    fn inline(&self, node: &Inline) -> String {
        match node {
            Inline::Text(t) => latex_escape(&t.value),
            Inline::Emphasis(e) => {
                format!("\\{}{{{}}}", emph_cmd(&e.kind), self.inlines(&e.inlines))
            }
            Inline::Math(m) => format!("${}$", m.latex),
            Inline::Ref(r) => ref_latex(r),
            Inline::Cite(c) => cite_latex(c),
            Inline::Link(l) => {
                let text = self.inlines(&l.inlines);
                match nonempty(&l.anchor) {
                    Some(a) => format!("\\hyperref[{a}]{{{text}}}"),
                    None => format!("\\href{{{}}}{{{text}}}", l.url),
                }
            }
            Inline::LineBreak(_) => "\\\\".into(),
            Inline::Footnote(f) => format!("\\footnote{{{}}}", self.inlines(&f.inlines)),
            Inline::Endnote(e) => format!("\\endnote{{{}}}", self.inlines(&e.inlines)),
            Inline::IndexEntry(i) => format!("\\index{{{}}}", i.term),
            Inline::Colored(c) => {
                let inner = self.inlines(&c.inlines);
                if let Some(bg) = &c.bg {
                    format!("\\colorbox[HTML]{{{bg}}}{{{inner}}}")
                } else if let Some(fg) = &c.fg {
                    format!("\\textcolor[HTML]{{{fg}}}{{{inner}}}")
                } else {
                    inner
                }
            }
            Inline::FontSize(f) => {
                let inner = self.inlines(&f.inlines);
                match hp_size_cmd(f.half_points) {
                    Some(cmd) => format!("{{\\{cmd} {inner}}}"),
                    None => inner,
                }
            }
            Inline::Image(i) => format!("\\includegraphics{{{}}}", i.path),
            Inline::RawInline(r) => r.latex.clone(),
            Inline::Comment(c) => {
                let note = pystr::split_ws(&c.text).join(" ");
                let who = if c.author.is_empty() {
                    String::new()
                } else {
                    format!("[{}] ", c.author)
                };
                format!("\n% comment: {who}{note}\n")
            }
        }
    }
}

fn math_block(block: &MathBlock) -> String {
    let mut latex = pystr::strip(&block.latex).to_string();
    if let Some(l) = nonempty(&block.label) {
        latex = format!("{latex}\n\\label{{{l}}}");
    }
    let env = &block.env;
    if env == "displaymath" || env == "math" {
        return format!("\\[\n{latex}\n\\]");
    }
    let name = if block.numbered {
        env.clone()
    } else {
        format!("{env}*")
    };
    format!("\\begin{{{name}}}\n{latex}\n\\end{{{name}}}")
}

fn bibliography(block: &Bibliography) -> String {
    let mut lines = vec![format!(
        "\\begin{{thebibliography}}{{{}}}",
        block.entries.len()
    )];
    for item in &block.entries {
        let note = item
            .csl_fields
            .get("note")
            .map(Value::py_str)
            .unwrap_or_default();
        lines.push(format!("\\bibitem{{{}}} {note}", item.id));
    }
    lines.push("\\end{thebibliography}".into());
    lines.join("\n")
}

fn ref_latex(node: &Ref) -> String {
    match (node.style.as_str(), node.ref_kind.as_str()) {
        ("abbrev", _) => format!("\\cref{{{}}}", node.key),
        ("full", _) => format!("\\Cref{{{}}}", node.key),
        (_, "equation") => format!("\\eqref{{{}}}", node.key),
        (_, "page") => format!("\\pageref{{{}}}", node.key),
        _ => format!("\\ref{{{}}}", node.key),
    }
}

fn cite_latex(node: &Cite) -> String {
    let opts = match (nonempty(&node.prefix), nonempty(&node.suffix)) {
        (Some(p), s) => format!("[{p}][{}]", s.unwrap_or("")),
        (None, Some(s)) => format!("[{s}]"),
        (None, None) => String::new(),
    };
    format!(
        "\\{}{opts}{{{}}}",
        cite_cmd(&node.mode),
        node.keys.join(",")
    )
}

// --------------------------------------------------------------------------- //
// Feature detection for the preamble
// --------------------------------------------------------------------------- //

#[derive(Default)]
struct Features {
    math: bool,
    graphics: bool,
    subfig: bool,
    booktabs: bool,
    multirow: bool,
    algorithm: bool,
    links: bool,
    refs: bool,
    cleveref: bool,
    cites: bool,
    index: bool,
    /// env name -> display name, sorted like Python's `sorted(items())`
    theorem_kinds: BTreeMap<String, String>,
}

fn scan(blocks: &[Block], feat: &mut Features) {
    for block in blocks {
        match block {
            Block::MathBlock(_) => feat.math = true,
            Block::Figure(f) => {
                feat.graphics = true;
                if !f.subfigures.is_empty() {
                    feat.subfig = true;
                }
            }
            Block::Table(t) => {
                feat.booktabs = feat.booktabs || t.booktabs;
                if t.rows.iter().any(|r| r.cells.iter().any(|c| c.rowspan > 1)) {
                    feat.multirow = true;
                }
                // the reference scans the cells' *blocks* as inlines, which
                // matches nothing
            }
            Block::Algorithm(_) => feat.algorithm = true,
            Block::Theorem(t) => {
                feat.theorem_kinds
                    .insert(theorem_env(&t.kind), t.kind.clone());
                scan(&t.blocks, feat);
            }
            Block::Quote(q) => scan(&q.blocks, feat),
            Block::ItemList(l) => {
                for item in &l.items {
                    scan(&item.blocks, feat);
                }
            }
            Block::Index(_) => feat.index = true,
            Block::Paragraph(p) => scan_inlines(&p.inlines, feat),
            Block::Heading(h) => scan_inlines(&h.inlines, feat),
            _ => {}
        }
    }
}

fn scan_inlines(inlines: &[Inline], feat: &mut Features) {
    for node in inlines {
        match node {
            Inline::Math(_) => feat.math = true,
            Inline::Link(_) => feat.links = true,
            Inline::Ref(r) => {
                feat.refs = true;
                if r.style == "abbrev" || r.style == "full" {
                    feat.cleveref = true;
                }
            }
            Inline::Cite(_) => feat.cites = true,
            Inline::Image(_) => feat.graphics = true,
            Inline::IndexEntry(_) => feat.index = true,
            Inline::Emphasis(e) => scan_inlines(&e.inlines, feat),
            Inline::Footnote(f) => scan_inlines(&f.inlines, feat),
            Inline::Endnote(e) => scan_inlines(&e.inlines, feat),
            Inline::Colored(c) => scan_inlines(&c.inlines, feat),
            Inline::FontSize(f) => scan_inlines(&f.inlines, feat),
            _ => {}
        }
    }
}

/// Serialize an IR document to LaTeX.
pub fn write_latex(doc: &Document) -> R<String> {
    LatexWriter::new().write(doc)
}
