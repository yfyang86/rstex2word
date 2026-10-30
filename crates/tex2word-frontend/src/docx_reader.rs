//! OOXML `document.xml` -> IR, the round-trip reader (port of
//! `frontend/docx_reader.py`).
//!
//! A best-effort reader for a foreign `.docx` (or one whose tex2word manifest
//! was stripped): Word styles map back to sectioning and blocks, runs to text
//! and emphasis, `m:oMath` to math, complex fields to `\ref`/`\cite`/links,
//! and tables/lists to their IR nodes. This is structural recovery, not a
//! byte-identical inverse.

use std::fmt;
use std::sync::LazyLock;

use tex2word_ir::json::{self, Value};
use tex2word_ir::re::Regex;
use tex2word_ir::{pystr, *};
use tex2word_math::omml_reader::{localname, omath_to_latex};
use tex2word_ooxml::parse::fromstring;
use tex2word_ooxml::xml::Element;
use tex2word_ooxml::zip::Archive;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const M: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

/// Must match the back-end's bibliography / figure SDT tags.
const BIB_SDT_TAG: &str = "tex2word:bibliography";
const FIG_SDT_TAG: &str = "tex2word:figure";

const TOC_TITLES: [&str; 3] = ["Contents", "List of Figures", "List of Tables"];

const THEOREM_KINDS: [&str; 18] = [
    "Theorem",
    "Lemma",
    "Corollary",
    "Proposition",
    "Definition",
    "Remark",
    "Example",
    "Conjecture",
    "Claim",
    "Fact",
    "Observation",
    "Notation",
    "Assumption",
    "Axiom",
    "Exercise",
    "Problem",
    "Question",
    "Proof",
];

/// (rPr child local name, emphasis kind)
const EMPH_FROM_RPR: [(&str, &str); 6] = [
    ("b", "bold"),
    ("i", "italic"),
    ("u", "underline"),
    ("smallCaps", "smallcaps"),
    ("strike", "strike"),
    ("highlight", "highlight"),
];

/// Common LaTeX cross-reference label prefixes (the no-manifest heuristic).
const REF_PREFIXES: [&str; 31] = [
    "eq", "fig", "tab", "tbl", "sec", "subsec", "thm", "lem", "cor", "def", "defn", "prop", "rem",
    "ex", "exa", "alg", "app", "ch", "chap", "chapter", "part", "line", "item", "lst", "list",
    "claim", "assum", "fact", "obs", "note", "step",
];

const QED_CHARS: &str = "\u{2003} □";

