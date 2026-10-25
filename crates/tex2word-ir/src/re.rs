//! A regular-expression engine compatible with Python's `re` module.
//!
//! The Python reference does much of its source rewriting with `re` (181 call
//! sites using lookahead/lookbehind, backreferences, named groups, lazy
//! quantifiers, `DOTALL`/`MULTILINE`…). Python's `sre` is a backtracking
//! engine with leftmost-first alternation, so a backtracking engine is what
//! reproduces its results exactly; this one is a small VM with an explicit
//! heap-allocated backtrack stack (no recursion per repetition, so scanning a
//! whole document with `.*?` cannot overflow the stack).
//!
//! Semantics follow CPython: Unicode `\w`/`\d`/`\s`/`\b` (via [`crate::pystr`]),
//! `.` excludes `\n` unless `DOTALL`, `$` also matches before a final
//! newline, empty matches in `sub`/`finditer`/`split` follow the Python 3.7+
//! rules, and replacement templates support `\1`, `\g<1>`, `\g<name>` and the
//! character escapes. Positions are byte offsets into the subject string.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use crate::pystr::{is_decimal, is_space, is_word};

// --------------------------------------------------------------------------- //
// Flags
// --------------------------------------------------------------------------- //

/// Pattern flags (`re.I`, `re.S`, `re.M`, `re.X`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags(u8);

impl Flags {
    pub const NONE: Flags = Flags(0);
    /// `re.IGNORECASE`
    pub const I: Flags = Flags(1);
    /// `re.DOTALL`
    pub const S: Flags = Flags(2);
    /// `re.MULTILINE`
    pub const M: Flags = Flags(4);
    /// `re.VERBOSE`
    pub const X: Flags = Flags(8);

    pub fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Flags {
    type Output = Flags;
    fn bitor(self, rhs: Flags) -> Flags {
        Flags(self.0 | rhs.0)
    }
}

/// A pattern syntax error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

// --------------------------------------------------------------------------- //
// AST
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone, PartialEq)]
enum ClassItem {
    Ch(char),
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

#[derive(Debug, Clone, PartialEq)]
struct Class {
    negated: bool,
    items: Vec<ClassItem>,
}

impl Class {
    fn single(item: ClassItem) -> Class {
        Class {
            negated: false,
            items: vec![item],
        }
    }

    fn matches_raw(&self, c: char) -> bool {
        self.items.iter().any(|it| match *it {
            ClassItem::Ch(x) => x == c,
            ClassItem::Range(a, b) => a <= c && c <= b,
            ClassItem::Digit(neg) => is_decimal(c) != neg,
            ClassItem::Word(neg) => is_word(c) != neg,
            ClassItem::Space(neg) => is_space(c) != neg,
        })
    }

    fn matches(&self, c: char, icase: bool) -> bool {
        let hit = self.matches_raw(c)
            || (icase && (lower(c).is_some_and(|l| self.matches_raw(l)) || upper(c).is_some_and(|u| self.matches_raw(u))));
        hit != self.negated
    }
}

fn lower(c: char) -> Option<char> {
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(l), None) => Some(l),
        _ => None,
    }
}

fn upper(c: char) -> Option<char> {
    let mut it = c.to_uppercase();
    match (it.next(), it.next()) {
        (Some(u), None) => Some(u),
        _ => None,
    }
}

fn eq_icase(a: char, b: char) -> bool {
    a == b || lower(a).is_some_and(|x| Some(x) == lower(b))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssertKind {
    /// `^`
    Start,
    /// `$`
    End,
    /// `\A`
    StartText,
    /// `\Z`
    EndText,
    /// `\b`
    WordB,
    /// `\B`
    NotWordB,
}

#[derive(Debug, Clone, PartialEq)]
enum Ast {
    Empty,
    Char(char),
    Any,
    Class(Class),
    Assert(AssertKind),
    Group(Box<Ast>, Option<usize>),
    Backref(usize),
    Look {
        ahead: bool,
        negate: bool,
        node: Box<Ast>,
    },
    Concat(Vec<Ast>),
    Alt(Vec<Ast>),
    Repeat {
        node: Box<Ast>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    },
}

impl Ast {
    /// Can this node match the empty string?
    fn nullable(&self) -> bool {
        match self {
            Ast::Empty | Ast::Assert(_) | Ast::Look { .. } | Ast::Backref(_) => true,
            Ast::Char(_) | Ast::Any | Ast::Class(_) => false,
            Ast::Group(n, _) => n.nullable(),
            Ast::Concat(v) => v.iter().all(Ast::nullable),
            Ast::Alt(v) => v.iter().any(Ast::nullable),
            Ast::Repeat { node, min, .. } => *min == 0 || node.nullable(),
        }
    }

    /// Fixed match width in chars (for lookbehind), if any.
    fn width(&self) -> Option<usize> {
        match self {
            Ast::Empty | Ast::Assert(_) | Ast::Look { .. } => Some(0),
            Ast::Char(_) | Ast::Any | Ast::Class(_) => Some(1),
            Ast::Group(n, _) => n.width(),
            Ast::Backref(_) => None,
            Ast::Concat(v) => v.iter().map(Ast::width).sum(),
            Ast::Alt(v) => {
                let w = v.first()?.width()?;
                v.iter().all(|x| x.width() == Some(w)).then_some(w)
            }
            Ast::Repeat { node, min, max, .. } => {
                let w = node.width()?;
                (Some(*min) == *max).then(|| w * *min as usize)
            }
        }
    }
}

// --------------------------------------------------------------------------- //
// Parser (Python `sre_parse` syntax)
// --------------------------------------------------------------------------- //

struct PatParser<'a> {
    chars: Vec<char>,
    i: usize,
    flags: Flags,
    ngroups: usize,
    names: Vec<(String, usize)>,
    _src: &'a str,
}

