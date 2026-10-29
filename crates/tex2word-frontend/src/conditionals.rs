//! Resolve LaTeX conditionals whose outcome is known without running TeX
//! (port of `frontend/conditionals.py`).
//!
//! Handled (the branch that TeX would take is kept, the other dropped):
//!
//! * primitive form: `\iftrue`/`\iffalse`, `\newif`-declared switches
//!   (`\newif\ifdraft \drafttrue ... \ifdraft A\else B\fi`),
//!   `\ifdefined\cs` and `\ifcsname name\endcsname`;
//! * ifthen: `\ifthenelse{\boolean{x}}{A}{B}` (also `\equal`, `\not`,
//!   `\isundefined`) with `\newboolean`/`\setboolean`;
//! * etoolbox: `\iftoggle`/`\nottoggle`, `\ifbool`/`\notbool`,
//!   `\ifdef`/`\ifundef`;
//! * `\IfFileExists`/`\InputIfFileExists`, `\@ifpackageloaded`,
//!   `\@ifclassloaded`, `\IfPackageLoadedTF`, `\@ifundefined`.
//!
//! Switch assignments are applied in document order, and the declarations
//! themselves are removed. Anything undecidable (`\ifnum`, `\ifx`, engine
//! tests such as `\ifxetex`) is left untouched. Verbatim text is never touched.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use tex2word_ir::dict::Dict;
use tex2word_ir::pyos;
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

/// primitive conditionals that open an \if...\fi block (for nesting), besides
/// \newif switches; \iff (math) and \ifthenelse (a macro) are not among them
const PRIMITIVE_IFS: &[&str] = &[
    "if",
    "ifcat",
    "ifnum",
    "ifdim",
    "ifodd",
    "ifvmode",
    "ifhmode",
    "ifmmode",
    "ifinner",
    "ifvoid",
    "ifhbox",
    "ifvbox",
    "ifx",
    "ifeof",
    "iftrue",
    "iffalse",
    "ifcase",
    "ifdefined",
    "ifcsname",
    "iffontchar",
    "ifincsname",
    "ifpdfprimitive",
    "ifpdf",
    "ifxetex",
    "ifluatex",
    "ifPDFTeX",
    "ifXeTeX",
    "ifLuaTeX",
    "ifptex",
    "ifuptex",
];

/// commands every LaTeX document has (for \ifdefined / \ifdef / \@ifundefined)
const KERNEL: &[&str] = &[
    "section",
    "subsection",
    "subsubsection",
    "paragraph",
    "subparagraph",
    "textbf",
    "textit",
    "emph",
    "texttt",
    "label",
    "ref",
    "cite",
    "caption",
    "footnote",
    "item",
    "begin",
    "end",
    "newcommand",
    "renewcommand",
    "def",
    "let",
    "relax",
    "par",
    "today",
    "LaTeX",
    "TeX",
    "maketitle",
    "title",
    "author",
    "date",
    "includegraphics",
    "url",
    "href",
    "textcolor",
    "color",
    "frac",
    "sqrt",
    "mathbb",
    "mathbf",
    "mathrm",
];

const VERB_ENVS: &[&str] = &[
    "verbatim",
    "verbatim*",
    "Verbatim",
    "lstlisting",
    "minted",
    "comment",
    "filecontents",
    "spverbatim",
    "BVerbatim",
    "LVerbatim",
];