static RE_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"REF\s+(\S+)"));
static RE_PAGEREF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"PAGEREF\s+(\S+)"));
static RE_HYPERLINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"HYPERLINK\s+"([^"]*)""#));
static RE_REC_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<rec-number>([^<]+)</rec-number>"));
static RE_EN_KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<key[^>]*>([^<]+)</key>"));
static RE_ALG_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\d+\s+"));
static RE_BIB_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[\d+\]\s*"));

/// An exception the reference lets escape from `read_docx`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError {
    /// the Python exception type (`BadZipFile`, `KeyError`, `XMLSyntaxError`,
    /// `ValueError`)
    pub type_name: String,
    /// `str(exc)`
    pub message: String,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ReadError {}

fn value_error(message: impl Into<String>) -> ReadError {
    ReadError {
        type_name: "ValueError".into(),
        message: message.into(),
    }
}

type R<T> = Result<T, ReadError>;

fn local(e: &Element) -> R<&str> {
    localname(e).map_err(value_error)
}

fn omath(e: &Element) -> R<String> {
    omath_to_latex(e).map_err(value_error)
}

/// Python `int(s)`.
fn py_int(s: &str) -> R<i64> {
    pystr::parse_int(s).ok_or_else(|| {
        value_error(format!(
            "invalid literal for int() with base 10: {}",
            json::py_str_repr(s)
        ))
    })
}

fn w(name: &str) -> String {
    format!("{{{W}}}{name}")
}

fn m(name: &str) -> String {
    format!("{{{M}}}{name}")
}

fn wval(e: &Element) -> Option<&str> {
    e.get(&w("val"))
}

fn text_inline(s: impl Into<String>) -> Inline {
    Inline::Text(Text { value: s.into() })
}

fn paragraph(inlines: Vec<Inline>) -> Block {
    Block::Paragraph(Paragraph {
        inlines,
        align: None,
    })
}

/// Top-level `Text` only, stripped.
fn plain_text(inlines: &[Inline]) -> String {
    let s: String = inlines
        .iter()
        .filter_map(|i| match i {
            Inline::Text(t) => Some(t.value.as_str()),
            _ => None,
        })
        .collect();
    pystr::strip(&s).to_string()
}

/// All text, recursing into Emphasis/Colored/FontSize wrappers.
fn all_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for i in inlines {
        match i {
            Inline::Text(t) => out.push_str(&t.value),
            Inline::Emphasis(e) => out.push_str(&all_text(&e.inlines)),
            Inline::Colored(c) => out.push_str(&all_text(&c.inlines)),
            Inline::FontSize(f) => out.push_str(&all_text(&f.inlines)),
            _ => {}
        }
    }
    out
}

/// Recover a theorem/proof from its bold (italic for proof) lead paragraph.
fn theorem_block(inlines: &[Inline]) -> Option<Block> {
    let Some(Inline::Emphasis(first)) = inlines.first() else {
        return None;
    };
    if first.kind != "bold" && first.kind != "italic" {
        return None;
    }
    let kind = pystr::strip(&all_text(&inlines[..1])).to_string();
    if !THEOREM_KINDS.contains(&kind.as_str()) {
        return None;
    }
    // the lead runs end at the bold/italic ". " separator
    let sep = (1..inlines.len()).find(|&i| {
        matches!(&inlines[i], Inline::Emphasis(_))
            && pystr::strip(&all_text(&inlines[i..i + 1])) == "."
    })?;
    let lead = &inlines[1..sep];
    let mut content = inlines[sep + 1..].to_vec();
    let title = paren_title(lead);
    if kind == "Proof" {
        content = strip_qed(content);
    }
    Some(Block::Theorem(Theorem {
        kind,
        blocks: vec![paragraph(content)],
        title,
        ..Theorem::default()
    }))
}

/// The `(title)` text from a theorem lead, if any.
fn paren_title(lead: &[Inline]) -> Option<Vec<Inline>> {
    let text = all_text(lead);
    let (start, end) = (text.find('('), text.rfind(')'));
    if let (Some(s), Some(e)) = (start, end) {
        if s < e {
            let inner = pystr::strip(&text[s + 1..e]);
            return if inner.is_empty() {
                None
            } else {
                Some(vec![text_inline(inner)])
            };
        }
    }
    None
}

/// Drop a trailing QED mark from a proof body.
fn strip_qed(mut out: Vec<Inline>) -> Vec<Inline> {
    while matches!(out.last(), Some(Inline::Text(t)) if pystr::strip_chars(&t.value, QED_CHARS).is_empty())
    {
        out.pop();
    }
    if let Some(Inline::Text(t)) = out.last() {
        let stripped = pystr::rstrip_chars(&t.value, QED_CHARS).to_string();
        if stripped != t.value {
            out.pop();
            if !stripped.is_empty() {
                out.push(text_inline(stripped));
            }
        }
    }
    out
}

/// The alt text (image source path) from a drawing's docPr/cNvPr `@descr`.
fn drawing_descr(el: &Element) -> R<String> {
    for e in el.iter_nodes() {
        let name = local(e)?;
        if name == "docPr" || name == "cNvPr" {
            if let Some(d) = e.get("descr").filter(|d| !d.is_empty()) {
                return Ok(d.to_string());
            }
        }
    }
    Ok(String::new())
}

/// A toggle/property element is on unless its `w:val` says otherwise.
fn toggle_on(e: Option<&Element>) -> bool {
    match e {
        None => false,
        Some(e) => {
            let v = pystr::lower(wval(e).filter(|v| !v.is_empty()).unwrap_or("true"));
            !matches!(v.as_str(), "false" | "0" | "none" | "off")
        }
    }
}

/// Best-effort reverse of bookmark sanitization without a manifest map.
fn unsanitize(bookmark: &str) -> String {
    let b = bookmark.strip_prefix("ref_").unwrap_or(bookmark);
    if let Some((head, rest)) = b.split_once('_') {
        if !rest.is_empty() && REF_PREFIXES.contains(&pystr::lower(head).as_str()) {
            return format!("{head}:{rest}");
        }
    }
    b.to_string()
}

/// `{comment id -> (author, plain text)}` from `word/comments.xml`.
fn read_comments(comments_xml: &[u8]) -> Vec<(String, (String, String))> {
    let mut out: Vec<(String, (String, String))> = Vec::new();
    let Ok(root) = fromstring(comments_xml) else {
        return out;
    };
    for c in root.findall(&w("comment")) {
        let Some(cid) = c.get(&w("id")) else {
            continue;
        };
        let text: String = c
            .iter(Some(&w("t")))
            .iter()
            .map(|t| t.text.as_deref().unwrap_or(""))
            .collect();
        let entry = (
            c.get(&w("author")).unwrap_or("").to_string(),
            pystr::strip(&text).to_string(),
        );
        match out.iter_mut().find(|(k, _)| k == cid) {
            Some(slot) => slot.1 = entry,
            None => out.push((cid.to_string(), entry)),
        }
    }
    out
}

/// Read a foreign `.docx` to the IR.
///
/// `label_map` maps sanitized Word bookmark names back to the original label
/// keys (built from a manifest, when one is present).
pub fn read_docx(docx_bytes: &[u8], label_map: Option<&[(String, String)]>) -> R<Document> {
    let zf = Archive::new(docx_bytes).map_err(|message| ReadError {
        type_name: "BadZipFile".into(),
        message,
    })?;
    let read = |name: &str| {
        if !zf.names().contains(&name) {
            return Err(ReadError {
                type_name: "KeyError".into(),
                message: format!("\"There is no item named '{name}' in the archive\""),
            });
        }
        zf.read(name).map_err(|message| ReadError {
            type_name: "BadZipFile".into(),
            message,
        })
    };
    let root = fromstring(&read("word/document.xml")?).map_err(|e| ReadError {
        type_name: "XMLSyntaxError".into(),
        message: e.0,
    })?;
    let comments = if zf.names().contains(&"word/comments.xml") {
        read_comments(&read("word/comments.xml")?)
    } else {
        Vec::new()
    };
    let mut reader = Reader {
        meta: DocumentMeta::default(),
        comments,
        label_map: label_map.map(<[_]>::to_vec).unwrap_or_default(),
    };
    let body = root.find(&w("body")).unwrap_or(&root);
    let blocks = reader.read_body(body)?;
    Ok(Document {
        blocks,
        meta: reader.meta,
        ..Document::default()
    })
}

/// A complex field being read (`begin` ... `separate` ... `end`).
#[derive(Debug, Clone)]
struct Field {
    instr: String,
    /// `instr` or `result`
    phase: &'static str,
    result: Vec<String>,
}

struct Reader {
    meta: DocumentMeta,
    comments: Vec<(String, (String, String))>,
    label_map: Vec<(String, String)>,
}

impl Reader {
    /// A sanitized bookmark -> the original label key.
    fn resolve_label(&self, bookmark: &str) -> String {
        match self.label_map.iter().find(|(k, _)| k == bookmark) {
            Some((_, v)) if !v.is_empty() => v.clone(),
            _ => unsanitize(bookmark),
        }
    }

    // -- body -----------------------------------------------------------------

    fn read_body(&mut self, body: &Element) -> R<Vec<Block>> {
        let mut blocks: Vec<Block> = Vec::new();
        let mut list_run: Vec<&Element> = Vec::new();
        let mut quote_run: Vec<&Element> = Vec::new();
        let mut code_run: Vec<&Element> = Vec::new();
        let mut desc_run: Vec<(Vec<Inline>, Vec<Inline>)> = Vec::new();

        macro_rules! flush_list {
            () => {
                if !list_run.is_empty() {
                    let lists = self.build_lists(&list_run)?;
                    blocks.extend(lists);
                    list_run.clear();
                }
            };
        }
        macro_rules! flush_quote {
            () => {
                if !quote_run.is_empty() {
                    let mut inner = Vec::new();
                    for p in &quote_run {
                        if let Some(b) = self.paragraph(p)? {
                            inner.push(b);
                        }
                    }
                    blocks.push(Block::Quote(Quote {
                        blocks: inner,
                        ..Quote::default()
                    }));
                    quote_run.clear();
                }
            };
        }
        macro_rules! flush_code {
            () => {
                if !code_run.is_empty() {
                    let mut lines = Vec::new();
                    for p in &code_run {
                        lines.push(all_text(&self.inlines(p)?));
                    }
                    blocks.push(Block::CodeBlock(CodeBlock {
                        text: lines.join("\n"),
                        lang: None,
                        ..Default::default()
                    }));
                    code_run.clear();
                }
            };
        }
        macro_rules! flush_desc {
            () => {
                if !desc_run.is_empty() {
                    let items = desc_run
                        .drain(..)
                        .map(|(t, d)| ListItem {
                            blocks: vec![paragraph(d)],
                            term: Some(t),
                            label: None,
                        })
                        .collect();
                    blocks.push(Block::ItemList(ItemList {
                        ordered: false,
                        items,
                        description: true,
                        ..Default::default()
                    }));
                }
            };
        }
        macro_rules! flush_all {
            () => {
                flush_list!();
                flush_quote!();
                flush_code!();
                flush_desc!();
            };
        }

        for child in &body.children {
            let tag = local(child)?;
            if tag == "tbl" {
                flush_all!();
                let b = match self.algorithm(child)? {
                    Some(a) => a,
                    None => self.table(child)?,
                };
                blocks.push(b);
                continue;
            }
            if tag == "sdt" {
                flush_all!();
                let bs = self.sdt(child)?;
                blocks.extend(bs);
                continue;
            }
            if tag != "p" {
                continue; // sectPr and others
            }
            let style = style_or_normal(child);
            if numpr(child).is_some() && !is_heading(child) {
                flush_quote!();
                flush_code!();
                list_run.push(child);
                continue;
            }
            if style == "Quote" {
                flush_list!();
                flush_code!();
                quote_run.push(child);
                continue;
            }
            if style == "SourceCode" {
                flush_list!();
                flush_quote!();
                flush_desc!();
                code_run.push(child);
                continue;
            }
            if style == "Normal" {
                if let Some(td) = self.description_item(child)? {
                    flush_list!();
                    flush_quote!();
                    flush_code!();
                    desc_run.push(td);
                    continue;
                }
            }
            flush_all!();
            // a "Figure N: ..." / "Table N: ..." Caption right after the float
            if style == "Caption" {
                match blocks.last() {
                    Some(Block::Figure(_)) => {
                        if let Some(cap) = self.caption_after(child, "Figure")? {
                            if let Some(Block::Figure(f)) = blocks.last_mut() {
                                f.caption = Some(cap);
                            }
                            continue;
                        }
                    }
                    Some(Block::Table(_)) => {
                        if let Some(cap) = self.caption_after(child, "Table")? {
                            if let Some(Block::Table(t)) = blocks.last_mut() {
                                t.caption = Some(cap);
                            }
                            continue;
                        }
                    }
                    _ => {}
                }
            }
            // a drawing-only paragraph is a Figure
            if child
                .iter(Some(&w("drawing")))
                .iter()
                .any(|d| !std::ptr::eq(*d, child))
                && plain_text(&self.inlines(child)?).is_empty()
            {
                let fig = self.figure(child)?;
                blocks.push(fig);
                continue;
            }
            if let Some(block) = self.paragraph(child)? {
                // drop a bold "Contents"/"List of ..." title before a TOC field
                if matches!(block, Block::TableOfContents(_)) {
                    if let Some(Block::Paragraph(prev)) = blocks.last() {
                        if TOC_TITLES.contains(&plain_text(&prev.inlines).as_str()) {
                            blocks.pop();
                        }
                    }
                }
                blocks.push(block);
            }
        }
        flush_all!();
        Ok(blocks)
    }

    /// A paragraph carrying a `w:drawing` -> a Figure (path from alt text).
    fn figure(&self, p: &Element) -> R<Block> {
        Ok(Block::Figure(Figure {
            image: Some(Image {
                path: drawing_descr(p)?,
                ..Image::default()
            }),
            ..Figure::default()
        }))
    }

    /// The caption text of a `'<prefix> N: text'` Caption paragraph.
    fn caption_after(&self, p: &Element, prefix: &str) -> R<Option<Vec<Inline>>> {
        let text = plain_text(&self.inlines(p)?);
        if !text.starts_with(prefix) {
            return Ok(None);
        }
        let rest = text.split_once(':').map(|(_, r)| r).unwrap_or("");
        let rest = pystr::strip(rest);
        Ok(if rest.is_empty() {
            None
        } else {
            Some(vec![text_inline(rest)])
        })
    }

    fn alignment(p: &Element) -> Option<String> {
        let jc = p.find(&w("pPr"))?.find(&w("jc"))?;
        let v = match wval(jc).unwrap_or("") {
            "center" => "center",
            "right" | "end" => "right",
            "left" | "start" => "left",
            _ => return None,
        };
        Some(v.to_string())
    }

    fn paragraph(&mut self, p: &Element) -> R<Option<Block>> {
        let style = style_or_normal(p);
        // display math paragraph (unnumbered: m:oMathPara)
        if p.find(&m("oMathPara")).is_some() {
            return Ok(Some(self.math_block(p)?));
        }
        if let Some(eq) = self.numbered_equation(p)? {
            return Ok(Some(eq));
        }
        if let Some(toc) = toc_block(p) {
            return Ok(Some(toc));
        }
        let inlines = self.inlines(p)?;
        let label = self.bookmark(p);
        match style.as_str() {
            "Title" => {
                self.meta.title = Some(inlines);
                return Ok(None);
            }
            "Subtitle" | "Author" => {
                self.meta.authors.push(inlines);
                return Ok(None);
            }
            "Abstract" => {
                self.meta
                    .abstract_
                    .get_or_insert_with(Vec::new)
                    .push(paragraph(inlines));
                return Ok(None);
            }
            _ => {}
        }
        if let Some(level) = heading_level(&style) {
            return Ok(Some(Block::Heading(Heading {
                level,
                inlines,
                label,
                numbered: numpr(p).is_some(),
                ..Heading::default()
            })));
        }
        if style == "Normal" {
            if let Some(thm) = theorem_block(&inlines) {
                return Ok(Some(thm));
            }
        }
        if inlines.is_empty() {
            return Ok(None);
        }
        Ok(Some(Block::Paragraph(Paragraph {
            inlines,
            align: Self::alignment(p),
        })))
    }

    fn math_block(&self, p: &Element) -> R<Block> {
        let latex = match p
            .find(&m("oMathPara"))
            .and_then(|para| para.find(&m("oMath")))
        {
            Some(o) => omath(o)?,
            None => String::new(),
        };
        Ok(Block::MathBlock(MathBlock {
            latex,
            numbered: false,
            env: "displaymath".into(),
            label: self.bookmark(p),
            ..MathBlock::default()
        }))
    }

    fn numbered_equation(&self, p: &Element) -> R<Option<Block>> {
        let Some(o) = p.iter(Some(&m("oMath"))).into_iter().next() else {
            return Ok(None);
        };
        if !instr_text(p).contains("SEQ Equation") {
            return Ok(None);
        }
        Ok(Some(Block::MathBlock(MathBlock {
            latex: omath(o)?,
            numbered: true,
            env: "equation".into(),
            label: self.bookmark(p),
            ..MathBlock::default()
        })))
    }

    fn bookmark(&self, p: &Element) -> Option<String> {
        let name = p.find(&w("bookmarkStart"))?.get(&w("name"))?;
        if name.is_empty() {
            None
        } else {
            Some(self.resolve_label(name))
        }
    }

    // -- inline ---------------------------------------------------------------

    fn inlines(&self, parent: &Element) -> R<Vec<Inline>> {
        let mut out = Vec::new();
        self.runs(parent, &mut out, None)?;
        Ok(out)
    }

    /// Walk run-level children, accepting tracked changes (`w:ins`/`w:moveTo`
    /// kept, `w:del`/`w:moveFrom` dropped).
    fn runs(
        &self,
        parent: &Element,
        out: &mut Vec<Inline>,
        mut field: Option<Field>,
    ) -> R<Option<Field>> {
        for el in &parent.children {
            match local(el)? {
                "bookmarkStart" | "bookmarkEnd" | "proofErr" | "del" | "moveFrom" => {}
                "ins" | "moveTo" => field = self.runs(el, out, field)?,
                "oMath" => out.push(Inline::Math(Math { latex: omath(el)? })),
                "hyperlink" => {
                    self.runs(el, out, None)?;
                }
                "r" => field = self.run(el, out, field)?,
                _ => {}
            }
        }
        Ok(field)
    }

    fn run(&self, r: &Element, out: &mut Vec<Inline>, field: Option<Field>) -> R<Option<Field>> {
        // a comment anchor -> the reviewer's note
        if let Some(cref) = r.find(&w("commentReference")) {
            let id = cref.get(&w("id")).unwrap_or("");
            if let Some((_, (author, text))) = self.comments.iter().find(|(k, _)| k == id) {
                out.push(Inline::Comment(Comment {
                    text: text.clone(),
                    author: author.clone(),
                }));
            }
            return Ok(field);
        }
        // complex-field state machine
        if let Some(fld) = r.find(&w("fldChar")) {
            return Ok(match (fld.get(&w("fldCharType")), field) {
                (Some("begin"), _) => Some(Field {
                    instr: String::new(),
                    phase: "instr",
                    result: Vec::new(),
                }),
                (Some("separate"), Some(mut f)) => {
                    f.phase = "result";
                    Some(f)
                }
                (Some("end"), Some(f)) => {
                    self.emit_field(&f, out);
                    None
                }
                (_, f) => f,
            });
        }
        let mut field = field;
        if let Some(instr) = r.find(&w("instrText")) {
            if let Some(f) = field.as_mut() {
                f.instr.push_str(instr.text.as_deref().unwrap_or(""));
                return Ok(field);
            }
        }
        if r.find(&w("drawing")).is_some() && field.is_none() {
            out.push(Inline::Image(Image {
                path: drawing_descr(r)?,
                ..Image::default()
            }));
            return Ok(field);
        }
        let text: String = r
            .findall(&w("t"))
            .iter()
            .map(|t| t.text.as_deref().unwrap_or(""))
            .collect();
        if let Some(f) = field.as_mut() {
            if f.phase == "result" {
                f.result.push(text);
                return Ok(field);
            }
        }
        if !text.is_empty() {
            out.push(styled_text(r, text)?);
        }
        Ok(field)
    }

    fn emit_field(&self, field: &Field, out: &mut Vec<Inline>) {
        let instr = pystr::strip(&field.instr);
        let result: String = field.result.concat();
        if let Some(mt) = RE_REF.match_start(instr) {
            out.push(Inline::Ref(Ref {
                key: self.resolve_label(mt.group_str(1)),
                ref_kind: "generic".into(),
                ..Ref::default()
            }));
            return;
        }
        if let Some(mt) = RE_PAGEREF.match_start(instr) {
            out.push(Inline::Ref(Ref {
                key: self.resolve_label(mt.group_str(1)),
                ref_kind: "page".into(),
                ..Ref::default()
            }));
            return;
        }
        if let Some(mt) = RE_HYPERLINK.match_start(instr) {
            let url = mt.group_str(1).to_string();
            let shown = if result.is_empty() {
                url.clone()
            } else {
                result
            };
            out.push(Inline::Link(Link {
                inlines: vec![text_inline(shown)],
                url,
                anchor: None,
            }));
            return;
        }
        if instr.contains("CSL_CITATION") {
            out.push(csl_cite(instr, &result));
            return;
        }
        if instr.contains("EN.CITE") || instr.contains("EN.REF") {
            out.push(endnote_cite(instr, &result));
            return;
        }
        if instr.starts_with("SEQ") {
            return; // a regenerated number
        }
        if !result.is_empty() {
            out.push(text_inline(result));
        }
    }

    // -- lists ----------------------------------------------------------------

    fn build_lists(&self, paras: &[&Element]) -> R<Vec<Block>> {
        // group by numId; nest by ilvl with a simple stack of paths into roots
        let mut roots: Vec<ItemList> = Vec::new();
        // (ilvl, path) where path indexes roots then (item, block) pairs
        let mut stack: Vec<(i64, Vec<usize>)> = Vec::new();
        for p in paras {
            let np = numpr(p).expect("selected for having numPr");
            let ilvl = py_int(
                np.find(&w("ilvl"))
                    .and_then(wval)
                    .filter(|v| !v.is_empty())
                    .unwrap_or("0"),
            )?;
            let numid = np
                .find(&w("numId"))
                .and_then(wval)
                .filter(|v| !v.is_empty())
                .unwrap_or("1")
                .to_string();
            let item = ListItem {
                blocks: vec![paragraph(self.inlines(p)?)],
                ..ListItem::default()
            };
            while stack.last().is_some_and(|(l, _)| *l > ilvl) {
                stack.pop();
            }
            if let Some((l, path)) = stack.last() {
                if *l == ilvl {
                    list_at(&mut roots, path).items.push(item);
                    continue;
                }
            }
            let lst = ItemList {
                ordered: numid == "2",
                items: vec![item],
                description: false,
                ..Default::default()
            };
            let path = match stack.last() {
                Some((_, parent)) => {
                    let parent_list = list_at(&mut roots, parent);
                    let ii = parent_list.items.len() - 1;
                    let last_item = &mut parent_list.items[ii];
                    last_item.blocks.push(Block::ItemList(lst));
                    let mut path = parent.clone();
                    path.push(ii);
                    path.push(last_item.blocks.len() - 1);
                    path
                }
                None => {
                    roots.push(lst);
                    vec![roots.len() - 1]
                }
            };
            stack.push((ilvl, path));
        }
        Ok(roots.into_iter().map(Block::ItemList).collect())
    }

    // -- algorithm boxes ------------------------------------------------------

    /// A single-cell ruled box of `SourceCode` lines -> an Algorithm.
    fn algorithm(&self, tbl: &Element) -> R<Option<Block>> {
        let trs = tbl.findall(&w("tr"));
        if trs.len() != 1 {
            return Ok(None);
        }
        let tcs = trs[0].findall(&w("tc"));
        if tcs.len() != 1 {
            return Ok(None);
        }
        let paras = tcs[0].findall(&w("p"));
        if !paras
            .iter()
            .any(|p| style(p).as_deref() == Some("SourceCode"))
        {
            return Ok(None);
        }
        let mut caption = None;
        let mut label = None;
        let mut lines = Vec::new();
        for p in paras {
            let st = style_or_normal(p);
            if st == "Caption" {
                let text = pystr::strip(&all_text(&self.inlines(p)?)).to_string();
                if text.starts_with("Algorithm") {
                    let rest = pystr::strip(text.split_once(':').map(|(_, r)| r).unwrap_or(""));
                    caption = if rest.is_empty() {
                        None
                    } else {
                        Some(vec![text_inline(rest)])
                    };
                    label = self.bookmark(p);
                }
                continue;
            }
            if st == "SourceCode" {
                lines.push(self.alg_line(p)?);
            }
        }
        let numbered = caption.is_some() || label.is_some();
        Ok(Some(Block::Algorithm(Algorithm {
            lines,
            caption,
            label,
            numbered,
        })))
    }

    /// One pseudocode paragraph -> an AlgLine (indent and number peeled).
    fn alg_line(&self, p: &Element) -> R<AlgLine> {
        let left_attr = p
            .find(&w("pPr"))
            .and_then(|ppr| ppr.find(&w("ind")))
            .and_then(|ind| ind.get(&w("left")));
        let left = match left_attr {
            Some(a) if !a.is_empty() && a.chars().all(pystr::is_digit) => py_int(a)?,
            _ => 360,
        };
        let indent = (pystr::round_half_even((left - 360) as f64 / 360.0) as i64).max(0);
        let mut inlines = self.inlines(p)?;
        let mut numbered = false;
        if let Some(Inline::Text(t)) = inlines.first() {
            if RE_ALG_NUMBER.fullmatch(&t.value).is_some() {
                numbered = true;
                inlines.remove(0);
            }
        }
        Ok(AlgLine {
            indent,
            inlines,
            number: numbered,
        })
    }

    // -- description lists ----------------------------------------------------

    /// A description-list item paragraph -> `(term, definition)`.
    fn description_item(&self, p: &Element) -> R<Option<(Vec<Inline>, Vec<Inline>)>> {
        let Some(ppr) = p.find(&w("pPr")) else {
            return Ok(None);
        };
        if ppr.find(&w("ind")).is_none() || ppr.find(&w("numPr")).is_some() {
            return Ok(None);
        }
        let inlines = self.inlines(p)?;
        let mut term = Vec::new();
        let mut i = 0;
        while i < inlines.len() {
            match &inlines[i] {
                Inline::Emphasis(e) if e.kind == "bold" => term.extend(e.inlines.iter().cloned()),
                _ => break,
            }
            i += 1;
        }
        if term.is_empty() || i >= inlines.len() {
            return Ok(None);
        }
        let Inline::Text(sep) = &inlines[i] else {
            return Ok(None);
        };
        let Some(rest) = sep.value.strip_prefix("  ") else {
            return Ok(None);
        };
        let mut definition = if rest.is_empty() {
            Vec::new()
        } else {
            vec![text_inline(rest)]
        };
        definition.extend(inlines[i + 1..].iter().cloned());
        Ok(Some((term, definition)))
    }

    // -- structured document tags ---------------------------------------------

    fn sdt(&mut self, sdt: &Element) -> R<Vec<Block>> {
        let Some(content) = sdt.find(&w("sdtContent")) else {
            return Ok(Vec::new());
        };
        let tag = sdt
            .find(&w("sdtPr"))
            .and_then(|pr| pr.find(&w("tag")))
            .and_then(wval);
        match tag {
            Some(BIB_SDT_TAG) => Ok(vec![self.bibliography_from_sdt(content)?]),
            Some(FIG_SDT_TAG) => Ok(vec![self.figure_from_sdt(content)?]),
            _ => self.read_body(content),
        }
    }

    /// Recover a figure (incl. a sub-figure grid) from its tagged SDT.
    fn figure_from_sdt(&self, content: &Element) -> R<Block> {
        let mut paths = Vec::new();
        for d in content.iter(Some(&w("drawing"))) {
            paths.push(drawing_descr(d)?);
        }
        let mut caption = None;
        for p in content.findall(&w("p")) {
            if style(p).as_deref() == Some("Caption") {
                if let Some(cap) = self.caption_after(p, "Figure")? {
                    caption = Some(cap);
                    break;
                }
            }
        }
        if paths.len() <= 1 {
            let image = paths.pop().map(|path| Image {
                path,
                ..Image::default()
            });
            return Ok(Block::Figure(Figure {
                image,
                caption,
                ..Figure::default()
            }));
        }
        let subfigures = paths
            .into_iter()
            .map(|path| SubFigure {
                image: Some(Image {
                    path,
                    ..Image::default()
                }),
                ..SubFigure::default()
            })
            .collect();
        Ok(Block::Figure(Figure {
            image: None,
            caption,
            subfigures,
            ..Figure::default()
        }))
    }

    /// Recover the reference list from its tagged SDT.
    fn bibliography_from_sdt(&self, content: &Element) -> R<Block> {
        let mut entries: Vec<CSLItem> = Vec::new();
        for p in content.findall(&w("p")) {
            if style(p).as_deref() != Some("Bibliography") {
                continue;
            }
            let text = RE_BIB_NUMBER.sub("", &plain_text(&self.inlines(p)?));
            let bm = self.bookmark(p).unwrap_or_default();
            let fallback = format!("ref{}", entries.len() + 1);
            let cid = match bm.strip_prefix("bib_") {
                Some(rest) if !rest.is_empty() => rest.to_string(),
                Some(_) => fallback,
                None => fallback,
            };
            let mut csl_fields = tex2word_ir::dict::Dict::new();
            csl_fields.insert("note".to_string(), Value::Str(text));
            entries.push(CSLItem {
                id: cid,
                item_type: String::new(),
                csl_fields,
            });
        }
        Ok(Block::Bibliography(Bibliography {
            entries,
            ..Bibliography::default()
        }))
    }

    // -- tables ---------------------------------------------------------------

    fn table(&mut self, tbl: &Element) -> R<Block> {
        let mut rows = Vec::new();
        let mut ncols = 0;
        for tr in tbl.findall(&w("tr")) {
            let mut cells = Vec::new();
            for tc in tr.findall(&w("tc")) {
                let mut span = 1;
                let mut shade = None;
                if let Some(tcpr) = tc.find(&w("tcPr")) {
                    if let Some(gs) = tcpr.find(&w("gridSpan")) {
                        span = py_int(wval(gs).filter(|v| !v.is_empty()).unwrap_or("1"))?;
                    }
                    if let Some(fill) = tcpr.find(&w("shd")).and_then(|s| s.get(&w("fill"))) {
                        if !fill.is_empty() && fill != "auto" {
                            shade = Some(fill.to_string());
                        }
                    }
                }
                let mut blocks = Vec::new();
                for p in tc.findall(&w("p")) {
                    blocks.push(self.paragraph(p)?.unwrap_or_else(|| paragraph(Vec::new())));
                }
                cells.push(TableCell {
                    blocks,
                    colspan: span,
                    shade,
                    ..TableCell::default()
                });
            }
            ncols = ncols.max(cells.iter().map(|c| c.colspan).sum::<i64>());
            rows.push(TableRow {
                cells,
                ..TableRow::default()
            });
        }
        Ok(Block::Table(Table {
            rows,
            colspec: vec!["left".to_string(); ncols.max(0) as usize],
            ..Table::default()
        }))
    }
}

/// The list at `path` (a root index, then (item, block) index pairs).
fn list_at<'a>(roots: &'a mut [ItemList], path: &[usize]) -> &'a mut ItemList {
    let mut cur = &mut roots[path[0]];
    let mut i = 1;
    while i + 1 < path.len() {
        let Block::ItemList(next) = &mut cur.items[path[i]].blocks[path[i + 1]] else {
            unreachable!("paths point at lists");
        };
        cur = next;
        i += 2;
    }
    cur
}

