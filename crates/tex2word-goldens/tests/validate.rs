//! `validate_docx` reproduces the Python reference on every recorded call.

use tex2word_goldens::{blob, load_all};
use tex2word_ir::json::Value;

#[test]
fn validate_docx_matches_python() {
    let mut failures = Vec::new();
    let mut n = 0;
    for case in load_all().into_iter().filter(|c| c.api == "validate_docx") {
        n += 1;
        let sha = case.arg("docx_bytes").and_then(|v| v.get("@blob")).and_then(Value::as_str).unwrap();
        let want: Vec<&str> = case
            .out
            .get("value")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        match tex2word_validate::validate_docx(&blob(sha)) {
            Ok(got) if got == want => {}
            got => failures.push(format!("{}: want {want:?}\n got {got:?}", case.id)),
        }
    }
    assert!(n > 50, "only {n} cases");
    assert!(failures.is_empty(), "{} of {n} differ:\n{}", failures.len(), failures.join("\n"));
}