static MASK_RE: LazyLock<Regex> = LazyLock::new(|| {
    let envs: Vec<String> = VERB_ENVS.iter().map(|e| re::escape(e)).collect();
    Regex::with_flags(
        &format!(
            r"\\begin\{{({})\}}.*?\\end\{{\1\}}|\\verb\*?([^\sA-Za-z*]).*?\2",
            envs.join("|")
        ),
        Flags::S,
    )
});
macro_rules! cs {
    () => {
        "[A-Za-z@]+"
    };
}
lazy_re!(NEWIF_RE, concat!(r"\\newif\s*\\if(", cs!(), ")"));
lazy_re!(
    DEF_RE,
    concat!(
        r"\\(?:re)?(?:newcommand|providecommand|DeclareRobustCommand|DeclareMathOperator)\*?",
        r"\s*\{?\s*\\(",
        cs!(),
        ")",
        r"|\\(?:def|gdef|edef|xdef|let|newlength|newsavebox|newif)\s*\\(",
        cs!(),
        ")",
        r"|\\(?:NewDocumentCommand|RenewDocumentCommand|ProvideDocumentCommand)\s*\{?\s*\\(",
        cs!(),
        ")"
    )
);
lazy_re!(TOKEN_RE, concat!(r"\\(", cs!(), ")"));
lazy_re!(UNMASK_RE, r"\x00(\d+)\x00");
lazy_re!(TAIL_CS_RE, r"\\[A-Za-z@]+$");
lazy_re!(NEWIF_ARG_RE, concat!(r"\s*\\if(", cs!(), ")"));
lazy_re!(IFDEFINED_ARG_RE, concat!(r"\s*\\(", cs!(), ")"));
lazy_re!(IFCSNAME_ARG_RE, concat!(r"\s*(", cs!(), r")\s*\\endcsname"));
lazy_re!(BOOLEAN_TEST_RE, r"\\boolean\s*\{([^{}]*)\}");
lazy_re!(EQUAL_TEST_RE, r"\\equal\s*\{([^{}]*)\}\s*\{([^{}]*)\}");
lazy_re!(
    ISUNDEFINED_TEST_RE,
    concat!(r"\\isundefined\s*\{?\s*\\(", cs!(), r")\s*\}?")
);

pub fn resolve_conditionals(source: &str, base_dir: &str) -> String {
    if !source.contains("\\if")
        && !source.contains("\\If")
        && !source.contains("@if")
        && !source.contains("toggle")
        && !source.contains("bool")
    {
        return source.to_string();
    }
    let (masked, saved) = mask(source);
    let mut r = Resolver::new(&masked, base_dir);
    let out = r.run();
    unmask(&out, &saved)
}

pub(crate) fn mask(source: &str) -> (String, Vec<String>) {
    let mut saved: Vec<String> = Vec::new();

    let keep = |m: &re::Match| -> String {
        saved.push(m.as_str().to_string());
        format!("\x00{}\x00", saved.len() - 1)
    };

    let masked = MASK_RE.sub_fn(source, keep);
    (masked, saved)
}

pub(crate) fn unmask(source: &str, saved: &[String]) -> String {
    // Python indexes `saved` unchecked (an out-of-range index raises); a
    // placeholder that does not name a saved region is left as it is
    UNMASK_RE.sub_fn(source, |m| {
        pystr::parse_int(m.group_str(1))
            .and_then(|k| usize::try_from(k).ok())
            .and_then(|k| saved.get(k))
            .cloned()
            .unwrap_or_else(|| m.as_str().to_string())
    })
}

struct Resolver<'a> {
    src: &'a str,
    base_dir: &'a str,
    switches: Dict<bool>,
    booleans: HashMap<String, bool>, // ifthen \newboolean / etoolbox \newbool
    toggles: HashMap<String, bool>,
    defined: HashSet<String>,
    packages: HashSet<String>,
    classes: HashSet<String>,
}

impl<'a> Resolver<'a> {
    fn new(src: &'a str, base_dir: &'a str) -> Self {
        let mut switches = Dict::new();
        for n in NEWIF_RE.findall(src) {
            switches.insert(n, false);
        }
        let mut defined = HashSet::new();
        let packages = loaded(src, "usepackage|RequirePackage");
        let classes = loaded(src, "documentclass|LoadClass");
        if ["book", "report", "memoir", "scrbook", "scrreprt", "amsbook"]
            .iter()
            .any(|c| classes.contains(*c))
        {
            defined.insert("chapter".to_string());
        }
        Resolver {
            src,
            base_dir,
            switches,
            booleans: HashMap::new(),
            toggles: HashMap::new(),
            defined,
            packages,
            classes,
        }
    }

