//! Parsing a BibTeX `.bbl` file -- the formatted `.bst` output (port of
//! `bib/bbl.py`). Its `\bibitem` entries become note-bearing CSL items
//! (`_order` keeps the order; `_label` carries an author-year marker).

use std::sync::LazyLock;

use tex2word_ir::pystr;
use tex2word_ir::re::{Flags, Regex};
use tex2word_ir::{CSLItem, Dict, Value};
use tex2word_latexwalker::latex2text;

macro_rules! lazy_re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat));
    };
    ($name:ident, $pat:expr, $flags:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::with_flags($pat, $flags));
    };
}

// \bibitem[label]{key}  (label optional, for author-year styles)
lazy_re!(
    BIBITEM_RE,
    r"\\bibitem\s*(?:\[(?P<label>(?:[^\[\]]|\[[^\]]*\])*)\])?\s*\{(?P<key>[^}]*)\}"
);
// noise commands inside .bbl entries that should become a space
lazy_re!(
    NEWBLOCK_RE,
    r"\\(?:newblock|bibinfo|natexlab|urlprefix|doibase)\b"
);
lazy_re!(CS_RE, r"\\[A-Za-z]+");
lazy_re!(WS_RE, r"\s+");
lazy_re!(THEBIB_RE, r"\\begin\{thebibliography\}.*?\}", Flags::S);

fn clean(text: &str) -> String {
    let text = NEWBLOCK_RE.sub(" ", text);
    let out = match latex2text::latex_to_text(&text) {
        Ok(t) => t,
        Err(_) => CS_RE.sub("", &text).replace(['{', '}'], ""),
    };
    let collapsed = WS_RE.sub(" ", &out);
    let dot = if pystr::strip(&out).is_empty() {
        ""
    } else {
        "."
    };
    format!("{}{dot}", pystr::strip_chars(&collapsed, " .,"))
}

fn thebib_body(text: &str) -> &str {
    let start = THEBIB_RE.search(text).map_or(0, |m| m.end());
    let end = text.find(r"\end{thebibliography}").unwrap_or(text.len());
    text.get(start..end).unwrap_or("")
}

/// Parse a `.bbl` (or inline `thebibliography`) into `{key: CSLItem}`.
pub fn parse_bbl(text: &str) -> Dict<CSLItem> {
    let body = thebib_body(text);
    let mut items = Dict::new();
    let matches = BIBITEM_RE.find_iter(body);
    for (idx, m) in matches.iter().enumerate() {
        let order = idx + 1;
        let key = pystr::strip(m.name("key").unwrap_or(""));
        if key.is_empty() {
            continue;
        }
        let chunk_end = matches.get(order).map_or(body.len(), |n| n.start());
        let raw = body.get(m.end()..chunk_end).unwrap_or("");
        let mut fields = Dict::new();
        fields.insert("note", Value::Str(clean(raw)));
        fields.insert("_order", Value::Int(order as i64));
        if let Some(label) = m.name("label").filter(|l| !l.is_empty()) {
            fields.insert(
                "_label",
                Value::Str(clean(label).trim_end_matches('.').to_string()),
            );
        }
        items.insert(
            key,
            CSLItem {
                id: key.to_string(),
                item_type: "document".into(),
                csl_fields: fields,
            },
        );
    }
    items
}

/// Infer numeric vs author-year from whether entries carry `_label`.
pub fn bbl_style(items: &Dict<CSLItem>) -> String {
    if items
        .values()
        .any(|it| it.csl_fields.contains_key("_label"))
    {
        "author-year".into()
    } else {
        "numeric".into()
    }
}
