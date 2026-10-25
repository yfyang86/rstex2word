//! A quantitative conversion-fidelity baseline (port of `benchmark.py`).
//!
//! Runs every `.tex` under a directory through the converter and records
//! native-OMML math share, validity, warning/error counts, block count and
//! hard aborts, so regressions are measurable.

use std::time::Instant;

use tex2word_ir::json::Value;
use tex2word_ir::pyos;

use crate::pipeline::{convert_file, ConvertOptions};

/// Per-document metrics.
#[derive(Debug, Clone, PartialEq)]
pub struct DocMetrics {
    pub name: String,
    pub math_total: i64,
    pub math_omml: i64,
    pub math_image: i64,
    pub math_raw: i64,
    pub blocks: i64,
    pub warnings: i64,
    pub errors: i64,
    pub valid: bool,
    pub aborted: bool,
    pub seconds: f64,
}

impl DocMetrics {
    fn new(name: String) -> Self {
        DocMetrics {
            name,
            math_total: 0,
            math_omml: 0,
            math_image: 0,
            math_raw: 0,
            blocks: 0,
            warnings: 0,
            errors: 0,
            valid: false,
            aborted: false,
            seconds: 0.0,
        }
    }

    pub fn math_omml_pct(&self) -> f64 {
        if self.math_total == 0 {
            100.0
        } else {
            100.0 * self.math_omml as f64 / self.math_total as f64
        }
    }

    fn to_value(&self) -> Value {
        Value::Object(vec![
            ("name".into(), Value::Str(self.name.clone())),
            ("math_total".into(), Value::Int(self.math_total)),
            ("math_omml".into(), Value::Int(self.math_omml)),
            ("math_image".into(), Value::Int(self.math_image)),
            ("math_raw".into(), Value::Int(self.math_raw)),
            ("blocks".into(), Value::Int(self.blocks)),
            ("warnings".into(), Value::Int(self.warnings)),
            ("errors".into(), Value::Int(self.errors)),
            ("valid".into(), Value::Bool(self.valid)),
            ("aborted".into(), Value::Bool(self.aborted)),
            ("seconds".into(), Value::Float(self.seconds)),
        ])
    }
}

/// Corpus-wide totals.
#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    pub documents: i64,
    pub aborted: i64,
    pub valid: i64,
    pub math_total: i64,
    pub math_omml: i64,
    pub math_raw: i64,
    pub math_image: i64,
    pub math_omml_pct: f64,
    pub warnings: i64,
    pub errors: i64,
    pub seconds: f64,
}

/// `benchmark_dir`'s result: per-document metrics and the aggregate.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkResult {
    pub documents: Vec<DocMetrics>,
    pub aggregate: Aggregate,
}

impl BenchmarkResult {
    /// The JSON shape Python's `benchmark_dir` returns.
    pub fn to_value(&self) -> Value {
        let a = &self.aggregate;
        Value::Object(vec![
            ("documents".into(), Value::Array(self.documents.iter().map(DocMetrics::to_value).collect())),
            (
                "aggregate".into(),
                Value::Object(vec![
                    ("documents".into(), Value::Int(a.documents)),
                    ("aborted".into(), Value::Int(a.aborted)),
                    ("valid".into(), Value::Int(a.valid)),
                    ("math_total".into(), Value::Int(a.math_total)),
                    ("math_omml".into(), Value::Int(a.math_omml)),
                    ("math_raw".into(), Value::Int(a.math_raw)),
                    ("math_image".into(), Value::Int(a.math_image)),
                    ("math_omml_pct".into(), Value::Float(a.math_omml_pct)),
                    ("warnings".into(), Value::Int(a.warnings)),
                    ("errors".into(), Value::Int(a.errors)),
                    ("seconds".into(), Value::Float(a.seconds)),
                ]),
            ),
        ])
    }
}

/// Python `round(x, n)` (correctly rounded, ties to even).
pub fn py_round(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().unwrap_or(x)
}

/// `os.devnull`.
pub const DEVNULL: &str = if cfg!(windows) { "nul" } else { "/dev/null" };

fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

/// `sorted(glob.glob(os.path.join(directory, "**", "*.tex"), recursive=True))`.
pub fn glob_tex(directory: &str) -> Vec<String> {
    fn subdirs(dir: &str, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(if dir.is_empty() { "." } else { dir }) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if is_hidden(&name) || !e.path().is_dir() {
                continue;
            }
            let p = pyos::join2(dir, &name);
            out.push(p.clone());
            subdirs(&p, out);
        }
    }
    let base = pyos::join2(directory, "");
    if !pyos::isdir(if base.is_empty() { "." } else { &base }) {
        return Vec::new();
    }
    let mut dirs = vec![base.clone()];
    subdirs(&base, &mut dirs);
    let mut out = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(if d.is_empty() { "." } else { &d }) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !is_hidden(&name) && name.ends_with(".tex") {
                out.push(pyos::join2(&d, &name));
            }
        }
    }
    out.sort();
    out
}

