//! LaTeX math -> math AST (port of `mathml/latex_math.py`).
//!
//! A small recursive-descent parser over the (macro-expanded) math token
//! stream, modelling the common + AMS math core. Anything it can't model is
//! a [`MathError::Unsupported`], which drives the fallback cascade.

use std::fmt;

use tex2word_ir::pystr;

use crate::symbols as S;

/// A failure of the direct LaTeX -> OMML path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MathError {
    /// `MathUnsupported(construct, detail)`
    Unsupported { construct: String, detail: String },
    /// any other Python exception (`type(exc).__name__`, `str(exc)`)
    Other { type_name: String, message: String },
}

impl MathError {
    pub fn unsupported(construct: impl Into<String>, detail: impl Into<String>) -> Self {
        MathError::Unsupported {
            construct: construct.into(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MathError::Unsupported { construct, detail } => {
                write!(
                    f,
                    "{}",
                    format!("unsupported math construct: {construct} {detail}").trim()
                )
            }
            MathError::Other { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for MathError {}

type R<T> = Result<T, MathError>;

/// A leaf run of math text with styling hints.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lit {
    pub text: String,
    pub upright: bool,
    pub bold: bool,
    /// OMML `m:scr`: script/double-struck/fraktur/...
    pub script: Option<String>,
}

impl Lit {
    pub fn new(text: impl Into<String>) -> Self {
        Lit {
            text: text.into(),
            ..Lit::default()
        }
    }

    pub fn upright(text: impl Into<String>) -> Self {
        Lit {
            text: text.into(),
            upright: true,
            ..Lit::default()
        }
    }
}

/// A sequence of nodes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row(pub Vec<MNode>);

/// An n-ary operator with optional limits.
#[derive(Debug, Clone, PartialEq)]
pub struct Nary {
    pub op: String,
    pub body: Option<Box<MNode>>,
    pub sub: Option<Box<MNode>>,
    pub sup: Option<Box<MNode>>,
    /// `undOvr` or `subSup`
    pub limloc: String,
}

/// The math AST.
#[derive(Debug, Clone, PartialEq)]
pub enum MNode {
    Lit(Lit),
    Row(Row),
    Group(Row),
    Frac(Row, Row),
    Sup(Box<MNode>, Box<MNode>),
    Sub(Box<MNode>, Box<MNode>),
    SubSup(Box<MNode>, Box<MNode>, Box<MNode>),
    Sqrt(Row),
    /// (index, radicand)
    Root(Row, Row),
    Nary(Nary),
    /// (base, accent char)
    Accent(Row, String),
    /// (open, close, body)
    Fenced(String, String, Row),
    /// function/operator with a lower limit, e.g. lim_{x\to0}
    LimLow(Box<MNode>, Box<MNode>),
    /// operator with an upper limit, e.g. \overset{a}{=}
    LimUpp(Box<MNode>, Box<MNode>),
    /// pre-scripts (m:sPre): (base, sub, sup)
    SPre(Box<MNode>, Option<Box<MNode>>, Option<Box<MNode>>),
    /// a boxed expression (m:borderBox)
    BorderBox(Row),
    /// an over/under brace group: (body, char, pos "top"/"bot")
    GroupChr(Row, String, String),
    /// rows of cells; `aligned` renders column-justified
    Matrix(Vec<Vec<Row>>, bool),
}

/// Functions whose subscript is rendered as an underscript limit.
fn is_limit_func(s: &str) -> bool {
    matches!(
        s,
        "lim" | "limsup" | "liminf" | "max" | "min" | "sup" | "inf" | "gcd" | "det"
    )
}

/// \big \Big \bigg \Bigg (+ l/r/m variants): manual delimiter sizing.
///
/// Returns (size, suffix); the sizing itself is visual-only, but the `l`/`r`
/// suffixes mark opening/closing delimiters that pair into an `m:d`.
fn big_delim(name: &str) -> Option<(&str, &str)> {
    for size in ["bigg", "Bigg", "big", "Big"] {
        if let Some(rest) = name.strip_prefix(size) {
            if matches!(rest, "" | "l" | "r" | "m") {
                return Some((size, rest));
            }
        }
    }
    None
}

/// `\not <cmd>`: the precomposed negated glyph for common relations.
fn negation(name: &str) -> Option<&'static str> {
    Some(match name {
        "le" | "leq" | "leqslant" => "≰",
        "ge" | "geq" | "geqslant" => "≱",
        "sim" => "≁",
        "cong" => "≇",
        "equiv" => "≢",
        "approx" => "≉",
        "in" => "∉",
        "ni" | "owns" => "∌",
        "subset" => "⊄",
        "supset" => "⊅",
        "subseteq" => "⊈",
        "supseteq" => "⊉",
        "mid" | "shortmid" => "∤",
        "parallel" | "shortparallel" => "∦",
        "preceq" => "⋠",
        "succeq" => "⋡",
        "models" | "vDash" => "⊭",
        _ => return None,
    })
}

/// `\not <char>`: precomposed negations for the single-char relations.
fn negation_char(c: char) -> Option<&'static str> {
    Some(match c {
        '<' => "≮",
        '>' => "≯",
        '=' => "≠",
        _ => return None,
    })
}

/// Escaped literals inside math: \{ \} \% \# \& \$ \_ and \| (norm bar).
fn cmd_literal(name: &str) -> Option<&'static str> {
    Some(match name {
        "{" => "{",
        "}" => "}",
        "|" => "‖",
        "%" => "%",
        "#" => "#",
        "&" => "&",
        "$" => "$",
        "_" => "_",
        _ => return None,
    })
}

/// Old math font declarations (`{\rm Lik}`): (upright, bold, script).
fn math_style_decl(name: &str) -> Option<(bool, bool, Option<&'static str>)> {
    Some(match name {
        "rm" | "sf" | "tt" | "sc" => (true, false, None),
        "bf" => (false, true, None),
        "it" | "mit" => (false, false, None),
        "cal" => (false, false, Some("script")),
        _ => return None,
    })
}

/// mathtools/physics paired-delimiter macros: \abs{x} -> |x| etc.
fn paired_delim(name: &str) -> Option<(&'static str, &'static str)> {
    Some(match name {
        "abs" => ("|", "|"),
        "norm" => ("‖", "‖"),
        "ceil" => ("⌈", "⌉"),
        "floor" => ("⌊", "⌋"),
        "round" => ("⌊", "⌉"),
        "set" | "Set" => ("{", "}"),
        "ket" => ("|", "⟩"),
        "bra" => ("⟨", "|"),
        "braket" | "ip" | "inner" => ("⟨", "⟩"),
        _ => return None,
    })
}

/// Extensible (stretchy) arrows: \xrightarrow[under]{over} etc.
fn xarrow(name: &str) -> Option<&'static str> {
    Some(match name {
        "xrightarrow" => "→",
        "xleftarrow" => "←",
        "xRightarrow" => "⇒",
        "xLeftarrow" => "⇐",
        "xleftrightarrow" => "↔",
        "xLeftrightarrow" => "⇔",
        "xhookrightarrow" => "↪",
        "xhookleftarrow" => "↩",
        "xmapsto" => "↦",
        "xrightharpoonup" => "⇀",
        "xrightleftharpoons" => "⇌",
        _ => return None,
    })
}

