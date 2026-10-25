//! Pragmatic OOXML/OPC validation of a generated `.docx` (port of
//! `validate.py`).
//!
//! Full ECMA-376 XSD validation needs the large transitional schema set; this
//! instead enforces the invariants that catch real bugs:
//!
//! 1. **OPC structure**: the package opens, the required parts exist, every
//!    XML part is well-formed, every part has a declared content type, and
//!    every relationship target resolves to a real part.
//! 2. **Content model**: the child order of the property complex types
//!    (`w:rPr`/`w:pPr`/`w:tblPr`/`w:trPr`/`w:tcPr` follow the schema
//!    sequence, the defect class Word silently repairs), plus a few
//!    enumerated attribute values.
//!
//! Python walks the part names as a `set`, so the order of problems from
//! different parts varies between runs there; here parts are visited in zip
//! order.

use std::fmt;

use tex2word_ir::{pyos, pystr};
use tex2word_ooxml::parse::fromstring;
use tex2word_ooxml::xml::Element;
use tex2word_ooxml::zip::Archive;

const CT_NS: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

const REQUIRED: [&str; 4] = ["[Content_Types].xml", "_rels/.rels", "word/document.xml", "word/styles.xml"];

/// ECMA-376 child order for the property complex types we emit (a subset:
/// children not listed carry no ordering constraint).
const CHILD_ORDER: [(&str, &[&str]); 5] = [
    (
        "rPr",
        &[
            "rStyle", "rFonts", "b", "bCs", "i", "iCs", "caps", "smallCaps", "strike", "dstrike", "outline",
            "shadow", "emboss", "imprint", "noProof", "snapToGrid", "vanish", "webHidden", "color", "spacing",
            "w", "kern", "position", "sz", "szCs", "highlight", "u", "effect", "bdr", "shd", "fitText",
            "vertAlign", "rtl", "cs", "em", "lang", "eastAsianLayout", "specVanish", "oMath",
        ],
    ),
    (
        "pPr",
        &[
            "pStyle", "keepNext", "keepLines", "pageBreakBefore", "framePr", "widowControl", "numPr",
            "suppressLineNumbers", "pBdr", "shd", "tabs", "suppressAutoHyphens", "kinsoku", "wordWrap",
            "overflowPunct", "topLinePunct", "autoSpaceDE", "autoSpaceDN", "bidi", "adjustRightInd",
            "snapToGrid", "spacing", "ind", "contextualSpacing", "mirrorIndents", "suppressOverlap", "jc",
            "textDirection", "textAlignment", "textboxTightWrap", "outlineLvl", "divId", "cnfStyle", "rPr",
            "sectPr", "pPrChange",
        ],
    ),
    (
        "tblPr",
        &[
            "tblStyle", "tblpPr", "tblOverlap", "bidiVisual", "tblStyleRowBandSize", "tblStyleColBandSize",
            "tblW", "jc", "tblCellSpacing", "tblInd", "tblBorders", "shd", "tblLayout", "tblCellMar", "tblLook",
            "tblCaption", "tblDescription", "tblPrChange",
        ],
    ),
    (
        "trPr",
        &[
            "cnfStyle", "divId", "gridBefore", "gridAfter", "wBefore", "wAfter", "cantSplit", "trHeight",
            "tblHeader", "tblCellSpacing", "jc", "hidden", "ins", "del", "trPrChange",
        ],
    ),
    (
        "tcPr",
        &[
            "cnfStyle", "tcW", "gridSpan", "hMerge", "vMerge", "tcBorders", "shd", "noWrap", "tcMar",
            "textDirection", "tcFitText", "vAlign", "hideMark", "cellIns", "cellDel", "cellMerge", "tcPrChange",
        ],
    ),
];

const ST_HIGHLIGHT: [&str; 17] = [
    "black", "blue", "cyan", "darkBlue", "darkCyan", "darkGray", "darkGreen", "darkMagenta", "darkRed",
    "darkYellow", "green", "lightGray", "magenta", "none", "red", "white", "yellow",
];
const ST_VERTALIGN: [&str; 3] = ["baseline", "superscript", "subscript"];

/// An exception the reference lets escape: reading a damaged zip member, or
/// a malformed `[Content_Types].xml` / `.rels` part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidateError {
    /// the Python exception type (`BadZipFile`, `XMLSyntaxError`, ...)
    pub type_name: String,
    pub message: String,
}

impl fmt::Display for ValidateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.type_name, self.message)
    }
}

impl std::error::Error for ValidateError {}

fn read_err(message: String) -> ValidateError {
    let type_name = if message.starts_with("compression type") {
        "NotImplementedError"
    } else if message.starts_with("Error ") {
        "zlib.error"
    } else {
        "BadZipFile"
    };
    ValidateError {
        type_name: type_name.into(),
        message,
    }
}

fn read(zf: &Archive, name: &str) -> Result<Vec<u8>, ValidateError> {
    zf.read(name).map_err(read_err)
}

fn parse_xml(data: &[u8]) -> Result<Element, ValidateError> {
    fromstring(data).map_err(|e| ValidateError {
        type_name: "XMLSyntaxError".into(),
        message: e.0,
    })
}

