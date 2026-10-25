//! CSL-JSON -> formatted reference strings + in-text citation resolution
//! (port of `bib/render.py`).
//!
//! A pragmatic static formatter supporting two styles: `numeric` ([1], [2],
//! ...) and `author-year` ((Author, 2020)). The optional `--csl` engine
//! (citeproc-py) is outside the core install; asking for it warns and falls
//! back to this formatter, as the reference does without the extra.

use tex2word_ir::pystr;
use tex2word_ir::{
    Bibliography, Block, CSLItem, Cite, ConversionReport, Dict, Document, Inline, Value,
};

/// bibliographystyle -> one of the two supported style families.
pub fn style_family(bibstyle: &str) -> String {
    let name = pystr::lower(pystr::strip(bibstyle));
    if name == "numeric" || name == "author-year" {
        return name; // already a family (idempotent)
    }
    match name.as_str() {
        "plainnat" | "apalike" | "abbrvnat" | "agsm" | "harvard" | "chicago" | "apa" => {
            "author-year"
        }
        _ => "numeric",
    }
    .into()
}

/// The resolved bibliography (`render.Bibliography`).
#[derive(Debug, Clone, Default)]
pub struct Biblio {
    pub entries: Vec<CSLItem>,
    pub style: String,
    /// citekey -> in-text label (e.g. "1" or "Smith, 2020")
    pub labels: Dict<String>,
    /// citekey -> 1-based position (numeric ordering)
    pub numbers: Dict<i64>,
}

// --------------------------------------------------------------------------- //
// Name / field helpers
// --------------------------------------------------------------------------- //

fn names<'a>(item: &'a CSLItem, role: &str) -> &'a [Value] {
    item.csl_fields
        .get(role)
        .and_then(Value::as_array)
        .unwrap_or(&[])
}

pub(crate) fn year(item: &CSLItem) -> String {
    if let Some(issued) = item
        .csl_fields
        .get("issued")
        .filter(|v| v.as_object().is_some())
    {
        if let Some(parts) = issued.get("date-parts").filter(|p| p.py_truthy()) {
            if let Some(first) = parts
                .as_array()
                .and_then(|a| a.first())
                .filter(|f| f.py_truthy())
            {
                if let Some(v) = first.as_array().and_then(|a| a.first()) {
                    return v.py_str();
                }
            }
        }
    }
    "n.d.".into()
}

fn name_part(name: &Value, key: &str) -> String {
    name.get(key).map_or(String::new(), Value::py_str)
}

fn family(name: &Value) -> String {
    pystr::strip(&name_part(name, "family")).to_string()
}