impl PatParser<'_> {
    fn err(&self, msg: &str) -> Error {
        Error(format!("{msg} at position {}", self.i))
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn skip_verbose(&mut self) {
        if !self.flags.contains(Flags::X) {
            return;
        }
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => self.i += 1,
                Some('#') => {
                    while let Some(c) = self.peek() {
                        self.i += 1;
                        if c == '\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    fn parse_alt(&mut self) -> Result<Ast, Error> {
        let mut alts = vec![self.parse_concat()?];
        while self.eat('|') {
            alts.push(self.parse_concat()?);
        }
        Ok(if alts.len() == 1 {
            alts.pop().unwrap()
        } else {
            Ast::Alt(alts)
        })
    }

    fn parse_concat(&mut self) -> Result<Ast, Error> {
        let mut items = Vec::new();
        loop {
            self.skip_verbose();
            match self.peek() {
                None | Some('|') | Some(')') => break,
                _ => {}
            }
            let atom = self.parse_atom()?;
            let atom = self.parse_quantifier(atom)?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Ast::Empty,
            1 => items.pop().unwrap(),
            _ => Ast::Concat(items),
        })
    }

    fn parse_number(&mut self) -> Option<u32> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i == start {
            None
        } else {
            self.chars[start..self.i].iter().collect::<String>().parse().ok()
        }
    }

    fn parse_quantifier(&mut self, atom: Ast) -> Result<Ast, Error> {
        let mut atom = atom;
        loop {
            self.skip_verbose();
            let (min, max) = match self.peek() {
                Some('*') => {
                    self.i += 1;
                    (0, None)
                }
                Some('+') => {
                    self.i += 1;
                    (1, None)
                }
                Some('?') => {
                    self.i += 1;
                    (0, Some(1))
                }
                Some('{') => {
                    // {m}, {m,}, {,n}, {m,n}; anything else is a literal '{'
                    let save = self.i;
                    self.i += 1;
                    let lo = self.parse_number();
                    let (lo, hi) = if self.eat(',') {
                        let hi = self.parse_number();
                        (lo.unwrap_or(0), hi)
                    } else {
                        match lo {
                            Some(v) => (v, Some(v)),
                            None => {
                                self.i = save;
                                return Ok(atom);
                            }
                        }
                    };
                    if !self.eat('}') {
                        self.i = save;
                        return Ok(atom);
                    }
                    (lo, hi)
                }
                _ => return Ok(atom),
            };
            if matches!(atom, Ast::Assert(_) | Ast::Empty) {
                return Err(self.err("nothing to repeat"));
            }
            if matches!(atom, Ast::Repeat { .. }) {
                return Err(self.err("multiple repeat"));
            }
            let greedy = !self.eat('?');
            atom = Ast::Repeat {
                node: Box::new(atom),
                min,
                max,
                greedy,
            };
        }
    }

    fn parse_hex(&mut self, n: usize) -> Result<char, Error> {
        let s: String = self.chars.get(self.i..self.i + n).ok_or_else(|| self.err("incomplete escape"))?.iter().collect();
        let v = u32::from_str_radix(&s, 16).map_err(|_| self.err("bad hex escape"))?;
        self.i += n;
        char::from_u32(v).ok_or_else(|| self.err("bad codepoint"))
    }

    /// An escape shared by classes and atoms; `None` means "not a char escape".
    fn char_escape(&mut self, c: char, in_class: bool) -> Result<Option<char>, Error> {
        Ok(Some(match c {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'f' => '\x0c',
            'v' => '\x0b',
            'a' => '\x07',
            'b' if in_class => '\x08',
            'x' => self.parse_hex(2)?,
            'u' => self.parse_hex(4)?,
            'U' => self.parse_hex(8)?,
            '0' => {
                let mut v = 0u32;
                for _ in 0..2 {
                    match self.peek() {
                        Some(d @ '0'..='7') => {
                            v = v * 8 + d.to_digit(8).unwrap();
                            self.i += 1;
                        }
                        _ => break,
                    }
                }
                char::from_u32(v).unwrap()
            }
            c if c.is_ascii_alphanumeric() => return Ok(None),
            c => c, // escaped punctuation is literal
        }))
    }

    fn parse_class(&mut self) -> Result<Ast, Error> {
        // the '[' is consumed
        let mut cls = Class {
            negated: self.eat('^'),
            items: Vec::new(),
        };
        let mut first = true;
        loop {
            let c = self.peek().ok_or_else(|| self.err("unterminated character set"))?;
            if c == ']' && !first {
                self.i += 1;
                break;
            }
            first = false;
            self.i += 1;
            let lo = if c == '\\' {
                let e = self.peek().ok_or_else(|| self.err("bad escape (end of pattern)"))?;
                self.i += 1;
                match e {
                    'd' => {
                        cls.items.push(ClassItem::Digit(false));
                        continue;
                    }
                    'D' => {
                        cls.items.push(ClassItem::Digit(true));
                        continue;
                    }
                    'w' => {
                        cls.items.push(ClassItem::Word(false));
                        continue;
                    }
                    'W' => {
                        cls.items.push(ClassItem::Word(true));
                        continue;
                    }
                    's' => {
                        cls.items.push(ClassItem::Space(false));
                        continue;
                    }
                    'S' => {
                        cls.items.push(ClassItem::Space(true));
                        continue;
                    }
                    _ => self
                        .char_escape(e, true)?
                        .ok_or_else(|| self.err(&format!("bad escape \\{e}")))?,
                }
            } else {
                c
            };
            // a range lo-hi (a '-' before ']' is literal)
            if self.peek() == Some('-') && self.chars.get(self.i + 1).is_some_and(|&n| n != ']') {
                self.i += 1;
                let h = self.peek().unwrap();
                self.i += 1;
                let hi = if h == '\\' {
                    let e = self.peek().ok_or_else(|| self.err("bad escape"))?;
                    self.i += 1;
                    self.char_escape(e, true)?
                        .ok_or_else(|| self.err("bad character range"))?
                } else {
                    h
                };
                if hi < lo {
                    return Err(self.err("bad character range"));
                }
                cls.items.push(ClassItem::Range(lo, hi));
            } else {
                cls.items.push(ClassItem::Ch(lo));
            }
        }
        Ok(Ast::Class(cls))
    }

    fn parse_group_name(&mut self, end: char) -> Result<String, Error> {
        let start = self.i;
        while self.peek().is_some_and(|c| c != end) {
            self.i += 1;
        }
        let name: String = self.chars[start..self.i].iter().collect();
        if !self.eat(end) || name.is_empty() {
            return Err(self.err("bad group name"));
        }
        Ok(name)
    }

    fn parse_atom(&mut self) -> Result<Ast, Error> {
        let c = self.peek().unwrap();
        self.i += 1;
        Ok(match c {
            '.' => Ast::Any,
            '^' => Ast::Assert(AssertKind::Start),
            '$' => Ast::Assert(AssertKind::End),
            '[' => self.parse_class()?,
            '(' => {
                if self.eat('?') {
                    match self.peek() {
                        Some(':') => {
                            self.i += 1;
                            let inner = self.parse_alt()?;
                            self.close()?;
                            Ast::Group(Box::new(inner), None)
                        }
                        Some('P') => {
                            self.i += 1;
                            if self.eat('<') {
                                let name = self.parse_group_name('>')?;
                                self.ngroups += 1;
                                let idx = self.ngroups;
                                self.names.push((name, idx));
                                let inner = self.parse_alt()?;
                                self.close()?;
                                Ast::Group(Box::new(inner), Some(idx))
                            } else if self.eat('=') {
                                let name = self.parse_group_name(')')?;
                                let idx = self
                                    .names
                                    .iter()
                                    .find(|(n, _)| *n == name)
                                    .map(|(_, i)| *i)
                                    .ok_or_else(|| self.err("unknown group name"))?;
                                Ast::Backref(idx)
                            } else {
                                return Err(self.err("unknown extension ?P"));
                            }
                        }
                        Some('=') | Some('!') => {
                            let negate = self.peek() == Some('!');
                            self.i += 1;
                            let inner = self.parse_alt()?;
                            self.close()?;
                            Ast::Look {
                                ahead: true,
                                negate,
                                node: Box::new(inner),
                            }
                        }
                        Some('<') if matches!(self.chars.get(self.i + 1), Some('=') | Some('!')) => {
                            let negate = self.chars[self.i + 1] == '!';
                            self.i += 2;
                            let inner = self.parse_alt()?;
                            self.close()?;
                            if inner.width().is_none() {
                                return Err(self.err("look-behind requires fixed-width pattern"));
                            }
                            Ast::Look {
                                ahead: false,
                                negate,
                                node: Box::new(inner),
                            }
                        }
                        Some('<') => {
                            // (?<name>…) — accepted since Python 3.? as an alias
                            self.i += 1;
                            let name = self.parse_group_name('>')?;
                            self.ngroups += 1;
                            let idx = self.ngroups;
                            self.names.push((name, idx));
                            let inner = self.parse_alt()?;
                            self.close()?;
                            Ast::Group(Box::new(inner), Some(idx))
                        }
                        Some('#') => {
                            while self.peek().is_some_and(|c| c != ')') {
                                self.i += 1;
                            }
                            self.close()?;
                            Ast::Empty
                        }
                        Some(c) if "aiLmsux".contains(c) => {
                            // global inline flags (?imsx)
                            while let Some(c) = self.peek() {
                                match c {
                                    'i' => self.flags = self.flags | Flags::I,
                                    's' => self.flags = self.flags | Flags::S,
                                    'm' => self.flags = self.flags | Flags::M,
                                    'x' => self.flags = self.flags | Flags::X,
                                    'a' | 'L' | 'u' => {}
                                    _ => break,
                                }
                                self.i += 1;
                            }
                            if self.eat(':') {
                                // scoped flags: approximated as global
                                let inner = self.parse_alt()?;
                                self.close()?;
                                Ast::Group(Box::new(inner), None)
                            } else {
                                self.close()?;
                                Ast::Empty
                            }
                        }
                        _ => return Err(self.err("unknown extension")),
                    }
                } else {
                    self.ngroups += 1;
                    let idx = self.ngroups;
                    let inner = self.parse_alt()?;
                    self.close()?;
                    Ast::Group(Box::new(inner), Some(idx))
                }
            }
            '\\' => {
                let e = self.peek().ok_or_else(|| self.err("bad escape (end of pattern)"))?;
                self.i += 1;
                match e {
                    'd' => Ast::Class(Class::single(ClassItem::Digit(false))),
                    'D' => Ast::Class(Class::single(ClassItem::Digit(true))),
                    'w' => Ast::Class(Class::single(ClassItem::Word(false))),
                    'W' => Ast::Class(Class::single(ClassItem::Word(true))),
                    's' => Ast::Class(Class::single(ClassItem::Space(false))),
                    'S' => Ast::Class(Class::single(ClassItem::Space(true))),
                    'b' => Ast::Assert(AssertKind::WordB),
                    'B' => Ast::Assert(AssertKind::NotWordB),
                    'A' => Ast::Assert(AssertKind::StartText),
                    'Z' => Ast::Assert(AssertKind::EndText),
                    '1'..='9' => {
                        let mut n = e.to_digit(10).unwrap() as usize;
                        if let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
                            n = n * 10 + d as usize;
                            self.i += 1;
                        }
                        if n > self.ngroups {
                            return Err(self.err("invalid group reference"));
                        }
                        Ast::Backref(n)
                    }
                    _ => match self.char_escape(e, false)? {
                        Some(ch) => Ast::Char(ch),
                        None => return Err(self.err(&format!("bad escape \\{e}"))),
                    },
                }
            }
            c => Ast::Char(c),
        })
    }

    fn close(&mut self) -> Result<(), Error> {
        if self.eat(')') {
            Ok(())
        } else {
            Err(self.err("missing ), unterminated subpattern"))
        }
    }
}

