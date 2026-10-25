//! tex2word: LaTeX -> Microsoft Word (`.docx`) with native OMML math.
//!
//! A port of the Python `tex2word` package (core install). The entry points
//! are [`convert_source`] and [`convert_file`].

pub mod difflib;
pub mod pipeline;
pub mod roundtrip;

pub use pipeline::{convert_file, convert_source, ConversionResult, ConvertOptions, Error};
pub use tex2word_ir as ir;
