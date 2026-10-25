//! The IR model round-trips every IR document recorded from the Python
//! reference: `Document::from_dict(json).to_dict() == json`, and the JSON text
//! re-serialises byte-identically under Python's `json.dumps` settings.

use tex2word_goldens::{json_diff, load_all};
use tex2word_ir::json::{dumps, DumpOptions, Value};
use tex2word_ir::{Document, FromJson, ToJson};

/// Every `{"@type": "Document", …}` value inside a golden case.
fn documents(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(o) => {
            if v.get("@type").and_then(Value::as_str) == Some("Document") {
                out.push(v.clone());
                return;
            }
            for (_, x) in o {
                documents(x, out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| documents(x, out)),
        _ => {}
    }
}

#[test]
fn every_recorded_ir_document_round_trips() {
    let mut checked = 0;
    let mut failures = Vec::new();
    for case in load_all() {
        let mut docs = Vec::new();
        documents(&case.out, &mut docs);
        documents(case.case.get("args").unwrap_or(&Value::Null), &mut docs);
        for d in docs {
            checked += 1;
            match Document::from_json(&d) {
                Err(e) => failures.push(format!("{}: from_dict failed: {e}", case.id)),
                Ok(doc) => {
                    if let Some(diff) = json_diff(&d, &doc.to_json(), "") {
                        failures.push(format!("{}: {diff}", case.id));
                    }
                    // byte-identical under the manifest's json.dumps settings
                    let opts = DumpOptions {
                        ensure_ascii: false,
                        indent: Some(2),
                        sort_keys: true,
                        compact: false,
                    };
                    if dumps(&d, opts) != dumps(&doc.to_json(), opts) {
                        failures.push(format!("{}: re-serialisation differs", case.id));
                    }
                }
            }
        }
    }
    assert!(checked > 600, "only {checked} IR documents found");
    assert!(
        failures.is_empty(),
        "{} of {checked} IR documents failed:\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