fn matrix_delims(env: &str) -> Option<(&'static str, &'static str)> {
    Some(match env {
        "matrix" | "aligned" | "array" | "smallmatrix" | "align" | "alignat" | "flalign"
        | "gather" | "gathered" | "split" | "multlined" | "eqnarray" => ("", ""),
        "pmatrix" => ("(", ")"),
        "bmatrix" => ("[", "]"),
        "Bmatrix" => ("{", "}"),
        "vmatrix" => ("|", "|"),
        "Vmatrix" => ("‖", "‖"),
        "cases" => ("{", ""),
        _ => return None,
    })
}

/// Environments whose `&` is an alignment marker.
fn is_aligned_env(env: &str) -> bool {
    matches!(
        env,
        "aligned" | "align" | "alignat" | "flalign" | "split" | "eqnarray"
    )
}

// --------------------------------------------------------------------------- //
// Tokenizer
// --------------------------------------------------------------------------- //

/// A token: (type, value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Cmd(String),
    Newline,
    LBrace,
    RBrace,
    Caret,
    Underscore,
    Amp,
    Num(String),
    Char(char),
    /// a blank; ignored in math mode, significant inside `\text{...}`
    Space,
    Eof,
}

impl Tok {
    fn kind(&self) -> &'static str {
        match self {
            Tok::Cmd(_) => "cmd",
            Tok::Newline => "newline",
            Tok::LBrace => "{",
            Tok::RBrace => "}",
            Tok::Caret => "^",
            Tok::Underscore => "_",
            Tok::Amp => "&",
            Tok::Num(_) => "num",
            Tok::Char(_) => "char",
            Tok::Space => "space",
            Tok::Eof => "eof",
        }
    }
}

/// Past the `*` and `[<dimen>]` that may follow `\\` (from `at`).
fn skip_row_spacing(chars: &[char], at: usize) -> usize {
    let mut i = at;
    if chars.get(i) == Some(&'*') {
        i += 1;
    }
    let mut k = i;
    while chars.get(k) == Some(&' ') {
        k += 1;
    }
    if chars.get(k) == Some(&'[') {
        if let Some(close) = chars[k..].iter().position(|&c| c == ']') {
            let inner: String = chars[k + 1..k + close].iter().collect();
            if is_dimen(&inner) {
                return k + close + 1;
            }
        }
    }
    i
}

/// A TeX length such as `2pt`, `-.5ex`, `1.2em` or `\jot`.
pub fn is_dimen(s: &str) -> bool {
    let t = s.trim();
    if let Some(name) = t.strip_prefix('\\') {
        return !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic());
    }
    let num_end = t
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | ' ')))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(num_end);
    num.chars().any(|c| c.is_ascii_digit())
        && matches!(
            unit.trim(),
            "pt" | "em" | "ex" | "mm" | "cm" | "in" | "bp" | "pc" | "sp" | "mu" | "dd" | "cc"
        )
}