// --------------------------------------------------------------------------- //
// Compiler -> VM program
// --------------------------------------------------------------------------- //

#[derive(Debug, Clone)]
enum Inst {
    Char(char),
    Any,
    Class(usize),
    /// try the first target, keep the second as a backtrack point
    Split(usize, usize),
    Jmp(usize),
    Save(usize),
    Assert(AssertKind),
    Backref(usize),
    Look {
        prog: usize,
        ahead: bool,
        negate: bool,
        width: usize,
    },
    /// record the position in a mark slot (empty-loop guard)
    Mark(usize),
    /// fail if the position equals the mark (the loop body matched empty)
    Progress(usize),
    Match,
}

#[derive(Debug, Clone, Default)]
struct Program {
    insts: Vec<Inst>,
}

struct Compiler {
    progs: Vec<Program>,
    classes: Vec<Class>,
    nslots: usize,
}

impl Compiler {
    fn compile_prog(&mut self, ast: &Ast) -> usize {
        let idx = self.progs.len();
        self.progs.push(Program::default());
        let mut insts = Vec::new();
        self.emit(ast, &mut insts);
        insts.push(Inst::Match);
        self.progs[idx].insts = insts;
        idx
    }

    fn emit(&mut self, ast: &Ast, out: &mut Vec<Inst>) {
        match ast {
            Ast::Empty => {}
            Ast::Char(c) => out.push(Inst::Char(*c)),
            Ast::Any => out.push(Inst::Any),
            Ast::Class(cls) => {
                self.classes.push(cls.clone());
                out.push(Inst::Class(self.classes.len() - 1));
            }
            Ast::Assert(k) => out.push(Inst::Assert(*k)),
            Ast::Group(inner, idx) => match idx {
                Some(i) => {
                    out.push(Inst::Save(2 * i));
                    self.emit(inner, out);
                    out.push(Inst::Save(2 * i + 1));
                }
                None => self.emit(inner, out),
            },
            Ast::Backref(i) => out.push(Inst::Backref(*i)),
            Ast::Look { ahead, negate, node } => {
                let width = if *ahead { 0 } else { node.width().unwrap_or(0) };
                let prog = self.compile_prog(node);
                out.push(Inst::Look {
                    prog,
                    ahead: *ahead,
                    negate: *negate,
                    width,
                });
            }
            Ast::Concat(items) => {
                for it in items {
                    self.emit(it, out);
                }
            }
            Ast::Alt(alts) => {
                // Split L1, next ; L1: alt0 ; Jmp end ; next: Split L2, … ; last
                let mut jumps = Vec::new();
                for (k, alt) in alts.iter().enumerate() {
                    if k + 1 < alts.len() {
                        let split = out.len();
                        out.push(Inst::Split(split + 1, 0));
                        self.emit(alt, out);
                        jumps.push(out.len());
                        out.push(Inst::Jmp(0));
                        let next = out.len();
                        out[split] = Inst::Split(split + 1, next);
                    } else {
                        self.emit(alt, out);
                    }
                }
                let end = out.len();
                for j in jumps {
                    out[j] = Inst::Jmp(end);
                }
            }
            Ast::Repeat {
                node,
                min,
                max,
                greedy,
            } => {
                for _ in 0..*min {
                    self.emit(node, out);
                }
                match max {
                    None => self.emit_star(node, *greedy, out),
                    Some(max) => {
                        // nested optionals: (x(x(x)?)?)?
                        let n = max.saturating_sub(*min);
                        let mut splits = Vec::new();
                        for _ in 0..n {
                            let split = out.len();
                            out.push(Inst::Split(0, 0));
                            splits.push(split);
                            self.emit(node, out);
                        }
                        let end = out.len();
                        for split in splits {
                            out[split] = if *greedy {
                                Inst::Split(split + 1, end)
                            } else {
                                Inst::Split(end, split + 1)
                            };
                        }
                    }
                }
            }
        }
    }

