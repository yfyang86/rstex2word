//! Source preprocessing: comment stripping, `\input`/`\include` flattening and
//! the source-level rewrites that run before macro expansion (port of
//! `frontend/preprocess.py`).

use std::io;
use std::sync::LazyLock;

use tex2word_ir::pyos::{self, Errors};
use tex2word_ir::pystr;
use tex2word_ir::re::{self, Flags, Regex};

macro_rules! lazy_re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat));
    };
    ($name:ident, $pat:expr, $flags:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::with_flags($pat, $flags));
    };
}

// A TeX comment runs from an unescaped `%` to the end of the line *and*
// consumes the line-ending newline plus the next line's leading whitespace.
// Eating the newline is essential: otherwise a full-line `% comment` collapses
// to a blank line and is misread as a paragraph break.
lazy_re!(COMMENT_RE, r"(?<!\\)%[^\n]*(?:\n[ \t]*)?");
lazy_re!(INPUT_RE, r"\\(?:input|include)\s*\{([^}]+)\}");
// import package: \import{dir/}{file}, \subimport{dir/}{file} (and *from variants)
lazy_re!(
    IMPORT_RE,
    r"\\(?:sub)?(?:import|includefrom|inputfrom)\s*\{([^}]+)\}\s*\{([^}]+)\}"
);
// booktabs \cmidrule(lr){2-3} trim spec -- drop the (lr)/(l)/(r)
lazy_re!(CMIDRULE_TRIM_RE, r"(\\cmidrule)\s*\([lr]*\)");

/// Remove LaTeX line comments, preserving escaped `\%`.
pub fn strip_comments(source: &str) -> String {
    COMMENT_RE.sub("", source)
}

/// Inline `\input`/`\include` (and import-package `\import`) files.
pub fn flatten_inputs(source: &str, base_dir: &str) -> io::Result<String> {
    flatten_inputs_depth(source, base_dir, 0)
}

