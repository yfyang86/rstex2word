//! Front matter of journal classes and templates (port of
//! `frontend/frontmatter.py`).
//!
//! These `Builder` methods turn the title-page commands of the common classes
//! into document metadata (authors, affiliations, notes) or blocks:
//!
//! * authors with markers: `\author*[1]{...}` (sn-jnl, authblk), elsarticle
//!   `\author[a]{...\corref{c}}`, llncs `\inst`/`\orcidID`, NeurIPS
//!   `\And`/`\AND`, IEEE `\IEEEmembership`, ICML `\icmlauthor`;
//! * structured affiliations: acmart `\institution`/`\city`/`\country`,
//!   sn-jnl `\orgdiv`/`\orgname`/`\orgaddress`, elsarticle
//!   `organization={...},city={...}` and `\affil[1]{...}` labels;
//! * notes: amsart `\curraddr`/`\urladdr`/`\dedicatory`, elsarticle
//!   `\cortext`/`\fntext`, KOMA `\subtitle`/`\subject`/`\publishers`, ICML
//!   `\icmlcorrespondingauthor`;
//! * class-specific body commands: sn-jnl `\abstract{}`/`\bmhead`, KOMA
//!   `\addsec`/`\minisec`, IEEE `\appendices`, beamer frames, the `letter`
//!   class, moderncv entries.
//!
//! The preamble is scanned for every front-matter call ([`preamble_calls`]),
//! so templates that list several `\author`/`\affil` lines before
//! `\begin{document}` (authblk) keep all of them.

use tex2word_ir::pystr;
use tex2word_ir::re::{self, Flags};
use tex2word_ir::{Block, Emphasis, Heading, Inline, LineBreak, Link, Paragraph};
use tex2word_latexwalker::{Node, NodeKind};

use super::builder::Builder;
use super::helpers::*;

/// Block-level commands handled by [`Builder::front_matter`].
const FRONT_MATTER: &[&str] = &[
    "title",
    "column",
    "author",
    "affil",
    "affiliation",
    "institute",
    "address",
    "email",
    "ead",
    "orcid",
    "orcidID",
    "keywords",
    "curraddr",
    "urladdr",
    "dedicatory",
    "dedication",
    "subtitle",
    "subject",
    "titlehead",
    "publishers",
    "uppertitleback",
    "lowertitleback",
    "extratitle",
    "cortext",
    "fntext",
    "abstract",
    "icmltitle",
    "icmlauthor",
    "icmlaffiliation",
    "icmlcorrespondingauthor",
    "icmlkeywords",
    "icmlsetsymbol",
    "icmltitlerunning",
    "printAffiliationsAndNotice",
    "authorrunning",
    "titlerunning",
    "IEEEpeerreviewmaketitle",
    "appendices",
    "bmhead",
    "addsec",
    "addchap",
    "addpart",
    "minisec",
    "frametitle",
    "framesubtitle",
    "frame",
    "titlepage",
    "opening",
    "closing",
    "ps",
    "cc",
    "encl",
    "signature",
    "name",
    "makecvtitle",
    "moderncvstyle",
    "moderncvcolor",
    "phone",
    "mobile",
    "homepage",
    "social",
    "extrainfo",
    "photo",
    "cventry",
    "cvitem",
    "cvitemwithcomment",
    "cvlistitem",
    "cvdoubleitem",
    "cvlistdoubleitem",
    "cvline",
    "acmConference",
    "acmYear",
    "acmDOI",
    "acmISBN",
    "acmBooktitle",
    "acmPrice",
    "copyrightyear",
    "setcopyright",
    "ccsdesc",
    "settopmatter",
    "received",
    "keywordsname",
];

/// Whether `\name` is a front-matter command (`name in FRONT_MATTER`).
pub(crate) fn is_front_matter(name: &str) -> bool {
    FRONT_MATTER.contains(&name)
}