    fn emit_star(&mut self, node: &Ast, greedy: bool, out: &mut Vec<Inst>) {
        let guard = node.nullable();
        let mark = if guard {
            let m = self.nslots;
            self.nslots += 1;
            Some(m)
        } else {
            None
        };
        let split = out.len();
        out.push(Inst::Split(0, 0));
        if let Some(m) = mark {
            out.push(Inst::Mark(m));
        }
        self.emit(node, out);
        if let Some(m) = mark {
            out.push(Inst::Progress(m));
        }
        out.push(Inst::Jmp(split));
        let end = out.len();
        out[split] = if greedy {
            Inst::Split(split + 1, end)
        } else {
            Inst::Split(end, split + 1)
        };
    }
}

// --------------------------------------------------------------------------- //
// Regex + Match
// --------------------------------------------------------------------------- //

/// A compiled pattern (`re.Pattern`).
#[derive(Debug, Clone)]
pub struct Regex {
    pattern: String,
    flags: Flags,
    progs: Rc<Vec<Program>>,
    classes: Rc<Vec<Class>>,
    ngroups: usize,
    /// capture slots (2 per group incl. group 0) + mark slots
    nslots: usize,
    names: Rc<Vec<(String, usize)>>,
    /// a literal the match must start with (search skips ahead to it)
    prefix: Option<String>,
}

/// A successful match (`re.Match`).
#[derive(Debug, Clone)]
pub struct Match<'s> {
    s: &'s str,
    slots: Vec<Option<usize>>,
    names: Rc<Vec<(String, usize)>>,
}

impl<'s> Match<'s> {
    /// `m.start()`
    pub fn start(&self) -> usize {
        self.slots[0].unwrap()
    }

    /// `m.end()`
    pub fn end(&self) -> usize {
        self.slots[1].unwrap()
    }

    /// `m.span(i)`
    pub fn span_of(&self, i: usize) -> Option<(usize, usize)> {
        match (self.slots.get(2 * i)?, self.slots.get(2 * i + 1)?) {
            (Some(a), Some(b)) => Some((*a, *b)),
            _ => None,
        }
    }

    /// `m.start(i)` (`None` if the group didn't participate; Python gives -1).
    pub fn start_of(&self, i: usize) -> Option<usize> {
        self.span_of(i).map(|s| s.0)
    }

    /// `m.end(i)`.
    pub fn end_of(&self, i: usize) -> Option<usize> {
        self.span_of(i).map(|s| s.1)
    }

