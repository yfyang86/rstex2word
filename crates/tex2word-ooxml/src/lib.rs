//! OOXML plumbing for tex2word, dependency-free:
//!
//! - [`xml`]: an element tree with lxml's semantics (Clark-notation tags,
//!   `text`/`tail`, ordered attributes, namespace declarations) and a
//!   serializer that reproduces `lxml.etree.tostring` byte for byte, plus
//!   the `el`/`sub` builders of the Python reference's `backend/ooxml.py`;
//! - [`parse`]: an XML parser producing that tree as `etree.fromstring` does;
//! - [`zip`]: reading and writing ZIP archives (stored and deflated members).

pub mod deflate;
pub mod inflate;
pub mod parse;
pub mod xml;
pub mod zip;

pub use xml::{
    el, preserve_space, qn, serialize, sub, text_el, xml_safe, Element, Kind, XmlError, NS,
};
