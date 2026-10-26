//! Static maps and free helper functions of `frontend/parser.py`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

use tex2word_ir::pystr;
use tex2word_ir::re::{self, Flags};
use tex2word_ir::{Block, Image, Inline, LineBreak, Paragraph, TableRow};
use tex2word_latexwalker::{Node, NodeKind};

macro_rules! lazy_re {
    ($name:ident, $pat:expr) => {
        pub(crate) static $name: std::sync::LazyLock<tex2word_ir::re::Regex> =
            std::sync::LazyLock::new(|| tex2word_ir::re::Regex::new($pat));
    };
    ($name:ident, $pat:expr, $flags:expr) => {
        pub(crate) static $name: std::sync::LazyLock<tex2word_ir::re::Regex> =
            std::sync::LazyLock::new(|| tex2word_ir::re::Regex::with_flags($pat, $flags));
    };
}
pub(crate) use lazy_re;

// --------------------------------------------------------------------------- //
// Static maps
// --------------------------------------------------------------------------- //

pub(crate) fn section_level(name: &str, book: bool) -> Option<i64> {
    Some(match (name, book) {
        ("part", _) | ("chapter", _) => 1,
        ("section", false) => 1,
        ("subsection", false) => 2,
        ("subsubsection", false) => 3,
        ("paragraph", false) | ("subparagraph", false) => 4,
        // book/report classes: \chapter is the top numbered level
        ("section", true) => 2,
        ("subsection", true) => 3,
        ("subsubsection", true) => 4,
        ("paragraph", true) | ("subparagraph", true) => 5,
        _ => return None,
    })
}

/// the sectioning commands LaTeX auto-numbers (\part is shown unnumbered here)
pub(crate) fn is_numbered_section(name: &str) -> bool {
    matches!(name, "chapter" | "section" | "subsection" | "subsubsection")
}

/// spacing/box macros that print nothing (their argument is consumed)
pub(crate) fn is_drop_macro(name: &str) -> bool {
    matches!(name, "phantom" | "hphantom" | "vphantom" | "rule")
}

/// box wrappers that are visually transparent: emit the (last) content group
pub(crate) fn is_transparent_box(name: &str) -> bool {
    matches!(name, "mbox" | "fbox" | "framebox" | "makebox" | "raisebox")
}

/// IEEEtran author-block wrappers: render their content.
pub(crate) fn is_author_block(name: &str) -> bool {
    matches!(name, "IEEEauthorblockN" | "IEEEauthorblockA")
}

pub(crate) fn emphasis_kind(name: &str) -> Option<&'static str> {
    Some(match name {
        "textbf" => "bold",
        "textit" | "emph" | "textsl" => "italic",
        "underline" | "uline" | "uuline" => "underline",
        "texttt" => "typewriter",
        "textsc" => "smallcaps",
        "textsuperscript" => "superscript",
        "textsubscript" => "subscript",
        // ulem strike/underline, soul highlight
        "sout" | "st" | "xout" => "strike",
        "hl" => "highlight",
        _ => return None,
    })
}

/// Declaration-form font switches (`{\bfseries ...}` / `{\bf ...}`).
pub(crate) fn emphasis_decl(name: &str) -> Option<&'static str> {
    Some(match name {
        "bfseries" | "bf" => "bold",
        "itshape" | "it" | "em" | "slshape" | "sl" => "italic",
        "ttfamily" | "tt" => "typewriter",
        "scshape" | "sc" => "smallcaps",
        _ => return None,
    })
}

/// Upright/roman/sans/medium font resets: rendered with no added emphasis.
pub(crate) fn is_font_reset(name: &str) -> bool {
    matches!(
        name,
        "textnormal" | "textrm" | "textsf" | "textmd" | "textup" | "text"
    )
}

/// Font-size declarations (10pt base) -> `w:sz` half-points.
pub(crate) fn font_size_hp(name: &str) -> Option<i64> {
    Some(match name {
        "tiny" => 10,
        "scriptsize" => 14,
        "footnotesize" => 16,
        "small" => 18,
        "normalsize" => 20,
        "large" => 24,
        "Large" => 29,
        "LARGE" => 34,
        "huge" => 41,
        "Huge" => 50,
        _ => return None,
    })
}

pub(crate) fn is_math_env(base: &str) -> bool {
    matches!(
        base,
        "equation"
            | "align"
            | "gather"
            | "multline"
            | "eqnarray"
            | "displaymath"
            | "math"
            | "alignat"
            | "flalign"
            | "aligned"
            | "gathered"
            | "split"
            | "multlined"
    )
}

/// Of the math environments, these never carry equation numbers.
pub(crate) fn is_unnumbered_math_env(base: &str) -> bool {
    matches!(
        base,
        "displaymath" | "math" | "aligned" | "gathered" | "split" | "multlined"
    )
}

/// Environments we cannot translate to Word primitives -> graphics placeholder.
pub(crate) const OPAQUE_ENVS: &[&str] = &[
    "tikzpicture",
    "pspicture",
    "pgfpicture",
    "circuitikz",
    "tikzcd",
    "tikzcd*",
    "forest",
    "pgfplots",
    "axis",
];

pub(crate) fn cite_mode(name: &str) -> Option<&'static str> {
    Some(match name {
        "cite" | "citep" | "Citep" | "parencite" | "citealp" | "smartcite" | "autocite"
        | "Autocite" | "Parencite" | "Cite" => "paren",
        "footcite" => "foot",
        "citet" | "Citet" | "textcite" | "citealt" | "Textcite" => "text",
        "citeauthor" | "Citeauthor" => "author",
        "citeyear" | "citeyearpar" => "year",
        "citenum" | "citenumber" => "num",
        _ => return None,
    })
}

pub(crate) fn ref_kind(name: &str) -> Option<&'static str> {
    Some(match name {
        "ref" | "autoref" | "cref" | "Cref" | "vref" | "crefrange" | "Crefrange" | "labelcref" => {
            "generic"
        }
        "eqref" => "equation",
        "pageref" => "page",
        "nameref" | "Nameref" => "name",
        _ => return None,
    })
}

/// cleveref-style commands carry a type prefix; \ref/\eqref/\pageref stay bare.
pub(crate) fn ref_style(name: &str) -> &'static str {
    match name {
        "autoref" | "Cref" | "vref" | "Crefrange" => "full",
        "cref" | "crefrange" => "abbrev",
        _ => "plain",
    }
}

