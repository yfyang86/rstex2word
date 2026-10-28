//! Render TikZ/PGF pictures by compiling them with a TeX engine (port of
//! `backend/tikz.py`, extended): each picture of a figure is wrapped in a
//! `standalone` document, compiled with the engine that suits the document,
//! and the cropped PDF is rasterised by `raster` (poppler's `pdftoppm`).
//! Without an engine or a rasteriser, `render*` return `None` (before
//! compiling, as the reference does) and the writer explains why.

use std::sync::LazyLock;

use tex2word_ir::re::Regex;

/// xelatex/lualatex first: they honour fontspec/xeCJK for CJK node text.
const ENGINES: &[&str] = &["xelatex", "lualatex", "pdflatex"];

/// drawing environments we try to compile
pub const DRAWING_ENVS: &[&str] = &[
    "tikzpicture",
    "pgfpicture",
    "circuitikz",
    "tikzcd",
    "forest",
    "pspicture",
];

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
/// Text-font/encoding packages: kept for pdflatex so node text uses the
/// paper's font (and so label widths match the original layout).
static KEEP_PDF_FONT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^\s*\\usepackage\b[^\n]*\{[^}]*\b",
        r"(times|mathptmx|newtxtext|newtxmath|txfonts|helvet|courier|palatino|mathpazo|",
        r"lmodern|libertine|libertinus|charter|bookman|avant|fontenc|inputenc)\b"
    ))
});
static KEEP_FONT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*\\(usepackage\b[^\n]*\{(fontspec|xeCJK)\}|set(main|CJK\w*)font\b)")
});
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
static PICTURE_BEGIN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"\\begin\{{({})\}}", DRAWING_ENVS.join("|"))));
static LOCAL_DEF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*\\(definecolor|colorlet|tikzset|pgfplotsset|tikzstyle|usetikzlibrary)\b")
});

/// `shutil.which(name)`: an executable file of that name on `PATH`.
pub fn which(name: &str) -> Option<String> {
    if std::env::var_os("TEX2WORD_NO_EXTERNAL_TOOLS").is_some() {
        return None;
    }
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
    ENGINES
        .iter()
        .copied()
        .filter(|e| which(e).is_some())
        .collect()
}

/// Pull the first `tikzpicture` (etc.) out of a figure's LaTeX source.
pub fn extract_picture(source: &str) -> Option<String> {
    extract_pictures(source).into_iter().next()
}

/// Every top-level drawing environment in a figure's source, in order.
///
/// A figure may stack several pictures (e.g. two `\input` TikZ files in one
/// `figure*`); each is compiled on its own. Nesting of the same environment
/// (a `tikzpicture` inside a node) is balanced, so the outer picture stays
/// whole.
pub fn extract_pictures(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(m) = PICTURE_BEGIN_RE.search_from(source, pos) {
        let env = m.group_str(1);
        let begin = format!("\\begin{{{env}}}");
        let end = format!("\\end{{{env}}}");
        let mut depth = 0usize;
        let mut i = m.start();
        let mut close = None;
        while i < source.len() {
            let rest = &source[i..];
            if rest.starts_with(&begin) {
                depth += 1;
                i += begin.len();
            } else if rest.starts_with(&end) {
                depth -= 1;
                i += end.len();
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            } else {
                i += rest.chars().next().map_or(1, char::len_utf8);
            }
        }
        match close {
            Some(stop) => {
                out.push(source[m.start()..stop].to_string());
                pos = stop;
            }
            None => break, // unbalanced: nothing compilable from here on
        }
    }
    out
}

/// Figure-local style/colour definitions outside the pictures (e.g. a
/// `\definecolor` just before an `\input` TikZ file) that the pictures need.
fn figure_definitions(source: &str) -> String {
    let mut rest = source.to_string();
    for pic in extract_pictures(source) {
        rest = rest.replacen(&pic, "\n", 1);
    }
    let lines = splitlines(&rest);
    let mut keep: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !LOCAL_DEF.is_match(lines[i]) {
            i += 1;
            continue;
        }
        keep.push(lines[i]);
        let mut depth = brace_delta(lines[i]);
        i += 1;
        while depth > 0 && i < lines.len() {
            keep.push(lines[i]);
            depth += brace_delta(lines[i]);
            i += 1;
        }
    }
    keep.join("\n")
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
        if matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
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
            || (unicode_engine && KEEP_FONT.is_match(line))
            || (!unicode_engine && KEEP_PDF_FONT.is_match(line));
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

/// Compile the first picture in `source` to a cropped PNG: `(png bytes, w, h)`.
pub fn render(source: &str, preamble: &str) -> Option<(Vec<u8>, i64, i64)> {
    let picture = extract_picture(source)?;
    render_picture(&picture, source, preamble, ".")
}

static UNICODE_PREAMBLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\usepackage\b[^\n]*\{[^}]*\b(fontspec|xeCJK|unicode-math|polyglossia)\b")
});

