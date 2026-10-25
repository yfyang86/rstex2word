//! Expansion of user-defined `\newcommand`/`\def`/`\NewDocumentCommand`
//! (port of `frontend/macros.py`).
//!
//! This is a deliberately bounded expander -- *not* a TeX engine. It handles
//! the common forms: fixed arity with `#1..#9` parameters and one optional
//! argument (`\newcommand`/`\def`), plus the xparse argspec grammar
//! (`\NewDocumentCommand` with `s`/`m`/`o`/`O{}`/`r`/`d` arguments and the
//! `\IfValueTF`/`\IfNoValueTF`/`\IfBooleanTF` conditionals). Arbitrary
//! macro/catcode trickery remains out of scope.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use tex2word_ir::pyos::{self, Errors};
use tex2word_ir::pystr;
use tex2word_ir::re::{self, Flags, Regex};

use crate::preprocess::{rewrite_mathoperators, strip_comments};

macro_rules! lazy_re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat));
    };
    ($name:ident, $pat:expr, $flags:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::with_flags($pat, $flags));
    };
}

lazy_re!(
    NEWCOMMAND_RE,
    concat!(
        r"\\(?:newcommand|renewcommand|providecommand|DeclareRobustCommand",
        r"|DeclareTextFontCommand)\s*\*?\s*",
        r"\{?\\(?P<name>[A-Za-z]+)\}?",
        r"(?:\[(?P<nargs>\d+)\])?",
        r"(?:\[(?P<default>[^\]]*)\])?"
    )
);
lazy_re!(DEF_RE, r"\\def\s*\\(?P<name>[A-Za-z]+)\s*(?P<params>(?:#\d)*)\s*\{");
// xparse: \NewDocumentCommand{\name}{argspec}{body}
lazy_re!(
    XPARSE_RE,
    r"\\(?:New|Renew|Provide|Declare)DocumentCommand\s*\{?\s*\\(?P<name>[A-Za-z]+)\}?\s*"
);

// sentinels for xparse argument conditionals (chosen not to appear in source)
const NOVALUE: &str = "\x00NoValue\x00";
const BOOL_T: &str = "\x00BoolTrue\x00";
const BOOL_F: &str = "\x00BoolFalse\x00";

/// One xparse argument: its kind (`s`, `m`, `o`, `O`) and default.
pub type ArgSpec = (char, Option<String>);

/// A user macro definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Macro {
    pub name: String,
    pub nargs: i64,
    pub default: Option<String>,
    pub body: String,
    /// xparse argument spec; `None` for `\newcommand`/`\def`.
    pub argspec: Option<Vec<ArgSpec>>,
}

/// A macro table (name -> definition).
pub type Macros = HashMap<String, Macro>;

fn char_at(s: &str, i: usize) -> Option<char> {
    s.get(i..).and_then(|t| t.chars().next())
}

fn skip_blanks(text: &str, mut i: usize) -> usize {
    let b = text.as_bytes();
    while i < b.len() && matches!(b[i], b' ' | b'\n' | b'\t') {
        i += 1;
    }
    i
}