// (the argument specs of these commands are part of the generated
// `CONTEXT_MACROS` table in `parser/mod.rs`)

/// Affiliation sub-fields: their contents are joined with ", ".
const AFFIL_PARTS: &[&str] = &[
    "institution",
    "department",
    "streetaddress",
    "city",
    "state",
    "postcode",
    "country",
    "orgdiv",
    "orgname",
    "orgaddress",
    "street",
    "position",
];

/// Name parts and similar wrappers that just contribute their content.
fn is_transparent(name: &str) -> bool {
    matches!(name, "fnm" | "sur" | "pfx" | "sfx" | "spfx" | "dgr") || AFFIL_PARTS.contains(&name)
}

/// Commands that carry no visible content in Word.
const SILENT: &[&str] = &[
    "icmlkeywords",
    "icmlsetsymbol",
    "icmltitlerunning",
    "printAffiliationsAndNotice",
    "authorrunning",
    "titlerunning",
    "IEEEpeerreviewmaketitle",
    "titlepage",
    "makecvtitle",
    "moderncvstyle",
    "moderncvcolor",
    "photo",
    "acmConference",
    "acmYear",
    "acmDOI",
    "acmISBN",
    "acmBooktitle",
    "acmPrice",
    "copyrightyear",
    "setcopyright",
    "ccsdesc",
    "settopmatter",
    "received",
    "keywordsname",
    "column",
];

/// Commands worth recovering from the preamble, with how many `{}` groups
/// they take.
const PREAMBLE_CALLS: &[(&str, usize)] = &[
    ("title", 1),
    ("subtitle", 1),
    ("author", 1),
    ("date", 1),
    ("keywords", 1),
    ("institute", 1),
    ("affiliation", 1),
    ("affil", 1),
    ("address", 1),
    ("email", 1),
    ("orcid", 1),
    ("subject", 1),
    ("titlehead", 1),
    ("publishers", 1),
    ("dedication", 1),
    ("name", 2),
    ("phone", 1),
    ("mobile", 1),
    ("homepage", 1),
    ("social", 1),
    ("extrainfo", 1),
    ("signature", 1),
];

lazy_re!(KV_RE, r"^\s*[A-Za-z]+\s*=");

static PREAMBLE_CALL_RE: std::sync::LazyLock<re::Regex> = std::sync::LazyLock::new(|| {
    let mut names: Vec<&str> = PREAMBLE_CALLS.iter().map(|(n, _)| *n).collect();
    // sorted(..., key=len, reverse=True): a stable sort, longest first
    names.sort_by_key(|n| std::cmp::Reverse(n.chars().count()));
    re::compile(
        &format!(r"\\({})(?![A-Za-z@])", names.join("|")),
        Flags::NONE,
    )
});

/// `(name, call source)` for every front-matter command in the preamble, in
/// order (several `\author`/`\affil` lines included). Mentions inside
/// definitions (`\renewcommand{\title}`) have no argument and are skipped.
pub(crate) fn preamble_calls(preamble: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = preamble.as_bytes();
    for m in PREAMBLE_CALL_RE.find_iter(preamble) {
        let name = m.group_str(1).to_string();
        let want = PREAMBLE_CALLS
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(0, |(_, k)| *k);
        let mut i = m.end();
        if i < b.len() && b[i] == b'*' {
            i += 1;
        }
        let mut groups = 0;
        while groups < want {
            let mut j = i;
            while j < b.len() && matches!(b[j], b' ' | b'\t' | b'\n') {
                j += 1;
            }
            if j < b.len() && b[j] == b'[' {
                let Some(close) = balanced_end(preamble, j, '[', ']') else {
                    break;
                };
                i = close + 1;
                continue;
            }
            if j < b.len() && b[j] == b'{' {
                let Some(close) = balanced_end(preamble, j, '{', '}') else {
                    break;
                };
                i = close + 1;
                groups += 1;
                continue;
            }
            break;
        }
        if groups == want {
            out.push((name, preamble[m.start()..i].to_string()));
        }
    }
    out
}

