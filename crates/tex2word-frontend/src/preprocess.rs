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

// \input{f}, \include{f}, and the plain-TeX \input f (ended by a space)
lazy_re!(
    INPUT_RE,
    r"\\(?:input|include)\s*\{([^}]+)\}|\\input[ \t]+(?![\\{])([^\s{}%\\]+)"
);
// subfiles: \subfile{f} / \subfileinclude{f} inline the subfile's document body
lazy_re!(SUBFILE_RE, r"\\subfile(?:include)?\s*\{([^}]+)\}");
lazy_re!(ENDINPUT_RE, r"\\endinput(?![A-Za-z@])");
// import package: \import{dir/}{file}, \subimport{dir/}{file} (and *from variants)
lazy_re!(
    IMPORT_RE,
    r"\\(?:sub)?(?:import|includefrom|inputfrom)\s*\{([^}]+)\}\s*\{([^}]+)\}"
);
// booktabs \cmidrule(lr){2-3} trim spec -- drop the (lr)/(l)/(r)
lazy_re!(CMIDRULE_TRIM_RE, r"(\\cmidrule)\s*\([lr]*\)");

/// environments whose body is verbatim: a `%` there is text, not a comment
const VERBATIM_ENVS: &[&str] = &[
    "verbatim",
    "verbatim*",
    "Verbatim",
    "Verbatim*",
    "BVerbatim",
    "LVerbatim",
    "lstlisting",
    "minted",
    "alltt",
    "filecontents",
    "filecontents*",
    "comment",
    "spverbatim",
    "boxedverbatim",
];

static COMMENT_SCAN_RE: LazyLock<Regex> = LazyLock::new(|| {
    let envs: Vec<String> = VERBATIM_ENVS.iter().map(|e| re::escape(e)).collect();
    Regex::new(&format!(
        concat!(
            r"\\begin\s*\{{(?P<env>{})\}}",
            r"|\\(?P<inline>verb\*?|lstinline|url|path|nolinkurl)(?![A-Za-z])",
            r"|\\[\\%]", // \\ and \% are not comment starts
            r"|%"
        ),
        envs.join("|")
    ))
});

/// Remove LaTeX line comments, preserving escaped `\%` and the bodies of
/// verbatim environments, `\verb`/`\lstinline` and `\url`/`\path`
/// arguments, where `%` is literal text.
pub fn strip_comments(source: &str) -> String {
    if !source.contains('%') {
        return source.to_string();
    }
    let mut out = String::with_capacity(source.len());
    let mut pos = 0;
    let n = source.len();
    loop {
        let Some(m) = COMMENT_SCAN_RE.search_from(source, pos) else {
            out.push_str(&source[pos..]);
            break;
        };
        let start = m.start();
        if let Some(env) = m.name("env").filter(|e| !e.is_empty()) {
            let end_tag = format!("\\end{{{env}}}");
            let stop = match source[m.end()..].find(&end_tag) {
                Some(k) => m.end() + k + end_tag.len(),
                None => n,
            };
            out.push_str(&source[pos..stop]);
            pos = stop;
        } else if let Some(cmd) = m.name("inline").filter(|c| !c.is_empty()) {
            let stop = inline_verbatim_end(source, m.end(), cmd);
            out.push_str(&source[pos..stop]);
            pos = stop;
        } else if m.as_str() == "%" {
            out.push_str(&source[pos..start]);
            let Some(k) = source[start..].find('\n') else {
                break;
            };
            // a comment eats its newline and the next line's indentation
            pos = start + k + 1;
            while pos < n && matches!(source.as_bytes()[pos], b' ' | b'\t') {
                pos += 1;
            }
        } else {
            // \\ or \%
            out.push_str(&source[pos..m.end()]);
            pos = m.end();
        }
    }
    out
}

