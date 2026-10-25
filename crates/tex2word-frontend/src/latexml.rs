//! LaTeXML front-end (port of `frontend/latexml.py`): shell out to `latexml`
//! for genuine TeX expansion and map its XML to the IR. Opt-in via
//! `--frontend latexml`; falls back to the pure front-end (with a warning)
//! when `latexml` is missing or fails.

use std::io;
use std::path::PathBuf;
use std::process::Command;

use tex2word_ir::pystr;
use tex2word_ir::{
    Block, Cite, CodeBlock, ConversionReport, Document, DocumentMeta, Emphasis, Figure, Footnote, Heading, Image,
    Inline, ItemList, LineBreak, Link, ListItem, Math, MathBlock, Paragraph, Quote, Ref, Table, TableCell, TableRow,
};
use tex2word_ooxml::parse::fromstring;
use tex2word_ooxml::xml::Element;

pub const LTX: &str = "http://dlmf.nist.gov/LaTeXML";

fn section_level(tag: &str) -> Option<i64> {
    Some(match tag {
        "part" | "chapter" | "section" => 1,
        "subsection" => 2,
        "subsubsection" => 3,
        "paragraph" | "subparagraph" => 4,
        _ => return None,
    })
}

const FONT_EMPH: &[(&str, &str)] = &[
    ("bold", "bold"),
    ("italic", "italic"),
    ("smallcaps", "smallcaps"),
    ("typewriter", "typewriter"),
    ("sansserif", "italic"),
];

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| {
        std::fs::metadata(p).is_ok_and(|m| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                m.is_file() && m.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                m.is_file()
            }
        })
    })
}

/// True if the `latexml` executable is on the PATH.
pub fn latexml_available() -> bool {
    which("latexml").is_some()
}

/// Run `latexml` on `source` and return the LaTeXML XML bytes.
pub fn run_latexml(source: &str, base_dir: &str) -> Result<Vec<u8>, String> {
    if !latexml_available() {
        return Err("latexml executable not found".into());
    }
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let td = std::env::temp_dir().join(format!("tex2word-latexml-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&td).map_err(|e| e.to_string())?;
    let result = (|| {
        let inp = td.join("main.tex");
        std::fs::write(&inp, source).map_err(|e| e.to_string())?;
        let out = td.join("out.xml");
        let proc = Command::new("latexml")
            .arg("--nocomments")
            .arg(format!("--path={base_dir}"))
            .arg(format!("--dest={}", out.display()))
            .arg(&inp)
            .output()
            .map_err(|e| e.to_string())?;
        let data = std::fs::read(&out).unwrap_or_default();
        if !proc.status.success() || data.is_empty() {
            let err = String::from_utf8_lossy(&proc.stderr);
            let err = pystr::strip(&err);
            let tail: String = {
                let chars: Vec<char> = err.chars().collect();
                chars[chars.len().saturating_sub(400)..].iter().collect()
            };
            let rc = proc.status.code().unwrap_or(-1);
            return Err(format!(
                "latexml rc={rc}, {}B output; {}",
                data.len(),
                if tail.is_empty() { "(no stderr)".to_string() } else { tail }
            ));
        }
        Ok(data)
    })();
    let _ = std::fs::remove_dir_all(&td);
    result
}

/// Parse via latexml, falling back to the pure front-end on failure.
pub fn parse_document(source: &str, base_dir: &str) -> io::Result<(Document, ConversionReport)> {
    let xml = match run_latexml(source, base_dir) {
        Ok(x) => x,
        Err(exc) => {
            let (doc, mut report) = crate::parser::parse_document(source, base_dir, None)?;
            report.warn("latexml", format!("latexml unavailable/failed ({exc}); used pure front-end"));
            return Ok((doc, report));
        }
    };
    let mut report = ConversionReport::new();
    let doc = match parse_latexml_xml(&xml, &mut report) {
        Ok(d) => d,
        Err(e) => {
            let (doc, mut report) = crate::parser::parse_document(source, base_dir, None)?;
            report.warn("latexml", format!("latexml unavailable/failed ({e}); used pure front-end"));
            return Ok((doc, report));
        }
    };
    report.info("latexml", "parsed with the latexml front-end");
    Ok((doc, report))
}

/// Map a LaTeXML XML document to the IR.
pub fn parse_latexml_xml(xml: &[u8], report: &mut ConversionReport) -> Result<Document, String> {
    let root = fromstring(xml).map_err(|e| e.0)?;
    Ok(Mapper { _report: report, meta: DocumentMeta::default() }.document(&root))
}

fn ltx(name: &str) -> String {
    format!("{{{LTX}}}{name}")
}

fn label_of(e: &Element) -> Option<String> {
    let labels = e.get("labels").filter(|l| !l.is_empty())?;
    let first = pystr::split_ws(labels).into_iter().next()?;
    Some(first.strip_prefix("LABEL:").unwrap_or(first).to_string())
}

/// The first descendant (not self) with the Clark tag (`.//tag`).
fn find_desc<'a>(e: &'a Element, clark: &str) -> Option<&'a Element> {
    e.children.iter().find_map(|c| c.iter(Some(clark)).into_iter().next())
}