/// Return the validation problems (empty == valid).
pub fn validate_docx(docx_bytes: &[u8]) -> Result<Vec<String>, ValidateError> {
    let zf = match Archive::new(docx_bytes) {
        Ok(z) => z,
        Err(e) => return Ok(vec![format!("not a valid zip: {e}")]),
    };
    let mut names: Vec<&str> = Vec::new();
    for n in zf.names() {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    let mut problems = Vec::new();
    for required in REQUIRED {
        if !names.contains(&required) {
            problems.push(format!("missing required part: {required}"));
        }
    }
    // every xml/rels part must be well-formed
    for name in &names {
        if name.ends_with(".xml") || name.ends_with(".rels") {
            let root = match fromstring(&read(&zf, name)?) {
                Ok(r) => r,
                Err(e) => {
                    problems.push(format!("malformed XML in {name}: {}", e.0));
                    continue;
                }
            };
            if name.starts_with("word/") && name.ends_with(".xml") {
                check_content_model(name, &root, &mut problems);
            }
        }
    }
    check_content_types(&zf, &names, &mut problems)?;
    check_relationships(&zf, &names, &mut problems)?;
    Ok(problems)
}

/// Schema-aware checks on WordprocessingML property elements.
fn check_content_model(part: &str, root: &Element, problems: &mut Vec<String>) {
    for el in root.iter(None) {
        if !el.is_element() || el.ns.as_deref() != Some(W) {
            continue;
        }
        let local = el.local.as_str();
        if let Some((_, order)) = CHILD_ORDER.iter().find(|(k, _)| *k == local) {
            check_order(part, local, el, order, problems);
        } else if local == "highlight" {
            check_enum(part, el, &ST_HIGHLIGHT, "highlight", problems);
        } else if local == "vertAlign" {
            check_enum(part, el, &ST_VERTALIGN, "vertAlign", problems);
        } else if local == "sz" || local == "szCs" {
            if let Some(val) = el.get(&format!("{{{W}}}val")) {
                if val.is_empty() || !val.chars().all(pystr::is_digit) {
                    problems.push(format!("{part}: w:{local}@w:val must be an integer, got {}", py_repr(val)));
                }
            }
        }
    }
}

fn check_order(part: &str, parent: &str, el: &Element, order: &[&str], problems: &mut Vec<String>) {
    let seen: Vec<(usize, &str)> = el
        .children
        .iter()
        .filter(|c| c.is_element() && c.ns.as_deref() == Some(W))
        .filter_map(|c| order.iter().position(|o| *o == c.local).map(|r| (r, c.local.as_str())))
        .collect();
    for w in seen.windows(2) {
        let ((a_rank, a_name), (b_rank, b_name)) = (w[0], w[1]);
        if b_rank < a_rank {
            problems.push(format!(
                "{part}: w:{parent} children out of ECMA-376 order: w:{b_name} must precede w:{a_name}"
            ));
            return;
        }
    }
}

fn check_enum(part: &str, el: &Element, allowed: &[&str], name: &str, problems: &mut Vec<String>) {
    if let Some(val) = el.get(&format!("{{{W}}}val")) {
        if !allowed.contains(&val) {
            problems.push(format!("{part}: w:{name}@w:val={} is not a valid ST value", py_repr(val)));
        }
    }
}

fn check_content_types(zf: &Archive, names: &[&str], problems: &mut Vec<String>) -> Result<(), ValidateError> {
    if !names.contains(&"[Content_Types].xml") {
        return Ok(());
    }
    let root = parse_xml(&read(zf, "[Content_Types].xml")?)?;
    let tag = |local: &str| format!("{{{CT_NS}}}{local}");
    let defaults: Vec<String> = root
        .findall(&tag("Default"))
        .into_iter()
        .map(|e| pystr::lower(e.get("Extension").unwrap_or("")))
        .collect();
    let overrides: Vec<Option<&str>> = root.findall(&tag("Override")).into_iter().map(|e| e.get("PartName")).collect();
    for name in names {
        if *name == "[Content_Types].xml" || name.ends_with('/') {
            continue;
        }
        let ext = match name.rsplit_once('.') {
            Some((_, e)) => pystr::lower(e),
            None => String::new(),
        };
        let partname = format!("/{name}");
        if !defaults.contains(&ext) && !overrides.contains(&Some(partname.as_str())) {
            problems.push(format!("no declared content type for part: {name}"));
        }
    }
    Ok(())
}

fn check_relationships(zf: &Archive, names: &[&str], problems: &mut Vec<String>) -> Result<(), ValidateError> {
    for name in names {
        if !name.ends_with(".rels") {
            continue;
        }
        let base = pyos::dirname(&pyos::dirname(name)); // strip "_rels/x.rels"
        let root = parse_xml(&read(zf, name)?)?;
        for rel in root.findall(&format!("{{{REL_NS}}}Relationship")) {
            let mode = rel.get("TargetMode").filter(|m| !m.is_empty()).unwrap_or("Internal");
            if mode == "External" {
                continue;
            }
            let target = rel.get("Target").unwrap_or("");
            let resolved = pyos::normpath(&pyos::join2(&base, target));
            let resolved = resolved.trim_start_matches('/');
            if !names.contains(&resolved) {
                problems.push(format!("{name}: relationship target not found: {target}"));
            }
        }
    }
    Ok(())
}

/// Python `repr()` of a string.
fn py_repr(s: &str) -> String {
    tex2word_ir::json::py_str_repr(s)
}