pub(crate) static TEXT_SYMBOLS: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| {
        [
            ("LaTeX", "LaTeX"),
            ("TeX", "TeX"),
            ("ldots", "…"),
            ("dots", "…"),
            ("textellipsis", "…"),
            ("textemdash", "—"),
            ("textendash", "–"),
            ("textasciitilde", "~"),
            ("textbackslash", "\\"),
            ("textasciicircum", "^"),
            ("textbar", "|"),
            ("textless", "<"),
            ("textgreater", ">"),
            ("S", "§"),
            ("P", "¶"),
            ("copyright", "©"),
            ("textregistered", "®"),
            ("texttrademark", "™"),
            ("dag", "†"),
            ("ddag", "‡"),
            ("pounds", "£"),
            ("euro", "€"),
            ("%", "%"),
            ("textdegree", "°"),
            ("textbullet", "•"),
            ("textmu", "µ"),
            ("textperthousand", "‰"),
            ("textquotedblleft", "“"),
            ("textquotedblright", "”"),
            ("textquoteleft", "‘"),
            ("textquoteright", "’"),
            ("guillemotleft", "«"),
            ("guillemotright", "»"),
            ("textsection", "§"),
            ("textparagraph", "¶"),
            // vulgar fractions, currencies, and assorted text symbols
            ("textonehalf", "½"),
            ("textonequarter", "¼"),
            ("textthreequarters", "¾"),
            ("textonesuperior", "¹"),
            ("texttwosuperior", "²"),
            ("textthreesuperior", "³"),
            ("texteuro", "€"),
            ("textcent", "¢"),
            ("textsterling", "£"),
            ("textyen", "¥"),
            ("textdollar", "$"),
            ("textnumero", "№"),
            ("textcelsius", "℃"),
            ("textohm", "Ω"),
            ("textmho", "℧"),
            ("textdiv", "÷"),
            ("texttimes", "×"),
            ("textpm", "±"),
            ("textminus", "−"),
            ("textperiodcentered", "·"),
            ("textdaggerdbl", "‡"),
            ("textdagger", "†"),
            ("checkmark", "✓"),
            ("slash", "/"),
            ("nobreakspace", " "),
            // bbding dingbats (\greencheck-style macros expand to these)
            ("Checkmark", "✓"),
            ("CheckmarkBold", "✔"),
            ("XSolid", "✗"),
            ("XSolidBold", "✘"),
            ("XSolidBrush", "✘"),
            ("Cross", "✗"),
            ("CrossBold", "✘"),
            ("HandRight", "☞"),
            ("HandLeft", "☜"),
            ("PencilRight", "✎"),
            ("Envelope", "✉"),
            ("Phone", "☎"),
            // gensymb package text/math symbols
            ("degree", "°"),
            ("celsius", "℃"),
            ("ohm", "Ω"),
            ("micro", "µ"),
            ("perthousand", "‰"),
            ("&", "&"),
            ("_", "_"),
            ("#", "#"),
            ("$", "$"),
            ("{", "{"),
            ("}", "}"),
            (" ", " "),
            (",", "\u{202f}"),
            (";", " "),
            (":", " "),
            ("!", ""),
            ("@", ""),
            // \quad = 1em, \qquad = 2em: em spaces
            ("quad", "\u{2003}"),
            ("qquad", "\u{2003}\u{2003}"),
            ("hfill", " "),
            ("newline", "\n"),
            // \xspace re-inserts the space pylatexenc gobbles after a control word
            ("xspace", " "),
            // common math-ish operators that also appear in text / pseudocode
            ("gets", "←"),
            ("to", "→"),
            ("leftarrow", "←"),
            ("rightarrow", "→"),
            ("Rightarrow", "⇒"),
            ("Leftarrow", "⇐"),
            ("leftrightarrow", "↔"),
            ("land", "∧"),
            ("lor", "∨"),
            ("lnot", "¬"),
            ("neg", "¬"),
            ("neq", "≠"),
            ("ne", "≠"),
            ("leq", "≤"),
            ("le", "≤"),
            ("geq", "≥"),
            ("ge", "≥"),
            ("times", "×"),
            ("cdot", "·"),
            ("infty", "∞"),
            ("star", "★"),
            ("ast", "∗"),
            ("approx", "≈"),
            ("equiv", "≡"),
            ("in", "∈"),
            ("forall", "∀"),
            ("exists", "∃"),
            ("emptyset", "∅"),
        ]
        .into_iter()
        .collect()
    });

/// Map a pifont `\ding{N}` code to a Unicode dingbat (`•` if unknown).
pub(crate) fn ding_char(code: &str) -> String {
    let Some(n) = pystr::parse_int(code) else {
        return "•".into();
    };
    let c = match n {
        51 => '✓',
        52 => '✔',
        53 => '✗',
        54 => '✘',
        55 => '✗',
        56 => '✘',
        172..=181 => char::from_u32(0x2780 + (n - 172) as u32).unwrap(),
        182..=191 => char::from_u32(0x2776 + (n - 182) as u32).unwrap(),
        192..=201 => char::from_u32(0x278A + (n - 192) as u32).unwrap(),
        _ => '•',
    };
    c.to_string()
}

/// Structural wrappers with no Word-visible formatting -> content passes through.
pub(crate) fn is_transparent_env(base: &str) -> bool {
    matches!(
        base,
        "subequations"
            | "samepage"
            | "sloppypar"
            | "spacing"
            | "singlespace"
            | "doublespace"
            | "onehalfspace"
            | "noindent"
            | "raggedright"
            | "raggedleft"
            | "small"
            | "footnotesize"
            | "large"
            | "Large"
            | "flushleftright"
    )
}

/// theorem-like environment name -> display name (proof is handled specially).
pub(crate) const THEOREM_ENVS: &[(&str, &str)] = &[
    ("theorem", "Theorem"),
    ("thm", "Theorem"),
    ("lemma", "Lemma"),
    ("corollary", "Corollary"),
    ("cor", "Corollary"),
    ("proposition", "Proposition"),
    ("prop", "Proposition"),
    ("definition", "Definition"),
    ("defn", "Definition"),
    ("remark", "Remark"),
    ("example", "Example"),
    ("conjecture", "Conjecture"),
    ("claim", "Claim"),
    ("fact", "Fact"),
    ("observation", "Observation"),
    ("notation", "Notation"),
    ("assumption", "Assumption"),
    ("axiom", "Axiom"),
    ("exercise", "Exercise"),
    ("problem", "Problem"),
    ("question", "Question"),
];

pub(crate) fn toc_kind(name: &str) -> Option<&'static str> {
    Some(match name {
        "tableofcontents" => "contents",
        "listoffigures" => "figures",
        "listoftables" => "tables",
        _ => return None,
    })
}

pub(crate) fn is_ignore_macro(name: &str) -> bool {
    matches!(
        name,
        "maketitle"
            | "frontmatter"
            | "mainmatter"
            | "backmatter"
            | "newpage"
            | "clearpage"
            | "pagebreak"
            | "noindent"
            | "centering"
            | "small"
            | "large"
            | "Large"
            | "LARGE"
            | "normalsize"
            | "footnotesize"
            | "scriptsize"
            | "tiny"
            | "huge"
            | "Huge"
            | "vspace"
            | "hspace"
            | "vfill"
            | "bigskip"
            | "medskip"
            | "smallskip"
            | "indent"
            | "par"
            | "protect"
            | "displaystyle"
            | "unskip"
            | "ignorespaces"
            | "leavevmode"
            | "/"
            | "sloppy"
            | "fussy"
            | "raggedright"
            | "raggedleft"
            | "flushbottom"
            | "samepage"
            | "frenchspacing"
            | "boldmath"
            | "unboldmath"
            | "selectfont"
            | "rmfamily"
            | "sffamily"
            | "normalfont"
            | "floatbarrier"
            | "rm"
            | "sf"
            | "md"
            | "up"
            | "mdseries"
            | "upshape"
            | "FloatBarrier"
            | "height"
            | "width"
            | "depth"
            | "totalheight"
            | "fontfamily"
            | "fontsize"
            | "nohyphens"
            | "documentclass"
            | "usepackage"
            | "RequirePackage"
            | "pagestyle"
            | "thispagestyle"
            | "setlength"
            | "setcounter"
            | "hypersetup"
            | "graphicspath"
            | "definecolor"
            | "pagenumbering"
            | "renewcommand"
            | "newcommand"
            | "providecommand"
            | "setitemize"
            | "setenumerate"
            | "hyphenation"
            | "settopmatter"
            | "begingroup"
            | "endgroup"
            | "bgroup"
            | "egroup"
            | "AddToShipoutPicture"
            | "ClearShipoutPicture"
            | "newcounter"
            | "addtocounter"
            | "refstepcounter"
            | "stepcounter"
            | "authornote"
            | "authornotemark"
            | "balance"
            | "acmnote"
            | "institution"
            | "department"
            | "city"
            | "state"
            | "country"
            | "postcode"
            | "streetaddress"
            | "position"
    )
}

/// math symbols that commonly serve as affiliation / footnote markers.
fn marker_symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "dagger" | "dag" => "†",
        "ddagger" | "ddag" => "‡",
        "ast" => "∗",
        "star" => "⋆",
        "S" => "§",
        "P" => "¶",
        "natural" => "♮",
        "sharp" => "♯",
        "flat" => "♭",
        "prime" => "′",
        "bullet" => "•",
        "diamond" => "⋄",
        "triangle" => "△",
        "clubsuit" => "♣",
        "spadesuit" => "♠",
        "heartsuit" => "♡",
        "diamondsuit" => "♢",
        "sim" => "∼",
        "#" => "#",
        "&" => "&",
        "%" => "%",
        "," | ";" | "!" => "",
        _ => return None,
    })
}

/// font wrappers whose argument is just text (`^{\mathrm{th}}` -> "th").
fn is_marker_wrapper(name: &str) -> bool {
    matches!(
        name,
        "mathrm"
            | "text"
            | "textrm"
            | "mathit"
            | "mathbf"
            | "textbf"
            | "mathsf"
            | "textit"
            | "textup"
            | "mathnormal"
    )
}

