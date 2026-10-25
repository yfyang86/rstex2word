//! The slice of Python's `argparse` the CLI uses: options (with prefix
//! abbreviation, `--opt=value` and `-oVALUE`), typed/choice values, required
//! positionals, help, and argparse's error messages and exit codes.

use std::collections::{HashMap, VecDeque};

/// What an option does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `store_true`
    Flag,
    /// takes one value
    Value,
    /// takes one value parsed as `int`
    Int,
    /// `-h/--help`
    Help,
}

pub struct Opt {
    /// the destination name (`output`, `no_manifest`, ...)
    pub dest: &'static str,
    /// option strings in declaration order (`-o`, `--output`)
    pub flags: &'static [&'static str],
    pub kind: Kind,
    pub choices: &'static [&'static str],
}

/// A (sub)parser.
pub struct Parser {
    /// `tex2word convert`
    pub prog: &'static str,
    /// the full `--help` text
    pub help: &'static str,
    pub opts: Vec<Opt>,
    /// positional destinations, in order
    pub positionals: &'static [&'static str],
}

/// Parsing stopped: print `text` to stdout (help) or stderr and exit.
#[derive(Debug)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Exit {
    pub fn error(parser: &Parser, message: &str) -> Exit {
        Exit {
            code: 2,
            stdout: String::new(),
            stderr: format!("{}\n{}: error: {message}\n", parser.usage(), parser.prog),
        }
    }
}

/// Parsed values: strings for valued options and positionals, `"1"` for set
/// flags; `extras` holds unrecognized arguments.
#[derive(Debug, Default)]
pub struct Parsed {
    pub values: HashMap<&'static str, String>,
    pub extras: Vec<String>,
}

impl Parsed {
    pub fn get(&self, dest: &str) -> Option<&str> {
        self.values.get(dest).map(String::as_str)
    }

    pub fn flag(&self, dest: &str) -> bool {
        self.values.contains_key(dest)
    }
}

/// `repr()` of a short ASCII-ish string, as argparse prints it.
pub fn repr(s: &str) -> String {
    tex2word::ir::json::py_str_repr(s)
}

/// argparse's test for an argument that looks like an option (negative
/// numbers are values: none of our options look like one).
pub fn is_option_like(arg: &str) -> bool {
    if !arg.starts_with('-') || arg == "-" {
        return false;
    }
    let rest = &arg[1..];
    let negative_number = !rest.is_empty()
        && (rest.chars().all(|c| c.is_ascii_digit())
            || rest.split_once('.').is_some_and(|(a, b)| {
                a.chars().all(|c| c.is_ascii_digit())
                    && !b.is_empty()
                    && b.chars().all(|c| c.is_ascii_digit())
            }));
    !negative_number
}

impl Parser {
    /// The `usage: ...` block of the help text.
    pub fn usage(&self) -> &str {
        self.help.split("\n\n").next().unwrap_or("")
    }

    fn find_exact(&self, flag: &str) -> Option<&Opt> {
        self.opts.iter().find(|o| o.flags.contains(&flag))
    }

