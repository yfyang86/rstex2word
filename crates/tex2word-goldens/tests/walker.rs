//! tex2word-latexwalker reproduces pylatexenc's node trees on the exact body
//! strings the Python parser walks (and on the raw sources), recorded by
//! `tools/golden/gen_walker_goldens.py`.

use std::sync::Arc;

use tex2word_goldens::{json_diff, root};
use tex2word_ir::json::{self, Value};
use tex2word_latexwalker::{
    ContextDb, EnvironmentSpec, LatexWalker, MacroSpec, Node, NodeKind, ParsedArgs,
};

fn args_json(a: &Option<ParsedArgs>) -> Value {
    match a {
        None => Value::Null,
        Some(a) => {
            let mut o = vec![
                ("spec".to_string(), Value::Str(a.argspec.clone())),
                (
                    "n".to_string(),
                    Value::Array(
                        a.argnlist
                            .iter()
                            .map(|n| n.as_ref().map_or(Value::Null, node_json))
                            .collect(),
                    ),
                ),
            ];
            if let Some(v) = &a.verbatim_text {
                o.push(("v".to_string(), Value::Str(v.clone())));
            }
            Value::Object(o)
        }
    }
}

fn list(nodes: &[Node]) -> Value {
    Value::Array(nodes.iter().map(node_json).collect())
}

fn node_json(n: &Node) -> Value {
    let s = |x: &str| Value::Str(x.to_string());
    let mut o = vec![
        ("p".to_string(), Value::Int(n.pos as i64)),
        ("l".to_string(), Value::Int(n.len as i64)),
    ];
    let t = match &n.kind {
        NodeKind::Chars { chars } => {
            o.push(("c".into(), s(chars)));
            "C"
        }
        NodeKind::Group {
            nodelist,
            delimiters,
        } => {
            o.push((
                "d".into(),
                Value::Array(vec![s(&delimiters.0), s(&delimiters.1)]),
            ));
            o.push(("n".into(), list(nodelist)));
            "G"
        }
        NodeKind::Comment {
            comment,
            comment_post_space,
        } => {
            o.push(("c".into(), s(comment)));
            o.push(("s".into(), s(comment_post_space)));
            "K"
        }
        NodeKind::Macro {
            macroname,
            nodeargd,
            macro_post_space,
            ..
        } => {
            o.push(("m".into(), s(macroname)));
            o.push(("s".into(), s(macro_post_space)));
            o.push(("a".into(), args_json(nodeargd)));
            "M"
        }
        NodeKind::Environment {
            environmentname,
            nodelist,
            nodeargd,
        } => {
            o.push(("e".into(), s(environmentname)));
            o.push(("n".into(), list(nodelist)));
            o.push(("a".into(), args_json(nodeargd)));
            "E"
        }
        NodeKind::Specials {
            specials_chars,
            nodeargd,
        } => {
            o.push(("c".into(), s(specials_chars)));
            o.push(("a".into(), args_json(nodeargd)));
            "S"
        }
        NodeKind::Math {
            displaytype,
            nodelist,
            delimiters,
        } => {
            o.push(("d".into(), s(displaytype)));
            o.push((
                "de".into(),
                Value::Array(vec![s(&delimiters.0), s(&delimiters.1)]),
            ));
            o.push(("n".into(), list(nodelist)));
            "X"
        }
    };
    o.push(("t".into(), s(t)));
    Value::Object(o)
}

fn context(cat: &Value) -> ContextDb {
    let pairs = |v: &Value| -> Vec<(String, String)> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let p = p.as_array().unwrap();
                (
                    p[0].as_str().unwrap().to_string(),
                    p[1].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    let cat = cat.as_array().unwrap();
    let mut db = ContextDb::default_db();
    db.add_context_category(
        "tex2word",
        pairs(&cat[0])
            .into_iter()
            .map(|(n, a)| MacroSpec::new(n, a)),
        pairs(&cat[1])
            .into_iter()
            .map(|(n, a)| EnvironmentSpec::new(n, a)),
        [],
        true,
    );
    db
}

#[test]
fn walker_matches_pylatexenc() {
    let text = std::fs::read_to_string(root().join("walker.jsonl")).expect("walker.jsonl");
    let mut lines = text.lines();
    let contexts: Vec<Arc<ContextDb>> = json::parse(lines.next().unwrap())
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|c| Arc::new(context(c)))
        .collect();
    let mut failures = Vec::new();
    let mut n = 0;
    for line in lines {
        n += 1;
        let row = json::parse(line).unwrap();
        let src = row.get("src").and_then(Value::as_str).unwrap();
        let ctx = contexts[row.get("ctx").and_then(Value::as_i64).unwrap() as usize].clone();
        let got = list(&LatexWalker::new(src, ctx).get_latex_nodes_all());
        if let Some(d) = json_diff(row.get("nodes").unwrap(), &got, "") {
            let head: String = src.chars().take(80).collect();
            failures.push(format!("{d}\n    input: {head:?}"));
        }
    }
    assert!(n > 1000, "only {n} walker goldens");
    assert!(
        failures.is_empty(),
        "{} of {n} inputs differ from pylatexenc:\n{}",
        failures.len(),
        failures
            .iter()
            .take(15)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
