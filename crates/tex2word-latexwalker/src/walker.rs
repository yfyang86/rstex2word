//! The LaTeX walker (port of `pylatexenc.latexwalker.LatexWalker`).
//!
//! The control flow mirrors pylatexenc 2.11 closely, including its tolerant
//! parsing: pylatexenc signals problems with two exceptions
//! (`LatexWalkerEndOfStream`, `LatexWalkerParseError`) and recovers from them
//! at well-defined points; here they are the two [`WalkError`] variants and
//! recovery happens at exactly the same points, so malformed input produces
//! the same node tree in both implementations.

use std::sync::Arc;

use crate::chars::{is_alpha, is_space, is_word};
use crate::node::{Node, NodeKind, ParsedArgs};
use crate::spec::{ArgsParser, ContextDb, VerbatimKind};

/// `ParsingState`: the math-mode context a node list is read in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsingState {
    pub in_math_mode: bool,
    pub math_mode_delimiter: Option<String>,
}

impl ParsingState {
    /// `sub_context(in_math_mode=True, math_mode_delimiter=delim)`.
    fn math(&self, delim: &str) -> ParsingState {
        ParsingState {
            in_math_mode: true,
            math_mode_delimiter: Some(delim.to_string()),
        }
    }

    /// `sub_context(in_math_mode=flag)`; leaving math mode drops the delimiter.
    fn with_math_mode(&self, flag: bool) -> ParsingState {
        ParsingState {
            in_math_mode: flag,
            math_mode_delimiter: if flag {
                self.math_mode_delimiter.clone()
            } else {
                None
            },
        }
    }
}

/// pylatexenc's two exceptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkError {
    /// `LatexWalkerEndOfStream(final_space)`
    EndOfStream { final_space: String },
    /// `LatexWalkerParseError(msg)` at `pos`
    Parse { msg: String, pos: usize },
}

type WResult<T> = Result<T, WalkError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Char,
    Macro,
    MathInline,
    MathDisplay,
    BraceOpen,
    BraceClose,
    Comment,
    BeginEnv,
    EndEnv,
    Specials,
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    arg: String,
    pos: usize,
    len: usize,
    pre_space: String,
    post_space: String,
}

/// Stop conditions for [`LatexWalker::get_latex_nodes`].
#[derive(Debug, Clone, Default)]
pub struct Stop {
    /// `stop_upon_closing_brace`: the `(opening, closing)` brace pair
    pub closing_brace: Option<(char, char)>,
    /// `stop_upon_end_environment`
    pub end_environment: Option<String>,
    /// `stop_upon_closing_mathmode`
    pub closing_mathmode: Option<String>,
    /// `read_max_nodes`
    pub read_max_nodes: Option<usize>,
}

/// `LatexWalker(s, latex_context, tolerant_parsing=True)`.
#[derive(Debug, Clone)]
pub struct LatexWalker {
    s: Arc<str>,
    ctx: Arc<ContextDb>,
}

fn char_at(s: &str, pos: usize) -> Option<char> {
    s.get(pos..).and_then(|r| r.chars().next())
}

enum ReadResult {
    /// `False`/`None`: keep reading
    Continue,
    /// `True`: a stop condition was met
    Stop,
    /// the `LatexWalkerEndOfStream` object returned in tolerant mode
    Eos(String),
}

struct PosPointer {
    pos: usize,
    ps: ParsingState,
    lastchars: String,
    lastchars_pos: Option<usize>,
}

impl PosPointer {
    fn push_lastchars(&mut self, pos: usize, chars: &str) {
        self.lastchars.push_str(chars);
        if self.lastchars_pos.is_none() {
            self.lastchars_pos = Some(pos);
        }
    }

    fn flush_lastchars(&mut self) -> (usize, String) {
        let pos = self.lastchars_pos.take().unwrap_or(self.pos);
        (pos, std::mem::take(&mut self.lastchars))
    }
}

impl LatexWalker {
    /// A walker over `s` with the given context database (always tolerant).
    pub fn new(s: impl Into<Arc<str>>, ctx: Arc<ContextDb>) -> Self {
        LatexWalker { s: s.into(), ctx }
    }

    /// The walked source.
    pub fn source(&self) -> &str {
        &self.s
    }