    /// `m.group(0)`
    pub fn as_str(&self) -> &'s str {
        &self.s[self.start()..self.end()]
    }

    /// `m.group(i)` (`None` for a group that didn't participate).
    pub fn group(&self, i: usize) -> Option<&'s str> {
        self.span_of(i).map(|(a, b)| &self.s[a..b])
    }

    /// `m.group(i) or ""`.
    pub fn group_str(&self, i: usize) -> &'s str {
        self.group(i).unwrap_or("")
    }

    /// `m.group(name)`.
    pub fn name(&self, name: &str) -> Option<&'s str> {
        let idx = self.names.iter().find(|(n, _)| n == name)?.1;
        self.group(idx)
    }

    /// The group index of a named group.
    pub fn name_index(&self, name: &str) -> Option<usize> {
        self.names.iter().find(|(n, _)| n == name).map(|(_, i)| *i)
    }

    /// Number of capturing groups.
    pub fn len(&self) -> usize {
        self.slots.len() / 2 - 1
    }

    /// Whether the pattern has no capturing groups.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `m.groups()`.
    pub fn groups(&self) -> Vec<Option<&'s str>> {
        (1..=self.len()).map(|i| self.group(i)).collect()
    }

    /// `m.expand(template)`.
    pub fn expand(&self, template: &str) -> String {
        expand_template(template, self)
    }

    /// The subject string.
    pub fn subject(&self) -> &'s str {
        self.s
    }
}

impl Regex {
    /// Compile `pattern` (panics on a syntax error — for static patterns).
    pub fn new(pattern: &str) -> Regex {
        Regex::with_flags(pattern, Flags::NONE)
    }

    /// Compile with flags (panics on a syntax error).
    pub fn with_flags(pattern: &str, flags: Flags) -> Regex {
        Regex::try_new(pattern, flags).unwrap_or_else(|e| panic!("invalid regex {pattern:?}: {e}"))
    }

    /// `re.compile(pattern, flags)`.
    pub fn try_new(pattern: &str, flags: Flags) -> Result<Regex, Error> {
        let mut p = PatParser {
            chars: pattern.chars().collect(),
            i: 0,
            flags,
            ngroups: 0,
            names: Vec::new(),
            _src: pattern,
        };
        let ast = p.parse_alt()?;
        if p.i != p.chars.len() {
            return Err(p.err("unbalanced parenthesis"));
        }
        let ngroups = p.ngroups;
        let flags = p.flags;
        let mut c = Compiler {
            progs: Vec::new(),
            classes: Vec::new(),
            nslots: 2 * (ngroups + 1),
        };
        let root = Ast::Group(Box::new(ast.clone()), Some(0));
        c.compile_prog(&root);
        let prefix = if flags.contains(Flags::I) {
            None
        } else {
            literal_prefix(&ast)
        };
        Ok(Regex {
            pattern: pattern.to_string(),
            flags,
            progs: Rc::new(c.progs),
            classes: Rc::new(c.classes),
            ngroups,
            nslots: c.nslots,
            names: Rc::new(p.names),
            prefix: prefix.filter(|s| !s.is_empty()),
        })
    }

    /// The pattern source.
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Number of capturing groups (`pattern.groups`).
    pub fn groups(&self) -> usize {
        self.ngroups
    }

