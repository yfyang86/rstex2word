//! `tex2word_ir::re` reproduces Python's `re` on the reference's real calls
//! (recorded by `tools/golden/capture_regex.py`).

use tex2word_goldens::{json_diff, root};
use tex2word_ir::json::{self, Value};
use tex2word_ir::re::{Flags, Match, Regex};

fn flags(bits: i64) -> Flags {
    let mut f = Flags::NONE;
    for (bit, flag) in [(1, Flags::I), (2, Flags::S), (4, Flags::M), (8, Flags::X)] {
        if bits & bit != 0 {
            f = f | flag;
        }
    }
    f
}

fn span_json(m: &Match) -> Value {
    let groups = (1..=m.len())
        .map(|i| match m.span_of(i) {
            Some((a, b)) => Value::Array(vec![Value::Int(a as i64), Value::Int(b as i64)]),
            None => Value::Null,
        })
        .collect();
    Value::Object(vec![
        (
            "span".into(),
            Value::Array(vec![
                Value::Int(m.start() as i64),
                Value::Int(m.end() as i64),
            ]),
        ),
        ("groups".into(), Value::Array(groups)),
    ])
}

fn opt_match(m: Option<Match>) -> Value {
    m.as_ref().map_or(Value::Null, span_json)
}

fn arg_i64(args: &Value, k: &str) -> usize {
    args.get(k).and_then(Value::as_i64).unwrap_or(0) as usize
}

#[test]
fn regex_engine_matches_python_re() {
    let text = std::fs::read_to_string(root().join("regex.jsonl")).expect("regex.jsonl");
    let mut failures = Vec::new();
    let mut n = 0;
    for line in text.lines() {
        n += 1;
        let rec = json::parse(line).unwrap();
        let pat = rec.get("p").and_then(Value::as_str).unwrap();
        let fl = flags(rec.get("f").and_then(Value::as_i64).unwrap());
        let method = rec.get("m").and_then(Value::as_str).unwrap();
        let s = rec.get("s").and_then(Value::as_str).unwrap();
        let args = rec.get("a").cloned().unwrap_or(Value::Null);
        let want = rec.get("r").unwrap();
        let re = match Regex::try_new(pat, fl) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{pat:?}: compile error {e}"));
                continue;
            }
        };
        let pos = arg_i64(&args, "pos");
        let got = match method {
            "search" => opt_match(re.search_from(s, pos)),
            "match" => opt_match(re.match_at(s, pos)),
            "fullmatch" => opt_match(re.fullmatch(s)),
            "finditer" | "sub_callable" => {
                let mut ms = re.find_iter_from(s, pos);
                let count = arg_i64(&args, "count");
                if count > 0 {
                    ms.truncate(count);
                }
                Value::Array(ms.iter().map(span_json).collect())
            }
            "findall" => {
                if re.groups() <= 1 {
                    Value::Array(
                        re.findall(s)
                            .into_iter()
                            .map(|x| Value::Str(x.into()))
                            .collect(),
                    )
                } else {
                    Value::Array(
                        re.findall_groups(s)
                            .into_iter()
                            .map(|t| {
                                Value::Array(t.into_iter().map(|x| Value::Str(x.into())).collect())
                            })
                            .collect(),
                    )
                }
            }
            "subn" => {
                let repl = args.get("repl").and_then(Value::as_str).unwrap();
                let (out, k) = re.subn(repl, s, arg_i64(&args, "count"));
                Value::Array(vec![Value::Str(out), Value::Int(k as i64)])
            }
            "split" => Value::Array(
                re.split(s, arg_i64(&args, "maxsplit"))
                    .into_iter()
                    .map(|x| x.map_or(Value::Null, |x| Value::Str(x.into())))
                    .collect(),
            ),
            other => panic!("unknown method {other}"),
        };
        if let Some(d) = json_diff(want, &got, "") {
            let head: String = s.chars().take(60).collect();
            failures.push(format!("{method} {pat:?} on {head:?}: {d}"));
        }
    }
    assert!(n > 1000, "only {n} regex goldens");
    assert!(
        failures.is_empty(),
        "{} of {n} regex calls differ from Python:\n{}",
        failures.len(),
        failures
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
