//! Golden-case loader for the parity tests.
//!
//! `goldens/` holds cases recorded from the Python tex2word 1.0.9 reference by
//! `tools/golden/capture_plugin.py`: every call the Python test suite made to a
//! public API, with Python's output. Each case lives in
//! `goldens/cases/<id>/` (`case.json` = inputs, `out.json` = Python's result);
//! binary payloads are content-addressed under `goldens/blobs/<sha256>`.

use std::fmt::Write;
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

/// A temporary directory holding a case's snapshot files; removed on drop.
pub struct Materialized {
    root: PathBuf,
    /// the `base_dir` argument to replay with (`"."` when none was recorded)
    pub base_dir: String,
}

impl Materialized {
    /// The path a recorded file argument (`{"@file": "@arg/name"}`) maps to.
    pub fn arg_path(&self, name: &str) -> String {
        self.root.join(name).to_string_lossy().into_owned()
    }

    /// The temp root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Replace the capture placeholders (`{BASE}`, `{ARG:name}`) in an
    /// expected value with this replay's real paths... and vice versa: map the
    /// real paths in a Rust-produced string back to the placeholders.
    pub fn to_placeholders(&self, s: &str) -> String {
        let mut out = s.to_string();
        let arg_dir = self.root.join("@arg");
        if let Ok(entries) = fs::read_dir(&arg_dir) {
            for e in entries.flatten() {
                let p = e.path().to_string_lossy().into_owned();
                let name = e.file_name().to_string_lossy().into_owned();
                out = out.replace(&p, &format!("{{ARG:{name}}}"));
            }
        }
        if self.base_dir != "." {
            out = out.replace(&self.base_dir, "{BASE}");
        }
        out
    }
}

impl Drop for Materialized {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Write a case's snapshot files (`case.json` `files`) into a fresh temp dir.
pub fn materialize(case: &Case) -> Materialized {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "t2w-golden-{}-{}-{}",
        std::process::id(),
        case.id,
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("temp dir");
    if let Some(files) = case.case.get("files").and_then(Value::as_object) {
        for (rel, sha) in files {
            let dest = root.join(rel);
            fs::create_dir_all(dest.parent().unwrap()).expect("mkdir");
            fs::write(&dest, blob(sha.as_str().unwrap())).expect("write snapshot file");
        }
    }
    let base_dir = if case.case.get("base_dir").and_then(Value::as_str) == Some("base") {
        let b = root.join("base");
        fs::create_dir_all(&b).expect("mkdir base");
        b.to_string_lossy().into_owned()
    } else {
        ".".to_string()
    };
    Materialized { root, base_dir }
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
    let api = case
        .get("api")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
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

/// SHA-256 hex digest (for comparing against hashes recorded in Python).
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().fold(String::new(), |mut out, x| {
        let _ = write!(out, "{x:08x}");
        out
    })
}

/// The `convert_source` keyword arguments a case recorded.
pub fn convert_options(case: &Case, m: &Materialized) -> tex2word::ConvertOptions {
    let mut o = tex2word::ConvertOptions::default();
    let s = |k: &str| {
        case.arg(k)
            .and_then(json::Value::as_str)
            .map(str::to_string)
    };
    let b = |k: &str| case.arg(k).and_then(json::Value::as_bool);
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
    if let Some(v) = case.arg("columns").and_then(json::Value::as_i64) {
        o.columns = v;
    }
    o.csl = s("csl");
    o.language = s("language");
    o.reference_doc = match case.arg("reference_doc") {
        Some(v) => match v.get("@file").and_then(json::Value::as_str) {
            Some(rel) => Some(m.arg_path(rel)),
            None => v.as_str().map(str::to_string),
        },
        None => None,
    };
    o
}
