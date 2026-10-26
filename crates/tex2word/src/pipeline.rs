//! End-to-end orchestration: LaTeX source -> IR -> transforms -> `.docx`
//! (port of `pipeline.py`).

use std::fmt;
use std::io;

use tex2word_backend::document::{DocumentWriter, WriterOptions};
use tex2word_backend::package::DocxPackage;
use tex2word_backend::reference::{extract_reference, ReferenceParts};
use tex2word_backend::{numbering_xml, runninghead, templates};
use tex2word_ir::crossref::resolve_crossrefs;
use tex2word_ir::{pyos, ConversionReport, Document};
use tex2word_ooxml::xml::XmlError;

use crate::roundtrip::build_manifest;

/// A conversion failure (the exceptions the reference lets escape).
#[derive(Debug)]
pub enum Error {
    /// reading an `\input` file, bibliography, image, ...
    Io(io::Error),
    /// a string that isn't XML compatible reached the document
    Xml(XmlError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Xml(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<XmlError> for Error {
    fn from(e: XmlError) -> Self {
        Error::Xml(e)
    }
}

/// `convert_source` keyword arguments.
#[derive(Debug, Clone)]
pub struct ConvertOptions {
    /// persist the IR as a custom part for round-tripping
    pub embed_manifest: bool,
    /// number figures/tables/equations `N.M` per section
    pub number_by_section: bool,
    /// `static` (formatted text) or `zotero` (live CSL_CITATION fields)
    pub citation_mode: String,
    /// page columns; 1 auto-detects, >1 overrides
    pub columns: i64,
    /// `pure` or `latexml` (falls back to pure when unavailable)
    pub frontend: String,
    pub math_image_fallback: bool,
    pub csl: Option<String>,
    /// a Word `.docx` whose styles, theme and page geometry to adopt
    pub reference_doc: Option<String>,
    /// overrides the detected document language
    pub language: Option<String>,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        ConvertOptions {
            embed_manifest: true,
            number_by_section: false,
            citation_mode: "static".into(),
            columns: 1,
            frontend: "pure".into(),
            math_image_fallback: false,
            csl: None,
            reference_doc: None,
            language: None,
        }
    }
}

/// The IR, the report and the `.docx` bytes.
#[derive(Debug, Clone)]
pub struct ConversionResult {
    pub document: Document,
    pub report: ConversionReport,
    pub docx: Vec<u8>,
}

const HF_HEADER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const HF_FOOTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";

type Parts = Vec<(String, Vec<u8>)>;

/// `(sectPr refs, .xml parts, document rels, extra parts)` for headers and
/// footers.
type HeaderFooterWiring = (Vec<(String, String, String)>, Parts, Vec<String>, Parts);

fn put(parts: &mut Parts, key: String, value: Vec<u8>) {
    match parts.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => parts.push((key, value)),
    }
}

/// Convert a LaTeX string to a `.docx` + IR + report.
pub fn convert_source(
    source: &str,
    base_dir: &str,
    opts: &ConvertOptions,
) -> Result<ConversionResult, Error> {
    let (mut doc, mut report) = if opts.frontend == "latexml" {
        tex2word_frontend::latexml::parse_document(source, base_dir)?
    } else {
        tex2word_frontend::parser::parse_document(source, base_dir, opts.csl.as_deref())?
    };
    resolve_crossrefs(&mut doc, &mut report);

    // the preamble (TikZ libraries, colours, macros) for compiling TikZ figures
    let preamble = match tex2word_frontend::preprocess::preprocess(source, base_dir) {
        Ok(pre) => tex2word_frontend::parser::split_document(&pre).1,
        Err(_) => String::new(),
    };

    let mut image_renderer = None;
    if opts.math_image_fallback {
        image_renderer =
            tex2word_math::imagemath::default_renderer(tex2word_math::imagemath::DEFAULT_DPI);
        if image_renderer.is_none() {
            report.warn(
                "math",
                "no math-image backend (install tex2word[mathimg] or TeX)",
            );
        }
    }

    let reference = load_reference(opts.reference_doc.as_deref(), &mut report);
    let (mut hf_refs, mut hf_parts, mut hf_rels, hf_extra) =
        header_footer_wiring(reference.as_ref(), &mut report);
    if hf_refs.is_empty() {
        if let Some(rh) = doc.meta.running_head.clone().filter(|r| !r.is_empty()) {
            // no template headers -> synthesise a running-head header + page footer
            add_running_head(&rh, &mut hf_refs, &mut hf_parts, &mut hf_rels);
        }
    }

    // explicit columns>1 wins; otherwise the detected column count
    let effective_columns = if opts.columns > 1 {
        opts.columns
    } else {
        doc.meta.columns.max(1)
    };

    let (document_xml, rels, media, footnotes, endnotes, comments) = {
        let mut writer = DocumentWriter::new(
            &mut report,
            WriterOptions {
                base_dir: base_dir.to_string(),
                image_math_renderer: image_renderer,
                number_by_section: opts.number_by_section,
                citation_mode: opts.citation_mode.clone(),
                columns: effective_columns,
                page_pgsz: reference.as_ref().and_then(|r| r.page_pgsz.clone()),
                page_pgmar: reference.as_ref().and_then(|r| r.page_pgmar.clone()),
                header_footer_refs: hf_refs,
                preamble,
            },
        );
        let document_xml = writer.build(&doc)?;
        let footnotes = writer.footnotes_xml()?;
        let endnotes = writer.endnotes_xml()?;
        let comments = writer.comments_xml()?;
        (
            document_xml,
            writer.document_rels.clone(),
            writer.media.clone(),
            footnotes,
            endnotes,
            comments,
        )
    };
    if let Some(lang) = &opts.language {
        doc.meta.language = Some(lang.clone()); // CLI/API override
    }
    let mut styles_xml = match &reference {
        Some(r) => r.styles_xml.clone(),
        None => templates::load_styles_xml(),
    };
    // Word reads w:eastAsia for the CJK proofing language
    let lang = doc.meta.language.clone().filter(|l| !l.is_empty());
    let nonempty = |x: &Option<String>| x.as_deref().is_some_and(|s| !s.is_empty());
    let mut east_asia: Option<String> = None;
    if let Some(l) = &lang {
        if matches!(
            l.split('-').next().unwrap_or("").to_lowercase().as_str(),
            "zh" | "ja" | "ko"
        ) {
            east_asia = Some(l.clone());
        }
    }
    if east_asia.is_none()
        && (nonempty(&doc.meta.cjk_main_font)
            || nonempty(&doc.meta.cjk_sans_font)
            || nonempty(&doc.meta.cjk_mono_font))
    {
        east_asia = Some("zh-CN".into());
    }
    if lang.is_some() || east_asia.is_some() {
        styles_xml = templates::apply_language(&styles_xml, lang.as_deref(), east_asia.as_deref());
    }
    if nonempty(&doc.meta.main_font)
        || nonempty(&doc.meta.cjk_main_font)
        || nonempty(&doc.meta.cjk_sans_font)
        || nonempty(&doc.meta.cjk_mono_font)
    {
        styles_xml = templates::apply_fonts(
            &styles_xml,
            doc.meta.main_font.as_deref(),
            doc.meta.cjk_main_font.as_deref(),
            doc.meta.cjk_sans_font.as_deref(),
            doc.meta.cjk_mono_font.as_deref(),
        );
    }
    let mut document_rels = rels;
    document_rels.extend(hf_rels);
    let package = DocxPackage {
        document_xml,
        styles_xml,
        numbering_xml: numbering_xml(),
        document_rels,
        media,
        footnotes,
        endnotes,
        comments,
        manifest: if opts.embed_manifest {
            Some(build_manifest(&doc))
        } else {
            None
        },
        theme: reference.as_ref().and_then(|r| r.theme_xml.clone()),
        header_footer_parts: hf_parts,
        extra_parts: hf_extra,
    };
    let docx = package.to_bytes();
    Ok(ConversionResult {
        document: doc,
        report,
        docx,
    })
}

/// Turn carried headers/footers into (sectPr refs, .xml parts, doc rels,
/// extra parts).
fn header_footer_wiring(
    reference: Option<&ReferenceParts>,
    report: &mut ConversionReport,
) -> HeaderFooterWiring {
    let mut refs = Vec::new();
    let mut parts: Parts = Vec::new();
    let mut rels = Vec::new();
    let mut extra: Parts = Vec::new();
    if let Some(r) = reference {
        for (i, hf) in r.headers_footers.iter().enumerate() {
            let rid = format!("rIdHF{}", i + 1);
            put(
                &mut parts,
                format!("word/{}", hf.part_name),
                hf.content.clone(),
            );
            refs.push((
                format!("{}Reference", hf.kind),
                hf.w_type.clone(),
                rid.clone(),
            ));
            let ty = if hf.kind == "header" {
                HF_HEADER
            } else {
                HF_FOOTER
            };
            rels.push(format!(
                "<Relationship Id=\"{rid}\" Type=\"{ty}\" Target=\"{}\"/>",
                hf.part_name
            ));
            if let Some(hr) = &hf.rels {
                put(
                    &mut extra,
                    format!("word/_rels/{}.rels", hf.part_name),
                    hr.clone(),
                );
            }
            for (k, v) in &hf.media {
                put(&mut extra, k.clone(), v.clone());
            }
        }
        if r.skipped_header_footers > 0 {
            report.info(
                "reference-doc",
                format!(
                    "skipped {} header/footer(s) with unsupported sub-resources",
                    r.skipped_header_footers
                ),
            );
        }
    }
    (refs, parts, rels, extra)
}

/// Synthesise a default running-head header + page-number footer.
fn add_running_head(
    running_head: &str,
    refs: &mut Vec<(String, String, String)>,
    parts: &mut Parts,
    rels: &mut Vec<String>,
) {
    put(
        parts,
        "word/header_rh.xml".into(),
        runninghead::header_xml(running_head),
    );
    put(
        parts,
        "word/footer_rh.xml".into(),
        runninghead::footer_xml(),
    );
    refs.push((
        "headerReference".into(),
        "default".into(),
        "rIdRunHead".into(),
    ));
    refs.push((
        "footerReference".into(),
        "default".into(),
        "rIdRunFoot".into(),
    ));
    rels.push(format!(
        "<Relationship Id=\"rIdRunHead\" Type=\"{HF_HEADER}\" Target=\"header_rh.xml\"/>"
    ));
    rels.push(format!(
        "<Relationship Id=\"rIdRunFoot\" Type=\"{HF_FOOTER}\" Target=\"footer_rh.xml\"/>"
    ));
}

/// Load a `--reference-doc` template, or warn and use the bundled styles.
fn load_reference(
    reference_doc: Option<&str>,
    report: &mut ConversionReport,
) -> Option<ReferenceParts> {
    let path = reference_doc.filter(|p| !p.is_empty())?;
    let exc = match std::fs::read(path) {
        Err(e) => pyos::os_error_str(&e, path),
        Ok(bytes) => match extract_reference(&bytes) {
            Ok(r) => {
                report.info(
                    "reference-doc",
                    format!("using template styles from {path}"),
                );
                return Some(r);
            }
            Err(msg) => msg,
        },
    };
    report.warn(
        "reference-doc",
        format!("ignored ({exc}); used the built-in styles"),
    );
    None
}

/// Convert a `.tex` file to `.docx` on disk; returns the output path.
pub fn convert_file(
    input_path: &str,
    output_path: Option<&str>,
    opts: &ConvertOptions,
) -> Result<(String, ConversionResult), Error> {
    let source = pyos::read_text(input_path, pyos::Errors::Strict)?;
    let base_dir = pyos::dirname(&pyos::abspath(input_path));
    let result = convert_source(&source, &base_dir, opts)?;
    let out = match output_path {
        Some(p) => p.to_string(),
        None => format!("{}.docx", pyos::splitext(input_path).0),
    };
    std::fs::write(&out, &result.docx)?;
    Ok((out, result))
}
