//! Math coverage tests: every command in the symbol tables must parse and
//! render to OMML, the rendering fixes keep their OMML structure, and the
//! new constructs round-trip through the OMML reader.

use tex2word_math::latex_math;
use tex2word_math::omml;
use tex2word_math::omml_reader;
use tex2word_math::symbols as S;
use tex2word_ooxml::xml::{tostring, ToString};

/// Render inline math to its OMML string (utf8, no declaration).
fn inline(latex: &str) -> String {
    let e = omml::render_inline(latex).unwrap_or_else(|e| panic!("{latex}: {e}"));
    tostring(
        &e,
        ToString {
            utf8: true,
            ..Default::default()
        },
    )
    .unwrap()
}

/// Commands that may appear bare; skips the single-character delimiter keys
/// the tokenizer never produces as commands.
fn is_command_key(k: &str) -> bool {
    k.chars().all(|c| c.is_ascii_alphabetic()) && !k.is_empty()
}

#[test]
fn every_symbol_entry_parses() {
    let mut missing = Vec::new();
    for (k, _) in S::SYMBOLS
        .iter()
        .chain(S::SPACING)
        .filter(|(k, _)| is_command_key(k))
    {
        if latex_math::parse(&format!("a \\{k} b")).is_err() {
            missing.push(*k);
        }
    }
    assert!(
        missing.is_empty(),
        "unparsable SYMBOLS/SPACING entries: {missing:?}"
    );
}

#[test]
fn every_nary_entry_parses_with_limits() {
    let mut missing = Vec::new();
    for (k, _) in S::NARY {
        if latex_math::parse(&format!(r"a \{k}_{{i=1}}^{{n}} b")).is_err() {
            missing.push(*k);
        }
    }
    assert!(missing.is_empty(), "unparsable NARY entries: {missing:?}");
}

#[test]
fn every_accent_entry_parses() {
    let mut missing = Vec::new();
    for (k, _) in S::ACCENTS {
        if latex_math::parse(&format!(r"\{k}{{x}}")).is_err() {
            missing.push(*k);
        }
    }
    assert!(
        missing.is_empty(),
        "unparsable ACCENTS entries: {missing:?}"
    );
}

#[test]
fn every_delimiter_entry_parses_bare_and_after_left() {
    let mut missing = Vec::new();
    for (k, _) in S::DELIMITERS {
        if !is_command_key(k) {
            continue;
        }
        if latex_math::parse(&format!(r"a \{k} b")).is_err() {
            missing.push(format!("bare \\{k}"));
        }
        if latex_math::parse(&format!(r"\left\{k} a \right)")).is_err() {
            missing.push(format!("left \\{k}"));
        }
    }
    assert!(
        missing.is_empty(),
        "unparsable DELIMITERS entries: {missing:?}"
    );
}

/// The amssymb/amsmath families that used to fall back to raw LaTeX.
#[test]
fn extended_families_parse() {
    let probes = [
        r"a \leqslant b \geqslant c",
        r"a \nleq b \ngeq c \nless d \ngtr e",
        r"\therefore p \because q",
        r"x \subsetneq S \subseteqq T \nsubseteq U",
        r"a \precneqq b \succneqq c \precsim d",
        r"\nvdash p \nvDash q \nVdash r \nVDash s",
        r"\blacksquare \square \bigstar",
        r"a \boxplus b \boxminus c \boxtimes d \boxdot e",
        r"\beth \gimel \daleth \Bbbk \Finv \Game",
        r"\vartriangle \triangleleft \triangleright \blacktriangleleft",
        r"a_1 \dotsc a_2 \dotsb \dotsm \dotso \dotsi",
        r"\varGamma + \varDelta + \varTheta + \varOmega",
        r"A \Longrightarrow B \Longleftarrow C \Longleftrightarrow D",
        r"\hat{\imath} + \hat{\jmath}",
        r"a \nearrow b \searrow c \swarrow d \nwarrow e",
        r"\leftrightharpoons \rightleftharpoons \curvearrowleft",
        r"a \sqsubset b \sqsupset c",
        r"a \wr b \amalg c \Diamond d \diamond e",
        r"x \multimap y \backepsilon z",
        r"a \lesssim b \gtrsim c \lessapprox d \gtrapprox e",
    ];
    for p in probes {
        assert!(latex_math::parse(p).is_ok(), "{p}: should parse");
    }
}

// --------------------------------------------------------------------------- //
// Structure of the fixed renderings
// --------------------------------------------------------------------------- //