/// End index of the argument of `\verb|..|`, `\lstinline[..]{..}`,
/// `\url{..}` & co. that starts at `i` (`i` itself if there is none).
fn inline_verbatim_end(s: &str, mut i: usize, cmd: &str) -> usize {
    let n = s.len();
    let b = s.as_bytes();
    if cmd == "lstinline" && i < n && b[i] == b'[' {
        i = match s[i..].find(']') {
            Some(k) => i + k + 1,
            None => n,
        };
    }
    if i >= n {
        return n;
    }
    if b[i] == b'{' && cmd != "verb" && cmd != "verb*" {
        let mut depth = 0i64;
        for (j, &c) in b.iter().enumerate().skip(i) {
            match c {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return j + 1;
                    }
                }
                // never let a broken \url swallow the document
                b'\n' if matches!(cmd, "url" | "path" | "nolinkurl") => return j,
                _ => {}
            }
        }
        return n;
    }
    let delim = s[i..].chars().next().unwrap();
    if pystr::is_space(delim) || pystr::is_alpha(delim) {
        return i;
    }
    let after = i + delim.len_utf8();
    let close = s[after..].find(delim).map(|k| after + k);
    let line_end = s[after..].find('\n').map(|k| after + k);
    match close {
        // not a well-formed inline verbatim: treat normally
        None => i,
        Some(c) if line_end.is_some_and(|l| l < c) => i,
        Some(c) => c + delim.len_utf8(),
    }
}

/// Inline `\input`/`\include` (and import-package `\import`) files.
///
/// As in TeX, a nested `\input` resolves against the main document's
/// directory first and only then against the including file's directory
/// (`gen/outer.tex` doing `\input{gen/inner}` means `gen/inner.tex`).
/// Inside an `\import`-ed file, the import directory comes first.
pub fn flatten_inputs(source: &str, base_dir: &str) -> io::Result<String> {
    flatten_inputs_depth(source, base_dir, 0, &[])
}