/// Byte index of the delimiter closing the one at `i` (`_balanced_end`).
fn balanced_end(s: &str, i: usize, op: char, cl: char) -> Option<usize> {
    let mut depth = 0i64;
    let mut chars = s[i..].char_indices();
    while let Some((off, c)) = chars.next() {
        if c == '\\' {
            chars.next(); // skip the escaped character
            continue;
        }
        if c == op {
            depth += 1;
        } else if c == cl {
            depth -= 1;
            if depth == 0 {
                return Some(i + off);
            }
        }
    }
    None
}

fn sup(text: &str) -> Inline {
    Inline::Emphasis(Emphasis {
        inlines: vec![Inline::text(text)],
        kind: "superscript".into(),
    })
}

fn emph(inlines: Vec<Inline>, kind: &str) -> Inline {
    Inline::Emphasis(Emphasis {
        inlines,
        kind: kind.into(),
    })
}

fn para(inlines: Vec<Inline>) -> Block {
    Block::Paragraph(Paragraph {
        inlines,
        align: None,
    })
}

fn unnumbered_heading(level: i64, inlines: Vec<Inline>, part: bool) -> Block {
    Block::Heading(Heading {
        level,
        inlines,
        numbered: false,
        part,
        ..Default::default()
    })
}

fn orcid_link(oid: &str, url: String) -> Vec<Inline> {
    vec![Inline::Link(Link {
        inlines: vec![Inline::text(format!("ORCID: {oid}"))],
        url,
        anchor: None,
    })]
}

/// Join the non-empty `parts` with `sep`.
fn join(parts: &[Vec<Inline>], sep: &str) -> Vec<Inline> {
    let mut out = Vec::new();
    for (i, p) in parts.iter().filter(|p| !p.is_empty()).enumerate() {
        if i > 0 {
            out.push(Inline::text(sep));
        }
        out.extend(p.iter().cloned());
    }
    out
}

fn blank(n: &Node) -> bool {
    match &n.kind {
        NodeKind::Chars { chars } => pystr::strip(chars).is_empty(),
        NodeKind::Comment { .. } => true,
        _ => false,
    }
}

fn top_level_split(s: &str, sep: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut depth = 0i64;
    for c in s.chars() {
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
        }
        if c == sep && depth == 0 {
            parts.push(std::mem::take(&mut buf));
        } else {
            buf.push(c);
        }
    }
    parts.push(buf);
    parts
}

fn kv_value(part: &str) -> String {
    let value = part.split_once('=').map_or("", |(_, v)| v);
    let mut value = pystr::strip(value);
    if value.starts_with('{') && value.ends_with('}') && !value.is_empty() {
        // value[1:-1]: a lone "{" leaves ""
        value = if value.len() >= 2 {
            &value[1..value.len() - 1]
        } else {
            ""
        };
    }
    pystr::strip(value).to_string()
}

/// The first `[...]` argument as plain text (None if absent).
fn opt_arg(node: &Node) -> Option<String> {
    argnlist(node)
        .iter()
        .flatten()
        .find(|a| is_bracket_group(a))
        .map(|a| pystr::strip(&chars_of(a.nodelist().unwrap_or(&[]))).to_string())
}

fn starred(node: &Node) -> bool {
    argnlist(node)
        .iter()
        .flatten()
        .any(|a| a.chars() == Some("*"))
}

/// Split at `\and`, `\And` and `\AND` (several authors).
fn segments(nodes: &[Node]) -> Vec<Vec<Node>> {
    let mut out: Vec<Vec<Node>> = vec![Vec::new()];
    for n in nodes {
        if matches!(n.macroname(), Some("and" | "And" | "AND")) {
            out.push(Vec::new());
        } else if let Some(last) = out.last_mut() {
            last.push(n.clone());
        }
    }
    out.retain(|s| s.iter().any(|x| !blank(x)));
    out
}