pub fn tokenize(s: &str) -> Vec<Tok> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c == '\\' {
            let j = i + 1;
            if j < n && !pystr::is_alpha(chars[j]) {
                // control symbol: \{ \} \, \\ \; \! \| etc.
                if chars[j] == '\\' {
                    toks.push(Tok::Newline);
                    // `\\*` and the row-spacing argument `\\[2pt]` are layout
                    i = skip_row_spacing(&chars, j + 1);
                    continue;
                } else {
                    toks.push(Tok::Cmd(chars[j].to_string()));
                }
                i = j + 1;
            } else {
                let mut k = j;
                while k < n && pystr::is_alpha(chars[k]) {
                    k += 1;
                }
                toks.push(Tok::Cmd(chars[j..k].iter().collect()));
                i = k;
                while i < n && chars[i] == ' ' {
                    i += 1;
                }
            }
        } else if c == '{' {
            toks.push(Tok::LBrace);
            i += 1;
        } else if c == '}' {
            toks.push(Tok::RBrace);
            i += 1;
        } else if c == '^' {
            toks.push(Tok::Caret);
            i += 1;
        } else if c == '_' {
            toks.push(Tok::Underscore);
            i += 1;
        } else if c == '&' {
            toks.push(Tok::Amp);
            i += 1;
        } else if c == ' ' {
            toks.push(Tok::Space);
            i += 1;
        } else if matches!(c, '\n' | '\t') {
            i += 1;
        } else if pystr::is_digit(c) {
            let mut k = i;
            while k < n && (pystr::is_digit(chars[k]) || chars[k] == '.') {
                k += 1;
            }
            toks.push(Tok::Num(chars[i..k].iter().collect()));
            i = k;
        } else {
            toks.push(Tok::Char(c));
            i += 1;
        }
    }
    toks
}

// --------------------------------------------------------------------------- //
// Parser
// --------------------------------------------------------------------------- //

/// Nesting beyond this raises like Python's `RecursionError`.
const MAX_DEPTH: usize = 300;

struct Parser {
    toks: Vec<Tok>,
    i: usize,
    depth: usize,
}

const STOP_NONE: &[&str] = &[];

impl Parser {
    fn peek(&self) -> Tok {
        self.toks.get(self.i).cloned().unwrap_or(Tok::Eof)
    }

    fn next(&mut self) -> Tok {
        match self.toks.get(self.i) {
            Some(t) => {
                self.i += 1;
                t.clone()
            }
            None => Tok::Eof,
        }
    }

    fn expect(&mut self, kind: &str) -> R<()> {
        if self.peek().kind() != kind {
            return Err(MathError::unsupported("syntax", format!("expected {kind}")));
        }
        self.next();
        Ok(())
    }

