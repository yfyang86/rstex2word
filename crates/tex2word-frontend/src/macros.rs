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
        r"\\(?P<kind>newcommand|renewcommand|providecommand|DeclareRobustCommand",
        r"|DeclareTextFontCommand)\s*\*?\s*",
        r"\{?\\(?P<name>[A-Za-z@]+)\}?",
        r"(?:\[(?P<nargs>\d+)\])?",
        r"(?:\[(?P<default>[^\]]*)\])?"
    )
);
// \def\x{..}, \def\x#1#2{..} and delimited \def\x(#1,#2){..} (also \gdef/\edef/\xdef)
lazy_re!(
    DEF_RE,
    r"\\[gex]?def\s*\\(?P<name>[A-Za-z@]+)(?P<params>[^{}\n\\]*?)\{"
);
// \let\a\b, \let\a=\b
lazy_re!(
    LET_RE,
    r"\\let\s*\\(?P<name>[A-Za-z@]+)\s*=?\s*(?P<target>\\[A-Za-z@]+|\\.|[^\s\\])"
);
// \newlength{\x} / \newlength\x (and save boxes): the name becomes empty text
lazy_re!(
    NEWLENGTH_RE,
    r"\\(?:newlength|newsavebox)\s*\{?\s*\\(?P<name>[A-Za-z@]+)\s*\}?"
);
lazy_re!(PLAIN_PARAMS_RE, r"\s*(?:#\d\s*)*");
lazy_re!(PARAM_SPLIT_RE, r"(#\d)");
/// a reference to a command the expander must not expand again (see `\let`)
const NOEXPAND: &str = "\\\x01";
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
    /// delimited `\def` parameter text, tokenised: `["(", "#1", ",", "#2", ")"]`
    pub pattern: Option<Vec<String>>,
}

impl Macro {
    fn simple(name: &str, nargs: i64, default: Option<String>, body: &str) -> Macro {
        Macro {
            name: name.to_string(),
            nargs,
            default,
            body: body.to_string(),
            argspec: None,
            pattern: None,
        }
    }
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
    collect_macros_known(source, &HashSet::new())
}