#[test]
fn left_lvert_renders_a_norm_delimiter() {
    let x = inline(r"\left\lVert A \right\rVert");
    assert!(x.contains(r#"<m:begChr m:val="‖""#), "{x}");
    assert!(x.contains(r#"<m:endChr m:val="‖""#), "{x}");
    assert!(!x.contains("lVert"), "command name leaked as text: {x}");
}

#[test]
fn bigl_pairs_into_a_stretching_delimiter() {
    let x = inline(r"\bigl(\frac{1}{2}\bigr)");
    assert!(x.contains("<m:d>"), "no m:d: {x}");
    assert!(x.contains(r#"<m:begChr m:val="(""#), "{x}");
    assert!(x.contains("<m:f>"), "{x}");
}

#[test]
fn bare_big_renders_the_delimiter_character() {
    let x = inline(r"\big(\frac{1}{2}\big)");
    assert!(x.contains(">("), "delimiter char missing: {x}");
    assert!(!x.contains("big"), "size hint leaked: {x}");
    let m = inline(r"x \bigm| y");
    assert!(m.contains(">|"), "{m}");
}

#[test]
fn text_keeps_inner_blanks() {
    // the text is one run, its blanks preserved around the word
    let x = inline(r"a \text{ if } b");
    assert!(
        x.contains(r#"xml:space="preserve"> if </m:t>"#),
        "blank runs lost: {x}"
    );
}

#[test]
fn nary_operand_fills_the_body() {
    // TeX's \sum_i e_i \sigma sums e_i \sigma; an empty m:e is a dotted box
    let x = inline(r"\sum_{i} e_i \sigma + 1");
    assert!(!x.contains("<m:e/>"), "empty n-ary body: {x}");
    let body = &x[x.find("<m:sup/>").unwrap()..x.find("</m:nary>").unwrap()];
    assert!(body.contains(">σ<") && !body.contains(">+<"), "{body}");
    // a bracketed operand keeps its inner minus; nested sums nest
    let y = inline(r"\sum_k (m_k - \bar m)\,y_k");
    let body = &y[y.find("<m:sup/>").unwrap()..y.rfind("</m:nary>").unwrap()];
    assert!(body.contains("\u{2212}") && body.contains(">y<"), "{body}");
    let z = inline(r"\sum_i \sum_j a_{ij}");
    assert_eq!(z.matches("<m:nary>").count(), 2);
    assert!(!z.contains("<m:e/>"), "{z}");
}

#[test]
fn minus_is_a_minus_sign_but_text_keeps_hyphens() {
    let x = inline(r"a - b, \text{non-AI}");
    assert!(x.contains(">\u{2212}<"), "{x}");
    assert!(x.contains("non-AI"), "{x}");
}

#[test]
fn row_spacing_does_not_leak() {
    let x = inline(r"\begin{cases} a & b \\[2pt] 0 & c \end{cases}");
    assert!(!x.contains("pt"), "{x}");
    let y = tex2word_math::omml::render_block_lines(r"a &= b \\[4pt] c &= d", false).unwrap();
    assert_eq!(y.len(), 2);
}

#[test]
fn operator_names_are_one_run() {
    let x = inline(r"\mathrm{Beta}(2,3) + \operatorname{rel}_t");
    assert!(x.contains(">Beta<") && x.contains(">rel<"), "{x}");
}

#[test]
fn boxed_renders_a_border_box() {
    assert!(inline(r"\boxed{a=b}").contains("<m:borderBox>"));
}

#[test]
fn not_negations_use_precomposed_glyphs() {
    let x = inline(r"a \not\le b \not\in c \not< d");
    assert!(x.contains("≰"), "{x}");
    assert!(x.contains("∉"), "{x}");
    assert!(x.contains("≮"), "{x}");
    let acc = inline(r"a \not\sqsubseteq b");
    assert!(
        acc.contains("<m:acc>"),
        "unknown base should use an accent: {acc}"
    );
}

#[test]
fn overset_underset_render_limits() {
    assert!(inline(r"\overset{a}{=}").contains("<m:limUpp>"));
    assert!(inline(r"\stackrel{?}{=}").contains("<m:limUpp>"));
    assert!(inline(r"\underset{c}{=}").contains("<m:limLow>"));
    assert!(
        inline(r"\overunderset{a}{c}{=}")
            .matches("<m:limUpp>")
            .count()
            == 1
    );
}

#[test]
fn prescripts_render_spre() {
    assert!(inline(r"\prescript{a}{2}{X}").contains("<m:sPre>"));
    let x = inline(r"\sideset{_a^b}{_c^d}\prod");
    assert!(x.contains("<m:sPre>"), "{x}");
    assert!(x.contains("<m:nary>"), "{x}");
}

#[test]
fn genfrac_binom_pod_smash_parse() {
    for p in [
        r"\genfrac{\{}{\}}{0pt}{}{a}{b}",
        r"\dbinom{n}{k} + \tbinom{n}{k}",
        r"x \pod{n}",
        r"\smash[b]{x} + \smash{x}",
        r"\idotsint_a^b f",
        r"a \nmid b \nsim c \ncong d",
    ] {
        assert!(latex_math::parse(p).is_ok(), "{p}");
    }
}

#[test]
fn unknown_delimiter_errors_instead_of_garbage() {
    // a command the tables don't know must fail cleanly (raw fallback),
    // never leak its name as the rendered delimiter
    let e = latex_math::parse(r"\left\notatable a \right)").unwrap_err();
    assert!(format!("{e}").contains("delimiter"), "{e}");
}

// --------------------------------------------------------------------------- //
// Round trip through the OMML reader
// --------------------------------------------------------------------------- //

#[test]
fn fixed_constructs_round_trip() {
    for (latex, expect) in [
        (r"\boxed{a=b}", r"\boxed{a=b}"),
        (r"\overset{a}{=}", r"\overset{a}{=}"),
        (r"\underset{c}{=}", r"=_{c}"),
        (r"\prescript{a}{2}{X}", r"\prescript{a}{2}{X}"),
        (r"\left\lvert a \right\rvert", r"\left| a \right|"),
    ] {
        let e = omml::render_inline(latex).unwrap_or_else(|e| panic!("{latex}: {e}"));
        let back = omml_reader::omath_to_latex(&e).unwrap();
        assert_eq!(back, expect, "round trip of {latex}");
    }
}