fn flatten_inputs_depth(
    source: &str,
    base_dir: &str,
    depth: usize,
    dirs: &[String],
) -> io::Result<String> {
    if depth > 20 {
        return Ok(source.to_string());
    }
    let dirs: Vec<String> = if dirs.is_empty() {
        vec![base_dir.to_string()]
    } else {
        dirs.to_vec()
    };
    let inline = |path: &str, nested: Vec<String>, body_only: bool| -> io::Result<String> {
        let mut inner = strip_comments(&control_chars(&pyos::read_text(path, Errors::Strict)?));
        if body_only {
            // a subfile: only what is between \begin/\end{document}
            if let Some(b) = inner.find("\\begin{document}") {
                let e = inner.rfind("\\end{document}");
                let from = b + "\\begin{document}".len();
                let to = match e {
                    Some(e) if e > b => e,
                    _ => inner.len(),
                };
                inner = inner[from..to.max(from)].to_string();
            }
        }
        if let Some(end) = ENDINPUT_RE.search(&inner) {
            // \endinput: TeX stops reading this file after the current line
            let (es, ee) = (end.start(), end.end());
            let rest = match inner[ee..].find('\n') {
                Some(k) => inner[ee..ee + k].to_string(),
                None => String::new(),
            };
            inner = format!("{}{}", &inner[..es], rest);
        }
        flatten_inputs_depth(&inner, base_dir, depth + 1, &dedupe(&nested))
    };
    let find = |name: &str, search: &[String]| -> Option<String> {
        let candidates = if name.ends_with(".tex") {
            vec![name.to_string()]
        } else {
            vec![name.to_string(), format!("{name}.tex")]
        };
        for directory in search {
            for cand in &candidates {
                let path = pyos::join2(directory, cand);
                if pyos::isfile(&path) {
                    return Some(path);
                }
            }
        }
        None
    };
    // \protect\input{f} must not glue f's first word onto \protect
    let glue = |src: &str, start: usize, text: String| -> String {
        let head = &src[..start];
        let mut from = start;
        for (k, (idx, _)) in head.char_indices().rev().enumerate() {
            if k == 64 {
                break;
            }
            from = idx;
        }
        if CONTROL_WORD_END_RE.search(&head[from..]).is_some() {
            format!(" {text}")
        } else {
            text
        }
    };
    let mut err: Option<io::Error> = None;
    let source = IMPORT_RE.sub_fn(source, |m| {
        // \import{dir/}{file}: the file lives under dir, relative to the
        // current file; its own \input{}s look in dir first
        let directory = pystr::strip(m.group_str(1));
        let name = pystr::strip(m.group_str(2));
        let search: Vec<String> = dirs.iter().map(|d| pyos::join2(d, directory)).collect();
        let Some(path) = find(name, &search) else {
            return String::new();
        };
        let mut nested = vec![pyos::dirname(&path)];
        nested.extend(dirs.iter().cloned());
        match inline(&path, nested, false) {
            Ok(text) => glue(source, m.start(), text),
            Err(e) => {
                err.get_or_insert(e);
                String::new()
            }
        }
    });
    let source = SUBFILE_RE.sub_fn(&source, |m| {
        // a subfile's own paths are relative to its directory (subfiles package)
        let Some(path) = find(pystr::strip(m.group_str(1)), &dirs) else {
            return String::new();
        };
        let nested = vec![pyos::dirname(&path), dirs[0].clone()];
        match inline(&path, nested, true) {
            Ok(text) => glue(&source, m.start(), text),
            Err(e) => {
                err.get_or_insert(e);
                String::new()
            }
        }
    });
    let out = INPUT_RE.sub_fn(&source, |m| {
        let raw = match m.group(1) {
            Some(g) if !g.is_empty() => g,
            _ => m.group_str(2),
        };
        let Some(path) = find(pystr::strip(raw), &dirs) else {
            return String::new(); // missing include -> drop (graceful degradation)
        };
        let nested = vec![dirs[0].clone(), pyos::dirname(&path)];
        match inline(&path, nested, false) {
            Ok(text) => glue(&source, m.start(), text),
            Err(e) => {
                err.get_or_insert(e);
                String::new()
            }
        }
    });
    match err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

lazy_re!(CONTROL_WORD_END_RE, r"\\[A-Za-z@]+$");

fn dedupe(dirs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in dirs {
        let norm = pyos::normpath(if d.is_empty() { "." } else { d });
        if !out.contains(&norm) {
            out.push(norm);
        }
    }
    out
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
    concat!(
        r"\\(?:lstinputlisting|verbatiminput)\s*(?:\[(?P<opts>(?:[^\[\]{}]|\{[^{}]*\})*)\])?",
        r"\s*\{([^}]+)\}"
    )
);

/// `key=value` pairs of a listings option list (top-level commas only).
fn split_options(opts: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut depth = 0i64;
    let mut buf = String::new();
    let mut parts: Vec<String> = Vec::new();
    for c in opts.chars() {
        depth += match c {
            '{' | '[' => 1,
            '}' | ']' => -1,
            _ => 0,
        };
        if c == ',' && depth == 0 {
            parts.push(std::mem::take(&mut buf));
        } else {
            buf.push(c);
        }
    }
    parts.push(buf);
    for part in parts {
        if let Some((key, value)) = part.split_once('=') {
            let mut value = pystr::strip(value);
            if value.starts_with('{') && value.ends_with('}') && value.len() >= 2 {
                value = pystr::strip(&value[1..value.len() - 1]);
            }
            let key = pystr::lower(pystr::strip(key));
            match out.iter_mut().find(|(k, _)| *k == key) {
                Some(slot) => slot.1 = value.to_string(),
                None => out.push((key, value.to_string())),
            }
        }
    }
    out
}

/// `\twlistingcaption{label}{caption}{1|0}` for a listing's
/// `caption=`/`title=`/`label=` options (empty if it has none).
fn listing_marker(opts: Option<&str>) -> String {
    let Some(opts) = opts.filter(|o| !o.is_empty()) else {
        return String::new();
    };
    let kv = split_options(opts);
    let get = |k: &str| kv.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    let (mut caption, mut numbered) = (get("caption"), "1");
    if caption.is_none() {
        if let Some(title) = get("title") {
            caption = Some(title);
            numbered = "0";
        }
    }
    if let Some(c) = caption.as_ref().filter(|c| c.starts_with('[')) {
        // caption={[short]long}
        caption = Some(match c.find(']') {
            Some(close) => pystr::strip(&c[close + 1..]).to_string(),
            None => c.clone(),
        });
    }
    let label = get("label").unwrap_or_default();
    if caption.as_deref().unwrap_or("").is_empty() && label.is_empty() {
        return String::new();
    }
    format!(
        "\\twlistingcaption{{{label}}}{{{}}}{{{numbered}}}",
        caption.unwrap_or_default()
    )
}

/// Replace `\lstinputlisting{file}`/`\verbatiminput{file}` by the file body
/// in a `verbatim` environment.
pub fn inline_listing_files(source: &str, base_dir: &str) -> io::Result<String> {
    let mut err = None;
    let out = LSTINPUT_RE.sub_fn(source, |m| {
        let path = pyos::join2(base_dir, pystr::strip(m.group_str(2)));
        if pyos::isfile(&path) {
            match pyos::read_text(&path, Errors::Strict) {
                Ok(text) => {
                    return format!(
                        "{}\\begin{{verbatim}}\n{}\n\\end{{verbatim}}",
                        listing_marker(m.name("opts")),
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
        r"[ \t]*(?:\[(?P<opts>(?:[^\[\]{}]|\{(?:[^{}]|\{[^{}]*\})*\}|\[[^\]]*\])*)\])?",
        r"[ \t]*(?:\{[^}]*\})?",
        r"(?P<body>.*?)",
        r"\\end\{\1\}"
    ),
    Flags::S
);

/// Rewrite `lstlisting`/`minted`/`Verbatim` blocks to `verbatim`.
pub fn normalize_listing_envs(source: &str) -> String {
    // lstlisting's caption=/label= become a marker the parser attaches to the
    // code block (a numbered, referenceable "Listing N")
    LISTING_ENV_RE.sub_fn(source, |m| {
        let marker = if m.group_str(1) == "lstlisting" {
            listing_marker(m.name("opts"))
        } else {
            String::new()
        };
        format!(
            "{marker}\\begin{{verbatim}}{}\\end{{verbatim}}",
            m.name("body").unwrap_or("")
        )
    })
}

// beamer overlay specifications: \item<2->, \alert<1>{..}, \only<2-3>{..}, ...
lazy_re!(
    OVERLAY_RE,
    concat!(
        r"(\\(?:item|only|uncover|visible|invisible|onslide|alt|temporal|alert|textbf|",
        r"textit|emph|color|textcolor|includegraphics|pause|structure|frame|column|",
        r"hyperlink|action)\*?)\s*<[^<>\n]*>"
    )
);
// overlay-only commands: \pause, \onslide<2-> (no argument) carry no content
lazy_re!(
    OVERLAY_ONLY_RE,
    r"\\(?:pause|onslide)(?![A-Za-z])(?:\s*\[[^\]]*\])?"
);

/// Remove beamer overlay specifications (the slides are flattened).
fn strip_overlays(source: &str) -> String {
    OVERLAY_ONLY_RE.sub("", &OVERLAY_RE.sub(r"\1", source))
}

/// LaTeX reads a form feed (^^L) as `\par`; other C0 control characters
/// have no output (and cannot be written to XML).
fn control_chars(source: &str) -> String {
    source
        .replace('\x0c', "\n\n")
        .chars()
        .filter(|&c| !matches!(c, '\x00'..='\x08' | '\x0b' | '\x0e'..='\x1f' | '\x7f'))
        .collect()
}

lazy_re!(
    BEAMER_CLASS_RE,
    r"\\documentclass\s*(?:\[[^\]]*\])?\s*\{beamer\}"
);

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
    let mut source = flatten_inputs(&strip_comments(&control_chars(source)), base_dir)?;
    if BEAMER_CLASS_RE.search(&source).is_some() {
        source = strip_overlays(&source); // \item<2-> \only<1>{..} -> no overlay spec
    }
    if uses_exam_class(&source, base_dir) {
        source = rewrite_exam_class(&source, base_dir);
        source = inject_exam_title(&source);
    }
    source = rewrite_halign(&source);
    source = TIKZSTYLE_RE.sub("", &source);
    source = recover_tikz_boxes(&source);
    source = normalize_begin_star(&source);
    source = crate::conditionals::resolve_conditionals(&source, base_dir);
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
    fn nested_inputs_resolve_like_tex() {
        let dir = std::env::temp_dir().join(format!("t2w-flatten-{}", std::process::id()));
        let gen = dir.join("generated");
        std::fs::create_dir_all(&gen).unwrap();
        // generated/outer.tex inputs generated/inner (relative to the main
        // file, as TeX does) and a sibling by its bare name (fallback)
        std::fs::write(
            gen.join("outer.tex"),
            "A \\input{generated/inner} \\input{sibling}",
        )
        .unwrap();
        std::fs::write(gen.join("inner.tex"), "B").unwrap();
        std::fs::write(gen.join("sibling.tex"), "C").unwrap();
        std::fs::write(gen.join("cap.tex"), "Quality").unwrap();
        let base = dir.to_str().unwrap();
        let out = flatten_inputs(r"\input{generated/outer}", base).unwrap();
        assert_eq!(out, "A B C");
        // \protect\input{x} must not glue into \protectQuality
        let out = flatten_inputs(r"\caption{\protect\input{generated/cap}}", base).unwrap();
        assert_eq!(out, r"\caption{\protect Quality}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spacing_primitives_are_dropped() {
        assert_eq!(
            normalize_spacing_and_icons(r"a\vskip -1mm b \faEnvelope c"),
            "a b ✉ c"
        );
    }
}
