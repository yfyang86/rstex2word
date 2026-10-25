//! Function-level parity: each pure frontend string function reproduces the
//! Python reference on `fn.jsonl` (from `tools/golden/gen_fn_goldens.py`):
//! chunks of every `.tex` file in the Python test tree, every recorded API
//! source, and random token soups aimed at each function's edge cases.

use tex2word_frontend::{macros, preprocess as p};
use tex2word_goldens::root;
use tex2word_ir::json::{self, Value};

fn call(f: &str, s: &str) -> Option<String> {
    Some(match f {
        "strip_comments" => p::strip_comments(s),
        "normalize_listing_envs" => p::normalize_listing_envs(s),
        "rewrite_mathoperators" => p::rewrite_mathoperators(s),
        "strip_iffalse" => p::strip_iffalse(s),
        "normalize_begin_star" => p::normalize_begin_star(s),
        "recover_tikz_boxes" => p::recover_tikz_boxes(s),
        "rewrite_halign" => p::rewrite_halign(s),
        "inject_exam_title" => p::inject_exam_title(s),
        "replace_inline_tikz" => p::replace_inline_tikz(s),
        "normalize_spacing_and_icons" => p::normalize_spacing_and_icons(s),
        "expand_macros" => macros::expand_macros(s, "."),
        "collect_macros_stripped" => macros::collect_macros(s).1,
        _ => return None,
    })
}

fn first_diff(a: &str, b: &str) -> String {
    let i = a
        .char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map(|((i, _), _)| i)
        .unwrap_or(a.len().min(b.len()));
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
    format!("at byte {i}:\n      python: {:?}\n      rust:   {:?}", ctx(a), ctx(b))
}

#[test]
fn frontend_functions_match_python() {
    let text = std::fs::read_to_string(root().join("fn.jsonl")).expect("fn.jsonl");
    let mut failures = Vec::new();
    let mut n = 0;
    for line in text.lines() {
        let row = json::parse(line).unwrap();
        let f = row.get("f").and_then(Value::as_str).unwrap();
        let input = row.get("in").and_then(Value::as_str).unwrap();
        let want = row.get("out").and_then(Value::as_str).unwrap();
        let Some(got) = call(f, input) else {
            failures.push(format!("{f}: not wired into the test"));
            continue;
        };
        n += 1;
        if got != want {
            failures.push(format!("{f} (input {} bytes) {}", input.len(), first_diff(want, &got)));
        }
    }
    assert!(n > 500, "only {n} rows");
    assert!(
        failures.is_empty(),
        "{} of {n} rows differ:\n{}",
        failures.len(),
        failures.iter().take(15).cloned().collect::<Vec<_>>().join("\n")
    );
}
