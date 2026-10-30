//! IR -> `word/document.xml` (port of `backend/document.py`).
//!
//! Walks the IR and emits OOXML, dropping to raw math (OMML) and field
//! plumbing as needed. Math goes through the cascade; on failure it degrades
//! to a raw text run recorded in the report -- never an abort.

use std::collections::HashMap;

use tex2word_ir::crossref::{figure_bookmark, sanitize_bookmark};
use tex2word_ir::pyos;
use tex2word_ir::pystr;
use tex2word_ir::{
    Algorithm, Bibliography, Block, CSLItem, Cite, CodeBlock, Comment, ConversionReport, Dict,
    Document, DocumentMeta, Emphasis, Endnote, Figure, Footnote, Heading, Image, Inline, ItemList,
    Link, ListItem, Math, MathBlock, Paragraph, Quote, Ref, Table, TableCell, TableOfContents,
    Text, Theorem,
};
use tex2word_math::cascade::{ImageMathRenderer, MathCascade, MathResult};
use tex2word_ooxml::xml::{el, preserve_space, qn, serialize, sub, text_el, Element, XmlError};

use crate::package::{
    BULLET_NUM_ID, FIRST_LIST_NUM_ID, HEADING_APPENDIX_NUM_ID, HEADING_NUM_ID, PART_NUM_ID,
};
use crate::{fields, images, raster, tikz, zotero};

fn heading_style(level: i64) -> &'static str {
    match level {
        0 => "Title",
        1 => "Heading1",
        2 => "Heading2",
        3 => "Heading3",
        4 => "Heading4",
        _ => "Heading5",
    }
}

/// multi-line display envs that number each line
fn is_multiline_numbered_env(env: &str) -> bool {
    matches!(env, "align" | "alignat" | "flalign" | "eqnarray")
}

/// Word TOC field instructions per `\tableofcontents`/`\listof…`.
fn toc_spec(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "figures" => ("List of Figures", "TOC \\h \\z \\c \"Figure\""),
        "tables" => ("List of Tables", "TOC \\h \\z \\c \"Table\""),
        _ => ("Contents", "TOC \\o \"1-3\" \\h \\z \\u"),
    }
}

const PIC_URI: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
const IMAGE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
/// `w:sdt` tags marking regions for faithful round-trip recovery.
pub const BIB_SDT_TAG: &str = "tex2word:bibliography";
pub const FIG_SDT_TAG: &str = "tex2word:figure";

/// Accumulated character formatting, applied at the text leaf.
#[derive(Debug, Clone, Default, PartialEq)]
struct RunStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    typewriter: bool,
    smallcaps: bool,
    hyperlink: bool,
    superscript: bool,
    subscript: bool,
    strike: bool,
    highlight: bool,
    color: Option<String>,
    shade: Option<String>,
    size: Option<i64>,
}

impl RunStyle {
    fn with_flag(&self, flag: &str) -> RunStyle {
        let mut s = self.clone();
        match flag {
            "bold" => s.bold = true,
            "italic" => s.italic = true,
            "underline" => s.underline = true,
            "typewriter" => s.typewriter = true,
            "smallcaps" => s.smallcaps = true,
            "hyperlink" => s.hyperlink = true,
            "superscript" => s.superscript = true,
            "subscript" => s.subscript = true,
            "strike" => s.strike = true,
            "highlight" => s.highlight = true,
            _ => {}
        }
        s
    }

    fn italic(&self) -> RunStyle {
        RunStyle {
            italic: true,
            ..self.clone()
        }
    }
}

/// cleveref-style type prefixes, keyed by (target kind, ref style).
fn ref_prefix(kind: &str, style: &str) -> Option<&'static str> {
    let names = match kind {
        "figure" => ("fig. ", "Figure "),
        "table" => ("tab. ", "Table "),
        "section" => ("sec. ", "Section "),
        "theorem" => ("thm. ", "Theorem "),
        "algorithm" => ("alg. ", "Algorithm "),
        "listing" => ("lst. ", "Listing "),
        "equation" => ("eq. ", "Equation "),
        _ => return None,
    };
    match style {
        "abbrev" => Some(names.0),
        "full" => Some(names.1),
        _ => None,
    }
}

fn bookmark_for(label: &str) -> String {
    sanitize_bookmark(label)
}

/// Writer options (the `DocumentWriter` keyword arguments).
pub struct WriterOptions {
    pub base_dir: String,
    pub image_math_renderer: Option<Box<dyn ImageMathRenderer>>,
    pub number_by_section: bool,
    pub citation_mode: String,
    pub columns: i64,
    /// page geometry from a `--reference-doc` (ordered attributes)
    pub page_pgsz: Option<Vec<(String, String)>>,
    pub page_pgmar: Option<Vec<(String, String)>>,
    /// (element, w:type, r:id) header/footer references
    pub header_footer_refs: Vec<(String, String, String)>,
    /// document preamble (for compiling TikZ pictures)
    pub preamble: String,
}

impl Default for WriterOptions {
    fn default() -> Self {
        WriterOptions {
            base_dir: ".".into(),
            image_math_renderer: None,
            number_by_section: false,
            citation_mode: "static".into(),
            columns: 1,
            page_pgsz: None,
            page_pgmar: None,
            header_footer_refs: Vec::new(),
            preamble: String::new(),
        }
    }
}

/// Builds `word/document.xml` and the notes/comments/media parts.
pub struct DocumentWriter<'r> {
    report: &'r mut ConversionReport,
    base_dir: String,
    preamble: String,
    number_by_section: bool,
    citation_mode: String,
    columns: i64,
    page_pgsz: Option<Vec<(String, String)>>,
    page_pgmar: Option<Vec<(String, String)>>,
    /// the output page's text column (EMU)
    pub text_width_emu: i64,
    pub text_height_emu: i64,
    header_footer_refs: Vec<(String, String, String)>,
    cite_items: Dict<CSLItem>,
    /// archive path -> bytes, e.g. "word/media/image1.png"
    pub media: Vec<(String, Vec<u8>)>,
    /// relationship XML strings for word/_rels/document.xml.rels
    pub document_rels: Vec<String>,
    image_counter: usize,
    drawing_counter: usize,
    media_by_hash: HashMap<Vec<u8>, (String, usize)>,
    footnotes: Vec<Element>,
    footnote_counter: usize,
    endnotes: Vec<Element>,
    endnote_counter: usize,
    comments: Vec<Element>,
    comment_counter: usize,
    math: MathCascade,
    /// (numId, ilvl, start) of every ordered list's own numbering instance
    pub list_restarts: Vec<(i64, i64, i64)>,
    /// number of Heading 1 paragraphs so far, and per SEQ counter the value
    /// it had at that counter's last section-relative number
    section_epoch: i64,
    seq_epoch: HashMap<String, i64>,
}

enum Region {
    Title,
    Block(usize),
}