/// `os.path.relpath(path, start)`.
pub fn relpath(path: &str, start: &str) -> String {
    let a = pyos::abspath(path);
    let b = pyos::abspath(start);
    let pa: Vec<&str> = a.split('/').filter(|s| !s.is_empty()).collect();
    let pb: Vec<&str> = b.split('/').filter(|s| !s.is_empty()).collect();
    let common = pa.iter().zip(&pb).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<&str> = vec![".."; pb.len() - common];
    parts.extend(&pa[common..]);
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

fn one(path: &str, base: &str) -> DocMetrics {
    let mut m = DocMetrics::new(relpath(path, base));
    let t0 = Instant::now();
    let opts = ConvertOptions {
        embed_manifest: false,
        ..ConvertOptions::default()
    };
    let converted = std::panic::catch_unwind(|| convert_file(path, Some(DEVNULL), &opts));
    let result = match converted {
        Ok(Ok((_, r))) => r,
        _ => {
            m.aborted = true;
            m.seconds = py_round(t0.elapsed().as_secs_f64(), 3);
            return m;
        }
    };
    m.seconds = py_round(t0.elapsed().as_secs_f64(), 3);
    m.math_omml = result.report.math_omml;
    m.math_image = result.report.math_image;
    m.math_raw = result.report.math_raw;
    m.math_total = m.math_omml + m.math_image + m.math_raw;
    m.blocks = result.document.blocks.len() as i64;
    m.warnings = result.report.warnings().len() as i64;
    m.errors = result.report.errors().len() as i64;
    m.valid = tex2word_validate::validate_docx(&result.docx).is_ok_and(|p| p.is_empty());
    m
}

fn aggregate(docs: &[DocMetrics]) -> Aggregate {
    let sum = |f: fn(&DocMetrics) -> i64| docs.iter().map(f).sum::<i64>();
    let math_total = sum(|d| d.math_total);
    let math_omml = sum(|d| d.math_omml);
    Aggregate {
        documents: docs.len() as i64,
        aborted: sum(|d| d.aborted as i64),
        valid: sum(|d| d.valid as i64),
        math_total,
        math_omml,
        math_raw: sum(|d| d.math_raw),
        math_image: sum(|d| d.math_image),
        math_omml_pct: if math_total == 0 {
            100.0
        } else {
            py_round(100.0 * math_omml as f64 / math_total as f64, 1)
        },
        warnings: sum(|d| d.warnings),
        errors: sum(|d| d.errors),
        seconds: py_round(docs.iter().map(|d| d.seconds).sum(), 3),
    }
}

/// Convert every `.tex` under `directory`; per-document and aggregate
/// metrics.
pub fn benchmark_dir(directory: &str) -> BenchmarkResult {
    let documents: Vec<DocMetrics> = glob_tex(directory).iter().map(|p| one(p, directory)).collect();
    let aggregate = aggregate(&documents);
    BenchmarkResult { documents, aggregate }
}

/// A compact text table of the benchmark result.
pub fn format_report(result: &BenchmarkResult) -> String {
    let mut lines = vec![format!(
        "{:<34} {:>6} {:>5} {:>4} {:>5} {:>6} valid",
        "document", "math%", "omml", "raw", "warn", "sec"
    )];
    lines.push("-".repeat(72));
    for d in &result.documents {
        let flag = if d.aborted {
            "ABORT"
        } else if d.valid {
            "ok"
        } else {
            "INVALID"
        };
        lines.push(format!(
            "{:<34} {:6.1} {:5} {:4} {:5} {:6.2} {flag}",
            d.name,
            d.math_omml_pct(),
            d.math_omml,
            d.math_raw,
            d.warnings,
            d.seconds
        ));
    }
    let a = &result.aggregate;
    lines.push("-".repeat(72));
    lines.push(format!(
        "{:<34} {:6.1} {:5} {:4} {:5} {:6.2} {}/{} valid, {} aborted",
        format!("TOTAL ({} docs)", a.documents),
        a.math_omml_pct,
        a.math_omml,
        a.math_raw,
        a.warnings,
        a.seconds,
        a.valid,
        a.documents,
        a.aborted
    ));
    lines.join("\n")
}