fn is_marker_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "*,.;:+-()'/=|!? ".contains(c)
}

fn char_at(s: &str, i: usize) -> Option<char> {
    s.get(i..).and_then(|t| t.chars().next())
}

lazy_re!(MARKER_CS_RE, r"\\([A-Za-z]+|.)");

/// Plain text for a script body made only of marker characters/symbols, or
/// `None` if it holds real math structure.
pub(crate) fn marker_text(body: &str) -> Option<String> {
    let mut out = String::new();
    let n = body.len();
    let mut i = 0;
    while i < n {
        let c = char_at(body, i).unwrap();
        if c == '\\' {
            let m = MARKER_CS_RE.match_start(&body[i..]);
            let name = m.as_ref().map_or("", |m| m.group_str(1));
            i += m.as_ref().map_or(1, |m| m.end());
            if let Some(sym) = marker_symbol(name) {
                out.push_str(sym);
            } else if is_marker_wrapper(name) {
                while body.as_bytes().get(i) == Some(&b' ') {
                    i += 1;
                }
                if body.as_bytes().get(i) == Some(&b'{') {
                    let (inner, end) = braced_group(body, i);
                    out.push_str(&marker_text(inner)?);
                    i = end + 1;
                }
            } else {
                return None;
            }
        } else if c == '{' || c == '}' {
            i += 1;
        } else if is_marker_char(c) {
            out.push(if c == '-' { '−' } else { c });
            i += 1;
        } else {
            return None;
        }
    }
    let text = pystr::strip(&out);
    (!text.is_empty()).then(|| text.to_string())
}

lazy_re!(SCRIPT_TOKEN_RE, r"\\[A-Za-z]+|\\.|.");

/// `^{*1}` -> ("superscript", "*1"); `_{a}` -> ("subscript", "a"); `^\circ`
/// -> ("plain", "°"). `None` unless the math is *only* a base-less script
/// whose content is marker text.
pub(crate) fn script_marker(latex: &str) -> Option<(&'static str, String)> {
    let s = pystr::strip(latex);
    let first = s.chars().next()?;
    if pystr::char_len(s) < 2 || !(first == '^' || first == '_') {
        return None;
    }
    let kind = if first == '^' {
        "superscript"
    } else {
        "subscript"
    };
    let rest = pystr::lstrip(&s[1..]);
    let (body, tail) = if rest.starts_with('{') {
        let (body, end) = braced_group(rest, 0);
        (body, rest.get(end + 1..).unwrap_or(""))
    } else {
        let m = SCRIPT_TOKEN_RE.match_start(rest)?;
        (m.as_str(), &rest[m.end()..])
    };
    if !pystr::strip(tail).is_empty() {
        // more after the script (^a_b, ^1 x): leave it as math
        return None;
    }
    if kind == "superscript" && matches!(pystr::strip(body), "\\circ" | "{\\circ}") {
        return Some(("plain", "°".into())); // $^\circ$ is a degree sign
    }
    marker_text(body).map(|t| (kind, t))
}

/// horizontal glue that can sit between side-by-side minipages.
fn is_layout_glue_macro(name: &str) -> bool {
    matches!(
        name,
        "hfill"
            | "hspace"
            | "hskip"
            | "quad"
            | "qquad"
            | "enspace"
            | "thinspace"
            | "centering"
            | "noindent"
            | "hss"
            | "hfil"
            | ","
            | " "
            | "~"
    )
}

pub(crate) fn is_layout_glue(node: &Node) -> bool {
    match &node.kind {
        NodeKind::Chars { chars } => pystr::strip(chars).is_empty(),
        NodeKind::Comment { .. } => true,
        NodeKind::Macro { macroname, .. } => is_layout_glue_macro(macroname),
        _ => false,
    }
}

pub(crate) fn is_minipage(node: &Node) -> bool {
    node.environmentname() == Some("minipage")
}

/// The verbatim LaTeX source of a node list (for \ensuremath bodies).
pub(crate) fn latex_of(nodes: &[Node]) -> String {
    let joined: String = nodes.iter().map(Node::latex_verbatim).collect();
    pystr::strip(&joined).to_string()
}

// --------------------------------------------------------------------------- //
// Node helpers
// --------------------------------------------------------------------------- //

/// The argument list of a macro/environment (empty when it has none).
pub(crate) fn argnlist(node: &Node) -> &[Option<Node>] {
    node.nodeargd().map_or(&[], |a| a.argnlist.as_slice())
}

pub(crate) fn group_parts(node: &Node) -> Option<(&[Node], &(String, String))> {
    match &node.kind {
        NodeKind::Group {
            nodelist,
            delimiters,
        } => Some((nodelist, delimiters)),
        _ => None,
    }
}

/// The node-lists of a macro's `{}` mandatory groups (skips `[]` options).
pub(crate) fn brace_groups(node: &Node) -> Vec<&[Node]> {
    argnlist(node)
        .iter()
        .flatten()
        .filter_map(group_parts)
        .filter(|(_, d)| d.0.is_empty() || first_delim_char(d) == Some('{'))
        .map(|(n, _)| n)
        .collect()
}

/// `delimiters[0]` (Python compares the opening delimiter string).
fn first_delim_char(d: &(String, String)) -> Option<char> {
    let mut it = d.0.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// The nodelist of a macro's first `[]` optional argument, or `None`.
pub(crate) fn optional_group(node: &Node) -> Option<&[Node]> {
    argnlist(node)
        .iter()
        .flatten()
        .filter_map(group_parts)
        .find(|(_, d)| first_delim_char(d) == Some('['))
        .map(|(n, _)| n)
}

/// The `[...]` optional-argument groups, as `(nodelist, delimiters)`.
pub(crate) fn is_bracket_group(node: &Node) -> bool {
    group_parts(node).is_some_and(|(_, d)| first_delim_char(d) == Some('['))
}

/// `a` / `a and b` / `a, b and c` (siunitx list separators).
pub(crate) fn join_and(items: &[String]) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        n => format!("{} and {}", items[..n - 1].join(", "), items[n - 1]),
    }
}

/// Split a node list at `\and` macros (one `\author` -> several authors).
pub(crate) fn split_on_and(nodes: &[Node]) -> Vec<Vec<Node>> {
    let mut segments: Vec<Vec<Node>> = vec![Vec::new()];
    for n in nodes {
        if n.macroname() == Some("and") {
            segments.push(Vec::new());
        } else {
            segments.last_mut().unwrap().push(n.clone());
        }
    }
    segments.retain(|s| !s.is_empty());
    segments
}

/// The nodelist of a macro's mandatory group argument (`which == -1` picks
/// the last group; otherwise the n-th group). With no group argument, the
/// first non-`None` argument (e.g. `\ref e`).
pub(crate) fn group_nodes(node: &Node) -> Cow<'_, [Node]> {
    group_nodes_at(node, -1)
}

pub(crate) fn group_nodes_at(node: &Node, which: i64) -> Cow<'_, [Node]> {
    let args = argnlist(node);
    if args.is_empty() {
        return Cow::Borrowed(&[]);
    }
    let groups: Vec<&[Node]> = args
        .iter()
        .flatten()
        .filter_map(group_parts)
        .map(|(n, _)| n)
        .collect();
    if groups.is_empty() {
        // a single non-group argument (e.g. \ref e)
        return match args.iter().flatten().next() {
            Some(a) => Cow::Owned(vec![a.clone()]),
            None => Cow::Borrowed(&[]),
        };
    }
    if which == -1 {
        return Cow::Borrowed(groups[groups.len() - 1]);
    }
    match groups.get(which as usize) {
        Some(g) => Cow::Borrowed(*g),
        None => Cow::Borrowed(&[]),
    }
}

/// Inner LaTeX of an environment, without the `\begin`/`\end` wrappers.
pub(crate) fn verbatim_inner(env: &Node) -> String {
    // verbatim/lstlisting bodies live in nodeargd.verbatim_text
    if let Some(verb) = env.nodeargd().and_then(|a| a.verbatim_text.as_deref()) {
        return pystr::strip_chars(verb, "\n").to_string();
    }
    let list = env.nodelist().unwrap_or(&[]);
    let (Some(first), Some(last)) = (list.first(), list.last()) else {
        return String::new();
    };
    env.source()[first.pos..last.pos + last.len].to_string()
}