fn attrs(pairs: &[(String, String)]) -> Vec<(&str, &str)> {
    pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

fn get_ppr(p: &mut Element) -> &mut Element {
    p.find_mut(&qn("w:pPr"))
        .expect("styled paragraph has a pPr")
}

impl<'r> DocumentWriter<'r> {
    pub fn new(report: &'r mut ConversionReport, opts: WriterOptions) -> Self {
        let mut w = DocumentWriter {
            report,
            base_dir: opts.base_dir,
            preamble: opts.preamble,
            number_by_section: opts.number_by_section,
            citation_mode: opts.citation_mode,
            columns: opts.columns.max(1),
            page_pgsz: opts.page_pgsz,
            page_pgmar: opts.page_pgmar,
            text_width_emu: 0,
            text_height_emu: 0,
            header_footer_refs: opts.header_footer_refs,
            cite_items: Dict::new(),
            media: Vec::new(),
            document_rels: Vec::new(),
            image_counter: 0,
            drawing_counter: 0,
            media_by_hash: HashMap::new(),
            footnotes: Vec::new(),
            footnote_counter: 0,
            endnotes: Vec::new(),
            endnote_counter: 0,
            comments: Vec::new(),
            comment_counter: 0,
            math: MathCascade::new(opts.image_math_renderer),
            list_restarts: Vec::new(),
            section_epoch: 0,
            seq_epoch: HashMap::new(),
        };
        let (tw, th) = w.text_box_emu();
        w.text_width_emu = tw;
        w.text_height_emu = th;
        fields::reset_bookmark_ids();
        w
    }

    // -- public ----------------------------------------------------------- //

    /// Build `word/document.xml`.
    pub fn build(&mut self, doc: &Document) -> Result<Vec<u8>, XmlError> {
        zotero::reset_ids();
        // CJK glyphs inside math get the document's East-Asian font (xeCJK)
        self.math.cjk_font = doc
            .meta
            .cjk_main_font
            .clone()
            .filter(|f| !f.is_empty())
            .or_else(|| doc.meta.cjk_sans_font.clone().filter(|f| !f.is_empty()));
        for block in &doc.blocks {
            if let Block::Bibliography(b) = block {
                self.cite_items = b
                    .entries
                    .iter()
                    .map(|e| (e.id.clone(), e.clone()))
                    .collect();
            }
        }
        let mut body = el("w:body", &[]);
        self.emit_body(doc, &mut body);
        // real cached numbers for SEQ/STYLEREF/REF (viewers that don't update
        // fields would otherwise show "Figure 1" everywhere and "see 0")
        crate::field_results::fill_field_results(
            &mut body,
            &crate::package::numbering_xml(&self.list_restarts),
            self.footnotes.iter_mut().chain(self.endnotes.iter_mut()),
        )?;
        let mut document = el("w:document", &[]);
        document.append(body);
        serialize(&document)
    }

    /// Continuous section breaks let `figure*`/`table*` (and a multi-column
    /// paper's title/abstract) span the page while text flows in N columns.
    fn emit_body(&mut self, doc: &Document, body: &mut Element) {
        let n = self.columns;
        let m = &doc.meta;
        let has_title = m.title.as_ref().is_some_and(|t| !t.is_empty())
            || !m.authors.is_empty()
            || !m.affiliations.is_empty()
            || m.date.as_ref().is_some_and(|d| !d.is_empty())
            || m.abstract_.as_ref().is_some_and(|a| !a.is_empty())
            || m.keywords.as_ref().is_some_and(|k| !k.is_empty())
            || m.subtitle.as_ref().is_some_and(|s| !s.is_empty())
            || !m.front_notes.is_empty();
        let mut regions: Vec<(i64, Region)> = Vec::new();
        if has_title {
            regions.push((if n > 1 { 1 } else { n }, Region::Title));
        }
        for (i, block) in doc.blocks.iter().enumerate() {
            let spanning = match block {
                Block::Table(t) => t.spanning,
                Block::Figure(f) => f.spanning,
                _ => false,
            };
            regions.push((if n > 1 && spanning { 1 } else { n }, Region::Block(i)));
        }
        let mut prev: Option<i64> = None;
        for (cols, region) in regions {
            if let Some(p) = prev {
                if cols != p {
                    let brk = self.column_break_para(p);
                    body.append(brk);
                }
            }
            match region {
                Region::Title => self.title_block(&doc.meta, body),
                Region::Block(i) => self.block(&doc.blocks[i], body, "Normal"),
            }
            prev = Some(cols);
        }
        let sect = self.sect_pr(Some(prev.unwrap_or(n)), false);
        body.append(sect);
    }

    /// An empty paragraph carrying a continuous section break.
    fn column_break_para(&self, columns: i64) -> Element {
        let mut p = el("w:p", &[]);
        let ppr = sub(&mut p, "w:pPr", &[]);
        ppr.append(self.sect_pr(Some(columns), true));
        p
    }

    // -- title / meta ----------------------------------------------------- //

    fn title_block(&mut self, meta: &DocumentMeta, body: &mut Element) {
        if let Some(title) = meta.title.as_ref().filter(|t| !t.is_empty()) {
            let mut p = styled_paragraph("Title");
            self.inlines(title, &mut p);
            body.append(p);
        }
        if let Some(subtitle) = meta.subtitle.as_ref().filter(|s| !s.is_empty()) {
            let mut p = styled_paragraph("Subtitle");
            let em = Inline::Emphasis(Emphasis {
                inlines: subtitle.clone(),
                kind: "bold".into(),
            });
            self.inlines(std::slice::from_ref(&em), &mut p);
            body.append(p);
        }
        for author in &meta.authors {
            let mut p = styled_paragraph("Subtitle");
            self.inlines(author, &mut p);
            body.append(p);
        }
        for affil in &meta.affiliations {
            let mut p = styled_paragraph("Subtitle");
            let em = Inline::Emphasis(Emphasis {
                inlines: affil.clone(),
                kind: "italic".into(),
            });
            self.inlines(std::slice::from_ref(&em), &mut p);
            body.append(p);
        }
        for note in &meta.front_notes {
            let mut p = styled_paragraph("Subtitle");
            self.inlines(note, &mut p);
            body.append(p);
        }
        if let Some(date) = meta.date.as_ref().filter(|d| !d.is_empty()) {
            let mut p = styled_paragraph("Subtitle");
            self.inlines(date, &mut p);
            body.append(p);
        }
        if let Some(abs) = meta.abstract_.as_ref().filter(|a| !a.is_empty()) {
            for block in abs {
                self.block(block, body, "Abstract");
            }
        }
        if let Some(kw) = meta.keywords.as_ref().filter(|k| !k.is_empty()) {
            let mut p = styled_paragraph("Abstract");
            p.append(run(
                "Keywords: ",
                &RunStyle {
                    bold: true,
                    ..RunStyle::default()
                },
            ));
            self.inlines(kw, &mut p);
            body.append(p);
        }
    }

    // -- blocks ----------------------------------------------------------- //

    fn block(&mut self, block: &Block, body: &mut Element, default_style: &str) {
        match block {
            Block::Heading(h) => self.heading(h, body),
            Block::Paragraph(para) => {
                let mut p = styled_paragraph(default_style);
                if let Some(a) = para.align.as_deref().filter(|a| !a.is_empty()) {
                    set_align(&mut p, a);
                }
                self.inlines(&para.inlines, &mut p);
                body.append(p);
            }
            Block::MathBlock(m) => self.math_block(m, body),
            Block::ItemList(l) => self.list(l, body, 0, None),
            Block::Table(t) => self.table(t, body),
            Block::Figure(f) => self.figure(f, body),
            Block::CodeBlock(c) => self.code_block(c, body),
            Block::Quote(q) => {
                if q.shade.as_deref().is_some_and(|s| !s.is_empty())
                    || q.border.as_deref().is_some_and(|s| !s.is_empty())
                    || q.title.as_ref().is_some_and(|t| !t.is_empty())
                {
                    self.callout(q, body);
                } else {
                    for inner in &q.blocks {
                        self.block(inner, body, "Quote");
                    }
                }
            }
            Block::Theorem(t) => self.theorem(t, body),
            Block::Algorithm(a) => self.algorithm(a, body),
            Block::Bibliography(b) => self.bibliography(b, body),
            Block::TableOfContents(t) => toc(t, body),
            Block::Index(_) => {
                let mut p = styled_paragraph("Normal");
                for r in fields::field("INDEX \\c \"2\" \\z \"1033\"", "") {
                    p.append(r);
                }
                body.append(p);
            }
            Block::RawPassthrough(r) => {
                let mut p = styled_paragraph("SourceCode");
                p.append(run(&r.latex, &RunStyle::default()));
                body.append(p);
            }
        }
    }

    fn heading(&mut self, block: &Heading, body: &mut Element) {
        let style = heading_style(block.level);
        if style == "Heading1" {
            self.section_epoch += 1;
        }
        let mut p = styled_paragraph(style);
        if block.part && block.numbered {
            let numpr = sub(get_ppr(&mut p), "w:numPr", &[]);
            sub(numpr, "w:ilvl", &[("w:val", "0")]);
            sub(numpr, "w:numId", &[("w:val", &PART_NUM_ID.to_string())]);
        } else if block.numbered && (1..=4).contains(&block.level) {
            let numpr = sub(get_ppr(&mut p), "w:numPr", &[]);
            sub(
                numpr,
                "w:ilvl",
                &[("w:val", &(block.level - 1).to_string())],
            );
            let num_id = if block.appendix {
                HEADING_APPENDIX_NUM_ID
            } else {
                HEADING_NUM_ID
            };
            sub(numpr, "w:numId", &[("w:val", &num_id.to_string())]);
        }
        let start = block
            .label
            .as_deref()
            .filter(|l| !l.is_empty())
            .map(|l| fields::bookmark_start(&bookmark_for(l)));
        if let Some(s) = &start {
            p.append(s.clone());
        }
        self.inlines(&block.inlines, &mut p);
        if let Some(s) = &start {
            p.append(fields::bookmark_end_for(s));
        }
        body.append(p);
    }

    fn math_block(&mut self, block: &MathBlock, body: &mut Element) {
        // top-level numbered align/eqnarray keep per-line numbers; everything
        // else collapses to one column-aligned matrix
        let collapse = !(block.numbered && is_multiline_numbered_env(&block.env));
        let result = self.math.block(&block.latex, collapse, self.report);
        match result {
            MathResult::Image(data, fmt) => {
                let mut p = styled_paragraph("Normal");
                set_align(&mut p, "center");
                let mut r = el("w:r", &[]);
                r.append(self.embed_image_bytes(&data, &fmt, "equation"));
                p.append(r);
                body.append(p);
            }
            MathResult::Raw(_) => {
                let mut p = styled_paragraph("Normal");
                p.append(run(
                    &format!("\\[{}\\]", block.latex),
                    &RunStyle::default().italic(),
                ));
                body.append(p);
            }
            MathResult::Omml(omaths) => {
                for (idx, omath) in omaths.into_iter().enumerate() {
                    if block.numbered {
                        body.append(self.numbered_equation(omath, block, idx));
                    } else {
                        let mut p = styled_paragraph("Normal");
                        let mut para = el("m:oMathPara", &[]);
                        para.append(omath);
                        p.append(para);
                        body.append(p);
                    }
                }
            }
        }
    }

    fn numbered_equation(&mut self, omath: Element, block: &MathBlock, idx: usize) -> Element {
        let mut p = el("w:p", &[]);
        let ppr = sub(&mut p, "w:pPr", &[]);
        let tabs = sub(ppr, "w:tabs", &[]);
        sub(tabs, "w:tab", &[("w:val", "center"), ("w:pos", "4680")]);
        sub(tabs, "w:tab", &[("w:val", "right"), ("w:pos", "9360")]);
        // tab -> equation -> tab -> (SEQ)
        p.append(tab_run());
        p.append(omath);
        p.append(tab_run());
        p.append(run("(", &RunStyle::default()));
        // each numbered line bookmarks its own \label
        let line_label: Option<&str> = if idx < block.line_labels.len() {
            block.line_labels[idx].as_deref()
        } else if idx == 0 && block.line_labels.is_empty() {
            block.label.as_deref()
        } else {
            None
        };
        let start = line_label
            .filter(|l| !l.is_empty())
            .map(|l| fields::bookmark_start(&bookmark_for(l)));
        if let Some(s) = &start {
            p.append(s.clone());
        }
        for r in self.number_runs("Equation", self.number_by_section) {
            p.append(r);
        }
        if let Some(s) = &start {
            p.append(fields::bookmark_end_for(s));
        }
        p.append(run(")", &RunStyle::default()));
        p
    }

    fn list(&mut self, block: &ItemList, body: &mut Element, level: i64, parent_num: Option<i64>) {
        if block.description {
            for item in &block.items {
                self.description_item(item, body, level);
            }
            return;
        }
        let num_id = if !block.ordered {
            BULLET_NUM_ID
        } else if let Some(parent) = parent_num.filter(|_| block.start == 1) {
            // nested in an ordered list: the parent's instance, whose deeper
            // level restarts at each parent item -- (a), (b) under every item
            parent
        } else {
            // its own instance, so the list restarts at 1 (or at its start=)
            let id = FIRST_LIST_NUM_ID + self.list_restarts.len() as i64;
            self.list_restarts.push((id, level, block.start));
            id
        };
        for item in &block.items {
            self.list_item(item, body, level, num_id);
        }
    }

    fn numbered_para(&self, level: i64, num_id: i64) -> Element {
        let mut p = styled_paragraph("Normal");
        let numpr = sub(get_ppr(&mut p), "w:numPr", &[]);
        sub(numpr, "w:ilvl", &[("w:val", &level.to_string())]);
        sub(numpr, "w:numId", &[("w:val", &num_id.to_string())]);
        p
    }

    fn list_item(&mut self, item: &ListItem, body: &mut Element, level: i64, num_id: i64) {
        if item.term.as_ref().is_some_and(|t| !t.is_empty()) {
            // a custom \item[label] -> show the label, suppress the bullet
            self.description_item(item, body, level);
            return;
        }
        let mut marked = false;
        // an item leading with a nested list gets its own (empty) numbered
        // paragraph first so its number renders above the sub-list
        let first_structural = item
            .blocks
            .iter()
            .find(|b| matches!(b, Block::Paragraph(_) | Block::ItemList(_)));
        if matches!(first_structural, Some(Block::ItemList(_))) {
            let mut p = self.numbered_para(level, num_id);
            bookmark_list_item(item, &mut p);
            body.append(p);
            marked = true;
        }
        let ordered_parent = num_id != BULLET_NUM_ID;
        for inner in &item.blocks {
            match inner {
                Block::ItemList(l) => {
                    self.list(l, body, level + 1, ordered_parent.then_some(num_id));
                    continue;
                }
                Block::Paragraph(para) => {
                    let mut p;
                    if !marked {
                        p = self.numbered_para(level, num_id);
                        // bookmark the numbered paragraph so \ref resolves to its number
                        bookmark_list_item(item, &mut p);
                        marked = true;
                    } else {
                        p = styled_paragraph("Normal");
                        sub(
                            get_ppr(&mut p),
                            "w:ind",
                            &[("w:left", &((level + 1) * 360).to_string())],
                        );
                    }
                    self.inlines(&para.inlines, &mut p);
                    body.append(p);
                }
                other => self.block(other, body, "Normal"),
            }
        }
        if !marked {
            let mut p = self.numbered_para(level, num_id);
            bookmark_list_item(item, &mut p);
            body.append(p);
        }
    }

    fn description_item(&mut self, item: &ListItem, body: &mut Element, level: i64) {
        let mut p = styled_paragraph("Normal");
        sub(
            get_ppr(&mut p),
            "w:ind",
            &[("w:left", &((level + 1) * 360).to_string())],
        );
        if let Some(term) = item.term.as_ref().filter(|t| !t.is_empty()) {
            for t in term {
                self.emit(t, &mut p, &RunStyle::default().with_flag("bold"));
            }
            p.append(run("  ", &RunStyle::default()));
        }
        let first_para = item
            .blocks
            .iter()
            .position(|b| matches!(b, Block::Paragraph(_)));
        if let Some(i) = first_para {
            if let Block::Paragraph(fp) = &item.blocks[i] {
                self.inlines(&fp.inlines, &mut p);
            }
        }
        body.append(p);
        for (i, inner) in item.blocks.iter().enumerate() {
            if Some(i) == first_para {
                continue;
            }
            match inner {
                Block::ItemList(l) => self.list(l, body, level + 1, None),
                other => self.block(other, body, "Normal"),
            }
        }
    }

    fn table(&mut self, block: &Table, body: &mut Element) {
        let mut tbl = el("w:tbl", &[]);
        let ncols = block
            .rows
            .iter()
            .map(|r| r.cells.iter().map(|c| c.colspan).sum::<i64>())
            .max()
            .unwrap_or(1);
        let widths: Vec<Option<i64>>;
        {
            let tpr = sub(&mut tbl, "w:tblPr", &[]);
            if block.layout {
                // side-by-side minipages: a borderless grid spanning the column
                sub(tpr, "w:tblW", &[("w:w", "5000"), ("w:type", "pct")]);
                let borders = sub(tpr, "w:tblBorders", &[]);
                for side in ["top", "left", "bottom", "right", "insideH", "insideV"] {
                    sub(borders, &format!("w:{side}"), &[("w:val", "nil")]);
                }
                let col_tw = (self.text_width_emu as f64 / 635.0 / ncols.max(1) as f64) as i64;
                widths = vec![Some(col_tw); ncols.max(0) as usize];
            } else {
                sub(tpr, "w:tblW", &[("w:w", "0"), ("w:type", "auto")]);
                let borders = sub(tpr, "w:tblBorders", &[]);
                for side in ["top", "left", "bottom", "right", "insideH", "insideV"] {
                    sub(
                        borders,
                        &format!("w:{side}"),
                        &[("w:val", "single"), ("w:sz", "4"), ("w:color", "auto")],
                    );
                }
                // p{width} column widths -> twips (dxa); 914400 EMU = 1440 twips
                widths = block
                    .colwidths
                    .iter()
                    .map(|w| w.filter(|w| *w != 0.0).map(|w| (w / 635.0) as i64))
                    .collect();
            }
        }
        {
            let grid = sub(&mut tbl, "w:tblGrid", &[]);
            for i in 0..ncols.max(0) as usize {
                match widths.get(i).copied().flatten().filter(|w| *w != 0) {
                    Some(w) => {
                        sub(grid, "w:gridCol", &[("w:w", &w.to_string())]);
                    }
                    None => {
                        sub(grid, "w:gridCol", &[]);
                    }
                }
            }
        }
        // vertical-merge continuations keyed by starting column index ->
        // (rows still to merge, colspan of the merged cell)
        let mut vmerge_pending: HashMap<i64, (i64, i64)> = HashMap::new();
        for row in &block.rows {
            let mut tr = el("w:tr", &[]);
            if row.is_header {
                let trpr = sub(&mut tr, "w:trPr", &[]);
                sub(trpr, "w:tblHeader", &[]); // repeat header on each page
            }
            let mut col: i64 = 0;
            for cell in &row.cells {
                if let Some(&(remaining, merged_colspan)) = vmerge_pending.get(&col) {
                    if remaining > 0 {
                        merge_continue_cell(&mut tr, merged_colspan);
                        vmerge_pending.insert(col, (remaining - 1, merged_colspan));
                        col += merged_colspan;
                        continue;
                    }
                }
                let span_w: i64 = (col..col + cell.colspan)
                    .filter(|&c| c >= 0 && (c as usize) < widths.len())
                    .map(|c| widths[c as usize].unwrap_or(0))
                    .sum();
                self.table_cell(&mut tr, cell, if span_w != 0 { Some(span_w) } else { None });
                if cell.rowspan > 1 {
                    vmerge_pending.insert(col, (cell.rowspan - 1, cell.colspan));
                }
                col += cell.colspan;
            }
            tbl.append(tr);
        }
        body.append(tbl);
        if let Some(caption) = &block.caption {
            let c = self.caption(
                "Table",
                caption,
                block.label.as_deref(),
                None,
                block.caption_numbered,
            );
            body.append(c);
        }
    }

    fn table_cell(&mut self, tr: &mut Element, cell: &TableCell, width: Option<i64>) {
        let mut tc = el("w:tc", &[]);
        {
            let tcpr = sub(&mut tc, "w:tcPr", &[]);
            // CT_TcPr child order: tcW, gridSpan, vMerge, ..., shd
            if let Some(w) = width {
                sub(tcpr, "w:tcW", &[("w:w", &w.to_string()), ("w:type", "dxa")]);
            }
            if cell.colspan > 1 {
                sub(tcpr, "w:gridSpan", &[("w:val", &cell.colspan.to_string())]);
            }
            if cell.rowspan > 1 {
                sub(tcpr, "w:vMerge", &[("w:val", "restart")]);
            }
            if cell.border_bottom {
                let borders = sub(tcpr, "w:tcBorders", &[]);
                sub(
                    borders,
                    "w:bottom",
                    &[
                        ("w:val", "single"),
                        ("w:sz", "4"),
                        ("w:space", "0"),
                        ("w:color", "auto"),
                    ],
                );
            }
            if let Some(shade) = cell.shade.as_deref().filter(|s| !s.is_empty()) {
                sub(
                    tcpr,
                    "w:shd",
                    &[("w:val", "clear"), ("w:color", "auto"), ("w:fill", shade)],
                );
            }
        }
        let mut last_is_para = false;
        for cb in &cell.blocks {
            if let Block::Paragraph(para) = cb {
                let mut p = styled_paragraph("Normal");
                set_align(&mut p, &cell.align);
                self.inlines(&para.inlines, &mut p);
                tc.append(p);
                last_is_para = true;
            } else {
                self.block(cb, &mut tc, "Normal");
                last_is_para = false;
            }
        }
        // a table cell must end with a paragraph
        if !last_is_para {
            tc.append(styled_paragraph("Normal"));
        }
        tr.append(tc);
    }

    fn figure(&mut self, block: &Figure, body: &mut Element) {
        // a tagged block SDT so the round-trip reader recovers one Figure
        let mut sdt = el("w:sdt", &[]);
        let sdtpr = sub(&mut sdt, "w:sdtPr", &[]);
        sub(sdtpr, "w:tag", &[("w:val", FIG_SDT_TAG)]);
        let mut content = el("w:sdtContent", &[]);
        if !block.subfigures.is_empty() {
            self.subfigures(block, &mut content);
        } else if block.image.is_none() && !block.blocks.is_empty() {
            // a graphic-less figure: its body is the figure content
            for inner in &block.blocks {
                self.block(inner, &mut content, "Normal");
            }
        } else if block.image.is_none() {
            // no convertible graphic: try compiling TikZ, else caption-only
            for rendered in self.render_tikz(block) {
                content.append(rendered);
            }
        } else if let Some(img) = &block.image {
            let p = self.image_paragraph(img, None);
            content.append(p);
        }
        if let Some(caption) = &block.caption {
            let bookmark = figure_bookmark(block);
            let c = self.caption(
                "Figure",
                caption,
                block.label.as_deref(),
                bookmark.as_deref(),
                block.caption_numbered,
            );
            content.append(c);
        }
        sdt.append(content);
        body.append(sdt);
    }

    /// Compile a figure's TikZ/PGF pictures to embedded PNGs, one centred
    /// paragraph per picture (stacked in source order); empty if none render.
    /// Compile each of a figure's TikZ/PGF pictures to an embedded PNG.
    ///
    /// A figure may stack several pictures (two `\input` TikZ files in one
    /// `figure*`): each becomes its own centred image paragraph, in order.
    fn render_tikz(&mut self, block: &Figure) -> Vec<Element> {
        if block.source.is_empty() {
            return Vec::new();
        }
        let pictures = tikz::extract_pictures(&block.source);
        let results: Vec<Option<(Vec<u8>, i64, i64)>> = pictures
            .iter()
            .map(|pic| tikz::render_picture(pic, &block.source, &self.preamble, &self.base_dir))
            .collect();
        if pictures.is_empty()
            && !tikz::DRAWING_ENVS
                .iter()
                .any(|env| block.source.contains(&format!("\\begin{{{env}}}")))
        {
            return Vec::new(); // no drawing at all: the parser already reported the figure
        }
        if pictures.is_empty() || results.iter().any(Option::is_none) {
            // Be specific about why so the user can fix their setup.
            let reason = if pictures.is_empty() {
                "no recognised TikZ/PGF picture in the figure"
            } else if tikz::available_engines().is_empty() {
                "no TeX engine on PATH (install e.g. xelatex/pdflatex)"
            } else if !raster::has_pdf_support() {
                "install poppler (pdftoppm) to rasterise the compiled figure"
            } else {
                "the TikZ compile failed (set TEX2WORD_TIKZ_DEBUG=1 to see the log)"
            };
            self.report
                .warn("figure", format!("TikZ figure not rendered: {reason}"));
        }
        let mut out: Vec<Element> = Vec::new();
        for (data, w, h) in results.into_iter().flatten() {
            self.report
                .info("figure", "rendered TikZ figure to an image");
            let mut p = styled_paragraph("Normal");
            set_align(&mut p, "center");
            let mut r = el("w:r", &[]);
            let info = images::ImageInfo {
                fmt: "png".into(),
                width_px: w,
                height_px: h,
            };
            // the PNG is rasterised at DEFAULT_DPI: show it at its printed
            // size, shrunk to the text width when wider
            let physical = w * images::EMU_PER_INCH / i64::from(raster::DEFAULT_DPI);
            let limit = physical.min(self.text_width_emu).max(1);
            r.append(self.register_and_draw(&data, &info, "tikz.png", Some(limit), None));
            p.append(r);
            out.push(p);
        }
        out
    }

    fn subfigures(&mut self, block: &Figure, body: &mut Element) {
        // side by side in a borderless 1-row table; each cell holds the image
        // and its "(a)" sub-caption
        let subs = &block.subfigures;
        let ncols = subs.len().max(1);
        let col_emu = (self.text_width_emu as f64 / ncols as f64) as i64 - 90000;
        let mut tbl = el("w:tbl", &[]);
        let tpr = sub(&mut tbl, "w:tblPr", &[]);
        sub(tpr, "w:tblW", &[("w:w", "0"), ("w:type", "auto")]);
        sub(tpr, "w:jc", &[("w:val", "center")]);
        let grid = sub(&mut tbl, "w:tblGrid", &[]);
        for _ in 0..ncols {
            sub(grid, "w:gridCol", &[]);
        }
        let mut tr = el("w:tr", &[]);
        for (idx, subfig) in subs.iter().enumerate() {
            let letter = char::from_u32('a' as u32 + idx as u32).unwrap_or('?');
            let mut tc = el("w:tc", &[]);
            let tcpr = sub(&mut tc, "w:tcPr", &[]);
            sub(tcpr, "w:tcW", &[("w:w", "0"), ("w:type", "auto")]);
            match &subfig.image {
                Some(img) => {
                    let p = self.image_paragraph(img, Some(col_emu));
                    tc.append(p);
                }
                None => {
                    let mut ph = styled_paragraph("Normal");
                    set_align(&mut ph, "center");
                    ph.append(run("[sub-figure omitted]", &RunStyle::default().italic()));
                    tc.append(ph);
                }
            }
            let mut cap = styled_paragraph("Caption");
            cap.append(run(
                &format!("({letter}) "),
                &RunStyle::default().with_flag("bold"),
            ));
            if let Some(c) = subfig.caption.as_ref().filter(|c| !c.is_empty()) {
                self.inlines(c, &mut cap);
            }
            tc.append(cap);
            tr.append(tc);
        }
        tbl.append(tr);
        body.append(tbl);
    }

    fn image_paragraph(&mut self, image: &Image, max_width_emu: Option<i64>) -> Element {
        let mut p = styled_paragraph("Normal");
        set_align(&mut p, "center");
        match self.embed_image(image, max_width_emu) {
            None => p.append(run(
                &format!("[image: {}]", image.path),
                &RunStyle::default().italic(),
            )),
            Some(drawing) => {
                let mut r = el("w:r", &[]);
                r.append(drawing);
                p.append(r);
            }
        }
        p
    }

    fn embed_image(&mut self, image: &Image, max_width_emu: Option<i64>) -> Option<Element> {
        let Some(path) = self.resolve_image_path(&image.path) else {
            self.report.warn(
                "includegraphics",
                format!("image file not found: {}", image.path),
            );
            return None;
        };
        let fmt = match path.rsplit_once('.') {
            Some((_, e)) => pystr::lower(e),
            None => String::new(),
        };
        let name = pyos::basename(&path).to_string();
        if let Some(info) = images::probe(&path).filter(|i| i.embeddable()) {
            let data = std::fs::read(&path).ok()?;
            return Some(self.register_and_draw(&data, &info, &name, max_width_emu, Some(image)));
        }
        // vector formats (PDF/EPS): rasterise to PNG when a backend is available
        if matches!(fmt.as_str(), "pdf" | "eps" | "ps") {
            if let Some((data, w, h)) = raster::rasterize(&path, &fmt, raster::DEFAULT_DPI) {
                self.report.info(
                    "includegraphics",
                    format!("rasterised {fmt} -> png: {}", image.path),
                );
                let info = images::ImageInfo {
                    fmt: "png".into(),
                    width_px: w,
                    height_px: h,
                };
                return Some(self.register_and_draw(
                    &data,
                    &info,
                    &name,
                    max_width_emu,
                    Some(image),
                ));
            }
            // name the system tool that would embed it (the Python
            // reference points at its `pdf` extra instead)
            let hint = if fmt == "pdf" && !raster::has_pdf_support() {
                "install poppler (pdftoppm) to embed PDF figures".to_string()
            } else if fmt != "pdf" && !raster::has_gs() {
                "install Ghostscript (gs) to embed EPS figures".to_string()
            } else {
                format!("{fmt} rasterisation failed")
            };
            self.report.warn(
                "includegraphics",
                format!("image not embedded ({hint}): {}", image.path),
            );
            return None;
        }
        self.report.warn(
            "includegraphics",
            format!("unsupported image format '{fmt}': {}", image.path),
        );
        None
    }

    fn resolve_image_path(&self, rel: &str) -> Option<String> {
        let path = if rel.starts_with('/') {
            rel.to_string()
        } else {
            pyos::join(&self.base_dir, &[rel])
        };
        if pyos::isfile(&path) {
            return Some(path);
        }
        // graphicx resolves a missing extension against known formats
        let base = pyos::splitext(&path).0.to_string();
        for candidate in [&path, &base] {
            for ext in [".pdf", ".png", ".jpg", ".jpeg", ".eps", ".ps"] {
                let c = format!("{candidate}{ext}");
                if pyos::isfile(&c) {
                    return Some(c);
                }
            }
        }
        None
    }

    fn embed_image_bytes(&mut self, data: &[u8], fmt: &str, name: &str) -> Element {
        let info = images::probe_bytes(data, fmt);
        self.register_and_draw(data, &info, name, None, None)
    }

    fn register_and_draw(
        &mut self,
        data: &[u8],
        info: &images::ImageInfo,
        name: &str,
        max_width_emu: Option<i64>,
        image: Option<&Image>,
    ) -> Element {
        // dedup identical image bytes -> one media part + one relationship
        let rel_id = match self.media_by_hash.get(data) {
            Some((rel_id, _)) => rel_id.clone(),
            None => {
                self.image_counter += 1;
                let n = self.image_counter;
                let ext = if info.fmt == "jpg" {
                    "jpeg"
                } else {
                    info.fmt.as_str()
                };
                let rel_id = format!("rIdImg{n}");
                self.media
                    .push((format!("word/media/image{n}.{ext}"), data.to_vec()));
                self.document_rels.push(format!(
                    "<Relationship Id=\"{rel_id}\" Type=\"{IMAGE_REL_TYPE}\" Target=\"media/image{n}.{ext}\"/>"
                ));
                self.media_by_hash
                    .insert(data.to_vec(), (rel_id.clone(), n));
                rel_id
            }
        };
        // each drawing instance needs a unique wp:docPr id
        self.drawing_counter += 1;
        let draw_id = self.drawing_counter;
        let (cx, cy) = self.image_extent(info, image, max_width_emu);
        let rot = rotation(image);
        let src_rect = src_rect(info, image);
        let alt = match image {
            Some(i) if !i.alt.is_empty() => i.alt.clone(),
            _ => name.to_string(),
        };
        drawing(&rel_id, cx, cy, draw_id, name, rot, src_rect, &alt)
    }

    /// The output page's text column (width, height) in EMU.
    fn text_box_emu(&self) -> (i64, i64) {
        const EMU_PER_TWIP: i64 = 635;
        let default_sz = vec![
            ("w:w".to_string(), "12240".to_string()),
            ("w:h".to_string(), "15840".to_string()),
        ];
        let default_mar = vec![
            ("w:top".to_string(), "1440".to_string()),
            ("w:right".to_string(), "1440".to_string()),
            ("w:bottom".to_string(), "1440".to_string()),
            ("w:left".to_string(), "1440".to_string()),
        ];
        let pgsz = self.page_pgsz.clone().unwrap_or(default_sz);
        let pgmar = self.page_pgmar.clone().unwrap_or(default_mar);
        let tw = |d: &[(String, String)], key: &str| -> i64 {
            let v = d
                .iter()
                .find(|(k, _)| k == key)
                .map_or("0", |(_, v)| v.as_str());
            pystr::parse_int(v).unwrap_or(0)
        };
        let w = tw(&pgsz, "w:w") - tw(&pgmar, "w:left") - tw(&pgmar, "w:right");
        let h = tw(&pgsz, "w:h") - tw(&pgmar, "w:top") - tw(&pgmar, "w:bottom");
        (w.max(1440) * EMU_PER_TWIP, h.max(1440) * EMU_PER_TWIP)
    }

    /// Final (cx, cy) in EMU: relative sizes against the text box,
    /// `keepaspectratio` for a width+height box, clamped to the column.
    fn image_extent(
        &self,
        info: &images::ImageInfo,
        image: Option<&Image>,
        max_width_emu: Option<i64>,
    ) -> (i64, i64) {
        let nat_cx = (info.width_px * images::EMU_PER_PX) as f64;
        let nat_cy = (info.height_px * images::EMU_PER_PX) as f64;
        let truthy = |x: Option<f64>| x.filter(|v| *v != 0.0);
        let mut w = image.and_then(|i| i.width);
        let mut h = image.and_then(|i| i.height);
        if let Some(rw) = image.and_then(|i| truthy(i.rel_width)) {
            w = Some(rw * self.text_width_emu as f64);
        }
        if let Some(rh) = image.and_then(|i| truthy(i.rel_height)) {
            h = Some(rh * self.text_height_emu as f64);
        }
        let scale = image.and_then(|i| truthy(i.scale));
        let keep = image.is_some_and(|i| i.keep_aspect);
        let (cx, cy) = match (truthy(w), truthy(h)) {
            (Some(w), Some(h)) => {
                if keep && nat_cx != 0.0 && nat_cy != 0.0 {
                    let (a, b) = (w / nat_cx, h / nat_cy);
                    let f = if b < a { b } else { a }; // fit inside the box
                    (nat_cx * f, nat_cy * f)
                } else {
                    (w, h)
                }
            }
            (Some(w), None) => (
                w,
                if nat_cx != 0.0 {
                    nat_cy * w / nat_cx
                } else {
                    nat_cy
                },
            ),
            (None, Some(h)) => (
                if nat_cy != 0.0 {
                    nat_cx * h / nat_cy
                } else {
                    nat_cx
                },
                h,
            ),
            (None, None) => match scale {
                Some(s) => (nat_cx * s, nat_cy * s),
                None => {
                    let limit = max_width_emu.unwrap_or(self.text_width_emu);
                    return images::emu_size(info, Some(limit as f64));
                }
            },
        };
        let limit = max_width_emu.unwrap_or(self.text_width_emu) as f64;
        let (mut cx, mut cy) = (cx, cy);
        if cx > limit {
            cy *= limit / cx;
            cx = limit;
        }
        (cx as i64, cy as i64)
    }

    /// `fields::number_field` plus an explicit restart on the first
    /// section-relative number of each counter after a new Heading 1.
    fn number_runs(&mut self, counter: &str, by_section: bool) -> Vec<Element> {
        let restart = by_section && self.seq_epoch.get(counter) != Some(&self.section_epoch);
        if by_section {
            self.seq_epoch
                .insert(counter.to_string(), self.section_epoch);
        }
        fields::number_field(counter, by_section, restart)
    }

    fn code_block(&mut self, block: &CodeBlock, body: &mut Element) {
        if block.caption.as_ref().is_some_and(|c| !c.is_empty())
            || block.label.as_ref().is_some_and(|l| !l.is_empty())
        {
            // lstlisting caption=/label=: "Listing N: ..." above the code
            let cap = self.caption(
                "Listing",
                block.caption.as_deref().unwrap_or(&[]),
                block.label.as_deref(),
                None,
                block.caption_numbered,
            );
            body.append(cap);
        }
        for line in block.text.split('\n') {
            let mut p = styled_paragraph("SourceCode");
            p.append(run(line, &RunStyle::default()));
            body.append(p);
        }
    }

    fn caption(
        &mut self,
        counter: &str,
        caption: &[Inline],
        label: Option<&str>,
        bookmark: Option<&str>,
        numbered: bool,
    ) -> Element {
        let mut p = styled_paragraph("Caption");
        if !numbered {
            // \caption*: no counter/SEQ, no "Figure N:" prefix
            self.inlines(caption, &mut p);
            return p;
        }
        p.append(run(&format!("{counter} "), &RunStyle::default()));
        let name = match bookmark.filter(|b| !b.is_empty()) {
            Some(b) => Some(b.to_string()),
            None => label.filter(|l| !l.is_empty()).map(bookmark_for),
        };
        let start = name
            .filter(|n| !n.is_empty())
            .map(|n| fields::bookmark_start(&n));
        if let Some(s) = &start {
            p.append(s.clone());
        }
        for r in self.number_runs(counter, self.number_by_section) {
            p.append(r);
        }
        if let Some(s) = &start {
            p.append(fields::bookmark_end_for(s));
        }
        p.append(run(": ", &RunStyle::default()));
        self.inlines(caption, &mut p);
        p
    }

    fn bibliography(&mut self, block: &Bibliography, body: &mut Element) {
        if block.entries.is_empty() {
            return;
        }
        // a tagged block SDT so the round-trip reader recovers it as one block
        let mut sdt = el("w:sdt", &[]);
        let sdtpr = sub(&mut sdt, "w:sdtPr", &[]);
        sub(sdtpr, "w:tag", &[("w:val", BIB_SDT_TAG)]);
        let mut content = el("w:sdtContent", &[]);
        let mut heading = styled_paragraph("Heading1");
        heading.append(run("References", &RunStyle::default()));
        content.append(heading);
        let zot = self.citation_mode == "zotero";
        for (i, item) in block.entries.iter().enumerate() {
            let i = i + 1;
            let mut p = styled_paragraph("Bibliography");
            let start = fields::bookmark_start(&bookmark_for(&format!("bib_{}", item.id)));
            p.append(start.clone());
            if i == 1 && zot {
                for r in zotero::bibliography_field_runs(&[]) {
                    p.append(r);
                }
            }
            if block.style == "numeric" {
                p.append(run(&format!("[{i}]\t"), &RunStyle::default()));
            }
            p.append(run(
                &tex2word_bib::render::format_reference(item),
                &RunStyle::default(),
            ));
            p.append(fields::bookmark_end_for(&start));
            content.append(p);
        }
        sdt.append(content);
        body.append(sdt);
    }

    /// A coloured callout box: a full-width single-cell table with a coloured
    /// border and background fill holding the box's content.
    fn callout(&mut self, block: &Quote, body: &mut Element) {
        let mut tbl = el("w:tbl", &[]);
        {
            let tpr = sub(&mut tbl, "w:tblPr", &[]);
            sub(tpr, "w:tblW", &[("w:w", "5000"), ("w:type", "pct")]);
            // CT_TblPr order: tblW -> tblBorders -> tblCellMar
            let borders = sub(tpr, "w:tblBorders", &[]);
            let color = block
                .border
                .as_deref()
                .filter(|c| !c.is_empty())
                .unwrap_or("auto");
            for side in ["top", "bottom", "left", "right"] {
                sub(
                    borders,
                    &format!("w:{side}"),
                    &[("w:val", "single"), ("w:sz", "8"), ("w:color", color)],
                );
            }
            for side in ["insideH", "insideV"] {
                sub(borders, &format!("w:{side}"), &[("w:val", "nil")]);
            }
            let cellmar = sub(tpr, "w:tblCellMar", &[]);
            for (side, w) in [
                ("top", "60"),
                ("bottom", "60"),
                ("left", "108"),
                ("right", "108"),
            ] {
                sub(
                    cellmar,
                    &format!("w:{side}"),
                    &[("w:w", w), ("w:type", "dxa")],
                );
            }
        }
        let grid = sub(&mut tbl, "w:tblGrid", &[]);
        sub(grid, "w:gridCol", &[]);
        if let Some(title) = block.title.as_ref().filter(|t| !t.is_empty()) {
            // tcolorbox title bar: its own shaded row above the body
            let mut ttc = el("w:tc", &[]);
            if let Some(ts) = block.title_shade.as_deref().filter(|s| !s.is_empty()) {
                let ttcpr = sub(&mut ttc, "w:tcPr", &[]);
                sub(
                    ttcpr,
                    "w:shd",
                    &[("w:val", "clear"), ("w:color", "auto"), ("w:fill", ts)],
                );
            }
            let mut tp = styled_paragraph("Normal");
            self.inlines(title, &mut tp);
            ttc.append(tp);
            let mut tr = el("w:tr", &[]);
            tr.append(ttc);
            tbl.append(tr);
        }
        let mut tc = el("w:tc", &[]);
        if let Some(shade) = block.shade.as_deref().filter(|s| !s.is_empty()) {
            let tcpr = sub(&mut tc, "w:tcPr", &[]);
            sub(
                tcpr,
                "w:shd",
                &[("w:val", "clear"), ("w:color", "auto"), ("w:fill", shade)],
            );
        }
        for inner in &block.blocks {
            self.block(inner, &mut tc, "Normal");
        }
        // a table cell must end with a paragraph (not a table or nothing)
        let wp = qn("w:p");
        if tc.find(&wp).is_none() || tc.is_empty() || !tc.children.last().unwrap().is(&wp) {
            tc.append(styled_paragraph("Normal"));
        }
        let mut tr = el("w:tr", &[]);
        tr.append(tc);
        tbl.append(tr);
        body.append(tbl);
    }

    /// A "ruled" box: a full-width single-cell table with top and bottom
    /// borders, the caption ruled off from the numbered body lines.
    fn algorithm(&mut self, block: &Algorithm, body: &mut Element) {
        let mut tbl = el("w:tbl", &[]);
        {
            let tpr = sub(&mut tbl, "w:tblPr", &[]);
            sub(tpr, "w:tblW", &[("w:w", "5000"), ("w:type", "pct")]);
            let borders = sub(tpr, "w:tblBorders", &[]);
            for side in ["top", "bottom"] {
                sub(
                    borders,
                    &format!("w:{side}"),
                    &[("w:val", "single"), ("w:sz", "12"), ("w:color", "auto")],
                );
            }
            for side in ["left", "right", "insideH", "insideV"] {
                sub(borders, &format!("w:{side}"), &[("w:val", "nil")]);
            }
        }
        let grid = sub(&mut tbl, "w:tblGrid", &[]);
        sub(grid, "w:gridCol", &[]);
        let mut tc = el("w:tc", &[]);
        let label = block.label.as_deref().filter(|l| !l.is_empty());
        if block.caption.is_some() || label.is_some() {
            let mut cap = styled_paragraph("Caption");
            let pbdr = sub(get_ppr(&mut cap), "w:pBdr", &[]);
            sub(
                pbdr,
                "w:bottom",
                &[
                    ("w:val", "single"),
                    ("w:sz", "6"),
                    ("w:space", "2"),
                    ("w:color", "auto"),
                ],
            );
            let bold = RunStyle::default().with_flag("bold");
            cap.append(run("Algorithm ", &bold));
            let start = label.map(|l| fields::bookmark_start(&bookmark_for(l)));
            if let Some(s) = &start {
                cap.append(s.clone());
            }
            for r in self.number_runs("Algorithm", self.number_by_section) {
                cap.append(r);
            }
            if let Some(s) = &start {
                cap.append(fields::bookmark_end_for(s));
            }
            cap.append(run(": ", &bold));
            if let Some(c) = block.caption.as_ref().filter(|c| !c.is_empty()) {
                self.inlines(c, &mut cap);
            }
            tc.append(cap);
        }
        // numbered, indented pseudocode lines
        let mut line_no = 0;
        for aline in &block.lines {
            let mut p = styled_paragraph("SourceCode");
            sub(
                get_ppr(&mut p),
                "w:ind",
                &[("w:left", &(360 + aline.indent * 360).to_string())],
            );
            if aline.number {
                line_no += 1;
                p.append(run(&format!("{line_no:>2}  "), &RunStyle::default()));
            }
            self.inlines(&aline.inlines, &mut p);
            tc.append(p);
        }
        if block.lines.is_empty() {
            tc.append(styled_paragraph("SourceCode"));
        }
        let mut tr = el("w:tr", &[]);
        tr.append(tc);
        tbl.append(tr);
        body.append(tbl);
    }

    fn theorem(&mut self, block: &Theorem, body: &mut Element) {
        let empty = [Block::Paragraph(Paragraph::default())];
        let blocks: &[Block] = if block.blocks.is_empty() {
            &empty
        } else {
            &block.blocks
        };
        let mut p = styled_paragraph("Normal");
        self.write_theorem_prefix(block, &mut p);
        let rest: &[Block] = if let Block::Paragraph(first) = &blocks[0] {
            self.inlines(&first.inlines, &mut p);
            &blocks[1..]
        } else {
            blocks
        };
        body.append(p);
        for inner in rest {
            self.block(inner, body, "Normal");
        }
        if block.kind == "Proof" {
            append_qed(body);
        }
    }

    fn write_theorem_prefix(&mut self, block: &Theorem, p: &mut Element) {
        let is_proof = block.kind == "Proof";
        let emphasis = if is_proof { "italic" } else { "bold" };
        let flag = RunStyle::default().with_flag(emphasis);
        p.append(run(&block.kind, &flag));
        if let Some(counter) = block.counter.as_deref().filter(|c| !c.is_empty()) {
            p.append(run(" ", &flag));
            let start = block
                .label
                .as_deref()
                .filter(|l| !l.is_empty())
                .map(|l| fields::bookmark_start(&bookmark_for(l)));
            if let Some(s) = &start {
                p.append(s.clone());
            }
            // \newtheorem{thm}{Theorem}[section] numbers "section.n" even
            // when the document's figures/equations are numbered flat
            let by_section = self.number_by_section
                || matches!(block.within.as_deref(), Some("section" | "chapter"));
            for r in self.number_runs(counter, by_section) {
                p.append(r);
            }
            if let Some(s) = &start {
                p.append(fields::bookmark_end_for(s));
            }
        }
        if let Some(title) = block.title.as_ref().filter(|t| !t.is_empty()) {
            p.append(run(" (", &RunStyle::default()));
            for t in title {
                self.emit(t, p, &RunStyle::default().with_flag(emphasis));
            }
            p.append(run(")", &RunStyle::default()));
        }
        p.append(run(". ", &flag));
    }

    // -- inline ----------------------------------------------------------- //

    fn inlines(&mut self, inlines: &[Inline], p: &mut Element) {
        for node in inlines {
            self.emit(node, p, &RunStyle::default());
        }
    }

    fn emit(&mut self, node: &Inline, p: &mut Element, st: &RunStyle) {
        // leaf + style-carrying nodes thread the accumulated run style; the
        // rest (math, refs, links, ...) render once
        match node {
            Inline::Text(Text { value }) => p.append(run(value, st)),
            Inline::Emphasis(e) => {
                let st2 = st.with_flag(&e.kind);
                for child in &e.inlines {
                    self.emit(child, p, &st2);
                }
            }
            Inline::Colored(c) => {
                let st2 = RunStyle {
                    color: c
                        .fg
                        .clone()
                        .filter(|s| !s.is_empty())
                        .or_else(|| st.color.clone()),
                    shade: c
                        .bg
                        .clone()
                        .filter(|s| !s.is_empty())
                        .or_else(|| st.shade.clone()),
                    ..st.clone()
                };
                for child in &c.inlines {
                    self.emit(child, p, &st2);
                }
            }
            Inline::FontSize(f) => {
                let st2 = RunStyle {
                    size: Some(f.half_points),
                    ..st.clone()
                };
                for child in &f.inlines {
                    self.emit(child, p, &st2);
                }
            }
            Inline::RawInline(r) => p.append(run(&r.latex, &st.italic())),
            Inline::LineBreak(_) => {
                let mut r = el("w:r", &[]);
                sub(&mut r, "w:br", &[]);
                p.append(r);
            }
            Inline::Image(img) => {
                if let Some(drawing) = self.embed_image(img, None) {
                    let mut r = el("w:r", &[]);
                    r.append(drawing);
                    p.append(r);
                }
            }
            Inline::Math(m) => self.inline_math(m, p, st),
            Inline::Ref(r) => ref_runs(r, p),
            Inline::Cite(c) => self.cite(c, p),
            Inline::Link(l) => self.link(l, p),
            Inline::Footnote(f) => self.footnote(f, p),
            Inline::Endnote(e) => self.endnote(e, p),
            Inline::Comment(c) => self.comment(c, p),
            Inline::IndexEntry(e) => {
                for r in fields::index_entry(&e.term) {
                    p.append(r);
                }
            }
        }
    }

    fn comment(&mut self, node: &Comment, p: &mut Element) {
        self.comment_counter += 1;
        let cid = self.comment_counter.to_string();
        // a point anchor in the body: empty range + a reference mark
        sub(p, "w:commentRangeStart", &[("w:id", &cid)]);
        sub(p, "w:commentRangeEnd", &[("w:id", &cid)]);
        let r = sub(p, "w:r", &[]);
        sub(r, "w:commentReference", &[("w:id", &cid)]);
        // the comment body (lives in comments.xml)
        let mut cp = styled_paragraph("Normal");
        self.inlines(&[Inline::text(node.text.clone())], &mut cp);
        let author = if node.author.is_empty() {
            "tex2word"
        } else {
            node.author.as_str()
        };
        let mut c = el("w:comment", &[]);
        c.append(cp);
        c.set(&qn("w:id"), cid.as_str());
        c.set(&qn("w:author"), author);
        c.set(&qn("w:date"), "2026-01-01T00:00:00Z");
        self.comments.push(c);
    }

    /// `word/comments.xml`, or `None` when there are no comments.
    pub fn comments_xml(&self) -> Result<Option<Vec<u8>>, XmlError> {
        if self.comments.is_empty() {
            return Ok(None);
        }
        let mut root = el("w:comments", &[]);
        for c in &self.comments {
            root.append(c.clone());
        }
        serialize(&root).map(Some)
    }

    fn inline_math(&mut self, node: &Math, p: &mut Element, st: &RunStyle) {
        match self.math.inline(&node.latex, self.report) {
            MathResult::Omml(mut v) if !v.is_empty() => {
                let mut omath = v.swap_remove(0);
                if st.color.as_deref().is_some_and(|c| !c.is_empty())
                    || st.size.is_some_and(|s| s != 0)
                {
                    style_math_runs(&mut omath, st.color.as_deref(), st.size);
                }
                p.append(omath);
            }
            MathResult::Image(data, fmt) => {
                let e = self.embed_image_bytes(&data, &fmt, "math");
                p.append(e);
            }
            _ => p.append(run(&format!("${}$", node.latex), &st.italic())),
        }
    }

    fn cite(&mut self, node: &Cite, p: &mut Element) {
        let rendered = node.rendered.as_deref();
        if let Some(rendered) = rendered.filter(|_| {
            self.citation_mode == "zotero"
                && node.keys.iter().any(|k| self.cite_items.contains_key(k))
        }) {
            for r in zotero::citation_field(node, &self.cite_items, rendered) {
                p.append(r);
            }
            return;
        }
        if let Some(r) = rendered {
            p.append(run(r, &RunStyle::default()));
            return;
        }
        // fallback when no bibliography is present
        let text = node.keys.join(", ");
        let s = if node.mode == "paren" {
            format!("[{text}]")
        } else {
            text
        };
        p.append(run(&s, &RunStyle::default()));
    }

    fn link(&mut self, node: &Link, p: &mut Element) {
        let instr = match node.anchor.as_deref().filter(|a| !a.is_empty()) {
            Some(a) => format!("HYPERLINK \\l \"{a}\""),
            None => format!("HYPERLINK \"{}\"", node.url),
        };
        p.append(fields::fldchar("begin"));
        p.append(fields::instr_run(&instr));
        p.append(fields::fldchar("separate"));
        for child in &node.inlines {
            self.emit(child, p, &RunStyle::default().with_flag("hyperlink"));
        }
        p.append(fields::fldchar("end"));
    }

    fn footnote(&mut self, node: &Footnote, p: &mut Element) {
        self.footnote_counter += 1;
        let note_id = self.footnote_counter.to_string();
        // reference mark in the body
        let mut r = el("w:r", &[]);
        let rpr = sub(&mut r, "w:rPr", &[]);
        sub(rpr, "w:rStyle", &[("w:val", "FootnoteReference")]);
        sub(&mut r, "w:footnoteReference", &[("w:id", &note_id)]);
        p.append(r);
        // the footnote content (lives in footnotes.xml)
        let mut mark = el("w:r", &[]);
        let mrpr = sub(&mut mark, "w:rPr", &[]);
        sub(mrpr, "w:rStyle", &[("w:val", "FootnoteReference")]);
        sub(&mut mark, "w:footnoteRef", &[]);
        if !node.blocks.is_empty() {
            let mut footnote = el("w:footnote", &[("w:id", &note_id)]);
            for block in &node.blocks {
                self.block(block, &mut footnote, "FootnoteText");
            }
            let wp = qn("w:p");
            let first = footnote.children.iter().position(|c| c.is(&wp));
            if first != Some(0) {
                footnote.insert(0, styled_paragraph("FootnoteText"));
            }
            let first = &mut footnote.children[0];
            let wppr = qn("w:pPr");
            let at = first
                .children
                .iter()
                .position(|c| c.is(&wppr))
                .map_or(0, |i| i + 1);
            first.insert(at, mark);
            first.insert(at + 1, run(" ", &RunStyle::default()));
            self.footnotes.push(footnote);
            return;
        }
        let mut fp = styled_paragraph("FootnoteText");
        fp.append(mark);
        fp.append(run(" ", &RunStyle::default()));
        self.inlines(&node.inlines, &mut fp);
        let mut footnote = el("w:footnote", &[]);
        footnote.append(fp);
        footnote.set(&qn("w:id"), note_id.as_str());
        self.footnotes.push(footnote);
    }

    /// `word/footnotes.xml`, or `None` when there are no notes.
    pub fn footnotes_xml(&self) -> Result<Option<Vec<u8>>, XmlError> {
        if self.footnotes.is_empty() {
            return Ok(None);
        }
        let mut root = el("w:footnotes", &[]);
        root.append(special_note("w:footnote", "separator", -1, "w:separator"));
        root.append(special_note(
            "w:footnote",
            "continuationSeparator",
            0,
            "w:continuationSeparator",
        ));
        for f in &self.footnotes {
            root.append(f.clone());
        }
        serialize(&root).map(Some)
    }

    fn endnote(&mut self, node: &Endnote, p: &mut Element) {
        self.endnote_counter += 1;
        let note_id = self.endnote_counter.to_string();
        let mut r = el("w:r", &[]);
        let rpr = sub(&mut r, "w:rPr", &[]);
        sub(rpr, "w:rStyle", &[("w:val", "FootnoteReference")]);
        sub(&mut r, "w:endnoteReference", &[("w:id", &note_id)]);
        p.append(r);
        let mut ep = styled_paragraph("FootnoteText");
        let mut mark = el("w:r", &[]);
        let mrpr = sub(&mut mark, "w:rPr", &[]);
        sub(mrpr, "w:rStyle", &[("w:val", "FootnoteReference")]);
        sub(&mut mark, "w:endnoteRef", &[]);
        ep.append(mark);
        ep.append(run(" ", &RunStyle::default()));
        self.inlines(&node.inlines, &mut ep);
        let mut e = el("w:endnote", &[]);
        e.append(ep);
        e.set(&qn("w:id"), note_id.as_str());
        self.endnotes.push(e);
    }

    /// `word/endnotes.xml`, or `None` when there are no endnotes.
    pub fn endnotes_xml(&self) -> Result<Option<Vec<u8>>, XmlError> {
        if self.endnotes.is_empty() {
            return Ok(None);
        }
        let mut root = el("w:endnotes", &[]);
        root.append(special_note("w:endnote", "separator", -1, "w:separator"));
        root.append(special_note(
            "w:endnote",
            "continuationSeparator",
            0,
            "w:continuationSeparator",
        ));
        for e in &self.endnotes {
            root.append(e.clone());
        }
        serialize(&root).map(Some)
    }

    fn sect_pr(&self, columns: Option<i64>, continuous: bool) -> Element {
        let cols = match columns {
            None => self.columns,
            Some(c) => c.max(1),
        };
        let mut sect = el("w:sectPr", &[]);
        // CT_SectPr child order: hdr/ftr refs, type, pgSz, pgMar, cols, …, titlePg
        for (tag, w_type, rid) in &self.header_footer_refs {
            sub(
                &mut sect,
                &format!("w:{tag}"),
                &[("w:type", w_type), ("r:id", rid)],
            );
        }
        if continuous {
            sub(&mut sect, "w:type", &[("w:val", "continuous")]);
        }
        let default_sz = vec![
            ("w:w".to_string(), "12240".to_string()),
            ("w:h".to_string(), "15840".to_string()),
        ];
        let default_mar: Vec<(String, String)> = [
            ("w:top", "1440"),
            ("w:right", "1440"),
            ("w:bottom", "1440"),
            ("w:left", "1440"),
            ("w:header", "720"),
            ("w:footer", "720"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let pgsz = self.page_pgsz.clone().unwrap_or(default_sz);
        let pgmar = self.page_pgmar.clone().unwrap_or(default_mar);
        sub(&mut sect, "w:pgSz", &attrs(&pgsz));
        sub(&mut sect, "w:pgMar", &attrs(&pgmar));
        // only a multi-column section needs w:cols
        if cols > 1 {
            sub(
                &mut sect,
                "w:cols",
                &[("w:num", &cols.to_string()), ("w:space", "720")],
            );
        }
        if self.header_footer_refs.iter().any(|(_, t, _)| t == "first") {
            sub(&mut sect, "w:titlePg", &[]);
        }
        sect
    }
}

// -- run / paragraph primitives ------------------------------------------- //

fn run(text: &str, st: &RunStyle) -> Element {
    let mut r = el("w:r", &[]);
    let color = st.color.as_deref().filter(|c| !c.is_empty());
    let shade = st.shade.as_deref().filter(|c| !c.is_empty());
    let size = st.size.filter(|s| *s != 0);
    if st.bold
        || st.italic
        || st.underline
        || st.typewriter
        || st.smallcaps
        || st.hyperlink
        || st.superscript
        || st.subscript
        || color.is_some()
        || shade.is_some()
        || st.strike
        || st.highlight
        || size.is_some()
    {
        let rpr = sub(&mut r, "w:rPr", &[]);
        // ECMA-376 CT_RPr order: rStyle, rFonts, b, i, smallCaps, strike,
        // color, sz, szCs, highlight, u, shd, vertAlign
        if st.hyperlink {
            sub(rpr, "w:rStyle", &[("w:val", "Hyperlink")]);
        }
        if st.typewriter {
            sub(
                rpr,
                "w:rFonts",
                &[("w:ascii", "Consolas"), ("w:hAnsi", "Consolas")],
            );
        }
        if st.bold {
            sub(rpr, "w:b", &[]);
        }
        if st.italic {
            sub(rpr, "w:i", &[]);
        }
        if st.smallcaps {
            sub(rpr, "w:smallCaps", &[]);
        }
        if st.strike {
            sub(rpr, "w:strike", &[]);
        }
        if let Some(c) = color {
            sub(rpr, "w:color", &[("w:val", c)]);
        }
        if let Some(s) = size {
            sub(rpr, "w:sz", &[("w:val", &s.to_string())]);
            sub(rpr, "w:szCs", &[("w:val", &s.to_string())]);
        }
        if st.highlight {
            sub(rpr, "w:highlight", &[("w:val", "yellow")]);
        }
        if st.underline {
            sub(rpr, "w:u", &[("w:val", "single")]);
        }
        if let Some(s) = shade {
            sub(
                rpr,
                "w:shd",
                &[("w:val", "clear"), ("w:color", "auto"), ("w:fill", s)],
            );
        }
        if st.superscript {
            sub(rpr, "w:vertAlign", &[("w:val", "superscript")]);
        }
        if st.subscript {
            sub(rpr, "w:vertAlign", &[("w:val", "subscript")]);
        }
    }
    let mut t = text_el("w:t", text, &[]);
    preserve_space(&mut t);
    r.append(t);
    r
}

fn tab_run() -> Element {
    let mut r = el("w:r", &[]);
    sub(&mut r, "w:tab", &[]);
    r
}

fn styled_paragraph(style: &str) -> Element {
    let mut p = el("w:p", &[]);
    let ppr = sub(&mut p, "w:pPr", &[]);
    sub(ppr, "w:pStyle", &[("w:val", style)]);
    p
}

fn set_align(p: &mut Element, align: &str) {
    if align == "left" {
        return;
    }
    let jc = match align {
        "center" => "center",
        "right" => "end",
        _ => "start",
    };
    sub(get_ppr(p), "w:jc", &[("w:val", jc)]);
}

/// Wrap a numbered list-item paragraph in its label bookmark, if any.
fn bookmark_list_item(item: &ListItem, p: &mut Element) {
    let Some(label) = item.label.as_deref().filter(|l| !l.is_empty()) else {
        return;
    };
    let start = fields::bookmark_start(&bookmark_for(label));
    p.append(start.clone());
    p.append(fields::bookmark_end_for(&start));
}

fn merge_continue_cell(tr: &mut Element, colspan: i64) {
    let mut tc = el("w:tc", &[]);
    let tcpr = sub(&mut tc, "w:tcPr", &[]);
    if colspan > 1 {
        sub(tcpr, "w:gridSpan", &[("w:val", &colspan.to_string())]);
    }
    sub(tcpr, "w:vMerge", &[]); // continue the merge
    tc.append(styled_paragraph("Normal"));
    tr.append(tc);
}

fn toc(block: &TableOfContents, body: &mut Element) {
    let (title, code) = toc_spec(&block.kind);
    let mut heading = styled_paragraph("Normal");
    heading.append(run(title, &RunStyle::default().with_flag("bold")));
    body.append(heading);
    let mut p = styled_paragraph("Normal");
    for r in fields::field(code, "Right-click and Update Field to build this list.") {
        p.append(r);
    }
    body.append(p);
}

fn append_qed(body: &mut Element) {
    let wp = qn("w:p");
    match body.children.last_mut() {
        Some(last) if last.is(&wp) => last.append(run("\u{2003}□", &RunStyle::default())), // em-space + QED box
        _ => {
            let mut q = styled_paragraph("Normal");
            set_align(&mut q, "right");
            q.append(run("□", &RunStyle::default()));
            body.append(q);
        }
    }
}

fn special_note(tag: &str, kind: &str, note_id: i64, mark_tag: &str) -> Element {
    let mut r = el("w:r", &[]);
    sub(&mut r, mark_tag, &[]);
    let mut p = el("w:p", &[]);
    p.append(r);
    let mut n = el(tag, &[]);
    n.append(p);
    n.set(&qn("w:type"), kind);
    n.set(&qn("w:id"), note_id.to_string());
    n
}

fn ref_runs(node: &Ref, p: &mut Element) {
    let Some(bookmark) = &node.bookmark else {
        p.append(run("??", &RunStyle::default()));
        return;
    };
    // cleveref-style type prefix ("Figure ", "fig. ", ...)
    if let Some(prefix) = ref_prefix(&node.ref_kind, &node.style) {
        p.append(run(prefix, &RunStyle::default()));
    }
    let runs = match node.ref_kind.as_str() {
        "page" => fields::pageref_field(bookmark, "0"),
        "equation" => {
            p.append(run("(", &RunStyle::default()));
            for r in fields::ref_field(bookmark, "0", false) {
                p.append(r);
            }
            p.append(run(")", &RunStyle::default()));
            return;
        }
        // \r inserts the paragraph's (list) number
        "section" | "listitem" => fields::ref_field(bookmark, "0", true),
        _ => fields::ref_field(bookmark, "0", false),
    };
    for r in runs {
        p.append(r);
    }
}

fn rotation(image: Option<&Image>) -> Option<i64> {
    let angle = image?.angle.filter(|a| *a != 0.0)?;
    // OOXML rot is 1/60000 deg, clockwise; LaTeX angle is counter-clockwise
    Some(pystr::round_half_even((-angle).rem_euclid(360.0) * 60000.0) as i64)
}

fn src_rect(info: &images::ImageInfo, image: Option<&Image>) -> Option<Vec<(String, String)>> {
    // graphicx only crops the trimmed region when `clip` is given
    let image = image?;
    let trim = image.trim.as_ref().filter(|t| !t.is_empty())?;
    if !image.clip || trim.len() != 4 {
        return None;
    }
    let or1 = |v: i64| if v == 0 { 1.0 } else { v as f64 };
    let nat_cx = or1(info.width_px * images::EMU_PER_PX);
    let nat_cy = or1(info.height_px * images::EMU_PER_PX);
    let (left, bottom, right, top) = (trim[0], trim[1], trim[2], trim[3]); // l b r t
    let pct = |v: f64, n: f64| ((v / n * 100000.0) as i64).to_string();
    Some(vec![
        ("l".into(), pct(left, nat_cx)),
        ("t".into(), pct(top, nat_cy)),
        ("r".into(), pct(right, nat_cx)),
        ("b".into(), pct(bottom, nat_cy)),
    ])
}

#[allow(clippy::too_many_arguments)]
fn drawing(
    rel_id: &str,
    cx: i64,
    cy: i64,
    n: usize,
    name: &str,
    rot: Option<i64>,
    src_rect: Option<Vec<(String, String)>>,
    alt: &str,
) -> Element {
    let mut d = el("w:drawing", &[]);
    let inline = sub(
        &mut d,
        "wp:inline",
        &[
            ("distT", "0"),
            ("distB", "0"),
            ("distL", "0"),
            ("distR", "0"),
        ],
    );
    sub(
        inline,
        "wp:extent",
        &[("cx", &cx.to_string()), ("cy", &cy.to_string())],
    );
    sub(
        inline,
        "wp:docPr",
        &[
            ("id", &n.to_string()),
            ("name", &format!("Picture {n}")),
            ("descr", alt),
        ],
    );
    let frame = sub(inline, "wp:cNvGraphicFramePr", &[]);
    sub(frame, "a:graphicFrameLocks", &[("noChangeAspect", "1")]);
    let graphic = sub(inline, "a:graphic", &[]);
    let gdata = sub(graphic, "a:graphicData", &[("uri", PIC_URI)]);
    let pic = sub(gdata, "pic:pic", &[]);
    {
        let nv = sub(pic, "pic:nvPicPr", &[]);
        sub(
            nv,
            "pic:cNvPr",
            &[("id", "0"), ("name", name), ("descr", alt)],
        );
        sub(nv, "pic:cNvPicPr", &[]);
    }
    {
        let blipfill = sub(pic, "pic:blipFill", &[]);
        sub(blipfill, "a:blip", &[("r:embed", rel_id)]);
        if let Some(sr) = &src_rect {
            sub(blipfill, "a:srcRect", &attrs(sr));
        }
        let stretch = sub(blipfill, "a:stretch", &[]);
        sub(stretch, "a:fillRect", &[]);
    }
    let sppr = sub(pic, "pic:spPr", &[]);
    {
        let rot_s = rot.filter(|r| *r != 0).map(|r| r.to_string());
        let xattrs: Vec<(&str, &str)> = match &rot_s {
            Some(r) => vec![("rot", r.as_str())],
            None => vec![],
        };
        let xfrm = sub(sppr, "a:xfrm", &xattrs);
        sub(xfrm, "a:off", &[("x", "0"), ("y", "0")]);
        sub(
            xfrm,
            "a:ext",
            &[("cx", &cx.to_string()), ("cy", &cy.to_string())],
        );
    }
    let geom = sub(sppr, "a:prstGeom", &[("prst", "rect")]);
    sub(geom, "a:avLst", &[]);
    d
}

/// Apply `w:color`/`w:sz` to every math run (`m:r`) of an OMML tree: the
/// `w:rPr` goes after any `m:rPr`, before the run content.
fn style_math_runs(omath: &mut Element, color: Option<&str>, size: Option<i64>) {
    let (m_r, m_rpr) = (qn("m:r"), qn("m:rPr"));
    omath.for_each_mut(&mut |mr| {
        if !mr.is(&m_r) {
            return;
        }
        let mut wrpr = el("w:rPr", &[]);
        if let Some(c) = color.filter(|c| !c.is_empty()) {
            sub(&mut wrpr, "w:color", &[("w:val", c)]);
        }
        if let Some(s) = size.filter(|s| *s != 0) {
            sub(&mut wrpr, "w:sz", &[("w:val", &s.to_string())]);
            sub(&mut wrpr, "w:szCs", &[("w:val", &s.to_string())]);
        }
        match mr.children.iter().position(|c| c.is(&m_rpr)) {
            Some(i) => mr.insert(i + 1, wrpr),
            None => mr.insert(0, wrpr),
        }
    });
}