    fn enter(&mut self) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(MathError::Other {
                type_name: "RecursionError".into(),
                message: "maximum recursion depth exceeded".into(),
            });
        }
        Ok(())
    }

    // -- rows ------------------------------------------------------------- //

    fn parse_row(&mut self, stop: &[&str]) -> R<Row> {
        self.enter()?;
        let mut items = Vec::new();
        loop {
            let k = self.peek().kind();
            if k == "}" || k == "eof" || stop.contains(&k) {
                break;
            }
            let Some(atom) = self.parse_atom()? else {
                continue;
            };
            items.push(self.maybe_scripts(atom)?);
        }
        self.depth -= 1;
        Ok(Row(items))
    }

    /// A braced group or a single atom (command arguments).
    fn parse_group(&mut self) -> R<Row> {
        if self.peek() == Tok::LBrace {
            self.next();
            let row = self.parse_row(STOP_NONE)?;
            self.expect("}")?;
            return Ok(row);
        }
        Ok(Row(self.parse_atom()?.into_iter().collect()))
    }

    /// A `\text{...}`-style group: blanks between atoms become blank runs.
    fn parse_text_group(&mut self) -> R<Vec<MNode>> {
        if self.peek() != Tok::LBrace {
            return Ok(self.parse_atom()?.into_iter().collect());
        }
        self.next();
        let mut items = Vec::new();
        loop {
            match self.peek() {
                Tok::RBrace | Tok::Eof => break,
                Tok::Space => {
                    self.next();
                    items.push(MNode::Lit(Lit::upright(" ")));
                    continue;
                }
                _ => {}
            }
            if let Some(atom) = self.parse_atom()? {
                items.push(self.maybe_scripts(atom)?);
            }
        }
        self.expect("}")?;
        text_hyphens(&mut items);
        Ok(items)
    }

    /// A group holding only `_{x}^{y}` scripts on an implicit empty base
    /// (the two arguments of `\sideset`).
    fn parse_scripts_group(&mut self) -> R<(Option<MNode>, Option<MNode>)> {
        if self.peek() == Tok::LBrace {
            self.next();
        }
        let mut sub = None;
        let mut sup = None;
        loop {
            match self.peek() {
                Tok::Underscore => {
                    self.next();
                    sub = Some(flatten(self.parse_group()?));
                }
                Tok::Caret => {
                    self.next();
                    sup = Some(flatten(self.parse_group()?));
                }
                _ => break,
            }
        }
        if self.peek() == Tok::RBrace {
            self.next();
        }
        Ok((sub, sup))
    }

    fn maybe_scripts(&mut self, base: MNode) -> R<MNode> {
        let mut sub: Option<MNode> = None;
        let mut sup: Option<MNode> = None;
        loop {
            match self.peek() {
                Tok::Caret => {
                    self.next();
                    sup = Some(flatten(self.parse_group()?));
                }
                Tok::Underscore => {
                    self.next();
                    sub = Some(flatten(self.parse_group()?));
                }
                _ => break,
            }
        }
        if let MNode::Nary(mut n) = base {
            n.sub = sub.map(Box::new);
            n.sup = sup.map(Box::new);
            return Ok(MNode::Nary(n));
        }
        if let MNode::Lit(l) = &base {
            if l.upright && is_limit_func(&l.text) && sup.is_none() {
                if let Some(sub) = sub {
                    return Ok(MNode::LimLow(Box::new(base), Box::new(sub)));
                }
            }
        }
        Ok(match (sub, sup) {
            (Some(b), Some(p)) => MNode::SubSup(Box::new(base), Box::new(b), Box::new(p)),
            (Some(b), None) => MNode::Sub(Box::new(base), Box::new(b)),
            (None, Some(p)) => MNode::Sup(Box::new(base), Box::new(p)),
            (None, None) => base,
        })
    }

    // -- atoms ------------------------------------------------------------ //

    fn parse_atom(&mut self) -> R<Option<MNode>> {
        self.enter()?;
        let t = self.next();
        let out = match t {
            Tok::LBrace => {
                let row = self.parse_row(STOP_NONE)?;
                self.expect("}")?;
                Some(MNode::Group(row))
            }
            Tok::Num(v) => Some(MNode::Lit(Lit::upright(v))),
            Tok::Char(c) => Some(char_atom(c)),
            Tok::Cmd(name) => Some(self.parse_command(&name)?),
            Tok::Caret | Tok::Underscore => {
                // script with no preceding base
                self.i -= 1;
                Some(MNode::Lit(Lit::new("")))
            }
            Tok::Newline | Tok::Amp => {
                return Err(MathError::unsupported("alignment", "stray & or \\\\"))
            }
            // blanks are skipped in math mode (kept only by parse_text_group)
            _ => None,
        };
        self.depth -= 1;
        Ok(out)
    }

    fn parse_command(&mut self, name: &str) -> R<MNode> {
        if let Some(acc) = S::get(S::ACCENTS, name) {
            return Ok(MNode::Accent(self.parse_group()?, acc.into()));
        }
        if matches!(name, "frac" | "dfrac" | "tfrac" | "cfrac") {
            let num = self.parse_group()?;
            let den = self.parse_group()?;
            return Ok(MNode::Frac(num, den));
        }
        if name == "overbrace" || name == "overbracket" {
            return Ok(MNode::GroupChr(
                self.parse_group()?,
                "⏞".into(),
                "top".into(),
            ));
        }
        if name == "underbrace" || name == "underbracket" {
            return Ok(MNode::GroupChr(
                self.parse_group()?,
                "⏟".into(),
                "bot".into(),
            ));
        }
        if matches!(name, "binom" | "dbinom" | "tbinom") {
            let a = self.parse_group()?;
            let b = self.parse_group()?;
            let inner = MNode::Matrix(vec![vec![a], vec![b]], false);
            return Ok(MNode::Fenced("(".into(), ")".into(), Row(vec![inner])));
        }
        if name == "sqrt" {
            if self.peek() == Tok::Char('[') {
                let index = self.parse_optional()?;
                return Ok(MNode::Root(index, self.parse_group()?));
            }
            return Ok(MNode::Sqrt(self.parse_group()?));
        }
        if let Some(op) = S::get(S::NARY, name) {
            let limloc = if matches!(name, "int" | "iint" | "iiint" | "oint") {
                "subSup"
            } else {
                "undOvr"
            };
            return Ok(MNode::Nary(Nary {
                op: op.into(),
                body: None,
                sub: None,
                sup: None,
                limloc: limloc.into(),
            }));
        }
        if let Some(l) = cmd_literal(name) {
            return Ok(MNode::Lit(Lit::upright(l)));
        }
        if let Some((_, suffix)) = big_delim(name) {
            match suffix {
                // an opening `\bigl <delim>` pairs with a closing `\bigr`/`\right`
                "l" => return self.parse_fenced(),
                "r" => return Err(MathError::unsupported("delimiter", "stray \\bigr")),
                // \big / \bigm: sizing is visual-only, the delimiter renders itself
                _ => return Ok(MNode::Lit(Lit::upright(self.read_delim()?))),
            }
        }
        if let Some((open, close)) = paired_delim(name) {
            if self.peek() == Tok::Char('*') {
                self.next(); // \abs*{x} auto-size form
            }
            return Ok(MNode::Fenced(
                open.into(),
                close.into(),
                self.parse_group()?,
            ));
        }
        if matches!(name, "dv" | "odv" | "pdv" | "dd") {
            return self.parse_derivative(name);
        }
        if let Some(arrow) = xarrow(name) {
            return self.parse_xarrow(arrow);
        }
        if name == "overset" || name == "stackrel" {
            let top = self.parse_group()?;
            let base = self.parse_group()?;
            return Ok(MNode::LimUpp(
                Box::new(flatten(base)),
                Box::new(flatten(top)),
            ));
        }
        if name == "underset" {
            let bot = self.parse_group()?;
            let base = self.parse_group()?;
            return Ok(MNode::LimLow(
                Box::new(flatten(base)),
                Box::new(flatten(bot)),
            ));
        }
        if name == "overunderset" {
            let top = self.parse_group()?;
            let bot = self.parse_group()?;
            let base = self.parse_group()?;
            return Ok(MNode::LimUpp(
                Box::new(MNode::LimLow(
                    Box::new(flatten(base)),
                    Box::new(flatten(bot)),
                )),
                Box::new(flatten(top)),
            ));
        }
        if name == "boxed" {
            return Ok(MNode::BorderBox(self.parse_group()?));
        }
        if name == "genfrac" {
            let open = row_text(&self.parse_group()?);
            let close = row_text(&self.parse_group()?);
            self.parse_group()?; // rule thickness
            self.parse_group()?; // math style
            let frac = MNode::Frac(self.parse_group()?, self.parse_group()?);
            if open.is_empty() && close.is_empty() {
                return Ok(frac);
            }
            return Ok(MNode::Fenced(open, close, Row(vec![frac])));
        }
        if name == "prescript" {
            let sub = flatten(self.parse_group()?);
            let sup = flatten(self.parse_group()?);
            let base = flatten(self.parse_group()?);
            return Ok(MNode::SPre(
                Box::new(base),
                Some(Box::new(sub)),
                Some(Box::new(sup)),
            ));
        }
        if name == "sideset" {
            let (lsub, lsup) = self.parse_scripts_group()?;
            let (rsub, rsup) = self.parse_scripts_group()?;
            let Some(op) = self.parse_atom()? else {
                return Err(MathError::unsupported("sideset", "missing operator"));
            };
            let base = match (op, rsub, rsup) {
                (MNode::Nary(mut n), s, p) => {
                    n.sub = s.map(Box::new);
                    n.sup = p.map(Box::new);
                    MNode::Nary(n)
                }
                (b, Some(s), Some(p)) => MNode::SubSup(Box::new(b), Box::new(s), Box::new(p)),
                (b, Some(s), None) => MNode::Sub(Box::new(b), Box::new(s)),
                (b, None, Some(p)) => MNode::Sup(Box::new(b), Box::new(p)),
                (b, None, None) => b,
            };
            return Ok(MNode::SPre(
                Box::new(base),
                lsub.map(Box::new),
                lsup.map(Box::new),
            ));
        }
        if name == "pod" {
            return Ok(MNode::Fenced("(".into(), ")".into(), self.parse_group()?));
        }
        if name == "smash" {
            if self.peek() == Tok::Char('[') {
                self.parse_optional()?; // [t]/[b]/[tb]: vertical extent only
            }
            return Ok(MNode::Group(self.parse_group()?));
        }
        if name == "idotsint" {
            return Ok(MNode::Row(Row(vec![
                MNode::Lit(Lit::upright("∫")),
                MNode::Lit(Lit::upright("⋯")),
                MNode::Lit(Lit::upright("∫")),
            ])));
        }
        if name == "not" {
            let neg = match self.peek() {
                Tok::Cmd(v) => negation(&v).map(str::to_string),
                Tok::Char(c) => negation_char(c).map(str::to_string),
                _ => None,
            };
            if let Some(glyph) = neg {
                self.next();
                return Ok(MNode::Lit(Lit::upright(glyph)));
            }
            // no precomposed glyph: overlay a combining long solidus
            return Ok(MNode::Accent(self.parse_group()?, "\u{338}".into()));
        }
        if matches!(
            name,
            "text"
                | "mathrm"
                | "operatorname"
                | "operatorname*"
                | "mathsf"
                | "mathtt"
                | "mbox"
                | "textrm"
                | "textnormal"
                | "textsf"
                | "texttt"
                | "textsc"
        ) {
            return Ok(styled(Row(self.parse_text_group()?), true, false, None));
        }
        if name == "textbf" {
            return Ok(styled(Row(self.parse_text_group()?), true, true, None));
        }
        if matches!(name, "textit" | "textsl" | "emph") {
            return Ok(MNode::Group(Row(self.parse_text_group()?)));
        }
        if matches!(name, "mathbf" | "boldsymbol" | "bm") {
            return Ok(styled(self.parse_group()?, false, true, None));
        }
        if name == "mathbb" {
            return Ok(styled(
                self.parse_group()?,
                true,
                false,
                Some("double-struck"),
            ));
        }
        if name == "mathcal" || name == "mathscr" {
            return Ok(styled(self.parse_group()?, false, false, Some("script")));
        }
        if name == "mathfrak" {
            return Ok(styled(self.parse_group()?, false, false, Some("fraktur")));
        }
        if name == "mathit" {
            return Ok(MNode::Group(self.parse_group()?));
        }
        if matches!(
            name,
            "mathbin"
                | "mathrel"
                | "mathop"
                | "mathord"
                | "mathopen"
                | "mathclose"
                | "mathpunct"
                | "mathinner"
        ) {
            // math-class wrappers only affect spacing -> render their content
            return Ok(MNode::Group(self.parse_group()?));
        }
        if name == "left" {
            return self.parse_fenced();
        }
        if name == "right" {
            return Err(MathError::unsupported("delimiter", "stray \\right"));
        }
        if name == "substack" {
            return self.parse_substack();
        }
        if name == "bmod" {
            // infix modulo: a \bmod n -> "a mod n"
            return Ok(MNode::Lit(Lit::upright("\u{2009}mod\u{2009}")));
        }
        if name == "pmod" {
            // \pmod{n} -> " (mod n)"
            let mut items = vec![MNode::Lit(Lit::upright("mod\u{2009}"))];
            items.push(MNode::Row(self.parse_group()?));
            let mod_row = Row(items);
            return Ok(MNode::Row(Row(vec![
                MNode::Lit(Lit::upright("\u{2009}")),
                MNode::Fenced("(".into(), ")".into(), mod_row),
            ])));
        }
        if S::is_function(name) {
            return Ok(MNode::Lit(Lit::upright(name)));
        }
        if let Some(v) = S::get(S::MATHBB, name) {
            return Ok(MNode::Lit(Lit::upright(v)));
        }
        if let Some(v) = S::get(S::SPACING, name) {
            return Ok(MNode::Lit(Lit::upright(v)));
        }
        if let Some(v) = S::get(S::SYMBOLS, name) {
            return Ok(MNode::Lit(Lit::upright(v)));
        }
        // bare delimiter commands (\lvert, \lceil, …) render as their char
        if let Some(d) = S::get(S::DELIMITERS, name) {
            return Ok(MNode::Lit(Lit::upright(d)));
        }
        if name == "begin" {
            return self.parse_environment();
        }
        if name == "end" {
            return Err(MathError::unsupported("environment", "stray \\end"));
        }
        if matches!(
            name,
            "displaystyle" | "textstyle" | "scriptstyle" | "limits" | "nolimits"
        ) {
            return Ok(MNode::Lit(Lit::new(""))); // styling hints we ignore
        }
        if matches!(name, "nonumber" | "notag" | "qedhere") {
            return Ok(MNode::Lit(Lit::new("")));
        }
        if matches!(
            name,
            "hline" | "toprule" | "midrule" | "bottomrule" | "hdashline"
        ) {
            return Ok(MNode::Lit(Lit::new(""))); // array rules: no OMML equivalent
        }
        if matches!(name, "cline" | "cmidrule" | "noalign") {
            // \cmidrule(lr){2-3}: consume an optional (l/r/lr) trim modifier
            if self.peek() == Tok::Char('(') {
                while self.peek() != Tok::Eof && self.next() != Tok::Char(')') {}
            }
            if self.peek() == Tok::LBrace {
                self.parse_group()?;
            }
            return Ok(MNode::Lit(Lit::new("")));
        }
        if let Some((upright, bold, script)) = math_style_decl(name) {
            // declaration form {\rm ...}: style the rest of the current group
            let row = self.parse_row(STOP_NONE)?;
            return Ok(styled(row, upright, bold, script));
        }
        Err(MathError::unsupported(format!("\\{name}"), ""))
    }

    /// physics `\dv`/`\pdv`/`\odv`/`\dd` derivatives.
    fn parse_derivative(&mut self, name: &str) -> R<MNode> {
        let d = if name == "pdv" { "∂" } else { "d" };
        if name == "dd" {
            let mut lead = vec![MNode::Lit(Lit::upright(d))];
            if self.peek() == Tok::LBrace {
                lead.extend(self.parse_group()?.0);
            }
            return Ok(MNode::Group(Row(lead)));
        }
        let first = self.parse_group()?;
        let second = if self.peek() == Tok::LBrace {
            Some(self.parse_group()?)
        } else {
            None
        };
        let (num, den) = match second {
            Some(second) => {
                let mut num = vec![MNode::Lit(Lit::upright(d))];
                num.extend(first.0);
                let mut den = vec![MNode::Lit(Lit::upright(d))];
                den.extend(second.0);
                (Row(num), Row(den))
            }
            None => {
                let mut den = vec![MNode::Lit(Lit::upright(d))];
                den.extend(first.0);
                (Row(vec![MNode::Lit(Lit::upright(d))]), Row(den))
            }
        };
        Ok(MNode::Frac(num, den))
    }

    /// `\xrightarrow[under]{over}` -> a labelled arrow.
    fn parse_xarrow(&mut self, arrow: &str) -> R<MNode> {
        let under = if self.peek() == Tok::Char('[') {
            Some(self.parse_optional()?)
        } else {
            None
        };
        let over = self.parse_group()?;
        let base = MNode::Lit(Lit::upright(arrow));
        if let Some(under) = under.filter(|u| !u.0.is_empty()) {
            return Ok(MNode::SubSup(
                Box::new(base),
                Box::new(flatten(under)),
                Box::new(flatten(over)),
            ));
        }
        Ok(MNode::Sup(Box::new(base), Box::new(flatten(over))))
    }

    /// A `[...]` optional argument.
    fn parse_optional(&mut self) -> R<Row> {
        self.next(); // the '['
        let mut items = Vec::new();
        loop {
            match self.peek() {
                Tok::Char(']') => {
                    self.next();
                    break;
                }
                Tok::Eof => break,
                _ => {}
            }
            if let Some(atom) = self.parse_atom()? {
                items.push(self.maybe_scripts(atom)?);
            }
        }
        Ok(Row(items))
    }

    fn read_delim(&mut self) -> R<String> {
        match self.next() {
            Tok::Char(c) => {
                let s = c.to_string();
                Ok(S::get(S::DELIMITERS, &s).map_or(s.clone(), str::to_string))
            }
            Tok::Cmd(v) => S::get(S::DELIMITERS, &v)
                .or_else(|| S::get(S::SYMBOLS, &v))
                .map(str::to_string)
                .ok_or_else(|| MathError::unsupported(format!("delimiter \\{v}"), "")),
            _ => Err(MathError::unsupported("delimiter", "")),
        }
    }

    fn parse_fenced(&mut self) -> R<MNode> {
        let open = self.read_delim()?;
        let mut items = Vec::new();
        loop {
            match self.peek() {
                Tok::Eof => return Err(MathError::unsupported("delimiter", "unclosed \\left")),
                Tok::Cmd(v)
                    if v == "right" || big_delim(&v).is_some_and(|(_, suffix)| suffix == "r") =>
                {
                    self.next();
                    let close = self.read_delim()?;
                    return Ok(MNode::Fenced(open, close, Row(items)));
                }
                _ => {}
            }
            if let Some(atom) = self.parse_atom()? {
                items.push(self.maybe_scripts(atom)?);
            }
        }
    }

    /// `\substack{a \\ b \\ c}` -> a single-column matrix.
    fn parse_substack(&mut self) -> R<MNode> {
        if self.peek() == Tok::LBrace {
            self.next();
        }
        let mut rows: Vec<Vec<Row>> = vec![Vec::new()];
        let mut current: Vec<MNode> = Vec::new();
        loop {
            match self.peek() {
                Tok::RBrace | Tok::Eof => break,
                Tok::Newline => {
                    self.next();
                    rows.last_mut()
                        .unwrap()
                        .push(Row(std::mem::take(&mut current)));
                    rows.push(Vec::new());
                    continue;
                }
                _ => {}
            }
            if let Some(atom) = self.parse_atom()? {
                current.push(self.maybe_scripts(atom)?);
            }
        }
        rows.last_mut().unwrap().push(Row(current));
        if self.peek() == Tok::RBrace {
            self.next();
        }
        if rows
            .last()
            .is_some_and(|r| r.iter().all(|c| c.0.is_empty()))
        {
            rows.pop();
        }
        Ok(MNode::Matrix(rows, false))
    }

    fn parse_environment(&mut self) -> R<MNode> {
        let name_row = self.parse_group()?;
        let env = row_text(&name_row);
        let env = env.trim_end_matches('*').to_string();
        let Some((open, close)) = matrix_delims(&env) else {
            return Err(MathError::unsupported(format!("environment {env}"), ""));
        };
        if (env == "array" || env == "alignat") && self.peek() == Tok::LBrace {
            self.parse_group()?; // the column spec / column count
        }
        let mut rows: Vec<Vec<Row>> = vec![Vec::new()];
        let mut current: Vec<MNode> = Vec::new();
        loop {
            match self.peek() {
                Tok::Eof => {
                    return Err(MathError::unsupported(
                        "environment",
                        format!("unclosed {env}"),
                    ))
                }
                Tok::Cmd(v) if v == "end" => {
                    self.next();
                    self.parse_group()?; // {env}
                    rows.last_mut()
                        .unwrap()
                        .push(Row(std::mem::take(&mut current)));
                    break;
                }
                Tok::Amp => {
                    self.next();
                    rows.last_mut()
                        .unwrap()
                        .push(Row(std::mem::take(&mut current)));
                    continue;
                }
                Tok::Newline => {
                    self.next();
                    rows.last_mut()
                        .unwrap()
                        .push(Row(std::mem::take(&mut current)));
                    rows.push(Vec::new());
                    continue;
                }
                Tok::Cmd(v) if v == "cr" => {
                    // plain-TeX \cr ends a row too
                    self.next();
                    rows.last_mut()
                        .unwrap()
                        .push(Row(std::mem::take(&mut current)));
                    rows.push(Vec::new());
                    continue;
                }
                _ => {}
            }
            if let Some(atom) = self.parse_atom()? {
                current.push(self.maybe_scripts(atom)?);
            }
        }
        // drop a trailing empty row (from a final \\)
        if rows
            .last()
            .is_some_and(|r| r.iter().all(|c| c.0.is_empty()))
        {
            rows.pop();
        }
        let matrix = MNode::Matrix(rows, is_aligned_env(&env));
        if !open.is_empty() || !close.is_empty() {
            return Ok(MNode::Fenced(open.into(), close.into(), Row(vec![matrix])));
        }
        Ok(matrix)
    }
}

