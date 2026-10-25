//! xcolor colour resolution: named colours, `\definecolor`, model parsing
//! (port of `frontend/colors.py`).
//!
//! Maps LaTeX colour specifications onto the 6-hex `RRGGBB` strings that
//! Word's `w:color`/`w:shd` run properties use. Deliberately bounded: the
//! common `rgb`/`RGB`/`HTML`/`gray`/`cmyk` models and the xcolor base + dvips
//! named palette. Unknown names degrade to `None` (the caller drops the colour
//! rather than emitting wrong output).

use std::collections::HashMap;
use std::sync::LazyLock;

use tex2word_ir::pystr;
use tex2word_ir::re::Regex;

/// xcolor base colours + the common dvipsnames a paper is likely to use.
const NAMED: &[(&str, &str)] = &[
    ("black", "000000"), ("white", "FFFFFF"), ("red", "FF0000"), ("green", "00FF00"),
    ("blue", "0000FF"), ("cyan", "00FFFF"), ("magenta", "FF00FF"), ("yellow", "FFFF00"),
    ("gray", "808080"), ("grey", "808080"), ("darkgray", "404040"), ("lightgray", "BFBFBF"),
    ("lightgrey", "BFBFBF"), ("brown", "BF8040"), ("lime", "BFFF00"), ("olive", "808000"),
    ("orange", "FF8000"), ("pink", "FFBFBF"), ("purple", "BF0040"), ("teal", "008080"),
    ("violet", "800080"), ("darkgreen", "008000"), ("darkblue", "000080"),
    // a few dvipsnames seen in ML papers
    ("navyblue", "006EB8"), ("royalblue", "4169E1"), ("forestgreen", "228B22"),
    ("maroon", "B03060"), ("crimson", "DC143C"), ("gold", "FFD700"),
];

static HEX6: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9A-Fa-f]{6}"));
static HEX3: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9A-Fa-f]{3}"));
static HASH_HEX6: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#?[0-9A-Fa-f]{6}"));

fn clamp(x: f64) -> i64 {
    // max(0, min(255, round(x))); Python raises on NaN/inf, here they clamp
    let r = pystr::round_half_even(x);
    if r.is_nan() {
        0
    } else {
        r.clamp(0.0, 255.0) as i64
    }
}

fn hex(r: f64, g: f64, b: f64) -> String {
    format!("{:02X}{:02X}{:02X}", clamp(r), clamp(g), clamp(b))
}

fn floats(spec: &str) -> Vec<f64> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = pystr::strip(part);
        if !part.is_empty() {
            match pystr::parse_float(part) {
                Some(v) => out.push(v),
                None => return Vec::new(),
            }
        }
    }
    out
}

/// Resolve an explicit `[model]{spec}` colour to `RRGGBB` (or `None`).
pub fn color_from_model(model: &str, spec: &str) -> Option<String> {
    let model = pystr::strip(model);
    let spec = pystr::strip(spec);
    if model == "HTML" {
        let h = pystr::lstrip_chars(pystr::strip(spec), "#");
        if HEX6.fullmatch(h).is_some() {
            return Some(pystr::upper(h));
        }
        if HEX3.fullmatch(h).is_some() {
            let doubled: String = h.chars().flat_map(|c| [c, c]).collect();
            return Some(pystr::upper(&doubled));
        }
        return None;
    }
    let nums = floats(spec);
    match (model, nums.len()) {
        ("rgb", 3) => Some(hex(nums[0] * 255.0, nums[1] * 255.0, nums[2] * 255.0)),
        ("RGB", 3) => Some(hex(nums[0], nums[1], nums[2])),
        ("gray", 1) => {
            let v = nums[0] * 255.0;
            Some(hex(v, v, v))
        }
        ("cmyk", 4) => {
            let (c, m, y, k) = (nums[0], nums[1], nums[2], nums[3]);
            Some(hex(
                255.0 * (1.0 - c) * (1.0 - k),
                255.0 * (1.0 - m) * (1.0 - k),
                255.0 * (1.0 - y) * (1.0 - k),
            ))
        }
        _ => None,
    }
}

