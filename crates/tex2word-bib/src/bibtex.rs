//! A small BibTeX parser producing CSL-JSON items (port of `bib/bibtex.py`).
//!
//! Not a full bibtex implementation: the common entry/field grammar
//! (`@type{key, field = {value} | "value" | bare, ...}`), brace-aware values,
//! `@string` macros, and a heuristic bibtex->CSL field/type mapping.

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

/// bibtex entry type -> CSL type
fn csl_type(entry_type: &str) -> &'static str {
    match entry_type {
        "article" => "article-journal",
        "book" | "proceedings" | "manual" => "book",
        "inbook" | "incollection" => "chapter",
        "inproceedings" | "conference" => "paper-conference",
        "phdthesis" | "mastersthesis" => "thesis",
        "techreport" => "report",
        "misc" => "document",
        "unpublished" => "manuscript",
        "online" | "electronic" => "webpage",
        "booklet" => "pamphlet",
        _ => "document",
    }
}

/// bibtex field -> CSL field (simple string fields)
fn csl_field(name: &str) -> Option<&'static str> {
    Some(match name {
        "title" => "title",
        "journal" | "booktitle" => "container-title",
        "publisher" | "school" | "institution" => "publisher",
        "volume" => "volume",
        "number" => "issue",
        "pages" => "page",
        "doi" => "DOI",
        "url" => "URL",
        "edition" => "edition",
        "series" => "collection-title",
        "note" => "note",
        "abstract" => "abstract",
        "address" => "publisher-place",
        "isbn" => "ISBN",
        "issn" => "ISSN",
        "chapter" => "chapter-number",
        _ => return None,
    })
}

lazy_re!(WS_RE, r"\s+");

/// Convert a bibtex field value to plain Unicode (accents, dashes, &, ...).
pub fn strip_braces(value: &str) -> String {
    let text = match latex2text::latex_to_text(value) {
        Ok(t) => t,
        Err(_) => value.replace(['{', '}'], "").replace("\\&", "&"),
    };
    pystr::strip(&WS_RE.sub(" ", &text)).to_string()
}

lazy_re!(AND_RE, r"\s+and\s+");

fn name(parts: &[(&str, String)]) -> Value {
    Value::Object(parts.iter().map(|(k, v)| (k.to_string(), Value::Str(v.clone()))).collect())
}

/// Parse a bibtex `author`/`editor` field into CSL name parts.
fn split_names(raw: &str) -> Vec<Value> {
    let mut names = Vec::new();
    for chunk in AND_RE.split_str(pystr::strip(raw)) {
        let chunk = strip_braces(chunk);
        if chunk.is_empty() {
            continue;
        }
        if let Some((family, given)) = chunk.split_once(',') {
            names.push(name(&[
                ("family", pystr::strip(family).to_string()),
                ("given", pystr::strip(given).to_string()),
            ]));
        } else {
            let parts = pystr::split_ws(&chunk);
            if parts.len() == 1 {
                names.push(name(&[("family", parts[0].to_string())]));
            } else {
                names.push(name(&[
                    ("family", parts[parts.len() - 1].to_string()),
                    ("given", parts[..parts.len() - 1].join(" ")),
                ]));
            }
        }
    }
    names
}

lazy_re!(FIELD_NAME_RE, r"\s*([A-Za-z][A-Za-z0-9_+-]*)\s*=\s*");

/// Parse `field = value, ...` honouring {} and "" delimiters.
fn tokenize_entry_body(body: &str) -> Dict<String> {
    let mut fields = Dict::new();
    let b = body.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        let Some(m) = FIELD_NAME_RE.match_start(&body[i..]) else {
            break;
        };
        let name = pystr::lower(m.group_str(1));
        i += m.end();
        if i >= n {
            break;
        }
        let value;
        if b[i] == b'{' {
            let mut depth = 0;
            let mut j = i;
            while j < n {
                if b[j] == b'{' {
                    depth += 1;
                } else if b[j] == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                j += 1;
            }
            value = &body[i + 1..j];
            i = j + 1;
        } else if b[i] == b'"' {
            let mut j = i + 1;
            while j < n && b[j] != b'"' {
                j += 1;
            }
            value = &body[i + 1..j];
            i = j + 1;
        } else {
            let mut j = i;
            while j < n && b[j] != b',' && b[j] != b'\n' {
                j += 1;
            }
            value = &body[i..j];
            i = j;
        }
        fields.insert(name, value.to_string());
        // consume trailing comma/whitespace
        while i < n && matches!(b[i], b',' | b' ' | b'\n' | b'\t' | b'\r') {
            i += 1;
        }
    }
    fields
}

lazy_re!(DATE_YEAR_RE, r"\s*(\d{4})");
lazy_re!(YEAR_RE, r"\d{4}");

