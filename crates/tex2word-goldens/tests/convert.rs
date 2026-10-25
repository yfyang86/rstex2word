//! `convert_source` reproduces the Python reference end to end: the IR, the
//! report, and every `.docx` part byte for byte, in the same zip order.

use tex2word::{convert_source, ConvertOptions};
use tex2word_goldens::{blob, json_diff, load_all, materialize, sha256_hex, Case, Materialized};
use tex2word_ir::json::{self, Value};
use tex2word_ir::ToJson;
use tex2word_ooxml::zip::Archive;

/// Both tests set `SOURCE_DATE_EPOCH`; run them one at a time.
static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn options(case: &Case, m: &Materialized) -> ConvertOptions {
    let mut o = ConvertOptions::default();
    let s = |k: &str| case.arg(k).and_then(Value::as_str).map(str::to_string);
    let b = |k: &str| case.arg(k).and_then(Value::as_bool);
    if let Some(v) = b("embed_manifest") {
        o.embed_manifest = v;
    }
    if let Some(v) = b("number_by_section") {
        o.number_by_section = v;
    }
    if let Some(v) = b("math_image_fallback") {
        o.math_image_fallback = v;
    }
    if let Some(v) = s("citation_mode") {
        o.citation_mode = v;
    }
    if let Some(v) = s("frontend") {
        o.frontend = v;
    }
    if let Some(v) = case.arg("columns").and_then(Value::as_i64) {
        o.columns = v;
    }
    o.csl = s("csl");
    o.language = s("language");
    o.reference_doc = match case.arg("reference_doc") {
        Some(v) => match v.get("@file").and_then(Value::as_str) {
            Some(rel) => Some(m.arg_path(rel)),
            None => v.as_str().map(str::to_string),
        },
        None => None,
    };
    o
}

/// `None` when everything matches.
fn check(case: &Case, dump: Option<&std::path::Path>) -> Option<String> {
    let source = case.arg("source").and_then(Value::as_str)?.to_string();
    let m = materialize(case);
    let opts = options(case, &m);
    let r = match convert_source(&source, &m.base_dir, &opts) {
        Ok(r) => r,
        Err(e) => return Some(format!("error: {e}")),
    };
    let got_ir = json::parse(&m.to_placeholders(&json::dumps(&r.document.to_json(), Default::default()))).unwrap();
    if let Some(d) = json_diff(case.out.get("ir").unwrap(), &got_ir, "ir") {
        return Some(d);
    }
    let got_report = json::parse(&m.to_placeholders(&json::dumps(&r.report.to_value(), Default::default()))).unwrap();
    if let Some(d) = json_diff(case.out.get("report").unwrap(), &got_report, "report") {
        return Some(d);
    }
    let want = case.out.get("docx").unwrap();
    let archive = match Archive::new(&r.docx) {
        Ok(a) => a,
        Err(e) => return Some(format!("unreadable docx: {e}")),
    };
    let names: Vec<String> = archive.names().into_iter().map(str::to_string).collect();
    let want_order: Vec<String> = want
        .get("order")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    if names != want_order {
        return Some(format!("zip order: want {want_order:?}\n got {names:?}"));
    }
    let parts = want.get("parts").unwrap();
    let mut bad = Vec::new();
    for name in &names {
        let data = archive.read(name).unwrap();
        let want_sha = parts.get(name).and_then(Value::as_str).unwrap();
        if sha256_hex(&data) != want_sha {
            if let Some(dir) = dump {
                let d = dir.join(&case.id);
                let flat = name.replace('/', "_");
                std::fs::create_dir_all(&d).unwrap();
                std::fs::write(d.join(format!("{flat}.rust")), &data).unwrap();
                std::fs::write(d.join(format!("{flat}.python")), blob(want_sha)).unwrap();
            }
            bad.push(name.clone());
        }
    }
    if bad.is_empty() {
        None
    } else {
        Some(format!("parts differ: {bad:?}"))
    }
}

#[test]
fn convert_source_matches_python() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dump = std::env::var_os("T2W_GOLDEN_DUMP").map(std::path::PathBuf::from);
    let only = std::env::var("T2W_GOLDEN_CASE").ok();
    let mut failures = Vec::new();
    let mut n = 0;
    for case in load_all().into_iter().filter(|c| c.api == "convert_source") {
        if only.as_deref().is_some_and(|id| id != case.id) {
            continue;
        }
        n += 1;
        // tests/test_reproducible.py records its cases under SOURCE_DATE_EPOCH=1700000000
        let epoch = if case.tests().iter().any(|t| t.starts_with("tests/test_reproducible.py")) {
            "1700000000"
        } else {
            "1767312000"
        };
        std::env::set_var("SOURCE_DATE_EPOCH", epoch);
        if let Some(d) = check(&case, dump.as_deref()) {
            failures.push(format!("{} {:?}: {d}", case.id, case.tests().first()));
        }
    }
    assert!(n > 0);
    assert!(
        failures.is_empty(),
        "{} of {n} cases differ:\n{}",
        failures.len(),
        failures.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn invalid_source_date_epoch_falls_back_to_now() {
    let _env = ENV.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("SOURCE_DATE_EPOCH", "not-a-number");
    let r = convert_source(r"\begin{document}Hello $x^2$.\end{document}", ".", &ConvertOptions::default()).unwrap();
    std::env::set_var("SOURCE_DATE_EPOCH", "1767312000");
    let a = Archive::new(&r.docx).unwrap();
    let manifest = String::from_utf8(a.read("word/tex2word/manifest.json").unwrap()).unwrap();
    assert!(manifest.contains("\"generated\": \"20"), "{manifest}");
}
