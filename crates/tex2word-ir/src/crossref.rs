//! Cross-reference resolution pass, IR -> IR (port of `transforms/crossref.py`).
//!
//! Collect every labelled target, assign it a sanitized Word bookmark name and
//! a `SEQ` counter, then rewrite each `Ref` to point at the matching bookmark
//! with the right kind. The back-end then emits live `SEQ`/`REF`/`PAGEREF`
//! fields — numbers that auto-renumber in Word.

use crate::dict::Dict;
use crate::nodes::{Block, Document, Figure, Inline, LabelInfo, Link};
use crate::pystr;
use crate::report::ConversionReport;
use crate::walk::{
    for_each_node, inline_lists_mut, meta_for_each_node, meta_inline_lists_mut, NodeRef,
};

fn counter_for(kind: &str) -> &'static str {
    match kind {
        "equation" => "Equation",
        "figure" => "Figure",
        "table" => "Table",
        "section" => "Section",
        "algorithm" => "Algorithm",
        "listing" => "Listing",
        _ => "Item",
    }
}

/// Word bookmark names: start with a letter, no spaces, at most 40 chars.
pub fn sanitize_bookmark(key: &str) -> String {
    let mut cleaned: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if !cleaned
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
    {
        cleaned = format!("ref_{cleaned}");
    }
    pystr::take_chars(&cleaned, 40).to_string()
}

/// Flatten an inline list to plain text (for `\nameref` display text).
pub fn inline_text(inlines: &[Inline]) -> String {
    fn walk(inlines: &[Inline], out: &mut String) {
        for n in inlines {
            match n {
                Inline::Text(t) => out.push_str(&t.value),
                other => {
                    if let Some(children) = other.inlines() {
                        out.push_str(&inline_text(children));
                    }
                }
            }
        }
    }
    let mut out = String::new();
    walk(inlines, &mut out);
    pystr::strip(&out).to_string()
}

/// The target's title/caption text, used by `\nameref`.
fn label_name(block: &Block) -> Option<String> {
    match block {
        Block::Heading(h) => Some(inline_text(&h.inlines)),
        Block::Theorem(t) => Some(match &t.title {
            Some(title) if !title.is_empty() => inline_text(title),
            _ => t.kind.clone(),
        }),
        Block::Figure(f) => f
            .caption
            .as_ref()
            .filter(|c| !c.is_empty())
            .map(|c| inline_text(c)),
        Block::Table(t) => t
            .caption
            .as_ref()
            .filter(|c| !c.is_empty())
            .map(|c| inline_text(c)),
        Block::CodeBlock(c) => c
            .caption
            .as_ref()
            .filter(|c| !c.is_empty())
            .map(|c| inline_text(c)),
        _ => None,
    }
}

fn label_kind(block: &Block) -> &'static str {
    match block {
        Block::MathBlock(_) => "equation",
        Block::Figure(_) => "figure",
        Block::Table(_) => "table",
        Block::Heading(_) => "section",
        Block::Theorem(_) => "theorem",
        Block::Algorithm(_) => "algorithm",
        Block::CodeBlock(_) => "listing",
        _ => "generic",
    }
}

/// The bookmark for a figure's number (shared by parent and sub-labels).
pub fn figure_bookmark(fig: &Figure) -> Option<String> {
    if let Some(label) = fig.label.as_deref().filter(|l| !l.is_empty()) {
        return Some(sanitize_bookmark(label));
    }
    fig.subfigures
        .iter()
        .find_map(|sub| sub.label.as_deref().filter(|l| !l.is_empty()))
        .map(|l| sanitize_bookmark(&format!("fig_{l}")))
}

fn info(kind: &str, counter_name: &str, bookmark: String, name: Option<String>) -> LabelInfo {
    LabelInfo {
        kind: kind.into(),
        counter_name: counter_name.into(),
        bookmark,
        number: None,
        name,
        sub: None,
    }
}

fn register(block: &Block, labels: &mut Dict<LabelInfo>) {
    if let Some(label) = block.label().filter(|l| !l.is_empty()) {
        let kind = label_kind(block);
        let counter_name = match block {
            Block::Theorem(t) => t
                .counter
                .clone()
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| "Item".into()),
            _ => counter_for(kind).to_string(),
        };
        labels.insert(
            label,
            info(
                kind,
                &counter_name,
                sanitize_bookmark(label),
                label_name(block),
            ),
        );
    }
    // every numbered line of an align/eqnarray that carries a \label is its
    // own referenceable equation (not just the first line).
    if let Block::MathBlock(m) = block {
        for lab in m.line_labels.iter().flatten().filter(|l| !l.is_empty()) {
            labels.insert(
                lab.clone(),
                info("equation", "Equation", sanitize_bookmark(lab), None),
            );
        }
    }
    // sub-figure labels resolve to the parent figure's number.
    if let Block::Figure(f) = block {
        if !f.subfigures.is_empty() {
            if let Some(bookmark) = figure_bookmark(f) {
                if let Some(label) = f.label.as_deref().filter(|l| !l.is_empty()) {
                    labels.insert(label, info("figure", "Figure", bookmark.clone(), None));
                }
                for (i, sub) in f.subfigures.iter().enumerate() {
                    if let Some(label) = sub.label.as_deref().filter(|l| !l.is_empty()) {
                        let mut li = info("figure", "Figure", bookmark.clone(), None);
                        li.sub = char::from_u32('a' as u32 + i as u32).map(|c| c.to_string());
                        labels.insert(label, li);
                    }
                }
            }
        }
    }
}

