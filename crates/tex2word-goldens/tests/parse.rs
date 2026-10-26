//! `parse_document` reproduces the Python reference: the IR and the report,
//! on every recorded `parse_document` call and on the parse step of every
//! recorded `convert_source` call (`parse.jsonl`).

use tex2word_goldens::{json_diff, load, load_all, materialize, root, Case};
use tex2word_ir::json::{self, Value};
use tex2word_ir::ToJson;

/// Run `parse_document` for a case and diff the IR and report against the
/// expected values; `None` when they match.
fn check(case: &Case, want_ir: &Value, want_report: &Value) -> Option<String> {
    let source = case.arg("source").and_then(Value::as_str)?.to_string();
    let m = materialize(case);
    let csl = case.arg("csl_path").and_then(Value::as_str);
    let (doc, report) = match tex2word_frontend::parser::parse_document(&source, &m.base_dir, csl) {
        Ok(r) => r,
        Err(e) => return Some(format!("error: {e}")),
    };
    let got_ir =
        json::parse(&m.to_placeholders(&json::dumps(&doc.to_json(), Default::default()))).unwrap();
    if let Some(d) = json_diff(want_ir, &got_ir, "ir") {
        return Some(d);
    }
    let got_report =
        json::parse(&m.to_placeholders(&json::dumps(&report.to_value(), Default::default())))
            .unwrap();
    json_diff(want_report, &got_report, "report")
}

#[test]
fn parse_document_matches_python() {
    // \today is frozen to the capture date (2026-01-02)
    std::env::set_var("SOURCE_DATE_EPOCH", "1767312000");
    let mut failures = Vec::new();
    let mut n = 0;
    for case in load_all().into_iter().filter(|c| c.api == "parse_document") {
        n += 1;
        let (Some(ir), Some(rep)) = (case.out.get("ir"), case.out.get("report")) else {
            continue;
        };
        if let Some(d) = check(&case, ir, rep) {
            failures.push(format!("{} {:?}: {d}", case.id, case.tests().first()));
        }
    }
    let text = std::fs::read_to_string(root().join("parse.jsonl")).expect("parse.jsonl");
    for line in text.lines() {
        n += 1;
        let row = json::parse(line).unwrap();
        let case = load(row.get("case").and_then(Value::as_str).unwrap());
        if let Some(d) = check(&case, row.get("ir").unwrap(), row.get("report").unwrap()) {
            failures.push(format!("{} {:?}: {d}", case.id, case.tests().first()));
        }
    }
    assert!(n > 500, "only {n} parse cases");
    assert!(
        failures.is_empty(),
        "{} of {n} cases differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