fn char_atom(c: char) -> MNode {
    if pystr::is_alpha(c) {
        MNode::Lit(Lit::new(c.to_string())) // italic by default in Word math
    } else if c == '-' {
        // TeX typesets `-` in math as the minus sign, not a hyphen
        MNode::Lit(Lit::upright("\u{2212}"))
    } else {
        MNode::Lit(Lit::upright(c.to_string()))
    }
}

/// Apply styling to every Lit leaf of a parsed group.
fn styled(mut row: Row, upright: bool, bold: bool, script: Option<&str>) -> MNode {
    for item in row.0.iter_mut() {
        apply_style(item, upright, bold, script);
    }
    MNode::Group(merge_lits(row))
}

/// Join neighbouring leaves of the same style into one run, so `\mathrm{Beta}`
/// or an `\operatorname{rel}` is one word, not four separately spaced letters.
fn merge_lits(row: Row) -> Row {
    let mut out: Vec<MNode> = Vec::with_capacity(row.0.len());
    for node in row.0 {
        if let (Some(MNode::Lit(prev)), MNode::Lit(cur)) = (out.last_mut(), &node) {
            if prev.upright == cur.upright
                && prev.bold == cur.bold
                && prev.script == cur.script
                && !prev.text.is_empty()
            {
                prev.text.push_str(&cur.text);
                continue;
            }
        }
        out.push(node);
    }
    Row(out)
}