    // -- scanning ----------------------------------------------------------- //

    fn run(&mut self) -> String {
        self.scan(self.src)
    }

    fn scan(&mut self, s: &str) -> String {
        let mut out: Vec<Cow<str>> = Vec::new();
        let (mut i, n) = (0, s.len());
        while i < n {
            let Some(m) = TOKEN_RE.search_from(s, i) else {
                out.push(Cow::Borrowed(&s[i..]));
                break;
            };
            out.push(Cow::Borrowed(&s[i..m.start()]));
            let name = m.group_str(1);
            let j = m.end();
            // remember definitions in document order (for \ifdefined & co.)
            if let Some(d) = DEF_RE.match_at(s, m.start()) {
                if let Some(g) = d.groups().into_iter().flatten().find(|g| !g.is_empty()) {
                    self.defined.insert(g.to_string());
                }
            }
            match self.handle(s, m.start(), j, name) {
                None => {
                    out.push(Cow::Borrowed(&s[m.start()..j]));
                    i = j;
                }
                Some((mut text, next)) => {
                    i = next;
                    // never glue the kept text onto a preceding control word
                    let tail = out[out.len().saturating_sub(3)..].concat();
                    if text.chars().next().is_some_and(pystr::is_alpha)
                        && TAIL_CS_RE.is_match(&tail)
                    {
                        text = format!(" {text}");
                    }
                    out.push(Cow::Owned(text));
                }
            }
        }
        out.concat()
    }

    fn handle(&mut self, s: &str, _start: usize, j: usize, name: &str) -> Option<(String, usize)> {
        // -- declarations and assignments -------------------------------------- //
        if name == "newif" {
            if let Some(m) = NEWIF_ARG_RE.match_at(s, j) {
                self.switches.set_default(m.group_str(1), false);
                return Some((String::new(), m.end()));
            }
            return None;
        }
        let hit = self.switches.keys().find(|sw| {
            name.strip_suffix("true") == Some(sw.as_str())
                || name.strip_suffix("false") == Some(sw.as_str())
        });
        if let Some(sw) = hit.cloned() {
            self.switches.insert(sw, name.ends_with("true"));
            return Some((String::new(), skip_space(s, j)));
        }
        if matches!(
            name,
            "newboolean"
                | "provideboolean"
                | "newbool"
                | "providebool"
                | "newtoggle"
                | "providetoggle"
        ) {
            if let Some((args, end)) = groups(s, j, 1) {
                let key = pystr::strip(args[0]);
                let table = if name.contains("toggle") {
                    &mut self.toggles
                } else {
                    &mut self.booleans
                };
                if !name.starts_with("provide") || !table.contains_key(key) {
                    table.insert(key.to_string(), false);
                }
                return Some((String::new(), end));
            }
            return None;
        }
        if matches!(name, "setboolean" | "setbool" | "settoggle") {
            if let Some((args, end)) = groups(s, j, 2) {
                let (key, val) = (args[0], args[1]);
                let table = if name == "settoggle" {
                    &mut self.toggles
                } else {
                    &mut self.booleans
                };
                table.insert(
                    pystr::strip(key).to_string(),
                    pystr::lower(pystr::strip(val)) == "true",
                );
                return Some((String::new(), end));
            }
            return None;
        }
        if matches!(
            name,
            "booltrue" | "boolfalse" | "toggletrue" | "togglefalse"
        ) {
            if let Some((args, end)) = groups(s, j, 1) {
                let key = args[0];
                let table = if name.starts_with("toggle") {
                    &mut self.toggles
                } else {
                    &mut self.booleans
                };
                table.insert(pystr::strip(key).to_string(), name.ends_with("true"));
                return Some((String::new(), end));
            }
            return None;
        }
        // -- brace-form conditionals ------------------------------------------- //
        if let Some((value, _nargs_before, end)) = self.brace_condition(s, j, name) {
            let (branches, stop) = groups(s, end, 2)?;
            let (yes, no) = (branches[0], branches[1]);
            return Some((self.scan(if value { yes } else { no }), stop));
        }
        // -- primitive \if...\else...\fi --------------------------------------- //
        if name.starts_with("if")
            && (PRIMITIVE_IFS.contains(&name) || self.switches.contains_key(&name[2..]))
        {
            return self.primitive(s, j, name);
        }
        None
    }

