//! Golden-case loader for the parity tests.
//!
//! `goldens/` holds cases recorded from the Python tex2word 1.0.9 reference by
//! `tools/golden/capture_plugin.py`: every call the Python test suite made to a
//! public API, with Python's output. Each case lives in
//! `goldens/cases/<id>/` (`case.json` = inputs, `out.json` = Python's result);
//! binary payloads are content-addressed under `goldens/blobs/<sha256>`.

use std::fs;
use std::path::{Path, PathBuf};

use tex2word_ir::json::{self, Value};

/// The goldens root (`crates/tex2word-goldens/goldens`).
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("goldens")
}

/// One recorded case.
#[derive(Debug, Clone)]
pub struct Case {
    pub id: String,
    /// the captured API (`convert_source`, `parse_document`, …)
    pub api: String,
    /// `case.json` (inputs: `args`, `base_dir`, `files`, `tests`)
    pub case: Value,
    /// `out.json` (Python's result)
    pub out: Value,
}

impl Case {
    /// An argument by name.
    pub fn arg(&self, name: &str) -> Option<&Value> {
        self.case.get("args")?.get(name)
    }

    /// The pytest node ids that exercised this case.
    pub fn tests(&self) -> Vec<String> {
        self.case
            .get("tests")
            .and_then(Value::as_array)
            .unwrap_or(&[])
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    }
}

/// Read a content-addressed blob.
pub fn blob(sha: &str) -> Vec<u8> {
    fs::read(root().join("blobs").join(sha)).unwrap_or_else(|e| panic!("blob {sha}: {e}"))
}

/// Load every case, sorted by id.
pub fn load_all() -> Vec<Case> {
    let dir = root().join("cases");
    let mut ids: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids.into_iter().map(|id| load(&id)).collect()
}

/// Load one case by id.
pub fn load(id: &str) -> Case {
    let d = root().join("cases").join(id);
    let read = |name: &str| {
        let text = fs::read_to_string(d.join(name)).unwrap_or_else(|e| panic!("{id}/{name}: {e}"));
        json::parse(&text).unwrap_or_else(|e| panic!("{id}/{name}: {e}"))
    };
    let case = read("case.json");
    let out = read("out.json");
    let api = case.get("api").and_then(Value::as_str).unwrap_or("").to_string();
    Case {
        id: id.to_string(),
        api,
        case,
        out,
    }
}

/// Order-insensitive structural JSON equality (objects compare as maps; ints
/// and floats stay distinct, as in Python). Returns the path of the first
/// difference.
pub fn json_diff(want: &Value, got: &Value, path: &str) -> Option<String> {
    match (want, got) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, va) in a {
                let p = format!("{path}/{k}");
                match b.iter().find(|(kb, _)| kb == k) {
                    None => return Some(format!("{p}: missing in Rust output")),
                    Some((_, vb)) => {
                        if let Some(d) = json_diff(va, vb, &p) {
                            return Some(d);
                        }
                    }
                }
            }
            for (k, _) in b {
                if !a.iter().any(|(ka, _)| ka == k) {
                    return Some(format!("{path}/{k}: unexpected in Rust output"));
                }
            }
            None
        }
        (Value::Array(a), Value::Array(b)) => {
            for (i, (va, vb)) in a.iter().zip(b).enumerate() {
                if let Some(d) = json_diff(va, vb, &format!("{path}[{i}]")) {
                    return Some(d);
                }
            }
            (a.len() != b.len())
                .then(|| format!("{path}: length {} (Python) vs {} (Rust)", a.len(), b.len()))
        }
        (Value::Float(x), Value::Float(y)) if x.is_nan() && y.is_nan() => None,
        _ if want == got => None,
        _ => Some(format!(
            "{path}: Python {} vs Rust {}",
            short(want),
            short(got)
        )),
    }
}

fn short(v: &Value) -> String {
    let s = json::dumps(v, json::DumpOptions::default());
    if s.chars().count() > 160 {
        format!("{}…", s.chars().take(160).collect::<String>())
    } else {
        s
    }
}
