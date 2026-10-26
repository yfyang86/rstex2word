//! The report helpers (`to_json`, `summary`, `to_html`, `aggregate_html`)
//! against Python on every recorded conversion report (`report.jsonl`).

use tex2word_goldens::{load, root, sha256_hex};
use tex2word_ir::json::{self, Value};
use tex2word_ir::report::aggregate_html;
use tex2word_ir::{ConversionReport, ReportEntry};

fn rebuild(rec: &Value) -> ConversionReport {
    let cov = rec.get("coverage").unwrap();
    let n = |k: &str| cov.get(k).and_then(Value::as_i64).unwrap();
    ConversionReport {
        entries: rec
            .get("entries")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|e| ReportEntry {
                severity: e.get("severity").and_then(Value::as_str).unwrap().into(),
                construct: e.get("construct").and_then(Value::as_str).unwrap().into(),
                message: e.get("message").and_then(Value::as_str).unwrap().into(),
            })
            .collect(),
        math_omml: n("math_omml"),
        math_image: n("math_image"),
        math_raw: n("math_raw"),
    }
}

#[test]
fn report_helpers_match_python() {
    let text = std::fs::read_to_string(root().join("report.jsonl")).unwrap();
    let mut failures = Vec::new();
    let mut reports = Vec::new();
    for line in text.lines() {
        let row = json::parse(line).unwrap();
        if let Some(want) = row.get("aggregate").and_then(Value::as_str) {
            if sha256_hex(aggregate_html(&reports).as_bytes()) != want {
                failures.push("aggregate_html".to_string());
            }
            continue;
        }
        let id = row.get("case").and_then(Value::as_str).unwrap();
        let rep = rebuild(load(id).out.get("report").unwrap());
        if sha256_hex(rep.to_json().as_bytes()) != row.get("json").and_then(Value::as_str).unwrap()
        {
            failures.push(format!("{id}: to_json"));
        }
        if rep.summary() != row.get("summary").and_then(Value::as_str).unwrap() {
            failures.push(format!("{id}: summary {:?}", rep.summary()));
        }
        if sha256_hex(rep.to_html().as_bytes()) != row.get("html").and_then(Value::as_str).unwrap()
        {
            failures.push(format!("{id}: to_html"));
        }
        reports.push((id.to_string(), rep));
    }
    assert!(reports.len() > 400);
    assert!(
        failures.is_empty(),
        "{} differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