    /// (value, -, index after the condition argument(s)) for brace-form tests.
    fn brace_condition(&self, s: &str, j: usize, name: &str) -> Option<(bool, usize, usize)> {
        if name == "ifthenelse" {
            let (args, end) = groups(s, j, 1)?;
            let test = args[0];
            let value = self.ifthen_test(test);
            return value.map(|value| (value, 1, end));
        }
        if matches!(name, "iftoggle" | "nottoggle" | "ifbool" | "notbool") {
            let (args, end) = groups(s, j, 1)?;
            let key = args[0];
            let table = if name.contains("toggle") {
                &self.toggles
            } else {
                &self.booleans
            };
            let value = *table.get(pystr::strip(key))?;
            return Some((
                if name.starts_with("not") {
                    !value
                } else {
                    value
                },
                1,
                end,
            ));
        }
        if matches!(
            name,
            "ifdef" | "ifundef" | "ifdefmacro" | "ifcsdef" | "ifcsundef"
        ) {
            let (args, end) = groups(s, j, 1)?;
            let cs = args[0];
            let value = self.is_defined(pystr::lstrip_chars(pystr::strip(cs), "\\"));
            return Some((
                if name.contains("undef") {
                    !value
                } else {
                    value
                },
                1,
                end,
            ));
        }
        if matches!(name, "IfFileExists" | "InputIfFileExists") {
            let (args, end) = groups(s, j, 1)?;
            let fname = args[0];
            return Some((self.file_exists(pystr::strip(fname)), 1, end));
        }
        if matches!(
            name,
            "@ifpackageloaded" | "IfPackageLoadedTF" | "@ifclassloaded" | "IfClassLoadedTF"
        ) {
            let (args, end) = groups(s, j, 1)?;
            let pkg = args[0];
            let pool = if name.to_lowercase().contains("class") {
                &self.classes
            } else {
                &self.packages
            };
            return Some((pool.contains(pystr::strip(pkg)), 1, end));
        }
        if name == "@ifundefined" {
            let (args, end) = groups(s, j, 1)?;
            let cs = args[0];
            return Some((
                !self.is_defined(pystr::lstrip_chars(pystr::strip(cs), "\\")),
                1,
                end,
            ));
        }
        None
    }

    fn primitive(&mut self, s: &str, j: usize, name: &str) -> Option<(String, usize)> {
        let value: Option<bool>;
        let mut cond_end = j;
        if name == "iftrue" {
            value = Some(true);
        } else if name == "iffalse" {
            value = Some(false);
        } else if let Some(&v) = self.switches.get(&name[2..]) {
            value = Some(v);
        } else if name == "ifdefined" {
            let m = IFDEFINED_ARG_RE.match_at(s, j)?;
            value = Some(self.is_defined(m.group_str(1)));
            cond_end = m.end();
        } else if name == "ifcsname" {
            let m = IFCSNAME_ARG_RE.match_at(s, j)?;
            value = Some(self.is_defined(m.group_str(1)));
            cond_end = m.end();
        } else {
            value = None;
        }
        let switches: HashSet<&str> = self.switches.keys().map(String::as_str).collect();
        let (else_at, fi_start, fi_end) = match_fi(s, cond_end, &switches)?;
        let Some(value) = value else {
            // undecidable: keep the whole block, but resolve what is inside it
            let head = &s[j - name.len() - 1..cond_end];
            let inner = self.scan(&s[cond_end..fi_start]);
            return Some((format!("{head}{inner}{}", &s[fi_start..fi_end]), fi_end));
        };
        let chosen = if value {
            match else_at {
                Some((else_start, _)) => &s[cond_end..else_start],
                None => &s[cond_end..fi_start],
            }
        } else {
            match else_at {
                Some((_, else_end)) => &s[else_end..fi_start],
                None => "",
            }
        };
        Some((self.scan(chosen), fi_end))
    }

