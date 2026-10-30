//! LaTeX -> IR conversion built on the pylatexenc-compatible walker (port of
//! `frontend/parser.py`).
//!
//! Macro expansion (`\newcommand`/`\def`) is done *before* walking, so custom
//! commands become standard content. Unknown constructs degrade gracefully
//! into `RawInline`/`RawPassthrough` nodes plus a coverage-report entry --
//! never an abort.

mod builder;
mod frontmatter;
mod helpers;

use std::io;
use std::sync::Arc;

use tex2word_ir::pyos::{self, Errors};
use tex2word_ir::pystr;
use tex2word_ir::{Block, CSLItem, ConversionReport, Dict, Document, Inline};
use tex2word_latexwalker::{
    ContextDb, EnvironmentSpec, LatexWalker, MacroSpec, ParsingState, Stop,
};

use builder::Builder;
use helpers::*;
pub use helpers::{split_document, today_text};

use crate::macros::{collect_macros, expand, expand_macros_known, local_package_sources};
use crate::preprocess::{normalize_spacing_and_icons, preprocess, replace_inline_tikz};

/// The `tex2word` category's macro specs, in Python's order (generated from
/// `_build_context`, after its duplicates collapse).
const CONTEXT_MACROS: &[(&str, &str)] = &[
    ("section", "*[{"),
    ("subsection", "*[{"),
    ("subsubsection", "*[{"),
    ("paragraph", "*[{"),
    ("subparagraph", "*[{"),
    ("chapter", "*[{"),
    ("part", "*[{"),
    ("caption", "*[{"),
    ("captionof", "*{[{"),
    ("AddToShipoutPicture", "*{"),
    ("newcounter", "{["),
    ("addtocounter", "{{"),
    ("refstepcounter", "{"),
    ("stepcounter", "{"),
    ("textsuperscript", "{"),
    ("textsubscript", "{"),
    ("sout", "{"),
    ("uline", "{"),
    ("uuline", "{"),
    ("xout", "{"),
    ("st", "{"),
    ("hl", "{"),
    ("href", "{{"),
    ("url", "{"),
    ("hyperref", "[{"),
    ("multicolumn", "{{{"),
    ("multirow", "{{{"),
    ("citep", "[[{"),
    ("citet", "[[{"),
    ("Citep", "[[{"),
    ("Citet", "[[{"),
    ("citealp", "[[{"),
    ("citealt", "[[{"),
    ("citeauthor", "[[{"),
    ("Citeauthor", "[[{"),
    ("citeyear", "[[{"),
    ("citeyearpar", "[[{"),
    ("citenum", "[[{"),
    ("smartcite", "[[{"),
    ("parencite", "[[{"),
    ("textcite", "[[{"),
    ("autocite", "[[{"),
    ("Autocite", "[[{"),
    ("Parencite", "[[{"),
    ("Textcite", "[[{"),
    ("Cite", "[[{"),
    ("IEEEPARstart", "{{"),
    ("autoref", "{"),
    ("cref", "{"),
    ("Cref", "{"),
    ("crefrange", "{{"),
    ("Crefrange", "{{"),
    ("eqref", "{"),
    ("pageref", "*{"),
    ("vref", "*{"),
    ("vpageref", "*{"),
    ("cpageref", "{"),
    ("Cpageref", "{"),
    ("labelcref", "{"),
    ("hypersetup", "{"),
    ("graphicspath", "{"),
    ("definecolor", "{{{"),
    ("colorlet", "{{"),
    ("textcolor", "[{{"),
    ("colorbox", "[{{"),
    ("fcolorbox", "[{{{"),
    ("color", "[{"),
    ("cellcolor", "[{"),
    ("rowcolor", "[{"),
    ("pagenumbering", "{"),
    ("keywords", "{"),
    ("IEEEkeywords", "{"),
    ("endnote", "{"),
    ("index", "{"),
    ("enquote", "*{"),
    ("textquote", "*[[{"),
    ("foreignquote", "*{{"),
    ("hyphenquote", "*{{"),
    ("blockquote", "*[[{"),
    ("blockcquote", "*[[{{"),
    ("foreignblockquote", "*{[[{"),
    ("epigraph", "{{"),
    ("thanks", "{"),
    ("marginpar", "[{"),
    ("sidenote", "[{"),
    ("footnotemark", "["),
    ("footnotetext", "[{"),
    ("nicefrac", "{{"),
    ("sfrac", "{{"),
    ("numrange", "[{{"),
    ("SIrange", "[{{{"),
    ("qtyrange", "[{{{"),
    ("numlist", "[{"),
    ("SIlist", "[{{"),
    ("qtylist", "[{{"),
    ("inst", "{"),
    ("tnote", "{"),
    ("subref", "*{"),
    ("twlistingcaption", "{{{"),
    ("IEEEauthorrefmark", "{"),
    ("IEEEauthorblockN", "{"),
    ("IEEEauthorblockA", "{"),
    ("todo", "[{"),
    ("comment", "[{"),
    ("note", "[{"),
    ("bibitem", "[{"),
    ("bibliography", "{"),
    ("bibliographystyle", "{"),
    ("addbibresource", "[{"),
    ("printbibliography", "["),
    ("setitemize", "{"),
    ("setenumerate", "{"),
    ("hyphenation", "{"),
    ("cmidrule", "{"),
    ("cline", "{"),
    ("resizebox", "{{{"),
    ("scalebox", "{[{"),
    ("rotatebox", "[{{"),
    ("parbox", "[[[{{"),
    ("foreignlanguage", "[{{"),
    ("selectlanguage", "{"),
    ("MakeUppercase", "{"),
    ("MakeLowercase", "{"),
    ("MakeTextUppercase", "{"),
    ("MakeTextLowercase", "{"),
    ("makecell", "[{"),
    ("thead", "[{"),
    ("path", "{"),
    ("nolinkurl", "{"),
    ("typeout", "{"),
    ("twocolumn", "["),
    ("subjclass", "[{"),
    ("setlength", "{{"),
    ("addtolength", "{{"),
    ("settowidth", "{{"),
    ("settoheight", "{{"),
    ("settodepth", "{{"),
    ("deflength", "{{"),
    ("phantom", "{"),
    ("hphantom", "{"),
    ("vphantom", "{"),
    ("rule", "[{{"),
    ("mbox", "{"),
    ("fbox", "{"),
    ("framebox", "[[{"),
    ("makebox", "[[{"),
    ("raisebox", "{[[{"),
    ("fontsize", "{{"),
    ("fontfamily", "{"),
    ("si", "[{"),
    ("unit", "[{"),
    ("num", "[{"),
    ("ang", "[{"),
    ("SI", "[{{"),
    ("qty", "[{{"),
    ("newacronym", "[{{{"),
    ("newglossaryentry", "{{"),
    ("nocite", "{"),
    ("lstinline", "[{"),
    ("mintinline", "[{{"),
    ("ensuremath", "{"),
    ("ding", "{"),
    ("shortstack", "[{"),
    ("authornote", "{"),
    ("texorpdfstring", "{{"),
    ("nameref", "{"),
    ("Nameref", "{"),
    ("title", "[{"),
    ("author", "*[{"),
    ("affil", "*[{"),
    ("affiliation", "*[{"),
    ("institute", "{"),
    ("address", "[{"),
    ("email", "[{"),
    ("ead", "[{"),
    ("orcid", "{"),
    ("orcidID", "{"),
    ("curraddr", "{"),
    ("urladdr", "[{"),
    ("dedicatory", "{"),
    ("dedication", "{"),
    ("subtitle", "[{"),
    ("subject", "{"),
    ("titlehead", "{"),
    ("publishers", "{"),
    ("uppertitleback", "{"),
    ("lowertitleback", "{"),
    ("extratitle", "{"),
    ("cortext", "[{"),
    ("fntext", "[{"),
    ("corref", "{"),
    ("fnref", "{"),
    ("abstract", "{"),
    ("icmltitle", "{"),
    ("icmlauthor", "{{"),
    ("icmlaffiliation", "{{"),
    ("icmlcorrespondingauthor", "{{"),
    ("icmlkeywords", "{"),
    ("icmlsetsymbol", "{{"),
    ("icmltitlerunning", "{"),
    ("printAffiliationsAndNotice", "{"),
    ("authorrunning", "{"),
    ("titlerunning", "{"),
    ("IEEEmembership", "{"),
    ("bmhead", "{"),
    ("addsec", "*[{"),
    ("addchap", "*[{"),
    ("addpart", "*[{"),
    ("minisec", "{"),
    ("frametitle", "[{"),
    ("framesubtitle", "{"),
    ("frame", "[{"),
    ("opening", "{"),
    ("closing", "{"),
    ("ps", "{"),
    ("cc", "{"),
    ("encl", "{"),
    ("signature", "{"),
    ("name", "{{"),
    ("moderncvstyle", "{"),
    ("moderncvcolor", "{"),
    ("phone", "[{"),
    ("mobile", "{"),
    ("homepage", "{"),
    ("social", "[{"),
    ("extrainfo", "{"),
    ("photo", "[[{"),
    ("cventry", "{{{{{{"),
    ("cvitem", "{{"),
    ("cvitemwithcomment", "{{{"),
    ("cvlistitem", "{"),
    ("cvdoubleitem", "{{{{"),
    ("cvlistdoubleitem", "{{"),
    ("cvline", "{{"),
    ("acmConference", "[{{{"),
    ("acmYear", "{"),
    ("acmDOI", "{"),
    ("acmISBN", "{"),
    ("acmBooktitle", "{"),
    ("acmPrice", "{"),
    ("copyrightyear", "{"),
    ("setcopyright", "{"),
    ("ccsdesc", "[{"),
    ("settopmatter", "{"),
    ("received", "[{"),
    ("marginnote", "[{["),
    ("newthought", "{"),
    ("alert", "{"),
    ("column", "[{"),
    ("fnm", "{"),
    ("sur", "{"),
    ("pfx", "{"),
    ("sfx", "{"),
    ("spfx", "{"),
    ("dgr", "{"),
    ("orgdiv", "{"),
    ("orgname", "{"),
    ("orgaddress", "{"),
    ("street", "{"),
    ("postcode", "{"),
    ("city", "{"),
    ("state", "{"),
    ("country", "{"),
    ("institution", "{"),
    ("department", "[{"),
    ("streetaddress", "{"),
    ("position", "{"),
    ("markboth", "{{"),
    ("markright", "{"),
    ("runninghead", "[{"),
    ("shorttitle", "{"),
    ("gls", "[{"),
    ("Gls", "[{"),
    ("glspl", "[{"),
    ("Glspl", "[{"),
    ("acrshort", "[{"),
    ("Acrshort", "[{"),
    ("acrshortpl", "[{"),
    ("Acrshortpl", "[{"),
    ("acrlong", "[{"),
    ("Acrlong", "[{"),
    ("acrlongpl", "[{"),
    ("Acrlongpl", "[{"),
    ("acrfull", "[{"),
    ("Acrfull", "[{"),
    ("acrfullpl", "[{"),
    ("Acrfullpl", "[{"),
    ("glsentryshort", "[{"),
    ("glsentrylong", "[{"),
    ("ac", "{"),
    ("Ac", "{"),
    ("acp", "{"),
    ("Acp", "{"),
    ("acs", "{"),
    ("Acs", "{"),
    ("acsp", "{"),
    ("Acsp", "{"),
    ("acl", "{"),
    ("Acl", "{"),
    ("aclp", "{"),
    ("Aclp", "{"),
    ("acf", "{"),
    ("Acf", "{"),
    ("acfp", "{"),
    ("Acfp", "{"),
    ("acro", "{[{"),
    ("acrodef", "{[{"),
    ("printacronyms", "["),
    ("printnoidxglossary", "["),
    ("printnoidxglossaries", "["),
    ("printglossaries", "["),
    ("printglossary", "["),
    ("printabbreviations", "["),
    ("pagestyle", "{"),
    ("thispagestyle", "{"),
    ("vspace", "*{"),
    ("hspace", "*{"),
    ("subfloat", "[{"),
    ("subfigure", "[{"),
];

