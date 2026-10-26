//! The IR node types — a one-to-one mirror of the Python reference's `ir.py`.
//!
//! Every dataclass there is a struct here with the same fields, the same
//! defaults and the same JSON form (`{"@type": "<ClassName>", <fields>}`), so a
//! document serialised by either implementation deserialises in the other.
//! Python's `Literal[...]`-typed fields stay plain strings, because the
//! reference itself stores values outside the declared literals (a label kind
//! of `"algorithm"`, say); the accepted values are listed on each field.

use crate::dict::Dict;
use crate::json::{FromJson, JsonError, ToJson, Value};

/// Look up an object key (helper for the generated `from_json`).
fn lookup<'a>(obj: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    obj.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

macro_rules! node {
    (
        $(#[$meta:meta])*
        $name:ident {
            $(
                $(#[$fmeta:meta])*
                $field:ident : $ty:ty = $json:literal => $default:expr
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            $( $(#[$fmeta])* pub $field: $ty, )*
        }

        impl Default for $name {
            fn default() -> Self {
                Self { $( $field: $default, )* }
            }
        }

        impl $name {
            /// The JSON discriminator (`@type`) — the Python class name.
            pub const TYPE: &'static str = stringify!($name);
        }

        impl ToJson for $name {
            #[allow(unused_mut)]
            fn to_json(&self) -> Value {
                let mut obj: Vec<(String, Value)> =
                    vec![("@type".to_string(), Value::Str(stringify!($name).to_string()))];
                $( obj.push(($json.to_string(), self.$field.to_json())); )*
                Value::Object(obj)
            }
        }

        impl FromJson for $name {
            #[allow(unused_variables, unused_mut)]
            fn from_json(v: &Value) -> Result<Self, JsonError> {
                let obj = v.as_object().ok_or_else(|| {
                    JsonError::new(concat!("expected an object for ", stringify!($name)))
                })?;
                let mut out = Self::default();
                $(
                    if let Some(x) = lookup(obj, $json) {
                        out.$field = FromJson::from_json(x).map_err(|e| {
                            JsonError::new(format!(
                                "{}.{}: {}", stringify!($name), $json, e
                            ))
                        })?;
                    }
                )*
                Ok(out)
            }
        }
    };
}

// --------------------------------------------------------------------------- //
// Inline nodes
// --------------------------------------------------------------------------- //

node! {
    /// A run of literal text.
    Text {
        value: String = "value" => String::new(),
    }
}

node! {
    /// A styled span. `kind` is one of `italic`, `bold`, `underline`,
    /// `smallcaps`, `typewriter`, `superscript`, `subscript`, `strike`,
    /// `highlight` (JSON field `kind_`).
    Emphasis {
        inlines: Vec<Inline> = "inlines" => Vec::new(),
        kind: String = "kind_" => "italic".into(),
    }
}

node! {
    /// Inline math carrying the raw (macro-expanded) LaTeX source.
    Math {
        latex: String = "latex" => String::new(),
    }
}

node! {
    /// A cross-reference (`\ref`/`\eqref`/`\cref`/`\pageref`/…). `bookmark` is
    /// filled in by the cross-reference pass. `style` is `plain` (bare number),
    /// `abbrev` (`fig. N`) or `full` (`Figure N`).
    Ref {
        key: String = "key" => String::new(),
        /// `generic`, `equation`, `figure`, `table`, `section`, `theorem`,
        /// `page`, `name`, `listitem` (and `algorithm` once resolved).
        ref_kind: String = "ref_kind" => "generic".into(),
        bookmark: Option<String> = "bookmark" => None,
        style: String = "style" => "plain".into(),
    }
}

node! {
    /// A citation. `mode` is `paren`, `text`, `foot`, `author`, `year` or `num`;
    /// `rendered` is the formatted in-text marker filled by the citation pass.
    Cite {
        keys: Vec<String> = "keys" => Vec::new(),
        mode: String = "mode" => "paren".into(),
        prefix: Option<String> = "prefix" => None,
        suffix: Option<String> = "suffix" => None,
        rendered: Option<String> = "rendered" => None,
    }
}

node! {
    /// A hyperlink: an external `url`, or an internal `anchor` (the raw label
    /// until the cross-reference pass replaces it with the sanitized bookmark).
    Link {
        inlines: Vec<Inline> = "inlines" => Vec::new(),
        url: String = "url" => String::new(),
        anchor: Option<String> = "anchor" => None,
    }
}

node! {
    /// An explicit line break (`\\`).
    LineBreak {}
}

node! {
    /// A footnote; the back-end lifts its content into `footnotes.xml`.
    Footnote {
        inlines: Vec<Inline> = "inlines" => Vec::new(),
    }
}

node! {
    /// An endnote; the back-end lifts its content into `endnotes.xml`.
    Endnote {
        inlines: Vec<Inline> = "inlines" => Vec::new(),
    }
}

node! {
    /// An `\index{term}` marker -> a hidden Word `XE` index-entry field.
    IndexEntry {
        term: String = "term" => String::new(),
    }
}

node! {
    /// Coloured inline content: `fg` text colour and/or `bg` shading, each a
    /// 6-hex `RRGGBB` string (`None` = inherit).
    Colored {
        inlines: Vec<Inline> = "inlines" => Vec::new(),
        fg: Option<String> = "fg" => None,
        bg: Option<String> = "bg" => None,
    }
}

node! {
    /// A font-size span (`\large` … `\tiny`); `half_points` is Word's `w:sz`.
    FontSize {
        inlines: Vec<Inline> = "inlines" => Vec::new(),
        half_points: i64 = "half_points" => 24,
    }
}

node! {
    /// Graceful-degradation escape hatch for inline constructs we can't model.
    RawInline {
        latex: String = "latex" => String::new(),
        reason: String = "reason" => "unsupported".into(),
    }
}

node! {
    /// A Word review comment recovered from `comments.xml` at its anchor.
    Comment {
        text: String = "text" => String::new(),
        author: String = "author" => String::new(),
    }
}

node! {
    /// A raster/vector image, used both inline and as a figure's graphic.
    /// Absolute sizes are EMU; `rel_*` are fractions of the text column/height.
    Image {
        path: String = "path" => String::new(),
        width: Option<f64> = "width" => None,
        height: Option<f64> = "height" => None,
        rel_width: Option<f64> = "rel_width" => None,
        rel_height: Option<f64> = "rel_height" => None,
        keep_aspect: bool = "keep_aspect" => false,
        original_format: String = "original_format" => String::new(),
        scale: Option<f64> = "scale" => None,
        /// rotation in degrees (counter-clockwise)
        angle: Option<f64> = "angle" => None,
        /// `[left, bottom, right, top]` in EMU (`trim=`)
        trim: Option<Vec<f64>> = "trim" => None,
        clip: bool = "clip" => false,
        alt: String = "alt" => String::new(),
    }
}

/// Inline-level content (`Inline` in `ir.py`).
#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(Text),
    Emphasis(Emphasis),
    Math(Math),
    Ref(Ref),
    Cite(Cite),
    Link(Link),
    LineBreak(LineBreak),
    Footnote(Footnote),
    Endnote(Endnote),
    Colored(Colored),
    FontSize(FontSize),
    Image(Image),
    RawInline(RawInline),
    Comment(Comment),
    IndexEntry(IndexEntry),
}

impl Inline {
    /// Shorthand for `Inline::Text(Text { value })`.
    pub fn text(value: impl Into<String>) -> Inline {
        Inline::Text(Text {
            value: value.into(),
        })
    }

    /// The node's `@type` discriminator.
    pub fn type_name(&self) -> &'static str {
        match self {
            Inline::Text(_) => Text::TYPE,
            Inline::Emphasis(_) => Emphasis::TYPE,
            Inline::Math(_) => Math::TYPE,
            Inline::Ref(_) => Ref::TYPE,
            Inline::Cite(_) => Cite::TYPE,
            Inline::Link(_) => Link::TYPE,
            Inline::LineBreak(_) => LineBreak::TYPE,
            Inline::Footnote(_) => Footnote::TYPE,
            Inline::Endnote(_) => Endnote::TYPE,
            Inline::Colored(_) => Colored::TYPE,
            Inline::FontSize(_) => FontSize::TYPE,
            Inline::Image(_) => Image::TYPE,
            Inline::RawInline(_) => RawInline::TYPE,
            Inline::Comment(_) => Comment::TYPE,
            Inline::IndexEntry(_) => IndexEntry::TYPE,
        }
    }

    /// The child inline list for container nodes (Python's
    /// `hasattr(node, "inlines")`).
    pub fn inlines(&self) -> Option<&Vec<Inline>> {
        match self {
            Inline::Emphasis(n) => Some(&n.inlines),
            Inline::Link(n) => Some(&n.inlines),
            Inline::Footnote(n) => Some(&n.inlines),
            Inline::Endnote(n) => Some(&n.inlines),
            Inline::Colored(n) => Some(&n.inlines),
            Inline::FontSize(n) => Some(&n.inlines),
            _ => None,
        }
    }

    /// Mutable access to the child inline list for container nodes.
    pub fn inlines_mut(&mut self) -> Option<&mut Vec<Inline>> {
        match self {
            Inline::Emphasis(n) => Some(&mut n.inlines),
            Inline::Link(n) => Some(&mut n.inlines),
            Inline::Footnote(n) => Some(&mut n.inlines),
            Inline::Endnote(n) => Some(&mut n.inlines),
            Inline::Colored(n) => Some(&mut n.inlines),
            Inline::FontSize(n) => Some(&mut n.inlines),
            _ => None,
        }
    }
}

impl ToJson for Inline {
    fn to_json(&self) -> Value {
        match self {
            Inline::Text(n) => n.to_json(),
            Inline::Emphasis(n) => n.to_json(),
            Inline::Math(n) => n.to_json(),
            Inline::Ref(n) => n.to_json(),
            Inline::Cite(n) => n.to_json(),
            Inline::Link(n) => n.to_json(),
            Inline::LineBreak(n) => n.to_json(),
            Inline::Footnote(n) => n.to_json(),
            Inline::Endnote(n) => n.to_json(),
            Inline::Colored(n) => n.to_json(),
            Inline::FontSize(n) => n.to_json(),
            Inline::Image(n) => n.to_json(),
            Inline::RawInline(n) => n.to_json(),
            Inline::Comment(n) => n.to_json(),
            Inline::IndexEntry(n) => n.to_json(),
        }
    }
}

impl FromJson for Inline {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        let ty = v
            .get("@type")
            .and_then(Value::as_str)
            .ok_or_else(|| JsonError::new("inline node without @type"))?;
        Ok(match ty {
            "Text" => Inline::Text(FromJson::from_json(v)?),
            "Emphasis" => Inline::Emphasis(FromJson::from_json(v)?),
            "Math" => Inline::Math(FromJson::from_json(v)?),
            "Ref" => Inline::Ref(FromJson::from_json(v)?),
            "Cite" => Inline::Cite(FromJson::from_json(v)?),
            "Link" => Inline::Link(FromJson::from_json(v)?),
            "LineBreak" => Inline::LineBreak(FromJson::from_json(v)?),
            "Footnote" => Inline::Footnote(FromJson::from_json(v)?),
            "Endnote" => Inline::Endnote(FromJson::from_json(v)?),
            "Colored" => Inline::Colored(FromJson::from_json(v)?),
            "FontSize" => Inline::FontSize(FromJson::from_json(v)?),
            "Image" => Inline::Image(FromJson::from_json(v)?),
            "RawInline" => Inline::RawInline(FromJson::from_json(v)?),
            "Comment" => Inline::Comment(FromJson::from_json(v)?),
            "IndexEntry" => Inline::IndexEntry(FromJson::from_json(v)?),
            other => return Err(JsonError::new(format!("unknown inline type {other:?}"))),
        })
    }
}

// --------------------------------------------------------------------------- //
// Block nodes
// --------------------------------------------------------------------------- //

node! {
    /// A heading. `level` 0 = title, 1 = top section/chapter, 2 = subsection, …
    Heading {
        level: i64 = "level" => 0,
        inlines: Vec<Inline> = "inlines" => Vec::new(),
        label: Option<String> = "label" => None,
        /// false for starred (`\section*`) and run-in headings
        numbered: bool = "numbered" => true,
        /// after `\appendix` -> letter (A, B, …) numbering
        appendix: bool = "appendix" => false,
        /// `\part` -> "Part I" upper-roman, independent of the section counter
        part: bool = "part" => false,
    }
}

node! {
    /// A body paragraph; `align` is `center`/`left`/`right` from the
    /// `center`/`flushleft`/`flushright` environments.
    Paragraph {
        inlines: Vec<Inline> = "inlines" => Vec::new(),
        align: Option<String> = "align" => None,
    }
}

node! {
    /// Display math; `env` is the originating environment name.
    MathBlock {
        latex: String = "latex" => String::new(),
        numbered: bool = "numbered" => false,
        label: Option<String> = "label" => None,
        env: String = "env" => "displaymath".into(),
        /// Per rendered line, that line's `\label` (or `None`), aligned with the
        /// numbered lines of a multi-line `align`/`eqnarray`. Empty for a
        /// single equation (`label` carries it).
        line_labels: Vec<Option<String>> = "line_labels" => Vec::new(),
    }
}

node! {
    /// One list item.
    ListItem {
        blocks: Vec<Block> = "blocks" => Vec::new(),
        /// the `\item[term]` of a description list
        term: Option<Vec<Inline>> = "term" => None,
        /// `\item\label{…}` -> referenceable list number
        label: Option<String> = "label" => None,
    }
}

node! {
    /// An `itemize`/`enumerate`/`description` list.
    ItemList {
        ordered: bool = "ordered" => false,
        items: Vec<ListItem> = "items" => Vec::new(),
        description: bool = "description" => false,
    }
}

node! {
    /// A table cell. `align` is `left`/`center`/`right`.
    TableCell {
        blocks: Vec<Block> = "blocks" => Vec::new(),
        colspan: i64 = "colspan" => 1,
        rowspan: i64 = "rowspan" => 1,
        align: String = "align" => "left".into(),
        /// `\cellcolor`/`\rowcolor` background as `RRGGBB`
        shade: Option<String> = "shade" => None,
        /// partial `\cmidrule`/`\cline` under this cell
        border_bottom: bool = "border_bottom" => false,
    }
}

node! {
    /// A table row.
    TableRow {
        cells: Vec<TableCell> = "cells" => Vec::new(),
        is_header: bool = "is_header" => false,
        rule_above: bool = "rule_above" => false,
        rule_below: bool = "rule_below" => false,
    }
}

node! {
    /// A table (`tabular` & friends, optionally inside a `table` float).
    Table {
        rows: Vec<TableRow> = "rows" => Vec::new(),
        /// per-column alignment: `left`/`center`/`right`
        colspec: Vec<String> = "colspec" => Vec::new(),
        booktabs: bool = "booktabs" => false,
        caption: Option<Vec<Inline>> = "caption" => None,
        label: Option<String> = "label" => None,
        /// per-column width in EMU (`p{}` columns), `None` = auto
        colwidths: Vec<Option<f64>> = "colwidths" => Vec::new(),
        /// false for `\caption*` (unnumbered caption)
        caption_numbered: bool = "caption_numbered" => true,
        /// `table*` -> span the full page width in a multi-column body
        spanning: bool = "spanning" => false,
        /// a borderless, full-width layout grid (side-by-side minipages), not
        /// a data table: no rules, equal-width columns across the text column
        layout: bool = "layout" => false,
    }
}

node! {
    /// A sub-figure of a figure (`subfigure`/`\subfloat`).
    SubFigure {
        image: Option<Image> = "image" => None,
        caption: Option<Vec<Inline>> = "caption" => None,
        label: Option<String> = "label" => None,
    }
}

node! {
    /// A figure float.
    Figure {
        image: Option<Image> = "image" => None,
        caption: Option<Vec<Inline>> = "caption" => None,
        label: Option<String> = "label" => None,
        source: String = "source" => String::new(),
        subfigures: Vec<SubFigure> = "subfigures" => Vec::new(),
        /// false for `\caption*` (unnumbered caption)
        caption_numbered: bool = "caption_numbered" => true,
        /// `figure*` -> span the full page width in a multi-column body
        spanning: bool = "spanning" => false,
        /// non-graphic body content (boxes, text, tabulars, …) for a figure with
        /// no `\includegraphics` and no drawing environment; rendered above the
        /// caption
        blocks: Vec<Block> = "blocks" => Vec::new(),
    }
}

node! {
    /// Verbatim code.
    CodeBlock {
        text: String = "text" => String::new(),
        lang: Option<String> = "lang" => None,
    }
}

node! {
    /// One line of pseudocode: an indentation depth + inline content.
    AlgLine {
        indent: i64 = "indent" => 0,
        inlines: Vec<Inline> = "inlines" => Vec::new(),
        /// whether the line gets a line number
        number: bool = "number" => true,
    }
}

node! {
    /// An `algorithm`/`algorithmic` block.
    Algorithm {
        lines: Vec<AlgLine> = "lines" => Vec::new(),
        caption: Option<Vec<Inline>> = "caption" => None,
        label: Option<String> = "label" => None,
        numbered: bool = "numbered" => true,
    }
}

node! {
    /// A set-off quote, or a coloured callout box when shaded/titled.
    Quote {
        blocks: Vec<Block> = "blocks" => Vec::new(),
        /// background fill (6-hex) for a callout box, or `None` for a plain quote
        shade: Option<String> = "shade" => None,
        /// border colour (6-hex) for a callout box
        border: Option<String> = "border" => None,
        /// a callout's title bar (tcolorbox `title=`)
        title: Option<Vec<Inline>> = "title" => None,
        title_shade: Option<String> = "title_shade" => None,
    }
}

node! {
    /// A theorem-like environment. `kind` is the display name ("Theorem",
    /// "Proof", …); `counter` the `SEQ` counter name (`None` = unnumbered).
    Theorem {
        kind: String = "kind" => String::new(),
        blocks: Vec<Block> = "blocks" => Vec::new(),
        title: Option<Vec<Inline>> = "title" => None,
        label: Option<String> = "label" => None,
        counter: Option<String> = "counter" => None,
    }
}

node! {
    /// `\tableofcontents`/`\listoffigures`/`\listoftables` -> a Word TOC
    /// field; `kind` is `contents`, `figures` or `tables`.
    TableOfContents {
        kind: String = "kind" => "contents".into(),
    }
}

node! {
    /// `\printindex` -> a Word `INDEX` field built from the `XE` entries.
    Index {}
}

node! {
    /// One bibliography entry as CSL-JSON (`type` is the CSL item type).
    CSLItem {
        id: String = "id" => String::new(),
        item_type: String = "type" => String::new(),
        csl_fields: Dict<Value> = "csl_fields" => Dict::new(),
    }
}

node! {
    /// The reference list; `style` is `numeric` or `author-year`.
    Bibliography {
        entries: Vec<CSLItem> = "entries" => Vec::new(),
        style: String = "style" => "numeric".into(),
    }
}

node! {
    /// Graceful-degradation escape hatch for block constructs we can't model.
    RawPassthrough {
        latex: String = "latex" => String::new(),
        reason: String = "reason" => "unsupported".into(),
    }
}

/// Block-level content (`Block` in `ir.py`).
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading(Heading),
    Paragraph(Paragraph),
    MathBlock(MathBlock),
    ItemList(ItemList),
    Table(Table),
    Figure(Figure),
    CodeBlock(CodeBlock),
    Quote(Quote),
    Theorem(Theorem),
    Algorithm(Algorithm),
    Bibliography(Bibliography),
    TableOfContents(TableOfContents),
    Index(Index),
    RawPassthrough(RawPassthrough),
}

impl Block {
    /// The node's `@type` discriminator.
    pub fn type_name(&self) -> &'static str {
        match self {
            Block::Heading(_) => Heading::TYPE,
            Block::Paragraph(_) => Paragraph::TYPE,
            Block::MathBlock(_) => MathBlock::TYPE,
            Block::ItemList(_) => ItemList::TYPE,
            Block::Table(_) => Table::TYPE,
            Block::Figure(_) => Figure::TYPE,
            Block::CodeBlock(_) => CodeBlock::TYPE,
            Block::Quote(_) => Quote::TYPE,
            Block::Theorem(_) => Theorem::TYPE,
            Block::Algorithm(_) => Algorithm::TYPE,
            Block::Bibliography(_) => Bibliography::TYPE,
            Block::TableOfContents(_) => TableOfContents::TYPE,
            Block::Index(_) => Index::TYPE,
            Block::RawPassthrough(_) => RawPassthrough::TYPE,
        }
    }

    /// The block's `label` attribute, for the kinds that carry one (Python's
    /// `getattr(block, "label", None)`).
    pub fn label(&self) -> Option<&str> {
        match self {
            Block::Heading(b) => b.label.as_deref(),
            Block::MathBlock(b) => b.label.as_deref(),
            Block::Table(b) => b.label.as_deref(),
            Block::Figure(b) => b.label.as_deref(),
            Block::Theorem(b) => b.label.as_deref(),
            Block::Algorithm(b) => b.label.as_deref(),
            _ => None,
        }
    }
}