fn style(p: &Element) -> Option<String> {
    p.find(&w("pPr"))?
        .find(&w("pStyle"))
        .and_then(wval)
        .map(str::to_string)
}

fn style_or_normal(p: &Element) -> String {
    style(p)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Normal".into())
}

fn heading_level(style: &str) -> Option<i64> {
    Some(match style {
        "Heading1" => 1,
        "Heading2" => 2,
        "Heading3" => 3,
        "Heading4" => 4,
        "Heading5" => 5,
        _ => return None,
    })
}

fn is_heading(p: &Element) -> bool {
    heading_level(&style(p).unwrap_or_default()).is_some()
}

fn numpr(p: &Element) -> Option<&Element> {
    p.find(&w("pPr"))?.find(&w("numPr"))
}

fn instr_text(p: &Element) -> String {
    p.iter(Some(&w("instrText")))
        .iter()
        .map(|t| t.text.as_deref().unwrap_or(""))
        .collect()
}

fn toc_block(p: &Element) -> Option<Block> {
    let instr = instr_text(p);
    if !pystr::split_ws(&instr).contains(&"TOC") {
        return None;
    }
    let kind = if instr.contains("\\c \"Figure\"") {
        "figures"
    } else if instr.contains("\\c \"Table\"") {
        "tables"
    } else {
        "contents"
    };
    Some(Block::TableOfContents(TableOfContents {
        kind: kind.into(),
    }))
}