/// The environment specs after the theorem-like ones (generated likewise).
const CONTEXT_ENVS: &[(&str, &str)] = &[
    ("subfigure", "[{"),
    ("subtable", "[{"),
    ("minipage", "[{"),
    ("algorithmic", "["),
    ("algorithmicx", "["),
    ("algpseudocode", "["),
    ("algpseudocodex", "["),
    ("tabularx", "{{"),
    ("tabulary", "{{"),
    ("supertabular", "{"),
    ("xtabular", "{"),
    ("mpsupertabular", "{"),
    ("longtable", "[{"),
    ("wrapfigure", "[{{"),
    ("wraptable", "[{{"),
    ("mdframed", "["),
    ("tcolorbox", "["),
    ("leftbar", "["),
    ("adjustbox", "{"),
    ("multicols", "{["),
    ("multicols*", "{["),
    ("otherlanguage", "{"),
    ("otherlanguage*", "{"),
    ("IEEEbiography", "[{"),
    ("IEEEbiographynophoto", "{"),
];

/// Augment the default walker context with the argument signatures it lacks
/// (`_build_context`).
pub fn build_context(
    extra_theorem_envs: &[String],
    extra_env_specs: &[(String, String)],
) -> ContextDb {
    let mut ctx = ContextDb::default_db();
    let macros: Vec<MacroSpec> = CONTEXT_MACROS
        .iter()
        .map(|&(name, spec)| MacroSpec::new(name.to_string(), spec))
        .collect();
    let e = |name: &str, spec: &str| EnvironmentSpec::new(name.to_string(), spec.to_string());
    let mut envs: Vec<EnvironmentSpec> = Vec::new();
    for (name, _) in THEOREM_ENVS {
        envs.push(e(name, "["));
    }
    envs.push(e("proof", "["));
    for name in extra_theorem_envs {
        envs.push(e(name, "["));
    }
    envs.extend(CONTEXT_ENVS.iter().map(|&(n, s)| e(n, s)));
    // user \newtcolorbox envs that take arguments (\begin{box}{title})
    for (name, spec) in extra_env_specs {
        if !spec.is_empty() {
            envs.push(e(name, spec));
        }
    }
    ctx.add_context_category("tex2word", macros, envs, [], true);
    ctx
}

