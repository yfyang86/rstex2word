//! The frontend's source-rewriting stages reproduce the Python reference on
//! every recorded case (`stages.jsonl`, from `tools/golden/gen_walker_goldens.py`):
//! the string after `preprocess`, `expand_macros`, `replace_inline_tikz`,
//! `normalize_spacing_and_icons` and the document split.

use tex2word_goldens::{load, materialize, root};
use tex2word_ir::json::{self, Value};

fn first_diff(a: &str, b: &str) -> String {
    let i = a
        .char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map(|((i, _), _)| i)
        .unwrap_or(a.len().min(b.len()));
    // (str::floor_char_boundary is newer than the MSRV)
    let ctx = |s: &str| -> String {
        let mut start = i.saturating_sub(40).min(s.len());
        while !s.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (i + 40).min(s.len());
        while !s.is_char_boundary(end) {
            end += 1;
        }
        s[start..end].to_string()
    };
    format!(
        "at byte {i}:\n      python: {:?}\n      rust:   {:?}",
        ctx(a),
        ctx(b)
    )
}

#[test]
fn frontend_stages_match_python() {
    let text = std::fs::read_to_string(root().join("stages.jsonl")).expect("stages.jsonl");
    let mut failures = Vec::new();
    let mut n = 0;
    for line in text.lines() {
        n += 1;
        let st = json::parse(line).unwrap();
        let id = st.get("case").and_then(Value::as_str).unwrap();
        let case = load(id);
        let source = case
            .arg("source")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let m = materialize(&case);
        let want = |k: &str| st.get(k).and_then(Value::as_str).map(str::to_string);

        let pre = match tex2word_frontend::preprocess::preprocess(&source, &m.base_dir) {
            Ok(p) => p,
            Err(e) => {
                failures.push(format!("{id}: preprocess error {e}"));
                continue;
            }
        };
        let pre_want = want("pre").unwrap();
        if pre != pre_want {
            failures.push(format!("{id} [preprocess] {}", first_diff(&pre_want, &pre)));
            continue;
        }
        // a null stage means "unchanged from the previous stage"
        let stage = |k: &str, prev: &str| want(k).unwrap_or_else(|| prev.to_string());

        let exp = tex2word_frontend::macros::expand_macros(&pre, &m.base_dir);
        let exp_want = stage("exp", &pre_want);
        if exp != exp_want {
            failures.push(format!(
                "{id} [expand_macros] {}",
                first_diff(&exp_want, &exp)
            ));
            continue;
        }

        let tikz = tex2word_frontend::preprocess::replace_inline_tikz(&exp);
        let tikz_want = stage("tikz", &exp_want);
        if tikz != tikz_want {
            failures.push(format!(
                "{id} [replace_inline_tikz] {}",
                first_diff(&tikz_want, &tikz)
            ));
            continue;
        }

        let norm = tex2word_frontend::preprocess::normalize_spacing_and_icons(&tikz);
        let norm_want = stage("norm", &tikz_want);
        if norm != norm_want {
            failures.push(format!(
                "{id} [normalize_spacing_and_icons] {}",
                first_diff(&norm_want, &norm)
            ));
            continue;
        }
    }
    assert!(n > 600, "only {n} stage goldens");
    assert!(
        failures.is_empty(),
        "{} of {n} cases differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(15)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
