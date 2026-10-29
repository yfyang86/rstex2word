//! LaTeX -> IR conversion built on the pylatexenc-compatible walker (port of
//! `frontend/parser.py`).
//!
//! Macro expansion (`\newcommand`/`\def`) is done *before* walking, so custom
//! commands become standard content. Unknown constructs degrade gracefully
//! into `RawInline`/`RawPassthrough` nodes plus a coverage-report entry --
//! never an abort.

mod builder;
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

use crate::macros::{collect_macros, expand, expand_macros, local_package_sources};
use crate::preprocess::{normalize_spacing_and_icons, preprocess, replace_inline_tikz};

/// Augment the default walker context with the argument signatures it lacks
/// (`_build_context`).
pub fn build_context(
    extra_theorem_envs: &[String],
    extra_env_specs: &[(String, String)],
) -> ContextDb {
    let mut ctx = ContextDb::default_db();
    let m = |name: &str, spec: &'static str| MacroSpec::new(name.to_string(), spec);
    let mut macros = vec![
        // sectioning: \cmd*[short]{title}
        m("section", "*[{"),
        m("subsection", "*[{"),
        m("subsubsection", "*[{"),
        m("paragraph", "*[{"),
        m("subparagraph", "*[{"),
        m("chapter", "*[{"),
        m("part", "*[{"),
        m("caption", "*[{"),
        // \captionof*{type}[short]{long}
        m("captionof", "*{[{"),
        // layout / front-matter commands: consume their args
        m("AddToShipoutPicture", "*{"),
        m("newcounter", "{["),
        m("addtocounter", "{{"),
        m("refstepcounter", "{"),
        m("stepcounter", "{"),
        m("textsuperscript", "{"),
        m("textsubscript", "{"),
        m("sout", "{"),
        m("uline", "{"),
        m("uuline", "{"),
        m("xout", "{"),
        m("st", "{"),
        m("hl", "{"),
        m("href", "{{"),
        m("url", "{"),
        // url package: verbatim paths without a link
        m("path", "{"),
        m("nolinkurl", "{"),
        // console/layout commands whose argument must not leak into the text
        m("typeout", "{"),
        m("twocolumn", "["),
        m("hyperref", "[{"),
        m("multicolumn", "{{{"),
        m("multirow", "{{{"),
        m("citep", "[[{"),
        m("citet", "[[{"),
        m("Citep", "[[{"),
        m("Citet", "[[{"),
        m("citealp", "[[{"),
        m("citealt", "[[{"),
        m("citeauthor", "[[{"),
        m("Citeauthor", "[[{"),
        m("citeyear", "[[{"),
        m("citeyearpar", "[[{"),
        m("citenum", "[[{"),
        m("smartcite", "[[{"),
        m("parencite", "[[{"),
        m("textcite", "[[{"),
        m("autocite", "[[{"),
        m("Autocite", "[[{"),
        m("Parencite", "[[{"),
        m("Textcite", "[[{"),
        m("Cite", "[[{"),
        m("IEEEPARstart", "{{"),
        m("autoref", "{"),
        m("cref", "{"),
        m("Cref", "{"),
        m("crefrange", "{{"),
        m("Crefrange", "{{"),
        m("eqref", "{"),
        m("hypersetup", "{"),
        m("graphicspath", "{"),
        m("definecolor", "{{{"),
        m("colorlet", "{{"),
        m("textcolor", "[{{"),
        m("colorbox", "[{{"),
        m("fcolorbox", "[{{{"),
        m("color", "[{"),
        m("cellcolor", "[{"),
        m("rowcolor", "[{"),
        m("pagenumbering", "{"),
        m("keywords", "{"),
        m("subjclass", "[{"),
        m("IEEEkeywords", "{"),
        m("endnote", "{"),
        m("index", "{"),
        m("enquote", "*{"),
        m("textquote", "*[[{"),
        m("foreignquote", "*{{"),
        m("hyphenquote", "*{{"),
        m("blockquote", "*[[{"),
        m("blockcquote", "*[[{{"),
        m("foreignblockquote", "*{[[{"),
        m("epigraph", "{{"),
        m("thanks", "{"),
        m("marginpar", "[{"),
        m("sidenote", "[{"),
        m("footnotemark", "["),
        m("footnotetext", "[{"),
        m("nicefrac", "{{"),
        m("sfrac", "{{"),
        m("numrange", "[{{"),
        m("SIrange", "[{{{"),
        m("qtyrange", "[{{{"),
        m("numlist", "[{"),
        m("SIlist", "[{{"),
        m("qtylist", "[{{"),
        m("institute", "{"),
        m("affiliation", "[{"),
        m("affil", "[{"),
        m("address", "{"),
        m("email", "{"),
        m("orcid", "{"),
        m("inst", "{"),
        m("IEEEauthorrefmark", "{"),
        m("IEEEauthorblockN", "{"),
        m("IEEEauthorblockA", "{"),
        // review annotations -> Word comments (todonotes / changes)
        m("todo", "[{"),
        m("comment", "[{"),
        m("note", "[{"),
        m("bibitem", "[{"),
        m("bibliography", "{"),
        m("bibliographystyle", "{"),
        m("addbibresource", "[{"),
        m("printbibliography", "["),
        m("setitemize", "{"),
        m("setenumerate", "{"),
        m("hyphenation", "{"),
        m("cmidrule", "{"),
        m("cline", "{"),
        m("resizebox", "{{{"),
        m("scalebox", "{{"),
        m("rotatebox", "[{{"),
        // makecell: multi-line cells, `\\` breaks the line
        m("makecell", "[{"),
        m("thead", "[{"),
        m("makegapedcells", ""),
        m("setlength", "{{"),
        m("phantom", "{"),
        m("hphantom", "{"),
        m("vphantom", "{"),
        m("rule", "[{{"),
        m("mbox", "{"),
        m("fbox", "{"),
        m("framebox", "[[{"),
        m("makebox", "[[{"),
        m("raisebox", "{[[{"),
        m("fontsize", "{{"),
        m("fontfamily", "{"),
        m("si", "[{"),
        m("unit", "[{"),
        m("num", "[{"),
        m("ang", "[{"),
        m("SI", "[{{"),
        m("qty", "[{{"),
        m("newacronym", "[{{{"),
        m("newglossaryentry", "{{"),
        m("nocite", "{"),
        m("lstinline", "[{"),
        m("mintinline", "[{{"),
        m("ensuremath", "{"),
        m("ding", "{"),
        m("shortstack", "[{"),
        // ACM affiliation sub-fields: consume the {…} so it isn't body text
        m("institution", "{"),
        m("department", "{"),
        m("city", "{"),
        m("state", "{"),
        m("country", "{"),
        m("postcode", "{"),
        m("streetaddress", "{"),
        m("position", "{"),
        m("authornote", "{"),
        m("texorpdfstring", "{{"),
        m("labelcref", "{"),
        m("nameref", "{"),
        m("Nameref", "{"),
        m("title", "[{"), // \title[running head]{full title}
        m("markboth", "{{"),
        m("markright", "{"),
        m("runninghead", "[{"),
        m("shorttitle", "{"),
    ];
    for name in [
        "gls",
        "Gls",
        "glspl",
        "Glspl",
        "acrshort",
        "Acrshort",
        "acrshortpl",
        "Acrshortpl",
        "acrlong",
        "Acrlong",
        "acrlongpl",
        "Acrlongpl",
        "acrfull",
        "Acrfull",
        "acrfullpl",
        "Acrfullpl",
        "glsentryshort",
        "glsentrylong",
    ] {
        macros.push(m(name, "[{"));
    }
    for name in [
        // acronym package references
        "ac", "Ac", "acp", "Acp", "acs", "Acs", "acsp", "Acsp", "acl", "Acl", "aclp", "Aclp", "acf",
        "Acf", "acfp", "Acfp",
    ] {
        macros.push(m(name, "{"));
    }
    macros.push(m("acro", "{[{"));
    macros.push(m("acrodef", "{[{"));
    for name in PRINTGLOSSARY_MACROS {
        macros.push(m(name, "["));
    }
    macros.extend([
        m("pagestyle", "{"),
        m("thispagestyle", "{"),
        m("settopmatter", "{"),
        m("vspace", "*{"),
        m("hspace", "*{"),
        m("subfloat", "[{"),
        m("subfigure", "[{"),
    ]);

    let e = |name: &str, spec: &str| EnvironmentSpec::new(name.to_string(), spec.to_string());
    let mut envs: Vec<EnvironmentSpec> = Vec::new();
    for (name, _) in THEOREM_ENVS {
        envs.push(e(name, "["));
    }
    envs.push(e("proof", "["));
    for name in extra_theorem_envs {
        envs.push(e(name, "["));
    }
    envs.push(e("subfigure", "[{"));
    envs.push(e("minipage", "[{"));
    for name in [
        "algorithmic",
        "algorithmicx",
        "algpseudocode",
        "algpseudocodex",
    ] {
        envs.push(e(name, "["));
    }
    envs.extend([
        // tabularx/tabulary carry a leading {width}
        e("tabularx", "{{"),
        e("tabulary", "{{"),
        e("supertabular", "{"),
        e("xtabular", "{"),
        e("mpsupertabular", "{"),
        // longtable: \begin{longtable}[pos]{colspec}
        e("longtable", "[{"),
        // wrapfig: \begin{wrapfigure}[lines]{placement}{width}
        e("wrapfigure", "[{{"),
        e("wraptable", "[{{"),
        // boxed environments with an optional [options] argument
        e("mdframed", "["),
        e("tcolorbox", "["),
        e("leftbar", "["),
    ]);
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

/// Run `f` on a fresh builder sharing `report` (Python's `_Builder(report)`),
/// resolving its slots in the inlines it returns.
fn with_fresh_builder(
    report: &mut ConversionReport,
    f: impl FnOnce(&mut Builder) -> Vec<Inline>,
) -> Vec<Inline> {
    let mut b = Builder::new(std::mem::take(report));
    let mut out = f(&mut b);
    b.resolve_inlines(&mut out);
    *report = std::mem::take(&mut b.report);
    out
}

/// Recover `\title`/`\author`/`\date`/`\keywords` defined in the preamble.
fn fill_meta_from_preamble(
    doc: &mut Document,
    preamble: &str,
    ctx: &Arc<ContextDb>,
    report: &mut ConversionReport,
) {
    if doc.meta.running_head.is_none() {
        doc.meta.running_head = running_head_from_preamble(preamble);
    }
    for macro_ in [
        "title",
        "author",
        "date",
        "keywords",
        "institute",
        "affiliation",
    ] {
        let skip = match macro_ {
            "title" => doc.meta.title.is_some(),
            "author" => !doc.meta.authors.is_empty(),
            "date" => doc.meta.date.is_some(),
            "keywords" => doc.meta.keywords.is_some(),
            _ => !doc.meta.affiliations.is_empty(),
        };
        if skip {
            continue;
        }
        let Some(content) = braced_content(preamble, macro_) else {
            continue;
        };
        let inlines = match walk(&content, ctx) {
            Some(sub_nodes) => {
                if matches!(macro_, "author" | "institute" | "affiliation") {
                    for seg in split_on_and(&sub_nodes) {
                        let inl = with_fresh_builder(report, |b| b.inlines(&seg));
                        if !inl.is_empty() {
                            if macro_ == "author" {
                                doc.meta.authors.push(inl);
                            } else {
                                doc.meta.affiliations.push(inl);
                            }
                        }
                    }
                    continue;
                }
                with_fresh_builder(report, |b| b.inlines(&sub_nodes))
            }
            None => vec![Inline::text(content.clone())],
        };
        match macro_ {
            "title" => doc.meta.title = Some(inlines),
            "date" => doc.meta.date = Some(inlines),
            "keywords" => doc.meta.keywords = Some(inlines),
            _ => {}
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
    let expanded = replace_inline_tikz(&expand_macros(&preprocess(source, base_dir)?, base_dir));
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
    builder.box_envs = collect_tcolorbox_envs(&theorem_src); // \newtcolorbox callouts
    builder.tcb_envs = tcb_envs;
    builder.tcb_styles = tcb_styles;
    builder.box_styles = collect_mdframed_envs(&theorem_src); // \newmdenv callouts
    let styled: Vec<String> = builder.box_styles.keys().cloned().collect();
    builder.box_envs.extend(styled);
    builder.book_mode = is_book_class(&expanded);
    collect_subjclass_names(source, &mut builder.subjclass_names); // amsart
    collect_color_defs(&expanded, &mut builder.colors); // \definecolor/\colorlet
    collect_acronyms(&expanded, &mut builder.acronyms); // \newacronym
    collect_glossary_entries(&expanded, &mut builder.glossary); // \newglossaryentry
                                                                // A class-level teaser figure declared in the preamble leads the body
    let mut blocks = preamble_teaser_blocks(&preamble, &ctx, &mut builder);
    blocks.extend(builder.blocks(&nodes));
    let mut doc = Document {
        blocks,
        meta: std::mem::take(&mut builder.meta),
        book: builder.book_mode,
        ..Document::default()
    };

    let mut report = std::mem::take(&mut builder.report);
    fill_meta_from_preamble(&mut doc, &preamble, &ctx, &mut report);
    builder.report = report;
    // \maketitle defaults an omitted \date to \today when a title is present
    if doc.meta.date.is_none() && doc.meta.title.as_ref().is_some_and(|t| !t.is_empty()) {
        doc.meta.date = Some(default_date(source, &mut builder));
    }
    // labels and \footnotemark contents set after their nodes were built
    builder.resolve_slots(&mut doc.blocks, &mut doc.meta);
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