/// preamble commands some classes use to place a captioned teaser figure on
/// the title page
const TEASER_MACROS: &[&str] = &["pretitlefigure", "teaserfigure", "titlefigure"];

fn walk(content: &str, ctx: &Arc<ContextDb>) -> Option<Vec<tex2word_latexwalker::Node>> {
    LatexWalker::new(content, ctx.clone())
        .get_latex_nodes(0, &Stop::default(), &ParsingState::default())
        .ok()
        .map(|(nodes, _, _)| nodes)
}

/// Parse a preamble teaser-figure command's body into blocks (best effort).
fn preamble_teaser_blocks(
    preamble: &str,
    ctx: &Arc<ContextDb>,
    builder: &mut Builder,
) -> Vec<Block> {
    let mut out = Vec::new();
    for macro_ in TEASER_MACROS {
        let Some(content) = braced_content(preamble, macro_).filter(|c| !c.is_empty()) else {
            continue;
        };
        let Some(nodes) = walk(&content, ctx) else {
            continue; // never let a class-specific teaser abort the conversion
        };
        out.extend(
            builder
                .blocks(&nodes)
                .into_iter()
                .filter(|b| !is_blank_paragraph(b)),
        );
    }
    out
}

/// Recover the title-page commands given in the preamble (best effort).
///
/// Every call counts (authblk lists several `\author`/`\affil` lines); a
/// kind of metadata the body already set is not overridden.
fn fill_meta_from_preamble(preamble: &str, ctx: &Arc<ContextDb>, builder: &mut Builder) {
    if builder.meta.running_head.is_none() {
        builder.meta.running_head = running_head_from_preamble(preamble);
    }
    if matches!(builder.doc_class.as_str(), "letter" | "moderncv") {
        return; // read before the body (see parse_document)
    }
    let meta = &builder.meta;
    let mut skip: Vec<&str> = Vec::new();
    if meta.title.is_some() {
        skip.push("title");
    }
    if !meta.authors.is_empty() {
        skip.push("author");
    }
    if meta.date.is_some() {
        skip.push("date");
    }
    if meta.keywords.is_some() {
        skip.push("keywords");
    }
    if !meta.affiliations.is_empty() {
        skip.extend([
            "institute",
            "affiliation",
            "affil",
            "address",
            "email",
            "orcid",
        ]);
    }
    if meta.subtitle.is_some() {
        skip.push("subtitle");
    }
    fill_meta_from_preamble_calls(preamble, ctx, builder, &skip, None);
}