/// Text inside `\text{...}` & co.: a `-` there is a hyphen.
fn text_hyphens(items: &mut [MNode]) {
    for item in items {
        match item {
            MNode::Lit(l) if l.text.contains('\u{2212}') => {
                l.text = l.text.replace('\u{2212}', "-");
            }
            MNode::Group(r) | MNode::Row(r) => text_hyphens(&mut r.0),
            _ => {}
        }
    }
}

fn apply_style(node: &mut MNode, upright: bool, bold: bool, script: Option<&str>) {
    match node {
        MNode::Lit(l) => {
            if upright {
                l.upright = true;
            }
            if bold {
                l.bold = true;
            }
            if let Some(s) = script.filter(|s| !s.is_empty()) {
                l.script = Some(s.to_string());
            }
        }
        MNode::Group(r) | MNode::Row(r) => {
            for item in r.0.iter_mut() {
                apply_style(item, upright, bold, script);
            }
        }
        _ => {}
    }
}

/// A single-item row collapses to its item; otherwise it stays a Group.
fn flatten(mut row: Row) -> MNode {
    if row.0.len() == 1 {
        return row.0.pop().unwrap();
    }
    MNode::Group(row)
}

fn row_text(row: &Row) -> String {
    let mut out = String::new();
    for item in &row.0 {
        match item {
            MNode::Lit(l) => out.push_str(&l.text),
            MNode::Group(r) => out.push_str(&row_text(r)),
            _ => {}
        }
    }
    out
}