impl ToJson for Block {
    fn to_json(&self) -> Value {
        match self {
            Block::Heading(n) => n.to_json(),
            Block::Paragraph(n) => n.to_json(),
            Block::MathBlock(n) => n.to_json(),
            Block::ItemList(n) => n.to_json(),
            Block::Table(n) => n.to_json(),
            Block::Figure(n) => n.to_json(),
            Block::CodeBlock(n) => n.to_json(),
            Block::Quote(n) => n.to_json(),
            Block::Theorem(n) => n.to_json(),
            Block::Algorithm(n) => n.to_json(),
            Block::Bibliography(n) => n.to_json(),
            Block::TableOfContents(n) => n.to_json(),
            Block::Index(n) => n.to_json(),
            Block::RawPassthrough(n) => n.to_json(),
        }
    }
}

impl FromJson for Block {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        let ty = v
            .get("@type")
            .and_then(Value::as_str)
            .ok_or_else(|| JsonError::new("block node without @type"))?;
        Ok(match ty {
            "Heading" => Block::Heading(FromJson::from_json(v)?),
            "Paragraph" => Block::Paragraph(FromJson::from_json(v)?),
            "MathBlock" => Block::MathBlock(FromJson::from_json(v)?),
            "ItemList" => Block::ItemList(FromJson::from_json(v)?),
            "Table" => Block::Table(FromJson::from_json(v)?),
            "Figure" => Block::Figure(FromJson::from_json(v)?),
            "CodeBlock" => Block::CodeBlock(FromJson::from_json(v)?),
            "Quote" => Block::Quote(FromJson::from_json(v)?),
            "Theorem" => Block::Theorem(FromJson::from_json(v)?),
            "Algorithm" => Block::Algorithm(FromJson::from_json(v)?),
            "Bibliography" => Block::Bibliography(FromJson::from_json(v)?),
            "TableOfContents" => Block::TableOfContents(FromJson::from_json(v)?),
            "Index" => Block::Index(FromJson::from_json(v)?),
            "RawPassthrough" => Block::RawPassthrough(FromJson::from_json(v)?),
            other => return Err(JsonError::new(format!("unknown block type {other:?}"))),
        })
    }
}