    fn make_match<'s>(&self, s: &'s str, slots: Vec<Option<usize>>) -> Match<'s> {
        let mut slots = slots;
        slots.truncate(2 * (self.ngroups + 1));
        Match {
            s,
            slots,
            names: self.names.clone(),
        }
    }

    /// Try a match anchored at `pos`.
    fn match_here<'s>(&self, s: &'s str, pos: usize, must_advance: bool, full: bool) -> Option<Match<'s>> {
        let mut slots = vec![None; self.nslots];
        let vm = Vm {
            re: self,
            s,
            must_advance_from: if must_advance { Some(pos) } else { None },
            full,
        };
        if vm.run(0, pos, &mut slots, true).is_some() {
            Some(self.make_match(s, slots))
        } else {
            None
        }
    }

    fn search_impl<'s>(&self, s: &'s str, pos: usize, must_advance: bool) -> Option<Match<'s>> {
        let mut at = pos;
        loop {
            if let Some(prefix) = &self.prefix {
                match s[at..].find(prefix.as_str()) {
                    Some(off) => at += off,
                    None => return None,
                }
            }
            if let Some(m) = self.match_here(s, at, must_advance && at == pos, false) {
                return Some(m);
            }
            match s[at..].chars().next() {
                Some(c) => at += c.len_utf8(),
                None => return None,
            }
        }
    }

    /// `pattern.search(s)`.
    pub fn search<'s>(&self, s: &'s str) -> Option<Match<'s>> {
        self.search_impl(s, 0, false)
    }

    /// `pattern.search(s, pos)`.
    pub fn search_from<'s>(&self, s: &'s str, pos: usize) -> Option<Match<'s>> {
        self.search_impl(s, pos, false)
    }

    /// `bool(pattern.search(s))`.
    pub fn is_match(&self, s: &str) -> bool {
        self.search(s).is_some()
    }

    /// `pattern.match(s)` (anchored at the start).
    pub fn match_start<'s>(&self, s: &'s str) -> Option<Match<'s>> {
        self.match_here(s, 0, false, false)
    }

    /// `pattern.match(s, pos)`.
    pub fn match_at<'s>(&self, s: &'s str, pos: usize) -> Option<Match<'s>> {
        self.match_here(s, pos, false, false)
    }

    /// `pattern.fullmatch(s)`.
    pub fn fullmatch<'s>(&self, s: &'s str) -> Option<Match<'s>> {
        self.match_here(s, 0, false, true)
    }

    /// `list(pattern.finditer(s))`.
    pub fn find_iter<'s>(&self, s: &'s str) -> Vec<Match<'s>> {
        self.find_iter_from(s, 0)
    }

    /// `list(pattern.finditer(s, pos))`.
    pub fn find_iter_from<'s>(&self, s: &'s str, pos: usize) -> Vec<Match<'s>> {
        let mut out = Vec::new();
        let mut pos = pos;
        let mut must_advance = false;
        while pos <= s.len() {
            let Some(m) = self.search_impl(s, pos, must_advance) else {
                break;
            };
            must_advance = m.start() == m.end();
            pos = m.end();
            out.push(m);
        }
        out
    }

    /// `pattern.findall(s)` with no or one group: the matched strings.
    pub fn findall<'s>(&self, s: &'s str) -> Vec<&'s str> {
        self.find_iter(s)
            .iter()
            .map(|m| if self.ngroups == 0 { m.as_str() } else { m.group_str(1) })
            .collect()
    }

    /// `pattern.findall(s)` with several groups: one tuple (vec) per match.
    pub fn findall_groups<'s>(&self, s: &'s str) -> Vec<Vec<&'s str>> {
        self.find_iter(s)
            .iter()
            .map(|m| (1..=self.ngroups).map(|i| m.group_str(i)).collect())
            .collect()
    }

    /// `pattern.sub(repl, s)` with a template replacement.
    pub fn sub(&self, repl: &str, s: &str) -> String {
        self.subn(repl, s, 0).0
    }

    /// `pattern.subn(repl, s, count)`.
    pub fn subn(&self, repl: &str, s: &str, count: usize) -> (String, usize) {
        self.subn_fn(s, count, |m| expand_template(repl, m))
    }

    /// `pattern.sub(func, s)`.
    pub fn sub_fn(&self, s: &str, f: impl FnMut(&Match) -> String) -> String {
        self.subn_fn(s, 0, f).0
    }

    /// `pattern.subn(func, s, count)`; `count == 0` replaces all.
    pub fn subn_fn(&self, s: &str, count: usize, mut f: impl FnMut(&Match) -> String) -> (String, usize) {
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        let mut n = 0;
        let mut pos = 0;
        let mut must_advance = false;
        while pos <= s.len() && (count == 0 || n < count) {
            let Some(m) = self.search_impl(s, pos, must_advance) else {
                break;
            };
            out.push_str(&s[last..m.start()]);
            out.push_str(&f(&m));
            last = m.end();
            n += 1;
            must_advance = m.start() == m.end();
            pos = m.end();
        }
        out.push_str(&s[last..]);
        (out, n)
    }

    /// `pattern.split(s, maxsplit)`: captured groups are included (`None` for
    /// a group that didn't participate).
    pub fn split<'s>(&self, s: &'s str, maxsplit: usize) -> Vec<Option<&'s str>> {
        let mut out = Vec::new();
        let mut last = 0;
        let mut n = 0;
        for m in self.find_iter(s) {
            if maxsplit != 0 && n >= maxsplit {
                break;
            }
            out.push(Some(&s[last..m.start()]));
            for i in 1..=self.ngroups {
                out.push(m.group(i));
            }
            last = m.end();
            n += 1;
        }
        out.push(Some(&s[last..]));
        out
    }

    /// `pattern.split(s)` as strings (no capturing groups expected).
    pub fn split_str<'s>(&self, s: &'s str) -> Vec<&'s str> {
        self.split(s, 0).into_iter().map(|x| x.unwrap_or("")).collect()
    }
}

/// A literal string every match must start with, if the pattern has one.
fn literal_prefix(ast: &Ast) -> Option<String> {
    fn walk(ast: &Ast, out: &mut String) -> bool {
        // returns true if the whole node was a literal (so the prefix may continue)
        match ast {
            Ast::Char(c) => {
                out.push(*c);
                true
            }
            Ast::Concat(items) => {
                for it in items {
                    if !walk(it, out) {
                        return false;
                    }
                }
                true
            }
            Ast::Group(inner, _) => walk(inner, out),
            Ast::Repeat { node, min, .. } if *min >= 1 => {
                walk(node, out);
                false
            }
            _ => false,
        }
    }
    let mut out = String::new();
    walk(ast, &mut out);
    Some(out)
}

// --------------------------------------------------------------------------- //
// The VM
// --------------------------------------------------------------------------- //

enum Frame {
    Branch { pc: usize, pos: usize },
    Restore { slot: usize, old: Option<usize> },
}

struct Vm<'r, 's> {
    re: &'r Regex,
    s: &'s str,
    /// reject a top-level match that ends here (Python's `must_advance`)
    must_advance_from: Option<usize>,
    /// fullmatch: the top-level match must end at the end of the string
    full: bool,
}

impl Vm<'_, '_> {
    fn char_at(&self, pos: usize) -> Option<char> {
        self.s[pos..].chars().next()
    }

    fn char_before(&self, pos: usize) -> Option<char> {
        self.s[..pos].chars().next_back()
    }

    fn assert(&self, kind: AssertKind, pos: usize) -> bool {
        let multiline = self.re.flags.contains(Flags::M);
        let len = self.s.len();
        match kind {
            AssertKind::StartText => pos == 0,
            AssertKind::EndText => pos == len,
            AssertKind::Start => pos == 0 || (multiline && self.char_before(pos) == Some('\n')),
            AssertKind::End => {
                pos == len
                    || (self.char_at(pos) == Some('\n') && (multiline || pos + 1 == len))
            }
            AssertKind::WordB | AssertKind::NotWordB => {
                let a = self.char_before(pos).is_some_and(is_word);
                let b = self.char_at(pos).is_some_and(is_word);
                (a != b) == (kind == AssertKind::WordB)
            }
        }
    }