impl Builder {
    fn fm_inl(&mut self, nodes: &[Node]) -> Vec<Inline> {
        let inl = self.inlines(nodes);
        clean_inlines(inl, true)
    }

    /// An affiliation: sub-fields (`\institution{..}\city{..}`) are joined
    /// with ", " unless the source already separates them.
    fn affil_inlines(&mut self, nodes: &[Node]) -> Vec<Inline> {
        let raw: String = nodes.iter().map(Node::latex_verbatim).collect();
        let raw = pystr::strip(&raw);
        if KV_RE.match_at(raw, 0).is_some() {
            // elsarticle organization={..},city={..}
            let parts: Vec<String> = top_level_split(raw, ',')
                .iter()
                .map(|p| kv_value(p))
                .filter(|p| !p.is_empty())
                .collect();
            return self.fragment_inlines(&parts.join(", "));
        }
        let mut out = Vec::new();
        let mut last_part = false;
        for n in nodes {
            if let Some(name) = n.macroname().filter(|m| AFFIL_PARTS.contains(m)) {
                let _ = name;
                let groups = brace_groups(n);
                let inner = match groups.last() {
                    Some(g) => self.affil_inlines(g),
                    None => Vec::new(),
                };
                if !inner.is_empty() {
                    if last_part {
                        out.push(Inline::text(", "));
                    }
                    out.extend(inner);
                    last_part = true;
                }
                continue;
            }
            if blank(n) {
                if !last_part {
                    let inl = self.inlines(std::slice::from_ref(n));
                    out.extend(inl);
                }
                continue;
            }
            let inl = self.inlines(std::slice::from_ref(n));
            out.extend(inl);
            last_part = false;
        }
        clean_inlines(out, true)
    }

    fn front_note(&mut self, inl: Vec<Inline>) {
        if !inl.is_empty() {
            self.meta.front_notes.push(inl);
        }
    }

    fn fn_mark(&mut self, label: &str) -> String {
        if let Some(m) = self.fn_marks.get(label) {
            return m.clone();
        }
        let mark = (self.fn_marks.len() + 1).to_string();
        self.fn_marks.insert(label, mark.clone());
        mark
    }

    fn icml_index(&mut self, key: &str) -> usize {
        match self.icml_keys.iter().position(|k| k == key) {
            Some(i) => i + 1,
            None => {
                self.icml_keys.push(key.to_string());
                self.icml_keys.len()
            }
        }
    }

    // -- block-level commands ------------------------------------------- //