fn chars_of_into(nodes: &[Node], out: &mut String) {
    for n in nodes {
        match &n.kind {
            NodeKind::Chars { chars } => out.push_str(chars),
            NodeKind::Group { nodelist, .. } => {
                let mut inner = String::new();
                chars_of_into(nodelist, &mut inner);
                out.push_str(pystr::strip(&inner));
            }
            _ => {}
        }
    }
}

/// The plain characters of a node list (chars + nested groups), stripped.
pub(crate) fn chars_of(nodes: &[Node]) -> String {
    let mut out = String::new();
    chars_of_into(nodes, &mut out);
    pystr::strip(&out).to_string()
}

/// True if a figure source holds a drawing environment (TikZ & co.).
pub(crate) fn has_opaque_env(source: &str) -> bool {
    OPAQUE_ENVS
        .iter()
        .any(|env| source.contains(&format!("\\begin{{{env}}}")))
}

/// A paragraph with no visible content (only whitespace / line breaks).
pub(crate) fn is_blank_paragraph(block: &Block) -> bool {
    let Block::Paragraph(p) = block else {
        return false;
    };
    p.inlines.iter().all(|node| match node {
        Inline::LineBreak(_) => true,
        Inline::Text(t) => pystr::strip(&t.value).is_empty(),
        _ => false,
    })
}

/// True if `nodes` (one level, plus groups) hold `\captionof{kind}`.
pub(crate) fn has_captionof(nodes: &[Node], kind: &str) -> bool {
    for n in nodes {
        if n.macroname() == Some("captionof") {
            let groups: Vec<&[Node]> = argnlist(n)
                .iter()
                .flatten()
                .filter_map(group_parts)
                .filter(|(_, d)| first_delim_char(d) == Some('{'))
                .map(|(n, _)| n)
                .collect();
            if !groups.is_empty() && pystr::strip(&chars_of(groups[0])) == kind {
                return true;
            }
        } else if let Some((list, _)) = group_parts(n) {
            if has_captionof(list, kind) {
                return true;
            }
        }
    }
    false
}

/// Flatten a single-column nested `tabular` (the line-stacking idiom common
/// inside cells) into one paragraph with line breaks -- only when every row
/// is a single cell of plain paragraphs.
pub(crate) fn flatten_stacked_tables(blocks: Vec<Block>) -> Vec<Block> {
    blocks
        .into_iter()
        .map(|b| match b {
            Block::Table(t)
                if !t.rows.is_empty()
                    && t.rows.iter().all(|r| r.cells.len() == 1)
                    && t.rows.iter().all(|r| {
                        r.cells[0]
                            .blocks
                            .iter()
                            .all(|cb| matches!(cb, Block::Paragraph(_)))
                    }) =>
            {
                let mut inlines = Vec::new();
                for (i, row) in t.rows.into_iter().enumerate() {
                    if i > 0 {
                        inlines.push(Inline::LineBreak(LineBreak {}));
                    }
                    for cb in row.cells.into_iter().next().unwrap().blocks {
                        if let Block::Paragraph(p) = cb {
                            inlines.extend(p.inlines);
                        }
                    }
                }
                Block::Paragraph(Paragraph {
                    inlines,
                    align: None,
                })
            }
            other => other,
        })
        .collect()
}

/// True if a macro was invoked in its starred form (e.g. `\section*`).
pub(crate) fn has_star(node: &Node) -> bool {
    matches!(argnlist(node).first(), Some(Some(first)) if first.chars() == Some("*"))
}

pub(crate) fn int_or_default(value: &str, default: i64) -> i64 {
    pystr::parse_int(value).unwrap_or(default)
}

pub(crate) fn inlines_to_text_with(
    inlines: &[Inline],
    resolve: &dyn Fn(&Inline) -> Option<Vec<Inline>>,
) -> String {
    let mut out: Vec<String> = Vec::new();
    for node in inlines {
        match node {
            Inline::Text(t) => out.push(t.value.clone()),
            Inline::Emphasis(e) => out.push(inlines_to_text_with(&e.inlines, resolve)),
            Inline::Link(l) => out.push(inlines_to_text_with(&l.inlines, resolve)),
            Inline::Footnote(f) => {
                // a \footnotemark slot reads as the content filled in so far
                let filled = f.inlines.first().and_then(resolve);
                match filled {
                    Some(content) => out.push(inlines_to_text_with(&content, resolve)),
                    None => out.push(inlines_to_text_with(&f.inlines, resolve)),
                }
            }
            Inline::Endnote(e) => out.push(inlines_to_text_with(&e.inlines, resolve)),
            Inline::Math(m) => out.push(m.latex.clone()),
            _ => {}
        }
    }
    pystr::collapse_ws(&out.join(" "))
}

/// Macro nodes in a list, descending into groups and environments.
pub(crate) fn walk_macros(nodes: &[Node]) -> Vec<&Node> {
    let mut out = Vec::new();
    fn go<'a>(nodes: &'a [Node], out: &mut Vec<&'a Node>) {
        for n in nodes {
            match &n.kind {
                NodeKind::Macro { .. } => out.push(n),
                NodeKind::Group { nodelist, .. } | NodeKind::Environment { nodelist, .. } => {
                    go(nodelist, out)
                }
                _ => {}
            }
        }
    }
    go(nodes, &mut out);
    out
}

// --------------------------------------------------------------------------- //
// Free helpers
// --------------------------------------------------------------------------- //

lazy_re!(WS_RUN_RE, r"\s+");

pub(crate) fn normalize_ws(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    WS_RUN_RE.sub(" ", text)
}

// Punctuation that should not be preceded by a space.
lazy_re!(SPACE_BEFORE_PUNCT, r" +([,.;:!?)\]}])");
lazy_re!(MULTI_SPACE_RE, r" {2,}");

/// Tidy a freshly built inline list: merge adjacent Text runs, squeeze
/// repeated spaces, drop a space before closing punctuation; with `trim`,
/// strip the run's leading/trailing whitespace.
pub(crate) fn clean_inlines(nodes: Vec<Inline>, trim: bool) -> Vec<Inline> {
    let mut merged: Vec<Inline> = Vec::with_capacity(nodes.len());
    for node in nodes {
        if let (Inline::Text(t), Some(Inline::Text(last))) = (&node, merged.last_mut()) {
            last.value.push_str(&t.value);
            continue;
        }
        merged.push(node);
    }
    for node in merged.iter_mut() {
        if let Inline::Text(t) = node {
            let value = MULTI_SPACE_RE.sub(" ", &t.value);
            t.value = SPACE_BEFORE_PUNCT.sub(r"\1", &value);
        }
    }
    if trim {
        if let Some(Inline::Text(t)) = merged.first_mut() {
            t.value = pystr::lstrip(&t.value).to_string();
        }
        if let Some(Inline::Text(t)) = merged.last_mut() {
            t.value = pystr::rstrip(&t.value).to_string();
        }
        merged.retain(|n| !matches!(n, Inline::Text(t) if t.value.is_empty()));
    }
    merged
}

pub(crate) fn is_display(node: &Node) -> bool {
    match &node.kind {
        NodeKind::Math { delimiters, .. } => delimiters.0 == "\\[" || delimiters.0 == "$$",
        _ => false,
    }
}

pub(crate) fn strip_math_delims(verbatim: &str, _env: &str) -> String {
    let v = pystr::strip(verbatim);
    for (op, cl) in [("\\[", "\\]"), ("$$", "$$"), ("\\(", "\\)"), ("$", "$")] {
        if v.starts_with(op) && v.ends_with(cl) {
            // v[len(op) : len(v) - len(cl)] (empty when they overlap)
            let inner = v
                .get(op.len()..v.len().saturating_sub(cl.len()))
                .unwrap_or("");
            return pystr::strip(inner).to_string();
        }
    }
    if v.starts_with("\\begin") {
        // \begin{env} ... \end{env}
        if let (Some(start), Some(end)) = (v.find('}'), v.rfind("\\end")) {
            return pystr::strip(v.get(start + 1..end).unwrap_or("")).to_string();
        }
    }
    v.to_string()
}

lazy_re!(
    LABEL_TAG_RE,
    r"\\(?:label|tag|nonumber|notag)\b\s*(?:\{[^{}]*\})?"
);
lazy_re!(EXTRACT_LABEL_RE, r"\\label\{([^}]*)\}");