    /// Run program `prog` anchored at `pos`; returns the end position.
    fn run(&self, prog: usize, pos: usize, slots: &mut [Option<usize>], toplevel: bool) -> Option<usize> {
        let insts = &self.re.progs[prog].insts;
        let icase = self.re.flags.contains(Flags::I);
        let dotall = self.re.flags.contains(Flags::S);
        let mut stack: Vec<Frame> = Vec::new();
        let mut pc = 0;
        let mut pos = pos;
        loop {
            let ok = match &insts[pc] {
                Inst::Char(c) => match self.char_at(pos) {
                    Some(x) if x == *c || (icase && eq_icase(x, *c)) => {
                        pos += x.len_utf8();
                        pc += 1;
                        true
                    }
                    _ => false,
                },
                Inst::Any => match self.char_at(pos) {
                    Some(x) if dotall || x != '\n' => {
                        pos += x.len_utf8();
                        pc += 1;
                        true
                    }
                    _ => false,
                },
                Inst::Class(i) => match self.char_at(pos) {
                    Some(x) if self.re.classes[*i].matches(x, icase) => {
                        pos += x.len_utf8();
                        pc += 1;
                        true
                    }
                    _ => false,
                },
                Inst::Split(a, b) => {
                    stack.push(Frame::Branch { pc: *b, pos });
                    pc = *a;
                    true
                }
                Inst::Jmp(a) => {
                    pc = *a;
                    true
                }
                Inst::Save(k) | Inst::Mark(k) => {
                    stack.push(Frame::Restore {
                        slot: *k,
                        old: slots[*k],
                    });
                    slots[*k] = Some(pos);
                    pc += 1;
                    true
                }
                Inst::Progress(k) => {
                    if slots[*k] == Some(pos) {
                        false
                    } else {
                        pc += 1;
                        true
                    }
                }
                Inst::Assert(kind) => {
                    if self.assert(*kind, pos) {
                        pc += 1;
                        true
                    } else {
                        false
                    }
                }
                Inst::Backref(g) => match (slots[2 * g], slots[2 * g + 1]) {
                    (Some(a), Some(b)) => {
                        let text = &self.s[a..b];
                        let rest = &self.s[pos..];
                        let hit = if icase {
                            let mut ri = rest.chars();
                            let mut len = 0;
                            let all = text.chars().all(|tc| match ri.next() {
                                Some(rc) if eq_icase(tc, rc) => {
                                    len += rc.len_utf8();
                                    true
                                }
                                _ => false,
                            });
                            all.then_some(len)
                        } else {
                            rest.starts_with(text).then_some(text.len())
                        };
                        match hit {
                            Some(n) => {
                                pos += n;
                                pc += 1;
                                true
                            }
                            None => false,
                        }
                    }
                    _ => false, // an unset group never matches
                },
                Inst::Look {
                    prog: sub,
                    ahead,
                    negate,
                    width,
                } => {
                    let start = if *ahead {
                        Some(pos)
                    } else {
                        let mut p = pos;
                        let mut ok = true;
                        for _ in 0..*width {
                            match self.char_before(p) {
                                Some(c) => p -= c.len_utf8(),
                                None => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        ok.then_some(p)
                    };
                    let saved: Vec<Option<usize>> = slots.to_vec();
                    let hit = match start {
                        None => false,
                        Some(st) => match self.run(*sub, st, slots, false) {
                            Some(end) => *ahead || end == pos,
                            None => false,
                        },
                    };
                    if *negate || !hit {
                        slots.copy_from_slice(&saved);
                    } else {
                        // keep captures made inside a positive lookaround, but
                        // undo them if we later backtrack past this point
                        for (k, old) in saved.iter().enumerate() {
                            if slots[k] != *old {
                                stack.push(Frame::Restore { slot: k, old: *old });
                            }
                        }
                    }
                    if hit != *negate {
                        pc += 1;
                        true
                    } else {
                        false
                    }
                }
                Inst::Match => {
                    let reject = toplevel
                        && ((self.full && pos != self.s.len())
                            || self.must_advance_from == Some(pos));
                    if reject {
                        false
                    } else {
                        return Some(pos);
                    }
                }
            };
            if !ok {
                // backtrack
                loop {
                    match stack.pop() {
                        None => return None,
                        Some(Frame::Restore { slot, old }) => slots[slot] = old,
                        Some(Frame::Branch { pc: p, pos: q }) => {
                            pc = p;
                            pos = q;
                            break;
                        }
                    }
                }
            }
        }
    }
}

// --------------------------------------------------------------------------- //
// Replacement templates
// --------------------------------------------------------------------------- //

/// Expand a `re.sub` replacement template against a match.
fn expand_template(t: &str, m: &Match) -> String {
    let chars: Vec<char> = t.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\\' || i + 1 >= chars.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let e = chars[i + 1];
        i += 2;
        match e {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'f' => out.push('\x0c'),
            'v' => out.push('\x0b'),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            '\\' => out.push('\\'),
            'g' if chars.get(i) == Some(&'<') => {
                let end = chars[i..].iter().position(|&c| c == '>').map(|p| i + p);
                match end {
                    Some(end) => {
                        let name: String = chars[i + 1..end].iter().collect();
                        let text = match name.parse::<usize>() {
                            Ok(n) => m.group(n),
                            Err(_) => m.name(&name),
                        };
                        out.push_str(text.unwrap_or(""));
                        i = end + 1;
                    }
                    None => out.push_str("\\g"),
                }
            }
            '0' => out.push('\0'),
            d if d.is_ascii_digit() => {
                let mut n = d.to_digit(10).unwrap() as usize;
                if let Some(d2) = chars.get(i).and_then(|c| c.to_digit(10)) {
                    n = n * 10 + d2 as usize;
                    i += 1;
                }
                out.push_str(m.group(n).unwrap_or(""));
            }
            // other escapes (e.g. "\&") keep the backslash
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

/// `re.escape(s)`: backslash every char that isn't alphanumeric or `_`.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if !(c.is_alphanumeric() || c == '_') && c.is_ascii() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// --------------------------------------------------------------------------- //
// Module-level functions with a compile cache (like Python's re cache)
// --------------------------------------------------------------------------- //

thread_local! {
    static CACHE: RefCell<HashMap<(String, Flags), Regex>> = RefCell::new(HashMap::new());
}

/// Compile (cached) — for patterns built at runtime.
pub fn compile(pattern: &str, flags: Flags) -> Regex {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(r) = c.get(&(pattern.to_string(), flags)) {
            return r.clone();
        }
        if c.len() > 512 {
            c.clear();
        }
        let r = Regex::with_flags(pattern, flags);
        c.insert((pattern.to_string(), flags), r.clone());
        r
    })
}

/// `re.search(pattern, s)`.
pub fn search<'s>(pattern: &str, s: &'s str) -> Option<Match<'s>> {
    compile(pattern, Flags::NONE).search(s)
}