    /// Handle a front-matter command: update the metadata and return the
    /// blocks to emit (often none), or `None` if the command is not ours.
    pub(crate) fn front_matter(&mut self, node: &Node) -> Option<Vec<Block>> {
        let name = node.macroname().unwrap_or("");
        if !is_front_matter(name) {
            return None;
        }
        if SILENT.contains(&name) {
            return Some(Vec::new());
        }
        let groups: Vec<Vec<Node>> = brace_groups(node).iter().map(|g| g.to_vec()).collect();
        let last: &[Node] = groups.last().map_or(&[], |g| g.as_slice());
        let moderncv = self.doc_class == "moderncv";
        let letter = self.doc_class == "letter";

        if name == "author" {
            let (opt, star) = (opt_arg(node), starred(node));
            let segs = segments(last);
            for seg in &segs {
                let mut inl = self.fm_inl(seg);
                if inl.is_empty() {
                    continue;
                }
                let marks = format!(
                    "{}{}",
                    opt.as_deref().unwrap_or(""),
                    if star { "*" } else { "" }
                );
                if !marks.is_empty() && segs.len() == 1 {
                    inl.push(sup(&marks));
                }
                self.meta.authors.push(inl);
            }
            return Some(Vec::new());
        }
        if matches!(name, "affil" | "affiliation" | "institute")
            || (name == "address" && !(letter || moderncv))
        {
            let opt = opt_arg(node);
            let segs = segments(last);
            for (i, seg) in segs.iter().enumerate() {
                let inl = self.affil_inlines(seg);
                if inl.is_empty() {
                    continue;
                }
                let mark = match &opt {
                    Some(o) if !o.is_empty() => o.clone(),
                    _ if segs.len() > 1 => (i + 1).to_string(),
                    _ => String::new(),
                };
                if mark.is_empty() {
                    self.meta.affiliations.push(inl);
                } else {
                    let mut v = vec![sup(&mark)];
                    v.extend(inl);
                    self.meta.affiliations.push(v);
                }
            }
            return Some(Vec::new());
        }
        if name == "address" && letter {
            self.letter_from = Some(self.fm_inl(last));
            return Some(Vec::new());
        }
        if matches!(name, "email" | "ead") {
            let inl = self.fm_inl(last);
            if !inl.is_empty() {
                let label = if pystr::lower(&opt_arg(node).unwrap_or_default()) == "url" {
                    "URL: "
                } else {
                    "Email: "
                };
                let mut v = vec![Inline::text(label)];
                v.extend(inl);
                if moderncv {
                    self.meta.front_notes.push(v);
                } else {
                    self.meta.affiliations.push(v);
                }
            }
            return Some(Vec::new());
        }
        if matches!(name, "orcid" | "orcidID") {
            let oid = pystr::strip(&chars_of(last)).to_string();
            if !oid.is_empty() {
                let url = if oid.starts_with("http") {
                    oid.clone()
                } else {
                    format!("https://orcid.org/{oid}")
                };
                self.meta.affiliations.push(orcid_link(&oid, url));
            }
            return Some(Vec::new());
        }
        if name == "keywords" {
            let parts: Vec<Vec<Inline>> = segments(last).iter().map(|s| self.fm_inl(s)).collect();
            self.meta.keywords = Some(join(&parts, " · "));
            return Some(Vec::new());
        }
        if name == "curraddr" {
            let mut v = vec![Inline::text("Current address: ")];
            v.extend(self.fm_inl(last));
            self.meta.affiliations.push(v);
            return Some(Vec::new());
        }
        if name == "urladdr" {
            let url = url_text(last);
            self.meta.affiliations.push(vec![
                Inline::text("URL: "),
                Inline::Link(Link {
                    inlines: vec![Inline::text(url.clone())],
                    url,
                    anchor: None,
                }),
            ]);
            return Some(Vec::new());
        }
        if matches!(name, "dedicatory" | "dedication") {
            let inl = self.fm_inl(last);
            self.front_note(vec![emph(inl, "italic")]);
            return Some(Vec::new());
        }
        if name == "subtitle" || (name == "title" && moderncv) {
            self.meta.subtitle = Some(self.fm_inl(last));
            return Some(Vec::new());
        }
        if matches!(
            name,
            "subject"
                | "titlehead"
                | "publishers"
                | "uppertitleback"
                | "lowertitleback"
                | "extratitle"
        ) {
            let inl = self.fm_inl(last);
            self.front_note(inl);
            return Some(Vec::new());
        }
        if matches!(name, "cortext" | "fntext") {
            let mark = if name == "cortext" {
                "\u{2217}".to_string()
            } else {
                self.fn_mark(&opt_arg(node).unwrap_or_default())
            };
            let mut v = vec![sup(&mark), Inline::text(" ")];
            v.extend(self.fm_inl(last));
            self.front_note(v);
            return Some(Vec::new());
        }
        if name == "abstract" {
            // sn-jnl \abstract{...}
            self.meta.abstract_ = Some(self.blocks(last));
            return Some(Vec::new());
        }
        if name == "icmltitle" {
            self.meta.title = Some(self.fm_inl(last));
            return Some(Vec::new());
        }
        if name == "icmlauthor" && groups.len() >= 2 {
            let mut inl = self.fm_inl(&groups[0]);
            let keys: Vec<String> = chars_of(&groups[1])
                .split(',')
                .map(|k| pystr::strip(k).to_string())
                .filter(|k| !k.is_empty())
                .collect();
            let nums: Vec<String> = keys
                .iter()
                .map(|k| self.icml_index(k).to_string())
                .collect();
            if !nums.is_empty() {
                inl.push(sup(&nums.join(",")));
            }
            self.meta.authors.push(inl);
            return Some(Vec::new());
        }
        if name == "icmlaffiliation" && groups.len() >= 2 {
            let key = pystr::strip(&chars_of(&groups[0])).to_string();
            let mark = self.icml_index(&key).to_string();
            let mut v = vec![sup(&mark)];
            v.extend(self.affil_inlines(&groups[1]));
            self.meta.affiliations.push(v);
            return Some(Vec::new());
        }
        if name == "icmlcorrespondingauthor" && groups.len() >= 2 {
            let mut v = vec![Inline::text("Correspondence to: ")];
            v.extend(self.fm_inl(&groups[0]));
            v.push(Inline::text(" <"));
            v.extend(self.fm_inl(&groups[1]));
            v.push(Inline::text(">"));
            self.front_note(v);
            return Some(Vec::new());
        }
        if name == "appendices" {
            // IEEEtran
            self.in_appendix = true;
            return Some(Vec::new());
        }
        if name == "bmhead" {
            // sn-jnl back-matter heading
            return Some(vec![unnumbered_heading(2, self.fm_inl(last), false)]);
        }
        if matches!(name, "addsec" | "addchap" | "addpart") {
            // KOMA unnumbered divisions
            return Some(vec![unnumbered_heading(
                1,
                self.fm_inl(last),
                name == "addpart",
            )]);
        }
        if name == "minisec" {
            return Some(vec![para(vec![emph(self.fm_inl(last), "bold")])]);
        }
        if name == "frametitle" {
            return Some(vec![unnumbered_heading(2, self.fm_inl(last), false)]);
        }
        if name == "framesubtitle" {
            return Some(vec![para(vec![emph(self.fm_inl(last), "italic")])]);
        }
        if name == "frame" {
            // beamer \frame{...}
            return Some(self.blocks(last));
        }
        if name == "opening" {
            return Some(vec![para(self.fm_inl(last))]);
        }
        if name == "closing" {
            let mut out = vec![para(self.fm_inl(last))];
            if let Some(sig) = self.letter_signature.clone().filter(|s| !s.is_empty()) {
                out.push(para(sig));
            }
            return Some(out);
        }
        if matches!(name, "ps" | "cc" | "encl") {
            let mut v = match name {
                "cc" => vec![Inline::text("cc: ")],
                "encl" => vec![Inline::text("encl: ")],
                _ => Vec::new(),
            };
            v.extend(self.fm_inl(last));
            return Some(vec![para(v)]);
        }
        if name == "signature" {
            self.letter_signature = Some(self.fm_inl(last));
            return Some(Vec::new());
        }
        if name == "name" && moderncv && groups.len() >= 2 {
            let mut v = self.fm_inl(&groups[0]);
            v.push(Inline::text(" "));
            v.extend(self.fm_inl(&groups[1]));
            self.meta.title = Some(v);
            return Some(Vec::new());
        }
        if matches!(
            name,
            "phone" | "mobile" | "homepage" | "social" | "extrainfo"
        ) || (name == "address" && moderncv)
        {
            let field = match name {
                "phone" => Some("Phone".to_string()),
                "mobile" => Some("Mobile".to_string()),
                "homepage" => Some("Homepage".to_string()),
                "social" => Some(pystr::capitalize(
                    &opt_arg(node)
                        .filter(|o| !o.is_empty())
                        .unwrap_or_else(|| "Social".into()),
                )),
                "address" => Some("Address".to_string()),
                _ => None,
            };
            let parts: Vec<Vec<Inline>> = groups.iter().map(|g| self.fm_inl(g)).collect();
            let mut line = match field {
                Some(f) => vec![Inline::text(format!("{f}: "))],
                None => Vec::new(),
            };
            line.extend(join(&parts, ", "));
            self.front_note(line);
            return Some(Vec::new());
        }
        if matches!(
            name,
            "cventry"
                | "cvitem"
                | "cvitemwithcomment"
                | "cvlistitem"
                | "cvdoubleitem"
                | "cvlistdoubleitem"
                | "cvline"
        ) {
            return Some(self.cv(name, &groups));
        }
        None
    }

