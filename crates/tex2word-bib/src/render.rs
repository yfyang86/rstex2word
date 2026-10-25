//! Citation rendering (port of `bib/render.py`). (Being ported.)

use tex2word_ir::{CSLItem, ConversionReport, Dict, Document};

pub fn resolve_citations(
    _doc: &mut Document,
    _items: &Dict<CSLItem>,
    _style: &str,
    _report: &mut ConversionReport,
    _csl_path: Option<&str>,
    _nocite_keys: &[String],
) {
}
