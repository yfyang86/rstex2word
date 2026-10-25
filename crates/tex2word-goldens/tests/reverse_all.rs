//! The reverse path on every recorded conversion (`reverse.jsonl`): the
//! `read_docx` IR, `to_latex` with and without reconciliation, and the
//! foreign-document path, against Python's digests.

use tex2word::convert_source;
use tex2word_goldens::{convert_options, load, materialize, root, sha256_hex};
use tex2word_ir::json::{self, DumpOptions, Value};
use tex2word_ir::ToJson;
use tex2word_ooxml::zip::{write, Archive};

const INSERTED: &[u8] = b"<w:p><w:r><w:t>Inserted paragraph.</w:t></w:r></w:p>";

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

fn replace_all(hay: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while let Some(k) = find(hay, from, i) {
        out.extend_from_slice(&hay[i..k]);
        out.extend_from_slice(to);
        i = k + from.len();
    }
    out.extend_from_slice(&hay[i..]);
    out
}

/// The deterministic Word-side edits of `gen_reverse_goldens.py`.
fn edits(xml: &[u8]) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = vec![("prose", replace_all(xml, b" the ", b" teh "))];
    if let Some(start) = find(xml, b"<w:p>", 0) {
        if let Some(end) = find(xml, b"</w:p>", start) {
            out.push(("delete", [&xml[..start], &xml[end + 6..]].concat()));
            out.push((
                "insert",
                [&xml[..end + 6], INSERTED, &xml[end + 6..]].concat(),
            ));
        }
    }
    out
}

fn digest(s: &str) -> Value {
    Value::Str(sha256_hex(s.as_bytes()))
}

fn error(type_name: &str) -> Value {
    Value::Object(vec![("error".into(), Value::Str(type_name.into()))])
}

#[test]
fn reverse_path_matches_python() {
    std::env::set_var("SOURCE_DATE_EPOCH", "1767312000");
    let text = std::fs::read_to_string(root().join("reverse.jsonl")).unwrap();
    let mut failures = Vec::new();
    let mut n = 0;
    for line in text.lines() {
        n += 1;
        let row = json::parse(line).unwrap();
        let case = load(row.get("case").and_then(Value::as_str).unwrap());
        if case
            .tests()
            .iter()
            .any(|t| t.starts_with("tests/test_reproducible.py"))
        {
            std::env::set_var("SOURCE_DATE_EPOCH", "1700000000");
        } else {
            std::env::set_var("SOURCE_DATE_EPOCH", "1767312000");
        }
        let source = case.arg("source").and_then(Value::as_str).unwrap();
        let m = materialize(&case);
        let docx = convert_source(source, &m.base_dir, &convert_options(&case, &m))
            .unwrap()
            .docx;

        let canon = DumpOptions {
            ensure_ascii: false,
            indent: None,
            sort_keys: true,
            compact: false,
        };
        let read = tex2word_frontend::docx_reader::read_docx(&docx, None);
        let got_read = match &read {
            Ok(d) => digest(&json::dumps(&d.to_json(), canon)),
            Err(e) => error(&e.type_name),
        };
        let got_foreign = match &read {
            Ok(d) => match tex2word_latex::write_latex(d) {
                Ok(t) => digest(&t),
                Err(_) => error("KeyError"),
            },
            Err(e) => error(&e.type_name),
        };
        let latex = |reconcile: bool| match tex2word::roundtrip::to_latex(&docx, reconcile) {
            Ok(Some(t)) => digest(&t),
            Ok(None) => Value::Null,
            Err(e) => error(&e.0),
        };
        let checks = [
            ("docx", Value::Str(sha256_hex(&docx))),
            ("read", got_read),
            ("foreign", got_foreign),
            ("latex", latex(true)),
            ("latex_raw", latex(false)),
        ];
        let archive = Archive::new(&docx).unwrap();
        let parts: Vec<(String, Vec<u8>)> = archive
            .names()
            .iter()
            .map(|n| (n.to_string(), archive.read(n).unwrap()))
            .collect();
        let xml = &parts
            .iter()
            .find(|(n, _)| n == "word/document.xml")
            .unwrap()
            .1;
        let mut edited_checks = Vec::new();
        for (kind, new_xml) in edits(xml) {
            let members: Vec<(String, Vec<u8>)> = parts
                .iter()
                .map(|(n, b)| {
                    (
                        n.clone(),
                        if n == "word/document.xml" {
                            new_xml.clone()
                        } else {
                            b.clone()
                        },
                    )
                })
                .collect();
            let edited = write(&members);
            let got = match tex2word::roundtrip::to_latex(&edited, true) {
                Ok(Some(t)) => digest(&t),
                Ok(None) => Value::Null,
                Err(e) => error(&e.0),
            };
            edited_checks.push((format!("edit_{kind}"), got));
        }
        let checks: Vec<(String, Value)> = checks
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .chain(edited_checks)
            .collect();
        for (key, got) in checks {
            let want = row.get(&key).unwrap();
            if &got != want {
                failures.push(format!("{} {:?}: {key}", case.id, case.tests().first()));
            }
        }
    }
    std::env::set_var("SOURCE_DATE_EPOCH", "1767312000");
    assert!(n > 400, "only {n} rows");
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