    /// moderncv entries as plain paragraphs.
    fn cv(&mut self, name: &str, groups: &[Vec<Node>]) -> Vec<Block> {
        let g: Vec<Vec<Inline>> = groups.iter().map(|x| self.fm_inl(x)).collect();
        let item = |label: &[Inline], text: &[Inline]| -> Block {
            let mut v = Vec::new();
            if !label.is_empty() {
                v.push(emph(label.to_vec(), "bold"));
                v.push(Inline::text(": "));
            }
            v.extend(text.iter().cloned());
            para(v)
        };
        let bullet = |text: &[Inline]| -> Block {
            let mut v = vec![Inline::text("• ")];
            v.extend(text.iter().cloned());
            para(v)
        };
        if name == "cventry" && g.len() >= 6 {
            let mut line = Vec::new();
            if !g[0].is_empty() {
                line.extend(g[0].iter().cloned());
                line.push(Inline::text("  "));
            }
            if !g[1].is_empty() {
                line.push(emph(g[1].clone(), "bold"));
            }
            let rest = join(&g[2..5], ", ");
            if !rest.is_empty() {
                line.push(Inline::text(if g[1].is_empty() { "" } else { ", " }));
                line.extend(rest);
            }
            if !g[5].is_empty() {
                line.push(Inline::LineBreak(LineBreak {}));
                line.extend(g[5].iter().cloned());
            }
            return vec![para(line)];
        }
        if matches!(name, "cvitem" | "cvline") && g.len() >= 2 {
            return vec![item(&g[0], &g[1])];
        }
        if name == "cvitemwithcomment" && g.len() >= 3 {
            return vec![item(&g[0], &join(&[g[1].clone(), g[2].clone()], " — "))];
        }
        if name == "cvlistitem" && !g.is_empty() {
            return vec![bullet(&g[0])];
        }
        if name == "cvdoubleitem" && g.len() >= 4 {
            return vec![item(&g[0], &g[1]), item(&g[2], &g[3])];
        }
        if name == "cvlistdoubleitem" && g.len() >= 2 {
            return vec![bullet(&g[0]), bullet(&g[1])];
        }
        Vec::new()
    }

