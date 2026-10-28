//! The LaTeX node -> IR builder (port of `_Builder` in `frontend/parser.py`).
//!
//! Python mutates some IR nodes after they were appended: a later `\label`
//! sets the label of the current target (`_labelable`), and `\footnotetext`
//! fills the content of an earlier `\footnotemark`. Rust values can't be
//! aliased that way, so those nodes carry a placeholder (a *slot*) that
//! [`Builder::resolve_slots`] replaces once parsing is done -- before anything
//! downstream reads the document, as in Python.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tex2word_ir::pystr;
use tex2word_ir::{
    AlgLine, Algorithm, Bibliography, Block, CSLItem, Cite, CodeBlock, Colored, Comment,
    ConversionReport, Dict, DocumentMeta, Emphasis, Endnote, Figure, FontSize, Footnote, Heading,
    Image, Index, IndexEntry, Inline, ItemList, LineBreak, Link, ListItem, Math, MathBlock,
    Paragraph, Quote, RawInline, RawPassthrough, Ref, SubFigure, Table, TableCell, TableOfContents,
    TableRow, Theorem, Value,
};
use tex2word_latexwalker::{ContextDb, LatexWalker, Node, NodeKind, ParsingState, Stop};

use super::helpers::*;
use crate::colors::ColorTable;
use crate::siunitx;

static BUILDER_IDS: AtomicUsize = AtomicUsize::new(0);
const LABEL_SLOT: &str = "\u{0}t2w-label:";
const FOOT_SLOT: &str = "\u{0}t2w-foot:";