    /// The option a `--long` (possibly abbreviated) string names.
    fn find_long(&self, name: &str) -> Result<Option<&Opt>, Exit> {
        if let Some(o) = self.find_exact(name) {
            return Ok(Some(o));
        }
        let matches: Vec<(&Opt, &str)> = self
            .opts
            .iter()
            .flat_map(|o| {
                o.flags
                    .iter()
                    .filter(|f| f.starts_with("--") && f.starts_with(name))
                    .map(move |f| (o, *f))
            })
            .collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(Some(matches[0].0)),
            _ => {
                let names: Vec<&str> = matches.iter().map(|(_, f)| *f).collect();
                Err(Exit::error(
                    self,
                    &format!("ambiguous option: {name} could match {}", names.join(", ")),
                ))
            }
        }
    }

    fn display_name(o: &Opt) -> String {
        o.flags.join("/")
    }

    fn store(&self, o: &Opt, value: Option<String>, out: &mut Parsed) -> Result<(), Exit> {
        match o.kind {
            Kind::Help => {
                return Err(Exit {
                    code: 0,
                    stdout: self.help.to_string(),
                    stderr: String::new(),
                })
            }
            Kind::Flag => {
                out.values.insert(o.dest, "1".into());
            }
            Kind::Value | Kind::Int => {
                let v = value.expect("valued options carry a value");
                if o.kind == Kind::Int && tex2word::ir::pystr::parse_int(&v).is_none() {
                    return Err(Exit::error(
                        self,
                        &format!(
                            "argument {}: invalid int value: {}",
                            Self::display_name(o),
                            repr(&v)
                        ),
                    ));
                }
                if !o.choices.is_empty() && !o.choices.contains(&v.as_str()) {
                    let choices: Vec<String> = o.choices.iter().map(|c| repr(c)).collect();
                    return Err(Exit::error(
                        self,
                        &format!(
                            "argument {}: invalid choice: {} (choose from {})",
                            Self::display_name(o),
                            repr(&v),
                            choices.join(", ")
                        ),
                    ));
                }
                out.values.insert(o.dest, v);
            }
        }
        Ok(())
    }

    /// `parse_known_args`: unknown options and surplus positionals go to
    /// `extras`.
    pub fn parse(&self, args: &[String]) -> Result<Parsed, Exit> {
        let mut out = Parsed::default();
        let mut positionals: Vec<String> = Vec::new();
        let mut queue: VecDeque<String> = args.iter().cloned().collect();
        let mut only_positionals = false;
        while let Some(arg) = queue.pop_front() {
            if only_positionals || !is_option_like(&arg) {
                positionals.push(arg);
                continue;
            }
            if arg == "--" {
                only_positionals = true;
                continue;
            }
            // resolve the option and any attached value
            let (opt, attached): (Option<&Opt>, Option<String>) = if arg.starts_with("--") {
                let (name, val) = match arg.split_once('=') {
                    Some((n, v)) => (n, Some(v.to_string())),
                    None => (arg.as_str(), None),
                };
                (self.find_long(name)?, val)
            } else if let Some(o) = self.find_exact(&arg) {
                (Some(o), None)
            } else {
                match self.find_exact(&arg[..2]) {
                    Some(o) if o.kind == Kind::Value || o.kind == Kind::Int => {
                        (Some(o), Some(arg[2..].to_string()))
                    }
                    Some(o) => {
                        // a flag cluster (-qo ...): this flag, then the rest
                        self.store(o, None, &mut out)?;
                        queue.push_front(format!("-{}", &arg[2..]));
                        continue;
                    }
                    None => (None, None),
                }
            };
            let Some(opt) = opt else {
                out.extras.push(arg);
                continue;
            };
            let value = match opt.kind {
                Kind::Flag | Kind::Help => {
                    if let Some(a) = attached {
                        return Err(Exit::error(
                            self,
                            &format!(
                                "argument {}: ignored explicit argument {}",
                                Self::display_name(opt),
                                repr(&a)
                            ),
                        ));
                    }
                    None
                }
                _ => match attached {
                    Some(v) => Some(v),
                    None => match queue.front() {
                        Some(next) if !is_option_like(next) => queue.pop_front(),
                        _ => {
                            return Err(Exit::error(
                                self,
                                &format!(
                                    "argument {}: expected one argument",
                                    Self::display_name(opt)
                                ),
                            ))
                        }
                    },
                },
            };
            self.store(opt, value, &mut out)?;
        }
        self.finish(out, positionals)
    }

    fn finish(&self, mut out: Parsed, positionals: Vec<String>) -> Result<Parsed, Exit> {
        let mut it = positionals.into_iter();
        let mut missing = Vec::new();
        for dest in self.positionals {
            match it.next() {
                Some(v) => {
                    out.values.insert(dest, v);
                }
                None => missing.push(*dest),
            }
        }
        out.extras.extend(it);
        if !missing.is_empty() {
            return Err(Exit::error(
                self,
                &format!(
                    "the following arguments are required: {}",
                    missing.join(", ")
                ),
            ));
        }
        Ok(out)
    }
}
