//! Parsed LaTeX nodes (port of pylatexenc's `LatexNode` classes).

use std::sync::Arc;

/// A parsed node. `pos`/`len` are byte offsets into the walked source;
/// [`Node::latex_verbatim`] returns that span, as pylatexenc's does.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub pos: usize,
    pub len: usize,
    pub kind: NodeKind,
    pub(crate) src: Arc<str>,
}

/// The node variants (`LatexCharsNode`, `LatexGroupNode`, …).
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// `LatexCharsNode`
    Chars { chars: String },
    /// `LatexGroupNode` (`{…}`, or `[…]` for an optional argument)
    Group {
        nodelist: Vec<Node>,
        delimiters: (String, String),
    },
    /// `LatexCommentNode`
    Comment {
        comment: String,
        comment_post_space: String,
    },
    /// `LatexMacroNode`; `nodeargd` is `None` when the macro was read as a
    /// bare expression (an argument of another macro) or its arguments failed
    /// to parse.
    Macro {
        macroname: String,
        nodeargd: Option<ParsedArgs>,
        macro_post_space: String,
        /// read by `get_latex_expression` (as another macro's argument):
        /// pylatexenc then sets the legacy `nodeoptarg`/`nodeargs` to `None`
        bare_expression: bool,
    },
    /// `LatexEnvironmentNode`
    Environment {
        environmentname: String,
        nodelist: Vec<Node>,
        nodeargd: Option<ParsedArgs>,
    },
    /// `LatexSpecialsNode`
    Specials {
        specials_chars: String,
        nodeargd: Option<ParsedArgs>,
    },
    /// `LatexMathNode`; `displaytype` is `inline` or `display`.
    Math {
        displaytype: String,
        nodelist: Vec<Node>,
        delimiters: (String, String),
    },
}

/// `ParsedMacroArgs` / `ParsedVerbatimArgs`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedArgs {
    pub argspec: String,
    /// one entry per argspec char; `None` for an absent optional argument
    pub argnlist: Vec<Option<Node>>,
    /// set for the verbatim parsers (`\verb`, `verbatim`)
    pub verbatim_text: Option<String>,
    pub verbatim_delimiters: Option<(char, char)>,
}

impl Node {
    /// `latex_verbatim()`: the exact source text of the node.
    pub fn latex_verbatim(&self) -> &str {
        &self.src[self.pos..self.pos + self.len]
    }

    /// The full source string the node was parsed from (`parsing_state.s`).
    pub fn source(&self) -> &str {
        &self.src
    }

    /// A shared handle to the source string.
    pub fn source_arc(&self) -> &Arc<str> {
        &self.src
    }

    /// The node class name, as pylatexenc spells it.
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            NodeKind::Chars { .. } => "LatexCharsNode",
            NodeKind::Group { .. } => "LatexGroupNode",
            NodeKind::Comment { .. } => "LatexCommentNode",
            NodeKind::Macro { .. } => "LatexMacroNode",
            NodeKind::Environment { .. } => "LatexEnvironmentNode",
            NodeKind::Specials { .. } => "LatexSpecialsNode",
            NodeKind::Math { .. } => "LatexMathNode",
        }
    }

    /// The text of a chars node.
    pub fn chars(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Chars { chars } => Some(chars),
            _ => None,
        }
    }

    /// The name of a macro node.
    pub fn macroname(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Macro { macroname, .. } => Some(macroname),
            _ => None,
        }
    }

    /// The name of an environment node.
    pub fn environmentname(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Environment { environmentname, .. } => Some(environmentname),
            _ => None,
        }
    }

    /// The parsed arguments of a macro, environment or specials node.
    pub fn nodeargd(&self) -> Option<&ParsedArgs> {
        match &self.kind {
            NodeKind::Macro { nodeargd, .. }
            | NodeKind::Environment { nodeargd, .. }
            | NodeKind::Specials { nodeargd, .. } => nodeargd.as_ref(),
            _ => None,
        }
    }

    /// The child nodes of a group, environment or math node.
    pub fn nodelist(&self) -> Option<&[Node]> {
        match &self.kind {
            NodeKind::Group { nodelist, .. }
            | NodeKind::Environment { nodelist, .. }
            | NodeKind::Math { nodelist, .. } => Some(nodelist),
            _ => None,
        }
    }

    pub fn is_chars(&self) -> bool {
        matches!(self.kind, NodeKind::Chars { .. })
    }
    pub fn is_group(&self) -> bool {
        matches!(self.kind, NodeKind::Group { .. })
    }
    pub fn is_comment(&self) -> bool {
        matches!(self.kind, NodeKind::Comment { .. })
    }
    pub fn is_macro(&self) -> bool {
        matches!(self.kind, NodeKind::Macro { .. })
    }
    pub fn is_environment(&self) -> bool {
        matches!(self.kind, NodeKind::Environment { .. })
    }
    pub fn is_specials(&self) -> bool {
        matches!(self.kind, NodeKind::Specials { .. })
    }
    pub fn is_math(&self) -> bool {
        matches!(self.kind, NodeKind::Math { .. })
    }
}