    fn node(&self, kind: NodeKind, pos: usize, len: usize) -> Node {
        Node {
            pos,
            len,
            kind,
            src: self.s.clone(),
        }
    }

    fn chars_node(&self, chars: &str, pos: usize, len: usize) -> Node {
        self.node(
            NodeKind::Chars {
                chars: chars.to_string(),
            },
            pos,
            len,
        )
    }

    /// `get_latex_nodes(pos=0)` over the whole source with the default
    /// parsing state.
    pub fn get_latex_nodes_all(&self) -> Vec<Node> {
        self.get_latex_nodes(0, &Stop::default(), &ParsingState::default())
            .map(|(nodes, _, _)| nodes)
            .unwrap_or_default()
    }

    // ----------------------------------------------------------------------- //
    // tokenizer
    // ----------------------------------------------------------------------- //

    /// `\begin`/`\end` name: `^\s*\{([\w* ._-]+)\}` at `start`; returns the
    /// name and the match length in bytes.
    fn env_name_at(&self, start: usize) -> Option<(String, usize)> {
        let s = &*self.s;
        let mut k = start;
        while let Some(c) = char_at(s, k) {
            if !is_space(c) {
                break;
            }
            k += c.len_utf8();
        }
        if char_at(s, k) != Some('{') {
            return None;
        }
        k += 1;
        let name_start = k;
        while let Some(c) = char_at(s, k) {
            if is_word(c) || matches!(c, '*' | ' ' | '.' | '_' | '-') {
                k += c.len_utf8();
            } else {
                break;
            }
        }
        if k == name_start || char_at(s, k) != Some('}') {
            return None;
        }
        Some((s[name_start..k].to_string(), k + 1 - start))
    }

    fn get_token(
        &self,
        mut pos: usize,
        include_brace_chars: &[(char, char)],
        environments: bool,
        ps: &ParsingState,
    ) -> WResult<Token> {
        let s = &*self.s;
        let mut space = String::new();
        let tok = |tok: Tok, arg: String, pos: usize, len: usize, pre: &str| Token {
            tok,
            arg,
            pos,
            len,
            pre_space: pre.to_string(),
            post_space: String::new(),
        };
        while let Some(c) = char_at(s, pos) {
            if !is_space(c) {
                break;
            }
            space.push(c);
            pos += c.len_utf8();
            if space.ends_with("\n\n") {
                let pre = &space[..space.len() - 2];
                return Ok(tok(Tok::Char, "\n\n".into(), pos - 2, 2, pre));
            }
        }
        let Some(c0) = char_at(s, pos) else {
            return Err(WalkError::EndOfStream { final_space: space });
        };

        if c0 == '\\' {
            let Some(c1) = char_at(s, pos + 1) else {
                return Err(WalkError::EndOfStream {
                    final_space: String::new(),
                });
            };
            let mut mac = c1.to_string();
            let mut end = pos + 1 + c1.len_utf8();
            let isalphamacro = is_alpha(c1);
            if isalphamacro {
                while let Some(c) = char_at(s, end) {
                    if !is_alpha(c) {
                        break;
                    }
                    mac.push(c);
                    end += c.len_utf8();
                }
            }
            let i = end - pos;
            if mac == "[" || mac == "]" {
                return Ok(tok(Tok::MathDisplay, format!("\\{mac}"), pos, i, &space));
            }
            if mac == "(" || mac == ")" {
                return Ok(tok(Tok::MathInline, format!("\\{mac}"), pos, i, &space));
            }
            if environments && (mac == "begin" || mac == "end") {
                return Ok(match self.env_name_at(end) {
                    // tolerant: a placeholder char token
                    None => tok(Tok::Char, format!("\\{mac}"), pos, i, &space),
                    Some((name, mlen)) => tok(
                        if mac == "begin" { Tok::BeginEnv } else { Tok::EndEnv },
                        name,
                        pos,
                        i + mlen,
                        &space,
                    ),
                });
            }
            let mut post_space = String::new();
            let mut j = end;
            if isalphamacro {
                while let Some(c) = char_at(s, j) {
                    if !is_space(c) {
                        break;
                    }
                    post_space.push(c);
                    j += c.len_utf8();
                    if post_space.ends_with("\n\n") {
                        post_space.truncate(post_space.len() - 2);
                        j -= 2;
                        break;
                    }
                }
            }
            return Ok(Token {
                tok: Tok::Macro,
                arg: mac,
                pos,
                len: j - pos,
                pre_space: space,
                post_space,
            });
        }

        if c0 == '%' {
            // (\n|\r|\n\r)(?P<extraspace>\s*)
            let (arglen, mlen, mspace) = match s[pos..].find(['\n', '\r']) {
                Some(off) => {
                    let mstart = pos + off;
                    let mut k = mstart + 1;
                    while let Some(c) = char_at(s, k) {
                        if !is_space(c) {
                            break;
                        }
                        k += c.len_utf8();
                    }
                    let extraspace = &s[mstart + 1..k];
                    if extraspace.starts_with(['\n', '\r']) {
                        (mstart - pos, mstart - pos, "")
                    } else {
                        (mstart - pos, k - pos, &s[mstart..k])
                    }
                }
                None => (s.len() - pos, s.len() - pos, ""),
            };
            return Ok(Token {
                tok: Tok::Comment,
                arg: s[pos + 1..pos + arglen].to_string(),
                pos,
                len: mlen,
                pre_space: space,
                post_space: mspace.to_string(),
            });
        }

        let is_open = c0 == '{' || include_brace_chars.iter().any(|(o, _)| *o == c0);
        if is_open {
            return Ok(tok(Tok::BraceOpen, c0.to_string(), pos, 1, &space));
        }
        let is_close = c0 == '}' || include_brace_chars.iter().any(|(_, c)| *c == c0);
        if is_close {
            return Ok(tok(Tok::BraceClose, c0.to_string(), pos, 1, &space));
        }
        if s[pos..].starts_with("$$")
            && !(ps.in_math_mode && ps.math_mode_delimiter.as_deref() == Some("$"))
        {
            return Ok(tok(Tok::MathDisplay, "$$".into(), pos, 2, &space));
        }
        if c0 == '$' {
            return Ok(tok(Tok::MathInline, "$".into(), pos, 1, &space));
        }
        if let Some(spec) = self.ctx.test_for_specials(s, pos) {
            let chars = spec.specials_chars.clone();
            let len = chars.len();
            return Ok(tok(Tok::Specials, chars, pos, len, &space));
        }
        Ok(tok(Tok::Char, c0.to_string(), pos, c0.len_utf8(), &space))
    }

