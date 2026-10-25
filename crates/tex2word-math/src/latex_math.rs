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
                write!(f, "{}", format!("unsupported math construct: {construct} {detail}").trim())
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
        Lit { text: text.into(), ..Lit::default() }
    }

    pub fn upright(text: impl Into<String>) -> Self {
        Lit { text: text.into(), upright: true, ..Lit::default() }
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
    /// an over/under brace group: (body, char, pos "top"/"bot")
    GroupChr(Row, String, String),
    /// rows of cells; `aligned` renders column-justified
    Matrix(Vec<Vec<Row>>, bool),
}

/// Functions whose subscript is rendered as an underscript limit.
fn is_limit_func(s: &str) -> bool {
    matches!(s, "lim" | "limsup" | "liminf" | "max" | "min" | "sup" | "inf" | "gcd" | "det")
}

/// \big \Big \bigg \Bigg (+ l/r/m variants): manual delimiter sizing.
fn is_big_delim(name: &str) -> bool {
    ["bigg", "Bigg", "big", "Big"]
        .iter()
        .any(|size| name.strip_prefix(size).is_some_and(|rest| matches!(rest, "" | "l" | "r" | "m")))
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
        "matrix" | "aligned" | "array" | "smallmatrix" | "align" | "alignat" | "flalign" | "gather"
        | "gathered" | "split" | "multlined" | "eqnarray" => ("", ""),
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
    matches!(env, "aligned" | "align" | "alignat" | "flalign" | "split" | "eqnarray")
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
            Tok::Eof => "eof",
        }
    }
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
        } else if matches!(c, ' ' | '\n' | '\t') {
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
            if l.upright && is_limit_func(&l.text) && sub.is_some() && sup.is_none() {
                return Ok(MNode::LimLow(Box::new(base), Box::new(sub.unwrap())));
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
            Tok::Newline | Tok::Amp => return Err(MathError::unsupported("alignment", "stray & or \\\\")),
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
            return Ok(MNode::GroupChr(self.parse_group()?, "⏞".into(), "top".into()));
        }
        if name == "underbrace" || name == "underbracket" {
            return Ok(MNode::GroupChr(self.parse_group()?, "⏟".into(), "bot".into()));
        }
        if name == "binom" {
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
            let limloc = if matches!(name, "int" | "iint" | "iiint" | "oint") { "subSup" } else { "undOvr" };
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
        if is_big_delim(name) {
            // delimiter sizing is visual-only; the next delimiter renders itself
            return Ok(MNode::Lit(Lit::new("")));
        }
        if let Some((open, close)) = paired_delim(name) {
            if self.peek() == Tok::Char('*') {
                self.next(); // \abs*{x} auto-size form
            }
            return Ok(MNode::Fenced(open.into(), close.into(), self.parse_group()?));
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
            return Ok(MNode::Matrix(vec![vec![top], vec![base]], false));
        }
        if name == "underset" {
            let bot = self.parse_group()?;
            let base = self.parse_group()?;
            return Ok(MNode::Matrix(vec![vec![base], vec![bot]], false));
        }
        if matches!(
            name,
            "text" | "mathrm" | "operatorname" | "operatorname*" | "mathsf" | "mathtt" | "mbox" | "textrm"
                | "textnormal" | "textsf" | "texttt" | "textsc"
        ) {
            let row = self.parse_group()?;
            return Ok(styled(row, true, false, None));
        }
        if name == "textbf" {
            return Ok(styled(self.parse_group()?, true, true, None));
        }
        if matches!(name, "textit" | "textsl" | "emph") {
            return Ok(MNode::Group(self.parse_group()?));
        }
        if matches!(name, "mathbf" | "boldsymbol" | "bm") {
            return Ok(styled(self.parse_group()?, false, true, None));
        }
        if name == "mathbb" {
            return Ok(styled(self.parse_group()?, true, false, Some("double-struck")));
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
            "mathbin" | "mathrel" | "mathop" | "mathord" | "mathopen" | "mathclose" | "mathpunct" | "mathinner"
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
        if name == "begin" {
            return self.parse_environment();
        }
        if name == "end" {
            return Err(MathError::unsupported("environment", "stray \\end"));
        }
        if matches!(name, "displaystyle" | "textstyle" | "scriptstyle" | "limits" | "nolimits") {
            return Ok(MNode::Lit(Lit::new(""))); // styling hints we ignore
        }
        if matches!(name, "nonumber" | "notag" | "qedhere") {
            return Ok(MNode::Lit(Lit::new("")));
        }
        if matches!(name, "hline" | "toprule" | "midrule" | "bottomrule" | "hdashline") {
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
        let second = if self.peek() == Tok::LBrace { Some(self.parse_group()?) } else { None };
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
        let under = if self.peek() == Tok::Char('[') { Some(self.parse_optional()?) } else { None };
        let over = self.parse_group()?;
        let base = MNode::Lit(Lit::upright(arrow));
        if let Some(under) = under.filter(|u| !u.0.is_empty()) {
            return Ok(MNode::SubSup(Box::new(base), Box::new(flatten(under)), Box::new(flatten(over))));
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
            Tok::Cmd(v) => Ok(S::get(S::DELIMITERS, &v)
                .or_else(|| S::get(S::SYMBOLS, &v))
                .map_or(v.clone(), str::to_string)),
            _ => Err(MathError::unsupported("delimiter", "")),
        }
    }

    fn parse_fenced(&mut self) -> R<MNode> {
        let open = self.read_delim()?;
        let mut items = Vec::new();
        loop {
            match self.peek() {
                Tok::Eof => return Err(MathError::unsupported("delimiter", "unclosed \\left")),
                Tok::Cmd(v) if v == "right" => {
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
                    rows.last_mut().unwrap().push(Row(std::mem::take(&mut current)));
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
        if rows.last().is_some_and(|r| r.iter().all(|c| c.0.is_empty())) {
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
                Tok::Eof => return Err(MathError::unsupported("environment", format!("unclosed {env}"))),
                Tok::Cmd(v) if v == "end" => {
                    self.next();
                    self.parse_group()?; // {env}
                    rows.last_mut().unwrap().push(Row(std::mem::take(&mut current)));
                    break;
                }
                Tok::Amp => {
                    self.next();
                    rows.last_mut().unwrap().push(Row(std::mem::take(&mut current)));
                    continue;
                }
                Tok::Newline => {
                    self.next();
                    rows.last_mut().unwrap().push(Row(std::mem::take(&mut current)));
                    rows.push(Vec::new());
                    continue;
                }
                Tok::Cmd(v) if v == "cr" => {
                    // plain-TeX \cr ends a row too
                    self.next();
                    rows.last_mut().unwrap().push(Row(std::mem::take(&mut current)));
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
        if rows.last().is_some_and(|r| r.iter().all(|c| c.0.is_empty())) {
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
    } else {
        MNode::Lit(Lit::upright(c.to_string()))
    }
}

/// Apply styling to every Lit leaf of a parsed group.
fn styled(mut row: Row, upright: bool, bold: bool, script: Option<&str>) -> MNode {
    for item in row.0.iter_mut() {
        apply_style(item, upright, bold, script);
    }
    MNode::Group(row)
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
    let mut p = Parser { toks: tokenize(latex), i: 0, depth: 0 };
    let row = p.parse_row(STOP_NONE)?;
    if p.peek() != Tok::Eof {
        return Err(MathError::unsupported("syntax", "trailing tokens"));
    }
    Ok(row)
}