/// Read a `{...}` group starting at `text[start] == '{'` (a backslash escapes
/// the next character); return the body and the index after the group.
pub fn read_balanced(text: &str, start: usize) -> (&str, usize) {
    debug_assert_eq!(text.as_bytes()[start], b'{');
    // Byte scanning is equivalent to Python's code-point scan here: after a
    // backslash skips one byte of a multi-byte character, the remaining
    // continuation bytes never match `{`, `}` or `\`.
    let b = text.as_bytes();
    let mut depth = 0i64;
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                i += 2;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return (&text[start + 1..i], i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    (&text[start + 1..], text.len())
}

/// Find macro definitions, returning the table and the source with the
/// definitions removed.
pub fn collect_macros(source: &str) -> (Macros, String) {
    let mut macros = Macros::new();
    let mut out = String::with_capacity(source.len());
    let n = source.len();
    let mut i = 0;
    while i < n {
        if let Some(x) = XPARSE_RE.match_at(source, i) {
            if pystr::lstrip(&source[x.end()..]).starts_with('{') {
                let spec_start = x.end() + source[x.end()..].find('{').unwrap();
                let (argspec_str, pos) = read_balanced(source, spec_start);
                if pos < n && pystr::lstrip(&source[pos..]).starts_with('{') {
                    let body_start = pos + source[pos..].find('{').unwrap();
                    let (body, end) = read_balanced(source, body_start);
                    let name = x.name("name").unwrap().to_string();
                    macros.insert(
                        name.clone(),
                        Macro {
                            name,
                            nargs: 0,
                            default: None,
                            body: body.to_string(),
                            argspec: Some(parse_argspec(argspec_str)),
                        },
                    );
                    i = end;
                    continue;
                }
            }
        }
        let m = NEWCOMMAND_RE.match_at(source, i);
        if let Some(m) = &m {
            if pystr::lstrip(&source[m.end()..]).starts_with('{') {
                let brace_start = m.end() + source[m.end()..].find('{').unwrap();
                let (body, end) = read_balanced(source, brace_start);
                let nargs = m.name("nargs").map_or(0, |s| pystr::parse_int(s).unwrap_or(0));
                let name = m.name("name").unwrap().to_string();
                macros.insert(
                    name.clone(),
                    Macro {
                        name,
                        nargs,
                        default: m.name("default").map(str::to_string),
                        body: body.to_string(),
                        argspec: None,
                    },
                );
                i = end;
                continue;
            }
            if m.name("nargs").is_none() {
                // Unbraced single-token body: `\newcommand\BE\textbf` (a common
                // preamble idiom -- \CAL\mathcal, \BB\mathbb, …). The definition
                // is the next single control sequence or character.
                let (body, end) = read_arg(source, m.end());
                if !body.is_empty() {
                    let name = m.name("name").unwrap().to_string();
                    macros.insert(
                        name.clone(),
                        Macro {
                            name,
                            nargs: 0,
                            default: None,
                            body: body.to_string(),
                            argspec: None,
                        },
                    );
                    i = end;
                    continue;
                }
            }
        }
        if let Some(d) = DEF_RE.match_at(source, i) {
            let brace_start = d.end() - 1;
            let (body, end) = read_balanced(source, brace_start);
            let nargs = re::findall(r"#\d", d.name("params").unwrap_or("")).len() as i64;
            let name = d.name("name").unwrap().to_string();
            macros.insert(
                name.clone(),
                Macro {
                    name,
                    nargs,
                    default: None,
                    body: body.to_string(),
                    argspec: None,
                },
            );
            i = end;
            continue;
        }
        let c = char_at(source, i).unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    (macros, out)
}

/// Parse an xparse argument specification into `[(kind, default)]`.
pub fn parse_argspec(spec: &str) -> Vec<ArgSpec> {
    let mut out = Vec::new();
    let n = spec.len();
    let mut i = 0;
    while i < n {
        let c = char_at(spec, i).unwrap();
        let next_is_brace = spec.as_bytes().get(i + c.len_utf8()) == Some(&b'{');
        match c {
            // spaces, long (+), required-token (!): no extra arg
            ' ' | '+' | '!' => i += 1,
            // >{processor} -- ignore the processor group
            '>' => {
                if next_is_brace {
                    i = read_balanced(spec, i + 1).1;
                } else {
                    i += 1;
                }
            }
            's' | 'm' | 'o' => {
                out.push((c, None));
                i += 1;
            }
            // O{default}
            'O' => {
                if next_is_brace {
                    let (default, j) = read_balanced(spec, i + 1);
                    out.push(('O', Some(default.to_string())));
                    i = j;
                } else {
                    out.push(('o', None));
                    i += 1;
                }
            }
            // required delimited r<d1><d2> -> treat as mandatory
            'r' | 'R' => {
                out.push(('m', None));
                i = advance_chars(spec, i, 3);
            }
            // optional delimited d<d1><d2> -> treat as optional
            'd' | 'D' => {
                out.push(('o', None));
                i = advance_chars(spec, i, 3);
            }
            _ => i += c.len_utf8(),
        }
    }
    out
}

/// `i + k` in code points (clamped past the end like a Python index).
fn advance_chars(s: &str, i: usize, k: usize) -> usize {
    match s[i..].char_indices().nth(k) {
        Some((off, _)) => i + off,
        None => s.len(),
    }
}

fn read_xparse_args(argspec: &[ArgSpec], text: &str, mut i: usize) -> (Vec<String>, usize) {
    let mut values = Vec::with_capacity(argspec.len());
    for (kind, default) in argspec {
        i = skip_blanks(text, i);
        match kind {
            's' => {
                if text.as_bytes().get(i) == Some(&b'*') {
                    values.push(BOOL_T.to_string());
                    i += 1;
                } else {
                    values.push(BOOL_F.to_string());
                }
            }
            'o' => {
                let (opt, j) = read_optional(text, i);
                i = j;
                values.push(opt.unwrap_or(NOVALUE).to_string());
            }
            'O' => {
                let (opt, j) = read_optional(text, i);
                i = j;
                values.push(opt.unwrap_or(default.as_deref().unwrap_or("")).to_string());
            }
            // 'm' (mandatory)
            _ => {
                let (arg, j) = read_arg(text, i);
                i = j;
                values.push(arg.to_string());
            }
        }
    }
    (values, i)
}

/// The xparse conditionals: number of branch groups, and the branch chosen
/// for a condition value (`-1` = none).
fn cond(name: &str) -> (usize, fn(&str) -> i64) {
    match name {
        "IfValueTF" => (2, |v| if v != NOVALUE { 0 } else { 1 }),
        "IfValueT" => (1, |v| if v != NOVALUE { 0 } else { -1 }),
        "IfValueF" => (1, |v| if v == NOVALUE { 0 } else { -1 }),
        "IfNoValueTF" => (2, |v| if v == NOVALUE { 0 } else { 1 }),
        "IfNoValueT" => (1, |v| if v == NOVALUE { 0 } else { -1 }),
        "IfNoValueF" => (1, |v| if v != NOVALUE { 0 } else { -1 }),
        "IfBooleanTF" => (2, |v| if v == BOOL_T { 0 } else { 1 }),
        "IfBooleanT" => (1, |v| if v == BOOL_T { 0 } else { -1 }),
        "IfBooleanF" => (1, |v| if v != BOOL_T { 0 } else { -1 }),
        _ => unreachable!("{name}"),
    }
}

lazy_re!(
    COND_RE,
    concat!(
        r"\\(IfValueTF|IfValueT|IfValueF|IfNoValueTF|IfNoValueT|IfNoValueF",
        r"|IfBooleanTF|IfBooleanT|IfBooleanF)(?![A-Za-z])"
    )
);

/// Resolve the xparse `\IfValueTF` / `\IfBooleanTF` family in `text`.
fn resolve_conditionals(text: &str) -> String {
    let mut text = text.to_string();
    for _ in 0..=40 {
        let Some(m) = COND_RE.search(&text) else {
            break;
        };
        let (nbranches, predicate) = cond(m.group_str(1));
        let (cond_value, mut i) = read_arg(&text, m.end());
        let mut branches: Vec<&str> = Vec::new();
        for _ in 0..nbranches {
            i = skip_blanks(&text, i);
            let (br, j) = if text.as_bytes().get(i) == Some(&b'{') {
                read_balanced(&text, i)
            } else {
                read_arg(&text, i)
            };
            branches.push(br);
            i = j;
        }
        let choice = predicate(cond_value);
        let chosen = if choice >= 0 && (choice as usize) < branches.len() {
            branches[choice as usize]
        } else {
            ""
        };
        text = format!("{}{}{}", &text[..m.start()], chosen, &text[i..]);
    }
    text
}

/// Read one macro argument: a `{...}` group, a control word, or one char.
pub fn read_arg(text: &str, i: usize) -> (&str, usize) {
    let i = skip_blanks(text, i);
    match char_at(text, i) {
        Some('{') => read_balanced(text, i),
        Some('\\') => {
            let mut j = i + 1;
            while let Some(c) = char_at(text, j) {
                if !pystr::is_alpha(c) {
                    break;
                }
                j += c.len_utf8();
            }
            (&text[i..j], j)
        }
        Some(c) => (&text[i..i + c.len_utf8()], i + c.len_utf8()),
        None => ("", i),
    }
}

/// Read a `[...]` optional argument (nested brackets balance) at `i`.
pub fn read_optional(text: &str, i: usize) -> (Option<&str>, usize) {
    let b = text.as_bytes();
    if b.get(i) == Some(&b'[') {
        let mut depth = 0i64;
        for j in i..b.len() {
            if b[j] == b'[' {
                depth += 1;
            } else if b[j] == b']' {
                depth -= 1;
                if depth == 0 {
                    return (Some(&text[i + 1..j]), j + 1);
                }
            }
        }
    }
    (None, i)
}

/// Expand known macros in `source` (bounded recursion).
pub fn expand(source: &str, macros: &Macros) -> String {
    if macros.is_empty() {
        return source.to_string();
    }
    // longest names first so e.g. \EE wins over \E at the same position
    let mut keys: Vec<&str> = macros.keys().map(String::as_str).collect();
    keys.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    let alternation: Vec<String> = keys.iter().map(|k| re::escape(k)).collect();
    let pattern = Regex::new(&format!(r"\\({})(?![A-Za-z])", alternation.join("|")));

    let mut source = source.to_string();
    for _depth in 0..=32 {
        let mut out = String::with_capacity(source.len());
        let n = source.len();
        let mut i = 0;
        let mut changed = false;
        while i < n {
            let Some(m) = pattern.search_from(&source, i) else {
                out.push_str(&source[i..]);
                break;
            };
            out.push_str(&source[i..m.start()]); // copy the gap up to the macro in one go
            let mac = &macros[m.group_str(1)];
            let mut j = m.end();
            let body = if let Some(argspec) = &mac.argspec {
                let (args, k) = read_xparse_args(argspec, &source, j);
                j = k;
                let mut body = mac.body.clone();
                for (idx, arg) in args.iter().enumerate() {
                    body = body.replace(&format!("#{}", idx + 1), arg);
                }
                resolve_conditionals(&body)
            } else {
                let mut args: Vec<String> = Vec::new();
                if mac.nargs != 0 {
                    let remaining = if let Some(default) = &mac.default {
                        let (opt, k) = read_optional(&source, j);
                        j = k;
                        args.push(opt.unwrap_or(default).to_string());
                        mac.nargs - 1
                    } else {
                        mac.nargs
                    };
                    for _ in 0..remaining.max(0) {
                        let (arg, k) = read_arg(&source, j);
                        j = k;
                        args.push(arg.to_string());
                    }
                }
                let mut body = mac.body.clone();
                for (idx, arg) in args.iter().enumerate() {
                    body = body.replace(&format!("#{}", idx + 1), arg);
                }
                body
            };
            out.push_str(&body);
            i = j;
            changed = true;
        }
        source = out;
        if !changed {
            break;
        }
    }
    source
}

// ----------------------------------------------------------------------------
// Local package (.sty) macro loading
// ----------------------------------------------------------------------------
// Real papers split their custom commands into a local style file pulled in
// via `\usepackage{style}`. We read any `<name>.sty` next to the source
// (system packages won't be found and are skipped) and harvest its
// definitions -- including the `\@for ... \do{...}` generator idiom common in
// ML papers (`\cA..\cZ` calligraphic, `\bA..\bZ` blackboard, etc.).

lazy_re!(
    USEPKG_RE,
    r"\\(?:usepackage|RequirePackage)\s*(?:\[[^\]]*\])?\s*\{(?P<names>[^}]+)\}"
);
lazy_re!(
    FOR_RE,
    r"\\@for\s*\\(?P<var>[A-Za-z@]+)\s*:=\s*\\(?P<list>[A-Za-z@]+)\s*\\do\s*"
);
lazy_re!(CSNAME_RE, r"\\csname\s*(?P<body>.*?)\\endcsname", Flags::S);

// A package macro is only safe to expand statically if its body is free of
// TeX-engine machinery (conditionals, \csname/\expandafter gymnastics, counter
// and group manipulation, nested \def).
lazy_re!(
    UNSAFE_BODY_RE,
    concat!(
        r"\\(?:if[A-Za-z@]*|else|fi|or|csname|endcsname|expandafter|noexpand",
        r"|@for|@ifnextchar|@ifstar|loop|repeat|global|let|edef|xdef|gdef|def",
        r"|newcommand|renewcommand|providecommand|DeclareRobustCommand",
        r"|begingroup|endgroup|protect|stepcounter|addtocounter|setcounter",
        r"|newcounter|the|value|lowercase|uppercase|futurelet|aftergroup)",
        r"(?![A-Za-z])"
    )
);

// Structural / front-matter commands the parser handles natively -- a package
// is free to redefine them, but harvesting that definition would clobber the
// real heading with raw markup.
const PROTECTED_NAMES: &[&str] = &[
    "section", "subsection", "subsubsection", "paragraph", "subparagraph",
    "chapter", "part", "title", "author", "date", "thanks", "maketitle",
    "caption", "captionof", "label", "ref", "cref", "Cref", "autoref",
    "eqref", "pageref", "cite", "citep", "citet", "item", "footnote",
    "begin", "end", "abstract", "appendix", "bibliography", "tableofcontents",
    "includegraphics", "newpage", "clearpage", "input", "include", "today",
];

// A control sequence containing `@` is package-internal machinery.
lazy_re!(AT_COMMAND_RE, r"\\[A-Za-z]*@|@[A-Za-z]");

/// True if a harvested package macro is safe for the pure expander.
fn is_safe_macro(mac: &Macro) -> bool {
    if mac.name.contains('@') || PROTECTED_NAMES.contains(&mac.name.as_str()) {
        return false;
    }
    if UNSAFE_BODY_RE.is_match(&mac.body) || AT_COMMAND_RE.is_match(&mac.body) {
        return false;
    }
    // reject self-reference (direct recursion the expander can't terminate)
    let self_ref = re::compile(&format!(r"\\{}(?![A-Za-z])", re::escape(&mac.name)), Flags::NONE);
    !self_ref.is_match(&mac.body)
}

fn safe_only(macros: Macros) -> Macros {
    macros.into_iter().filter(|(_, m)| is_safe_macro(m)).collect()
}

/// Turn `\csname foo\endcsname` into `\foo` (dropping inner spaces).
fn resolve_csname(text: &str) -> String {
    CSNAME_RE.sub_fn(text, |m| format!("\\{}", pystr::strip(m.name("body").unwrap()).replace(' ', "")))
}

/// Unroll `\@for\i:=\List\do{ body }` and return *only* the bodies.
///
/// Everything outside the loops is discarded: the surrounding stylesheet can
/// contain TeX we don't emulate, and we only want the synthesised generator
/// calls. Errors like Python's `re.error` (an element that is a bad template
/// escape, e.g. `\mathcal`).
fn unroll_for_loops(source: &str, macros: &Macros) -> Result<String, re::Error> {
    let mut parts = String::new();
    for m in FOR_RE.find_iter(source) {
        let j = skip_blanks(source, m.end());
        let list_macro = macros.get(m.name("list").unwrap());
        if let (Some(b'{'), Some(list_macro)) = (source.as_bytes().get(j), list_macro) {
            let (body, _) = read_balanced(source, j);
            let elems: Vec<&str> = list_macro
                .body
                .split(',')
                .map(pystr::strip)
                .filter(|e| !e.is_empty())
                .collect();
            let var_re = re::compile(
                &format!(r"\\{}(?![A-Za-z])", re::escape(m.name("var").unwrap())),
                Flags::NONE,
            );
            for elem in elems {
                // brace the element so \expandafter\genCal\i -> \genCal{A}
                // (the \expandafter, dropped next, is what separates the tokens)
                parts.push_str(&var_re.try_sub(&format!("{{{elem}}}"), body)?);
            }
        }
    }
    Ok(parts)
}

/// Harvest macro definitions from a style-file source (best effort).
fn collect_package_macros(sty_source: &str) -> Macros {
    // \DeclareMathOperator in a package becomes a \newcommand wrapping
    // \operatorname, so the operator names defined there are harvested too.
    let sty_source = rewrite_mathoperators(&strip_comments(sty_source));
    let (macros, _) = collect_macros(&sty_source);
    // Unroll generator loops, run the (terminating) generator macros, and
    // resolve \csname so the synthesised \newcommand definitions are collectable.
    // (A stylesheet quirk never aborts the build.)
    if let Ok(generated) = unroll_for_loops(&strip_comments(&sty_source), &macros) {
        if !pystr::strip(&generated).is_empty() {
            let generated = generated.replace("\\expandafter", "");
            let generated = expand(&generated, &macros);
            let generated = generated.replace("\\expandafter", "");
            let generated = resolve_csname(&generated);
            let (mut merged, _) = collect_macros(&generated);
            // generated fills in; explicitly-written definitions still win
            merged.extend(macros);
            return safe_only(merged);
        }
    }
    safe_only(macros)
}

/// Read each local `<name>.sty` named by a `\usepackage` in `source` (once
/// per absolute path), calling `f` with its text.
fn for_each_local_sty(source: &str, base_dir: &str, seen: &mut HashSet<String>, mut f: impl FnMut(String, &mut HashSet<String>)) {
    for m in USEPKG_RE.find_iter(source) {
        for name in m.name("names").unwrap().split(',') {
            let name = pystr::strip(name);
            if name.is_empty() {
                continue;
            }
            let path = pyos::join(base_dir, &[&format!("{name}.sty")]);
            let real = pyos::abspath(&path);
            if seen.contains(&real) || !pyos::isfile(&path) {
                continue;
            }
            seen.insert(real);
            let Ok(sty) = pyos::read_text(&path, Errors::Replace) else {
                continue;
            };
            f(sty, seen);
        }
    }
}

fn load_local_package_macros(source: &str, base_dir: &str, seen: &mut HashSet<String>, depth: usize) -> Macros {
    if depth > 8 {
        return Macros::new();
    }
    let mut macros = Macros::new();
    for_each_local_sty(source, base_dir, seen, |sty, seen| {
        // a .sty may pull in further local style files
        macros.extend(load_local_package_macros(&sty, base_dir, seen, depth + 1));
        macros.extend(collect_package_macros(&sty));
    });
    macros
}

/// Concatenated text of local `.sty` files referenced by `\usepackage`.
///
/// Used by preamble scanners (`\newtheorem`, `\definecolor`, …) that would
/// otherwise miss declarations split out into a local style file. Comments
/// are stripped; system packages are skipped.
pub fn local_package_sources(source: &str, base_dir: &str) -> String {
    local_package_sources_depth(source, base_dir, &mut HashSet::new(), 0)
}

fn local_package_sources_depth(source: &str, base_dir: &str, seen: &mut HashSet<String>, depth: usize) -> String {
    if depth > 8 {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    for_each_local_sty(source, base_dir, seen, |sty, seen| {
        let sty = strip_comments(&sty);
        parts.push(local_package_sources_depth(&sty, base_dir, seen, depth + 1));
        parts.push(sty);
    });
    parts.retain(|p| !p.is_empty());
    parts.join("\n")
}

/// Collect user macros (incl. local `.sty` packages) and expand them.
pub fn expand_macros(source: &str, base_dir: &str) -> String {
    let mut merged = load_local_package_macros(source, base_dir, &mut HashSet::new(), 0);
    let (macros, stripped) = collect_macros(source);
    // document-body definitions take precedence over package definitions
    merged.extend(macros);
    expand(&stripped, &merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newcommand_with_optional_and_xparse() {
        let src = r"\newcommand{\pair}[2][x]{(#1,#2)}\NewDocumentCommand{\v}{s m}{\IfBooleanTF{#1}{*#2}{#2}}\pair{b} \pair[a]{b} \v*{q} \v{r}";
        assert_eq!(expand_macros(src, "."), "(x,b) (a,b) *q r");
    }

    #[test]
    fn def_and_unbraced_body() {
        assert_eq!(expand_macros(r"\def\sq#1{#1^2}\newcommand\BB\mathbb $\sq{x}\BB R$", "."), r" $x^2\mathbb R$");
    }
}