fn styled_text(r: &Element, text: String) -> R<Inline> {
    let mut node = text_inline(text);
    let Some(rpr) = r.find(&w("rPr")) else {
        return Ok(node);
    };
    if rpr.find(&w("rStyle")).and_then(wval) == Some("Hyperlink") {
        return Ok(node); // handled by the hyperlink wrapper
    }
    for (child, kind) in EMPH_FROM_RPR {
        if toggle_on(rpr.find(&w(child))) {
            node = Inline::Emphasis(Emphasis {
                inlines: vec![node],
                kind: kind.into(),
            });
        }
    }
    if let Some(fonts) = rpr.find(&w("rFonts")) {
        if fonts.get(&w("ascii")).unwrap_or("").starts_with("Consolas") {
            node = Inline::Emphasis(Emphasis {
                inlines: vec![node],
                kind: "typewriter".into(),
            });
        }
    }
    if let Some(v) = rpr.find(&w("vertAlign")).and_then(wval) {
        if v == "superscript" || v == "subscript" {
            node = Inline::Emphasis(Emphasis {
                inlines: vec![node],
                kind: v.into(),
            });
        }
    }
    let fg = rpr
        .find(&w("color"))
        .and_then(wval)
        .filter(|v| !v.is_empty() && *v != "auto");
    let bg = rpr
        .find(&w("shd"))
        .and_then(|s| s.get(&w("fill")))
        .filter(|v| !v.is_empty() && *v != "auto");
    if fg.is_some() || bg.is_some() {
        node = Inline::Colored(Colored {
            inlines: vec![node],
            fg: fg.map(str::to_string),
            bg: bg.map(str::to_string),
        });
    }
    if let Some(sz) = rpr.find(&w("sz")).and_then(wval) {
        if !sz.is_empty() && sz.chars().all(pystr::is_digit) {
            node = Inline::FontSize(FontSize {
                inlines: vec![node],
                half_points: py_int(sz)?,
            });
        }
    }
    Ok(node)
}