    // ----------------------------------------------------------------------- //
    // expressions, groups, environments
    // ----------------------------------------------------------------------- //

    /// `get_latex_expression(pos, strict_braces=False)`: a single token or a
    /// braced group. Returns the node and its `(pos, len)`.
    pub fn get_latex_expression(&self, pos: usize, ps: &ParsingState) -> WResult<(Node, usize, usize)> {
        let tok = self.get_token(pos, &[], false, ps)?;
        let made = |n: Node| {
            let (p, l) = (n.pos, n.len);
            Ok((n, p, l))
        };
        match tok.tok {
            Tok::Macro => {
                if tok.arg == "end" {
                    return made(self.chars_node("", tok.pos, 0));
                }
                made(self.node(
                    NodeKind::Macro {
                        macroname: tok.arg,
                        nodeargd: None,
                        macro_post_space: tok.post_space,
                        bare_expression: true,
                    },
                    tok.pos,
                    tok.len,
                ))
            }
            Tok::Specials => made(self.node(
                NodeKind::Specials {
                    specials_chars: tok.arg,
                    nodeargd: None,
                },
                tok.pos,
                tok.len,
            )),
            Tok::Comment => self.get_latex_expression(tok.pos + tok.len, ps),
            Tok::BraceOpen => self.get_latex_braced_group(tok.pos, '{', ps),
            Tok::BraceClose => made(self.chars_node("", tok.pos, 0)),
            Tok::Char => made(self.chars_node(&tok.arg, tok.pos, tok.len)),
            Tok::MathInline | Tok::MathDisplay => {
                if tok.arg.starts_with('\\') {
                    made(self.node(
                        NodeKind::Macro {
                            macroname: tok.arg,
                            nodeargd: Some(ParsedArgs::default()),
                            macro_post_space: tok.post_space,
                            bare_expression: true,
                        },
                        tok.pos,
                        tok.len,
                    ))
                } else {
                    made(self.chars_node(&tok.arg, tok.pos, tok.len))
                }
            }
            Tok::BeginEnv | Tok::EndEnv => Err(WalkError::Parse {
                msg: "Unknown token type".into(),
                pos,
            }),
        }
    }