// --------------------------------------------------------------------------- //
// Document + metadata
// --------------------------------------------------------------------------- //

node! {
    /// Title-block and preamble metadata.
    DocumentMeta {
        title: Option<Vec<Inline>> = "title" => None,
        authors: Vec<Vec<Inline>> = "authors" => Vec::new(),
        date: Option<Vec<Inline>> = "date" => None,
        abstract_: Option<Vec<Block>> = "abstract" => None,
        keywords: Option<Vec<Inline>> = "keywords" => None,
        affiliations: Vec<Vec<Inline>> = "affiliations" => Vec::new(),
        /// BCP-47 document language (from babel/polyglossia)
        language: Option<String> = "language" => None,
        /// `\markboth`/`\markright`/`\title[short]` running head
        running_head: Option<String> = "running_head" => None,
        /// body column count (`\documentclass[twocolumn]`/`\twocolumn`)
        columns: i64 = "columns" => 1,
        /// `\setmainfont` -> Latin ascii/hAnsi default
        main_font: Option<String> = "main_font" => None,
        /// `\setCJKmainfont` -> East-Asian default (`eastAsia`)
        cjk_main_font: Option<String> = "cjk_main_font" => None,
        /// `\setCJKsansfont` -> East-Asian font for headings
        cjk_sans_font: Option<String> = "cjk_sans_font" => None,
        /// `\setCJKmonofont` -> East-Asian font for code
        cjk_mono_font: Option<String> = "cjk_mono_font" => None,
    }
}

node! {
    /// A resolved `\label`: its bookmark, counter series and display name.
    LabelInfo {
        kind: String = "kind" => String::new(),
        counter_name: String = "counter_name" => String::new(),
        bookmark: String = "bookmark" => String::new(),
        number: Option<String> = "number" => None,
        /// the target's title/caption text (for `\nameref`)
        name: Option<String> = "name" => None,
    }
}

node! {
    /// A whole document.
    Document {
        blocks: Vec<Block> = "blocks" => Vec::new(),
        meta: DocumentMeta = "meta" => DocumentMeta::default(),
        labels: Dict<LabelInfo> = "labels" => Dict::new(),
        /// book/report class with `\chapter` (top level is the chapter)
        book: bool = "book" => false,
    }
}

impl Document {
    /// `Document.to_dict()`.
    pub fn to_dict(&self) -> Value {
        self.to_json()
    }

    /// `Document.from_dict(data)`.
    pub fn from_dict(v: &Value) -> Result<Document, JsonError> {
        Document::from_json(v)
    }
}