/// A block-level declaration scope (`\color{c}` / `{\bfseries}` unbraced).
enum Mark {
    Color(Option<String>),
    Emph(&'static str),
}

/// One `\includegraphics` panel of a figure: image, caption, label, numbered.
struct Panel {
    image: Option<Image>,
    caption: Option<Vec<Inline>>,
    label: Option<String>,
    numbered: bool,
}

pub(crate) struct Builder {
    pub report: ConversionReport,
    pub meta: DocumentMeta,
    pub bib_files: Vec<String>,
    pub bib_style: String,
    /// an explicit \bibliographystyle was seen
    pub bibstyle_set: bool,
    pub thebib_items: Dict<CSLItem>,
    pub colors: ColorTable,
    /// glossaries/acronyms: label -> (short, long)
    pub acronyms: Dict<(String, String)>,
    /// plain glossary terms (\newglossaryentry): label -> name
    pub glossary: Dict<String>,
    acr_seen: HashSet<String>,
    /// \nocite{key} / \nocite{*}
    pub nocite_keys: Vec<String>,
    /// book/report class: \chapter is the top level
    pub book_mode: bool,
    /// seen \appendix -> later sections use letters
    pub in_appendix: bool,
    /// \footnotemark slots awaiting their \footnotetext{...} content
    pending_footmarks: VecDeque<usize>,
    /// theorem-like environment name -> display title
    pub theorem_envs: HashMap<String, String>,
    /// \newtheorem*-defined names
    pub unnumbered_theorems: HashSet<String>,
    /// env name -> counter display title (shared counters resolve here)
    pub theorem_counters: HashMap<String, String>,
    /// user \newtcolorbox / \newmdenv callout environments -> Quote blocks
    pub box_envs: HashSet<String>,
    pub tcb_envs: Dict<TcbEnv>,
    pub tcb_styles: HashMap<String, String>,
    /// the walker context (for parsing option fragments such as a title)
    pub ctx: Option<Arc<ContextDb>>,
    /// True while parsing a graphic-less figure body (its \caption is the
    /// figure's, already collected -> suppress it there).
    in_figure_body: bool,
    /// env name -> (backgroundcolor, linecolor) names for coloured callouts
    pub box_styles: HashMap<String, (Option<String>, Option<String>)>,
    /// the current `\label` target (a label slot)
    labelable: Option<usize>,
    id: usize,
    labels: Vec<Option<String>>,
    /// true while scanning inside a TikZ/PGF picture of a figure
    in_picture: bool,
    /// extra `\label`s on an already-labelled target -> the label it kept;
    /// `\ref`s to them are redirected so every alias resolves
    label_aliases: HashMap<String, String>,
    footmarks: Vec<Option<Vec<Inline>>>,
}

impl Builder {
    pub fn new(report: ConversionReport) -> Self {
        Builder {
            report,
            meta: DocumentMeta::default(),
            bib_files: Vec::new(),
            bib_style: "plain".into(),
            bibstyle_set: false,
            thebib_items: Dict::new(),
            colors: ColorTable::new(),
            acronyms: Dict::new(),
            glossary: Dict::new(),
            acr_seen: HashSet::new(),
            nocite_keys: Vec::new(),
            book_mode: false,
            in_appendix: false,
            pending_footmarks: VecDeque::new(),
            theorem_envs: THEOREM_ENVS
                .iter()
                .map(|&(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            unnumbered_theorems: HashSet::new(),
            theorem_counters: HashMap::new(),
            box_envs: HashSet::new(),
            tcb_envs: Dict::new(),
            tcb_styles: HashMap::new(),
            ctx: None,
            in_figure_body: false,
            box_styles: HashMap::new(),
            labelable: None,
            id: BUILDER_IDS.fetch_add(1, Ordering::Relaxed),
            labels: Vec::new(),
            in_picture: false,
            label_aliases: HashMap::new(),
            footmarks: Vec::new(),
        }
    }

    // -- slots ------------------------------------------------------------ //

    /// Make a new label target (Python's `self._labelable = node`); returns
    /// the placeholder to store in the node's `label` field.
    fn label_target(&mut self, initial: Option<String>) -> Option<String> {
        let n = self.labels.len();
        self.labels.push(initial);
        self.labelable = Some(n);
        Some(format!("{LABEL_SLOT}{}:{n}", self.id))
    }

    fn slot_index(&self, s: &str, prefix: &str) -> Option<usize> {
        let rest = s.strip_prefix(prefix)?;
        let (id, n) = rest.split_once(':')?;
        (id.parse::<usize>().ok()? == self.id).then(|| n.parse().ok())?
    }

    fn footmark_slot(&self, f: &Footnote) -> Option<usize> {
        match f.inlines.as_slice() {
            [Inline::RawInline(r)] => self.slot_index(&r.latex, FOOT_SLOT),
            _ => None,
        }
    }

    /// The content a `\footnotemark` slot holds so far.
    fn footmark_content(&self, first: &Inline) -> Option<Vec<Inline>> {
        let Inline::RawInline(r) = first else {
            return None;
        };
        let n = self.slot_index(&r.latex, FOOT_SLOT)?;
        Some(self.footmarks[n].clone().unwrap_or_default())
    }

    fn resolve_label(&self, label: &mut Option<String>) {
        if let Some(n) = label
            .as_deref()
            .and_then(|l| self.slot_index(l, LABEL_SLOT))
        {
            *label = self.labels[n].clone();
        }
    }

    /// Replace this builder's slots in a document's blocks and metadata.
    pub fn resolve_slots(&self, blocks: &mut [Block], meta: &mut DocumentMeta) {
        self.resolve_blocks(blocks);
        let opt = |b: &Self, x: &mut Option<Vec<Inline>>| {
            if let Some(v) = x {
                b.resolve_inlines(v);
            }
        };
        opt(self, &mut meta.title);
        opt(self, &mut meta.date);
        opt(self, &mut meta.keywords);
        for a in meta.authors.iter_mut().chain(meta.affiliations.iter_mut()) {
            self.resolve_inlines(a);
        }
        if let Some(a) = &mut meta.abstract_ {
            self.resolve_blocks(a);
        }
    }

    pub fn resolve_inlines(&self, inlines: &mut [Inline]) {
        for node in inlines.iter_mut() {
            if let Inline::Ref(r) = node {
                // follow alias chains (bounded: a cycle can't hang us)
                for _ in 0..8 {
                    match self.label_aliases.get(&r.key) {
                        Some(k) if *k != r.key => r.key = k.clone(),
                        _ => break,
                    }
                }
            }
            if let Inline::Footnote(f) = node {
                if let Some(n) = self.footmark_slot(f) {
                    f.inlines = self.footmarks[n].clone().unwrap_or_default();
                }
            }
            if let Some(children) = node.inlines_mut() {
                self.resolve_inlines(children);
            }
        }
    }

    fn resolve_opt_inlines(&self, x: &mut Option<Vec<Inline>>) {
        if let Some(v) = x {
            self.resolve_inlines(v);
        }
    }

    pub fn resolve_blocks(&self, blocks: &mut [Block]) {
        for b in blocks.iter_mut() {
            match b {
                Block::Heading(h) => {
                    self.resolve_label(&mut h.label);
                    self.resolve_inlines(&mut h.inlines);
                }
                Block::Paragraph(p) => self.resolve_inlines(&mut p.inlines),
                Block::MathBlock(m) => self.resolve_label(&mut m.label),
                Block::ItemList(l) => {
                    for item in l.items.iter_mut() {
                        self.resolve_label(&mut item.label);
                        self.resolve_opt_inlines(&mut item.term);
                        self.resolve_blocks(&mut item.blocks);
                    }
                }
                Block::Table(t) => {
                    self.resolve_label(&mut t.label);
                    self.resolve_opt_inlines(&mut t.caption);
                    for row in t.rows.iter_mut() {
                        for cell in row.cells.iter_mut() {
                            self.resolve_blocks(&mut cell.blocks);
                        }
                    }
                }
                Block::Figure(f) => {
                    self.resolve_label(&mut f.label);
                    self.resolve_opt_inlines(&mut f.caption);
                    for s in f.subfigures.iter_mut() {
                        self.resolve_opt_inlines(&mut s.caption);
                    }
                    self.resolve_blocks(&mut f.blocks);
                }
                Block::Quote(q) => {
                    self.resolve_opt_inlines(&mut q.title);
                    self.resolve_blocks(&mut q.blocks);
                }
                Block::Theorem(t) => {
                    self.resolve_label(&mut t.label);
                    self.resolve_opt_inlines(&mut t.title);
                    self.resolve_blocks(&mut t.blocks);
                }
                Block::Algorithm(a) => {
                    self.resolve_label(&mut a.label);
                    self.resolve_opt_inlines(&mut a.caption);
                    for line in a.lines.iter_mut() {
                        self.resolve_inlines(&mut line.inlines);
                    }
                }
                _ => {}
            }
        }
    }

    // -- inline ----------------------------------------------------------- //

    pub fn inlines(&mut self, nodes: &[Node]) -> Vec<Inline> {
        let out = self.scoped_inlines(nodes);
        clean_inlines(out, false)
    }

    /// Build inlines, honouring `\color` (and font-size/emphasis
    /// declarations) as a scope switch over the following siblings.
    fn scoped_inlines(&mut self, nodes: &[Node]) -> Vec<Inline> {
        let mut out = Vec::new();
        for (i, node) in nodes.iter().enumerate() {
            if let Some(name) = node.macroname() {
                if name == "color" || name == "normalcolor" {
                    let rest = self.scoped_inlines(&nodes[i + 1..]);
                    let fg = if name == "color" {
                        self.color_of(node)
                    } else {
                        None
                    };
                    match fg.filter(|f| !f.is_empty()) {
                        Some(fg) => out.push(Inline::Colored(Colored {
                            inlines: rest,
                            fg: Some(fg),
                            bg: None,
                        })),
                        // unresolved colour or \normalcolor reset -- untinted
                        None => out.extend(rest),
                    }
                    return out;
                }
                if let Some(hp) = font_size_hp(name) {
                    let rest = self.scoped_inlines(&nodes[i + 1..]);
                    if hp == 20 {
                        out.extend(rest); // \normalsize -- no span needed
                    } else {
                        out.push(Inline::FontSize(FontSize {
                            inlines: rest,
                            half_points: hp,
                        }));
                    }
                    return out;
                }
                if let Some(kind) = emphasis_decl(name) {
                    let rest = self.scoped_inlines(&nodes[i + 1..]); // {\bfseries ...}
                    out.push(Inline::Emphasis(Emphasis {
                        inlines: rest,
                        kind: kind.into(),
                    }));
                    return out;
                }
            }
            self.inline_node(node, &mut out);
        }
        out
    }

    /// Resolve the colour of a `\color`/`\textcolor`-style macro argument.
    fn color_of(&self, node: &Node) -> Option<String> {
        let mut model: Option<String> = None;
        let mut color: Option<String> = None;
        for a in argnlist(node).iter().flatten() {
            if is_bracket_group(a) {
                model = Some(chars_of(a.nodelist().unwrap_or(&[])));
            } else if color.is_none() {
                color = Some(match a.nodelist() {
                    Some(list) if a.is_group() => chars_of(list),
                    _ => chars_of(std::slice::from_ref(a)),
                });
            }
        }
        let color = color.filter(|c| !c.is_empty())?;
        self.colors
            .resolve(&color, model.as_deref().filter(|m| !m.is_empty()))
    }

    fn inline_node(&mut self, node: &Node, out: &mut Vec<Inline>) {
        match &node.kind {
            NodeKind::Chars { chars } => {
                let text = normalize_ws(chars);
                if !text.is_empty() {
                    out.push(Inline::text(text));
                }
            }
            NodeKind::Comment { .. } => {}
            NodeKind::Math { displaytype, .. } => {
                let latex = strip_math_delims(node.latex_verbatim(), "math");
                let marker = if displaytype == "inline" {
                    script_marker(&latex)
                } else {
                    None
                };
                if let Some((kind, text)) = marker {
                    // $^{*1}$ / $^\dagger$ / 1$^{st}$: a base-less script is a
                    // text marker, not math
                    if kind == "plain" {
                        out.push(Inline::text(text));
                    } else {
                        out.push(Inline::Emphasis(Emphasis {
                            inlines: vec![Inline::text(text)],
                            kind: kind.into(),
                        }));
                    }
                    return;
                }
                out.push(Inline::Math(Math { latex }));
            }
            NodeKind::Group { nodelist, .. } => {
                let inner = self.scoped_inlines(nodelist); // contain \color scope
                out.extend(inner);
            }
            NodeKind::Specials { specials_chars, .. } => inline_special(specials_chars, out),
            NodeKind::Macro { .. } => self.inline_macro(node, out),
            NodeKind::Environment { .. } => {}
        }
    }

    fn inline_macro(&mut self, node: &Node, out: &mut Vec<Inline>) {
        let name = node.macroname().unwrap_or("");
        if is_font_reset(name) {
            // \textrm/\textnormal/... -> upright passthrough
            let inner = self.inlines(&group_nodes(node));
            out.extend(inner);
            return;
        }
        if let Some(kind) = emphasis_kind(name) {
            let inner = self.inlines(&group_nodes(node));
            out.push(Inline::Emphasis(Emphasis {
                inlines: inner,
                kind: kind.into(),
            }));
            return;
        }
        if matches!(name, "textcolor" | "colorbox" | "fcolorbox") {
            self.inline_color(node, name, out);
            return;
        }
        if name == "\\" || name == "newline" {
            out.push(Inline::LineBreak(LineBreak {}));
            return;
        }
        if name == "verb" {
            if let Some(text) = node.nodeargd().and_then(|a| a.verbatim_text.clone()) {
                out.push(typewriter(text));
                return;
            }
        }
        if name == "lstinline" || name == "mintinline" {
            // brace form -> typewriter text (\mintinline{lang}{code}: last group)
            let groups = brace_groups(node);
            if let Some(last) = groups.last() {
                out.push(typewriter(chars_of(last)));
            }
            return;
        }
        if name == "crefrange" || name == "Crefrange" {
            // \crefrange{a}{b} -> "secs. 1 to 3"
            let groups = brace_groups(node);
            let kind = ref_kind(name).unwrap();
            let style = ref_style(name);
            if groups.len() >= 2 {
                out.push(make_ref(pystr::strip(&chars_of(groups[0])), kind, style));
                out.push(Inline::text(" to "));
                out.push(make_ref(pystr::strip(&chars_of(groups[1])), kind, "plain"));
            } else if let Some(last) = groups.last() {
                out.push(make_ref(pystr::strip(&chars_of(last)), kind, style));
            }
            return;
        }
        if let Some(kind) = ref_kind(name) {
            let style = ref_style(name);
            let chars = chars_of(&group_nodes(node));
            let keys: Vec<&str> = chars
                .split(',')
                .map(pystr::strip)
                .filter(|k| !k.is_empty())
                .collect();
            for (idx, key) in keys.iter().enumerate() {
                if idx > 0 {
                    // cleveref joins multi-labels with ", " / " and "
                    out.push(Inline::text(if idx == keys.len() - 1 {
                        " and "
                    } else {
                        ", "
                    }));
                }
                // only the first reference carries the cleveref type prefix
                out.push(make_ref(key, kind, if idx == 0 { style } else { "plain" }));
            }
            if keys.is_empty() {
                out.push(make_ref("", kind, style));
            }
            return;
        }
        if let Some(mode) = cite_mode(name) {
            let key = chars_of(&group_nodes(node));
            let keys: Vec<String> = key
                .split(',')
                .map(pystr::strip)
                .filter(|k| !k.is_empty())
                .map(str::to_string)
                .collect();
            let (prefix, suffix) = cite_locators(node);
            out.push(Inline::Cite(Cite {
                keys,
                mode: mode.into(),
                prefix,
                suffix,
                rendered: None,
            }));
            return;
        }
        if name == "label" {
            let label = chars_of(&group_nodes(node));
            if let Some(n) = self.labelable {
                if !label.is_empty() {
                    if let Some(old) = self.labels[n].take() {
                        if old != label {
                            self.label_aliases.insert(old, label.clone());
                        }
                    }
                    self.labels[n] = Some(label);
                }
            }
            return;
        }
        if self.in_figure_body && (name == "caption" || name == "captionof") {
            return; // the enclosing figure's caption, collected separately
        }
        if name == "href" || name == "url" {
            self.inline_link(node, out);
            return;
        }
        if name == "path" || name == "nolinkurl" {
            // url-package path: monospace text, no hyperlink
            let text = chars_of(&group_nodes(node));
            if !text.is_empty() {
                out.push(Inline::Emphasis(Emphasis {
                    inlines: vec![Inline::text(text)],
                    kind: "typewriter".into(),
                }));
            }
            return;
        }
        if name == "hyperref" {
            // \hyperref[label]{text} -> internal link to label
            let opt = optional_group(node);
            let groups = brace_groups(node);
            let text = match groups.last() {
                Some(g) => self.inlines(g),
                None => Vec::new(),
            };
            match opt {
                Some(opt) => {
                    let anchor = pystr::strip(&chars_of(opt)).to_string();
                    let inlines = if text.is_empty() {
                        vec![Inline::text(anchor.clone())]
                    } else {
                        text
                    };
                    out.push(Inline::Link(Link {
                        inlines,
                        url: String::new(),
                        anchor: Some(anchor),
                    }));
                }
                None => out.extend(text), // \hyperref{url}{cat}{name}{text}
            }
            return;
        }
        if matches!(name, "footnote" | "thanks" | "marginpar" | "sidenote") {
            // all degrade to a Word footnote -- the closest aside Word offers
            let inner = self.inlines(&group_nodes(node));
            out.push(Inline::Footnote(Footnote { inlines: inner }));
            return;
        }
        if name == "footnotemark" {
            // split footnote: a mark here, its text supplied by \footnotetext
            let n = self.footmarks.len();
            self.footmarks.push(None);
            self.pending_footmarks.push_back(n);
            out.push(Inline::Footnote(Footnote {
                inlines: vec![Inline::RawInline(RawInline {
                    latex: format!("{FOOT_SLOT}{}:{n}", self.id),
                    reason: String::new(),
                })],
            }));
            return;
        }
        if name == "footnotetext" {
            // fill the oldest pending \footnotemark; if none, emit inline
            let content = self.inlines(&group_nodes(node));
            match self.pending_footmarks.pop_front() {
                Some(n) => self.footmarks[n] = Some(content),
                None => out.push(Inline::Footnote(Footnote { inlines: content })),
            }
            return;
        }
        if matches!(
            name,
            "enquote" | "textquote" | "foreignquote" | "hyphenquote"
        ) {
            // csquotes inline quotes -> curly quotes around the quoted group
            let inner = self.inlines(&group_nodes(node));
            let (lq, rq) = if has_star(node) {
                ("‘", "’")
            } else {
                ("“", "”")
            };
            out.push(Inline::text(lq));
            out.extend(inner);
            out.push(Inline::text(rq));
            return;
        }
        if name == "endnote" {
            let inner = self.inlines(&group_nodes(node));
            out.push(Inline::Endnote(Endnote { inlines: inner }));
            return;
        }
        if name == "theendnotes" {
            return; // endnotes render natively at the doc end
        }
        if name == "index" {
            // -> a hidden Word XE index-entry field
            let term = pystr::strip(&chars_of(&group_nodes(node))).replace('"', "'");
            if !term.is_empty() {
                out.push(Inline::IndexEntry(IndexEntry { term }));
            }
            return;
        }
        if matches!(name, "todo" | "comment" | "note") {
            // review annotations -> Word comment
            let text = pystr::strip(&chars_of(&group_nodes(node))).to_string();
            if !text.is_empty() {
                out.push(Inline::Comment(Comment {
                    text,
                    author: String::new(),
                }));
            }
            return;
        }
        if name == "nicefrac" || name == "sfrac" {
            let groups = brace_groups(node);
            if groups.len() >= 2 {
                out.push(Inline::text(chars_of(groups[0])));
                out.push(Inline::text("/"));
                out.push(Inline::text(chars_of(groups[1])));
            }
            return;
        }
        if name == "si" || name == "unit" {
            out.push(Inline::text(siunitx::units_to_text(&group_nodes(node))));
            return;
        }
        if name == "SI" || name == "qty" {
            let groups = brace_groups(node);
            let num = groups
                .first()
                .map_or(String::new(), |g| siunitx::num_to_text(&chars_of(g)));
            let unit = groups
                .get(1)
                .map_or(String::new(), |g| siunitx::units_to_text(g));
            let text = format!("{num}{}{unit}", siunitx::THIN);
            out.push(Inline::text(pystr::strip(&text)));
            return;
        }
        if name == "num" {
            out.push(Inline::text(siunitx::num_to_text(&chars_of(&group_nodes(
                node,
            )))));
            return;
        }
        if name == "ang" {
            out.push(Inline::text(siunitx::ang_to_text(&chars_of(&group_nodes(
                node,
            )))));
            return;
        }
        if matches!(name, "numrange" | "SIrange" | "qtyrange") {
            // a to b [unit]
            let groups = brace_groups(node);
            if groups.len() >= 2 {
                let a = siunitx::num_to_text(&chars_of(groups[0]));
                let b = siunitx::num_to_text(&chars_of(groups[1]));
                let unit = groups
                    .get(2)
                    .map_or(String::new(), |g| siunitx::units_to_text(g));
                let tail = if unit.is_empty() {
                    String::new()
                } else {
                    format!("{}{unit}", siunitx::THIN)
                };
                out.push(Inline::text(format!("{a} to {b}{tail}")));
            }
            return;
        }
        if matches!(name, "numlist" | "SIlist" | "qtylist") {
            // a, b and c [unit]
            let groups = brace_groups(node);
            if let Some(first) = groups.first() {
                let items: Vec<String> = chars_of(first)
                    .split(';')
                    .filter(|x| !pystr::strip(x).is_empty())
                    .map(|x| siunitx::num_to_text(pystr::strip(x)))
                    .collect();
                let unit = groups
                    .get(1)
                    .map_or(String::new(), |g| siunitx::units_to_text(g));
                let tail = if unit.is_empty() {
                    String::new()
                } else {
                    format!("{}{unit}", siunitx::THIN)
                };
                out.push(Inline::text(format!("{}{tail}", join_and(&items))));
            }
            return;
        }
        if name == "includegraphics" {
            out.push(Inline::Image(make_image(node))); // an inline image (icon/logo)
            return;
        }
        if is_drop_macro(name) {
            return; // phantom/rule: reserve space, print nothing
        }
        if name == "IEEEPARstart" {
            // \IEEEPARstart{T}{he} -> "The" (IEEE drop cap)
            for g in brace_groups(node) {
                let inner = self.inlines(g);
                out.extend(inner);
            }
            return;
        }
        if name == "ensuremath" {
            out.push(Inline::Math(Math {
                latex: latex_of(&group_nodes(node)),
            }));
            return;
        }
        if name == "texorpdfstring" {
            // \texorpdfstring{TeX}{PDF} -> the TeX form
            if let Some(first) = brace_groups(node).first() {
                let inner = self.inlines(first);
                out.extend(inner);
            }
            return;
        }
        if is_transparent_box(name) || is_author_block(name) {
            if let Some(last) = brace_groups(node).last() {
                let inner = self.inlines(last);
                out.extend(inner);
            }
            return;
        }
        if name == "inst" || name == "IEEEauthorrefmark" {
            // affiliation marker -> superscript
            let inner = self.inlines(&group_nodes(node));
            out.push(Inline::Emphasis(Emphasis {
                inlines: inner,
                kind: "superscript".into(),
            }));
            return;
        }
        if matches!(name, "newacronym" | "newglossaryentry" | "acro" | "acrodef") {
            return; // definitions are collected separately
        }
        let lower = pystr::lower(name);
        if let Some(tname) = ac_to_gls(&lower) {
            // acronym package: \ac/\acs/\acl/\acf/...
            let label = pystr::strip(&chars_of(&group_nodes(node))).to_string();
            let tname = if starts_upper(name) {
                upper_first(tname)
            } else {
                tname.to_string()
            };
            let text = self.acronym_text(&tname, &label);
            out.push(Inline::text(text));
            return;
        }
        if is_gls_macro(&lower) {
            let label = pystr::strip(&chars_of(&group_nodes(node))).to_string();
            let text = self.acronym_text(name, &label);
            out.push(Inline::text(text));
            return;
        }
        if name == "ding" {
            out.push(Inline::text(ding_char(&chars_of(&group_nodes(node)))));
            return;
        }
        if matches!(name, "shortstack" | "stackanchor" | "Shortstack") {
            // stacked lines -> the content with \\ kept as line breaks
            let inner = self.inlines(&group_nodes(node));
            out.extend(inner);
            return;
        }
        if name == "today" {
            out.push(Inline::text(today_text()));
            return;
        }
        if let Some(sym) = TEXT_SYMBOLS.get(name) {
            out.push(Inline::text(*sym));
            return;
        }
        if is_ignore_macro(name) {
            return;
        }
        if accent_char(name).is_some() {
            let base = chars_of(&group_nodes(node));
            out.push(Inline::text(apply_text_accent(name, &base)));
            return;
        }
        // graceful degradation
        self.report.warn(
            &format!("\\{name}"),
            format!("unsupported inline macro \\{name}"),
        );
        out.push(Inline::RawInline(RawInline {
            latex: node.latex_verbatim().to_string(),
            reason: format!("unsupported macro \\{name}"),
        }));
    }

    /// Expand a glossaries/acronym reference (\gls/\acrshort/…) to text.
    fn acronym_text(&mut self, name: &str, label: &str) -> String {
        let Some((short, long)) = self.acronyms.get(label).cloned() else {
            // a plain \newglossaryentry term: \gls/\glspl print its name
            if let Some(term) = self.glossary.get(label) {
                let low = pystr::lower(name);
                let mut text = format!("{term}{}", if low.ends_with("pl") { "s" } else { "" });
                if starts_upper(name) && !text.is_empty() {
                    text = upper_first(&text);
                }
                return text;
            }
            return label.to_string(); // undefined acronym -> emit the key
        };
        let low = pystr::lower(name);
        let plural = low.ends_with("pl");
        let key = if plural {
            &low[..low.len() - 2]
        } else {
            low.as_str()
        };
        let s = format!("{short}{}", if plural { "s" } else { "" });
        let lg = format!("{long}{}", if plural { "s" } else { "" });
        let full = format!("{lg} ({s})");
        let mut text = match key {
            "gls" => {
                let t = if self.acr_seen.contains(label) {
                    s
                } else {
                    full
                };
                self.acr_seen.insert(label.to_string());
                t
            }
            "acrshort" | "glsentryshort" => s,
            "acrlong" | "glsentrylong" => lg,
            "acrfull" => full,
            _ => s,
        };
        if starts_upper(name) && !text.is_empty() {
            // \Gls/\Acrlong/... capitalise
            text = upper_first(&text);
        }
        text
    }

    /// \printglossaries/\printacronyms -> a description list of the entries.
    fn glossary_list(&self) -> Option<Block> {
        let mut items = Vec::new();
        let item = |term: &str, body: &str| ListItem {
            blocks: vec![Block::Paragraph(Paragraph {
                inlines: vec![Inline::text(body)],
                align: None,
            })],
            term: Some(vec![Inline::text(term)]),
            label: None,
        };
        for (short, long) in self.acronyms.values() {
            items.push(item(short, long));
        }
        for (key, name) in self.glossary.iter() {
            items.push(item(key, name));
        }
        if items.is_empty() {
            return None;
        }
        Some(Block::ItemList(ItemList {
            ordered: false,
            items,
            description: true,
        }))
    }

    fn inline_color(&mut self, node: &Node, name: &str, out: &mut Vec<Inline>) {
        let mut model: Option<String> = None;
        let mut groups: Vec<&[Node]> = Vec::new();
        for a in argnlist(node).iter().flatten() {
            if is_bracket_group(a) {
                model = Some(chars_of(a.nodelist().unwrap_or(&[])));
            } else if let Some((list, _)) = group_parts(a) {
                groups.push(list);
            }
        }
        let Some(last) = groups.last() else {
            return;
        };
        let inner = self.inlines(last); // last mandatory group is the content
                                        // \textcolor{C}{t} / \colorbox{C}{t} -> groups[0]; \fcolorbox{frame}{C}{t}
                                        // -> groups[-2] (the background colour)
        let color_group = if groups.len() >= 2 {
            Some(groups[groups.len() - 2])
        } else {
            None
        };
        let hexval = match color_group {
            Some(g) if !g.is_empty() => self
                .colors
                .resolve(&chars_of(g), model.as_deref().filter(|m| !m.is_empty())),
            _ => None,
        };
        match hexval {
            None => out.extend(inner),
            Some(h) if name == "textcolor" => out.push(Inline::Colored(Colored {
                inlines: inner,
                fg: Some(h),
                bg: None,
            })),
            Some(h) => out.push(Inline::Colored(Colored {
                inlines: inner,
                fg: None,
                bg: Some(h),
            })),
        }
    }

    fn inline_link(&mut self, node: &Node, out: &mut Vec<Inline>) {
        let groups: Vec<&[Node]> = argnlist(node)
            .iter()
            .flatten()
            .filter_map(group_parts)
            .map(|(n, _)| n)
            .collect();
        if node.macroname() == Some("url") && !groups.is_empty() {
            let url = chars_of(groups[0]);
            out.push(Inline::Link(Link {
                inlines: vec![Inline::text(url.clone())],
                url,
                anchor: None,
            }));
        } else if groups.len() >= 2 {
            let url = chars_of(groups[0]);
            let inner = self.inlines(groups[1]);
            out.push(Inline::Link(Link {
                inlines: inner,
                url,
                anchor: None,
            }));
        } else if let Some(g) = groups.first() {
            let url = chars_of(g);
            out.push(Inline::Link(Link {
                inlines: vec![Inline::text(url.clone())],
                url,
                anchor: None,
            }));
        }
    }

    // -- blocks ----------------------------------------------------------- //

    pub fn blocks(&mut self, nodes: &[Node]) -> Vec<Block> {
        let mut out: Vec<Block> = Vec::new();
        let mut inline_buf: Vec<Inline> = Vec::new();
        // block-level declaration scopes: (index into inline_buf, scope)
        let mut scope_marks: Vec<(usize, Mark)> = Vec::new();

        for node in nodes {
            let macroname = node.macroname();
            match &node.kind {
                NodeKind::Chars { chars } => {
                    // split on blank lines (paragraph breaks)
                    for (idx, part) in chars.split("\n\n").enumerate() {
                        if idx > 0 {
                            flush(&mut inline_buf, &mut scope_marks, &mut out);
                        }
                        let text = normalize_ws(part);
                        if !text.is_empty() {
                            inline_buf.push(Inline::text(text));
                        }
                    }
                    continue;
                }
                NodeKind::Comment { .. } => continue,
                NodeKind::Math { .. } if is_display(node) => {
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    let b = self.math_block(node.latex_verbatim(), "displaymath", false);
                    out.push(b);
                    continue;
                }
                NodeKind::Environment { .. } => {
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    self.environment(node, &mut out);
                    continue;
                }
                // a bare {...} group wrapping a block environment -> descend
                NodeKind::Group { nodelist, .. }
                    if nodelist
                        .iter()
                        .any(|n| n.is_environment() || is_block_macro_node(n)) =>
                {
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    let inner = self.blocks(nodelist);
                    out.extend(inner);
                    continue;
                }
                _ => {}
            }
            if let Some(name) = macroname {
                if name == "label" {
                    let label = chars_of(&group_nodes(node));
                    // don't overwrite a label the target already captured
                    if let Some(n) = self.labelable {
                        if !label.is_empty() {
                            match &self.labels[n] {
                                None => self.labels[n] = Some(label),
                                // a second \label on the same target (e.g. a
                                // heading with several keys) becomes an alias
                                Some(kept) if *kept != label => {
                                    self.label_aliases.insert(label, kept.clone());
                                }
                                Some(_) => {}
                            }
                        }
                    }
                    continue;
                }
                if name == "appendix" || name == "beginappendix" {
                    self.in_appendix = true;
                    continue;
                }
                if section_level(name.trim_end_matches('*'), false).is_some() {
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    self.heading(node, &mut out);
                    continue;
                }
                if let Some(kind) = toc_kind(name) {
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    out.push(Block::TableOfContents(TableOfContents {
                        kind: kind.into(),
                    }));
                    continue;
                }
                if matches!(
                    name,
                    "title"
                        | "author"
                        | "date"
                        | "keywords"
                        | "IEEEkeywords"
                        | "institute"
                        | "affiliation"
                        | "affil"
                        | "address"
                        | "email"
                        | "orcid"
                        | "markboth"
                        | "markright"
                        | "runninghead"
                        | "shorttitle"
                ) {
                    self.meta_macro(node);
                    continue;
                }
                if name == "resizebox" || name == "scalebox" {
                    // box-scaling wrappers are transparent
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    if let Some(last) = brace_groups(node).last() {
                        let inner = self.blocks(last);
                        out.extend(inner);
                    }
                    continue;
                }
                if name == "includegraphics" {
                    // mid-paragraph -> inline image; otherwise a bare Figure
                    if has_visible(&inline_buf) {
                        self.inline_node(node, &mut inline_buf);
                    } else {
                        flush(&mut inline_buf, &mut scope_marks, &mut out);
                        out.push(Block::Figure(Figure {
                            image: Some(make_image(node)),
                            source: node.latex_verbatim().to_string(),
                            ..Figure::default()
                        }));
                    }
                    continue;
                }
                if name == "bibliographystyle" {
                    let style = chars_of(&group_nodes(node));
                    if !style.is_empty() {
                        self.bib_style = style;
                    }
                    self.bibstyle_set = true;
                    continue;
                }
                if name == "nocite" {
                    for key in chars_of(&group_nodes(node)).split(',') {
                        if !pystr::strip(key).is_empty() {
                            self.nocite_keys.push(pystr::strip(key).to_string());
                        }
                    }
                    continue;
                }
                if name == "bibliography" {
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    for n in chars_of(&group_nodes(node)).split(',') {
                        if !pystr::strip(n).is_empty() {
                            self.bib_files.push(pystr::strip(n).to_string());
                        }
                    }
                    out.push(numeric_bibliography());
                    continue;
                }
                if name == "addbibresource" {
                    for n in chars_of(&group_nodes(node)).split(',') {
                        if !pystr::strip(n).is_empty() {
                            self.bib_files.push(pystr::strip(n).to_string());
                        }
                    }
                    continue;
                }
                if name == "printbibliography" {
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    out.push(numeric_bibliography());
                    continue;
                }
                if name == "printindex" {
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    out.push(Block::Index(Index {}));
                    continue;
                }
                if PRINTGLOSSARY_MACROS.contains(&name) {
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    if let Some(g) = self.glossary_list() {
                        out.push(g);
                    }
                    continue;
                }
                if matches!(name, "blockquote" | "blockcquote" | "foreignblockquote") {
                    // csquotes block quote -> a Quote block (last {} group)
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    let body = match brace_groups(node).last() {
                        Some(g) => self.blocks(g),
                        None => Vec::new(),
                    };
                    let body = if body.is_empty() {
                        vec![Block::Paragraph(Paragraph::default())]
                    } else {
                        body
                    };
                    out.push(Block::Quote(Quote {
                        blocks: body,
                        ..Quote::default()
                    }));
                    continue;
                }
                if name == "epigraph" {
                    // \epigraph{quote}{source} -> a Quote with a right-aligned
                    // italic attribution line underneath
                    flush(&mut inline_buf, &mut scope_marks, &mut out);
                    let groups = brace_groups(node);
                    let quote = match groups.first() {
                        Some(g) => self.inlines(g),
                        None => Vec::new(),
                    };
                    let mut blocks = vec![Block::Paragraph(Paragraph {
                        inlines: quote,
                        align: None,
                    })];
                    if groups.len() > 1 {
                        let src = self.inlines(groups[1]);
                        let src = clean_inlines(src, true);
                        if !src.is_empty() {
                            blocks.push(Block::Paragraph(Paragraph {
                                inlines: vec![Inline::Emphasis(Emphasis {
                                    inlines: src,
                                    kind: "italic".into(),
                                })],
                                align: Some("right".into()),
                            }));
                        }
                    }
                    out.push(Block::Quote(Quote {
                        blocks,
                        ..Quote::default()
                    }));
                    continue;
                }
                // block-level declaration scopes used unbraced
                if name == "color" || name == "normalcolor" {
                    let fg = if name == "color" {
                        self.color_of(node)
                    } else {
                        None
                    };
                    scope_marks.push((inline_buf.len(), Mark::Color(fg)));
                    continue;
                }
                if let Some(kind) = emphasis_decl(name) {
                    scope_marks.push((inline_buf.len(), Mark::Emph(kind)));
                    continue;
                }
            }
            // otherwise inline content
            self.inline_node(node, &mut inline_buf);
        }
        flush(&mut inline_buf, &mut scope_marks, &mut out);
        out
    }

    fn meta_macro(&mut self, node: &Node) {
        let name = node.macroname().unwrap_or("");
        if name == "author" {
            // one \author may list several authors separated by \and
            for seg in split_on_and(&group_nodes(node)) {
                let inner = self.inlines(&seg);
                let inner = clean_inlines(inner, true);
                if !inner.is_empty() {
                    self.meta.authors.push(inner);
                }
            }
            return;
        }
        if matches!(name, "institute" | "affiliation" | "affil" | "address") {
            for seg in split_on_and(&group_nodes(node)) {
                let inner = self.inlines(&seg);
                let inner = clean_inlines(inner, true);
                if !inner.is_empty() {
                    self.meta.affiliations.push(inner);
                }
            }
            return;
        }
        if matches!(
            name,
            "markboth" | "markright" | "runninghead" | "shorttitle"
        ) {
            // \markboth{even}{odd} -> the odd (recto) head
            let groups = brace_groups(node);
            let head_nodes: std::borrow::Cow<[Node]> = match groups.last() {
                Some(g) => std::borrow::Cow::Borrowed(*g),
                None => group_nodes(node),
            };
            let head = pystr::strip(&normalize_ws(&chars_of(&head_nodes))).to_string();
            if !head.is_empty() && self.meta.running_head.as_deref().unwrap_or("").is_empty() {
                self.meta.running_head = Some(head);
            }
            return;
        }
        let inner = self.inlines(&group_nodes(node));
        let inner = clean_inlines(inner, true);
        match name {
            "title" => {
                self.meta.title = Some(inner);
                // \title[running head]{full title}
                if let Some(opt) = optional_group(node) {
                    if self.meta.running_head.as_deref().unwrap_or("").is_empty() {
                        let head = pystr::strip(&normalize_ws(&chars_of(opt))).to_string();
                        if !head.is_empty() {
                            self.meta.running_head = Some(head);
                        }
                    }
                }
            }
            "date" => self.meta.date = Some(inner),
            "keywords" | "IEEEkeywords" => self.meta.keywords = Some(inner),
            "email" if !inner.is_empty() => {
                let mut v = vec![Inline::text("Email: ")];
                v.extend(inner);
                self.meta.affiliations.push(v);
            }
            "orcid" => {
                let oid = pystr::strip(&chars_of(&group_nodes(node))).to_string();
                if !oid.is_empty() {
                    let url = if oid.starts_with("http") {
                        oid.clone()
                    } else {
                        format!("https://orcid.org/{oid}")
                    };
                    self.meta.affiliations.push(vec![Inline::Link(Link {
                        inlines: vec![Inline::text(format!("ORCID: {oid}"))],
                        url,
                        anchor: None,
                    })]);
                }
            }
            _ => {}
        }
    }

    fn heading(&mut self, node: &Node, out: &mut Vec<Block>) {
        let name = node.macroname().unwrap_or("").trim_end_matches('*');
        let level = section_level(name, self.book_mode).unwrap_or(1);
        let inner = self.inlines(&group_nodes(node));
        let inner = clean_inlines(inner, true);
        // \chapter..\subsubsection are numbered by default; starred forms and
        // the run-in \paragraph/\subparagraph are not; \part has its own flag
        let is_part = name == "part";
        let numbered = !has_star(node) && (is_numbered_section(name) || is_part);
        let label = self.label_target(None);
        out.push(Block::Heading(Heading {
            level,
            inlines: inner,
            label,
            numbered,
            appendix: self.in_appendix && numbered && !is_part,
            part: is_part,
        }));
    }

    fn math_block(&mut self, verbatim: &str, env: &str, numbered: bool) -> Block {
        let latex = strip_math_delims(verbatim, env);
        // Per-line labels, aligned with the lines the math engine renders
        let mut line_labels: Vec<Option<String>> = Vec::new();
        if latex.contains("\\label") {
            for seg in tex2word_math::omml::split_top_level(&latex, "\\\\") {
                let lab = if seg.contains("\\label") {
                    extract_label(&seg)
                } else {
                    None
                };
                if !pystr::strip(&LABEL_TAG_RE.sub("", &seg)).is_empty() {
                    line_labels.push(lab);
                }
            }
        }
        let label = line_labels
            .iter()
            .flatten()
            .find(|l| !l.is_empty())
            .cloned();
        let latex = pystr::strip(&LABEL_TAG_RE.sub("", &latex)).to_string();
        let label = self.label_target(label);
        Block::MathBlock(MathBlock {
            latex,
            numbered,
            label,
            env: env.into(),
            line_labels,
        })
    }

    fn environment(&mut self, node: &Node, out: &mut Vec<Block>) {
        let name = node.environmentname().unwrap_or("");
        let base = name.trim_end_matches('*');
        let starred = name.ends_with('*');
        let nodelist = node.nodelist().unwrap_or(&[]);
        if is_math_env(base) {
            let numbered = !is_unnumbered_math_env(base) && !starred;
            let b = self.math_block(&verbatim_inner(node), base, numbered);
            out.push(b);
            return;
        }
        if matches!(base, "itemize" | "enumerate" | "description") {
            let l = self.list(node, base == "enumerate", base == "description");
            out.push(l);
            return;
        }
        if base == "figure" || base == "wrapfigure" {
            self.figure(node, out, starred);
            return;
        }
        if base == "table" || base == "wraptable" {
            self.table_float(node, out, starred);
            return;
        }
        if matches!(
            base,
            "tabular"
                | "array"
                | "tabularx"
                | "tabulary"
                | "longtable"
                | "supertabular"
                | "xtabular"
                | "mpsupertabular"
        ) {
            let t = self.tabular(node);
            out.push(t);
            return;
        }
        if base == "tcolorbox" || self.tcb_envs.contains_key(base) {
            let q = self.tcolorbox_quote(node, base);
            out.push(q);
            return;
        }
        if matches!(
            base,
            "quote"
                | "quotation"
                | "verse"
                | "displayquote"
                | "displaycquote"
                | "framed"
                | "shaded"
                | "mdframed"
                | "tcolorbox"
                | "boxedminipage"
                | "leftbar"
        ) || self.box_envs.contains(base)
        {
            let (bg, line) = self.box_styles.get(base).cloned().unwrap_or((None, None));
            let shade = bg
                .filter(|s| !s.is_empty())
                .and_then(|b| self.colors.resolve(&b, None));
            let border = line
                .filter(|s| !s.is_empty())
                .and_then(|l| self.colors.resolve(&l, None));
            let blocks = self.blocks(nodelist);
            out.push(Block::Quote(Quote {
                blocks,
                shade,
                border,
                title: None,
                title_shade: None,
            }));
            return;
        }
        if matches!(base, "verbatim" | "lstlisting" | "minted") {
            out.push(Block::CodeBlock(CodeBlock {
                text: verbatim_inner(node),
                lang: None,
            }));
            return;
        }
        if base == "abstract" {
            let blocks = self.blocks(nodelist);
            self.meta.abstract_ = Some(blocks);
            return;
        }
        if matches!(base, "center" | "flushleft" | "flushright" | "minipage")
            && has_captionof(nodelist, "figure")
        {
            // a non-floating figure: \includegraphics + \captionof{figure}{…}
            self.figure(node, out, false);
            return;
        }
        if matches!(base, "center" | "flushleft" | "flushright") {
            let align = match base {
                "center" => "center",
                "flushleft" => "left",
                _ => "right",
            };
            for mut blk in self.blocks(nodelist) {
                if let Block::Paragraph(p) = &mut blk {
                    if p.align.is_none() {
                        p.align = Some(align.into());
                    }
                }
                out.push(blk);
            }
            return;
        }
        if base == "document" || base == "minipage" {
            let blocks = self.blocks(nodelist);
            out.extend(blocks);
            return;
        }
        if base == "thebibliography" {
            let b = self.thebibliography(node);
            out.push(b);
            return;
        }
        if self.theorem_envs.contains_key(base) || base == "proof" {
            let t = self.theorem(node, base, starred);
            out.push(t);
            return;
        }
        if base == "algorithm" || base == "algorithm2e" {
            let a = self.algorithm(node);
            out.push(a);
            return;
        }
        if OPAQUE_ENVS.contains(&base) {
            // graphics we cannot translate -> placeholder + warning
            self.report
                .warn(name, format!("{name} kept as a graphics placeholder"));
            out.push(Block::Figure(Figure {
                image: None,
                source: node.latex_verbatim().to_string(),
                ..Figure::default()
            }));
            return;
        }
        // A custom wrapper that contains \item is a list
        if nodelist.iter().any(|c| c.macroname() == Some("item")) {
            let ordered =
                base.contains("enum") || base.contains("ordered") || base.contains("number");
            self.report.info(
                name,
                format!("custom list environment {name}: rendered as a list"),
            );
            let l = self.list(node, ordered, false);
            out.push(l);
            return;
        }
        if is_transparent_env(base) {
            let blocks = self.blocks(nodelist);
            out.extend(blocks);
            return;
        }
        // Otherwise treat the unknown environment as a transparent wrapper
        self.report.warn(
            name,
            format!("unknown environment {name}: treated as transparent"),
        );
        let blocks = self.blocks(nodelist);
        out.extend(blocks);
    }

    fn theorem(&mut self, node: &Node, base: &str, starred: bool) -> Block {
        let is_proof = base == "proof";
        let display = if is_proof {
            "Proof".to_string()
        } else {
            self.theorem_envs
                .get(base)
                .cloned()
                .unwrap_or_else(|| pystr::capitalize(base))
        };
        let title = self.env_optional_title(node);
        // proof is unnumbered; so are starred uses and \newtheorem* envs
        let unnumbered = is_proof || starred || self.unnumbered_theorems.contains(base);
        // a shared-counter env numbers against the counter it shares
        let counter = if unnumbered {
            None
        } else {
            Some(
                self.theorem_counters
                    .get(base)
                    .cloned()
                    .unwrap_or_else(|| display.clone()),
            )
        };
        // become the label target *before* parsing the body
        let label = self.label_target(None);
        let blocks = self.blocks(node.nodelist().unwrap_or(&[]));
        Block::Theorem(Theorem {
            kind: display,
            blocks,
            title,
            label,
            counter,
        })
    }

    fn algorithm(&mut self, node: &Node) -> Block {
        let nodelist = node.nodelist().unwrap_or(&[]);
        let mut caption: Option<Vec<Inline>> = None;
        let mut label: Option<String> = None;
        for child in walk_macros(nodelist) {
            match child.macroname() {
                Some("caption") => caption = Some(self.inlines(&group_nodes(child))),
                Some("label") => label = Some(chars_of(&group_nodes(child))),
                _ => {}
            }
        }
        let lines: Vec<AlgLine> =
            crate::algorithms::parse_algorithm_body(nodelist, &mut |ns| self.inlines(ns));
        let label = self.label_target(label);
        Block::Algorithm(Algorithm {
            lines,
            caption,
            label,
            numbered: true,
        })
    }

    fn env_optional_title(&mut self, node: &Node) -> Option<Vec<Inline>> {
        if let Some(Some(opt)) = argnlist(node).first() {
            if let Some((list, _)) = group_parts(opt) {
                if !list.is_empty() {
                    return Some(self.inlines(list));
                }
            }
        }
        None
    }

    /// Parse a `thebibliography` env's `\bibitem` entries into CSL items.
    fn thebibliography(&mut self, node: &Node) -> Block {
        let mut key: Option<String> = None;
        let mut buf: Vec<Node> = Vec::new();
        let mut order: i64 = 0;
        let flush_item =
            |b: &mut Self, key: &Option<String>, buf: &mut Vec<Node>, order: &mut i64| {
                if let Some(k) = key {
                    let inl = b.inlines(buf);
                    let text = inlines_to_text_with(&inl, &|i| b.footmark_content(i));
                    *order += 1;
                    let mut fields = Dict::new();
                    fields.insert("note", Value::Str(text));
                    fields.insert("_order", Value::Int(*order));
                    b.thebib_items.insert(
                        k.clone(),
                        CSLItem {
                            id: k.clone(),
                            item_type: "document".into(),
                            csl_fields: fields,
                        },
                    );
                }
                buf.clear();
            };
        for child in node.nodelist().unwrap_or(&[]) {
            if child.macroname() == Some("bibitem") {
                flush_item(self, &key, &mut buf, &mut order);
                key = Some(chars_of(&group_nodes(child)));
            } else if key.is_some() {
                buf.push(child.clone());
            }
        }
        flush_item(self, &key, &mut buf, &mut order);
        numeric_bibliography()
    }

    fn list(&mut self, node: &Node, ordered: bool, description: bool) -> Block {
        let mut items: Vec<ListItem> = Vec::new();
        let mut current: Vec<Node> = Vec::new();
        let mut term: Option<Vec<Inline>> = None;
        let mut started = false;
        let flush_item = |b: &mut Self,
                          items: &mut Vec<ListItem>,
                          current: &mut Vec<Node>,
                          term: &mut Option<Vec<Inline>>| {
            // become the label target before parsing the body
            let label = b.label_target(None);
            let blocks = b.blocks(current);
            items.push(ListItem {
                blocks,
                term: term.take(),
                label,
            });
            current.clear();
        };
        for child in node.nodelist().unwrap_or(&[]) {
            if child.macroname() == Some("item") {
                if started {
                    flush_item(self, &mut items, &mut current, &mut term);
                }
                started = true;
                term = self.item_term(child);
            } else if started {
                current.push(child.clone());
            }
            // content before the first \item is discarded
        }
        if started {
            flush_item(self, &mut items, &mut current, &mut term);
        }
        Block::ItemList(ItemList {
            ordered,
            items,
            description,
        })
    }

    /// Extract the optional `\item[term]` label (description lists).
    fn item_term(&mut self, node: &Node) -> Option<Vec<Inline>> {
        for arg in argnlist(node).iter().flatten() {
            if let Some((list, d)) = group_parts(arg) {
                if d.0 == "[" && d.1 == "]" {
                    let inl = self.inlines(list);
                    return Some(clean_inlines(inl, true));
                }
            }
        }
        None
    }

    fn figure(&mut self, node: &Node, out: &mut Vec<Block>, spanning: bool) {
        let nodelist = node.nodelist().unwrap_or(&[]);
        let mut fig = Figure {
            image: None,
            source: node.latex_verbatim().to_string(),
            spanning,
            ..Figure::default()
        };
        // panels: one entry per top-level \includegraphics (plus a possible
        // leading caption-only entry)
        let mut panels: Vec<Panel> = Vec::new();
        self.collect_figure_parts(nodelist, &mut fig, Some(&mut panels));
        let img_panels = panels.iter().filter(|p| p.image.is_some()).count();
        if !fig.subfigures.is_empty() || img_panels <= 1 {
            if fig.image.is_none() && fig.subfigures.is_empty() && !has_opaque_env(&fig.source) {
                // no graphic and no drawing env: the figure *is* its body
                fig.blocks = self.figure_body_blocks(nodelist);
            }
            if fig.image.is_none() && fig.subfigures.is_empty() && fig.blocks.is_empty() {
                self.report
                    .warn("figure", "figure without convertible graphics (e.g. TikZ)");
            }
            fig.label = self.label_target(fig.label.take());
            out.push(Block::Figure(fig));
            return;
        }
        // Several \includegraphics in one figure: one numbered figure per
        // image+caption panel (each with its own \label)
        for p in panels {
            if p.image.is_none() && p.caption.is_none() {
                continue;
            }
            out.push(Block::Figure(Figure {
                image: p.image,
                source: String::new(),
                spanning,
                caption: p.caption,
                label: p.label,
                caption_numbered: p.numbered,
                ..Figure::default()
            }));
        }
        self.labelable = None; // a trailing \label would be ambiguous across panels
    }

    /// Parse a graphic-less figure body into blocks (caption suppressed,
    /// label target detached, layout-only paragraphs dropped).
    fn figure_body_blocks(&mut self, nodes: &[Node]) -> Vec<Block> {
        let (prev_flag, prev_target) = (self.in_figure_body, self.labelable);
        self.in_figure_body = true;
        self.labelable = None;
        let mut blocks: Vec<Block> = Vec::new();
        let mut segment: Vec<Node> = Vec::new();
        let n = nodes.len();
        let mut i = 0;
        while i < n {
            if is_minipage(&nodes[i]) {
                // a run of sibling minipages (joined only by \hfill & co.)
                // sits side by side in LaTeX -> one borderless layout row
                let mut run = vec![&nodes[i]];
                let mut k = i + 1;
                let mut last = i;
                while k < n && (is_minipage(&nodes[k]) || is_layout_glue(&nodes[k])) {
                    if is_minipage(&nodes[k]) {
                        run.push(&nodes[k]);
                        last = k;
                    }
                    k += 1;
                }
                if run.len() >= 2 {
                    let seg_blocks = self.blocks(&segment);
                    blocks.extend(seg_blocks);
                    segment.clear();
                    let row = self.minipage_row(&run);
                    blocks.push(row);
                    i = last + 1;
                    continue;
                }
            }
            segment.push(nodes[i].clone());
            i += 1;
        }
        let seg_blocks = self.blocks(&segment);
        blocks.extend(seg_blocks);
        self.in_figure_body = prev_flag;
        self.labelable = prev_target;
        blocks.retain(|b| !is_blank_paragraph(b));
        blocks
    }

    /// Side-by-side minipages -> a one-row borderless layout table.
    fn minipage_row(&mut self, minipages: &[&Node]) -> Block {
        let mut cells = Vec::new();
        for mp in minipages {
            let mut blocks = self.blocks(mp.nodelist().unwrap_or(&[]));
            blocks.retain(|b| !is_blank_paragraph(b));
            cells.push(TableCell {
                blocks,
                ..TableCell::default()
            });
        }
        let n = cells.len();
        Block::Table(Table {
            rows: vec![TableRow {
                cells,
                ..TableRow::default()
            }],
            colspec: vec!["left".to_string(); n],
            layout: true,
            ..Table::default()
        })
    }

    /// A `tcolorbox` (or a `\newtcolorbox` env) -> a coloured callout.
    fn tcolorbox_quote(&mut self, node: &Node, base: &str) -> Block {
        let mut texts: Vec<String> = Vec::new();
        if let Some(env) = self.tcb_envs.get(base).cloned() {
            let mut opts = env.opts.clone();
            for (k, arg) in env_arg_texts(node, &env).iter().enumerate() {
                opts = opts.replace(&format!("#{}", k + 1), arg);
            }
            texts.push(opts);
        }
        if base == "tcolorbox" {
            texts.push(graphics_option_text(node));
        }
        let eff = self.tcb_resolve(&texts);
        let colour = |b: &Self, key: &str, default: &str| -> Option<String> {
            let spec = eff
                .get(key)
                .filter(|s| !s.is_empty())
                .map_or(default, String::as_str);
            b.colors.resolve(spec, None)
        };
        let shade = colour(self, "colback", "black!5!white");
        let mut border = colour(self, "colframe", "black!75!white");
        if length_to_emu(eff.get("boxrule").map_or("1pt", String::as_str)) == Some(0.0) {
            border = shade.clone(); // boxrule=0pt: no visible frame
        }
        let mut title = None;
        let mut title_shade = None;
        if let Some(t) = eff.get("title").filter(|t| !t.is_empty()).cloned() {
            let mut inl = self.fragment_inlines(&t);
            if eff
                .get("fonttitle")
                .is_some_and(|f| f.contains("\\bfseries"))
            {
                inl = vec![Inline::Emphasis(Emphasis {
                    inlines: inl,
                    kind: "bold".into(),
                })];
            }
            title = Some(vec![Inline::Colored(Colored {
                inlines: inl,
                fg: colour(self, "coltitle", "white"),
                bg: None,
            })]);
            let frame = eff
                .get("colframe")
                .filter(|s| !s.is_empty())
                .map_or("black!75!white", String::as_str);
            title_shade = colour(self, "colbacktitle", frame);
        }
        let blocks = self.blocks(node.nodelist().unwrap_or(&[]));
        Block::Quote(Quote {
            blocks,
            shade,
            border,
            title,
            title_shade,
        })
    }

    fn tcb_resolve(&self, texts: &[String]) -> HashMap<String, String> {
        let mut eff = HashMap::new();
        fn apply(b: &Builder, raw: &str, depth: usize, eff: &mut HashMap<String, String>) {
            if depth > 8 {
                return; // guard against self-referencing styles
            }
            for (key, val) in kv_list(raw) {
                match val {
                    None => {
                        if let Some(style) = b.tcb_styles.get(&key) {
                            apply(b, style, depth + 1, eff);
                        }
                    }
                    Some(v) if is_tcb_key(&key) => {
                        eff.insert(key, strip_braces(&v));
                    }
                    Some(_) => {}
                }
            }
        }
        for t in texts {
            if !t.is_empty() {
                apply(self, t, 0, &mut eff);
            }
        }
        eff
    }

    /// Parse a LaTeX fragment (e.g. a box title) into inlines.
    pub fn fragment_inlines(&mut self, latex: &str) -> Vec<Inline> {
        let ctx = self
            .ctx
            .clone()
            .unwrap_or_else(|| Arc::new(ContextDb::default_db()));
        let walker = LatexWalker::new(latex, ctx);
        match walker.get_latex_nodes(0, &Stop::default(), &ParsingState::default()) {
            Ok((nodes, _, _)) => {
                let inl = self.inlines(&nodes);
                clean_inlines(inl, true)
            }
            // never let a title abort the conversion
            Err(_) => vec![Inline::text(latex)],
        }
    }

    /// Walk a figure body, pulling out the parent image/caption/label and any
    /// subfigures; top-level parts are also recorded into `panels`.
    fn collect_figure_parts(
        &mut self,
        nodes: &[Node],
        fig: &mut Figure,
        mut panels: Option<&mut Vec<Panel>>,
    ) {
        for child in nodes {
            match &child.kind {
                NodeKind::Environment {
                    environmentname,
                    nodelist,
                    ..
                } => {
                    if environmentname.trim_end_matches('*') == "subfigure" {
                        let s = self.subfigure_from_env(child);
                        fig.subfigures.push(s);
                    } else if is_drawing_env(environmentname) {
                        // a picture's own \label/\caption (e.g. a `\foreach
                        // \label/\value in ...` loop variable) are not the
                        // figure's; graphics inside nodes still count
                        let prev = std::mem::replace(&mut self.in_picture, true);
                        self.collect_figure_parts(nodelist, fig, panels.as_deref_mut());
                        self.in_picture = prev;
                    } else {
                        self.collect_figure_parts(nodelist, fig, panels.as_deref_mut());
                    }
                }
                NodeKind::Group { nodelist, .. } => {
                    self.collect_figure_parts(nodelist, fig, panels.as_deref_mut());
                }
                NodeKind::Macro { macroname, .. } => match macroname.as_str() {
                    "subfloat" | "subfigure" => {
                        let s = self.subfigure_from_macro(child);
                        fig.subfigures.push(s);
                    }
                    "includegraphics" => {
                        let img = make_image(child);
                        fig.image = Some(img.clone());
                        if let Some(p) = panels.as_deref_mut() {
                            p.push(Panel {
                                image: Some(img),
                                caption: None,
                                label: None,
                                numbered: true,
                            });
                        }
                    }
                    "caption" | "captionof" | "label" if self.in_picture => {}
                    "caption" | "captionof" => {
                        let cap = self.inlines(&group_nodes(child));
                        let numbered = !has_star(child);
                        fig.caption = Some(cap.clone());
                        fig.caption_numbered = numbered;
                        if let Some(p) = panels.as_deref_mut() {
                            match p.last_mut() {
                                Some(last) if last.caption.is_none() => {
                                    last.caption = Some(cap);
                                    last.numbered = numbered;
                                }
                                _ => p.push(Panel {
                                    image: None,
                                    caption: Some(cap),
                                    label: None,
                                    numbered,
                                }),
                            }
                        }
                    }
                    "label" => {
                        let lab = chars_of(&group_nodes(child));
                        if fig.label.is_none() {
                            fig.label = Some(lab.clone());
                        }
                        if let Some(last) = panels.as_deref_mut().and_then(|p| p.last_mut()) {
                            last.label = Some(lab);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn subfigure_from_env(&mut self, node: &Node) -> SubFigure {
        let mut sub = SubFigure::default();
        for child in walk_macros(node.nodelist().unwrap_or(&[])) {
            match child.macroname() {
                Some("includegraphics") => sub.image = Some(make_image(child)),
                Some("caption") => sub.caption = Some(self.inlines(&group_nodes(child))),
                Some("label") => sub.label = Some(chars_of(&group_nodes(child))),
                _ => {}
            }
        }
        sub
    }

    fn subfigure_from_macro(&mut self, node: &Node) -> SubFigure {
        // \subfloat[caption]{content}; content holds the \includegraphics
        let groups: Vec<&[Node]> = argnlist(node)
            .iter()
            .flatten()
            .filter_map(group_parts)
            .map(|(n, _)| n)
            .collect();
        let caption = self.env_optional_title(node);
        let mut sub = SubFigure {
            image: None,
            caption,
            label: None,
        };
        if let Some(last) = groups.last() {
            for child in walk_macros(last) {
                match child.macroname() {
                    Some("includegraphics") => sub.image = Some(make_image(child)),
                    Some("label") => sub.label = Some(chars_of(&group_nodes(child))),
                    _ => {}
                }
            }
        }
        sub
    }

    fn table_float(&mut self, node: &Node, out: &mut Vec<Block>, spanning: bool) {
        // Pull the float's own caption/label, then process the rest as blocks
        let mut caption: Option<Vec<Inline>> = None;
        let mut caption_numbered = true;
        let mut label: Option<String> = None;
        let mut content: Vec<Node> = Vec::new();
        for child in node.nodelist().unwrap_or(&[]) {
            match child.macroname() {
                Some("caption") => {
                    caption = Some(self.inlines(&group_nodes(child)));
                    caption_numbered = !has_star(child);
                }
                Some("label") => label = Some(chars_of(&group_nodes(child))),
                _ => content.push(child.clone()),
            }
        }
        let mut sub_blocks = self.blocks(&content);
        let Some(first) = sub_blocks.iter().position(|b| matches!(b, Block::Table(_))) else {
            self.report.warn("table", "table float without tabular");
            out.push(Block::RawPassthrough(RawPassthrough {
                latex: node.latex_verbatim().to_string(),
                reason: "table without tabular".into(),
            }));
            return;
        };
        // attach the float caption/label to the first table (for \ref)
        let slot = self.label_target(label);
        if let Block::Table(t) = &mut sub_blocks[first] {
            t.caption = caption;
            t.caption_numbered = caption_numbered;
            t.label = slot;
            t.spanning = spanning;
        }
        out.extend(sub_blocks);
    }

    fn tabular(&mut self, node: &Node) -> Block {
        // column spec is the env's last mandatory group argument
        let (colspec, colwidths) = parse_colspec(&env_colspec(node));
        let verb = node.latex_verbatim();
        let booktabs = verb.contains("\\toprule") || verb.contains("\\midrule");
        let mut nodes: Vec<Node> = node.nodelist().unwrap_or(&[]).to_vec();
        let mut caption = None;
        let mut label = None;
        if node.environmentname() == Some("longtable") {
            let (n, c, l) = self.longtable_prep(&nodes);
            nodes = n;
            caption = c;
            label = l;
        }
        let rows = self.tabular_rows(&nodes, &colspec);
        Block::Table(Table {
            rows,
            colspec,
            booktabs,
            caption,
            label,
            colwidths,
            ..Table::default()
        })
    }

    /// Prepare a `longtable` body for the generic row parser: pull out the
    /// caption/label and keep a single header block plus the body rows.
    fn longtable_prep(
        &mut self,
        nodes: &[Node],
    ) -> (Vec<Node>, Option<Vec<Inline>>, Option<String>) {
        let marker = |m: &str| -> Option<&'static str> {
            Some(match m {
                "endfirsthead" => "firsthead",
                "endhead" => "head",
                "endfoot" => "foot",
                "endlastfoot" => "lastfoot",
                _ => return None,
            })
        };
        let mut sections: HashMap<&'static str, Vec<Node>> = HashMap::new();
        let mut markers: HashMap<&'static str, Node> = HashMap::new();
        let mut caption: Option<Vec<Inline>> = None;
        let mut label: Option<String> = None;
        let mut buf: Vec<Node> = Vec::new();
        for child in nodes {
            if let Some(m) = child.macroname().and_then(marker) {
                sections.insert(m, std::mem::take(&mut buf));
                markers.insert(m, child.clone());
                continue;
            }
            match child.macroname() {
                Some("caption") => {
                    if caption.is_none() {
                        caption = Some(self.inlines(&group_nodes(child)));
                    }
                    continue; // never keep the caption node among the rows
                }
                Some("label") => {
                    if label.is_none() {
                        label = Some(pystr::strip(&chars_of(&group_nodes(child))).to_string());
                    }
                    continue;
                }
                _ => {}
            }
            buf.push(child.clone());
        }
        let src = if sections.contains_key("head") {
            "head"
        } else if sections.contains_key("firsthead") {
            "firsthead"
        } else {
            return (buf, caption, label);
        };
        // re-append the section's delimiter so the row parser marks the
        // header rows
        let mut out = sections.remove(src).unwrap();
        out.push(markers.remove(src).unwrap());
        out.extend(buf);
        (out, caption, label)
    }

    fn tabular_rows(&mut self, nodes: &[Node], colspec: &[String]) -> Vec<TableRow> {
        let mut rows: Vec<TableRow> = Vec::new();
        let mut cur_cells: Vec<Vec<Node>> = vec![Vec::new()];
        let mut row_started = false;
        let mut row_shade: Option<String> = None; // \rowcolor
                                                  // number of leading rows that form the (repeatable) header
        let mut header_end: Option<usize> = None;

        let end_row = |b: &mut Self,
                       rows: &mut Vec<TableRow>,
                       cur_cells: &mut Vec<Vec<Node>>,
                       row_started: &mut bool,
                       row_shade: &mut Option<String>| {
            if *row_started {
                let mut cells = Vec::new();
                for (ci, cell_nodes) in cur_cells.iter().enumerate() {
                    let align = colspec.get(ci).map_or("left", String::as_str);
                    let mut cell = b.make_cell(cell_nodes, align);
                    if cell.shade.is_none() {
                        // \cellcolor overrides the row colour
                        cell.shade = row_shade.clone();
                    }
                    cells.push(cell);
                }
                rows.push(TableRow {
                    cells,
                    ..TableRow::default()
                });
            }
            cur_cells.clear();
            cur_cells.push(Vec::new());
            *row_started = false;
            *row_shade = None;
        };

        for child in nodes {
            if matches!(&child.kind, NodeKind::Specials { specials_chars, .. } if specials_chars == "&")
            {
                cur_cells.push(Vec::new());
                row_started = true;
                continue;
            }
            if let Some(name) = child.macroname() {
                match name {
                    "rowcolor" => {
                        row_shade = self.color_of(child);
                        continue;
                    }
                    "\\" | "tabularnewline" => {
                        end_row(
                            self,
                            &mut rows,
                            &mut cur_cells,
                            &mut row_started,
                            &mut row_shade,
                        );
                        continue;
                    }
                    "midrule" | "endhead" | "endfirsthead" => {
                        end_row(
                            self,
                            &mut rows,
                            &mut cur_cells,
                            &mut row_started,
                            &mut row_shade,
                        );
                        if header_end.is_none() {
                            header_end = Some(rows.len());
                        }
                        continue;
                    }
                    "cline" | "cmidrule" => {
                        if let (Some(rng), Some(last)) = (cmidrule_range(child), rows.last_mut()) {
                            apply_partial_rule(last, rng);
                        }
                        continue;
                    }
                    "hline" | "toprule" | "bottomrule" | "endfoot" | "endlastfoot"
                    | "morecmidrules" | "addlinespace" => continue,
                    _ => {}
                }
            }
            if matches!(&child.kind, NodeKind::Chars { chars } if pystr::strip(chars).is_empty()) {
                cur_cells.last_mut().unwrap().push(child.clone());
                continue;
            }
            cur_cells.last_mut().unwrap().push(child.clone());
            row_started = true;
        }
        end_row(
            self,
            &mut rows,
            &mut cur_cells,
            &mut row_started,
            &mut row_shade,
        );

        if let Some(h) = header_end.filter(|&h| h > 0) {
            for row in rows.iter_mut().take(h) {
                row.is_header = true;
            }
        }
        rows
    }

    /// Pull any `\cellcolor[model]{c}` out of a node list; (shade, rest).
    fn strip_cellcolor(&self, nodes: &[Node]) -> (Option<String>, Vec<Node>) {
        let mut shade = None;
        let mut content = Vec::new();
        for n in nodes {
            if n.macroname() == Some("cellcolor") {
                shade = self.color_of(n).filter(|s| !s.is_empty()).or(shade);
            } else {
                content.push(n.clone());
            }
        }
        (shade, content)
    }

    fn make_cell(&mut self, cell_nodes: &[Node], align: &str) -> TableCell {
        let (mut shade, mut cell_nodes) = self.strip_cellcolor(cell_nodes);
        let mut align = align.to_string();
        let (mut colspan, mut rowspan) = (1, 1);
        // Peel \multicolumn / \multirow wrappers (they nest in either order)
        let mut peeled = true;
        while peeled {
            peeled = false;
            let mut next: Option<Vec<Node>> = None;
            for m in &cell_nodes {
                let Some(name) = m.macroname() else {
                    continue;
                };
                let groups: Vec<&[Node]> = argnlist(m)
                    .iter()
                    .flatten()
                    .filter_map(group_parts)
                    .map(|(n, _)| n)
                    .collect();
                if name == "multicolumn" && groups.len() >= 3 {
                    colspan = int_or_default(&chars_of(groups[0]), 1);
                    let (cspec, _) = parse_colspec(&chars_of(groups[1]));
                    align = cspec.first().cloned().unwrap_or_else(|| "center".into());
                    let (inner_shade, rest) = self.strip_cellcolor(groups[2]);
                    shade = shade.filter(|s| !s.is_empty()).or(inner_shade);
                    next = Some(rest);
                    break;
                }
                if name == "multirow" && groups.len() >= 2 {
                    // \multirow{n}{width}{content}: span from the first group,
                    // content from the last; a negative span can't merge
                    let span = int_or_default(&chars_of(groups[0]), 1);
                    if span > 1 {
                        rowspan = span;
                    }
                    let (inner_shade, rest) = self.strip_cellcolor(groups[groups.len() - 1]);
                    shade = shade.filter(|s| !s.is_empty()).or(inner_shade);
                    next = Some(rest);
                    break;
                }
            }
            if let Some(rest) = next {
                cell_nodes = rest;
                peeled = true;
            }
        }
        let blocks = self.blocks(&cell_nodes);
        TableCell {
            blocks: flatten_stacked_tables(blocks),
            colspan,
            rowspan,
            align,
            shade,
            border_bottom: false,
        }
    }
}

/// Flush the buffered inline run into a paragraph, applying any block-level
/// declaration scopes (`\color{c}` / `\bfseries` used unbraced).
fn flush(inline_buf: &mut Vec<Inline>, scope_marks: &mut Vec<(usize, Mark)>, out: &mut Vec<Block>) {
    for (idx, mark) in scope_marks.drain(..).rev() {
        if idx >= inline_buf.len() {
            continue;
        }
        match mark {
            Mark::Color(Some(fg)) if !fg.is_empty() => {
                let seg = inline_buf.split_off(idx);
                inline_buf.push(Inline::Colored(Colored {
                    inlines: seg,
                    fg: Some(fg),
                    bg: None,
                }));
            }
            Mark::Emph(kind) => {
                let seg = inline_buf.split_off(idx);
                inline_buf.push(Inline::Emphasis(Emphasis {
                    inlines: seg,
                    kind: kind.into(),
                }));
            }
            Mark::Color(_) => {}
        }
    }
    if has_visible(inline_buf) {
        let cleaned = clean_inlines(inline_buf.clone(), true);
        if !cleaned.is_empty() {
            out.push(Block::Paragraph(Paragraph {
                inlines: cleaned,
                align: None,
            }));
        }
    }
    inline_buf.clear();
}

/// Any inline that isn't whitespace-only text.
fn has_visible(inlines: &[Inline]) -> bool {
    inlines
        .iter()
        .any(|x| !matches!(x, Inline::Text(t) if pystr::strip(&t.value).is_empty()))
}

fn inline_special(spec: &str, out: &mut Vec<Inline>) {
    match spec {
        "~" => out.push(Inline::text("\u{a0}")),
        "``" | "''" => out.push(Inline::text("\"")),
        "`" | "'" => out.push(Inline::text("'")),
        "--" => out.push(Inline::text("–")),
        "---" => out.push(Inline::text("—")),
        _ => {} // otherwise ignore
    }
}

fn typewriter(text: String) -> Inline {
    Inline::Emphasis(Emphasis {
        inlines: vec![Inline::text(text)],
        kind: "typewriter".into(),
    })
}

fn make_ref(key: &str, kind: &str, style: &str) -> Inline {
    Inline::Ref(Ref {
        key: key.to_string(),
        ref_kind: kind.into(),
        bookmark: None,
        style: style.into(),
    })
}

fn numeric_bibliography() -> Block {
    Block::Bibliography(Bibliography {
        entries: Vec::new(),
        style: "numeric".into(),
    })
}

/// Extract `\citep[pre][post]{key}` locators (natbib: one optional is the
/// post-note; two are (pre, post)).
fn cite_locators(node: &Node) -> (Option<String>, Option<String>) {
    let opts: Vec<String> = argnlist(node)
        .iter()
        .flatten()
        .filter_map(group_parts)
        .filter(|(_, d)| d.0 == "[" && d.1 == "]")
        .map(|(n, _)| chars_of(n))
        .collect();
    let nonempty = |s: &String| (!s.is_empty()).then(|| s.clone());
    match opts.len() {
        0 => (None, None),
        1 => (None, nonempty(&opts[0])),
        _ => (nonempty(&opts[0]), nonempty(&opts[1])),
    }
}

/// Raw LaTeX of a user env's `\begin` arguments (an optional first argument
/// falls back to its declared default).
fn env_arg_texts(node: &Node, env: &TcbEnv) -> Vec<String> {
    let args = argnlist(node);
    // args[: env.nargs] (a negative count slices from the end, as in Python)
    let take = if env.nargs >= 0 {
        (env.nargs as usize).min(args.len())
    } else {
        args.len().saturating_sub(env.nargs.unsigned_abs() as usize)
    };
    let mut texts: Vec<String> = Vec::new();
    for a in &args[..take] {
        let Some(a) = a else {
            texts.push(String::new());
            continue;
        };
        let v = pystr::strip(a.latex_verbatim());
        let first_ok = v.chars().next().is_none_or(|c| c == '{' || c == '[');
        let last_ok = v.chars().last().is_none_or(|c| c == '}' || c == ']');
        if first_ok && last_ok {
            texts.push(
                v.get(1..v.len().saturating_sub(1))
                    .unwrap_or("")
                    .to_string(),
            );
        } else {
            texts.push(v.to_string());
        }
    }
    if let Some(d) = &env.default {
        if texts.first().is_none_or(String::is_empty) {
            if texts.is_empty() {
                texts.push(d.clone());
            } else {
                texts[0] = d.clone();
            }
        }
    }
    texts
}

fn starts_upper(s: &str) -> bool {
    s.chars().next().is_some_and(char::is_uppercase)
}

fn upper_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}
