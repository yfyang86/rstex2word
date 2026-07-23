//! tex2word — LaTeX → Microsoft Word (`.docx`) with native OMML math.
//!
//! This crate ties the front-end (LaTeX → IR) and back-end (IR → OOXML) into a
//! one-call pipeline: [`convert_source`] / [`convert_file`] take LaTeX and
//! return a real, valid `.docx` (headings, lists, tables, floats, citations,
//! footnotes, cross-references, and math Word edits natively). The ZIP
//! container, XML, and LaTeX→OMML engine are all hand-written, with no external
//! crates. See the `ROADMAP.md` at the repository root for the feature surface.

pub use tex2word_backend::PageGeometry;
pub use tex2word_ir as ir;

pub mod crossref;
pub mod report;

use std::fs;
use std::io;
use std::path::Path;

pub use crossref::Warning;
pub use report::Coverage;

/// Convert LaTeX source to a normalized `.tex` string via the IR (round-trip
/// writer). Useful for differential testing and re-emitting a canonical form.
pub fn to_latex_source(source: &str) -> String {
    let document = tex2word_frontend::parse_document(source);
    tex2word_latex::to_latex(&document)
}

/// The result of a conversion: the `.docx` bytes, the parsed IR, any non-fatal
/// warnings (e.g. unresolved cross-references), and a coverage report.
pub struct Conversion {
    pub docx: Vec<u8>,
    pub document: ir::Document,
    pub warnings: Vec<Warning>,
    pub coverage: Coverage,
}

/// Convert LaTeX source to a `.docx` byte buffer (+ the IR + warnings + coverage).
pub fn convert_source(source: &str) -> Conversion {
    let (mut document, unsupported) =
        tex2word_frontend::parse_document_reporting(source, Path::new("."));
    let mut warnings = crossref::resolve(&mut document);
    warnings.extend(unsupported_warnings(&unsupported));
    let coverage = report::coverage(&document, &unsupported);
    let docx = tex2word_backend::to_docx(&document, Path::new("."));
    Conversion {
        docx,
        document,
        warnings,
        coverage,
    }
}

/// Map unsupported-macro names to warnings.
fn unsupported_warnings(macros: &[String]) -> Vec<Warning> {
    macros
        .iter()
        .map(|m| Warning {
            context: "unsupported".into(),
            message: format!("macro '\\{m}' is not supported (dropped)"),
        })
        .collect()
}

/// Convert a `.tex` file to a `.docx` on disk. Returns the output path used and
/// any conversion warnings. `\input`/`\include` files are resolved relative to
/// the input file's directory.
pub fn convert_file(
    input: &Path,
    output: Option<&Path>,
    page: &PageGeometry,
) -> io::Result<(std::path::PathBuf, Vec<Warning>, Coverage)> {
    let source = fs::read_to_string(input)?;
    let base = input
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let (mut document, unsupported) = tex2word_frontend::parse_document_reporting(&source, base);
    let mut warnings = crossref::resolve(&mut document);
    warnings.extend(unsupported_warnings(&unsupported));
    let coverage = report::coverage(&document, &unsupported);
    let docx = tex2word_backend::to_docx_with(&document, base, page);
    let out = match output {
        Some(p) => p.to_path_buf(),
        None => input.with_extension("docx"),
    };
    fs::write(&out, &docx)?;
    Ok((out, warnings, coverage))
}
