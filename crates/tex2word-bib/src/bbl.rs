//! `.bbl` parsing (port of `bib/bbl.py`). (Being ported.)

use tex2word_ir::{CSLItem, Dict};

pub fn parse_bbl(_text: &str) -> Dict<CSLItem> {
    Dict::new()
}

pub fn bbl_style(_items: &Dict<CSLItem>) -> String {
    "numeric".into()
}
