//! Parsing pseudocode bodies (algorithmic / algpseudocode / algorithm2e) —
//! port of `frontend/algorithms.py`.
//!
//! These packages have no fixed argument signatures, so the raw node list is
//! walked directly. The output is a flat list of [`AlgLine`]s (indent depth +
//! inline content), which the back-end renders as numbered, indented lines.

use tex2word_ir::pystr;
use tex2word_ir::{AlgLine, Emphasis, Inline};
use tex2word_latexwalker::{Node, NodeKind};

/// command (lowercased) -> (opener keyword, keyword after the condition)
fn open(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        "if" => Some(("if", "then")),
        "for" => Some(("for", "do")),
        "forall" => Some(("for all", "do")),
        "while" => Some(("while", "do")),
        _ => None,
    }
}

/// statement commands taking the rest of the line, mapped to a keyword prefix
fn stmt(name: &str) -> Option<&'static str> {
    Some(match name {
        "state" | "statex" => "",
        "return" => "return ",
        "print" => "print ",
        "require" | "input" | "kwin" => "Input: ",
        "ensure" | "output" | "kwout" => "Output: ",
        "kwresult" => "Result: ",
        "kwdata" => "Data: ",
        _ => return None,
    })
}

fn is_close(name: &str) -> bool {
    matches!(
        name,
        "endif" | "endfor" | "endwhile" | "endloop" | "endfunction" | "endprocedure"
    )
}

/// structural commands that flush the current line and emit their own
fn is_structural(name: &str) -> bool {
    open(name).is_some()
        || stmt(name).is_some()
        || is_close(name)
        || matches!(
            name,
            "else"
                | "elsif"
                | "elif"
                | "elseif"
                | "repeat"
                | "until"
                | "loop"
                | "function"
                | "procedure"
                | "eif"
                | "uif"
        )
}

fn is_comment(name: &str) -> bool {
    matches!(
        name,
        "comment" | "tcp" | "tcc" | "tcp*" | "algorithmiccomment"
    )
}

/// handled at the float level, not in the body
fn is_skip(name: &str) -> bool {
    matches!(name, "caption" | "label")
}

fn kw(text: &str) -> Inline {
    Inline::Emphasis(Emphasis {
        inlines: vec![Inline::text(text)],
        kind: "bold".into(),
    })
}

fn find_algorithmic(nodes: &[Node]) -> Option<&Node> {
    nodes.iter().find(|n| {
        n.environmentname().is_some_and(|e| {
            matches!(
                e.trim_end_matches('*'),
                "algorithmic" | "algorithmicx" | "algpseudocode" | "algpseudocodex"
            )
        })
    })
}

/// Parse an algorithm body into lines; `inlines` converts a raw node run to
/// IR inlines (the parser's inline converter).
pub fn parse_algorithm_body(
    nodes: &[Node],
    inlines: &mut dyn FnMut(&[Node]) -> Vec<Inline>,
) -> Vec<AlgLine> {
    let body = match find_algorithmic(nodes) {
        Some(inner) => inner.nodelist().unwrap_or(&[]),
        None => nodes,
    };
    let mut p = Parser {
        inlines,
        lines: Vec::new(),
        indent: 0,
        prefix: Vec::new(),
        pending: Vec::new(),
    };
    p.run(body);
    p.lines
}

/// A pending line item: a raw node (converted on flush) or a built inline.
enum Pending {
    Raw(Node),
    Built(Inline),
}

struct Parser<'f> {
    inlines: &'f mut dyn FnMut(&[Node]) -> Vec<Inline>,
    lines: Vec<AlgLine>,
    indent: i64,
    /// content for the line currently being built
    prefix: Vec<Inline>,
    pending: Vec<Pending>,
}