fn fill_meta_from_preamble_calls(
    preamble: &str,
    ctx: &Arc<ContextDb>,
    builder: &mut Builder,
    skip: &[&str],
    only: Option<&[&str]>,
) {
    for (name, call) in frontmatter::preamble_calls(preamble) {
        if skip.contains(&name.as_str()) || only.is_some_and(|o| !o.contains(&name.as_str())) {
            continue;
        }
        let Some(nodes) = walk(&call, ctx) else {
            continue; // never let a preamble oddity abort the conversion
        };
        let Some(node) = nodes.iter().find(|n| n.is_macro()) else {
            continue;
        };
        if builder.front_matter(node).is_none() {
            builder.meta_macro(node);
        }
    }
}

/// The date `\maketitle` shows when no `\date` is given: the document's own
/// `\today` if it redefines it, else the current date.
fn default_date(source: &str, builder: &mut Builder) -> Vec<Inline> {
    let (macros, _) = collect_macros(source);
    if macros.contains_key("today") {
        let expanded = expand("\\today", &macros);
        let text = pystr::strip(&expanded);
        if !text.is_empty() && !text.contains("\\today") {
            let inl = builder.fragment_inlines(text);
            if !inl.is_empty() {
                return inl;
            }
        }
    }
    vec![Inline::text(today_text())]
}

helpers::lazy_re!(
    ADDBIBRESOURCE_RE,
    r"\\addbibresource(?:\[[^\]]*\])?\{([^}]*)\}"
);
helpers::lazy_re!(BIBLATEX_RE, r"\\usepackage\[([^\]]*)\]\{biblatex\}");
helpers::lazy_re!(BIBLATEX_STYLE_RE, r"\bstyle\s*=\s*([A-Za-z0-9\-]+)");
helpers::lazy_re!(NATBIB_RE, r"\\usepackage\[([^\]]*)\]\{natbib\}");
helpers::lazy_re!(NUMBERS_RE, r"\bnumbers\b");
helpers::lazy_re!(
    DOCCLASS_RE,
    r"\\documentclass\s*(?:\[[^\]]*\])?\s*\{([^}]*)\}"
);
helpers::lazy_re!(
    SUBJCLASSNAME_RE,
    r"\\@namedef\s*\{subjclassname@([^}]*)\}\s*\{([^{}]*)\}"
);