/// Register every labelled target in one node (table cells, captions,
/// theorem and figure bodies, list items included).
fn collect_node(node: NodeRef<'_>, labels: &mut Dict<LabelInfo>) {
    match node {
        NodeRef::ListItem(item) => {
            if let Some(label) = item.label.as_deref().filter(|l| !l.is_empty()) {
                // \item\label{} -> the item's (Word auto-numbered) list number;
                // the back-end references it with a REF \r field.
                labels.insert(
                    label,
                    info("listitem", "Item", sanitize_bookmark(label), None),
                );
            }
        }
        NodeRef::Block(
            b @ (Block::Heading(_)
            | Block::MathBlock(_)
            | Block::Table(_)
            | Block::Figure(_)
            | Block::Theorem(_)
            | Block::Algorithm(_)
            | Block::CodeBlock(_)),
        ) => register(b, labels),
        _ => {} // a SubFigure is registered with its parent figure
    }
}

/// Point every `Ref` (and `\hyperref` link) in one inline list at its bookmark.
fn rewrite_list(
    inlines: &mut Vec<Inline>,
    labels: &Dict<LabelInfo>,
    report: &mut ConversionReport,
) {
    // (index, letter) to insert after a Ref
    let mut letters: Vec<(usize, String)> = Vec::new();
    for (i, node) in inlines.iter_mut().enumerate() {
        match &mut *node {
            Inline::Ref(r) => {
                let Some(info) = labels.get(&r.key) else {
                    report.warn("\\ref", format!("unresolved reference to '{}'", r.key));
                    continue;
                };
                if r.ref_kind == "subref" {
                    // \subref -> the sub-figure's own letter, "(a)"
                    let text = match &info.sub {
                        Some(s) if !s.is_empty() => format!("({s})"),
                        _ => String::new(),
                    };
                    *node = Inline::text(text);
                    continue;
                }
                if let Some(sub) = info.sub.as_ref().filter(|s| !s.is_empty()) {
                    if r.ref_kind == "generic" || r.ref_kind == "figure" {
                        letters.push((i, sub.clone())); // \ref to a panel -> "1a"
                    }
                }
                if r.ref_kind == "name" {
                    // \nameref -> the target's title as an internal hyperlink
                    let text = info
                        .name
                        .clone()
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| r.key.clone());
                    *node = Inline::Link(Link {
                        inlines: vec![Inline::text(text)],
                        url: String::new(),
                        anchor: Some(info.bookmark.clone()),
                    });
                    continue;
                }
                r.bookmark = Some(info.bookmark.clone());
                if r.ref_kind == "generic" {
                    r.ref_kind = info.kind.clone();
                }
            }
            Inline::Link(link) => {
                if let Some(anchor) = link.anchor.clone() {
                    // \hyperref[label] -> sanitized bookmark
                    match labels.get(&anchor) {
                        None => report
                            .warn("\\hyperref", format!("unresolved \\hyperref to '{anchor}'")),
                        Some(info) => link.anchor = Some(info.bookmark.clone()),
                    }
                }
            }
            _ => {}
        }
    }
    for (i, letter) in letters.into_iter().rev() {
        inlines.insert(i + 1, Inline::text(letter));
    }
}

/// Populate `doc.labels` and wire `Ref` nodes to bookmarks (in place).
pub fn resolve_crossrefs(doc: &mut Document, report: &mut ConversionReport) {
    let mut labels = Dict::new();
    for_each_node(&doc.blocks, |n| collect_node(n, &mut labels));
    meta_for_each_node(&doc.meta, |n| collect_node(n, &mut labels));
    // a target's extra \label{}s share the kept label's bookmark
    for (alias, kept) in doc.label_aliases.iter() {
        if !labels.contains_key(alias) {
            if let Some(info) = labels.get(kept).cloned() {
                labels.insert(alias.clone(), info);
            }
        }
    }
    inline_lists_mut(&mut doc.blocks, |l| rewrite_list(l, &labels, report));
    meta_inline_lists_mut(&mut doc.meta, |l| rewrite_list(l, &labels, report));
    doc.labels = labels;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rules() {
        assert_eq!(sanitize_bookmark("fig:plot 1"), "fig_plot_1");
        assert_eq!(sanitize_bookmark("2eq"), "ref_2eq");
        assert_eq!(sanitize_bookmark(""), "ref_");
        assert_eq!(sanitize_bookmark("é"), "ref__");
        assert_eq!(sanitize_bookmark(&"a".repeat(50)).len(), 40);
    }
}