pub(crate) fn extract_label(latex: &str) -> Option<String> {
    EXTRACT_LABEL_RE
        .search(latex)
        .map(|m| m.group_str(1).to_string())
}

pub(crate) fn env_colspec(node: &Node) -> String {
    // The column spec is the *last* mandatory group: tabularx/tabulary carry
    // a leading {width} we must skip.
    let args = argnlist(node);
    if args.is_empty() {
        return String::new();
    }
    let groups: Vec<&Node> = args.iter().flatten().filter(|a| a.is_group()).collect();
    if let Some(last) = groups.last() {
        let verb = pystr::strip(last.latex_verbatim());
        if verb.starts_with('{') && verb.ends_with('}') {
            return verb
                .get(1..verb.len().saturating_sub(1))
                .unwrap_or("")
                .to_string(); // keep p{3cm} braces
        }
        return chars_of(last.nodelist().unwrap_or(&[]));
    }
    for a in args.iter().flatten() {
        if let Some(c) = a.chars() {
            return c.to_string();
        }
    }
    String::new()
}

lazy_re!(CMIDRULE_RE, r"(\d+)\s*-\s*(\d+)");

/// The 1-based `{a-b}` column range of a `\cmidrule`/`\cline`.
pub(crate) fn cmidrule_range(node: &Node) -> Option<(i64, i64)> {
    let chars = chars_of(&group_nodes(node));
    let text = if chars.is_empty() {
        node.latex_verbatim().to_string()
    } else {
        chars
    };
    let m = CMIDRULE_RE.search(&text)?;
    let a = pystr::parse_int(m.group_str(1))?;
    let b = pystr::parse_int(m.group_str(2))?;
    Some(if a <= b { (a, b) } else { (b, a) })
}

/// Set `border_bottom` on the cells of `row` overlapping 1-based `rng`.
pub(crate) fn apply_partial_rule(row: &mut TableRow, rng: (i64, i64)) {
    let (a, b) = rng;
    let mut col = 1;
    for cell in row.cells.iter_mut() {
        let cell_end = col + cell.colspan - 1;
        if col <= b && cell_end >= a {
            cell.border_bottom = true;
        }
        col = cell_end + 1;
    }
}

/// Read a `{...}` group beginning at `spec[start] == '{'`; return the body
/// and the index of the closing brace (or `len` when unbalanced).
pub(crate) fn braced_group(spec: &str, start: usize) -> (&str, usize) {
    let b = spec.as_bytes();
    let mut depth = 0i64;
    let mut i = start;
    while i < b.len() {
        if b[i] == b'{' {
            depth += 1;
        } else if b[i] == b'}' {
            depth -= 1;
            if depth == 0 {
                return (&spec[start + 1..i], i);
            }
        }
        i += 1;
    }
    (&spec[(start + 1).min(spec.len())..], spec.len())
}

/// Parse a column spec into (alignments, per-column EMU widths or `None`).
pub(crate) fn parse_colspec(spec: &str) -> (Vec<String>, Vec<Option<f64>>) {
    let mut aligns: Vec<String> = Vec::new();
    let mut widths: Vec<Option<f64>> = Vec::new();
    let mut pending_align: Option<&'static str> = None; // set by >{...}
    let b = spec.as_bytes();
    let mut i = 0;
    while i < spec.len() {
        let c = char_at(spec, i).unwrap();
        let next_brace = b.get(i + c.len_utf8()) == Some(&b'{');
        if c == '>' && next_brace {
            let (body, j) = braced_group(spec, i + 1);
            for (key, al) in [
                ("centering", "center"),
                ("raggedright", "left"),
                ("raggedleft", "right"),
            ] {
                if body.contains(&format!("\\{key}")) {
                    pending_align = Some(al);
                }
            }
            i = j + 1;
            continue;
        }
        match c {
            'l' | 'c' | 'r' => {
                let al = match c {
                    'l' => "left",
                    'c' => "center",
                    _ => "right",
                };
                aligns.push(pending_align.unwrap_or(al).into());
                widths.push(None);
                pending_align = None;
            }
            // tabularx flexible column
            'X' => {
                aligns.push(pending_align.unwrap_or("left").into());
                widths.push(None);
                pending_align = None;
            }
            // tabulary auto-width columns
            'L' | 'C' | 'R' | 'J' => {
                let al = match c {
                    'C' => "center",
                    'R' => "right",
                    _ => "left",
                };
                aligns.push(pending_align.unwrap_or(al).into());
                widths.push(None);
                pending_align = None;
            }
            'p' | 'm' | 'b' if next_brace => {
                aligns.push(pending_align.unwrap_or("left").into());
                pending_align = None;
                let (body, j) = braced_group(spec, i + 1);
                widths.push(length_to_emu(body));
                i = j;
            }
            // ignore |, @{...}, etc.
            _ => {}
        }
        i += if i < spec.len() {
            char_at(spec, i).map_or(1, char::len_utf8)
        } else {
            1
        };
    }
    (aligns, widths)
}

/// LaTeX length units -> EMU (914400 per inch).
const EMU_PER_INCH: f64 = 914400.0;

fn unit_emu(unit: &str) -> Option<f64> {
    Some(match unit {
        "in" => EMU_PER_INCH,
        "pt" => EMU_PER_INCH / 72.27,
        "bp" => EMU_PER_INCH / 72.0,
        "cm" => EMU_PER_INCH / 2.54,
        "mm" => EMU_PER_INCH / 25.4,
        "px" => EMU_PER_INCH / 96.0,
        "em" => EMU_PER_INCH / 72.27 * 10.0,
        "ex" => EMU_PER_INCH / 72.27 * 4.3,
        _ => return None,
    })
}

lazy_re!(LEN_RE, r"^\s*([-\d.]+)\s*([a-zA-Z]*)");

/// Convert a LaTeX length (`2.5cm`, `300pt`) to EMU; relative widths skip.
pub(crate) fn length_to_emu(value: &str) -> Option<f64> {
    let value = pystr::strip(value);
    if value.contains('\\') {
        return None; // relative to \linewidth & co -- the backend fits it
    }
    let m = LEN_RE.match_start(value)?;
    let num = pystr::parse_float(m.group_str(1))?;
    let unit_raw = m.group_str(2);
    let unit = pystr::lower(if unit_raw.is_empty() { "pt" } else { unit_raw });
    Some(num * unit_emu(&unit)?)
}

const REL_WIDTH_REFS: &[&str] = &["\\textwidth", "\\linewidth", "\\columnwidth", "\\hsize"];
const REL_HEIGHT_REFS: &[&str] = &["\\textheight", "\\paperheight", "\\vsize"];

/// Parse an `\includegraphics` width/height value into
/// `(abs_emu, rel_width_fraction, rel_height_fraction)`.
pub(crate) fn length_spec(value: &str) -> (Option<f64>, Option<f64>, Option<f64>) {
    let value = pystr::strip(value);
    let frac = |r: &str| -> f64 {
        let replaced = value.replace(r, "");
        let num = pystr::strip(&replaced);
        if matches!(num, "" | "+" | "-") {
            return 1.0;
        }
        pystr::parse_float(num).unwrap_or(1.0)
    };
    for r in REL_WIDTH_REFS {
        if value.contains(r) {
            return (None, Some(frac(r)), None);
        }
    }
    for r in REL_HEIGHT_REFS {
        if value.contains(r) {
            return (None, None, Some(frac(r)));
        }
    }
    (length_to_emu(value), None, None)
}

/// Split an option list on commas outside braces/brackets.
fn split_top_commas(opts: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut depth: i64 = 0;
    for ch in opts.chars() {
        if ch == '{' || ch == '[' {
            depth += 1;
        } else if ch == '}' || ch == ']' {
            depth = (depth - 1).max(0);
        }
        if ch == ',' && depth == 0 {
            parts.push(std::mem::take(&mut buf));
        } else {
            buf.push(ch);
        }
    }
    parts.push(buf);
    parts
}

/// Parse the `[key=val,key=val,flag]` option list of `\includegraphics`.
pub(crate) fn parse_graphics_options(opts: &str) -> tex2word_ir::Dict<String> {
    let mut out = tex2word_ir::Dict::new();
    for part in split_top_commas(opts) {
        if let Some((key, val)) = part.split_once('=') {
            out.insert(
                pystr::lower(pystr::strip(key)),
                pystr::strip(val).to_string(),
            );
        } else if !pystr::strip(&part).is_empty() {
            out.insert(pystr::lower(pystr::strip(&part)), String::new());
        }
    }
    out
}

