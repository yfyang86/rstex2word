//! Acceptance tests for amsart-style manuscripts.
//!
//! `fixtures/amsart_paper` is a small synthetic paper (MIT, written for this
//! repository) reproducing what tex2word 2.2.0 got wrong on an amsart
//! economics manuscript: a `\paragraph` redefined through `\@startsection`,
//! `\subjclass[JEL]`, citations and `\ref`s inside table cells, captions and
//! theorem-like blocks, a landscape table built from
//! `\rotatebox{90}{minipage + \captionof{table}}` with `\makecell` headers,
//! and display math with sums, `\\[2pt]` row spacing, `\DeclareMathOperator`,
//! `\mathrm{Beta}` and minus signs.
//!
//! Embedding the PDF figure needs `pdftoppm`; without it the test checks the
//! graceful path. `scissors_manuscript` runs the same checks on the original
//! manuscript when `T2W_UAT_SCISSORS` points at its directory (it is not
//! distributed with the repository).

mod common;

use std::path::Path;

use common::{body_text, convert_dir, fixture_dir, ir_json, package_parts, warnings};
use tex2word::ConversionResult;

/// What every converted amsart paper must satisfy, whatever its content.
fn assert_clean_math_and_refs(r: &ConversionResult, bib_keys: &[String]) {
    let ir = ir_json(r);
    let (_, _, xml) = package_parts(&r.docx);
    let text = body_text(&xml);
    assert!(!ir.contains("RawInline"), "raw LaTeX left in the IR");
    assert_eq!(r.report.math_raw, 0, "equations fell back to raw LaTeX");
    // citations and references resolve everywhere, tables and captions too
    for key in bib_keys {
        assert!(
            !text.contains(key.as_str()),
            "raw citation key {key:?} in the text"
        );
    }
    assert!(!text.contains("??"), "unresolved reference rendered as ??");
    // layout macros never leak as text
    for junk in [
        "startsection",
        "baselineskip",
        "makecell",
        "rotatebox",
        "captionof",
    ] {
        assert!(!text.contains(junk), "{junk} leaked into the text");
    }
    // every n-ary operator has its operand (an empty body is a dotted box in
    // Word), and row spacing like \\[2pt] stays out of the math
    assert!(!xml.contains("<m:e/>"), "n-ary operator with an empty body");
    assert!(!xml.contains("pt</m:t>"), "row spacing leaked into math");
}

fn bib_keys(dir: &Path) -> Vec<String> {
    let bib = std::fs::read_to_string(dir.join("refs.bib")).unwrap_or_default();
    bib.split('@')
        .skip(1)
        .filter_map(|e| e.split_once('{').and_then(|(_, rest)| rest.split_once(',')))
        .map(|(key, _)| key.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect()
}

#[test]
fn amsart_paper_fixture() {
    let dir = fixture_dir("amsart_paper");
    let r = convert_dir(&dir);
    assert_clean_math_and_refs(&r, &bib_keys(&dir));
    let ir = ir_json(&r);
    let (media, _, xml) = package_parts(&r.docx);
    let text = body_text(&xml);

    // \subjclass[JEL] prints with the label from \@namedef{subjclassname@JEL}
    assert!(text.contains("JEL classification:"), "{text}");
    assert!(text.contains("D91, E21, J63"));
    // the redefined \paragraph is still a run-in heading, with its citation
    assert!(text.contains("Reference-dependent adoption"));
    // citations in cells, captions and the design rule render as author-year
    assert!(text.matches("Tversky").count() >= 4, "{text}");
    assert!(text.matches("Laibson").count() >= 4, "{text}");
    // the landscape table keeps its \captionof caption and \label
    assert!(
        ir.contains("\"tab:results\""),
        "rotated table lost its label"
    );
    assert!(text.contains("Stress-test results"));
    // \makecell headers break lines inside the cell
    let cell = &xml[xml.find("Peak junior").expect("makecell header")..];
    assert!(
        cell[..200].contains("<w:br/>"),
        "makecell line break missing"
    );
    // math: minus signs, operator names as words, sums own their operands
    assert!(xml.contains(">\u{2212}<"), "minus rendered as a hyphen");
    assert!(xml.contains(">Beta<") && xml.contains(">rel<"));
    assert!(xml.matches("<m:nary>").count() >= 3);

    let w = warnings(&r);
    if tex2word_backend::raster::has_pdf_support() {
        assert!(w.is_empty(), "{w:#?}");
        assert_eq!(media.len(), 1, "the PDF figure is embedded");
    } else {
        // only the figure degrades, and the warning names the tool to install
        assert_eq!(w.len(), 1, "{w:#?}");
        assert!(w[0].contains("pdftoppm"), "{w:#?}");
    }
}

#[test]
fn scissors_manuscript() {
    let Some(dir) = std::env::var_os("T2W_UAT_SCISSORS") else {
        eprintln!("skipped: set T2W_UAT_SCISSORS to the manuscript directory");
        return;
    };
    let dir = Path::new(&dir);
    let r = convert_dir(dir);
    assert_clean_math_and_refs(&r, &bib_keys(dir));
    assert!(r.report.math_omml >= 362, "math: {}", r.report.math_omml);
    let (media, _, _) = package_parts(&r.docx);
    let w = warnings(&r);
    if tex2word_backend::raster::has_pdf_support()
        && !tex2word_backend::tikz::available_engines().is_empty()
    {
        assert!(w.is_empty(), "{w:#?}");
        // three PDF figures and the TikZ diagram
        assert_eq!(media.len(), 4, "{media:?}");
    } else {
        assert!(
            w.iter()
                .all(|m| m.starts_with("figure: ") || m.starts_with("includegraphics: ")),
            "{w:#?}"
        );
    }
}