impl Parser<'_> {
    fn run(&mut self, nodes: &[Node]) {
        let n = nodes.len();
        let mut i = 0;
        while i < n {
            let node = &nodes[i];
            if let Some(macroname) = node.macroname() {
                let name = pystr::lower(macroname);
                if macroname == ";" || macroname == "\\" {
                    // \; and \\ end a line
                    self.flush();
                    i += 1;
                    continue;
                }
                if is_skip(&name) {
                    i += 1;
                    continue;
                }
                if is_comment(&name) {
                    // attach to the current line, don't flush
                    let (note, j) = self.read_group(nodes, i + 1);
                    i = j;
                    self.pending.push(Pending::Built(Inline::text("  ▷ ")));
                    self.pending.extend(note.into_iter().map(Pending::Built));
                    continue;
                }
                if is_structural(&name) {
                    self.flush();
                    i = self.command(nodes, i, &name);
                    continue;
                }
                self.pending.push(Pending::Raw(node.clone()));
                i += 1;
            } else if is_semicolon(node) {
                self.flush();
                i += 1;
            } else {
                self.pending.push(Pending::Raw(node.clone()));
                i += 1;
            }
        }
        self.flush();
    }

    // -- line assembly ---------------------------------------------------- //

    fn flush(&mut self) {
        let mut out: Vec<Inline> = std::mem::take(&mut self.prefix);
        let mut batch: Vec<Node> = Vec::new();
        for item in std::mem::take(&mut self.pending) {
            match item {
                Pending::Built(inline) => {
                    if !batch.is_empty() {
                        out.extend((self.inlines)(&batch));
                        batch.clear();
                    }
                    out.push(inline);
                }
                Pending::Raw(node) => batch.push(node),
            }
        }
        if !batch.is_empty() {
            out.extend((self.inlines)(&batch));
        }
        let visible = out
            .iter()
            .any(|x| !matches!(x, Inline::Text(t) if pystr::strip(&t.value).is_empty()));
        if visible {
            self.lines.push(AlgLine {
                indent: self.indent.max(0),
                inlines: out,
                number: true,
            });
        }
    }

    fn emit(&mut self, parts: Vec<Inline>) {
        self.lines.push(AlgLine {
            indent: self.indent.max(0),
            inlines: parts,
            number: true,
        });
    }

    fn read_group(&mut self, nodes: &[Node], i: usize) -> (Vec<Inline>, usize) {
        match peek_group(nodes, i) {
            (Some(nodelist), j) => ((self.inlines)(nodelist), j),
            (None, _) => (Vec::new(), i),
        }
    }

    fn run_group(&mut self, nodes: &[Node], i: usize) -> usize {
        let (body, j) = peek_group(nodes, i);
        let Some(body) = body else {
            return i;
        };
        self.indent += 1;
        self.run(body);
        self.indent -= 1;
        j
    }

    // -- command dispatch ------------------------------------------------- //

    fn command(&mut self, nodes: &[Node], i: usize, name: &str) -> usize {
        if name == "eif" || name == "uif" {
            // algorithm2e \eIf{cond}{then}{else}
            let (cond, j) = self.read_group(nodes, i + 1);
            let mut parts = vec![kw("if"), Inline::text(" ")];
            parts.extend(cond);
            parts.extend([Inline::text(" "), kw("then")]);
            self.emit(parts);
            let j = self.run_group(nodes, j);
            self.emit(vec![kw("else")]);
            return self.run_group(nodes, j);
        }
        if let Some((opener, tail)) = open(name) {
            let (cond, j) = self.read_group(nodes, i + 1);
            let mut parts = vec![kw(opener), Inline::text(" ")];
            parts.extend(cond);
            parts.extend([Inline::text(" "), kw(tail)]);
            self.emit(parts);
            self.indent += 1;
            // algorithm2e puts the body in a following group; algorithmic lets
            // it follow as statements until \End... .
            if let (Some(body), j2) = peek_group(nodes, j) {
                self.run(body);
                self.indent -= 1;
                return j2;
            }
            return j;
        }
        match name {
            "elsif" | "elif" | "elseif" => {
                let (cond, j) = self.read_group(nodes, i + 1);
                self.indent -= 1;
                let mut parts = vec![kw("else if"), Inline::text(" ")];
                parts.extend(cond);
                parts.extend([Inline::text(" "), kw("then")]);
                self.emit(parts);
                self.indent += 1;
                j
            }
            "else" => {
                self.indent = (self.indent - 1).max(0);
                self.emit(vec![kw("else")]);
                self.indent += 1;
                i + 1
            }
            "repeat" | "loop" => {
                self.emit(vec![kw(name)]);
                self.indent += 1;
                i + 1
            }
            "until" => {
                let (cond, j) = self.read_group(nodes, i + 1);
                self.indent = (self.indent - 1).max(0);
                let mut parts = vec![kw("until"), Inline::text(" ")];
                parts.extend(cond);
                self.emit(parts);
                j
            }
            "function" | "procedure" => {
                let (head, j) = self.read_group(nodes, i + 1);
                let (args, j) = self.read_group(nodes, j);
                let mut parts = vec![kw(name), Inline::text(" ")];
                parts.extend(head);
                if !args.is_empty() {
                    parts.push(Inline::text("("));
                    parts.extend(args);
                    parts.push(Inline::text(")"));
                }
                self.emit(parts);
                self.indent += 1;
                j
            }
            _ if is_close(name) => {
                self.indent = (self.indent - 1).max(0);
                i + 1
            }
            _ => {
                if let Some(prefix) = stmt(name) {
                    let p = pystr::strip(prefix);
                    if !p.is_empty() {
                        self.prefix = vec![kw(p), Inline::text(" ")];
                    }
                }
                i + 1
            }
        }
    }
}

fn peek_group(nodes: &[Node], mut i: usize) -> (Option<&[Node]>, usize) {
    while i < nodes.len() && is_blank(&nodes[i]) {
        i += 1;
    }
    if let Some(NodeKind::Group { nodelist, .. }) = nodes.get(i).map(|n| &n.kind) {
        return (Some(nodelist), i + 1);
    }
    (None, i)
}

fn is_blank(node: &Node) -> bool {
    matches!(&node.kind, NodeKind::Chars { chars } if pystr::strip(chars).is_empty())
}

fn is_semicolon(node: &Node) -> bool {
    matches!(&node.kind, NodeKind::Specials { specials_chars, .. } if specials_chars == ";")
}