    // -- tests ----------------------------------------------------------------- //

    fn ifthen_test(&self, test: &str) -> Option<bool> {
        let mut t = pystr::strip(test);
        let mut neg = false;
        while let Some(rest) = t.strip_prefix("\\not") {
            neg = !neg;
            t = pystr::strip(rest);
        }
        if let Some(m) = BOOLEAN_TEST_RE.fullmatch(t) {
            let v = *self.booleans.get(pystr::strip(m.group_str(1)))?;
            return Some(if neg { !v } else { v });
        }
        if let Some(m) = EQUAL_TEST_RE.fullmatch(t) {
            let v = pystr::strip(m.group_str(1)) == pystr::strip(m.group_str(2));
            return Some(if neg { !v } else { v });
        }
        if let Some(m) = ISUNDEFINED_TEST_RE.fullmatch(t) {
            let v = !self.is_defined(m.group_str(1));
            return Some(if neg { !v } else { v });
        }
        None
    }

    fn is_defined(&self, cs: &str) -> bool {
        self.defined.contains(cs)
            || KERNEL.contains(&cs)
            || self.switches.contains_key(cs)
            || self
                .switches
                .keys()
                .any(|sw| cs.strip_prefix("if") == Some(sw.as_str()))
    }

    fn file_exists(&self, name: &str) -> bool {
        for cand in [name.to_string(), format!("{name}.tex")] {
            if pyos::isfile(&pyos::join2(self.base_dir, &cand)) {
                return true;
            }
        }
        false
    }
}

fn loaded(src: &str, cmds: &str) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    let pat = format!(r"\\(?:{cmds})\s*(?:\[[^\]]*\])?\s*\{{([^}}]*)\}}");
    for m in re::compile(&pat, Flags::NONE).find_iter(src) {
        out.extend(
            m.group_str(1)
                .split(',')
                .filter(|p| !pystr::strip(p).is_empty())
                .map(|p| pystr::strip(p).to_string()),
        );
    }
    out
}

fn skip_space(s: &str, mut i: usize) -> usize {
    let b = s.as_bytes();
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    i
}

/// Read `count` balanced `{...}` groups starting at `i` (whitespace and
/// newlines between them allowed).
fn groups(s: &str, mut i: usize, count: usize) -> Option<(Vec<&str>, usize)> {
    // byte-wise: every character tested is ASCII, and a UTF-8 continuation
    // byte never equals one, so this matches Python's per-character loop
    let b = s.as_bytes();
    let mut out: Vec<&str> = Vec::new();
    for _ in 0..count {
        while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if i >= b.len() || b[i] != b'{' {
            return None;
        }
        let mut depth = 0i64;
        let mut closed = false;
        for (k, &c) in b.iter().enumerate().skip(i) {
            if c == b'\\' {
                continue;
            }
            if c == b'{' && (k == 0 || b[k - 1] != b'\\') {
                depth += 1;
            } else if c == b'}' && (k == 0 || b[k - 1] != b'\\') {
                depth -= 1;
                if depth == 0 {
                    out.push(&s[i + 1..k]);
                    i = k + 1;
                    closed = true;
                    break;
                }
            }
        }
        if !closed {
            return None;
        }
    }
    Some((out, i))
}

