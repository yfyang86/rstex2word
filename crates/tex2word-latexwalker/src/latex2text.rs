//! LaTeX -> plain text (port of pylatexenc's `latex2text`), configured as
//! tex2word uses it: `LatexNodes2Text(keep_comments=False,
//! strict_latex_spaces=False)` with the default text-spec database.
//!
//! [`latex_to_text`] returns `Err` where pylatexenc raises (a missing
//! `%(n)s` key, a matrix with no cells, a callable indexing a missing
//! argument, …), so callers can apply the same fallbacks as the reference.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use tex2word_ir::pystr;

use crate::latex2text_specs::L2T_CATEGORIES;
use crate::{ContextDb, LatexWalker, Node, NodeKind, ParsingState, Stop};

/// A replacement callable of the spec database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fn {
    EquationEnv,
    InputMacro,
    Placeholder(&'static str),
    MatrixEnv,
    Uebung,
    MathStyle(&'static str),
    Title,
    Author,
    Date,
    Maketitle,
    Item,
    Href,
    Part,
    Chapter,
    Section,
    Subsection,
    Subsubsection,
    Paragraph,
    Subparagraph,
    Texorpdfstring,
    Percent,
    Accent(&'static str),
    Today,
}

/// `simplify_repl` of a text spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repl {
    None,
    Str(&'static str),
    Fn(Fn),
}

/// `MacroTextSpec` / `EnvironmentTextSpec` / `SpecialsTextSpec`
/// (`discard` is `None` for specials, which have no such attribute).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextSpec {
    pub repl: Repl,
    pub discard: Option<bool>,
}

/// A pylatexenc exception during conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L2tError(pub String);

type R<T> = Result<T, L2tError>;

struct Db {
    macros: HashMap<&'static str, TextSpec>,
    envs: HashMap<&'static str, TextSpec>,
    specials: HashMap<&'static str, TextSpec>,
}

static DB: LazyLock<Db> = LazyLock::new(|| {
    let mut db = Db {
        macros: HashMap::new(),
        envs: HashMap::new(),
        specials: HashMap::new(),
    };
    // the first category that defines a name wins
    for (_, macros, envs, specials) in L2T_CATEGORIES {
        for (n, s) in macros.iter() {
            db.macros.entry(n).or_insert(*s);
        }
        for (n, s) in envs.iter() {
            db.envs.entry(n).or_insert(*s);
        }
        for (n, s) in specials.iter() {
            db.specials.entry(n).or_insert(*s);
        }
    }
    db
});

static WALKER_DB: LazyLock<Arc<ContextDb>> = LazyLock::new(|| Arc::new(ContextDb::default_db()));

/// `strict_latex_spaces` flags.
#[derive(Debug, Clone, Copy)]
struct Strict {
    between_macro_and_chars: bool,
    between_latex_constructs: bool,
    after_comment: bool,
    /// the preset pushed inside equations (`None` = keep)
    in_equations_based_on_source: bool,
}

/// `strict_latex_spaces=False` == the "macros" preset.
const MACROS: Strict = Strict {
    between_macro_and_chars: true,
    between_latex_constructs: true,
    after_comment: false,
    in_equations_based_on_source: true,
};
/// the "based-on-source" preset (pushed inside equations)
const BASED_ON_SOURCE: Strict = Strict {
    between_macro_and_chars: false,
    between_latex_constructs: false,
    after_comment: false,
    in_equations_based_on_source: false,
};

/// `LatexNodes2Text(keep_comments=False, strict_latex_spaces=False)`.
pub struct LatexNodes2Text {
    strict: Strict,
    doc_title: Option<String>,
    doc_author: Option<String>,
    doc_date: Option<String>,
}

impl Default for LatexNodes2Text {
    fn default() -> Self {
        Self::new()
    }
}

/// `LatexNodes2Text(keep_comments=False, strict_latex_spaces=False).latex_to_text(latex)`.
pub fn latex_to_text(latex: &str) -> Result<String, L2tError> {
    LatexNodes2Text::new().latex_to_text(latex)
}

/// pylatexenc's legacy `(nodeoptarg, nodeargs)` view of a node's arguments;
/// `nodeargs` is `None` for a macro read as another macro's bare argument.
fn legacy_args_opt(node: &Node) -> (Option<&Node>, Option<Vec<Option<&Node>>>) {
    if matches!(
        node.kind,
        NodeKind::Macro {
            bare_expression: true,
            ..
        }
    ) {
        return (None, None);
    }
    let (o, a) = legacy_args(node);
    (o, Some(a))
}

fn none_err() -> L2tError {
    L2tError("'NoneType' object is not subscriptable".into())
}

fn legacy_args(node: &Node) -> (Option<&Node>, Vec<Option<&Node>>) {
    let Some(argd) = node.nodeargd() else {
        return (None, Vec::new());
    };
    let list: Vec<Option<&Node>> = argd.argnlist.iter().map(Option::as_ref).collect();
    let spec = argd.argspec.as_str();
    let nskip = spec.len() - spec.trim_start_matches('*').len();
    let rest = &spec[nskip..];
    if rest.starts_with('[') && rest[1..].chars().all(|c| c == '{') {
        let opt = list.get(nskip).copied().flatten();
        let args = list.get(nskip + 1..).map(<[_]>::to_vec).unwrap_or_default();
        return (opt, args);
    }
    (None, list)
}

fn argnlist(node: &Node) -> Vec<Option<&Node>> {
    node.nodeargd().map_or_else(Vec::new, |a| {
        a.argnlist.iter().map(Option::as_ref).collect()
    })
}

impl LatexNodes2Text {
    pub fn new() -> Self {
        LatexNodes2Text {
            strict: MACROS,
            doc_title: None,
            doc_author: None,
            doc_date: None,
        }
    }

    /// Parse `latex` with the default walker context and convert it.
    pub fn latex_to_text(&mut self, latex: &str) -> R<String> {
        let walker = LatexWalker::new(latex, WALKER_DB.clone());
        let (nodes, _, _) = walker
            .get_latex_nodes(0, &Stop::default(), &ParsingState::default())
            .map_err(|e| L2tError(format!("{e:?}")))?;
        self.nodelist_to_text(&nodes)
    }

    pub fn nodelist_to_text(&mut self, nodelist: &[Node]) -> R<String> {
        let refs: Vec<Option<&Node>> = nodelist.iter().map(Some).collect();
        self.nodes_to_text(&refs)
    }

    fn nodes_to_text(&mut self, nodelist: &[Option<&Node>]) -> R<String> {
        let mut s = String::new();
        let mut prev: Option<&Node> = None;
        for node in nodelist {
            if let (Some(p), Some(n)) = (prev, node) {
                if is_bare_macro(p)? && n.is_chars() && !self.strict.between_macro_and_chars {
                    // after a macro with no arguments, keep its post_space
                    if let NodeKind::Macro {
                        macro_post_space, ..
                    } = &p.kind
                    {
                        s.push_str(macro_post_space);
                    }
                }
            }
            s.push_str(&self.node_to_text(*node)?);
            prev = *node;
        }
        Ok(s)
    }

    fn node_to_text(&mut self, node: Option<&Node>) -> R<String> {
        let Some(node) = node else {
            return Ok(String::new());
        };
        match &node.kind {
            NodeKind::Chars { chars } => {
                if !self.strict.between_latex_constructs && pystr::strip(chars).is_empty() {
                    return Ok(String::new());
                }
                Ok(chars.clone())
            }
            NodeKind::Comment {
                comment_post_space, ..
            } => {
                // keep_comments=False
                if self.strict.after_comment {
                    Ok(String::new())
                } else {
                    Ok(comment_post_space.clone())
                }
            }
            NodeKind::Group { nodelist, .. } => self.nodelist_to_text(nodelist),
            NodeKind::Macro { macroname, .. } => {
                let spec = DB
                    .macros
                    .get(macroname.as_str())
                    .copied()
                    .unwrap_or(TextSpec {
                        repl: Repl::Str(""),
                        discard: Some(true),
                    });
                if is_truthy(&spec.repl) {
                    return self.apply_simplify_repl(node, spec.repl);
                }
                if spec.discard == Some(true) {
                    return Ok(String::new());
                }
                let mut out = String::new();
                for a in argnlist(node) {
                    out.push_str(&self.groupnode_contents(a)?);
                }
                Ok(out)
            }
            NodeKind::Environment {
                environmentname,
                nodelist,
                ..
            } => {
                let spec = DB
                    .envs
                    .get(environmentname.as_str())
                    .copied()
                    .unwrap_or(TextSpec {
                        repl: Repl::Str(""),
                        discard: Some(false),
                    });
                if is_truthy(&spec.repl) {
                    return self.apply_simplify_repl(node, spec.repl);
                }
                if spec.discard == Some(true) {
                    return Ok(String::new());
                }
                self.nodelist_to_text(nodelist)
            }
            NodeKind::Specials { specials_chars, .. } => {
                let Some(spec) = DB.specials.get(specials_chars.as_str()).copied() else {
                    return Ok(specials_chars.clone());
                };
                if is_truthy(&spec.repl) {
                    return self.apply_simplify_repl(node, spec.repl);
                }
                match spec.discard {
                    None => Err(L2tError(
                        "'SpecialsTextSpec' object has no attribute 'discard'".into(),
                    )),
                    Some(true) => Ok(String::new()),
                    Some(false) => {
                        let mut out = String::new();
                        for a in argnlist(node) {
                            out.push_str(&self.groupnode_contents(a)?);
                        }
                        Ok(out)
                    }
                }
            }
            NodeKind::Math { .. } => self.math_node_to_text(node),
        }
    }

    /// `math_mode='text'`: the content (indented block when display).
    fn math_node_to_text(&mut self, node: &Node) -> R<String> {
        let saved = self.strict;
        if self.strict.in_equations_based_on_source {
            self.strict = BASED_ON_SOURCE;
        }
        let content = self.nodelist_to_text(node.nodelist().unwrap_or(&[]));
        self.strict = saved;
        let content = pystr::strip(&content?).to_string();
        let display = match &node.kind {
            NodeKind::Environment { .. } => true,
            NodeKind::Math { displaytype, .. } => displaytype == "display",
            _ => false,
        };
        Ok(if display {
            indented_block(&content, "    ")
        } else {
            content
        })
    }

    fn groupnode_contents(&mut self, n: Option<&Node>) -> R<String> {
        match n {
            None => Ok(String::new()),
            Some(g) => match &g.kind {
                NodeKind::Group { nodelist, .. } => self.nodelist_to_text(nodelist),
                _ => self.node_to_text(Some(g)),
            },
        }
    }

    /// `node_arg_to_text(node, k)`.
    fn node_arg_to_text(&mut self, node: &Node, k: usize) -> R<String> {
        let args = argnlist(node);
        if args.is_empty() {
            return Ok(String::new());
        }
        let a = *args
            .get(k)
            .ok_or_else(|| L2tError("list index out of range".into()))?;
        self.groupnode_contents(a)
    }

    fn apply_simplify_repl(&mut self, node: &Node, repl: Repl) -> R<String> {
        match repl {
            Repl::None => Ok(String::new()),
            Repl::Fn(f) => self.call(node, f),
            Repl::Str(s) => {
                if !(s.contains('%') && pystr::char_len(s) != 1) {
                    return Ok(s.to_string());
                }
                let nodeargs = argnlist(node);
                let has_percent_s = has_percent_s(s);
                let args = if let NodeKind::Environment { nodelist, .. } = &node.kind {
                    if has_percent_s {
                        PArgs::Tuple(vec![self.nodelist_to_text(nodelist)?])
                    } else {
                        let mut d = Vec::new();
                        for (j, nn) in nodeargs.iter().enumerate() {
                            d.push(((j + 1).to_string(), self.groupnode_contents(*nn)?));
                        }
                        d.push(("body".to_string(), self.nodelist_to_text(nodelist)?));
                        PArgs::Dict(d)
                    }
                } else if has_percent_s {
                    let mut t = Vec::new();
                    for nn in &nodeargs {
                        t.push(self.groupnode_contents(*nn)?);
                    }
                    PArgs::Tuple(t)
                } else {
                    let mut d = Vec::new();
                    for (j, nn) in nodeargs.iter().enumerate() {
                        d.push(((j + 1).to_string(), self.groupnode_contents(*nn)?));
                    }
                    PArgs::Dict(d)
                };
                match percent_format(s, &args) {
                    Ok(r) => Ok(r),
                    // TypeError/ValueError: keep the percent signs as they are
                    Err(PercentError::Type) => Ok(s.to_string()),
                    Err(PercentError::Key(k)) => Err(L2tError(format!("KeyError: {k:?}"))),
                }
            }
        }
    }

    fn call(&mut self, node: &Node, f: Fn) -> R<String> {
        let out = match f {
            Fn::EquationEnv => self.math_node_to_text(node)?,
            Fn::InputMacro => {
                let nodeargs = legacy_args_opt(node).1.ok_or_else(none_err)?;
                let first = *nodeargs
                    .first()
                    .ok_or_else(|| L2tError("list index out of range".into()))?;
                self.nodes_to_text(&[first])?;
                String::new() // no tex_input_directory: never reads files
            }
            Fn::Placeholder(pht) => {
                let spaced: Vec<String> = pht.chars().map(String::from).collect();
                indented_block(&format!("< {} >", spaced.join(" ")), "    ")
            }
            Fn::MatrixEnv => self.matrix(node)?,
            Fn::Uebung => {
                let nodeargs = legacy_args_opt(node).1.ok_or_else(none_err)?;
                let first = *nodeargs
                    .first()
                    .ok_or_else(|| L2tError("list index out of range".into()))?;
                let mut s = format!("\n{}\n", self.nodes_to_text(&[first])?);
                let optarg = *nodeargs
                    .get(1)
                    .ok_or_else(|| L2tError("list index out of range".into()))?;
                if optarg.is_some() {
                    s.push_str(&format!("[{}]\n", self.nodes_to_text(&[optarg])?));
                }
                s
            }
            Fn::MathStyle(style) => {
                let arg = self.node_arg_to_text(node, 0)?;
                fmt_math_text_style(&arg, style)
            }
            Fn::Title | Fn::Author | Fn::Date => {
                let argd = node.nodeargd().ok_or_else(|| {
                    L2tError("'NoneType' object has no attribute 'argnlist'".into())
                })?;
                let first: Vec<Option<&Node>> =
                    argd.argnlist.iter().take(1).map(Option::as_ref).collect();
                let text = self.nodes_to_text(&first)?;
                match f {
                    Fn::Title => self.doc_title = Some(text),
                    Fn::Author => self.doc_author = Some(text),
                    _ => self.doc_date = Some(text),
                }
                String::new()
            }
            Fn::Maketitle => {
                let title = self
                    .doc_title
                    .clone()
                    .unwrap_or_else(|| r"[NO \title GIVEN]".into());
                let author = self
                    .doc_author
                    .clone()
                    .unwrap_or_else(|| r"[NO \author GIVEN]".into());
                let date = self
                    .doc_date
                    .clone()
                    .unwrap_or_else(tex2word_ir::pyos::today_text);
                let width = pystr::char_len(&title)
                    .max(4 + pystr::char_len(&author))
                    .max(4 + pystr::char_len(&date));
                format!(
                    "{title}\n    {author}\n    {date}\n{}\n\n",
                    "=".repeat(width)
                )
            }
            Fn::Item => {
                let (opt, _) = legacy_args(node);
                match opt {
                    Some(o) => format!("\n  {}", self.nodes_to_text(&[Some(o)])?),
                    None => "\n  * ".to_string(),
                }
            }
            Fn::Href => {
                let args = node
                    .nodeargd()
                    .ok_or_else(|| {
                        L2tError("'NoneType' object has no attribute 'argnlist'".into())
                    })?
                    .argnlist
                    .iter()
                    .map(Option::as_ref)
                    .collect::<Vec<_>>();
                let idx = |k: usize| {
                    args.get(k)
                        .copied()
                        .ok_or_else(|| L2tError("list index out of range".into()))
                };
                let (a1, a0) = (idx(1)?, idx(0)?);
                let text = self.nodes_to_text(&[a1])?;
                let url = self.nodes_to_text(&[a0])?;
                format!("{text} <{url}>")
            }
            Fn::Part => format!(
                "\n\nPART: {}\n",
                pystr::upper(&self.node_arg_to_text(node, 2)?)
            ),
            Fn::Chapter => format!(
                "\n\nCHAPTER: {}\n",
                pystr::upper(&self.node_arg_to_text(node, 2)?)
            ),
            Fn::Section => format!("\n\n§ {}\n", pystr::upper(&self.node_arg_to_text(node, 2)?)),
            Fn::Subsection => format!("\n\n §.§ {}\n", self.node_arg_to_text(node, 2)?),
            Fn::Subsubsection => format!("\n\n  §.§.§ {}\n", self.node_arg_to_text(node, 2)?),
            Fn::Paragraph => format!("\n\n  {}\n", self.node_arg_to_text(node, 2)?),
            Fn::Subparagraph => format!("\n\n    {}\n", self.node_arg_to_text(node, 2)?),
            Fn::Texorpdfstring => {
                let nodeargs = legacy_args_opt(node).1.ok_or_else(none_err)?;
                let second: Vec<Option<&Node>> = nodeargs.into_iter().skip(1).take(1).collect();
                self.nodes_to_text(&second)?
            }
            Fn::Percent => "%".to_string(),
            Fn::Accent(combining) => {
                // `if node.nodeargs and len(node.nodeargs)` (None is falsy)
                let nodeargs = legacy_args_opt(node).1.unwrap_or_default();
                let c = match nodeargs.first() {
                    Some(first) => pystr::strip(&self.nodes_to_text(&[*first])?).to_string(),
                    None => " ".to_string(),
                };
                c.chars()
                    .map(|ch| {
                        let ch = match ch {
                            'ı' => 'i',
                            'ȷ' => 'j',
                            other => other,
                        };
                        pystr::nfc(&format!("{ch}{combining}"))
                    })
                    .collect()
            }
            Fn::Today => tex2word_ir::pyos::today_text(),
        };
        Ok(out)
    }

    /// `fmt_matrix_environment_node`: `[ a11 a12; a21 a22 ]`.
    fn matrix(&mut self, node: &Node) -> R<String> {
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut this_col: Vec<String> = Vec::new();
        let mut buf: Vec<&Node> = Vec::new();
        let new_column =
            |b: &mut Self, buf: &mut Vec<&Node>, this_col: &mut Vec<String>| -> R<()> {
                if !buf.is_empty() {
                    let refs: Vec<Option<&Node>> = buf.iter().map(|n| Some(*n)).collect();
                    this_col.push(pystr::strip(&b.nodes_to_text(&refs)?).to_string());
                }
                buf.clear();
                Ok(())
            };
        for n in node.nodelist().unwrap_or(&[]) {
            if matches!(&n.kind, NodeKind::Specials { specials_chars, .. } if specials_chars == "&")
            {
                new_column(self, &mut buf, &mut this_col)?;
                continue;
            }
            if n.macroname() == Some("\\") {
                new_column(self, &mut buf, &mut this_col)?;
                rows.push(std::mem::take(&mut this_col));
                continue;
            }
            buf.push(n);
        }
        new_column(self, &mut buf, &mut this_col)?;
        rows.push(this_col);
        let width = rows
            .iter()
            .flatten()
            .map(|x| pystr::char_len(x))
            .max()
            .ok_or_else(|| L2tError("max() arg is an empty sequence".into()))?;
        let body: Vec<String> = rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|x| format!("{}{x}", " ".repeat(width - pystr::char_len(x))))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        Ok(format!("[ {} ]", body.join("; ")))
    }
}

