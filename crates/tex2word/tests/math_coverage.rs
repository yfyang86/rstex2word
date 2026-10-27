//! End-to-end math coverage: a document exercising the extended symbol
//! tables and every fixed construct must convert entirely through the direct
//! OMML path -- no raw-LaTeX fallback, no warnings -- produce the expected
//! OMML structures, validate cleanly and round-trip to LaTeX.

use tex2word::roundtrip::to_latex;
use tex2word::{convert_source, ConvertOptions};
use tex2word_ooxml::zip::Archive;

/// One inline equation per line, plus one display equation below: 26 oMath.
const SRC: &str = r"\documentclass{article}
\usepackage{amsmath,amssymb}
\begin{document}
$\mathcal{L} \le \mu$
$\mathbb{R}^n$, $\mathfrak{g}$, $\mathbf{v}$
$\bigl(\frac{1}{2}\bigr)$
$\left\lVert A x \right\rVert$
$\boxed{a=b}$
$a \not\le b$
$\overset{\text{def}}{=}$
$\underbrace{ab}_{c}$
$\prescript{a}{2}{X}$
$\sideset{_a^b}{^c}\sum_i$
$\genfrac{(}{)}{0pt}{}{a}{b}$
$\dbinom{n}{k}$
$x \pod{n}$
$\smash{x}$
$a \leqslant b \nsubseteq c$
$\therefore P \land \lnot Q$
$\varGamma \to \varOmega$
$\hat{\imath} + \hat{\jmath}$
$a \text{ if } b$
$A \Longrightarrow B \Longleftrightarrow C$
$\dotsc, \dotsb$
$\beth, \gimel, \daleth$
$\square \blacksquare \lozenge$
$a \upuparrows b \rightleftharpoons c$
$x \mapsto f(x)$
\begin{equation}\mathcal{F} = \bigl\lvert \psi \bigr\rvert\end{equation}
\end{document}
";

/// 27 inline `$...$` snippets -- line 2 holds three -- plus one display line.
const EXPECTED_OMML: i64 = 28;

#[test]
fn extended_math_converts_without_fallbacks() {
    let r = convert_source(SRC, ".", &ConvertOptions::default()).unwrap();
    assert_eq!(
        r.report.math_raw,
        0,
        "raw fallbacks: {:?}",
        r.report.warnings()
    );
    assert_eq!(r.report.math_image, 0);
    assert_eq!(r.report.math_omml, EXPECTED_OMML);
    assert!(
        r.report.warnings().is_empty(),
        "warnings: {:?}",
        r.report.warnings()
    );

    let problems = tex2word_validate::validate_docx(&r.docx).unwrap();
    assert_eq!(problems, Vec::<String>::new(), "docx must validate");

    let archive = Archive::new(&r.docx).unwrap();
    let xml = String::from_utf8(archive.read("word/document.xml").unwrap().to_vec()).unwrap();
    // the structures behind the fixes
    for marker in [
        "<m:borderBox>",                   // \boxed
        "<m:limUpp>",                      // \overset
        "<m:sPre>",                        // \prescript / \sideset
        "≰",                               // \not\le
        "⩽",                               // \leqslant
        "∴",                               // \therefore
        "𝛤",                               // \varGamma
        "ℶ",                               // \beth
        "ı",                               // \imath
        r#"m:begChr m:val="(""#,           // \bigl( pairing
        r#"m:begChr m:val="‖""#,           // \left\lVert
        r#"xml:space="preserve"> </m:t>"#, // \text{ if }
    ] {
        assert!(xml.contains(marker), "document.xml lacks {marker}");
    }
}

#[test]
fn extended_math_round_trips() {
    let r = convert_source(SRC, ".", &ConvertOptions::default()).unwrap();
    let tex = to_latex(&r.docx, true).unwrap().expect("round trip");
    for marker in [
        r"\boxed{a=b}",
        r"\bigl(",
        r"\leqslant",
        r"\therefore",
        r"\prescript{a}{2}{X}",
        r"\varGamma",
        r"\not\le",
    ] {
        assert!(tex.contains(marker), "round trip lost {marker}:\n{tex}");
    }
}