/// amsart `\@namedef{subjclassname@JEL}{JEL classification}`: the label a
/// `\subjclass[JEL]{...}` line is printed with.
fn collect_subjclass_names(source: &str, out: &mut std::collections::HashMap<String, String>) {
    for m in SUBJCLASSNAME_RE.find_iter(source) {
        out.insert(
            pystr::strip(m.group_str(1)).to_string(),
            pystr::strip(m.group_str(2)).to_string(),
        );
    }
}
helpers::lazy_re!(AUTHORYEAR_RE, r"\bauthoryear\b");

/// Pick up biblatex `\addbibresource{file.bib}` declared in the preamble.
fn collect_bib_resources(preamble: &str, builder: &mut Builder) {
    for m in ADDBIBRESOURCE_RE.find_iter(preamble) {
        for name in m.group_str(1).split(',') {
            let name = pystr::strip(name);
            if !name.is_empty() && !builder.bib_files.iter().any(|f| f == name) {
                builder.bib_files.push(name.to_string());
            }
        }
    }
}

/// Pick up the citation style from the biblatex/natbib package options when
/// no `\bibliographystyle` set it.
fn detect_bib_style(preamble: &str, builder: &mut Builder) {
    if builder.bibstyle_set {
        // natbib's `numbers` option makes citations numeric whatever the
        // (natbib-compatible) .bst, e.g. [numbers]{natbib} + plainnat
        if let Some(nb) = NATBIB_RE.search(preamble) {
            if NUMBERS_RE.is_match(&pystr::lower(nb.group_str(1))) {
                builder.bib_style = "numeric".into();
            }
        }
        return;
    }
    const AY: &[&str] = &[
        "authoryear",
        "apa",
        "chicago",
        "harvard",
        "mla",
        "author-year",
    ];
    if let Some(m) = BIBLATEX_RE.search(preamble) {
        if let Some(opt) = BIBLATEX_STYLE_RE.search(m.group_str(1)) {
            let s = pystr::lower(opt.group_str(1));
            if AY.iter().any(|a| s.contains(a)) {
                builder.bib_style = "author-year".into();
            } else if s.contains("numeric") || s.contains("ieee") {
                builder.bib_style = "numeric".into();
            }
        }
    }
    if let Some(nb) = NATBIB_RE.search(preamble) {
        let nopt = pystr::lower(nb.group_str(1));
        if NUMBERS_RE.is_match(&nopt) {
            builder.bib_style = "numeric".into();
        } else if AUTHORYEAR_RE.is_match(&nopt) {
            builder.bib_style = "author-year".into();
        }
    }
}

/// Find and parse a single `.bbl` in `base_dir` (if exactly one).
fn load_bbl(base_dir: &str) -> io::Result<Dict<CSLItem>> {
    // glob(join(base_dir, "*.bbl")): hidden files don't match
    let dir = if base_dir.is_empty() { "." } else { base_dir };
    let mut bbls: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".bbl") && !name.starts_with('.') {
                bbls.push(pyos::join(base_dir, &[&name]));
            }
        }
    }
    if bbls.len() != 1 {
        return Ok(Dict::new());
    }
    match std::fs::read(&bbls[0]) {
        Ok(bytes) => {
            let text = pyos::universal_newlines(&pyos::decode_utf8(&bytes, Errors::Strict)?);
            Ok(tex2word_bib::bbl::parse_bbl(&text))
        }
        Err(_) => Ok(Dict::new()), // OSError -> no .bbl
    }
}