lazy_re!(IF_TOKEN_RE, concat!(r"\\(", cs!(), ")"));

type ElseSpan = Option<(usize, usize)>;

/// Find this conditional's `\else` (start, end) and `\fi` (start, end).
fn match_fi(s: &str, start: usize, switches: &HashSet<&str>) -> Option<(ElseSpan, usize, usize)> {
    let mut depth = 1;
    let mut else_at: ElseSpan = None;
    // the pattern never matches empty, so searching on from each match's end
    // equals `finditer`
    let mut pos = start;
    while let Some(m) = IF_TOKEN_RE.search_from(s, pos) {
        pos = m.end();
        let name = m.group_str(1);
        if name == "fi" {
            depth -= 1;
            if depth == 0 {
                return Some((else_at, m.start(), m.end()));
            }
        } else if name == "else" {
            if depth == 1 && else_at.is_none() {
                else_at = Some((m.start(), m.end()));
            }
        } else if name.starts_with("if")
            && (PRIMITIVE_IFS.contains(&name) || switches.contains(&name[2..]))
        {
            depth += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    // expected strings are the Python reference's output
    use super::*;

    fn rc(s: &str) -> String {
        resolve_conditionals(s, "/nonexistent-base-dir")
    }

    #[test]
    fn newif_switch_follows_assignments() {
        assert_eq!(
            rc(r"\newif\ifdraft \ifdraft A\else B\fi \drafttrue \ifdraft C\else D\fi"),
            r"  B  C"
        );
        assert_eq!(
            rc("\\newif\\ifdraft\n\\drafttrue\n\\ifdraft\nA\n\\else\nB\n\\fi\n"),
            "\n\n\nA\n\n"
        );
        assert_eq!(rc(r"\drafttrue\newif\ifdraft\ifdraft A\else B\fi"), r" A");
    }

    #[test]
    fn iftrue_iffalse_and_nesting() {
        assert_eq!(rc(r"\iftrue A\else B\fi"), r" A");
        assert_eq!(rc(r"\iffalse A\else B\fi"), r" B");
        assert_eq!(rc(r"x \iffalse dropped \section{y} \fi z"), r"x  z");
        assert_eq!(rc(r"\iffalse $a \iff b$ \fi kept"), r" kept");
        assert_eq!(
            rc(r"\newif\ifa\atrue \ifa X \iffalse no \else yes \fi Y\else Z\fi"),
            r" X  yes  Y"
        );
        assert_eq!(rc(r"\iffalse a\else b\else c\fi"), r" b\else c");
    }

    #[test]
    fn undecidable_tests_keep_the_block() {
        assert_eq!(
            rc(r"\ifnum\value{page}>1 A \iftrue B\fi \else C\fi"),
            r"\ifnum\value{page}>1 A  B \else C\fi"
        );
        assert_eq!(rc(r"\ifx\a\b A\else B\fi"), r"\ifx\a\b A\else B\fi");
        assert_eq!(rc(r"\ifxetex A\else B\fi"), r"\ifxetex A\else B\fi");
        assert_eq!(rc(r"\iffalse \ifnum1=1 x\else y\fi\else z\fi"), r" z");
    }

    #[test]
    fn ifdefined_and_ifcsname() {
        assert_eq!(
            rc(r"\newcommand{\foo}{x}\ifdefined\foo A\else B\fi \ifdefined\nope C\else D\fi"),
            r"\newcommand{\foo}{x} A  D"
        );
        assert_eq!(rc(r"\ifcsname section\endcsname S\else T\fi"), r" S");
        assert_eq!(
            rc(r"\documentclass{book}\ifdefined\chapter A\else B\fi"),
            r"\documentclass{book} A"
        );
    }

    #[test]
    fn ifthenelse() {
        assert_eq!(
            rc(
                r"\newboolean{long}\setboolean{long}{true}\ifthenelse{\boolean{long}}{L}{S} \ifthenelse{\not\boolean{long}}{N}{Y} \ifthenelse{\equal{a}{a}}{E}{F}"
            ),
            r"L Y E"
        );
        assert_eq!(rc(r"\ifthenelse{\isundefined{\foo}}{U}{D}"), r"U");
        assert_eq!(
            rc(r"\ifthenelse{\boolean{zz}}{A}{B}"),
            r"\ifthenelse{\boolean{zz}}{A}{B}"
        );
    }

    #[test]
    fn etoolbox() {
        assert_eq!(
            rc(
                r"\newtoggle{ans}\toggletrue{ans}\iftoggle{ans}{T}{F} \nottoggle{ans}{N}{Y} \newbool{b}\ifbool{b}{B}{C} \ifdef{\section}{D}{E} \ifundef{\zzz}{U}{V}"
            ),
            r"T Y C D U"
        );
        assert_eq!(rc("\\newtoggle{t}\\iftoggle{t} {A}\n{B} tail"), r"B tail");
        assert_eq!(rc(r"\newtoggle{t}\iftoggle{t}{A\{}{B}after"), r"Bafter");
    }

    #[test]
    fn packages_and_classes() {
        assert_eq!(
            rc(
                r"\usepackage{a, b ,c}\@ifpackageloaded{b}{Y}{N}\IfPackageLoadedTF{c}{Y}{N}\@ifpackageloaded{d}{Y}{N}"
            ),
            r"\usepackage{a, b ,c}YYN"
        );
        assert_eq!(
            rc(r"\documentclass{amsbook}\@ifclassloaded{amsbook}{Y}{N}\@ifundefined{zz}{A}{B}"),
            r"\documentclass{amsbook}YA"
        );
    }

    #[test]
    fn kept_text_is_not_glued_to_a_control_word() {
        assert_eq!(rc(r"\foo\iftrue bar\fi"), r"\foo bar");
        assert_eq!(rc(r"\foo\iftrue 日本\fi"), r"\foo 日本");
        assert_eq!(rc(r"\x\iftrue ébar\fi"), r"\x ébar");
        assert_eq!(rc(r"\textbf{}\iftrue X\fi"), r"\textbf{} X");
    }

    #[test]
    fn verbatim_is_untouched() {
        assert_eq!(
            rc("\\begin{verbatim}\n\\iftrue A\\else B\\fi\n\\end{verbatim} \\verb|\\iffalse| C"),
            "\\begin{verbatim}\n\\iftrue A\\else B\\fi\n\\end{verbatim} \\verb|\\iffalse| C"
        );
        assert_eq!(
            rc(r"\begin{comment}\iftrue\end{comment}\iffalse A\fi B"),
            r"\begin{comment}\iftrue\end{comment} B"
        );
    }

    #[test]
    fn non_ascii_and_unbalanced() {
        assert_eq!(rc(r"é \iftrue ü\else 日本\fi 中文"), r"é  ü 中文");
        assert_eq!(rc(r""), r"");
        assert_eq!(rc(r"\iftrue never closed"), r"\iftrue never closed");
        assert_eq!(
            rc(r"\ifthenelse{\boolean{x}}{A}"),
            r"\ifthenelse{\boolean{x}}{A}"
        );
        assert_eq!(rc(r"\fi\iftrue x\fi"), r"\fi x");
    }

    #[test]
    fn file_tests_look_in_base_dir() {
        let dir = std::env::temp_dir().join(format!("t2w-cond-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join("adir.tex")).unwrap(); // a directory
        for f in ["here.tex", "sub/x.tex", "é.tex"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        let src = concat!(
            r"\IfFileExists{here}{F}{G} \IfFileExists{gone.tex}{H}{I} \IfFileExists{sub/x}{J}{K}",
            r" \IfFileExists{adir}{L}{M} \InputIfFileExists{é}{N}{O}"
        );
        let out = resolve_conditionals(src, dir.to_str().unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(out, "F I J M N");
    }
}
