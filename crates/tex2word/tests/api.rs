//! Smoke tests of the public API: convert, validate, read back.

use tex2word::roundtrip::{recover_ir, to_latex};
use tex2word::{convert_source, ConvertOptions};

const SRC: &str = r"\documentclass{article}
\title{Smoke}
\begin{document}
\maketitle
\section{Intro}\label{sec:intro}
Euler: $e^{i\pi} + 1 = 0$, see Section~\ref{sec:intro}.
\begin{equation}\label{eq:a}
\int_0^1 x^2\,dx = \frac{1}{3}
\end{equation}
\end{document}
";

#[test]
fn converts_to_a_valid_docx() {
    let r = convert_source(SRC, ".", &ConvertOptions::default()).unwrap();
    assert!(r.docx.starts_with(b"PK"));
    assert_eq!(tex2word_validate::validate_docx(&r.docx).unwrap(), Vec::<String>::new());
    assert_eq!(r.report.math_omml, 2);
}

#[test]
fn round_trips_through_the_manifest() {
    let r = convert_source(SRC, ".", &ConvertOptions::default()).unwrap();
    let ir = recover_ir(&r.docx).unwrap().expect("manifest");
    assert_eq!(ir.blocks, r.document.blocks);
    let tex = to_latex(&r.docx, true).unwrap().unwrap();
    assert!(tex.contains("\\section{Intro}\\label{sec:intro}"), "{tex}");
    assert!(tex.contains("\\begin{equation}"), "{tex}");
}

#[test]
fn reads_a_docx_without_manifest() {
    let opts = ConvertOptions {
        embed_manifest: false,
        ..ConvertOptions::default()
    };
    let r = convert_source(SRC, ".", &opts).unwrap();
    let tex = to_latex(&r.docx, true).unwrap().unwrap();
    assert!(tex.contains("\\section{Intro}"), "{tex}");
    assert!(tex.contains("\\frac{1}{3}"), "{tex}");
}