fn resolve_bibliography(
    doc: &mut Document,
    builder: &mut Builder,
    base_dir: &str,
    csl_path: Option<&str>,
) -> io::Result<()> {
    let style = builder.bib_style.clone();
    let nocite = builder.nocite_keys.clone();

    // A .bbl (BibTeX's formatted .bst output) is authoritative
    let mut bbl_items = if builder.bib_files.is_empty() {
        Dict::new()
    } else {
        load_bbl(base_dir)?
    };
    if !bbl_items.is_empty() {
        for (k, v) in builder.thebib_items.iter() {
            bbl_items.insert(k.clone(), v.clone());
        }
        let bstyle = tex2word_bib::bbl::bbl_style(&bbl_items);
        tex2word_bib::render::resolve_citations(
            doc,
            &bbl_items,
            &bstyle,
            &mut builder.report,
            csl_path,
            &nocite,
        );
        builder
            .report
            .info("\\bibliography", "used the .bbl (formatted .bst output)");
        return Ok(());
    }

    let mut items: Dict<CSLItem> = Dict::new();
    for name in builder.bib_files.clone() {
        let candidates = if name.ends_with(".bib") {
            vec![name.clone()]
        } else {
            vec![name.clone(), format!("{name}.bib")]
        };
        let mut found = false;
        for cand in candidates {
            let path = pyos::join(base_dir, &[&cand]);
            if pyos::isfile(&path) {
                let text = pyos::read_text(&path, Errors::Strict)?;
                for (k, v) in tex2word_bib::bibtex::parse_bibtex(&text) {
                    items.insert(k, v);
                }
                found = true;
                break;
            }
        }
        if !found {
            builder.report.warn(
                "\\bibliography",
                format!("bibliography file not found: {name}"),
            );
        }
    }
    for (k, v) in builder.thebib_items.iter() {
        items.insert(k.clone(), v.clone());
    }
    if !items.is_empty()
        || !builder.bib_files.is_empty()
        || !builder.thebib_items.is_empty()
        || !nocite.is_empty()
    {
        tex2word_bib::render::resolve_citations(
            doc,
            &items,
            &style,
            &mut builder.report,
            csl_path,
            &nocite,
        );
    }
    Ok(())
}