    // -- inline commands ------------------------------------------------ //

    pub(crate) fn front_inline(&mut self, node: &Node, out: &mut Vec<Inline>) -> bool {
        let name = node.macroname().unwrap_or("");
        match name {
            "corref" => {
                out.push(sup("\u{2217}"));
                true
            }
            "fnref" => {
                let label = pystr::strip(&chars_of(&group_nodes(node))).to_string();
                let marks: Vec<String> = label
                    .split(',')
                    .map(|x| self.fn_mark(pystr::strip(x)))
                    .collect();
                out.push(sup(&marks.join(",")));
                true
            }
            "IEEEmembership" => {
                let inl = self.fm_inl(&group_nodes(node));
                out.push(emph(inl, "italic"));
                true
            }
            "orcidID" => {
                let oid = pystr::strip(&chars_of(&group_nodes(node))).to_string();
                if !oid.is_empty() {
                    let url = format!("https://orcid.org/{oid}");
                    self.meta.affiliations.push(orcid_link(&oid, url));
                }
                true
            }
            "email" | "ead" => {
                // inside \institute / an author block
                let inl = self.fm_inl(&group_nodes(node));
                out.push(emph(inl, "typewriter"));
                true
            }
            "and" | "And" | "AND" => {
                out.push(Inline::text(" · "));
                true
            }
            "alert" => {
                let inl = self.fm_inl(&group_nodes(node));
                out.push(emph(inl, "bold"));
                true
            }
            "newthought" => {
                let inl = self.fm_inl(&group_nodes(node));
                out.push(emph(inl, "smallcaps"));
                true
            }
            "pause" | "titlepage" | "IEEEpeerreviewmaketitle" | "makecvtitle" | "column" => true,
            _ if is_transparent(name) => {
                let groups = brace_groups(node);
                if let Some(g) = groups.last() {
                    let inl = self.inlines(g);
                    out.extend(inl);
                }
                true
            }
            _ => false,
        }
    }