pub(crate) fn make_image(node: &Node) -> Image {
    let path = chars_of(&group_nodes(node));
    let ext = match path.rsplit_once('.') {
        Some((_, e)) => pystr::lower(e),
        None => String::new(),
    };
    let mut image = Image {
        path: path.clone(),
        original_format: ext,
        alt: path,
        ..Image::default()
    };
    let opts = graphics_option_text(node);
    if !opts.is_empty() {
        let parsed = parse_graphics_options(&opts);
        if let Some(w) = parsed.get("width") {
            let (abs_emu, rw, _) = length_spec(w);
            image.width = abs_emu;
            image.rel_width = rw;
        }
        if let Some(h) = parsed.get("height") {
            let (abs_emu, _, rh) = length_spec(h);
            image.height = abs_emu;
            image.rel_height = rh;
        }
        if parsed.contains_key("keepaspectratio") {
            image.keep_aspect = true;
        }
        if let Some(v) = parsed.get("scale").and_then(|s| pystr::parse_float(s)) {
            image.scale = Some(v);
        }
        if let Some(v) = parsed.get("angle").and_then(|s| pystr::parse_float(s)) {
            image.angle = Some(v);
        }
        if parsed.contains_key("clip") {
            image.clip = true;
        }
        if let Some(t) = parsed.get("trim") {
            let trims: Vec<Option<f64>> =
                pystr::split_ws(t).into_iter().map(length_to_emu).collect();
            if trims.len() == 4 && trims.iter().all(Option::is_some) {
                image.trim = Some(trims.into_iter().flatten().collect());
            }
        }
    }
    image
}

/// The raw `[...]` optional-argument text of a macro (verbatim, so
/// `width=0.5\linewidth` keeps its macro).
pub(crate) fn graphics_option_text(node: &Node) -> String {
    for a in argnlist(node).iter().flatten() {
        if let Some((list, d)) = group_parts(a) {
            if first_delim_char(d) == Some('[') {
                let verb = pystr::strip(a.latex_verbatim());
                if verb.starts_with('[') && verb.ends_with(']') {
                    return verb
                        .get(1..verb.len().saturating_sub(1))
                        .unwrap_or("")
                        .to_string();
                }
                return chars_of(list);
            }
        }
    }
    String::new()
}

/// Text-mode accents via combining characters.
pub(crate) fn accent_char(name: &str) -> Option<&'static str> {
    Some(match name {
        "'" => "\u{301}",
        "`" => "\u{300}",
        "^" => "\u{302}",
        "\"" => "\u{308}",
        "~" => "\u{303}",
        "=" => "\u{304}",
        "." => "\u{307}",
        "v" => "\u{30C}",
        "u" => "\u{306}",
        "c" => "\u{327}",
        "H" => "\u{30B}",
        "r" => "\u{30A}",
        _ => return None,
    })
}

pub(crate) fn apply_text_accent(name: &str, base: &str) -> String {
    let comb = accent_char(name).unwrap_or("");
    let mut chars = base.chars();
    match chars.next() {
        None => comb.to_string(),
        Some(first) => pystr::nfc(&format!("{first}{comb}{}", chars.as_str())),
    }
}

// --------------------------------------------------------------------------- //
// tcolorbox / mdframed definitions
// --------------------------------------------------------------------------- //

lazy_re!(
    NEWTCOLORBOX_RE,
    concat!(
        r"\\(?:new|renew|provide|Declare|New|Renew|Provide)?",
        r"(?:tcolorbox|TColorBox|tcbox)\s*(?:\[[^\]]*\])?\s*\{([^}]+)\}"
    )
);