/// Engines to try for this document, best first: a document written for
/// pdflatex (no fontspec & co., ASCII picture) compiles with pdflatex so its
/// text fonts and metrics match the paper; Unicode documents prefer
/// xelatex/lualatex. Later engines are fallbacks when a compile fails.
pub fn engine_order(picture: &str, preamble: &str) -> Vec<&'static str> {
    let mut engines = available_engines();
    let unicode = UNICODE_PREAMBLE.is_match(preamble) || !picture.is_ascii();
    if !unicode {
        if let Some(i) = engines.iter().position(|e| *e == "pdflatex") {
            let e = engines.remove(i);
            engines.insert(0, e);
        }
    }
    engines
}

/// Compile one `picture` (taken from the figure `source`) to a cropped PNG.
///
/// Writes a `standalone` document into a private temp dir, runs the TeX
/// engines from `engine_order` until one succeeds (with `base_dir` on
/// `TEXINPUTS`, so pictures can `\input` sibling files), then rasterises the
/// cropped PDF (`raster`). `None` on any failure — the writer reports the
/// reason. Set `TEX2WORD_TIKZ_DEBUG=1` to keep the compile directory and print
/// the engine log to stderr.
pub fn render_picture(
    picture: &str,
    source: &str,
    preamble: &str,
    base_dir: &str,
) -> Option<(Vec<u8>, i64, i64)> {
    if !crate::raster::has_pdf_support() {
        return None;
    }
    let local = figure_definitions(source);
    let preamble = if local.is_empty() {
        preamble.to_string()
    } else {
        format!("{preamble}\n{local}")
    };
    let debug = std::env::var("TEX2WORD_TIKZ_DEBUG").as_deref() == Ok("1");
    let texinputs = {
        let base = std::fs::canonicalize(base_dir)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| base_dir.to_string());
        let sep = if cfg!(windows) { ";" } else { ":" };
        // the trailing empty entry keeps the engine's default search path
        format!("{base}{sep}")
    };
    for engine in engine_order(picture, &preamble) {
        let doc = build_standalone(picture, &preamble, engine != "pdflatex");
        // unique per call: several converts (or tests) may run in one process
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("tex2word-tikz-{}-{}", std::process::id(), seq));
        std::fs::create_dir_all(&dir).ok()?;
        let tex = dir.join("figure.tex");
        std::fs::write(&tex, doc).ok()?;
        let mut cmd = std::process::Command::new(engine);
        cmd.arg("-interaction=nonstopmode")
            .arg("-halt-on-error")
            .arg("-output-directory")
            .arg(&dir)
            .arg(&tex)
            .env("TEXINPUTS", &texinputs);
        if !debug {
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
        }
        let ok = cmd.status().map(|s| s.success()).unwrap_or(false);
        let out = if ok {
            let pdf = dir.join("figure.pdf");
            crate::raster::rasterize_pdf(&pdf.display().to_string(), crate::raster::DEFAULT_DPI)
        } else {
            if debug {
                eprintln!("tex2word: {engine} failed on {}", tex.display());
                if let Ok(log) = std::fs::read_to_string(dir.join("figure.log")) {
                    eprint!("{log}");
                }
            }
            None
        };
        if !debug {
            let _ = std::fs::remove_dir_all(&dir);
        }
        if out.is_some() {
            return out;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_every_picture_in_order() {
        let src = concat!(
            r"\centering\begin{tikzpicture}\node{a};\end{tikzpicture}\par",
            r"\begin{tikzpicture}\node{\begin{tikzpicture}\node{in};\end{tikzpicture}};",
            r"\end{tikzpicture}\caption{x}"
        );
        let pics = extract_pictures(src);
        assert_eq!(pics.len(), 2);
        assert_eq!(pics[0], r"\begin{tikzpicture}\node{a};\end{tikzpicture}");
        // the nested picture stays inside the outer one
        assert!(pics[1].ends_with(r"\end{tikzpicture}};\end{tikzpicture}"));
        assert_eq!(extract_picture(src).as_deref(), Some(pics[0].as_str()));
        assert!(extract_pictures(r"\begin{tikzpicture} unclosed").is_empty());
    }

    #[test]
    fn keeps_figure_local_definitions() {
        let src = concat!(
            "\\centering\n\\definecolor{bar}{RGB}{1,2,3}\n",
            "\\begin{tikzpicture}\n\\definecolor{inner}{RGB}{4,5,6}\n\\end{tikzpicture}\n"
        );
        // the picture's own definitions travel with the picture
        assert_eq!(figure_definitions(src), r"\definecolor{bar}{RGB}{1,2,3}");
    }

    #[test]
    fn text_fonts_follow_the_engine() {
        let pre = "\\usepackage{times}\n\\usepackage{fontspec}\n\\usepackage{hyperref}";
        let pdf = filtered_preamble(pre, false);
        assert!(pdf.contains("{times}") && !pdf.contains("fontspec"));
        let uni = filtered_preamble(pre, true);
        assert!(uni.contains("fontspec") && !uni.contains("{times}"));
        assert!(!pdf.contains("hyperref") && !uni.contains("hyperref"));
    }

    #[test]
    fn pdflatex_documents_prefer_pdflatex() {
        let order = engine_order("\\begin{tikzpicture}\\end{tikzpicture}", "");
        if order.contains(&"pdflatex") {
            assert_eq!(order[0], "pdflatex");
        }
        let order = engine_order("x", "\\usepackage{fontspec}");
        if let Some(first) = order.first() {
            assert_ne!(*first, "pdflatex", "{order:?}");
        }
    }
}
