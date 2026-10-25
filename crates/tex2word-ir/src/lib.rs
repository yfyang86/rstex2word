//! tex2word intermediate representation (IR).
//!
//! The IR is the contract between the front-end (LaTeX parsing) and the
//! back-ends (OOXML generation): a typed, format-neutral document tree. It is a
//! one-to-one port of the Python reference's `ir.py` — same node types, same
//! fields, same defaults and the same JSON form — so a document serialised by
//! either implementation (for instance into a `.docx` round-trip manifest)
//! deserialises in the other.
//!
//! The crate also hosts the pieces every other crate needs:
//!
//! * [`json`] — a JSON value type whose serializer reproduces Python's
//!   `json.dumps` byte-for-byte;
//! * [`report`] — the [`ConversionReport`] sink for warnings and coverage;
//! * [`crossref`] — the cross-reference resolution transform (IR -> IR);
//! * [`pystr`] — Python `str` semantics the ported code relies on;
//! * [`re`] — a regex engine compatible with Python's `re` module;
//! * [`pyos`] — `os.path` and text-file reading with Python semantics.

pub mod crossref;
pub mod dict;
pub mod json;
mod nodes;
pub mod pyos;
pub mod pystr;
pub mod re;
pub mod report;
mod unicode_norm;
mod unicode_tables;

pub use dict::Dict;
pub use json::{FromJson, JsonError, ToJson, Value};
pub use nodes::*;
pub use report::{ConversionReport, ReportEntry};