/// Parse a LaTeX math string into a math AST row.
pub fn parse(latex: &str) -> Result<Row, MathError> {
    let mut p = Parser {
        toks: tokenize(latex),
        i: 0,
        depth: 0,
    };
    let mut row = p.parse_row(STOP_NONE)?;
    if p.peek() != Tok::Eof {
        return Err(MathError::unsupported("syntax", "trailing tokens"));
    }
    attach_nary_bodies(&mut row);
    Ok(row)
}

/// +1 for a bare opening bracket leaf, -1 for a closing one, else 0.
fn bracket_delta(node: &MNode) -> i32 {
    match node {
        MNode::Lit(l) => match l.text.as_str() {
            "(" | "[" | "{" | "⟨" | "⌊" | "⌈" => 1,
            ")" | "]" | "}" | "⟩" | "⌋" | "⌉" => -1,
            _ => 0,
        },
        _ => 0,
    }
}

/// True for a leaf that ends an n-ary operand at its level: a binary `+`/`−`,
/// a relation, punctuation, or a wide space (`\quad`).
fn ends_operand(node: &MNode) -> bool {
    let MNode::Lit(l) = node else {
        return false;
    };
    let t = l.text.trim_matches(' ');
    if t.is_empty() {
        // `\quad`/`\qquad` separate formulas; thin spaces do not
        return l.text.contains(['\u{2003}', '\u{2002}']) || l.text.chars().count() > 1;
    }
    matches!(
        t,
        "+" | "\u{2212}"
            | "-"
            | "±"
            | "∓"
            | "="
            | "≠"
            | "<"
            | ">"
            | "≤"
            | "≥"
            | "≦"
            | "≧"
            | "≪"
            | "≫"
            | "≈"
            | "≃"
            | "≅"
            | "≡"
            | "∼"
            | "∝"
            | "→"
            | "←"
            | "↦"
            | "⇒"
            | "⟹"
            | "⇐"
            | "⇔"
            | "⟺"
            | "↔"
            | "∈"
            | "∉"
            | "⊂"
            | "⊆"
            | "⊃"
            | "⊇"
            | ","
            | ";"
            | ":"
            | "≔"
            | "≜"
            | "≺"
            | "≻"
            | "⪯"
            | "⪰"
            | "∣"
            | "|"
            | "."
            | "&"
    )
}