/// Named-colour registry seeded with xcolor defaults, extended by
/// `\definecolor`/`\colorlet`.
#[derive(Debug, Clone)]
pub struct ColorTable {
    named: HashMap<String, String>,
}

impl Default for ColorTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ColorTable {
    pub fn new() -> Self {
        ColorTable {
            named: NAMED.iter().map(|&(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    /// `\definecolor{name}{model}{spec}`.
    pub fn define(&mut self, name: &str, model: &str, spec: &str) {
        if let Some(hexval) = color_from_model(model, spec) {
            self.named.insert(pystr::lower(pystr::strip(name)), hexval);
        }
    }

    /// `\colorlet{name}{target}` -- copy an existing colour.
    pub fn define_alias(&mut self, name: &str, target: &str) {
        if let Some(resolved) = self.resolve(target, None) {
            self.named.insert(pystr::lower(pystr::strip(name)), resolved);
        }
    }

    /// Resolve `{spec}` (a name, `!`-mix, or hex) to `RRGGBB`; with a model,
    /// resolve `[model]{spec}`.
    pub fn resolve(&self, spec: &str, model: Option<&str>) -> Option<String> {
        if let Some(model) = model.filter(|m| !m.is_empty()) {
            return color_from_model(model, spec);
        }
        let spec = pystr::strip(spec);
        if spec.contains('!') {
            return self.resolve_mix(spec);
        }
        self.resolve_name(spec)
    }

    fn resolve_name(&self, spec: &str) -> Option<String> {
        let key = pystr::lower(pystr::strip(spec));
        if let Some(v) = self.named.get(&key) {
            return Some(v.clone());
        }
        if HASH_HEX6.fullmatch(pystr::strip(spec)).is_some() {
            return Some(pystr::upper(pystr::lstrip_chars(pystr::strip(spec), "#")));
        }
        None
    }

    fn rgb(&self, name: &str) -> Option<[f64; 3]> {
        let h = self.resolve_name(name)?;
        let byte = |i: usize| i64::from_str_radix(&h[i..i + 2], 16).unwrap_or(0) as f64;
        Some([byte(0), byte(2), byte(4)])
    }

    /// An xcolor `!`-mix, e.g. `blue!8` = 8% blue + 92% white, `red!40!blue`
    /// = 40% red + 60% blue (left-associative fold).
    fn resolve_mix(&self, spec: &str) -> Option<String> {
        let parts: Vec<&str> = spec.split('!').map(pystr::strip).collect();
        let mut cur = self.rgb(parts[0])?;
        let white = [255.0, 255.0, 255.0];
        let mut i = 1;
        while i < parts.len() {
            let frac = pystr::parse_float(parts[i])? / 100.0;
            let other = if i + 1 < parts.len() { self.rgb(parts[i + 1])? } else { white };
            for k in 0..3 {
                cur[k] = frac * cur[k] + (1.0 - frac) * other[k];
            }
            i += 2;
        }
        Some(hex(cur[0], cur[1], cur[2]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_and_mixes() {
        assert_eq!(color_from_model("rgb", "0.5, 0.5, 0.5").as_deref(), Some("808080"));
        assert_eq!(color_from_model("HTML", "#abc").as_deref(), Some("AABBCC"));
        assert_eq!(color_from_model("cmyk", "0,1,1,0").as_deref(), Some("FF0000"));
        let t = ColorTable::new();
        assert_eq!(t.resolve("blue!8", None).as_deref(), Some("EBEBFF"));
        assert_eq!(t.resolve("red!40!blue", None).as_deref(), Some("660099"));
        assert_eq!(t.resolve("nosuch", None), None);
    }
}