fn is_truthy(r: &Repl) -> bool {
    match r {
        Repl::None => false,
        Repl::Str(s) => !s.is_empty(),
        Repl::Fn(_) => true,
    }
}

/// `_is_bare_macro_node` (`len(None)` raises for a bare-expression macro).
fn is_bare_macro(node: &Node) -> R<bool> {
    if !node.is_macro() {
        return Ok(false);
    }
    let (opt, args) = legacy_args_opt(node);
    if opt.is_some() {
        return Ok(false);
    }
    match args {
        Some(a) => Ok(a.is_empty()),
        None => Err(L2tError("object of type 'NoneType' has no len()".into())),
    }
}

fn indented_block(contents: &str, indent: &str) -> String {
    format!(
        "\n{indent}{}\n",
        contents.replace('\n', &format!("\n{indent}"))
    )
}

/// `re.search('(^|[^%])(%%)*%s', s)`.
fn has_percent_s(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let start = i;
            while i < b.len() && b[i] == b'%' {
                i += 1;
            }
            let run = i - start;
            if run % 2 == 1 && i < b.len() && b[i] == b's' {
                return true;
            }
        } else {
            i += 1;
        }
    }
    false
}

enum PArgs {
    Tuple(Vec<String>),
    Dict(Vec<(String, String)>),
}