    /// `get_latex_maybe_optional_arg(pos)`: a `[…]` group, if one follows.
    pub fn get_latex_maybe_optional_arg(
        &self,
        pos: usize,
        ps: &ParsingState,
    ) -> WResult<Option<(Node, usize, usize)>> {
        let tok = match self.get_token(pos, &[('[', ']')], false, ps) {
            Ok(t) => t,
            Err(WalkError::EndOfStream { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        if tok.tok == Tok::BraceOpen && tok.arg == "[" {
            return self.get_latex_braced_group(pos, '[', ps).map(Some);
        }
        Ok(None)
    }

    /// `get_latex_braced_group(pos, brace_type)`.
    pub fn get_latex_braced_group(
        &self,
        pos: usize,
        brace_type: char,
        ps: &ParsingState,
    ) -> WResult<(Node, usize, usize)> {
        let closing = match brace_type {
            '{' => '}',
            '[' => ']',
            '(' => ')',
            '<' => '>',
            other => other,
        };
        let include: Vec<(char, char)> = if brace_type == '{' {
            vec![]
        } else {
            vec![(brace_type, closing)]
        };
        let firsttok = self.get_token(pos, &include, true, ps)?;
        if firsttok.tok != Tok::BraceOpen || firsttok.arg != brace_type.to_string() {
            return Err(WalkError::Parse {
                msg: "get_latex_braced_group: not an opening brace/bracket".into(),
                pos,
            });
        }
        let stop = Stop {
            closing_brace: Some((brace_type, closing)),
            ..Stop::default()
        };
        let (nodelist, npos, nlen) = self.get_latex_nodes(firsttok.pos + firsttok.len, &stop, ps)?;
        let len = npos + nlen - firsttok.pos;
        let n = self.node(
            NodeKind::Group {
                nodelist,
                delimiters: (brace_type.to_string(), closing.to_string()),
            },
            firsttok.pos,
            len,
        );
        Ok((n, firsttok.pos, len))
    }

    /// `get_latex_environment(pos, environmentname)`.
    pub fn get_latex_environment(
        &self,
        pos: usize,
        environmentname: &str,
        ps: &ParsingState,
    ) -> WResult<(Node, usize, usize)> {
        let startpos = pos;
        let firsttok = self.get_token(pos, &[], true, ps)?;
        if firsttok.tok != Tok::BeginEnv || firsttok.arg != environmentname {
            return Err(WalkError::Parse {
                msg: format!("get_latex_environment: expected \\begin{{{environmentname}}}"),
                pos,
            });
        }
        let pos = firsttok.pos + firsttok.len;
        let (parser, is_math) = match self.ctx.get_environment_spec(environmentname) {
            Some(spec) => (spec.args_parser.clone(), spec.is_math_mode),
            None => (ArgsParser::standard(""), false),
        };
        let (argd, apos, alen) = match self.parse_args(&parser, pos, ps) {
            Ok(r) => r,
            Err(_) => (None, pos, 0), // tolerant
        };
        let pos = apos + alen;
        let inner = if is_math {
            ps.math(&format!("{{{environmentname}}}"))
        } else {
            ps.clone()
        };
        let stop = Stop {
            end_environment: Some(environmentname.to_string()),
            ..Stop::default()
        };
        let (nodelist, npos, nlen) = self.get_latex_nodes(pos, &stop, &inner)?;
        let len = npos + nlen - startpos;
        let n = self.node(
            NodeKind::Environment {
                environmentname: environmentname.to_string(),
                nodelist,
                nodeargd: argd,
            },
            startpos,
            len,
        );
        Ok((n, startpos, len))
    }

    // ----------------------------------------------------------------------- //
    // argument parsing (macrospec._argparsers)
    // ----------------------------------------------------------------------- //

    /// `args_parser.parse_args(w, pos, parsing_state)`: the parsed arguments
    /// plus `(pos, len)` of the consumed span.
    fn parse_args(
        &self,
        parser: &ArgsParser,
        pos: usize,
        ps: &ParsingState,
    ) -> WResult<(Option<ParsedArgs>, usize, usize)> {
        match parser {
            ArgsParser::Standard {
                argspec,
                optional_arg_no_space,
                args_math_mode,
            } => {
                let inner = |j: usize| -> ParsingState {
                    match args_math_mode.as_ref().and_then(|a| a.get(j).copied().flatten()) {
                        Some(flag) if flag != ps.in_math_mode => ps.with_math_mode(flag),
                        _ => ps.clone(),
                    }
                };
                let mut p = pos;
                let mut argnlist = Vec::new();
                for (j, argt) in argspec.chars().enumerate() {
                    match argt {
                        '{' => {
                            let (node, np, nl) = self.get_latex_expression(p, &inner(j))?;
                            p = np + nl;
                            argnlist.push(Some(node));
                        }
                        '[' => {
                            if *optional_arg_no_space && char_at(&self.s, p).is_some_and(is_space) {
                                argnlist.push(None);
                                continue;
                            }
                            match self.get_latex_maybe_optional_arg(p, &inner(j))? {
                                None => argnlist.push(None),
                                Some((node, np, nl)) => {
                                    p = np + nl;
                                    argnlist.push(Some(node));
                                }
                            }
                        }
                        '*' => {
                            // pylatexenc reads the star with the default state
                            let tok = self.get_token(p, &[], true, &ParsingState::default())?;
                            if tok.tok == Tok::Char && tok.arg.starts_with('*') {
                                argnlist.push(Some(self.chars_node("*", tok.pos, 1)));
                                p = tok.pos + 1;
                            } else {
                                argnlist.push(None);
                            }
                        }
                        other => {
                            return Err(WalkError::Parse {
                                msg: format!("Unknown macro argument kind: {other:?}"),
                                pos,
                            })
                        }
                    }
                }
                Ok((
                    Some(ParsedArgs {
                        argspec: argspec.to_string(),
                        argnlist,
                        verbatim_text: None,
                        verbatim_delimiters: None,
                    }),
                    pos,
                    p - pos,
                ))
            }
            ArgsParser::Verbatim(VerbatimKind::VerbatimEnvironment) => {
                let s = &*self.s;
                let Some(off) = s[pos..].find("\\end{verbatim}") else {
                    return Err(WalkError::Parse {
                        msg: "Cannot find matching \\end{verbatim}".into(),
                        pos,
                    });
                };
                let text = &s[pos..pos + off];
                Ok((
                    Some(ParsedArgs {
                        argspec: "{".into(),
                        argnlist: vec![Some(self.chars_node(text, pos, off))],
                        verbatim_text: Some(text.to_string()),
                        verbatim_delimiters: None,
                    }),
                    pos,
                    off,
                ))
            }
            ArgsParser::Verbatim(VerbatimKind::VerbMacro) => {
                let s = &*self.s;
                let mut pos = pos;
                loop {
                    match char_at(s, pos) {
                        Some(c) if is_space(c) => pos += c.len_utf8(),
                        Some(_) => break,
                        None => {
                            return Err(WalkError::Parse {
                                msg: "Missing argument to \\verb command".into(),
                                pos,
                            })
                        }
                    }
                }
                let delim = char_at(s, pos).expect("checked above");
                let beginpos = pos + delim.len_utf8();
                let Some(off) = s[beginpos..].find(delim) else {
                    return Err(WalkError::Parse {
                        msg: "End of stream reached while reading argument to \\verb command".into(),
                        pos,
                    });
                };
                let endpos = beginpos + off;
                let text = &s[beginpos..endpos];
                Ok((
                    Some(ParsedArgs {
                        argspec: "{".into(),
                        argnlist: vec![Some(self.chars_node(text, beginpos, endpos - beginpos))],
                        verbatim_text: Some(text.to_string()),
                        verbatim_delimiters: Some((delim, delim)),
                    }),
                    pos,
                    endpos + delim.len_utf8() - pos,
                ))
            }
        }
    }

    // ----------------------------------------------------------------------- //
    // node lists
    // ----------------------------------------------------------------------- //

    /// `get_latex_nodes(pos, stop_upon_…, parsing_state)`: returns the node
    /// list and the `(pos, len)` of the consumed span.
    pub fn get_latex_nodes(
        &self,
        pos: usize,
        stop: &Stop,
        ps: &ParsingState,
    ) -> WResult<(Vec<Node>, usize, usize)> {
        let include: Vec<(char, char)> = match stop.closing_brace {
            Some((open, close)) if close != '}' => vec![(open, close)],
            _ => vec![],
        };
        let origpos = pos;
        let mut nodelist = Vec::new();
        let mut p = PosPointer {
            pos,
            ps: ps.clone(),
            lastchars: String::new(),
            lastchars_pos: None,
        };
        loop {
            let r = match self.do_read(&mut nodelist, &mut p, stop, &include) {
                Ok(r) => r,
                // tolerant: ignore the error and keep reading
                Err(WalkError::Parse { .. }) => ReadResult::Continue,
                Err(WalkError::EndOfStream { final_space }) => {
                    if stop.closing_brace.is_some()
                        || stop.end_environment.is_some()
                        || stop.closing_mathmode.is_some()
                    {
                        ReadResult::Stop
                    } else {
                        ReadResult::Eos(final_space)
                    }
                }
            };
            match r {
                ReadResult::Continue => continue,
                ReadResult::Stop | ReadResult::Eos(_) => {
                    if let ReadResult::Eos(final_space) = r {
                        let at = p.pos;
                        p.push_lastchars(at, &final_space);
                        p.pos += final_space.len();
                    }
                    if !p.lastchars.is_empty() {
                        let (charspos, chars) = p.flush_lastchars();
                        let len = chars.len();
                        nodelist.push(self.node(NodeKind::Chars { chars }, charspos, len));
                    }
                    return Ok((nodelist, origpos, p.pos - origpos));
                }
            }
        }
    }

    fn reached_max(stop: &Stop, nodelist: &[Node]) -> bool {
        stop.read_max_nodes.is_some_and(|m| m > 0 && nodelist.len() >= m)
    }

    fn do_read(
        &self,
        nodelist: &mut Vec<Node>,
        p: &mut PosPointer,
        stop: &Stop,
        include: &[(char, char)],
    ) -> WResult<ReadResult> {
        let tok = match self.get_token(p.pos, include, true, &p.ps) {
            Ok(t) => t,
            Err(WalkError::EndOfStream { final_space }) => return Ok(ReadResult::Eos(final_space)),
            Err(e) => return Err(e),
        };
        p.pos = tok.pos + tok.len;
        if tok.tok == Tok::Char {
            p.push_lastchars(tok.pos - tok.pre_space.len(), &format!("{}{}", tok.pre_space, tok.arg));
            return Ok(ReadResult::Continue);
        }
        if !p.lastchars.is_empty() {
            let (charspos, chars) = p.flush_lastchars();
            nodelist.push(self.node(
                NodeKind::Chars {
                    chars: format!("{chars}{}", tok.pre_space),
                },
                charspos,
                tok.pos - charspos,
            ));
            if Self::reached_max(stop, nodelist) {
                p.pos = tok.pos;
                return Ok(ReadResult::Stop);
            }
        } else if !tok.pre_space.is_empty() {
            let n = tok.pre_space.len();
            nodelist.push(self.chars_node(&tok.pre_space, tok.pos - n, n));
            if Self::reached_max(stop, nodelist) {
                p.pos = tok.pos;
                return Ok(ReadResult::Stop);
            }
        }

        match tok.tok {
            Tok::BraceClose => {
                if stop.closing_brace.map(|(_, c)| c.to_string()) != Some(tok.arg.clone()) {
                    return Err(WalkError::Parse {
                        msg: format!("Unexpected mismatching closing brace: '{}'", tok.arg),
                        pos: tok.pos,
                    });
                }
                Ok(ReadResult::Stop)
            }
            Tok::EndEnv => match &stop.end_environment {
                None => Err(WalkError::Parse {
                    msg: format!("Unexpected closing environment: '{}'", tok.arg),
                    pos: tok.pos,
                }),
                Some(env) if *env != tok.arg => Err(WalkError::Parse {
                    msg: format!(
                        "Unexpected mismatching closing environment: '{}', was expecting '{env}'",
                        tok.arg
                    ),
                    pos: tok.pos,
                }),
                Some(_) => Ok(ReadResult::Stop),
            },
            Tok::MathInline | Tok::MathDisplay => {
                if let Some(closing) = &stop.closing_mathmode {
                    if tok.arg == *closing {
                        return Ok(ReadResult::Stop);
                    }
                    if tok.arg == "\\)" || tok.arg == "\\]" {
                        return Err(WalkError::Parse {
                            msg: format!("Mismatching closing math mode: '{}'", tok.arg),
                            pos: tok.pos,
                        });
                    }
                } else if tok.arg == "\\)" || tok.arg == "\\]" {
                    return Err(WalkError::Parse {
                        msg: format!("Unexpected closing math mode: '{}'", tok.arg),
                        pos: tok.pos,
                    });
                }
                let closing = match tok.arg.as_str() {
                    "\\(" => "\\)".to_string(),
                    "\\[" => "\\]".to_string(),
                    other => other.to_string(),
                };
                let displaytype = if tok.arg == "\\(" || tok.arg == "$" { "inline" } else { "display" };
                let inner = p.ps.math(&tok.arg);
                let mstop = Stop {
                    closing_mathmode: Some(closing.clone()),
                    ..Stop::default()
                };
                let (nl, mpos, mlen) = self.get_latex_nodes(p.pos, &mstop, &inner)?;
                p.pos = mpos + mlen;
                nodelist.push(self.node(
                    NodeKind::Math {
                        displaytype: displaytype.into(),
                        nodelist: nl,
                        delimiters: (tok.arg.clone(), closing),
                    },
                    tok.pos,
                    mpos + mlen - tok.pos,
                ));
                Ok(if Self::reached_max(stop, nodelist) {
                    ReadResult::Stop
                } else {
                    ReadResult::Continue
                })
            }
            Tok::Comment => {
                nodelist.push(self.node(
                    NodeKind::Comment {
                        comment: tok.arg,
                        comment_post_space: tok.post_space,
                    },
                    tok.pos,
                    tok.len,
                ));
                Ok(if Self::reached_max(stop, nodelist) {
                    ReadResult::Stop
                } else {
                    ReadResult::Continue
                })
            }
            Tok::BraceOpen => {
                let brace = tok.arg.chars().next().unwrap_or('{');
                let (group, bpos, blen) = self.get_latex_braced_group(tok.pos, brace, &p.ps)?;
                p.pos = bpos + blen;
                nodelist.push(group);
                Ok(if Self::reached_max(stop, nodelist) {
                    ReadResult::Stop
                } else {
                    ReadResult::Continue
                })
            }
            Tok::BeginEnv => {
                let (env, epos, elen) = self.get_latex_environment(tok.pos, &tok.arg, &p.ps)?;
                p.pos = epos + elen;
                nodelist.push(env);
                Ok(if Self::reached_max(stop, nodelist) {
                    ReadResult::Stop
                } else {
                    ReadResult::Continue
                })
            }
            Tok::Macro => {
                let parser = self
                    .ctx
                    .get_macro_spec(&tok.arg)
                    .map(|m| m.args_parser.clone())
                    .unwrap_or_else(|| ArgsParser::standard(""));
                let after = tok.pos + tok.len;
                let (nodeargd, mapos, malen) = match self.parse_args(&parser, after, &p.ps) {
                    Ok(r) => r,
                    Err(_) => (None, after, 0), // tolerant
                };
                p.pos = mapos + malen;
                nodelist.push(self.node(
                    NodeKind::Macro {
                        macroname: tok.arg,
                        nodeargd,
                        macro_post_space: tok.post_space,
                        bare_expression: false,
                    },
                    tok.pos,
                    p.pos - tok.pos,
                ));
                Ok(if Self::reached_max(stop, nodelist) {
                    ReadResult::Stop
                } else {
                    ReadResult::Continue
                })
            }
            Tok::Specials => {
                // specials carry no argument parser in tex2word's databases
                p.pos = tok.pos + tok.len;
                nodelist.push(self.node(
                    NodeKind::Specials {
                        specials_chars: tok.arg,
                        nodeargd: None,
                    },
                    tok.pos,
                    p.pos - tok.pos,
                ));
                Ok(if Self::reached_max(stop, nodelist) {
                    ReadResult::Stop
                } else {
                    ReadResult::Continue
                })
            }
            Tok::Char => unreachable!("handled above"),
        }
    }
}