/// Commands the converter itself handles (`\providecommand` keeps them):
/// `_known_commands()`, generated from the Python tables.
const KNOWN_COMMANDS: &[&str] = &[
    " ",
    "!",
    "#",
    "$",
    "%",
    "&",
    ",",
    "/",
    ":",
    ";",
    "@",
    "AA",
    "AE",
    "AddToShipoutPicture",
    "Autocite",
    "Checkmark",
    "CheckmarkBold",
    "Cite",
    "Citeauthor",
    "Citep",
    "Citet",
    "ClearShipoutPicture",
    "Cpageref",
    "Cref",
    "Crefrange",
    "Cross",
    "CrossBold",
    "DH",
    "DJ",
    "Envelope",
    "FloatBarrier",
    "HandLeft",
    "HandRight",
    "Huge",
    "L",
    "LARGE",
    "LaTeX",
    "LaTeXe",
    "Large",
    "Leftarrow",
    "NG",
    "Nameref",
    "O",
    "OE",
    "P",
    "Parencite",
    "PencilRight",
    "Phone",
    "RequirePackage",
    "Rightarrow",
    "S",
    "SS",
    "TH",
    "TeX",
    "Textcite",
    "XSolid",
    "XSolidBold",
    "XSolidBrush",
    "_",
    "aa",
    "acmnote",
    "addtocounter",
    "addtolength",
    "ae",
    "alpha",
    "approx",
    "ast",
    "author",
    "authornote",
    "authornotemark",
    "autocite",
    "autoref",
    "backmatter",
    "balance",
    "begingroup",
    "beta",
    "bf",
    "bfseries",
    "bgroup",
    "bigskip",
    "boldmath",
    "caption",
    "cdot",
    "cdots",
    "celsius",
    "centering",
    "chapter",
    "checkmark",
    "cite",
    "citealp",
    "citealt",
    "citeauthor",
    "citenum",
    "citenumber",
    "citep",
    "citet",
    "citeyear",
    "citeyearpar",
    "clearpage",
    "copyright",
    "cpageref",
    "cref",
    "crefrange",
    "dag",
    "date",
    "ddag",
    "definecolor",
    "deflength",
    "degree",
    "depth",
    "dh",
    "displaystyle",
    "dj",
    "documentclass",
    "dots",
    "egroup",
    "em",
    "emph",
    "emptyset",
    "endgroup",
    "eqref",
    "equiv",
    "euro",
    "exists",
    "fbox",
    "floatbarrier",
    "flq",
    "flqq",
    "flushbottom",
    "fontfamily",
    "fontsize",
    "footcite",
    "footnote",
    "footnotesize",
    "forall",
    "foreignlanguage",
    "frac",
    "framebox",
    "frenchspacing",
    "frontmatter",
    "frq",
    "frqq",
    "fussy",
    "gamma",
    "ge",
    "geq",
    "gets",
    "glq",
    "glqq",
    "graphicspath",
    "grq",
    "grqq",
    "guillemotleft",
    "guillemotright",
    "height",
    "hfill",
    "hl",
    "href",
    "hspace",
    "huge",
    "hypersetup",
    "hyphenation",
    "i",
    "ignorespaces",
    "in",
    "includegraphics",
    "indent",
    "infty",
    "it",
    "item",
    "itshape",
    "j",
    "l",
    "label",
    "labelcref",
    "land",
    "large",
    "ldots",
    "le",
    "leavevmode",
    "leftarrow",
    "leftrightarrow",
    "leq",
    "lnot",
    "lor",
    "mainmatter",
    "makeatletter",
    "makeatother",
    "makebox",
    "makecell",
    "maketitle",
    "mathbb",
    "mathbf",
    "mathcal",
    "mathrm",
    "mbox",
    "md",
    "mdseries",
    "medskip",
    "micro",
    "nameref",
    "ne",
    "neg",
    "neq",
    "newcommand",
    "newcounter",
    "newline",
    "newpage",
    "ng",
    "nobalance",
    "nobreakspace",
    "nohyphens",
    "noindent",
    "normalfont",
    "normalsize",
    "o",
    "oe",
    "ohm",
    "onecolumn",
    "otherlanguage",
    "pagebreak",
    "pagenumbering",
    "pageref",
    "pagestyle",
    "par",
    "paragraph",
    "parbox",
    "parencite",
    "part",
    "perthousand",
    "pounds",
    "protect",
    "providecommand",
    "qquad",
    "quad",
    "quotedblbase",
    "quotesinglbase",
    "raggedleft",
    "raggedright",
    "raisebox",
    "ref",
    "refstepcounter",
    "renewcommand",
    "resizebox",
    "rightarrow",
    "rm",
    "rmfamily",
    "rotatebox",
    "samepage",
    "sc",
    "scalebox",
    "scriptsize",
    "scshape",
    "section",
    "selectfont",
    "selectlanguage",
    "setcounter",
    "setenumerate",
    "setitemize",
    "setlength",
    "settodepth",
    "settoheight",
    "settopmatter",
    "settowidth",
    "sf",
    "sffamily",
    "sl",
    "slash",
    "sloppy",
    "slshape",
    "small",
    "smallskip",
    "smartcite",
    "sout",
    "sqrt",
    "ss",
    "st",
    "star",
    "stepcounter",
    "subparagraph",
    "subref",
    "subsection",
    "subsubsection",
    "text",
    "textasciicircum",
    "textasciitilde",
    "textbackslash",
    "textbar",
    "textbf",
    "textbullet",
    "textcelsius",
    "textcent",
    "textcite",
    "textdagger",
    "textdaggerdbl",
    "textdegree",
    "textdiv",
    "textdollar",
    "textellipsis",
    "textemdash",
    "textendash",
    "texteuro",
    "textgreater",
    "textit",
    "textless",
    "textmho",
    "textminus",
    "textmu",
    "textnumero",
    "textohm",
    "textonehalf",
    "textonequarter",
    "textonesuperior",
    "textparagraph",
    "textperiodcentered",
    "textperthousand",
    "textpm",
    "textquotedblleft",
    "textquotedblright",
    "textquoteleft",
    "textquoteright",
    "textregistered",
    "textrm",
    "textsc",
    "textsection",
    "textsf",
    "textsl",
    "textsterling",
    "textsubscript",
    "textsuperscript",
    "textthreequarters",
    "textthreesuperior",
    "texttimes",
    "texttrademark",
    "texttt",
    "texttwosuperior",
    "textyen",
    "th",
    "thead",
    "thispagestyle",
    "times",
    "tiny",
    "title",
    "to",
    "today",
    "totalheight",
    "tt",
    "ttfamily",
    "twocolumn",
    "typeout",
    "uline",
    "unboldmath",
    "underline",
    "unskip",
    "up",
    "upshape",
    "url",
    "usepackage",
    "uuline",
    "vfill",
    "vpageref",
    "vref",
    "vspace",
    "width",
    "xout",
    "xspace",
    "{",
    "}",
];

fn known_commands() -> std::collections::HashSet<String> {
    KNOWN_COMMANDS.iter().map(|s| s.to_string()).collect()
}