enum PercentError {
    /// TypeError / ValueError
    Type,
    /// KeyError
    Key(String),
}

/// Python's `fmt % args` for the `%s` / `%(key)s` / `%%` forms the spec
/// database uses.
fn percent_format(fmt: &str, args: &PArgs) -> Result<String, PercentError> {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    let mut next_arg = 0;
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut key: Option<String> = None;
        if chars.peek() == Some(&'(') {
            chars.next();
            let mut depth = 1;
            let mut k = String::new();
            loop {
                match chars.next() {
                    None => return Err(PercentError::Type), // incomplete format key
                    Some('(') => {
                        depth += 1;
                        k.push('(');
                    }
                    Some(')') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        k.push(')');
                    }
                    Some(ch) => k.push(ch),
                }
            }
            key = Some(k);
        }
        // flags, width, precision (unused by the specs)
        while matches!(chars.peek(), Some('-' | '+' | ' ' | '#' | '0'..='9' | '.')) {
            chars.next();
        }
        let conv = chars.next().ok_or(PercentError::Type)?;
        if conv == '%' && key.is_none() {
            out.push('%');
            continue;
        }
        if conv != 's' {
            return Err(PercentError::Type);
        }
        match (args, key) {
            (PArgs::Dict(d), Some(k)) => match d.iter().find(|(dk, _)| *dk == k) {
                Some((_, v)) => out.push_str(v),
                None => return Err(PercentError::Key(k)),
            },
            (PArgs::Tuple(_), Some(_)) => return Err(PercentError::Type), // format requires a mapping
            (PArgs::Tuple(t), None) => {
                let v = t.get(next_arg).ok_or(PercentError::Type)?;
                next_arg += 1;
                out.push_str(v);
            }
            // %s with a mapping formats the dict itself (never in the specs)
            (PArgs::Dict(_), None) => return Err(PercentError::Type),
        }
    }
    if let PArgs::Tuple(t) = args {
        if next_arg != t.len() {
            return Err(PercentError::Type); // not all arguments converted
        }
    }
    Ok(out)
}