fn flatten_inputs_depth(source: &str, base_dir: &str, depth: usize) -> io::Result<String> {
    if depth > 20 {
        return Ok(source.to_string());
    }
    let inline = |path: &str| -> io::Result<Option<String>> {
        if !pyos::isfile(path) {
            return Ok(None);
        }
        let inner = strip_comments(&pyos::read_text(path, Errors::Strict)?);
        let dir = pyos::dirname(path);
        let dir = if dir.is_empty() {
            base_dir.to_string()
        } else {
            dir
        };
        flatten_inputs_depth(&inner, &dir, depth + 1).map(Some)
    };
    let candidates = |name: &str| -> Vec<String> {
        if name.ends_with(".tex") {
            vec![name.to_string()]
        } else {
            vec![name.to_string(), format!("{name}.tex")]
        }
    };
    let mut err: Option<io::Error> = None;
    let source = IMPORT_RE.sub_fn(source, |m| {
        // \import{dir/}{file}: the file lives under dir, relative to base_dir
        let directory = pystr::strip(m.group_str(1));
        let name = pystr::strip(m.group_str(2));
        for cand in candidates(name) {
            match inline(&pyos::join(base_dir, &[directory, &cand])) {
                Ok(Some(text)) => return text,
                Ok(None) => {}
                Err(e) => {
                    err.get_or_insert(e);
                    return String::new();
                }
            }
        }
        String::new()
    });
    let out = INPUT_RE.sub_fn(&source, |m| {
        let name = pystr::strip(m.group_str(1));
        for cand in candidates(name) {
            match inline(&pyos::join2(base_dir, &cand)) {
                Ok(Some(text)) => return text,
                Ok(None) => {}
                Err(e) => {
                    err.get_or_insert(e);
                    return String::new();
                }
            }
        }
        String::new() // missing include -> drop (graceful degradation)
    });
    match err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

// \verb* (show-spaces form) -> \verb, which pylatexenc parses natively.
lazy_re!(VERBSTAR_RE, r"\\verb\*");
// Delimiter-form inline listings -> \verb<delim>…<delim>.
lazy_re!(
    LSTINLINE_DELIM_RE,
    r"\\lstinline\s*(?:\[[^\]]*\])?\s*([^\s{[*])"
);
lazy_re!(
    MINTINLINE_DELIM_RE,
    r"\\mintinline\s*(?:\[[^\]]*\])?\s*\{[^}]*\}\s*([^\s{[*])"
);
// \lstinputlisting[opts]{file} / \verbatiminput{file}: embed an external
// source file verbatim (after flattening, so it is never comment-stripped).
lazy_re!(
    LSTINPUT_RE,
    r"\\(?:lstinputlisting|verbatiminput)\s*(?:\[[^\]]*\])?\s*\{([^}]+)\}"
);

/// Replace `\lstinputlisting{file}`/`\verbatiminput{file}` by the file body
/// in a `verbatim` environment.
pub fn inline_listing_files(source: &str, base_dir: &str) -> io::Result<String> {
    let mut err = None;
    let out = LSTINPUT_RE.sub_fn(source, |m| {
        let path = pyos::join2(base_dir, pystr::strip(m.group_str(1)));
        if pyos::isfile(&path) {
            match pyos::read_text(&path, Errors::Strict) {
                Ok(text) => {
                    return format!(
                        "\\begin{{verbatim}}\n{}\n\\end{{verbatim}}",
                        pystr::rstrip_chars(&text, "\n")
                    )
                }
                Err(e) => {
                    err.get_or_insert(e);
                }
            }
        }
        String::new() // missing source file -> drop (graceful degradation)
    });
    match err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

// Code-listing environments pylatexenc has no verbatim spec for: normalise
// them to `verbatim` (which it captures literally), dropping [options] and a
// minted {lang}. The backreference ties \end to the same environment name.
lazy_re!(
    LISTING_ENV_RE,
    concat!(
        r"\\begin\{(lstlisting|minted|Verbatim\*?|verbatim\*)\}",
        r"[ \t]*(?:\[(?:[^\[\]{}]|\{(?:[^{}]|\{[^{}]*\})*\}|\[[^\]]*\])*\])?",
        r"[ \t]*(?:\{[^}]*\})?",
        r"(?P<body>.*?)",
        r"\\end\{\1\}"
    ),
    Flags::S
);

/// Rewrite `lstlisting`/`minted`/`Verbatim` blocks to `verbatim`.
pub fn normalize_listing_envs(source: &str) -> String {
    LISTING_ENV_RE.sub_fn(source, |m| {
        format!(
            "\\begin{{verbatim}}{}\\end{{verbatim}}",
            m.name("body").unwrap_or("")
        )
    })
}

// amsmath \DeclareMathOperator{\name}{body} (and starred) -> a \newcommand
// wrapping the body in \operatorname (bodies may nest two levels of braces).
lazy_re!(
    DECLAREMATHOP_RE,
    concat!(
        r"\\DeclareMathOperator\s*(\*?)\s*\{\s*\\([A-Za-z]+)\s*\}\s*",
        r"\{((?:[^{}]|\{(?:[^{}]|\{[^{}]*\})*\})*)\}"
    )
);

/// `\DeclareMathOperator` -> `\newcommand{\name}{\operatorname{body}}`.
pub fn rewrite_mathoperators(source: &str) -> String {
    DECLAREMATHOP_RE.sub_fn(source, |m| {
        format!(
            "\\newcommand{{\\{}}}{{\\operatorname{}{{{}}}}}",
            m.group_str(2),
            m.group_str(1),
            m.group_str(3)
        )
    })
}

lazy_re!(IFFALSE_RE, r"\\iffalse(?![a-zA-Z@])");
// An \if… opener is a control word; \fi closes; \else splits. The lookaheads
// keep \fi from matching \fill/\final and \else from \elsewhere.
lazy_re!(
    IFFI_RE,
    r"\\(if[a-zA-Z@]*|else(?![a-zA-Z@])|fi(?![a-zA-Z@]))"
);

/// Resolve `\iffalse … \fi` blocks: drop the false branch, keep an `\else`
/// branch, tracking nested `\if…`/`\fi`.
pub fn strip_iffalse(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let n = source.len();
    let mut i = 0;
    while i < n {
        let Some(m) = IFFALSE_RE.search_from(source, i) else {
            out.push_str(&source[i..]);
            break;
        };
        out.push_str(&source[i..m.start()]);
        let mut depth: i64 = 1;
        let mut else_at: Option<usize> = None;
        let mut j = n; // unbalanced -> drop to end of source
        for cm in IFFI_RE.find_iter_from(source, m.end()) {
            let tok = cm.group_str(1);
            if tok == "else" {
                if depth == 1 && else_at.is_none() {
                    else_at = Some(cm.end());
                }
                continue;
            }
            depth += if tok == "fi" { -1 } else { 1 };
            if depth == 0 {
                if let Some(e) = else_at {
                    out.push_str(&source[e..cm.start()]);
                }
                j = cm.end();
                break;
            }
        }
        i = j;
    }
    out
}

// \begin*{figure} (a common typo) -> \begin{figure*}
lazy_re!(BEGINEND_STAR_RE, r"\\(begin|end)\*\s*\{([^}]*)\}");

#[doc(hidden)]
pub fn normalize_begin_star(source: &str) -> String {
    BEGINEND_STAR_RE.sub_fn(source, |m| {
        format!(
            "\\{}{{{}*}}",
            m.group_str(1),
            pystr::rstrip_chars(pystr::strip(m.group_str(2)), "*")
        )
    })
}

// exam class: question/part markers (optional [points]) and environments.
lazy_re!(
    EXAM_ITEM_RE,
    r"\\(?:miquestion|question|subpart|part)\b\s*(?:\[[^\]]*\])?"
);
const EXAM_ENVS: [&str; 3] = ["questions", "parts", "subparts"];
const EXAM_SOLUTION_ENVS: [&str; 4] = [
    "solution",
    "solutionorbox",
    "solutionorlines",
    "solutionordottedlines",
];

/// True if the document is (directly or via a local `.cls`) the exam class.
pub fn uses_exam_class(source: &str, base_dir: &str) -> bool {
    if re::search(r"\\documentclass(?:\[[^\]]*\])?\{exam\}", source).is_some() {
        return true;
    }
    let Some(m) = re::search(r"\\documentclass(?:\[[^\]]*\])?\{([^}]+)\}", source) else {
        return false;
    };
    let path = pyos::join2(base_dir, &format!("{}.cls", pystr::strip(m.group_str(1))));
    match pyos::read_text(&path, Errors::Ignore) {
        Ok(content) => re::search(
            r"\\LoadClass(?:WithOptions)?(?:\[[^\]]*\])?\{exam\}",
            &content,
        )
        .is_some(),
        Err(_) => false,
    }
}

/// Rewrite the `exam` class's structure into nested `enumerate`/`\item`.
fn rewrite_exam_class(source: &str, base_dir: &str) -> String {
    if !uses_exam_class(source, base_dir) {
        return source.to_string();
    }
    let mut source = source.to_string();
    if re::search(r"\\printanswers\b", &source).is_none() {
        for env in EXAM_SOLUTION_ENVS {
            let pat = format!(r"\\begin\{{{env}\}}.*?\\end\{{{env}\}}");
            source = re::compile(&pat, Flags::S).sub("", &source);
        }
    }
    for env in EXAM_ENVS {
        source = re::sub(
            &format!(r"\\begin\{{{env}\}}"),
            r"\\begin{enumerate}",
            &source,
        );
        source = re::sub(&format!(r"\\end\{{{env}\}}"), r"\\end{enumerate}", &source);
    }
    EXAM_ITEM_RE.sub(r"\\item ", &source)
}

// TikZ drawing primitives: a real diagram, left for the image/compile path.
lazy_re!(
    TIKZ_DRAW_RE,
    r"\\(draw|fill|filldraw|path|clip|shade|shadedraw|pgf|coordinate|foreach|pic)\b"
);
// a \tikzstyle{name} = [ ... ] declaration (produces no content)
lazy_re!(
    TIKZSTYLE_RE,
    r"\\tikzstyle\s*\{[^}]*\}\s*=\s*\[[^\]]*\]",
    Flags::S
);

/// Each `\node[style] … {content}`'s (style, content).
fn extract_tikz_nodes(body: &str) -> Vec<(String, String)> {
    let mut nodes = Vec::new();
    for m in re::compile(r"\\node\b", Flags::NONE).find_iter(body) {
        let mut i = m.end();
        while i < body.len() && matches!(body.as_bytes()[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        let mut style = String::new();
        if body[i..].starts_with('[') {
            let (s, j) = read_balanced(body, i, '[', ']');
            style = s.to_string();
            i = j;
        }
        let Some(off) = body[i..].find('{') else {
            continue;
        };
        let (content, _) = read_balanced(body, i + off, '{', '}');
        nodes.push((style, content.to_string()));
    }
    nodes
}

/// Recover content from the "cheatsheet" TikZ idiom: a `tikzpicture` of
/// `\node{…minipage…}` content boxes plus a `\node[fancytitle]{Title}`, with
/// no drawing. A `title`-styled node becomes a `\subsection*`.
#[doc(hidden)]
pub fn recover_tikz_boxes(source: &str) -> String {
    let title_re = re::compile("title", Flags::I);
    re::compile(r"\\begin\{tikzpicture\}(.*?)\\end\{tikzpicture\}", Flags::S).sub_fn(source, |m| {
        let body = m.group_str(1);
        if TIKZ_DRAW_RE.is_match(body) {
            return m.as_str().to_string();
        }
        if !body.contains("\\begin{minipage}") && !title_re.is_match(body) {
            return m.as_str().to_string();
        }
        let nodes = extract_tikz_nodes(body);
        if nodes.is_empty() {
            return m.as_str().to_string();
        }
        let mut out: Vec<String> = Vec::new();
        for (style, content) in nodes {
            if title_re.is_match(&style) {
                let title = pystr::strip(&content);
                if !title.is_empty() {
                    out.push(format!("\n\n\\subsection*{{{title}}}\n"));
                }
            } else {
                out.push(content);
            }
        }
        out.join("\n") + "\n"
    })
}

/// Convert a plain-TeX `\halign` system inside `\[ … \]` into an `array`.
#[doc(hidden)]
pub fn rewrite_halign(source: &str) -> String {
    fn to_array(inner: &str) -> String {
        let Some(h) = inner.find(r"\halign") else {
            return inner.to_string();
        };
        let Some(brace) = inner[h..].find('{').map(|k| h + k) else {
            return inner.to_string();
        };
        let (body, _) = read_balanced(inner, brace, '{', '}');
        // the template precedes the first \cr; the rest are the data rows
        let data = match body.find(r"\cr") {
            Some(cr) => &body[cr + 3..],
            None => body,
        };
        let rows: Vec<&str> = data
            .split(r"\cr")
            .map(pystr::strip)
            .filter(|r| !r.is_empty())
            .collect();
        if rows.is_empty() {
            return inner.to_string();
        }
        let ncols = rows
            .iter()
            .map(|r| r.matches('&').count())
            .max()
            .unwrap_or(0)
            + 1;
        format!(
            "\\begin{{array}}{{{}}}{}\\end{{array}}",
            "c".repeat(ncols),
            rows.join(r" \\ ")
        )
    }
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < source.len() {
        if source[i..].starts_with(r"\[") {
            if let Some(off) = source[i + 2..].find(r"\]") {
                let end = i + 2 + off;
                if source[i + 2..end].contains(r"\halign") {
                    out.push_str(r"\[");
                    out.push_str(&to_array(&source[i + 2..end]));
                    out.push_str(r"\]");
                    i = end + 2;
                    continue;
                }
            }
        }
        let c = source[i..].chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Recover an Oxford-style problem-sheet title block from
/// `\course`/`\sheetnumber`/`\oxfordterm`/`\sheettitle`.
#[doc(hidden)]
pub fn inject_exam_title(source: &str) -> String {
    let val = |cmd: &str| -> String {
        re::search(&format!(r"\\{cmd}\s*\{{([^}}]*)\}}"), source)
            .map(|m| pystr::strip(m.group_str(1)).to_string())
            .unwrap_or_default()
    };
    let (course, num, term, title) = (
        val("course"),
        val("sheetnumber"),
        val("oxfordterm"),
        val("sheettitle"),
    );
    if course.is_empty() && title.is_empty() {
        return source.to_string();
    }
    let mut lines: Vec<String> = Vec::new();
    if !course.is_empty() {
        lines.push(format!(r"{{\Large\bfseries {course}}}"));
    }
    let mut sheet = if num.is_empty() {
        String::new()
    } else {
        format!("Sheet {num}")
    };
    if !term.is_empty() {
        sheet = if sheet.is_empty() {
            term.clone()
        } else {
            format!("{sheet} --- {term}")
        };
    }
    if !sheet.is_empty() {
        lines.push(sheet);
    }
    if !title.is_empty() {
        lines.push(title);
    }
    let block = format!(
        "\n\\begin{{center}}\n{}\n\\end{{center}}\n",
        lines.join(r"\\")
    );
    re::compile(r"\\begin\{document\}", Flags::NONE)
        .subn_fn(source, 1, |m| format!("{}{block}", m.as_str()))
        .0
}

/// The full preprocessing pass (`preprocess(source, base_dir)`).
pub fn preprocess(source: &str, base_dir: &str) -> io::Result<String> {
    // Flatten \input/\include FIRST so every later rewrite sees the included
    // content too.
    let mut source = flatten_inputs(&strip_comments(source), base_dir)?;
    if uses_exam_class(&source, base_dir) {
        source = rewrite_exam_class(&source, base_dir);
        source = inject_exam_title(&source);
    }
    source = rewrite_halign(&source);
    source = TIKZSTYLE_RE.sub("", &source);
    source = recover_tikz_boxes(&source);
    source = normalize_begin_star(&source);
    source = strip_iffalse(&source);
    source = inline_listing_files(&source, base_dir)?;
    source = rewrite_mathoperators(&source);
    source = VERBSTAR_RE.sub(r"\\verb", &source);
    source = MINTINLINE_DELIM_RE.sub_fn(&source, |m| format!(r"\verb{}", m.group_str(1)));
    source = LSTINLINE_DELIM_RE.sub_fn(&source, |m| format!(r"\verb{}", m.group_str(1)));
    source = CMIDRULE_TRIM_RE.sub(r"\1", &source);
    Ok(normalize_listing_envs(&source))
}

/// Read a balanced `open..close` group starting at byte `i` (which must hold
/// `open`); returns `(inner, end_index)`.
pub fn read_balanced(s: &str, i: usize, open: char, close: char) -> (&str, usize) {
    debug_assert!(s[i..].starts_with(open));
    let mut depth = 0i64;
    for (off, c) in s[i..].char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                let j = i + off;
                return (&s[i + open.len_utf8()..j], j + close.len_utf8());
            }
        }
    }
    (&s[i + open.len_utf8()..], s.len())
}

fn circled(number: i64) -> Option<String> {
    match number {
        0 => Some("⓪".into()),
        1..=20 => char::from_u32(0x2460 + number as u32 - 1).map(String::from),
        _ => None,
    }
}

/// An inline TikZ snippet's fallback: a circled number, else nothing.
fn tikz_replacement(content: &str) -> String {
    let nums = re::findall(r"\{\s*(\d+)\s*\}", content);
    match nums.last() {
        Some(last) => {
            let n = pystr::parse_int(last).unwrap_or(0);
            circled(n).unwrap_or_else(|| format!("({last})"))
        }
        None => String::new(),
    }
}

/// Replace inline `\tikz ...;` / `\tikz{...}` constructs (pylatexenc cannot
/// scope them) with a circled number for the circled-number idiom, else
/// nothing.
pub fn replace_inline_tikz(source: &str) -> String {
    let n = source.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        let next_is_alpha = source[i..]
            .strip_prefix("\\tikz")
            .map(|rest| rest.chars().next().is_some_and(pystr::is_alpha));
        if next_is_alpha == Some(false) {
            let mut j = i + 5;
            let skip_ws = |j: &mut usize| {
                while *j < n && matches!(source.as_bytes()[*j], b' ' | b'\t' | b'\n') {
                    *j += 1;
                }
            };
            skip_ws(&mut j);
            if source[j..].starts_with('[') {
                j = read_balanced(source, j, '[', ']').1;
            }
            skip_ws(&mut j);
            let content: String;
            if source[j..].starts_with('{') {
                let (c, k) = read_balanced(source, j, '{', '}');
                content = c.to_string();
                j = k;
            } else {
                let end = source[j..].find(';').map_or(n, |k| j + k);
                content = source[j..end].to_string();
                j = (end + 1).min(n);
            }
            out.push_str(&tikz_replacement(&content));
            i = j;
        } else {
            let c = source[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

// -- TeX spacing primitives and icon fonts (applied after macro expansion) -- //

const GLUE_UNIT: &str = r"(?:pt|mm|cm|in|em|ex|bp|pc|sp|dd|cc|mu|fil{1,3}|true\s*(?:pt|mm|cm|in))";

fn dimen() -> String {
    format!(r"(?:[-+]\s*)?(?:(?:\d+(?:\.\d*)?|\.\d+)\s*(?:{GLUE_UNIT}|\\[A-Za-z@]+)|\\[A-Za-z@]+)")
}

static SPACING_PRIM_RE: LazyLock<Regex> = LazyLock::new(|| {
    let d = dimen();
    let glue = format!(r"{d}(?:\s*plus\s*{d})?(?:\s*minus\s*{d})?");
    Regex::new(&format!(
        r"\\(?:vskip|hskip|mskip|kern|mkern)(?![A-Za-z@])\s*{glue}"
    ))
});

/// fontawesome icons with a sensible Unicode stand-in; the rest are dropped.
fn fa_glyph(name: &str) -> &'static str {
    match name {
        "Envelope" | "EnvelopeOpen" => "✉",
        "Phone" | "PhoneAlt" => "☎",
        "Check" => "✓",
        "CheckCircle" => "✔",
        "CheckSquare" => "☑",
        "Times" | "TimesCircle" | "Close" => "✗",
        "Star" | "StarHalf" => "★",
        "Heart" => "♥",
        "ArrowRight" => "→",
        "ArrowLeft" => "←",
        "ArrowUp" => "↑",
        "ArrowDown" => "↓",
        "LongArrowAltRight" => "⟶",
        "InfoCircle" | "Info" => "ℹ",
        "ExclamationTriangle" => "⚠",
        "Exclamation" => "!",
        "Question" => "?",
        "Copyright" => "©",
        "Music" => "♪",
        "Sun" => "☀",
        "Cloud" => "☁",
        "Plus" => "+",
        "Minus" => "−",
        "Bars" => "≡",
        "Square" => "□",
        "Circle" => "●",
        "Flag" => "⚑",
        _ => "",
    }
}

lazy_re!(
    FA_RE,
    r"\\fa([A-Z][A-Za-z]*)\*?(?:\s*\[[^\]]*\])?(?![A-Za-z@])"
);
lazy_re!(FAICON_RE, r"\\faIcon\*?(?:\s*\[[^\]]*\])?\s*\{([^}]*)\}");
lazy_re!(
    VERBATIM_BLOCK_RE,
    r"\\begin\{verbatim\}.*?\\end\{verbatim\}",
    Flags::S
);

fn fa_name_glyph(name: &str) -> &'static str {
    // \faIcon{arrow-right} -> ArrowRight
    let camel: String = re::split(r"[-_\s]+", pystr::strip(name))
        .into_iter()
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut cs = p.chars();
            let first = cs
                .next()
                .map(|c| c.to_uppercase().collect::<String>())
                .unwrap_or_default();
            first + cs.as_str()
        })
        .collect();
    fa_glyph(&camel)
}

fn clean_spacing_and_icons(text: &str) -> String {
    let text = SPACING_PRIM_RE.sub("", text);
    let text = FAICON_RE.sub_fn(&text, |m| fa_name_glyph(m.group_str(1)).to_string());
    FA_RE.sub_fn(&text, |m| fa_glyph(m.group_str(1)).to_string())
}

/// Drop TeX spacing primitives with their glue (`\vskip -1mm`, `\kern2pt`) and
/// replace fontawesome icons by a Unicode stand-in (or nothing); verbatim
/// bodies are left untouched.
pub fn normalize_spacing_and_icons(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut pos = 0;
    for m in VERBATIM_BLOCK_RE.find_iter(source) {
        out.push_str(&clean_spacing_and_icons(&source[pos..m.start()]));
        out.push_str(m.as_str());
        pos = m.end();
    }
    out.push_str(&clean_spacing_and_icons(&source[pos..]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_stripping_eats_the_newline() {
        assert_eq!(strip_comments("a % c\n  b \\% d"), "a b \\% d");
    }

    #[test]
    fn iffalse_keeps_else_branch() {
        assert_eq!(strip_iffalse(r"x\iffalse A \else B\fi y"), "x B y");
        assert_eq!(strip_iffalse(r"x\iffalse A \ifnum1>0 C\fi \fi y"), "x y");
    }

    #[test]
    fn inline_tikz_circled_number() {
        assert_eq!(
            replace_inline_tikz(r"see \tikz\node[circle]{3}; here"),
            "see ③ here"
        );
    }

    #[test]
    fn spacing_primitives_are_dropped() {
        assert_eq!(
            normalize_spacing_and_icons(r"a\vskip -1mm b \faEnvelope c"),
            "a b ✉ c"
        );
    }
}