/// Names of user `\newtcolorbox` environments (rendered as Quote blocks).
pub(crate) fn collect_tcolorbox_envs(source: &str) -> std::collections::HashSet<String> {
    NEWTCOLORBOX_RE
        .find_iter(source)
        .iter()
        .map(|m| pystr::strip(m.group_str(1)).to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// A `\newtcolorbox{name}[nargs][default]{options}` definition.
#[derive(Debug, Clone, Default)]
pub(crate) struct TcbEnv {
    pub nargs: i64,
    /// default of an optional first argument
    pub default: Option<String>,
    /// raw option text (may reference #1..#n)
    pub opts: String,
}

lazy_re!(NEWTCB_HEAD_RE, r"\\(?:re)?newtcolorbox(?![A-Za-z])");
lazy_re!(TCBSET_RE, r"\\tcbset\s*\{");

/// tcolorbox keys that shape the rendered callout; everything else is layout.
pub(crate) fn is_tcb_key(key: &str) -> bool {
    matches!(
        key,
        "colback" | "colframe" | "colbacktitle" | "coltitle" | "title" | "fonttitle" | "boxrule"
    )
}

pub(crate) fn skip_ws(s: &str, mut i: usize) -> usize {
    let b = s.as_bytes();
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

/// Read a brace-aware `[...]` group at `s[i] == '['`; (inner, index-after).
pub(crate) fn read_bracket(s: &str, i: usize) -> (&str, usize) {
    let b = s.as_bytes();
    let mut depth: i64 = 0;
    for j in i + 1..b.len() {
        match b[j] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b']' if depth == 0 => return (&s[i + 1..j], j + 1),
            _ => {}
        }
    }
    (&s[(i + 1).min(s.len())..], s.len())
}

/// Drop one level of braces wrapping a whole value (`{#1}` -> `#1`).
pub(crate) fn strip_braces(value: &str) -> String {
    let v = pystr::strip(value);
    if v.len() >= 2 && v.starts_with('{') && v.ends_with('}') {
        let (inner, end) = braced_group(v, 0);
        if end == v.len() - 1 {
            return pystr::strip(inner).to_string();
        }
    }
    v.to_string()
}

/// Split a `key=value,flag,...` option list on top-level commas, preserving
/// order and case.
pub(crate) fn kv_list(opts: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for part in split_top_commas(opts) {
        let part = pystr::strip(&part);
        if part.is_empty() {
            continue;
        }
        match part.split_once('=') {
            Some((k, v)) => out.push((
                pystr::strip(k).to_string(),
                Some(pystr::strip(v).to_string()),
            )),
            None => out.push((part.to_string(), None)),
        }
    }
    out
}

/// `\newtcolorbox` environments and `\tcbset{name/.style={...}}` styles.
pub(crate) fn collect_tcolorbox_defs(
    source: &str,
) -> (tex2word_ir::Dict<TcbEnv>, HashMap<String, String>) {
    let mut envs = tex2word_ir::Dict::new();
    let b = source.as_bytes();
    for m in NEWTCB_HEAD_RE.find_iter(source) {
        let mut i = skip_ws(source, m.end());
        if b.get(i) == Some(&b'[') {
            i = skip_ws(source, read_bracket(source, i).1);
        }
        if b.get(i) != Some(&b'{') {
            continue;
        }
        let (name, end) = braced_group(source, i);
        i = skip_ws(source, end + 1);
        let (mut nargs, mut default) = (0, None);
        if b.get(i) == Some(&b'[') {
            let (n, j) = read_bracket(source, i);
            i = skip_ws(source, j);
            nargs = pystr::parse_int(n).unwrap_or(0);
            if b.get(i) == Some(&b'[') {
                let (d, j) = read_bracket(source, i);
                default = Some(d.to_string());
                i = skip_ws(source, j);
            }
        }
        let opts = if b.get(i) == Some(&b'{') {
            braced_group(source, i).0.to_string()
        } else {
            String::new()
        };
        let name = pystr::strip(name);
        if !name.is_empty() {
            envs.insert(
                name,
                TcbEnv {
                    nargs,
                    default,
                    opts,
                },
            );
        }
    }
    let mut styles = HashMap::new();
    for m in TCBSET_RE.find_iter(source) {
        let (body, _) = braced_group(source, m.end() - 1);
        for (key, val) in kv_list(body) {
            if let (Some(base), Some(val)) = (key.strip_suffix("/.style"), val) {
                styles.insert(pystr::strip(base).to_string(), strip_braces(&val));
            }
        }
    }
    (envs, styles)
}

/// pylatexenc argument spec for a user tcolorbox env (`[` = optional).
pub(crate) fn tcb_argspec(env: &TcbEnv) -> String {
    if env.nargs <= 0 {
        return String::new();
    }
    if env.default.is_some() {
        return format!("[{}", "{".repeat((env.nargs - 1) as usize));
    }
    "{".repeat(env.nargs as usize)
}

lazy_re!(
    NEWMDENV_RE,
    concat!(
        r"\\(?:newmdenv|newmdtheoremenv|surroundwithmdframed)\s*",
        r"(?:\[([^\]]*)\])?\s*\{([^}]+)\}"
    ),
    Flags::S
);
lazy_re!(
    MDOPT_RE,
    r"(backgroundcolor|linecolor)\s*=\s*([A-Za-z][\w!.]*)"
);

/// Each user `\newmdenv` environment -> `(backgroundcolor, linecolor)`.
pub(crate) fn collect_mdframed_envs(
    source: &str,
) -> HashMap<String, (Option<String>, Option<String>)> {
    let mut out = HashMap::new();
    for m in NEWMDENV_RE.find_iter(source) {
        let name = pystr::strip(m.group_str(2));
        if name.is_empty() {
            continue;
        }
        let mut opts: HashMap<String, String> = HashMap::new();
        for g in MDOPT_RE.findall_groups(m.group(1).unwrap_or("")) {
            opts.insert(g[0].to_string(), g[1].to_string());
        }
        out.insert(
            name.to_string(),
            (
                opts.get("backgroundcolor").cloned(),
                opts.get("linecolor").cloned(),
            ),
        );
    }
    out
}

// --------------------------------------------------------------------------- //
// \newtheorem
// --------------------------------------------------------------------------- //

lazy_re!(
    NEWTHEOREM_RE,
    concat!(
        r"\\newtheorem(\*?)\s*\{([^}]*)\}\s*(?:\[([^\]]*)\])?\s*",
        r"\{((?:[^{}]|\{[^{}]*\})*)\}"
    )
);
lazy_re!(
    TITLE_DECL_RE,
    concat!(
        r"\\(?:textbf|textit|textrm|textsf|textsc|textnormal|texttt|emph|mathrm|",
        r"bfseries|itshape|scshape|sffamily|mdseries|normalfont|upshape|rmfamily)\b"
    )
);

/// Strip wrapping font commands/braces from a `\newtheorem` display title.
pub(crate) fn clean_theorem_title(raw: &str) -> String {
    let title = TITLE_DECL_RE.sub("", raw).replace(['{', '}'], "");
    pystr::strip(&normalize_ws(&title)).to_string()
}

/// User `\newtheorem` environments: `(envs, unnumbered, shared)`.
pub(crate) fn collect_newtheorems(
    source: &str,
) -> (
    tex2word_ir::Dict<String>,
    std::collections::HashSet<String>,
    HashMap<String, String>,
) {
    let mut envs = tex2word_ir::Dict::new();
    let mut unnumbered = std::collections::HashSet::new();
    let mut shared = HashMap::new();
    for m in NEWTHEOREM_RE.find_iter(source) {
        let name = pystr::strip(m.group_str(2)).to_string();
        let display = clean_theorem_title(m.group_str(4));
        if !name.is_empty() && !display.is_empty() {
            envs.insert(name.clone(), display);
            if let Some(s) = m.group(3).filter(|s| !s.is_empty()) {
                shared.insert(name.clone(), pystr::strip(s).to_string());
            }
            if !m.group_str(1).is_empty() {
                unnumbered.insert(name);
            }
        }
    }
    (envs, unnumbered, shared)
}

/// Counter *display name* each theorem env numbers against.
pub(crate) fn resolve_theorem_counters(
    envs: &tex2word_ir::Dict<String>,
    shared: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut counters = HashMap::new();
    for (name, display) in envs.iter() {
        let mut root = name.clone();
        let mut seen = std::collections::HashSet::from([name.clone()]);
        while let Some(next) = shared.get(&root) {
            if seen.contains(next) {
                break;
            }
            root = next.clone();
            seen.insert(root.clone());
        }
        counters.insert(
            name.clone(),
            envs.get(&root).cloned().unwrap_or_else(|| display.clone()),
        );
    }
    counters
}

// --------------------------------------------------------------------------- //
// Preamble scanners
// --------------------------------------------------------------------------- //

/// Return `(body, preamble)`: only the content between `\begin{document}`
/// and `\end{document}` is parsed; without `\begin{document}` the whole
/// input is the body.
pub fn split_document(source: &str) -> (String, String) {
    let Some(begin) = source.find(r"\begin{document}") else {
        return (source.to_string(), String::new());
    };
    let preamble = &source[..begin];
    let body_start = begin + r"\begin{document}".len();
    let end = source.rfind(r"\end{document}").unwrap_or(source.len());
    let body = source.get(body_start..end).unwrap_or("");
    (body.to_string(), preamble.to_string())
}

/// Extract the first `\name{...}` argument from `source` (brace-aware; an
/// optional `[...]` before the mandatory group is skipped).
pub(crate) fn braced_content(source: &str, name: &str) -> Option<String> {
    let pat = re::compile(&format!(r"\\{name}\s*(?:\[[^\]]*\])?\s*\{{"), Flags::NONE);
    let m = pat.search(source)?;
    let b = source.as_bytes();
    let start = m.end() - 1;
    let mut depth: i64 = 0;
    for i in start..b.len() {
        if b[i] == b'{' {
            depth += 1;
        } else if b[i] == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(source[start + 1..i].to_string());
            }
        }
    }
    None
}

lazy_re!(TITLE_OPT_RE, r"\\title\s*\[([^\]]*)\]");
lazy_re!(MARKBOTH_RE, r"\\markboth\s*\{[^{}]*\}\s*\{([^{}]*)\}");

/// Recover a running head declared in the preamble (plain text).
pub(crate) fn running_head_from_preamble(preamble: &str) -> Option<String> {
    let nonempty = |s: String| (!s.is_empty()).then_some(s);
    if let Some(m) = TITLE_OPT_RE.search(preamble) {
        return nonempty(pystr::strip(&normalize_ws(m.group_str(1))).to_string());
    }
    for macro_ in [
        "runninghead",
        "shorttitle",
        "markright",
        "setheadertext",
        "titlerunning",
        "runningtitle",
    ] {
        if let Some(content) = braced_content(preamble, macro_) {
            if !content.is_empty() {
                return nonempty(pystr::strip(&normalize_ws(&content)).to_string());
            }
        }
    }
    if let Some(mb) = MARKBOTH_RE.search(preamble) {
        return nonempty(pystr::strip(&normalize_ws(mb.group_str(1))).to_string());
    }
    None
}

lazy_re!(
    DEFINECOLOR_RE,
    r"\\definecolor\s*\{([^}]*)\}\s*\{([^}]*)\}\s*\{([^}]*)\}"
);
lazy_re!(COLORLET_RE, r"\\colorlet\s*\{([^}]*)\}\s*\{([^}]*)\}");

pub(crate) fn collect_color_defs(source: &str, table: &mut crate::colors::ColorTable) {
    for m in DEFINECOLOR_RE.find_iter(source) {
        table.define(m.group_str(1), m.group_str(2), m.group_str(3));
    }
    for m in COLORLET_RE.find_iter(source) {
        table.define_alias(m.group_str(1), m.group_str(2));
    }
}

/// glossaries acronym references (capitalised/plural variants are matched
/// case-insensitively / by the "pl" suffix).
pub(crate) fn is_gls_macro(lower: &str) -> bool {
    matches!(
        lower,
        "gls"
            | "glspl"
            | "acrshort"
            | "acrshortpl"
            | "acrlong"
            | "acrlongpl"
            | "acrfull"
            | "acrfullpl"
            | "glsentryshort"
            | "glsentrylong"
    )
}

pub(crate) const PRINTGLOSSARY_MACROS: &[&str] = &[
    "printglossaries",
    "printglossary",
    "printnoidxglossaries",
    "printnoidxglossary",
    "printacronyms",
    "printabbreviations",
];