fn issued(year: Value) -> Value {
    Value::Object(vec![("date-parts".into(), Value::Array(vec![Value::Array(vec![year])]))])
}

fn int_or(value: &str) -> Value {
    match YEAR_RE.search(value).and_then(|m| pystr::parse_int(m.as_str())) {
        Some(y) => Value::Int(y),
        None => Value::Str(strip_braces(value)),
    }
}

fn to_csl(entry_type: &str, key: &str, raw_fields: &Dict<String>) -> CSLItem {
    let mut out: Dict<Value> = Dict::new();
    for (name, value) in raw_fields.iter() {
        match name.as_str() {
            "author" => out.insert("author", Value::Array(split_names(value))),
            "editor" => out.insert("editor", Value::Array(split_names(value))),
            "year" => {
                out.set_default("issued", Value::Object(Vec::new()));
                out.insert("issued", issued(int_or(value)));
            }
            "month" => {}
            "date" => {
                if let Some(y) = DATE_YEAR_RE.match_start(value).and_then(|m| pystr::parse_int(m.group_str(1))) {
                    out.insert("issued", issued(Value::Int(y)));
                }
            }
            other => {
                if let Some(f) = csl_field(other) {
                    out.insert(f, Value::Str(strip_braces(value)));
                }
            }
        }
    }
    apply_eprint(&mut out, raw_fields);
    CSLItem {
        id: key.to_string(),
        item_type: csl_type(entry_type).to_string(),
        csl_fields: out,
    }
}

/// Make a biblatex `eprint` resolvable: arXiv eprints become an abs URL.
fn apply_eprint(out: &mut Dict<Value>, raw_fields: &Dict<String>) {
    let eprint = strip_braces(raw_fields.get("eprint").map_or("", String::as_str));
    let eprint = pystr::strip(&eprint);
    if eprint.is_empty() {
        return;
    }
    let prefix = raw_fields
        .get("archiveprefix")
        .filter(|s| !s.is_empty())
        .or_else(|| raw_fields.get("eprinttype").filter(|s| !s.is_empty()))
        .map_or(String::new(), |s| pystr::lower(pystr::strip(s)));
    let is_arxiv = prefix.is_empty() || prefix == "arxiv";
    if !out.contains_key("URL") && !out.contains_key("DOI") && is_arxiv {
        out.insert("URL", Value::Str(format!("https://arxiv.org/abs/{eprint}")));
    }
    let label = if is_arxiv { format!("arXiv:{eprint}") } else { eprint.to_string() };
    if !out.contains_key("note") {
        out.insert("note", Value::Str(label));
    }
}

lazy_re!(
    STRING_RE,
    r#"@string\s*\{\s*([A-Za-z0-9_]+)\s*=\s*["{](.+?)["}]\s*\}"#,
    Flags::I | Flags::S
);
lazy_re!(ENTRY_RE, r"@(\w+)\s*\{");

/// Parse BibTeX `source` into `{citekey: CSLItem}` (insertion order).
pub fn parse_bibtex(source: &str) -> Dict<CSLItem> {
    // expand simple @string macros
    let mut macros: Dict<String> = Dict::new();
    for sm in STRING_RE.find_iter(source) {
        macros.insert(pystr::lower(sm.group_str(1)), sm.group_str(2).to_string());
    }
    let mut items = Dict::new();
    let b = source.as_bytes();
    for m in ENTRY_RE.find_iter(source) {
        let entry_type = pystr::lower(m.group_str(1));
        if matches!(entry_type.as_str(), "string" | "comment" | "preamble") {
            continue;
        }
        let start = m.end();
        let mut depth = 1;
        let mut j = start;
        while j < b.len() && depth != 0 {
            if b[j] == b'{' {
                depth += 1;
            } else if b[j] == b'}' {
                depth -= 1;
            }
            j += 1;
        }
        // source[start : j - 1] (one code point back)
        let mut stop = j;
        if stop > 0 {
            stop -= 1;
            while !source.is_char_boundary(stop) {
                stop -= 1;
            }
        }
        let inner = if stop > start { &source[start..stop] } else { "" };
        let (key, body) = inner.split_once(',').unwrap_or((inner, ""));
        let key = pystr::strip(key);
        if key.is_empty() {
            continue;
        }
        let mut raw_fields = tokenize_entry_body(body);
        // substitute @string macros for bare values
        let names: Vec<String> = raw_fields.keys().cloned().collect();
        for fname in names {
            let fval = pystr::lower(pystr::strip(raw_fields.get(&fname).unwrap()));
            if let Some(v) = macros.get(&fval).cloned() {
                raw_fields.insert(fname, v);
            }
        }
        items.insert(key, to_csl(&entry_type, key, &raw_fields));
    }
    items
}
