//! tex2word back-end: IR -> `.docx` (a port of the Python reference's
//! `backend` package, plus its `templates` and the Zotero field writer).

pub mod data;
pub mod document;
pub mod fields;
pub mod images;
pub mod package;
pub mod raster;
pub mod reference;
pub mod runninghead;
pub mod templates;
pub mod tikz;
pub mod zotero;

pub use document::{DocumentWriter, WriterOptions};
pub use package::{numbering_xml, DocxPackage};
