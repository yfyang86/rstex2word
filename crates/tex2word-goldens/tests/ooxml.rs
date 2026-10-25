//! The ooxml crate against Python: lxml's parse + re-serialize of every
//! recorded XML blob (`xml.jsonl`), and reading every recorded `.docx`
//! (written by Python's zipfile/zlib).

use tex2word_goldens::{blob, load_all, root, sha256_hex};
use tex2word_ir::json::{self, Value};
use tex2word_ooxml::parse::fromstring;
use tex2word_ooxml::xml::{tostring, ToString};
use tex2word_ooxml::zip::Archive;

#[test]
fn xml_round_trip_matches_lxml() {
    let text = std::fs::read_to_string(root().join("xml.jsonl")).unwrap();
    let mut failures = Vec::new();
    let mut n = 0;
    for line in text.lines() {
        n += 1;
        let row = json::parse(line).unwrap();
        let sha = row.get("sha").and_then(Value::as_str).unwrap();
        let data = blob(sha);
        let parsed = fromstring(&data);
        if row.get("error").is_some() {
            if parsed.is_ok() {
                failures.push(format!("{sha}: lxml rejects it, Rust parsed it"));
            }
            continue;
        }
        let r = match parsed {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{sha}: {e}"));
                continue;
            }
        };
        let utf8 = tostring(&r, ToString { utf8: true, declaration: true, with_tail: true }).unwrap();
        if sha256_hex(utf8.as_bytes()) != row.get("out").and_then(Value::as_str).unwrap() {
            failures.push(format!("{sha}: UTF-8 serialization differs"));
        }
        let ascii = tostring(&r, ToString { utf8: false, declaration: false, with_tail: true }).unwrap();
        if sha256_hex(ascii.as_bytes()) != row.get("ascii").and_then(Value::as_str).unwrap() {
            failures.push(format!("{sha}: ASCII serialization differs"));
        }
    }
    assert!(n > 300, "only {n} rows");
    assert!(failures.is_empty(), "{} of {n} differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn reads_python_written_docx() {
    let mut n = 0;
    for case in load_all() {
        let Some(sha) = case.arg("docx_bytes").and_then(|v| v.get("@blob")).and_then(Value::as_str) else {
            continue;
        };
        let data = blob(sha);
        let Ok(a) = Archive::new(&data) else {
            continue; // deliberately broken inputs (validate tests)
        };
        for name in a.names() {
            a.read(name).unwrap_or_else(|e| panic!("{}: {name}: {e}", case.id));
        }
        n += 1;
    }
    assert!(n > 20, "only {n} archives");
}