/// A Zotero/Mendeley `...CSL_CITATION {...}` field -> a Cite.
fn csl_cite(instr: &str, result: &str) -> Inline {
    let mut keys: Vec<String> = Vec::new();
    // any failure keeps the keys gathered so far (Python's `except: pass`)
    let _ = (|| -> Option<()> {
        let payload_text = instr.split_once("CSL_CITATION")?.1;
        let payload = json::parse(pystr::strip(payload_text)).ok()?;
        let obj = payload.as_object()?;
        let items = match obj.iter().find(|(k, _)| k == "citationItems") {
            Some((_, v)) => v.clone(),
            None => Value::Array(Vec::new()),
        };
        for it in py_iter(&items)? {
            let it_obj = it.as_object()?;
            let get = |k: &str| it_obj.iter().find(|(kk, _)| kk == k).map(|(_, v)| v);
            let uris = get("uris").filter(|v| v.py_truthy());
            let mut key = match uris {
                Some(u) => {
                    let first = py_index0(u)?;
                    let s = first.as_str()?;
                    s.rsplit_once('/').map(|(_, t)| t).unwrap_or(s).to_string()
                }
                None => String::new(),
            };
            if key.is_empty() {
                let item_data = match get("itemData") {
                    Some(d) => Some(d.as_object()?),
                    None => None,
                };
                let data_id =
                    item_data.and_then(|d| d.iter().find(|(k, _)| k == "id").map(|(_, v)| v));
                let v = data_id
                    .filter(|v| v.py_truthy())
                    .or_else(|| get("id").filter(|v| v.py_truthy()));
                key = v.map(Value::py_str).unwrap_or_default();
            }
            if !key.is_empty() {
                keys.push(key);
            }
        }
        Some(())
    })();
    Inline::Cite(Cite {
        keys,
        mode: "paren".into(),
        rendered: if result.is_empty() {
            None
        } else {
            Some(result.to_string())
        },
        ..Cite::default()
    })
}

