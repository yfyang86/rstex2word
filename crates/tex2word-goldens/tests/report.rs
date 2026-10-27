//! The report helpers (`to_json`, `summary`, `to_html`, `aggregate_html`)
//! against Python on every recorded conversion report (`report.jsonl`).

use tex2word_goldens::{load, root, sha256_hex};
use tex2word_ir::json::{self, Value};
use tex2word_ir::report::aggregate_html;
use tex2word_ir::{ConversionReport, ReportEntry};

/// Set `key` on a report.jsonl row object.
fn set(row: &mut Value, key: &str, val: Value) {
    if let Value::Object(pairs) = row {
        for (k, v) in pairs.iter_mut() {
            if k == key {
                *v = val;
                return;
            }
        }
    }
}

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
    let update = std::env::var_os("T2W_GOLDEN_UPDATE").is_some();
    let text = std::fs::read_to_string(root().join("report.jsonl")).unwrap();
    let mut failures = Vec::new();
    let mut reports = Vec::new();
    let mut out_lines = Vec::new();
    let mut n_updated = 0;
    for line in text.lines() {
        let mut row = json::parse(line).unwrap();
        if let Some(want) = row.get("aggregate").and_then(Value::as_str) {
            let got = sha256_hex(aggregate_html(&reports).as_bytes());
            if got != want {
                if update {
                    set(&mut row, "aggregate", Value::Str(got));
                    n_updated += 1;
                } else {
                    failures.push("aggregate_html".to_string());
                }
            }
            out_lines.push(json::dumps(&row, Default::default()));
            continue;
        }
        let id = row.get("case").and_then(Value::as_str).unwrap().to_string();
        let rep = rebuild(load(&id).out.get("report").unwrap());
        let mut bad = false;
        let json_sha = sha256_hex(rep.to_json().as_bytes());
        if json_sha != row.get("json").and_then(Value::as_str).unwrap() {
            if update {
                set(&mut row, "json", Value::Str(json_sha));
                n_updated += 1;
            } else {
                failures.push(format!("{id}: to_json"));
                bad = true;
            }
        }
        if rep.summary() != row.get("summary").and_then(Value::as_str).unwrap() {
            if update {
                set(&mut row, "summary", Value::Str(rep.summary()));
                n_updated += 1;
            } else {
                failures.push(format!("{id}: summary {:?}", rep.summary()));
                bad = true;
            }
        }
        let html_sha = sha256_hex(rep.to_html().as_bytes());
        if html_sha != row.get("html").and_then(Value::as_str).unwrap() {
            if update {
                set(&mut row, "html", Value::Str(html_sha));
                n_updated += 1;
            } else {
                failures.push(format!("{id}: to_html"));
                bad = true;
            }
        }
        let _ = bad;
        out_lines.push(json::dumps(&row, Default::default()));
        reports.push((id, rep));
    }
    assert!(reports.len() > 400);
    if update && n_updated > 0 {
        std::fs::write(root().join("report.jsonl"), out_lines.join("\n") + "\n").unwrap();
        eprintln!("updated {n_updated} report.jsonl entries");
        return;
    }
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