/// acronym package: `\ac/\acs/\acl/\acf` (+ plural -p) -> glossaries names.
pub(crate) fn ac_to_gls(lower: &str) -> Option<&'static str> {
    Some(match lower {
        "ac" => "gls",
        "acp" => "glspl",
        "acs" => "acrshort",
        "acsp" => "acrshortpl",
        "acl" => "acrlong",
        "aclp" => "acrlongpl",
        "acf" => "acrfull",
        "acfp" => "acrfullpl",
        _ => return None,
    })
}

lazy_re!(
    NEWACRONYM_RE,
    r"\\newacronym\s*(?:\[[^\]]*\])?\s*\{([^}]*)\}\s*\{([^}]*)\}\s*\{([^}]*)\}"
);
lazy_re!(
    ACRO_RE,
    r"\\acro(?:def)?\s*\{([^}]*)\}\s*(?:\[([^\]]*)\])?\s*\{([^}]*)\}"
);

pub(crate) fn collect_acronyms(source: &str, acronyms: &mut tex2word_ir::Dict<(String, String)>) {
    for m in NEWACRONYM_RE.find_iter(source) {
        acronyms.insert(
            pystr::strip(m.group_str(1)),
            (
                pystr::strip(m.group_str(2)).to_string(),
                pystr::strip(m.group_str(3)).to_string(),
            ),
        );
    }
    for m in ACRO_RE.find_iter(source) {
        let key = pystr::strip(m.group_str(1)).to_string();
        let short = pystr::strip(m.group(2).unwrap_or(&key)).to_string();
        acronyms.set_default(
            key.clone(),
            (short, pystr::strip(m.group_str(3)).to_string()),
        );
    }
}

lazy_re!(
    NEWGLOSSARYENTRY_RE,
    r"\\newglossaryentry\s*\{([^}]*)\}\s*\{"
);
lazy_re!(GLS_NAME_RE, r"\bname\s*=\s*(?:\{([^{}]*)\}|([^,}]+))");

/// Register `\newglossaryentry{key}{name=…, description=…}` term names.
pub(crate) fn collect_glossary_entries(source: &str, glossary: &mut tex2word_ir::Dict<String>) {
    let b = source.as_bytes();
    let n = b.len();
    for m in NEWGLOSSARYENTRY_RE.find_iter(source) {
        let key = pystr::strip(m.group_str(1)).to_string();
        let i = m.end(); // just past the opening "{" of the options group
        let mut depth = 1;
        let mut j = i;
        while j < n && depth != 0 {
            if b[j] == b'{' {
                depth += 1;
            } else if b[j] == b'}' {
                depth -= 1;
            }
            j += 1;
        }
        // source[i : j - 1] (j - 1 is one code point back)
        let mut stop = j;
        if stop > 0 {
            stop -= 1;
            while !source.is_char_boundary(stop) {
                stop -= 1;
            }
        }
        let opts = if stop > i { &source[i..stop] } else { "" };
        if let Some(nm) = GLS_NAME_RE.search(opts) {
            let name = nm
                .group(1)
                .filter(|s| !s.is_empty())
                .or(nm.group(2))
                .unwrap_or("");
            glossary.insert(key, pystr::strip(name).to_string());
        }
    }
}

lazy_re!(
    BOOK_CLASS_RE,
    r"\\documentclass(?:\[[^\]]*\])?\{(book|report|memoir|scrbook|scrreprt)\}"
);
lazy_re!(CHAPTER_RE, r"\\chapter\b");

/// True for a book/report-style document (top sectioning level is \chapter).
pub(crate) fn is_book_class(source: &str) -> bool {
    BOOK_CLASS_RE.is_match(source) || CHAPTER_RE.is_match(source)
}

lazy_re!(DOCCLASS_OPTS_RE, r"\\documentclass\s*\[([^\]]*)\]");
lazy_re!(MULTICOLS_RE, r"\\begin\{multicols\*?\}\s*\{\s*(\d+)\s*\}");
lazy_re!(TWOCOLUMN_RE, r"\\twocolumn\b");

/// Body column count from the class options / `\twocolumn` / `multicols`.
pub(crate) fn detect_columns(source: &str) -> i64 {
    let mut cols = 1;
    if let Some(m) = DOCCLASS_OPTS_RE.search(source) {
        if m.group_str(1)
            .split(',')
            .any(|o| pystr::strip(o) == "twocolumn")
        {
            cols = 2;
        }
    }
    if TWOCOLUMN_RE.is_match(source) {
        cols = cols.max(2);
    }
    for mc in MULTICOLS_RE.find_iter(source) {
        if let Some(v) = pystr::parse_int(mc.group_str(1)) {
            cols = cols.max(v);
        }
    }
    cols
}

pub(crate) fn babel_lang(name: &str) -> Option<&'static str> {
    Some(match name {
        "english" | "american" | "usenglish" | "usenglish " => "en-US",
        "british" | "ukenglish" => "en-GB",
        "australian" => "en-AU",
        "canadian" => "en-CA",
        "ngerman" | "german" => "de-DE",
        "austrian" | "naustrian" => "de-AT",
        "french" | "francais" => "fr-FR",
        "canadien" => "fr-CA",
        "spanish" => "es-ES",
        "italian" => "it-IT",
        "portuguese" | "portuges" => "pt-PT",
        "brazil" | "brazilian" => "pt-BR",
        "dutch" => "nl-NL",
        "russian" => "ru-RU",
        "polish" => "pl-PL",
        "swedish" => "sv-SE",
        "danish" => "da-DK",
        "norsk" => "nb-NO",
        "finnish" => "fi-FI",
        "czech" => "cs-CZ",
        "greek" => "el-GR",
        "turkish" => "tr-TR",
        "japanese" => "ja-JP",
        "chinese" => "zh-CN",
        _ => return None,
    })
}

lazy_re!(SETLANG_RE, r"\\set(?:main|default)language\{([^}]*)\}");
lazy_re!(BABEL_RE, r"\\usepackage\[([^\]]*)\]\{babel\}");
lazy_re!(DOCCLASS_OPT_RE, r"\\documentclass\[([^\]]*)\]");

/// The main document language as a BCP-47 code from babel/polyglossia.
pub(crate) fn detect_language(preamble: &str) -> Option<String> {
    if let Some(m) = SETLANG_RE.search(preamble) {
        if let Some(l) = babel_lang(&pystr::lower(pystr::strip(m.group_str(1)))) {
            return Some(l.into());
        }
    }
    if let Some(m) = BABEL_RE.search(preamble) {
        let opts: Vec<String> = m
            .group_str(1)
            .split(',')
            .map(|o| pystr::lower(pystr::strip(o)))
            .collect();
        let main = opts
            .iter()
            .find(|o| o.starts_with("main="))
            .map(|o| o.split_once('=').unwrap().1.to_string());
        if let Some(l) = main.as_deref().and_then(babel_lang) {
            return Some(l.into());
        }
        if let Some(l) = opts.iter().rev().find_map(|o| babel_lang(o)) {
            return Some(l.into()); // babel's main language is the last option
        }
    }
    if let Some(m) = DOCCLASS_OPT_RE.search(preamble) {
        if let Some(l) = m
            .group_str(1)
            .split(',')
            .map(|o| pystr::lower(pystr::strip(o)))
            .rev()
            .find_map(|o| babel_lang(&o))
        {
            return Some(l.into());
        }
    }
    None
}

/// A fontspec/xeCJK `\cmd[opts]{Font}` value from the preamble.
pub(crate) fn font_cmd(preamble: &str, cmd: &str) -> Option<String> {
    let pat = re::compile(
        &format!(r"\\{cmd}\s*(?:\[[^\]]*\])?\s*\{{([^}}]*)\}}"),
        Flags::NONE,
    );
    let m = pat.search(preamble)?;
    let v = pystr::strip(m.group_str(1));
    (!v.is_empty()).then(|| v.to_string())
}

pub use tex2word_ir::pyos::today_text;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_markers() {
        assert_eq!(script_marker("^{*1}"), Some(("superscript", "*1".into())));
        assert_eq!(script_marker("^\\circ"), Some(("plain", "°".into())));
        assert_eq!(script_marker("^{a}_b"), None);
        assert_eq!(
            parse_colspec("|l|>{\\centering}p{2cm}r|").0,
            vec!["left", "center", "right"]
        );
        assert_eq!(apply_text_accent("'", "e"), "é");
    }
}
