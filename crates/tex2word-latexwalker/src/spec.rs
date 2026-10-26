//! Macro/environment/specials specifications and the context database
//! (port of `pylatexenc.macrospec`).

use std::borrow::Cow;
use std::collections::HashMap;

use crate::default_specs::DEFAULT_CATEGORIES;

/// Which verbatim argument parser (`pylatexenc.macrospec.VerbatimArgsParser`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbatimKind {
    /// `\verb|…|`: the next non-space char delimits the argument.
    VerbMacro,
    /// `\begin{verbatim}…\end{verbatim}`: the body up to `\end{verbatim}`.
    VerbatimEnvironment,
}

/// An argument parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgsParser {
    /// `MacroStandardArgsParser`: `argspec` is a string of `*` (optional
    /// star), `[` (optional bracket argument) and `{` (mandatory argument).
    Standard {
        argspec: Cow<'static, str>,
        /// don't look for an optional `[…]` after whitespace
        optional_arg_no_space: bool,
        /// per-argument math mode override (`None` = inherit)
        args_math_mode: Option<Cow<'static, [Option<bool>]>>,
    },
    /// `VerbatimArgsParser`.
    Verbatim(VerbatimKind),
}

impl ArgsParser {
    /// `MacroStandardArgsParser(argspec)`.
    pub fn standard(argspec: impl Into<Cow<'static, str>>) -> Self {
        ArgsParser::Standard {
            argspec: argspec.into(),
            optional_arg_no_space: false,
            args_math_mode: None,
        }
    }

    /// `MacroStandardArgsParser(argspec, optional_arg_no_space, args_math_mode)`
    /// (const, for the generated default tables).
    pub const fn standard_full(
        argspec: &'static str,
        optional_arg_no_space: bool,
        args_math_mode: Option<&'static [Option<bool>]>,
    ) -> Self {
        ArgsParser::Standard {
            argspec: Cow::Borrowed(argspec),
            optional_arg_no_space,
            args_math_mode: match args_math_mode {
                Some(a) => Some(Cow::Borrowed(a)),
                None => None,
            },
        }
    }

    /// The argument spec string (`{` for the verbatim parsers).
    pub fn argspec(&self) -> &str {
        match self {
            ArgsParser::Standard { argspec, .. } => argspec,
            ArgsParser::Verbatim(_) => "{",
        }
    }
}

/// `MacroSpec(macroname, args_parser)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacroSpec {
    pub macroname: String,
    pub args_parser: ArgsParser,
}

impl MacroSpec {
    /// `MacroSpec(name, argspec)`.
    pub fn new(name: impl Into<String>, argspec: impl Into<Cow<'static, str>>) -> Self {
        MacroSpec {
            macroname: name.into(),
            args_parser: ArgsParser::standard(argspec),
        }
    }
}

/// `EnvironmentSpec(environmentname, args_parser, is_math_mode)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentSpec {
    pub environmentname: String,
    pub args_parser: ArgsParser,
    pub is_math_mode: bool,
}

impl EnvironmentSpec {
    /// `EnvironmentSpec(name, argspec)`.
    pub fn new(name: impl Into<String>, argspec: impl Into<Cow<'static, str>>) -> Self {
        EnvironmentSpec {
            environmentname: name.into(),
            args_parser: ArgsParser::standard(argspec),
            is_math_mode: false,
        }
    }
}

/// `SpecialsSpec(specials_chars)` — specials never take arguments in the
/// databases tex2word uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialsSpec {
    pub specials_chars: String,
}

#[derive(Debug, Clone, Default)]
struct Category {
    name: String,
    macros: HashMap<String, MacroSpec>,
    environments: HashMap<String, EnvironmentSpec>,
    /// insertion order matters for `test_for_specials` tie-breaking
    specials: Vec<SpecialsSpec>,
}

/// `LatexContextDb`: an ordered list of categories; lookups take the first
/// category that defines the name.
#[derive(Debug, Clone, Default)]
pub struct ContextDb {
    categories: Vec<Category>,
}

impl ContextDb {
    /// An empty database.
    pub fn new() -> Self {
        Self::default()
    }

    /// `get_default_latex_context_db()`.
    pub fn default_db() -> Self {
        let mut db = ContextDb::new();
        for (name, macros, envs, specials) in DEFAULT_CATEGORIES {
            db.add_context_category(
                name,
                macros.iter().map(|(m, ap)| MacroSpec {
                    macroname: (*m).to_string(),
                    args_parser: ap.clone().unwrap_or_else(|| ArgsParser::standard("")),
                }),
                envs.iter().map(|(e, ap, math)| EnvironmentSpec {
                    environmentname: (*e).to_string(),
                    args_parser: ap.clone().unwrap_or_else(|| ArgsParser::standard("")),
                    is_math_mode: *math,
                }),
                specials.iter().map(|s| SpecialsSpec {
                    specials_chars: (*s).to_string(),
                }),
                false,
            );
        }
        db
    }

    /// `add_context_category(name, macros, environments, specials, prepend)`.
    ///
    /// # Panics
    /// If the category is already registered (as pylatexenc raises).
    pub fn add_context_category(
        &mut self,
        name: &str,
        macros: impl IntoIterator<Item = MacroSpec>,
        environments: impl IntoIterator<Item = EnvironmentSpec>,
        specials: impl IntoIterator<Item = SpecialsSpec>,
        prepend: bool,
    ) {
        assert!(
            !self.categories.iter().any(|c| c.name == name),
            "Category {name} is already registered in the context database"
        );
        // dict((m.macroname, m) for m in macros): a later duplicate wins
        let mut cat = Category {
            name: name.to_string(),
            ..Category::default()
        };
        for m in macros {
            cat.macros.insert(m.macroname.clone(), m);
        }
        for e in environments {
            cat.environments.insert(e.environmentname.clone(), e);
        }
        for s in specials {
            match cat
                .specials
                .iter_mut()
                .find(|x| x.specials_chars == s.specials_chars)
            {
                Some(slot) => *slot = s,
                None => cat.specials.push(s),
            }
        }
        if prepend {
            self.categories.insert(0, cat);
        } else {
            self.categories.push(cat);
        }
    }

    /// Category names in lookup order.
    pub fn categories(&self) -> Vec<&str> {
        self.categories.iter().map(|c| c.name.as_str()).collect()
    }

    pub fn get_macro_spec(&self, name: &str) -> Option<&MacroSpec> {
        self.categories.iter().find_map(|c| c.macros.get(name))
    }

    pub fn get_environment_spec(&self, name: &str) -> Option<&EnvironmentSpec> {
        self.categories
            .iter()
            .find_map(|c| c.environments.get(name))
    }

    pub fn get_specials_spec(&self, chars: &str) -> Option<&SpecialsSpec> {
        self.categories
            .iter()
            .find_map(|c| c.specials.iter().find(|s| s.specials_chars == chars))
    }

    /// `test_for_specials(s, pos)`: the longest specials spec matching at
    /// `pos` (the earliest category wins a tie).
    pub fn test_for_specials(&self, s: &str, pos: usize) -> Option<&SpecialsSpec> {
        let rest = &s[pos..];
        let mut best: Option<&SpecialsSpec> = None;
        let mut best_len = 0;
        for cat in &self.categories {
            for spec in &cat.specials {
                let n = spec.specials_chars.chars().count();
                if n > best_len && rest.starts_with(spec.specials_chars.as_str()) {
                    best = Some(spec);
                    best_len = n;
                }
            }
        }
        best
    }
}
