//! Generic IR traversal in reading order (port of `ir.node_lists` /
//! `ir.iter_nodes`).
//!
//! Fields are visited in declaration order, except that a table's caption
//! (set above the tabular in LaTeX) is read before its rows and a figure's
//! caption after its body. Passes over `Ref`/`Cite` leaves use this so they
//! never miss a container: table cells, captions, theorem bodies, styled
//! spans, notes and the document metadata.

use crate::nodes::*;

/// A node handed to [`for_each_node`]: the ones passes care about by type,
/// everything else as `Other`.
pub enum NodeRef<'a> {
    Block(&'a Block),
    ListItem(&'a ListItem),
    SubFigure(&'a SubFigure),
    Inline(&'a Inline),
    Other,
}

/// What a traversal does at each stop.
trait Visit {
    /// Every list of inline nodes, before its elements are descended into
    /// (so items can be replaced in place). Empty lists are skipped, as
    /// Python yields only lists that hold a node.
    fn inline_list(&mut self, _list: &mut Vec<Inline>) {}
    /// Every node, right before its own children.
    fn node(&mut self, _node: NodeRef<'_>) {}
}

fn inlines<V: Visit>(list: &mut Vec<Inline>, v: &mut V) {
    if list.is_empty() {
        return;
    }
    v.inline_list(list);
    for item in list.iter_mut() {
        v.node(NodeRef::Inline(item));
        match item {
            Inline::Emphasis(n) => inlines(&mut n.inlines, v),
            Inline::Link(n) => inlines(&mut n.inlines, v),
            Inline::Footnote(n) => {
                inlines(&mut n.inlines, v);
                blocks(&mut n.blocks, v);
            }
            Inline::Endnote(n) => inlines(&mut n.inlines, v),
            Inline::Colored(n) => inlines(&mut n.inlines, v),
            Inline::FontSize(n) => inlines(&mut n.inlines, v),
            _ => {}
        }
    }
}

fn opt_inlines<V: Visit>(list: &mut Option<Vec<Inline>>, v: &mut V) {
    if let Some(l) = list {
        inlines(l, v);
    }
}

fn image<V: Visit>(img: &mut Option<Image>, v: &mut V) {
    if img.is_some() {
        v.node(NodeRef::Other);
    }
}

fn blocks<V: Visit>(list: &mut [Block], v: &mut V) {
    for block in list.iter_mut() {
        v.node(NodeRef::Block(block));
        match block {
            Block::Heading(h) => inlines(&mut h.inlines, v),
            Block::Paragraph(p) => inlines(&mut p.inlines, v),
            Block::ItemList(l) => {
                for item in l.items.iter_mut() {
                    v.node(NodeRef::ListItem(item));
                    blocks(&mut item.blocks, v);
                    opt_inlines(&mut item.term, v);
                }
            }
            Block::Table(t) => {
                opt_inlines(&mut t.caption, v); // caption first: above the tabular
                for row in t.rows.iter_mut() {
                    v.node(NodeRef::Other);
                    for cell in row.cells.iter_mut() {
                        v.node(NodeRef::Other);
                        blocks(&mut cell.blocks, v);
                    }
                }
            }
            Block::Figure(f) => {
                image(&mut f.image, v);
                for sub in f.subfigures.iter_mut() {
                    v.node(NodeRef::SubFigure(sub));
                    image(&mut sub.image, v);
                    opt_inlines(&mut sub.caption, v);
                }
                blocks(&mut f.blocks, v);
                opt_inlines(&mut f.caption, v); // caption last: below the body
            }
            Block::CodeBlock(c) => opt_inlines(&mut c.caption, v),
            Block::Quote(q) => {
                blocks(&mut q.blocks, v);
                opt_inlines(&mut q.title, v);
            }
            Block::Theorem(t) => {
                blocks(&mut t.blocks, v);
                opt_inlines(&mut t.title, v);
            }
            Block::Algorithm(a) => {
                for line in a.lines.iter_mut() {
                    v.node(NodeRef::Other);
                    inlines(&mut line.inlines, v);
                }
                opt_inlines(&mut a.caption, v);
            }
            Block::Bibliography(b) => {
                for _ in &b.entries {
                    v.node(NodeRef::Other);
                }
            }
            Block::MathBlock(_)
            | Block::TableOfContents(_)
            | Block::Index(_)
            | Block::RawPassthrough(_) => {}
        }
    }
}

fn meta<V: Visit>(m: &mut DocumentMeta, v: &mut V) {
    opt_inlines(&mut m.title, v);
    for a in m.authors.iter_mut() {
        inlines(a, v);
    }
    opt_inlines(&mut m.date, v);
    if let Some(abs) = m.abstract_.as_mut() {
        blocks(abs, v);
    }
    opt_inlines(&mut m.keywords, v);
    for a in m.affiliations.iter_mut() {
        inlines(a, v);
    }
    opt_inlines(&mut m.subtitle, v);
    for n in m.front_notes.iter_mut() {
        inlines(n, v);
    }
}

struct Lists<F>(F);
impl<F: FnMut(&mut Vec<Inline>)> Visit for Lists<F> {
    fn inline_list(&mut self, list: &mut Vec<Inline>) {
        (self.0)(list)
    }
}

struct Nodes<F>(F);
impl<F: FnMut(NodeRef<'_>)> Visit for Nodes<F> {
    fn node(&mut self, node: NodeRef<'_>) {
        (self.0)(node)
    }
}

/// `ir.node_lists(blocks)` restricted to inline lists: `f` sees every
/// non-empty `Vec<Inline>` below `blocks`, in reading order, before its
/// elements are descended into.
pub fn inline_lists_mut(list: &mut [Block], f: impl FnMut(&mut Vec<Inline>)) {
    blocks(list, &mut Lists(f));
}

/// [`inline_lists_mut`] over the document metadata.
pub fn meta_inline_lists_mut(m: &mut DocumentMeta, f: impl FnMut(&mut Vec<Inline>)) {
    meta(m, &mut Lists(f));
}

/// `ir.iter_nodes(blocks)`: every node below `blocks`, depth-first in reading
/// order, a node right before its own children.
pub fn for_each_node(list: &[Block], f: impl FnMut(NodeRef<'_>)) {
    // the walk is shared with the mutable traversals; it never mutates
    let mut owned = list.to_vec();
    blocks(&mut owned, &mut Nodes(f));
}

/// [`for_each_node`] over the document metadata.
pub fn meta_for_each_node(m: &DocumentMeta, f: impl FnMut(NodeRef<'_>)) {
    let mut owned = m.clone();
    meta(&mut owned, &mut Nodes(f));
}