/// `for x in v` over a JSON value (`None` where Python raises).
fn py_iter(v: &Value) -> Option<Vec<Value>> {
    match v {
        Value::Array(a) => Some(a.clone()),
        Value::Object(o) => Some(o.iter().map(|(k, _)| Value::Str(k.clone())).collect()),
        Value::Str(s) => Some(s.chars().map(|c| Value::Str(c.to_string())).collect()),
        _ => None,
    }
}

/// `v[0]` over a truthy JSON value (`None` where Python raises).
fn py_index0(v: &Value) -> Option<Value> {
    match v {
        Value::Array(a) => a.first().cloned(),
        Value::Str(s) => s.chars().next().map(|c| Value::Str(c.to_string())),
        _ => None,
    }
}

/// An EndNote `ADDIN EN.CITE ...<rec-number>...` field -> a Cite.
fn endnote_cite(instr: &str, result: &str) -> Inline {
    let mut keys = RE_REC_NUMBER.findall(instr);
    if keys.is_empty() {
        keys = RE_EN_KEY.findall(instr);
    }
    Inline::Cite(Cite {
        keys: keys
            .into_iter()
            .map(pystr::strip)
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .collect(),
        mode: "paren".into(),
        rendered: if result.is_empty() {
            None
        } else {
            Some(result.to_string())
        },
        ..Cite::default()
    })
}
