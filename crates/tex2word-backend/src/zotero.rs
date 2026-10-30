//! Live Zotero/Mendeley citation fields (port of `bib/zotero.py`; it lives
//! here because it builds on the backend's field primitives).

use std::cell::Cell;

use tex2word_ir::json::{dumps, DumpOptions, Value};
use tex2word_ir::{CSLItem, Cite, Dict};
use tex2word_ooxml::xml::Element;
use tex2word_ooxml::zip::crc32;

use crate::fields;

const CSL_SCHEMA: &str =
    "https://github.com/citation-style-language/schema/raw/master/csl-citation.json";

thread_local! {
    static CITATION_SEQ: Cell<u64> = const { Cell::new(1) };
}

pub fn reset_ids() {
    CITATION_SEQ.with(|c| c.set(1));
}

fn numeric_id(key: &str) -> i64 {
    (crc32(key.as_bytes()) & 0x7FFF_FFFF) as i64
}

fn item_data(item: &CSLItem) -> Value {
    let mut data = Dict::new();
    data.insert("id", Value::Str(item.id.clone()));
    data.insert("type", Value::Str(item.item_type.clone()));
    for (k, v) in item.csl_fields.iter() {
        data.insert(k.clone(), v.clone());
    }
    Value::Object(data.into_iter().collect())
}

fn citation_item(cite: &Cite, key: &str, item: &CSLItem) -> Value {
    let mut out = vec![
        ("id".to_string(), Value::Int(numeric_id(key))),
        (
            "uris".to_string(),
            Value::Array(vec![Value::Str(format!(
                "http://zotero.org/users/local/tex2word/items/{key}"
            ))]),
        ),
        ("itemData".to_string(), item_data(item)),
    ];
    if let Some(s) = cite.suffix.as_deref().filter(|s| !s.is_empty()) {
        // Zotero parses locators
        out.push(("locator".into(), Value::Str(s.replace('\u{a0}', " "))));
    }
    if let Some(p) = cite.prefix.as_deref().filter(|p| !p.is_empty()) {
        out.push(("prefix".into(), Value::Str(p.into())));
    }
    Value::Object(out)
}

/// The runs for one `ADDIN ZOTERO_ITEM CSL_CITATION` field.
pub fn citation_field(cite: &Cite, items: &Dict<CSLItem>, rendered: &str) -> Vec<Element> {
    let seq = CITATION_SEQ.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    let citation = Value::Object(vec![
        ("citationID".into(), Value::Str(format!("l2w{seq}"))),
        (
            "properties".into(),
            Value::Object(vec![
                ("formattedCitation".into(), Value::Str(rendered.into())),
                ("plainCitation".into(), Value::Str(rendered.into())),
                ("noteIndex".into(), Value::Int(0)),
            ]),
        ),
        (
            "citationItems".into(),
            Value::Array(
                cite.keys
                    .iter()
                    .filter_map(|k| items.get(k).map(|it| citation_item(cite, k, it)))
                    .collect(),
            ),
        ),
        ("schema".into(), Value::Str(CSL_SCHEMA.into())),
    ]);
    let payload = dumps(
        &citation,
        DumpOptions {
            ensure_ascii: false,
            indent: None,
            sort_keys: false,
            compact: true,
        },
    );
    fields::field(
        &format!("ADDIN ZOTERO_ITEM CSL_CITATION {payload}"),
        if rendered.is_empty() { " " } else { rendered },
    )
}

/// The runs for the `CSL_BIBLIOGRAPHY` field instruction.
pub fn bibliography_field_runs(_rendered_lines: &[String]) -> Vec<Element> {
    let code = r#"ADDIN ZOTERO_BIBL {"uncited":[],"omitted":[],"custom":[]} CSL_BIBLIOGRAPHY"#;
    fields::field(code, "")
}
