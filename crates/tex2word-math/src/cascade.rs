//! The math decision cascade (port of `mathml/cascade.py`).
//!
//! Per math node, try in order:
//!   1. direct LaTeX -> OMML (the primary path)
//!   2. LaTeX -> presentation MathML -> OMML (needs the optional
//!      `latex2mathml` extra; never enabled in the core install)
//!   3. image fallback (pluggable renderer; off by default)
//!   4. raw LaTeX text (last resort)
//!
//! Every node records the path it took on the [`ConversionReport`].

use std::collections::HashMap;

use tex2word_ir::ConversionReport;
use tex2word_ooxml::xml::{check_xml_string, Element};

use crate::latex_math::MathError;
use crate::omml;

/// Renders a math snippet to an image: `(bytes, format)` or `None`.
pub trait ImageMathRenderer {
    fn render(&mut self, latex: &str, display: bool) -> Option<(Vec<u8>, String)>;
}

/// The outcome of the cascade for one node.
#[derive(Debug, Clone, PartialEq)]
pub enum MathResult {
    /// one or more `m:oMath` elements (block) or one (inline)
    Omml(Vec<Element>),
    /// image bytes and format to embed
    Image(Vec<u8>, String),
    /// the source LaTeX to render as text
    Raw(String),
}

/// Drives the per-node cascade and updates the report.
pub struct MathCascade {
    pub image_renderer: Option<Box<dyn ImageMathRenderer>>,
    image_cache: HashMap<(String, bool), Option<(Vec<u8>, String)>>,
    /// East-Asian font for CJK glyphs inside math (`None` leaves runs untagged)
    pub cjk_font: Option<String>,
}

/// lxml raises `ValueError` while *building* the tree when a string isn't
/// XML compatible; Rust checks the finished subtree instead.
fn check_tree(e: &Element) -> Result<(), MathError> {
    let mut bad = None;
    let mut visit = |s: &str| {
        if bad.is_none() {
            if let Err(err) = check_xml_string(s) {
                bad = Some(err.0);
            }
        }
    };
    fn walk(e: &Element, f: &mut dyn FnMut(&str)) {
        if let Some(t) = &e.text {
            f(t);
        }
        for a in &e.attrs {
            f(&a.value);
        }
        for c in &e.children {
            walk(c, f);
        }
    }
    walk(e, &mut visit);
    match bad {
        Some(message) => Err(MathError::Other { type_name: "ValueError".into(), message }),
        None => Ok(()),
    }
}

impl Default for MathCascade {
    fn default() -> Self {
        Self::new(None)
    }
}

impl MathCascade {
    pub fn new(image_renderer: Option<Box<dyn ImageMathRenderer>>) -> Self {
        MathCascade {
            image_renderer,
            image_cache: HashMap::new(),
            cjk_font: None,
        }
    }

    pub fn inline(&mut self, latex: &str, report: &mut ConversionReport) -> MathResult {
        self.run(latex, false, true, report)
    }

    pub fn block(&mut self, latex: &str, collapse_align: bool, report: &mut ConversionReport) -> MathResult {
        self.run(latex, true, collapse_align, report)
    }

    /// Tag CJK math runs with the document font, count, and wrap.
    fn omml_result(&self, mut omath: Vec<Element>, report: &mut ConversionReport) -> MathResult {
        if let Some(font) = &self.cjk_font {
            for e in omath.iter_mut() {
                omml::tag_cjk_runs(e, font);
            }
        }
        report.math_omml += omath.len() as i64;
        MathResult::Omml(omath)
    }

    fn run(&mut self, latex: &str, display: bool, collapse_align: bool, report: &mut ConversionReport) -> MathResult {
        // 1. direct OMML
        let direct = if display {
            omml::render_block_lines(latex, collapse_align)
        } else {
            omml::render_inline(latex).map(|e| vec![e])
        }
        .and_then(|v| {
            for e in &v {
                check_tree(e)?;
            }
            Ok(v)
        });
        let construct = match direct {
            Ok(omath) => return self.omml_result(omath, report),
            Err(MathError::Unsupported { construct, .. }) => construct,
            Err(MathError::Other { type_name, message }) => {
                // never let a parser bug abort the conversion
                report.warn("math", format!("direct math parse failed ({message}); falling back"));
                type_name
            }
        };

        // 2. presentation MathML: the latex2mathml extra isn't available

        // 3. image fallback, memoised by (latex, display)
        if self.image_renderer.is_some() {
            let key = (latex.to_string(), display);
            let rendered = match self.image_cache.get(&key) {
                Some(r) => r.clone(),
                None => {
                    let r = self.image_renderer.as_mut().unwrap().render(latex, display);
                    self.image_cache.insert(key, r.clone());
                    r
                }
            };
            if let Some((bytes, fmt)) = rendered {
                report.math_image += 1;
                report.info("math", format!("rendered '{construct}' via image fallback"));
                return MathResult::Image(bytes, fmt);
            }
        }

        // 4. raw
        report.math_raw += 1;
        report.warn("math", format!("fell back to raw LaTeX for construct {construct}"));
        MathResult::Raw(latex.to_string())
    }
}
