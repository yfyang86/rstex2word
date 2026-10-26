//! A Rust port of [pylatexenc](https://github.com/phfaist/pylatexenc) 2.11's
//! LaTeX walker, as used by tex2word.
//!
//! The Python tex2word reference parses LaTeX by walking pylatexenc's node
//! tree. Porting its parser faithfully requires exactly the same tree — node
//! boundaries, argument grouping, `latex_verbatim` spans, and tolerant-mode
//! recovery from malformed input — so this crate mirrors pylatexenc's
//! `latexwalker` and `macrospec` modules closely, including the default macro
//! database (generated from the Python one) and Python's Unicode character
//! classes.
//!
//! ```
//! use std::sync::Arc;
//! use tex2word_latexwalker::{ContextDb, LatexWalker};
//!
//! let w = LatexWalker::new(r"Hi \emph{there}!", Arc::new(ContextDb::default_db()));
//! let nodes = w.get_latex_nodes_all();
//! assert_eq!(nodes[1].macroname(), Some("emph"));
//! assert_eq!(nodes[1].latex_verbatim(), r"\emph{there}");
//! ```

pub mod chars;
mod default_specs;
pub mod latex2text;
mod latex2text_specs;
mod node;
mod spec;

mod walker;

pub use node::{Node, NodeKind, ParsedArgs};
pub use spec::{ArgsParser, ContextDb, EnvironmentSpec, MacroSpec, SpecialsSpec, VerbatimKind};
pub use walker::{LatexWalker, ParsingState, Stop, WalkError};