fn style_offsets(style: &str) -> (u32, u32) {
    match style {
        "bold" => (0x1D400, 0x1D41A),
        "italic" => (0x1D434, 0x1D44E),
        "bold-italic" => (0x1D468, 0x1D482),
        "script" => (0x1D49C, 0x1D4B6),
        "bold-script" => (0x1D4D0, 0x1D4EA),
        "fraktur" => (0x1D504, 0x1D51E),
        "doublestruck" => (0x1D538, 0x1D552),
        "bold-fraktur" => (0x1D56C, 0x1D586),
        "sans" => (0x1D5A0, 0x1D5BA),
        "sans-bold" => (0x1D5D4, 0x1D5EE),
        "sans-italic" => (0x1D608, 0x1D622),
        "sans-bold-italic" => (0x1D63C, 0x1D656),
        "monospace" => (0x1D670, 0x1D68A),
        _ => ('A' as u32, 'a' as u32),
    }
}

fn style_exception(style: &str, c: char) -> Option<char> {
    let cp = match (style, c) {
        ("italic", 'h') => 0x210E,
        ("script", 'B') => 0x212C,
        ("script", 'E') => 0x2130,
        ("script", 'F') => 0x2131,
        ("script", 'H') => 0x210B,
        ("script", 'I') => 0x2110,
        ("script", 'L') => 0x2112,
        ("script", 'M') => 0x2133,
        ("script", 'R') => 0x211B,
        ("script", 'e') => 0x212F,
        ("script", 'g') => 0x210A,
        ("script", 'o') => 0x2134,
        ("fraktur", 'C') => 0x212D,
        ("fraktur", 'H') => 0x210C,
        ("fraktur", 'I') => 0x2111,
        ("fraktur", 'R') => 0x211C,
        ("fraktur", 'Z') => 0x2128,
        ("doublestruck", 'C') => 0x2102,
        ("doublestruck", 'H') => 0x210D,
        ("doublestruck", 'N') => 0x2115,
        ("doublestruck", 'P') => 0x2119,
        ("doublestruck", 'Q') => 0x211A,
        ("doublestruck", 'R') => 0x211D,
        ("doublestruck", 'Z') => 0x2124,
        _ => return None,
    };
    char::from_u32(cp)
}

/// `fmt_math_text_style`: letters -> Unicode math alphanumerics.
pub fn fmt_math_text_style(text: &str, style: &str) -> String {
    let (up, lo) = style_offsets(style);
    text.chars()
        .map(|c| {
            if let Some(z) = style_exception(style, c) {
                return z;
            }
            let oc = c as u32;
            if c.is_ascii_uppercase() {
                char::from_u32(up + oc - 'A' as u32).unwrap_or(c)
            } else if c.is_ascii_lowercase() {
                char::from_u32(lo + oc - 'a' as u32).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        assert_eq!(
            latex_to_text(r"Caf\'e \& {Bar} --- \textbf{x}").unwrap(),
            "Café & Bar — x"
        );
        assert_eq!(latex_to_text(r"$\alpha + \mathbb{R}$").unwrap(), "α + ℝ");
        assert!(has_percent_s("%s") && has_percent_s("a%%%s") && !has_percent_s("%%s"));
    }
}