fn initials(given: &str) -> String {
    pystr::split_ws(given)
        .into_iter()
        .map(|p| format!("{}.", p.chars().next().unwrap()))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn author_label(item: &CSLItem) -> String {
    let ns = names(item, "author");
    match ns.len() {
        0 => item.id.clone(),
        1 => family(&ns[0]),
        2 => format!("{} & {}", family(&ns[0]), family(&ns[1])),
        _ => format!("{} et al.", family(&ns[0])),
    }
}

fn format_authors(item: &CSLItem) -> String {
    let rendered: Vec<String> = names(item, "author")
        .iter()
        .map(|n| {
            let fam = family(n);
            let given = name_part(n, "given");
            if given.is_empty() {
                fam
            } else {
                let s = format!("{fam}, {}", initials(&given));
                pystr::strip(&s).trim_end_matches(',').to_string()
            }
        })
        .collect();
    match rendered.len() {
        0 => String::new(),
        1 => rendered[0].clone(),
        n => format!("{} & {}", rendered[..n - 1].join(", "), rendered[n - 1]),
    }
}

// --------------------------------------------------------------------------- //
// Reference list formatting
// --------------------------------------------------------------------------- //

/// Render a single CSL item as a formatted reference string.
pub fn format_reference(item: &CSLItem) -> String {
    let f = &item.csl_fields;
    let get = |k: &str| f.get(k).filter(|v| v.py_truthy());
    // a real CSL engine may have pre-formatted this entry
    if let Some(v) = get("_formatted") {
        return pystr::strip(&v.py_str()).to_string();
    }
    // thebibliography \bibitem entries carry only free-text 'note'
    if names(item, "author").is_empty() && !f.contains_key("title") {
        if let Some(note) = f.get("note") {
            return pystr::strip(&note.py_str()).to_string();
        }
    }
    let mut parts: Vec<String> = Vec::new();
    let authors = format_authors(item);
    if authors.is_empty() {
        parts.push(format!("({}).", year(item)));
    } else {
        parts.push(format!("{authors} ({}).", year(item)));
    }
    if let Some(title) = get("title") {
        parts.push(format!("{}.", title.py_str()));
    }
    if let Some(container) = get("container-title") {
        let mut detail = container.py_str();
        if let Some(v) = get("volume") {
            detail.push_str(&format!(", {}", v.py_str()));
        }
        if let Some(v) = get("issue") {
            detail.push_str(&format!("({})", v.py_str()));
        }
        if let Some(v) = get("page") {
            detail.push_str(&format!(", {}", v.py_str()));
        }
        parts.push(format!("{detail}."));
    } else if let Some(p) = get("publisher") {
        parts.push(format!("{}.", p.py_str()));
    }
    if let Some(doi) = get("DOI") {
        parts.push(format!("https://doi.org/{}", doi.py_str()));
    } else if let Some(url) = get("URL") {
        parts.push(url.py_str());
    }
    let joined: Vec<&str> = parts
        .iter()
        .map(String::as_str)
        .filter(|p| !p.is_empty())
        .collect();
    pystr::strip(&joined.join(" ")).to_string()
}

// --------------------------------------------------------------------------- //
// In-text citation rendering
// --------------------------------------------------------------------------- //

fn cite_one(key: &str, biblio: &Biblio) -> String {
    biblio
        .labels
        .get(key)
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// Render an in-text marker for a `Cite` node against the bibliography.
pub fn render_citation(cite: &Cite, biblio: &Biblio) -> String {
    let index = |k: &str| biblio.entries.iter().rev().find(|e| e.id == k);
    // author-/year-only modes are style-independent
    match cite.mode.as_str() {
        "author" => {
            return cite
                .keys
                .iter()
                .map(|k| index(k).map_or_else(|| k.clone(), author_label))
                .collect::<Vec<_>>()
                .join(", ")
        }
        "year" => {
            return cite
                .keys
                .iter()
                .map(|k| index(k).map_or_else(|| k.clone(), year))
                .collect::<Vec<_>>()
                .join(", ")
        }
        "num" => {
            return cite
                .keys
                .iter()
                .map(|k| {
                    biblio
                        .numbers
                        .get(k)
                        .map_or_else(|| "?".to_string(), i64::to_string)
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
        _ => {}
    }
    let pre = cite
        .prefix
        .as_deref()
        .filter(|p| !p.is_empty())
        .map_or(String::new(), |p| format!("{p} "));
    let post = cite
        .suffix
        .as_deref()
        .filter(|s| !s.is_empty())
        .map_or(String::new(), |s| format!(", {s}"));
    if biblio.style == "numeric" {
        let inner: Vec<String> = cite.keys.iter().map(|k| cite_one(k, biblio)).collect();
        return format!("[{pre}{}{post}]", inner.join(", "));
    }
    // author-year
    if cite.mode == "text" {
        let rendered: Vec<String> = cite
            .keys
            .iter()
            .map(|k| match index(k) {
                None => k.clone(),
                Some(item) => format!("{} ({}{post})", author_label(item), year(item)),
            })
            .collect();
        return rendered.join(", ");
    }
    let inner: Vec<String> = cite.keys.iter().map(|k| cite_one(k, biblio)).collect();
    format!("({pre}{}{post})", inner.join(", "))
}

// --------------------------------------------------------------------------- //
// Assembly
// --------------------------------------------------------------------------- //

/// Build the bibliography for the cited keys in the chosen style. A `_order`
/// (from a `.bbl`/`thebibliography`) fixes the order; a `_label` overrides
/// the computed in-text label.
pub fn build_bibliography(items: &Dict<CSLItem>, cited_keys: &[String], style: &str) -> Biblio {
    let numeric = style == "numeric";
    let known: Vec<&String> = cited_keys
        .iter()
        .filter(|k| items.contains_key(k))
        .collect();
    let item = |k: &str| items.get(k).unwrap();
    let has_order = known
        .iter()
        .any(|k| item(k).csl_fields.contains_key("_order"));
    let mut ordered: Vec<&String> = known.clone();
    if has_order {
        let order_of = |k: &str| {
            item(k)
                .csl_fields
                .get("_order")
                .and_then(Value::as_f64)
                .unwrap_or((1u64 << 30) as f64)
        };
        ordered.sort_by(|a, b| {
            order_of(a)
                .partial_cmp(&order_of(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    } else if !numeric {
        ordered.sort_by_key(|k| (pystr::lower(&author_label(item(k))), year(item(k))));
    }
    let mut biblio = Biblio {
        style: style.to_string(),
        ..Biblio::default()
    };
    for (i, key) in ordered.iter().enumerate() {
        let it = item(key);
        let n = i as i64 + 1;
        biblio.entries.push(it.clone());
        biblio.numbers.insert(key.as_str(), n);
        let label = match it.csl_fields.get("_label").filter(|v| v.py_truthy()) {
            Some(explicit) => explicit.py_str(),
            None if numeric => n.to_string(),
            None => format!("{}, {}", author_label(it), year(it)),
        };
        biblio.labels.insert(key.as_str(), label);
    }
    biblio
}

/// Visit every `Cite` in body order (paragraphs/headings, quotes, figure
/// bodies, list items; through emphasis, links and footnotes).
fn walk_cites(blocks: &mut [Block], f: &mut dyn FnMut(&mut Cite)) {
    fn inl(inlines: &mut [Inline], f: &mut dyn FnMut(&mut Cite)) {
        for n in inlines {
            match n {
                Inline::Cite(c) => f(c),
                Inline::Emphasis(e) => inl(&mut e.inlines, f),
                Inline::Link(l) => inl(&mut l.inlines, f),
                Inline::Footnote(x) => inl(&mut x.inlines, f),
                _ => {}
            }
        }
    }
    for block in blocks {
        match block {
            Block::Paragraph(p) => inl(&mut p.inlines, f),
            Block::Heading(h) => inl(&mut h.inlines, f),
            Block::Quote(q) => walk_cites(&mut q.blocks, f),
            Block::Figure(fig) if !fig.blocks.is_empty() => walk_cites(&mut fig.blocks, f),
            Block::ItemList(l) => {
                for item in l.items.iter_mut() {
                    walk_cites(&mut item.blocks, f);
                }
            }
            _ => {}
        }
    }
}

/// Resolve cites, fill `Cite.rendered`, and populate the Bibliography block.
pub fn resolve_citations(
    doc: &mut Document,
    items: &Dict<CSLItem>,
    bibstyle: &str,
    report: &mut ConversionReport,
    csl_path: Option<&str>,
    nocite_keys: &[String],
) -> Biblio {
    let style = style_family(bibstyle);
    let mut cites: Vec<Cite> = Vec::new();
    walk_cites(&mut doc.blocks, &mut |c| cites.push(c.clone()));

    let mut cited_order: Vec<String> = Vec::new();
    for cite in &cites {
        for key in &cite.keys {
            if !cited_order.contains(key) {
                cited_order.push(key.clone());
            }
            if !items.contains_key(key) {
                report.warn("\\cite", format!("unknown citation key '{key}'"));
            }
        }
    }
    // \nocite{key} / \nocite{*}: in the reference list, no in-text cite
    for key in nocite_keys {
        let targets: Vec<String> = if key == "*" {
            items.keys().cloned().collect()
        } else {
            vec![key.clone()]
        };
        for k in targets {
            if items.contains_key(&k) && !cited_order.contains(&k) {
                cited_order.push(k);
            }
        }
    }
    if csl_path.is_some_and(|p| !p.is_empty()) {
        // the citeproc engine is an optional extra the core install lacks
        report.warn(
            "--csl",
            "citeproc-py not installed; run `uv sync --extra csl`",
        );
    }
    let biblio = build_bibliography(items, &cited_order, &style);
    walk_cites(&mut doc.blocks, &mut |c| {
        c.rendered = Some(render_citation(c, &biblio))
    });
    fill_biblio_block(doc, &biblio.entries, &style);
    biblio
}

/// Fill a `\bibliography` placeholder block, else append one at the end.
fn fill_biblio_block(doc: &mut Document, entries: &[CSLItem], style: &str) {
    if let Some(Block::Bibliography(b)) = doc
        .blocks
        .iter_mut()
        .find(|b| matches!(b, Block::Bibliography(_)))
    {
        b.entries = entries.to_vec();
        b.style = style.into();
    } else if !entries.is_empty() {
        doc.blocks.push(Block::Bibliography(Bibliography {
            entries: entries.to_vec(),
            style: style.into(),
        }));
    }
}