/// Parse LaTeX `source` into an IR [`Document`] plus its conversion report.
///
/// `csl_path` is an optional `.csl` style for the citation pass. Errors are
/// I/O failures reading `\input` files or bibliographies.
pub fn parse_document(
    source: &str,
    base_dir: &str,
    csl_path: Option<&str>,
) -> io::Result<(Document, ConversionReport)> {
    let report = ConversionReport::new();
    let expanded = replace_inline_tikz(&expand_macros_known(
        &preprocess(source, base_dir)?,
        base_dir,
        &known_commands(),
    ));
    // \newcounter values: \arabic{c}, \thec, ...
    let expanded = crate::counters::resolve_counters(&expanded);
    // after expansion, so macro-generated \vskip/\faX are cleaned up too
    let expanded = normalize_spacing_and_icons(&expanded);
    let (body, preamble) = split_document(&expanded);
    // \newtheorem declarations may live in a \usepackage'd local .sty
    let theorem_src = format!("{expanded}\n{}", local_package_sources(source, base_dir));
    let (custom_theorems, unnumbered_theorems, shared_counters) = collect_newtheorems(&theorem_src);
    let (tcb_envs, tcb_styles) = collect_tcolorbox_defs(&theorem_src);
    let theorem_names: Vec<String> = custom_theorems.keys().cloned().collect();
    let tcb_specs: Vec<(String, String)> = tcb_envs
        .iter()
        .map(|(n, e)| (n.clone(), tcb_argspec(e)))
        .collect();
    let ctx = Arc::new(build_context(&theorem_names, &tcb_specs));
    let walker = LatexWalker::new(body, ctx.clone());
    let (nodes, _, _) = walker
        .get_latex_nodes(0, &Stop::default(), &ParsingState::default())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;

    let mut builder = Builder::new(report);
    builder.ctx = Some(ctx.clone());
    for (k, v) in custom_theorems.iter() {
        builder.theorem_envs.insert(k.clone(), v.clone());
    }
    builder.unnumbered_theorems = unnumbered_theorems;
    builder.theorem_counters = resolve_theorem_counters(&custom_theorems, &shared_counters);
    builder.theorem_within = collect_theorem_within(&theorem_src);
    builder.box_envs = collect_tcolorbox_envs(&theorem_src); // \newtcolorbox callouts
    builder.tcb_envs = tcb_envs;
    builder.tcb_styles = tcb_styles;
    builder.box_styles = collect_mdframed_envs(&theorem_src); // \newmdenv callouts
    let styled: Vec<String> = builder.box_styles.keys().cloned().collect();
    builder.box_envs.extend(styled);
    builder.book_mode = is_book_class(&expanded);
    builder.doc_class = DOCCLASS_RE.search(&expanded).map_or_else(
        || "article".to_string(),
        |m| pystr::strip(m.group_str(1)).to_string(),
    );
    if matches!(builder.doc_class.as_str(), "letter" | "moderncv") {
        // sender address / signature / CV contact lines live in the preamble
        fill_meta_from_preamble_calls(
            &preamble,
            &ctx,
            &mut builder,
            &[],
            Some(&[
                "address",
                "signature",
                "name",
                "title",
                "phone",
                "mobile",
                "homepage",
                "social",
                "extrainfo",
                "email",
            ]),
        );
    }
    collect_subjclass_names(source, &mut builder.subjclass_names); // before \@namedef is expanded
    collect_color_defs(&expanded, &mut builder.colors); // \definecolor/\colorlet
    collect_acronyms(&expanded, &mut builder.acronyms); // \newacronym
    collect_glossary_entries(&expanded, &mut builder.glossary); // \newglossaryentry
                                                                // A class-level teaser figure declared in the preamble leads the body
    let mut blocks = preamble_teaser_blocks(&preamble, &ctx, &mut builder);
    blocks.extend(builder.blocks(&nodes));

    // (Python fills `doc.meta`, which is `builder.meta`: fill it in place)
    fill_meta_from_preamble(&preamble, &ctx, &mut builder);
    // \maketitle defaults an omitted \date to \today when a title is present
    if builder.meta.date.is_none()
        && builder.meta.title.as_ref().is_some_and(|t| !t.is_empty())
        && builder.doc_class != "moderncv"
    {
        builder.meta.date = Some(default_date(source, &mut builder));
    }
    let mut doc = Document {
        blocks,
        meta: std::mem::take(&mut builder.meta),
        book: builder.book_mode,
        ..Document::default()
    };
    // labels and \footnotemark contents set after their nodes were built
    builder.resolve_slots(&mut doc.blocks, &mut doc.meta);
    doc.label_aliases = builder.label_aliases.clone();
    collect_bib_resources(&preamble, &mut builder); // biblatex \addbibresource
    detect_bib_style(&preamble, &mut builder); // biblatex/natbib author-year option
    if doc.meta.language.is_none() {
        doc.meta.language = detect_language(&preamble); // babel/polyglossia
    }
    // fontspec/xeCJK \setmainfont / \setCJK*font
    if let Some(f) = font_cmd(&preamble, "setmainfont") {
        doc.meta.main_font = Some(f);
    }
    if let Some(f) = font_cmd(&preamble, "setCJKmainfont") {
        doc.meta.cjk_main_font = Some(f);
    }
    if let Some(f) = font_cmd(&preamble, "setCJKsansfont") {
        doc.meta.cjk_sans_font = Some(f);
    }
    if let Some(f) = font_cmd(&preamble, "setCJKmonofont") {
        doc.meta.cjk_mono_font = Some(f);
    }
    doc.meta.columns = detect_columns(&expanded);
    resolve_bibliography(&mut doc, &mut builder, base_dir, csl_path)?;
    Ok((doc, builder.report))
}