/// TeX reads `\sum_i e_i \sigma` as the sum of `e_i \sigma`: move each
/// n-ary operator's operand (up to the next binary operator or relation at
/// the same level) into its body. An empty `m:e` shows as a dotted
/// placeholder box in Word.
fn attach_nary_bodies(row: &mut Row) {
    let items = std::mem::take(&mut row.0);
    let mut out: Vec<MNode> = Vec::with_capacity(items.len());
    let mut iter = items.into_iter().peekable();
    while let Some(mut node) = iter.next() {
        if let MNode::Nary(n) = &mut node {
            if n.body.is_none() {
                let mut body: Vec<MNode> = Vec::new();
                // bare brackets are separate leaves: only break outside them,
                // and never swallow a closer that belongs to an outer group
                let mut depth = 0i32;
                while let Some(next) = iter.peek() {
                    match bracket_delta(next) {
                        1 => depth += 1,
                        -1 if depth == 0 => break,
                        -1 => depth -= 1,
                        _ if depth == 0 && ends_operand(next) => break,
                        _ => {}
                    }
                    body.push(iter.next().unwrap());
                }
                // leading thin spaces belong to the layout, not the operand
                while matches!(body.first(), Some(MNode::Lit(l)) if l.text.trim().is_empty()) {
                    out.push(body.remove(0));
                }
                if !body.is_empty() {
                    let mut inner = Row(body);
                    attach_nary_bodies(&mut inner); // nested sums
                    n.body = Some(Box::new(MNode::Row(inner)));
                }
            }
        }
        attach_in_children(&mut node);
        out.push(node);
    }
    row.0 = out;
}

fn attach_in_children(node: &mut MNode) {
    let one = |b: &mut Box<MNode>| attach_in_children(b);
    match node {
        MNode::Row(r) | MNode::Group(r) | MNode::Sqrt(r) | MNode::BorderBox(r) => {
            attach_nary_bodies(r)
        }
        MNode::Accent(r, _) | MNode::Fenced(_, _, r) | MNode::GroupChr(r, _, _) => {
            attach_nary_bodies(r)
        }
        MNode::Frac(a, b) | MNode::Root(a, b) => {
            attach_nary_bodies(a);
            attach_nary_bodies(b);
        }
        MNode::Sup(a, b) | MNode::Sub(a, b) | MNode::LimLow(a, b) | MNode::LimUpp(a, b) => {
            one(a);
            one(b);
        }
        MNode::SubSup(a, b, c) => {
            one(a);
            one(b);
            one(c);
        }
        MNode::SPre(a, b, c) => {
            one(a);
            if let Some(b) = b {
                one(b);
            }
            if let Some(c) = c {
                one(c);
            }
        }
        MNode::Nary(n) => {
            for b in [&mut n.sub, &mut n.sup].into_iter().flatten() {
                one(b);
            }
        }
        MNode::Matrix(rows, _) => {
            for cell in rows.iter_mut().flatten() {
                attach_nary_bodies(cell);
            }
        }
        MNode::Lit(_) => {}
    }
}
