//! BibTeX parsing (port of `bib/bibtex.py`). (Being ported.)

use tex2word_ir::{CSLItem, Dict};

pub fn parse_bibtex(_source: &str) -> Dict<CSLItem> {
    Dict::new()
}