/// `re.match(pattern, s)`.
pub fn match_start<'s>(pattern: &str, s: &'s str) -> Option<Match<'s>> {
    compile(pattern, Flags::NONE).match_start(s)
}

/// `re.fullmatch(pattern, s)`.
pub fn fullmatch<'s>(pattern: &str, s: &'s str) -> Option<Match<'s>> {
    compile(pattern, Flags::NONE).fullmatch(s)
}

/// `re.sub(pattern, repl, s)`.
pub fn sub(pattern: &str, repl: &str, s: &str) -> String {
    compile(pattern, Flags::NONE).sub(repl, s)
}

/// `re.sub(pattern, repl, s, flags=flags)`.
pub fn sub_flags(pattern: &str, repl: &str, s: &str, flags: Flags) -> String {
    compile(pattern, flags).sub(repl, s)
}

/// `re.findall(pattern, s)` (no or one group).
pub fn findall<'s>(pattern: &str, s: &'s str) -> Vec<&'s str> {
    compile(pattern, Flags::NONE).findall(s)
}

/// `re.split(pattern, s)`.
pub fn split<'s>(pattern: &str, s: &'s str) -> Vec<&'s str> {
    compile(pattern, Flags::NONE).split_str(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics_match_python() {
        let r = Regex::new(r"(?<!\\)%[^\n]*(?:\n[ \t]*)?");
        assert_eq!(r.sub("", "a % c\n  b \\% d"), "a b \\% d");
        let r = Regex::new(r"\\(?:input|include)\s*\{([^}]+)\}");
        let m = r.search("x \\input {ch1} y").unwrap();
        assert_eq!(m.group(1), Some("ch1"));
        assert_eq!((m.start(), m.end()), (2, 14));
    }

    #[test]
    fn backreference_and_lazy_dotall() {
        let r = Regex::with_flags(r"\\begin\{(lstlisting|minted)\}(?P<body>.*?)\\end\{\1\}", Flags::S);
        let s = "\\begin{minted}a$\nb\\end{minted} \\begin{lstlisting}x\\end{lstlisting}";
        let ms = r.find_iter(s);
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0].name("body"), Some("a$\nb"));
        assert_eq!(ms[1].name("body"), Some("x"));
    }

    #[test]
    fn empty_matches_follow_python_37_rules() {
        // re.sub('x*', '-', 'abxd') == '-a-b--d-'
        assert_eq!(Regex::new("x*").sub("-", "abxd"), "-a-b--d-");
        // re.split(r'\b', 'a b') == ['', 'a', ' ', 'b', '']
        assert_eq!(Regex::new(r"\b").split_str("a b"), vec!["", "a", " ", "b", ""]);
        // re.findall(r'\d*', '12a') == ['12', '', '']
        assert_eq!(Regex::new(r"\d*").findall("12a"), vec!["12", "", ""]);
    }

    #[test]
    fn anchors_and_lookarounds() {
        assert!(Regex::new(r"\\fi(?![a-zA-Z@])").is_match("x\\fi y"));
        assert!(!Regex::new(r"\\fi(?![a-zA-Z@])").is_match("x\\fill"));
        assert_eq!(Regex::with_flags("^a", Flags::M).find_iter("a\na").len(), 2);
        assert_eq!(Regex::new("^a").find_iter("a\na").len(), 1);
        assert!(Regex::new("a$").is_match("xa\n"), "$ before a final newline");
        assert!(Regex::new(r"(?<=ab)c").is_match("abc"));
        assert!(!Regex::new(r"(?<!ab)c").is_match("abc"));
    }

    #[test]
    fn templates() {
        let r = Regex::new(r"(\w+)-(?P<b>\w+)");
        assert_eq!(r.sub(r"\2:\g<1>:\g<b>\n", "ab-cd"), "cd:ab:cd\n");
        // unknown escapes of punctuation keep the backslash
        assert_eq!(Regex::new("a").sub(r"\&", "a"), "\\&");
        // r"\\verb" -> backslash + verb
        assert_eq!(Regex::new(r"\\verb\*").sub(r"\\verb", "\\verb*|x|"), "\\verb|x|");
    }

    #[test]
    fn counted_repeats_and_classes() {
        let r = Regex::new(r"fil{1,3}");
        assert_eq!(r.find_iter("fil fill filll fillll").iter().map(|m| m.as_str()).collect::<Vec<_>>(),
                   vec!["fil", "fill", "filll", "filll"]);
        assert!(Regex::new(r"[\w* ._-]+").fullmatch("my env*-1.x").is_some());
        assert!(Regex::new(r"[]a]").is_match("]"));
        assert!(Regex::new(r"x{,2}y").fullmatch("xxy").is_some());
        assert!(Regex::new(r"a{x").fullmatch("a{x").is_some(), "literal brace");
    }

    #[test]
    fn ignorecase() {
        assert!(Regex::with_flags("title", Flags::I).is_match("FancyTitle"));
        assert!(Regex::with_flags("[a-c]x", Flags::I).is_match("BX"));
    }

    #[test]
    fn long_repetitions_do_not_overflow_the_stack() {
        // Anchored, so this checks stack depth rather than the O(n^2) search
        // every backtracking engine (sre included) would do unanchored.
        let s = "a".repeat(200_000);
        assert!(Regex::new(r"(?:a|b)*c").match_start(&s).is_none());
        assert_eq!(
            Regex::new(r"(?:a|b)*").match_start(&s).unwrap().end(),
            s.len()
        );
        let lazy = Regex::with_flags(r"x.*?y", Flags::S);
        assert_eq!(lazy.search(&format!("x{s}y")).unwrap().end(), s.len() + 2);
    }
}
