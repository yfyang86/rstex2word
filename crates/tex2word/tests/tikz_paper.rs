//! Acceptance tests for TikZ-heavy papers.
//!
//! `fixtures/tikz_paper` is a small synthetic paper (MIT, written for this
//! repository) that reproduces what broke on arXiv:2610.32813: TikZ figures
//! kept in `\input` files, two pictures stacked in one `figure*`, captions
//! built with `\protect\input`, nested `\input` paths relative to the main
//! file, a heading with two `\label`s, `\path`, natbib `[numbers]` with
//! `plainnat`, and `\nobalance`/`\typeout`/`\onecolumn` end matter.
//!
//! TikZ rendering needs a TeX engine and `pdftoppm`. Without them the test
//! checks the graceful path instead; set `T2W_REQUIRE_TIKZ=1` (as CI's TikZ
//! job does) to fail when they are missing.
//!
//! `arxiv_2610_32813` runs the same checks on the real paper. Its source is
//! under arXiv's non-exclusive license, so it is not in the repository:
//! download and extract the e-print, then point `T2W_UAT_ARXIV_2610_32813`
//! at the directory holding `main.tex`.

mod common;

use common::{convert_dir, fixture_dir, package_parts, tikz_tools, warnings};
use std::path::Path;
use tex2word_ir::json;
use tex2word_ir::ToJson;

#[test]
fn tikz_paper_fixture() {
    let r = convert_dir(&fixture_dir("tikz_paper"));
    let ir = json::dumps(&r.document.to_json(), Default::default());
    let (media, widths, xml) = package_parts(&r.docx);

    // nothing degraded to raw LaTeX: \path, \typeout, \onecolumn, ...
    assert!(!ir.contains("RawInline"), "{ir}");
    // \protect\input in captions inlines the file's text, not \protectText
    assert!(
        ir.contains("Pipeline overview. Each stage is one crate"),
        "{ir}"
    );
    assert!(
        ir.contains("Quality and cost of the three variants"),
        "{ir}"
    );
    assert!(!ir.contains("protectEach") && !ir.contains("protectQuality"));
    // nested \input{generated/...} from generated/*.tex: the table arrives
    assert!(ir.contains("\"tab:results\""), "{ir}");
    // the figure keeps its own \label, not the \foreach variable \label
    assert!(ir.contains("\"fig:bars\""), "{ir}");
    // \path renders as monospace text
    assert!(xml.contains("generated/results_raw.json"));
    // natbib [numbers] beats plainnat's author-year default
    assert!(xml.contains("[1]"), "numeric citations expected");

    let rendered = tikz_tools();
    let w = warnings(&r);
    if rendered {
        // every reference resolves (incl. the second \label of a heading)
        // and both figures render: 1 + 2 stacked pictures, none wider than
        // the text column (6.5in = 5943600 EMU on letter paper, 1in margins)
        assert!(w.is_empty(), "{w:#?}");
        assert_eq!(media.len(), 3, "{media:?}");
        assert_eq!(widths.len(), 3);
        assert!(
            widths.iter().all(|&cx| cx > 0 && cx <= 5_943_600),
            "{widths:?}"
        );
    } else {
        // graceful degradation: only the TikZ notes, never a dropped ref
        assert!(media.is_empty(), "{media:?}");
        assert!(
            w.iter().all(|m| m.starts_with("figure: ")),
            "unexpected warnings: {w:#?}"
        );
        assert!(
            w.iter().any(|m| m.contains("TikZ figure not rendered")),
            "{w:#?}"
        );
    }
}

#[test]
fn arxiv_2610_32813() {
    let Some(dir) = std::env::var_os("T2W_UAT_ARXIV_2610_32813") else {
        eprintln!("skipped: set T2W_UAT_ARXIV_2610_32813 to the extracted e-print");
        return;
    };
    let r = convert_dir(Path::new(&dir));
    assert_eq!(r.report.math_omml, 162);
    assert_eq!(r.report.math_raw, 0);
    let ir = json::dumps(&r.document.to_json(), Default::default());
    assert!(!ir.contains("RawInline"), "raw LaTeX left in the IR");
    let (media, _, _) = package_parts(&r.docx);
    let w = warnings(&r);
    if tikz_tools() {
        assert!(w.is_empty(), "{w:#?}");
        // 1 PNG + 1 PDF figure + 4 TikZ pictures (two stacked in Figure 4)
        assert_eq!(media.len(), 6, "{media:?}");
    } else {
        // the TikZ pictures and the PDF figure degrade, nothing else
        assert!(
            w.iter()
                .all(|m| m.starts_with("figure: ") || m.starts_with("includegraphics: ")),
            "{w:#?}"
        );
    }
}
