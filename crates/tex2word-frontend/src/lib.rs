//! LaTeX front-end: parse LaTeX source into the tex2word IR (a port of the
//! Python reference's `frontend` package).

pub mod algorithms;
pub mod colors;
pub mod macros;
pub mod preprocess;
pub mod siunitx;

/// Re-export of the IR crate, so callers can name the parser's types without
/// depending on `tex2word-ir` separately.
pub use tex2word_ir as ir;
