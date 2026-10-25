//! tex2word math engine: LaTeX math -> OMML (a port of the Python reference's
//! `mathml` package): the math parser, the OMML emitter, the symbol tables
//! and the per-node fallback cascade.

pub mod cascade;
pub mod imagemath;
pub mod latex_math;
pub mod omml;
pub mod symbols;

pub use latex_math::MathError;