fn find_all_desc<'a>(e: &'a Element, clark: &str) -> Vec<&'a Element> {
    e.children.iter().flat_map(|c| c.iter(Some(clark))).collect()
}

struct Mapper<'r> {
    _report: &'r mut ConversionReport,
    meta: DocumentMeta,
}

impl Mapper<'_> {
    fn document(mut self, root: &Element) -> Document {
        // LaTeXML wraps the body in <ltx:document>; find it (root may be it)
        let doc_el = if root.local == "document" { root } else { root.find(&ltx("document")).unwrap_or(root) };
        let blocks = self.blocks(doc_el);
        Document { blocks, meta: self.meta, ..Document::default() }
    }

    fn blocks(&mut self, parent: &Element) -> Vec<Block> {
        let mut out = Vec::new();
        for child in parent.elements() {
            self.block(child, &parent.local, &mut out);
        }
        out
    }

    fn block(&mut self, e: &Element, parent_tag: &str, out: &mut Vec<Block>) {
        let tag = e.local.as_str();
        if let Some(level) = section_level(tag) {
            let inlines = e.find(&ltx("title")).map_or_else(Vec::new, |t| self.inlines(Some(t)));
            let numbered = !e.get("class").unwrap_or("").contains("unnumbered");
            out.push(Block::Heading(Heading { level, inlines, label: label_of(e), numbered, ..Heading::default() }));
            for child in e.elements() {
                if child.local != "title" {
                    self.block(child, tag, out);
                }
            }
            return;
        }
        match tag {
            "title" if parent_tag == "document" => self.meta.title = Some(self.inlines(Some(e))),
            "creator" => {
                let a = self.inlines(Some(e));
                self.meta.authors.push(a);
            }
            "abstract" => self.meta.abstract_ = Some(self.blocks(e)),
            "para" => {
                let b = self.blocks(e);
                out.extend(b);
            }
            "p" => {
                let inl = self.inlines(Some(e));
                if !inl.is_empty() {
                    out.push(Block::Paragraph(Paragraph { inlines: inl, align: None }));
                }
            }
            "equation" | "equationgroup" => out.push(self.equation(e)),
            "itemize" | "enumerate" | "description" => out.push(self.list(e, tag)),
            "quote" | "block" => {
                let b = self.blocks(e);
                out.push(Block::Quote(Quote { blocks: b, ..Quote::default() }));
            }
            "tabular" => out.push(self.table(e)),
            "figure" | "float" => out.push(self.figure(e)),
            "verbatim" | "listing" => out.push(Block::CodeBlock(CodeBlock { text: e.itertext(), lang: None })),
            "note" => {} // footnotes handled inline
            _ => {
                // unknown container: recurse, preserving whatever blocks it holds
                let inner = self.blocks(e);
                if !inner.is_empty() {
                    out.extend(inner);
                } else {
                    let inl = self.inlines(Some(e));
                    if !inl.is_empty() {
                        out.push(Block::Paragraph(Paragraph { inlines: inl, align: None }));
                    }
                }
            }
        }
    }

    fn equation(&mut self, e: &Element) -> Block {
        let tex = find_desc(e, &ltx("Math")).and_then(|m| m.get("tex")).unwrap_or("").to_string();
        let numbered = e.get("refnum").is_some() || label_of(e).is_some();
        Block::MathBlock(MathBlock { latex: tex, numbered, env: "equation".into(), label: label_of(e), line_labels: Vec::new() })
    }

    fn list(&mut self, e: &Element, tag: &str) -> Block {
        let mut items = Vec::new();
        for item in e.findall(&ltx("item")) {
            let tag_el = item.find(&ltx("tag"));
            let term = match (tag == "description", tag_el) {
                (true, Some(t)) => Some(self.inlines(Some(t))),
                _ => None,
            };
            let blocks = self.blocks(item);
            items.push(ListItem { blocks, term, label: None });
        }
        Block::ItemList(ItemList { ordered: tag == "enumerate", items, description: tag == "description" })
    }

    fn table(&mut self, e: &Element) -> Block {
        let mut rows = Vec::new();
        for tr in e.findall(&ltx("tr")) {
            let mut cells = Vec::new();
            for td in tr.findall(&ltx("td")) {
                let span = pystr::parse_int(td.get("colspan").filter(|v| !v.is_empty()).unwrap_or("1")).unwrap_or(1);
                let inl = self.inlines(Some(td));
                cells.push(TableCell {
                    blocks: vec![Block::Paragraph(Paragraph { inlines: inl, align: None })],
                    colspan: span,
                    ..TableCell::default()
                });
            }
            rows.push(TableRow { cells, ..TableRow::default() });
        }
        let ncols = rows.iter().map(|r: &TableRow| r.cells.len()).max().unwrap_or(1);
        Block::Table(Table { rows, colspec: vec!["left".to_string(); ncols], ..Table::default() })
    }

    fn figure(&mut self, e: &Element) -> Block {
        let image = find_desc(e, &ltx("graphics")).map(|g| {
            let path = g
                .get("graphic")
                .filter(|v| !v.is_empty())
                .or_else(|| g.get("candidates").filter(|v| !v.is_empty()))
                .unwrap_or("")
                .to_string();
            let ext = path.rsplit_once('.').map_or(String::new(), |(_, e)| e.to_string());
            Image { path, original_format: ext, ..Image::default() }
        });
        let caption = e.find(&ltx("caption")).map(|c| self.inlines(Some(c)));
        Block::Figure(Figure { image, caption, label: label_of(e), ..Figure::default() })
    }

    // -- inline ----------------------------------------------------------- //

    fn inlines(&mut self, e: Option<&Element>) -> Vec<Inline> {
        let Some(e) = e else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(t) = e.text.as_deref().filter(|t| !t.is_empty()) {
            out.push(Inline::text(t));
        }
        for child in &e.children {
            if child.is_element() {
                self.inline(child, &mut out);
            }
            if let Some(t) = child.tail.as_deref().filter(|t| !t.is_empty()) {
                out.push(Inline::text(t));
            }
        }
        merge_text(out)
    }

    fn inline(&mut self, e: &Element, out: &mut Vec<Inline>) {
        match e.local.as_str() {
            "Math" => out.push(Inline::Math(Math { latex: e.get("tex").unwrap_or("").to_string() })),
            "text" => {
                let font = e.get("font").unwrap_or("");
                let kind = FONT_EMPH.iter().find(|(k, _)| font.contains(k)).map(|(_, v)| *v);
                let inner = self.inlines(Some(e));
                match kind {
                    Some(k) => out.push(Inline::Emphasis(Emphasis { inlines: inner, kind: k.into() })),
                    None => out.extend(inner), // unstyled group -> transparent
                }
            }
            "emph" => {
                let inner = self.inlines(Some(e));
                out.push(Inline::Emphasis(Emphasis { inlines: inner, kind: "italic".into() }));
            }
            "ref" => {
                let raw = e
                    .get("labelref")
                    .filter(|v| !v.is_empty())
                    .or_else(|| e.get("idref").filter(|v| !v.is_empty()))
                    .unwrap_or("");
                let labelref = raw.strip_prefix("LABEL:").unwrap_or(raw);
                if !labelref.is_empty() {
                    out.push(Inline::Ref(Ref { key: labelref.into(), ref_kind: "generic".into(), ..Ref::default() }));
                } else if let Some(href) = e.get("href").filter(|h| !h.is_empty()) {
                    let inner = self.inlines(Some(e));
                    let inlines = if inner.is_empty() { vec![Inline::text(href)] } else { inner };
                    out.push(Inline::Link(Link { inlines, url: href.into(), anchor: None }));
                } else {
                    let inner = self.inlines(Some(e));
                    out.extend(inner);
                }
            }
            "cite" => {
                let mut keys: Vec<String> = Vec::new();
                for br in find_all_desc(e, &ltx("bibref")) {
                    keys.extend(br.get("bibrefs").unwrap_or("").split(',').map(str::to_string));
                }
                keys.retain(|k| !k.is_empty());
                out.push(Inline::Cite(Cite { keys, mode: "paren".into(), ..Cite::default() }));
            }
            "note" => {
                let inner = self.inlines(Some(e));
                out.push(Inline::Footnote(Footnote { inlines: inner }));
            }
            "break" => out.push(Inline::LineBreak(LineBreak {})),
            _ => {
                let inner = self.inlines(Some(e));
                out.extend(inner);
            }
        }
    }
}

fn merge_text(nodes: Vec<Inline>) -> Vec<Inline> {
    let mut merged: Vec<Inline> = Vec::new();
    for n in nodes {
        if let (Inline::Text(t), Some(Inline::Text(last))) = (&n, merged.last_mut()) {
            last.value.push_str(&t.value);
            continue;
        }
        merged.push(n);
    }
    merged.retain(|n| !matches!(n, Inline::Text(t) if t.value.is_empty()));
    merged
}