/// [`collect_macros`]; `known` holds commands the converter handles itself:
/// `\providecommand` leaves those alone (it only defines a command that does
/// not exist yet).
pub fn collect_macros_known(source: &str, known: &HashSet<String>) -> (Macros, String) {
    let mut macros = Macros::new();
    let mut out = String::with_capacity(source.len());
    let n = source.len();
    let mut i = 0;
    while i < n {
        if source[i..].starts_with("\\let") {
            if let Some(lt) = LET_RE.match_at(source, i) {
                let name = lt.name("name").unwrap().to_string();
                let target = lt.name("target").unwrap();
                let tname = &target[1.min(target.len())..];
                let mac = if let Some(src) = macros.get(tname) {
                    // snapshot the current definition
                    Macro {
                        name: name.clone(),
                        ..src.clone()
                    }
                } else if target.starts_with('\\') && target.chars().count() > 2 {
                    // the (built-in) command itself, protected from a later
                    // user redefinition of it (\let\oldsection\section ...)
                    Macro::simple(&name, 0, None, &format!("{NOEXPAND}{tname}"))
                } else {
                    Macro::simple(&name, 0, None, target)
                };
                macros.insert(name, mac);
                i = lt.end();
                continue;
            }
        }
        if source[i..].starts_with("\\new") {
            if let Some(nl) = NEWLENGTH_RE.match_at(source, i) {
                let name = nl.name("name").unwrap();
                macros.insert(name.to_string(), Macro::simple(name, 0, None, ""));
                i = nl.end();
                continue;
            }
        }
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
                            argspec: Some(parse_argspec(argspec_str)),
                            ..Macro::simple(&name, 0, None, body)
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
                let nargs = m
                    .name("nargs")
                    .map_or(0, |s| pystr::parse_int(s).unwrap_or(0));
                let name = m.name("name").unwrap().to_string();
                let provide = m.name("kind") == Some("providecommand");
                if !(provide && (macros.contains_key(&name) || known.contains(&name))) {
                    macros.insert(
                        name.clone(),
                        Macro::simple(&name, nargs, m.name("default").map(str::to_string), body),
                    );
                }
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
                    macros.insert(name.clone(), Macro::simple(&name, 0, None, body));
                    i = end;
                    continue;
                }
            }
        }
        if let Some(d) = DEF_RE.match_at(source, i) {
            let brace_start = d.end() - 1;
            let (body, end) = read_balanced(source, brace_start);
            let params = d.name("params").unwrap_or("");
            let nargs = re::findall(r"#\d", params).len() as i64;
            let mut pattern = None;
            if !pystr::strip(params).is_empty() && PLAIN_PARAMS_RE.fullmatch(params).is_none() {
                // delimited parameters: \def\pt(#1,#2){...}
                pattern = Some(
                    PARAM_SPLIT_RE
                        .split(params, 0)
                        .into_iter()
                        .flatten()
                        .filter(|t| !t.is_empty())
                        .map(str::to_string)
                        .collect(),
                );
            }
            let name = d.name("name").unwrap().to_string();
            macros.insert(
                name.clone(),
                Macro {
                    pattern,
                    ..Macro::simple(&name, nargs, None, body)
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
    let pattern = Regex::new(&format!(r"\\({})(?![A-Za-z@])", alternation.join("|")));

    let mut source = source.to_string();
    for _depth in 0..=32 {
        let mut out = Pieces::default();
        let n = source.len();
        let mut i = 0;
        let mut changed = false;
        while i < n {
            let Some(m) = pattern.search_from(&source, i) else {
                out.push(&source[i..]);
                break;
            };
            out.push(&source[i..m.start()]); // copy the gap up to the macro in one go
            let mac = &macros[m.group_str(1)];
            let mut j = m.end();
            if let Some(pat) = &mac.pattern {
                let Some((args, k)) = match_pattern(&source, j, pat) else {
                    // the delimiters don't match: leave it alone
                    out.push(m.as_str());
                    i = j;
                    continue;
                };
                j = k;
                let mut body = mac.body.clone();
                for (idx, arg) in args.iter().enumerate() {
                    body = body.replace(&format!("#{}", idx + 1), arg);
                }
                let joined = join(&out, &body);
                out.push(&joined);
                i = j;
                changed = true;
                continue;
            }
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
            let joined = join(&out, &body);
            out.push(&joined);
            i = j;
            changed = true;
        }
        source = out.text;
        if !changed {
            break;
        }
    }
    source
}

/// The expander's output, remembering where its last two pieces start (as
/// Python's list of pieces does, for [`join`]).
#[derive(Default)]
struct Pieces {
    text: String,
    starts: [usize; 2],
}

impl Pieces {
    fn push(&mut self, piece: &str) {
        self.starts = [self.starts[1], self.text.len()];
        self.text.push_str(piece);
    }

    /// `"".join(out[-2:])`.
    fn tail(&self) -> &str {
        &self.text[self.starts[0]..]
    }
}

/// `body` as inserted after `out`: a space separates it from a control word
/// just before it (TeX had already ended that word).
fn join(out: &Pieces, body: &str) -> String {
    let first = body.chars().next();
    if first.is_some_and(|c| pystr::is_alpha(c) || c == '@')
        && CONTROL_WORD_END_RE.search(out.tail()).is_some()
    {
        return format!(" {body}");
    }
    body.to_string()
}

lazy_re!(CONTROL_WORD_END_RE, r"\\[A-Za-z@]+$");

/// Match a delimited `\def` parameter text at `j`; `(args, end)` or `None`.
fn match_pattern(text: &str, j: usize, pattern: &[String]) -> Option<(Vec<String>, usize)> {
    let mut args: Vec<String> = Vec::new();
    let mut i = j;
    let b = text.as_bytes();
    for (idx, tok) in pattern.iter().enumerate() {
        if !tok.starts_with('#') {
            if !text[i..].starts_with(tok.as_str()) {
                return None;
            }
            i += tok.len();
            continue;
        }
        let nxt = pattern.get(idx + 1);
        let Some(nxt) = nxt.filter(|t| !t.starts_with('#')) else {
            // undelimited: one argument
            let (arg, k) = read_arg(text, i);
            args.push(arg.to_string());
            i = k;
            continue;
        };
        let mut depth = 0i64;
        let mut k = i;
        let mut found = false;
        while k < b.len() {
            match b[k] {
                b'\\' => {
                    // skip the backslash and the (whole) character it escapes
                    k += 1 + text[k + 1..].chars().next().map_or(1, char::len_utf8);
                    continue;
                }
                b'{' => depth += 1,
                b'}' => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                }
                _ if depth == 0 && text[k..].starts_with(nxt.as_str()) => {
                    found = true;
                    break;
                }
                _ => {}
            }
            k += 1;
            // keep k on a character boundary (multi-byte text between delimiters)
            while k < b.len() && !text.is_char_boundary(k) {
                k += 1;
            }
        }
        if !found {
            return None;
        }
        let mut arg = &text[i..k];
        if arg.starts_with('{') && arg.ends_with('}') && read_balanced(arg, 0).1 == arg.len() {
            arg = &arg[1..arg.len() - 1]; // TeX strips one pair of braces around an argument
        }
        args.push(arg.to_string());
        i = k;
    }
    Some((args, i))
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
    "section",
    "subsection",
    "subsubsection",
    "paragraph",
    "subparagraph",
    "chapter",
    "part",
    "title",
    "author",
    "date",
    "thanks",
    "maketitle",
    "caption",
    "captionof",
    "label",
    "ref",
    "cref",
    "Cref",
    "autoref",
    "eqref",
    "pageref",
    "cite",
    "citep",
    "citet",
    "item",
    "footnote",
    "begin",
    "end",
    "abstract",
    "appendix",
    "bibliography",
    "tableofcontents",
    "includegraphics",
    "newpage",
    "clearpage",
    "input",
    "include",
    "today",
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
    let self_ref = re::compile(
        &format!(r"\\{}(?![A-Za-z])", re::escape(&mac.name)),
        Flags::NONE,
    );
    !self_ref.is_match(&mac.body)
}

fn safe_only(macros: Macros) -> Macros {
    macros
        .into_iter()
        .filter(|(_, m)| is_safe_macro(m))
        .collect()
}

/// Turn `\csname foo\endcsname` into `\foo` (dropping inner spaces).
fn resolve_csname(text: &str) -> String {
    CSNAME_RE.sub_fn(text, |m| {
        format!(
            "\\{}",
            pystr::strip(m.name("body").unwrap()).replace(' ', "")
        )
    })
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
fn for_each_local_sty(
    source: &str,
    base_dir: &str,
    seen: &mut HashSet<String>,
    mut f: impl FnMut(String, &mut HashSet<String>),
) {
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

fn load_local_package_macros(
    source: &str,
    base_dir: &str,
    seen: &mut HashSet<String>,
    depth: usize,
) -> Macros {
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

fn local_package_sources_depth(
    source: &str,
    base_dir: &str,
    seen: &mut HashSet<String>,
    depth: usize,
) -> String {
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

// ----------------------------------------------------------------------------
// User environments: \newenvironment / \NewDocumentEnvironment
// ----------------------------------------------------------------------------

lazy_re!(
    NEWENV_RE,
    concat!(
        r"\\(?P<kind>new|renew|provide)environment\s*\*?\s*\{(?P<name>[A-Za-z@*]+)\}",
        r"(?:\s*\[(?P<nargs>\d+)\])?(?:\s*\[(?P<default>[^\]]*)\])?\s*"
    )
);
lazy_re!(
    XENV_RE,
    r"\\(?P<kind>New|Renew|Provide|Declare)DocumentEnvironment\s*\{(?P<name>[A-Za-z@*]+)\}\s*"
);
/// environments the parser handles itself: a user (re)definition of one is
/// usually cosmetic (spacing, fonts) and expanding it would lose the structure
const NATIVE_ENVS: &[&str] = &[
    "document",
    "abstract",
    "figure",
    "figure*",
    "table",
    "table*",
    "itemize",
    "enumerate",
    "description",
    "equation",
    "equation*",
    "align",
    "align*",
    "gather",
    "multline",
    "verbatim",
    "lstlisting",
    "minted",
    "tabular",
    "tabularx",
    "longtable",
    "center",
    "flushleft",
    "flushright",
    "quote",
    "quotation",
    "verse",
    "minipage",
    "thebibliography",
    "proof",
    "theorem",
    "lemma",
    "definition",
    "list",
    "trivlist",
    "titlepage",
    "array",
    "math",
    "displaymath",
];
// begin/end code the static expander cannot follow (list internals, kernel @-macros)
lazy_re!(
    ENV_UNSAFE_RE,
    r"\\(?:list|endlist|trivlist|endtrivlist|item\b)|@"
);

/// A user environment definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    pub name: String,
    pub nargs: i64,
    pub default: Option<String>,
    pub begin: String,
    pub end: String,
    pub argspec: Option<Vec<ArgSpec>>,
}

/// Find `\newenvironment`-style definitions; return them (in definition
/// order) and the source with the definitions removed.
pub fn collect_environments(source: &str) -> (Vec<Env>, String) {
    let mut envs: Vec<Env> = Vec::new();
    let mut out = String::with_capacity(source.len());
    let n = source.len();
    let b = source.as_bytes();
    let mut i = 0;
    loop {
        let m = NEWENV_RE.search_from(source, i);
        let x = XENV_RE.search_from(source, i);
        // the earlier hit (the \newenvironment one on a tie, as Python's min)
        let (hit, is_x) = match (&m, &x) {
            (None, None) => {
                out.push_str(&source[i..]);
                break;
            }
            (Some(m), None) => (m, false),
            (None, Some(x)) => (x, true),
            (Some(m), Some(x)) => {
                if x.start() < m.start() {
                    (x, true)
                } else {
                    (m, false)
                }
            }
        };
        out.push_str(&source[i..hit.start()]);
        let mut j = hit.end();
        let mut argspec = None;
        if is_x {
            if j >= n || b[j] != b'{' {
                out.push_str(&source[hit.start()..j]);
                i = j;
                continue;
            }
            let (spec, k) = read_balanced(source, j);
            j = k;
            argspec = Some(parse_argspec(spec));
        }
        let mut bodies: Vec<&str> = Vec::new();
        for _ in 0..2 {
            while j < n && matches!(b[j], b' ' | b'\n' | b'\t') {
                j += 1;
            }
            if j >= n || b[j] != b'{' {
                break;
            }
            let (body, k) = read_balanced(source, j);
            bodies.push(body);
            j = k;
        }
        if bodies.len() != 2 {
            out.push_str(&source[hit.start()..j]);
            i = j;
            continue;
        }
        let name = hit.name("name").unwrap().to_string();
        let kind = pystr::lower(hit.name("kind").unwrap());
        let safe = !(ENV_UNSAFE_RE.search(bodies[0]).is_some()
            || ENV_UNSAFE_RE.search(bodies[1]).is_some());
        let replaces_native = NATIVE_ENVS.contains(&name.as_str());
        let existing = envs.iter().position(|e| e.name == name);
        if safe && !replaces_native && !(kind == "provide" && existing.is_some()) {
            let nargs = if is_x {
                argspec.as_ref().map_or(0, |a| a.len() as i64)
            } else {
                hit.name("nargs")
                    .map_or(0, |s| pystr::parse_int(s).unwrap_or(0))
            };
            let default = if is_x {
                None
            } else {
                hit.name("default").map(str::to_string)
            };
            let env = Env {
                name: name.clone(),
                nargs,
                default,
                begin: bodies[0].to_string(),
                end: bodies[1].to_string(),
                argspec,
            };
            match existing {
                Some(k) => envs[k] = env, // a dict update keeps the first position
                None => envs.push(env),
            }
        }
        i = j;
    }
    (envs, out)
}

/// Replace `\begin{name}args BODY\end{name}` by begin code + body + end code.
pub fn expand_environments(source: &str, envs: &[Env]) -> String {
    let mut source = source.to_string();
    if envs.is_empty() {
        return source;
    }
    let names: Vec<String> = envs.iter().map(|e| re::escape(&e.name)).collect();
    let pattern = Regex::new(&format!(r"\\begin\s*\{{({})\}}", names.join("|")));
    for _depth in 0..=16 {
        let mut out = String::with_capacity(source.len());
        let mut i = 0;
        let mut changed = false;
        loop {
            let Some(m) = pattern.search_from(&source, i) else {
                out.push_str(&source[i..]);
                break;
            };
            let env = envs.iter().find(|e| e.name == m.group_str(1)).unwrap();
            let Some((end_start, end_stop)) = matching_end(&source, m.end(), &env.name) else {
                out.push_str(&source[i..m.end()]);
                i = m.end();
                continue;
            };
            out.push_str(&source[i..m.start()]);
            let mut j = m.end();
            let args: Vec<String> = if let Some(argspec) = &env.argspec {
                let (args, k) = read_xparse_args(argspec, &source, j);
                j = k;
                args
            } else {
                let mut args = Vec::new();
                let mut remaining = env.nargs;
                if env.nargs != 0 {
                    if let Some(default) = &env.default {
                        let (opt, k) = read_optional(&source, j);
                        j = k;
                        args.push(opt.unwrap_or(default).to_string());
                        remaining -= 1;
                    }
                }
                for _ in 0..remaining.max(0) {
                    let (arg, k) = read_arg(&source, j);
                    j = k;
                    args.push(arg.to_string());
                }
                args
            };
            let inner_start = j;
            let (mut begin, mut end) = (env.begin.clone(), env.end.clone());
            for (idx, arg) in args.iter().enumerate() {
                begin = begin.replace(&format!("#{}", idx + 1), arg);
                end = end.replace(&format!("#{}", idx + 1), arg);
            }
            if env.argspec.is_some() {
                begin = resolve_conditionals(&begin);
                end = resolve_conditionals(&end);
            }
            out.push_str(&begin);
            out.push_str(&source[inner_start.min(end_start)..end_start]);
            out.push_str(&end);
            i = end_stop;
            changed = true;
        }
        source = out;
        if !changed {
            break;
        }
    }
    source
}

/// `(start, stop)` of the `\end{name}` closing a `\begin{name}` (nesting-aware).
fn matching_end(source: &str, start: usize, name: &str) -> Option<(usize, usize)> {
    let tok = Regex::new(&format!(r"\\(begin|end)\s*\{{{}\}}", re::escape(name)));
    let mut depth = 1i64;
    for t in tok.find_iter_from(source, start) {
        depth += if t.group_str(1) == "begin" { 1 } else { -1 };
        if depth == 0 {
            return Some((t.start(), t.end()));
        }
    }
    None
}

lazy_re!(
    EXPANDAFTER_DEF_RE,
    r"\\expandafter\s*(?=\\(?:[gex]?def|let|newcommand|renewcommand|providecommand)\b)"
);
lazy_re!(NAMEDEF_RE, r"\\@namedef\s*\{([A-Za-z@]+)\}");
lazy_re!(NAMEUSE_RE, r"\\@nameuse\s*\{([A-Za-z@]+)\}");

/// Collect user macros (incl. local `.sty` packages) and expand them.
pub fn expand_macros(source: &str, base_dir: &str) -> String {
    expand_macros_known(source, base_dir, &HashSet::new())
}

/// [`expand_macros`] with the commands the converter handles itself
/// (`\providecommand` leaves those alone).
pub fn expand_macros_known(source: &str, base_dir: &str, known: &HashSet<String>) -> String {
    let mut merged = load_local_package_macros(source, base_dir, &mut HashSet::new(), 0);
    let (envs, source) = collect_environments(source);
    let mut source = expand_environments(&source, &envs);
    // \expandafter\def\csname name\endcsname{..}, \@namedef{name}{..}, \@nameuse{name}
    if source.contains("\\csname") || source.contains("\\@name") {
        source = EXPANDAFTER_DEF_RE.sub("", &source);
        source = resolve_csname(&source);
        source = NAMEDEF_RE.sub_fn(&source, |m| format!("\\def\\{}", m.group_str(1)));
        source = NAMEUSE_RE.sub_fn(&source, |m| format!("\\{}", m.group_str(1)));
    }
    let (macros, stripped) = collect_macros_known(&source, known);
    // A document redefining a structural command through LaTeX internals
    // (\def\paragraph{\@startsection{paragraph}...} inside \makeatletter) only
    // restyles it; expanding that would replace every heading with raw markup.
    // Document-body definitions take precedence over package definitions.
    merged.extend(macros.into_iter().filter(|(name, m)| {
        !(PROTECTED_NAMES.contains(&name.as_str()) && AT_COMMAND_RE.is_match(&m.body))
    }));
    expand(&stripped, &merged).replace(NOEXPAND, "\\")
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
        assert_eq!(
            expand_macros(r"\def\sq#1{#1^2}\newcommand\BB\mathbb $\sq{x}\BB R$", "."),
            r" $x^2\mathbb R$"
        );
    }
}
