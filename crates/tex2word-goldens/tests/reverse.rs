//! The reverse path against the Python reference: `read_docx`, `to_latex`,
//! `recover_ir` and `write_latex` on every recorded call.

use tex2word_goldens::{blob, json_diff, load_all, Case};
use tex2word_ir::json::Value;
use tex2word_ir::{Document, FromJson, ToJson};

fn docx(case: &Case) -> Vec<u8> {
    blob(case.arg("docx_bytes").and_then(|v| v.get("@blob")).and_then(Value::as_str).unwrap())
}

fn want_ir(case: &Case) -> Option<&Value> {
    case.out.get("value").and_then(|v| v.get("@ir"))
}

fn check_all(api: &str, f: impl Fn(&Case) -> Option<String>) {
    let mut failures = Vec::new();
    let mut n = 0;
    for case in load_all().into_iter().filter(|c| c.api == api) {
        n += 1;
        if let Some(d) = f(&case) {
            failures.push(format!("{} {:?}: {d}", case.id, case.tests().first()));
        }
    }
    assert!(n > 0, "no {api} cases");
    assert!(failures.is_empty(), "{} of {n} {api} cases differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn read_docx_matches_python() {
    check_all("read_docx", |case| {
        let label_map: Option<Vec<(String, String)>> = case.arg("label_map").and_then(Value::as_object).map(|o| {
            o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect()
        });
        match tex2word_frontend::docx_reader::read_docx(&docx(case), label_map.as_deref()) {
            Ok(doc) => json_diff(want_ir(case)?, &doc.to_json(), "ir"),
            Err(e) => Some(format!("error: {e}")),
        }
    });
}

#[test]
fn recover_ir_matches_python() {
    check_all("recover_ir", |case| match tex2word::roundtrip::recover_ir(&docx(case)) {
        Ok(Some(doc)) => json_diff(want_ir(case)?, &doc.to_json(), "ir"),
        Ok(None) => (!case.out.get("value").unwrap().is_null()).then(|| "got None".to_string()),
        Err(e) => Some(format!("error: {e}")),
    });
}

#[test]
fn to_latex_matches_python() {
    check_all("to_latex", |case| {
        let reconcile = case.arg("reconcile").and_then(Value::as_bool).unwrap_or(true);
        let want = case.out.get("value").and_then(Value::as_str);
        match tex2word::roundtrip::to_latex(&docx(case), reconcile) {
            Ok(got) if got.as_deref() == want => None,
            got => Some(format!("\nwant {want:?}\n got {got:?}")),
        }
    });
}

#[test]
fn write_latex_matches_python() {
    check_all("write_latex", |case| {
        let doc = Document::from_json(case.arg("doc")?.get("@ir")?).unwrap();
        let want = case.out.get("value").and_then(Value::as_str);
        match tex2word_latex::write_latex(&doc) {
            Ok(got) if Some(got.as_str()) == want => None,
            got => Some(format!("\nwant {want:?}\n got {got:?}")),
        }
    });
}
