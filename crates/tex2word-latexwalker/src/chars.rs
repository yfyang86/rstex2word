//! Python `str` character predicates (re-exported from `tex2word_ir::pystr`,
//! whose tables are generated from CPython's Unicode database).
//!
//! pylatexenc tokenises with `str.isspace()`/`str.isalpha()` and the regex
//! `\w` class; these differ from Rust's `char` methods at the edges, so the
//! walker uses the exact Python sets.

pub use tex2word_ir::pystr::{is_alnum, is_alpha, is_space, is_word};
