//! A running-head header + page-number footer (port of
//! `backend/runninghead.py`).

use crate::data::{FOOTER_XML, HEADER_TMPL};

/// `xml.sax.saxutils.escape`.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn header_xml(running_head: &str) -> Vec<u8> {
    HEADER_TMPL
        .replace("{text}", &escape(running_head))
        .into_bytes()
}

pub fn footer_xml() -> Vec<u8> {
    FOOTER_XML.as_bytes().to_vec()
}
