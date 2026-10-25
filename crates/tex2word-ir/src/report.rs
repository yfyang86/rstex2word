//! Conversion report: warnings, coverage telemetry and fallback accounting
//! (port of `report.py`).
//!
//! Graceful degradation is a hard requirement: the converter never aborts on
//! an unknown construct, and every fallback is logged here so per-construct
//! coverage can be reported.

use crate::dict::Dict;
use crate::json::{dumps, DumpOptions, Value};

/// One report entry. `severity` is `info`, `warning` or `error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportEntry {
    pub severity: String,
    pub construct: String,
    pub message: String,
}

/// The single sink for warnings, errors and math-path telemetry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversionReport {
    pub entries: Vec<ReportEntry>,
    /// math nodes routed to each path of the decision cascade
    pub math_omml: i64,
    pub math_image: i64,
    pub math_raw: i64,
}

impl ConversionReport {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, severity: &str, construct: &str, message: impl Into<String>) {
        self.entries.push(ReportEntry {
            severity: severity.into(),
            construct: construct.into(),
            message: message.into(),
        });
    }

    pub fn warn(&mut self, construct: &str, message: impl Into<String>) {
        self.push("warning", construct, message);
    }

    pub fn error(&mut self, construct: &str, message: impl Into<String>) {
        self.push("error", construct, message);
    }

    pub fn info(&mut self, construct: &str, message: impl Into<String>) {
        self.push("info", construct, message);
    }

    pub fn warnings(&self) -> Vec<&ReportEntry> {
        self.entries.iter().filter(|e| e.severity == "warning").collect()
    }

    pub fn errors(&self) -> Vec<&ReportEntry> {
        self.entries.iter().filter(|e| e.severity == "error").collect()
    }

    /// `coverage()`: `math_total`, `math_omml`, `math_image`, `math_raw`.
    pub fn coverage(&self) -> Dict<i64> {
        let mut d = Dict::new();
        d.insert("math_total", self.math_omml + self.math_image + self.math_raw);
        d.insert("math_omml", self.math_omml);
        d.insert("math_image", self.math_image);
        d.insert("math_raw", self.math_raw);
        d
    }

    /// `construct_counts()`: entry count per construct, first-seen order.
    pub fn construct_counts(&self) -> Dict<i64> {
        let mut d: Dict<i64> = Dict::new();
        for e in &self.entries {
            *d.set_default(e.construct.clone(), 0) += 1;
        }
        d
    }

    /// The report as a JSON value (the payload of `to_json`).
    pub fn to_value(&self) -> Value {
        let to_obj = |d: Dict<i64>| {
            Value::Object(d.into_iter().map(|(k, v)| (k, Value::Int(v))).collect())
        };
        Value::Object(vec![
            ("coverage".into(), to_obj(self.coverage())),
            ("constructs".into(), to_obj(self.construct_counts())),
            (
                "entries".into(),
                Value::Array(
                    self.entries
                        .iter()
                        .map(|e| {
                            Value::Object(vec![
                                ("severity".into(), Value::Str(e.severity.clone())),
                                ("construct".into(), Value::Str(e.construct.clone())),
                                ("message".into(), Value::Str(e.message.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }

    /// `to_json()` — `json.dumps(…, indent=2)`.
    pub fn to_json(&self) -> String {
        dumps(
            &self.to_value(),
            DumpOptions {
                indent: Some(2),
                ..DumpOptions::default()
            },
        )
    }

    /// `summary()` — the one-line CLI summary.
    pub fn summary(&self) -> String {
        format!(
            "math: {} OMML / {} image / {} raw; {} warning(s), {} error(s)",
            self.math_omml,
            self.math_image,
            self.math_raw,
            self.warnings().len(),
            self.errors().len()
        )
    }

    /// `to_html()` — a standalone HTML report.
    pub fn to_html(&self) -> String {
        let total = self.math_omml + self.math_image + self.math_raw;
        let pct = 100.0 * self.math_omml as f64 / total.max(1) as f64;
        let rows: String = self
            .entries
            .iter()
            .map(|e| {
                format!(
                    "<tr class='{}'><td>{}</td><td><code>{}</code></td><td>{}</td></tr>",
                    e.severity,
                    e.severity,
                    esc(&e.construct),
                    esc(&e.message)
                )
            })
            .collect();
        let mut counts: Vec<(String, i64)> = self.construct_counts().into_iter().collect();
        counts.sort();
        let constructs: String = counts
            .iter()
            .map(|(k, v)| format!("<tr><td><code>{}</code></td><td>{v}</td></tr>", esc(k)))
            .collect();
        let constructs = if constructs.is_empty() {
            "<tr><td colspan=2>none</td></tr>".to_string()
        } else {
            constructs
        };
        let rows = if rows.is_empty() {
            "<tr><td colspan=3>no warnings or errors</td></tr>".to_string()
        } else {
            rows
        };
        format!(
            r#"<!doctype html>
<html><head><meta charset="utf-8"><title>tex2word conversion report</title>
<style>
 body{{font-family:system-ui,sans-serif;margin:2rem;max-width:60rem}}
 table{{border-collapse:collapse;width:100%;margin:1rem 0}}
 th,td{{border:1px solid #ccc;padding:.3rem .6rem;text-align:left;font-size:.9rem}}
 .warning{{background:#fff7e6}} .error{{background:#fde8e8}}
 .bar{{height:1.4rem;background:#e6f4ea;border:1px solid #b7d7be}}
 .bar>span{{display:block;height:100%;background:#34a853}}
</style></head><body>
<h1>tex2word conversion report</h1>
<h2>Math coverage</h2>
<div class="bar"><span style="width:{pct:.0}%"></span></div>
<p>{omml} OMML &middot; {image} image &middot;
   {raw} raw (of {total} equations, {pct:.0}% editable)</p>
<h2>Constructs</h2>
<table><tr><th>construct</th><th>count</th></tr>{constructs}</table>
<h2>Messages</h2>
<table><tr><th>severity</th><th>construct</th><th>message</th></tr>
{rows}</table>
</body></html>
"#,
            omml = self.math_omml,
            image = self.math_image,
            raw = self.math_raw,
        )
    }
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `aggregate_html(results)` — a coverage dashboard across a corpus of
/// `(name, report)` pairs.
pub fn aggregate_html(results: &[(String, ConversionReport)]) -> String {
    let mut tot: Dict<i64> = Dict::new();
    let mut rows = Vec::new();
    let mut construct_totals: Dict<i64> = Dict::new();
    for (name, rep) in results {
        let cov = rep.coverage();
        for (k, v) in cov.iter() {
            *tot.set_default(k.clone(), 0) += *v;
        }
        for (k, v) in rep.construct_counts().iter() {
            *construct_totals.set_default(k.clone(), 0) += *v;
        }
        let omml = *cov.get("math_omml").unwrap_or(&0);
        let total = *cov.get("math_total").unwrap_or(&0);
        let pct = 100.0 * omml as f64 / total.max(1) as f64;
        rows.push(format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.0}%</td><td>{}</td><td>{}</td></tr>",
            esc(name),
            total,
            omml,
            cov.get("math_raw").unwrap_or(&0),
            pct,
            rep.warnings().len(),
            rep.errors().len()
        ));
    }
    let get = |k: &str| *tot.get(k).unwrap_or(&0);
    let grand_pct = 100.0 * get("math_omml") as f64 / get("math_total").max(1) as f64;
    // Counter.most_common: by count, descending; ties keep first-seen order.
    let mut common: Vec<(String, i64)> = construct_totals.into_iter().collect();
    common.sort_by(|a, b| b.1.cmp(&a.1));
    let constructs: String = common
        .iter()
        .take(40)
        .map(|(k, v)| format!("<tr><td><code>{}</code></td><td>{v}</td></tr>", esc(k)))
        .collect();
    let constructs = if constructs.is_empty() {
        "<tr><td colspan=2>none</td></tr>".to_string()
    } else {
        constructs
    };
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><title>tex2word corpus coverage</title>
<style>
 body{{font-family:system-ui,sans-serif;margin:2rem;max-width:64rem}}
 table{{border-collapse:collapse;width:100%;margin:1rem 0}}
 th,td{{border:1px solid #ccc;padding:.3rem .6rem;text-align:left;font-size:.9rem}}
 .bar{{height:1.4rem;background:#e6f4ea;border:1px solid #b7d7be;max-width:40rem}}
 .bar>span{{display:block;height:100%;background:#34a853}}
</style></head><body>
<h1>tex2word corpus coverage</h1>
<p>{n} document(s) &middot; {mt} equations &middot;
   {mo} editable OMML &middot; {mr} raw fallback</p>
<div class="bar"><span style="width:{grand_pct:.0}%"></span></div>
<p>{grand_pct:.0}% of equations are editable OMML across the corpus.</p>
<h2>Per document</h2>
<table><tr><th>file</th><th>eqns</th><th>OMML</th><th>raw</th><th>OMML%</th>
<th>warnings</th><th>errors</th></tr>{rows}</table>
<h2>Constructs (warnings/info, all docs)</h2>
<table><tr><th>construct</th><th>count</th></tr>{constructs}</table>
</body></html>
"#,
        n = results.len(),
        mt = get("math_total"),
        mo = get("math_omml"),
        mr = get("math_raw"),
        rows = rows.join(""),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_json_matches_python_layout() {
        let mut r = ConversionReport::new();
        r.warn("\\ref", "unresolved reference to 'x'");
        r.math_omml = 2;
        assert_eq!(
            r.to_json(),
            "{\n  \"coverage\": {\n    \"math_total\": 2,\n    \"math_omml\": 2,\n    \"math_image\": 0,\n    \"math_raw\": 0\n  },\n  \"constructs\": {\n    \"\\\\ref\": 1\n  },\n  \"entries\": [\n    {\n      \"severity\": \"warning\",\n      \"construct\": \"\\\\ref\",\n      \"message\": \"unresolved reference to 'x'\"\n    }\n  ]\n}"
        );
        assert_eq!(r.summary(), "math: 2 OMML / 0 image / 0 raw; 1 warning(s), 0 error(s)");
    }
}
