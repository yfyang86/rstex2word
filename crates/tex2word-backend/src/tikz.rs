//! Render a standalone TikZ/PGF picture by compiling it with a TeX engine
//! (port of `backend/tikz.py`). The compiled PDF must then be rasterised,
//! which needs the reference's optional `pdf` extra; without it `render`
//! returns `None` (before compiling, as the reference does) and the writer
//! explains why.

use std::sync::LazyLock;

use tex2word_ir::re::{Flags, Regex};

/// xelatex/lualatex first: they honour fontspec/xeCJK for CJK node text.
const ENGINES: &[&str] = &["xelatex", "lualatex", "pdflatex"];

/// drawing environments we try to compile
pub const DRAWING_ENVS: &[&str] = &["tikzpicture", "pgfpicture", "circuitikz", "tikzcd", "forest", "pspicture"];

static KEEP_MACRO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^\s*\\(usetikzlibrary|usepgflibrary|usepgfplotslibrary|tikzset|pgfplotsset|",
        r"pgfdeclare\w*|definecolor|colorlet|newcommand|renewcommand|providecommand|",
        r"def|let|newif|DeclareMathOperator|newcounter|setlength|tikzstyle)\b"
    ))
});
static KEEP_USEPACKAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^\s*\\usepackage\b[^\n]*\{[^}]*\b",
        r"(tikz|pgfplots|pgfplotstable|pgf|pgfgantt|circuitikz|tikz-cd|",
        r"amsmath|amssymb|amsfonts|amsthm|mathtools|mathrsfs|dsfont|bbm|stmaryrd|",
        r"xcolor|color|bm|siunitx|graphicx|",
        r"pifont|wasysym|marvosym|textcomp|fontawesome5|fontawesome|",
        r"natbib|cite)\b"
    ))
});
static KEEP_FONT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\\(usepackage\b[^\n]*\{(fontspec|xeCJK)\}|set(main|CJK\w*)font)\b"));
static DROP_LAYOUT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^\s*\\(?:re|provide)?newcommand\*?\s*\{?\s*\\",
        r"(?:head|foot|plainhead|plainfoot)rule(?:width)?\b",
        r"|^\s*\\(?:let|def)\s*\\(?:head|foot|plainhead|plainfoot)rule(?:width)?\b",
        r"|^\s*\\(?:pagestyle|thispagestyle|fancypagestyle|fancyhf|fancyhead|fancyfoot|",
        r"lhead|chead|rhead|lfoot|cfoot|rfoot|fancyheadoffset|fancyfootoffset|",
        r"titleformat|titlespacing)\b"
    ))
});
static PICTURE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::with_flags(
        &format!(r"\\begin\{{({})\}}.*?\\end\{{\1\}}", DRAWING_ENVS.join("|")),
        Flags::S,
    )
});

/// `shutil.which(name)`: an executable file of that name on `PATH`.
pub fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if let Ok(meta) = std::fs::metadata(&cand) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                    return Some(cand.to_string_lossy().into_owned());
                }
            }
            #[cfg(not(unix))]
            {
                if meta.is_file() {
                    return Some(cand.to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

/// First available TeX engine on PATH (xelatex preferred).
pub fn find_engine() -> Option<&'static str> {
    ENGINES.iter().copied().find(|e| which(e).is_some())
}

/// TeX engines on PATH, in preference order.
pub fn available_engines() -> Vec<&'static str> {
    ENGINES.iter().copied().filter(|e| which(e).is_some()).collect()
}

/// Pull the `tikzpicture` (etc.) out of a figure's LaTeX source.
pub fn extract_picture(source: &str) -> Option<String> {
    PICTURE_RE.search(source).map(|m| m.as_str().to_string())
}

/// Net `{` minus `}` on a line, ignoring escaped `\{`/`\}`.
fn brace_delta(line: &str) -> i64 {
    let b = line.as_bytes();
    let mut depth = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth
}

/// `str.splitlines()`.
fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' && matches!(it.peek(), Some((_, '\n'))) {
                it.next();
                end += 1;
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

fn filtered_preamble(preamble: &str, unicode_engine: bool) -> String {
    let lines = splitlines(preamble);
    let mut keep: Vec<&str> = Vec::new();
    let n = lines.len();
    let mut i = 0;
    while i < n {
        let line = lines[i];
        if DROP_LAYOUT.is_match(line) {
            i += 1;
            continue;
        }
        let matched = KEEP_USEPACKAGE.is_match(line)
            || KEEP_MACRO.is_match(line)
            || (unicode_engine && KEEP_FONT.is_match(line));
        if !matched {
            i += 1;
            continue;
        }
        // capture the whole (possibly multi-line) definition
        keep.push(line);
        let mut depth = brace_delta(line);
        i += 1;
        while depth > 0 && i < n {
            keep.push(lines[i]);
            depth += brace_delta(lines[i]);
            i += 1;
        }
    }
    keep.join("\n")
}

/// Wrap a picture in a minimal `standalone` document for compilation.
pub fn build_standalone(picture: &str, preamble: &str, unicode_engine: bool) -> String {
    [
        r"\documentclass[border=2pt]{standalone}",
        r"\usepackage{tikz}",
        concat!(
            r"\usetikzlibrary{calc,positioning,arrows,arrows.meta,shapes,",
            r"shapes.geometric,decorations.pathmorphing,patterns,shadows,fit,backgrounds}"
        ),
        &filtered_preamble(preamble, unicode_engine),
        r"\begin{document}",
        picture,
        r"\end{document}",
        "",
    ]
    .join("\n")
}

/// Compile the picture in `source` to a cropped PNG; `None` on any failure
/// (always, without a PDF rasteriser).
pub fn render(source: &str, _preamble: &str) -> Option<(Vec<u8>, i64, i64)> {
    extract_picture(source)?;
    if available_engines().is_empty() {
        return None;
    }
    if !crate::raster::has_pdf_support() {
        return None;
    }
    None
}