    // -- environments --------------------------------------------------- //

    pub(crate) fn front_env(&mut self, node: &Node, out: &mut Vec<Block>) -> bool {
        let base = node.environmentname().unwrap_or("").trim_end_matches('*');
        let nodelist = node.nodelist().unwrap_or(&[]);
        if base == "letter" {
            let first: Option<Vec<Node>> = argnlist(node)
                .iter()
                .flatten()
                .find(|a| matches!(group_parts(a), Some((_, d)) if d.0 == "{"))
                .map(|a| a.nodelist().unwrap_or(&[]).to_vec());
            if let Some(from) = self.letter_from.clone().filter(|f| !f.is_empty()) {
                out.push(Block::Paragraph(Paragraph {
                    inlines: from,
                    align: Some("right".into()),
                }));
            }
            if let Some(g) = first {
                out.push(para(self.fm_inl(&g)));
            }
            let inner = self.blocks(nodelist);
            out.extend(inner);
            return true;
        }
        if base == "frame" {
            // beamer: \begin{frame}[opts]{Title}{Subtitle}
            let mut nodes: Vec<Node> = nodelist.to_vec();
            while nodes.first().is_some_and(blank) {
                nodes.remove(0);
            }
            if let Some(chars) = nodes
                .first()
                .and_then(|n| n.chars())
                .filter(|c| c.starts_with('['))
                .map(str::to_string)
            {
                let rest = chars.find(']').map_or("", |close| &chars[close + 1..]);
                nodes.remove(0);
                if !pystr::strip(rest).is_empty() {
                    // _chars_like: a chars node placed at the environment
                    let mut n = node.clone();
                    n.len = rest.len();
                    n.kind = NodeKind::Chars {
                        chars: rest.to_string(),
                    };
                    nodes.insert(0, n);
                }
            }
            while nodes.first().is_some_and(blank) {
                nodes.remove(0);
            }
            if nodes.first().is_some_and(Node::is_group) {
                let g = nodes.remove(0);
                let title = self.fm_inl(g.nodelist().unwrap_or(&[]));
                if !title.is_empty() {
                    out.push(unnumbered_heading(2, title, false));
                }
            }
            let inner = self.blocks(&nodes);
            out.extend(inner);
            return true;
        }
        if base == "icmlauthorlist" {
            let inner = self.blocks(nodelist);
            out.extend(inner);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preamble_calls_finds_every_call() {
        let calls = preamble_calls(
            "\\renewcommand{\\title}{x}\\author[1]{A}\n\\author*{B}\\affil[1]{U}\\name{F}{L}\\title{T}",
        );
        let names: Vec<&str> = calls.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["author", "author", "affil", "name", "title"]);
        assert_eq!(calls[0].1, "\\author[1]{A}");
        assert_eq!(calls[3].1, "\\name{F}{L}");
    }

    #[test]
    fn kv_values() {
        assert_eq!(
            top_level_split("organization={A, B},city={C}", ',')
                .iter()
                .map(|p| kv_value(p))
                .collect::<Vec<_>>(),
            ["A, B", "C"]
        );
    }
}
