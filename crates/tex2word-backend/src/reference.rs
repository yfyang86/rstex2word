//! Reference-document ("template") support for `--reference-doc` (port of
//! `templates/reference.py`): lift a Word template's styles (merged with the
//! custom styles the writer needs), theme, page geometry and rels-safe
//! headers/footers.

use std::collections::HashSet;

use tex2word_ir::pyos;
use tex2word_ooxml::parse::fromstring;
use tex2word_ooxml::xml::{serialize, Element};
use tex2word_ooxml::zip::Archive;

use crate::templates::load_styles_xml;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

fn w(name: &str) -> String {
    format!("{{{W}}}{name}")
}

/// A carried header/footer part from the reference template.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderFooter {
    /// "header" or "footer"
    pub kind: String,
    /// "default" / "first" / "even"
    pub w_type: String,
    /// archive basename, e.g. "header1.xml"
    pub part_name: String,
    pub content: Vec<u8>,
    /// rewritten `word/_rels/<part>.rels`, if it has any
    pub rels: Option<Vec<u8>>,
    /// arcname -> bytes
    pub media: Vec<(String, Vec<u8>)>,
}

/// Styling parts lifted from a reference `.docx`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReferenceParts {
    pub styles_xml: Vec<u8>,
    pub theme_xml: Option<Vec<u8>>,
    pub page_pgsz: Option<Vec<(String, String)>>,
    pub page_pgmar: Option<Vec<(String, String)>>,
    pub headers_footers: Vec<HeaderFooter>,
    pub skipped_header_footers: usize,
}

/// Lift the styling parts from a reference `.docx`. `Err` (Python's
/// `ValueError`) if it isn't a readable docx with a `styles.xml`.
pub fn extract_reference(docx_bytes: &[u8]) -> Result<ReferenceParts, String> {
    let inner = || -> Result<ReferenceParts, (bool, String)> {
        let zf = Archive::new(docx_bytes).map_err(|e| (false, e))?;
        let names: HashSet<String> = zf.names().into_iter().map(str::to_string).collect();
        if !names.contains("word/styles.xml") {
            return Err((true, "reference docx has no word/styles.xml".into()));
        }
        let styles = merge_styles(
            &zf.read("word/styles.xml").map_err(|e| (false, e))?,
            &load_styles_xml(),
        )
        .map_err(|e| (false, e))?;
        let mut sorted: Vec<&String> = names.iter().collect();
        sorted.sort();
        let theme = match sorted
            .iter()
            .find(|n| n.starts_with("word/theme/") && n.ends_with(".xml"))
        {
            Some(n) => Some(zf.read(n).map_err(|e| (false, e))?),
            None => None,
        };
        let (mut pgsz, mut pgmar) = (None, None);
        let mut hfs = Vec::new();
        let mut skipped = 0;
        if names.contains("word/document.xml") {
            let doc_xml = zf.read("word/document.xml").map_err(|e| (false, e))?;
            let g = page_geometry(&doc_xml).map_err(|e| (false, e))?;
            pgsz = g.0;
            pgmar = g.1;
            let h = headers_footers(&doc_xml, &zf, &names).map_err(|e| (false, e))?;
            hfs = h.0;
            skipped = h.1;
        }
        Ok(ReferenceParts {
            styles_xml: styles,
            theme_xml: theme,
            page_pgsz: pgsz,
            page_pgmar: pgmar,
            headers_footers: hfs,
            skipped_header_footers: skipped,
        })
    };
    match inner() {
        Ok(r) => Ok(r),
        Err((true, msg)) => Err(msg),
        // corrupt zip / malformed XML
        Err((false, msg)) => Err(format!("unreadable reference docx: {msg}")),
    }
}

/// Reference styles, augmented with any of *our* styles it doesn't define
/// (the reference's definitions of standard ids win).
pub fn merge_styles(reference_styles: &[u8], our_styles: &[u8]) -> Result<Vec<u8>, String> {
    let mut ref_root = fromstring(reference_styles).map_err(|e| e.0)?;
    let style = w("style");
    let sid_key = w("styleId");
    let mut have: HashSet<String> = ref_root
        .findall(&style)
        .iter()
        .filter_map(|s| {
            s.get(&sid_key)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        })
        .collect();
    let our_root = fromstring(our_styles).map_err(|e| e.0)?;
    for s in our_root.findall(&style) {
        if let Some(sid) = s.get(&sid_key).filter(|v| !v.is_empty()) {
            if !have.contains(sid) {
                // lxml's append moves the element with its tail text
                ref_root.append(s.clone());
                have.insert(sid.to_string());
            }
        }
    }
    serialize(&ref_root).map_err(|e| e.0)
}

fn body_sect(root: &Element) -> Option<&Element> {
    let body = root.find(&w("body"))?;
    body.children.iter().rev().find(|c| c.is(&w("sectPr")))
}

/// `{relationship id -> target}` from `word/_rels/document.xml.rels`.
fn rel_targets(zf: &Archive, names: &HashSet<String>) -> Result<Vec<(String, String)>, String> {
    let path = "word/_rels/document.xml.rels";
    if !names.contains(path) {
        return Ok(Vec::new());
    }
    let root = fromstring(&zf.read(path)?).map_err(|e| e.0)?;
    let mut out: Vec<(String, String)> = Vec::new();
    for r in root.findall(&format!("{{{PKG_REL}}}Relationship")) {
        if let (Some(rid), Some(target)) = (
            r.get("Id").filter(|v| !v.is_empty()),
            r.get("Target").filter(|v| !v.is_empty()),
        ) {
            match out.iter_mut().find(|(k, _)| k == rid) {
                Some(slot) => slot.1 = target.to_string(),
                None => out.push((rid.to_string(), target.to_string())),
            }
        }
    }
    Ok(out)
}

fn headers_footers(
    document_xml: &[u8],
    zf: &Archive,
    names: &HashSet<String>,
) -> Result<(Vec<HeaderFooter>, usize), String> {
    let root = fromstring(document_xml).map_err(|e| e.0)?;
    let Some(sect) = body_sect(&root) else {
        return Ok((Vec::new(), 0));
    };
    let rid_target = rel_targets(zf, names)?;
    let rid_attr = format!("{{{R}}}id");
    let mut carried = Vec::new();
    let mut skipped = 0;
    for r in &sect.children {
        let kind = if r.is(&w("headerReference")) {
            "header"
        } else if r.is(&w("footerReference")) {
            "footer"
        } else {
            continue;
        };
        let rid = r.get(&rid_attr).unwrap_or("");
        let Some(target) = rid_target
            .iter()
            .find(|(k, _)| k == rid)
            .map(|(_, t)| t.clone())
            .filter(|t| !t.is_empty())
        else {
            continue;
        };
        let base = target.rsplit('/').next().unwrap_or(&target).to_string();
        let part = format!("word/{base}");
        if !names.contains(&part) {
            skipped += 1;
            continue;
        }
        let Some((rels_bytes, media)) = carry_subresources(zf, names, &base)? else {
            skipped += 1; // an unsupported sub-resource: skip, don't dangle a rel
            continue;
        };
        carried.push(HeaderFooter {
            kind: kind.into(),
            w_type: r
                .get(&w("type"))
                .filter(|t| !t.is_empty())
                .unwrap_or("default")
                .into(),
            part_name: base,
            content: zf.read(&part)?,
            rels: if rels_bytes.is_empty() {
                None
            } else {
                Some(rels_bytes)
            },
            media,
        });
    }
    Ok((carried, skipped))
}

/// Rewrite a header/footer's `.rels` and collect its image media; `None`
/// when a sub-resource can't be carried safely.
#[allow(clippy::type_complexity)]
fn carry_subresources(
    zf: &Archive,
    names: &HashSet<String>,
    base: &str,
) -> Result<Option<(Vec<u8>, Vec<(String, Vec<u8>)>)>, String> {
    let rels_path = format!("word/_rels/{base}.rels");
    if !names.contains(&rels_path) {
        return Ok(Some((Vec::new(), Vec::new())));
    }
    let Ok(mut root) = zf
        .read(&rels_path)
        .and_then(|b| fromstring(&b).map_err(|e| e.0))
    else {
        return Ok(None);
    };
    let stem = base.rsplit_once('.').map_or(base, |(s, _)| s).to_string();
    let mut media: Vec<(String, Vec<u8>)> = Vec::new();
    let rel_tag = format!("{{{PKG_REL}}}Relationship");
    for rel in root.children.iter_mut().filter(|c| c.is(&rel_tag)) {
        if rel
            .get("TargetMode")
            .filter(|v| !v.is_empty())
            .unwrap_or("Internal")
            == "External"
        {
            continue; // a URL: keep the relationship as-is
        }
        let rtype = rel.get("Type").unwrap_or("").to_string();
        let target = rel.get("Target").unwrap_or("").to_string();
        let ext = match target.rsplit_once('.') {
            Some((_, e)) => e.to_lowercase(),
            None => String::new(),
        };
        if !rtype.ends_with("/image")
            || !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "emf")
            || target.contains("..")
        {
            return Ok(None);
        }
        let src = pyos::normpath(&format!("word/{}", target.trim_start_matches('/')));
        if !names.contains(&src) {
            return Ok(None);
        }
        let new_base = format!("{stem}_{}", target.rsplit('/').next().unwrap_or(&target));
        media.push((format!("word/media/tmpl/{new_base}"), zf.read(&src)?));
        rel.set("Target", format!("media/tmpl/{new_base}"));
    }
    let rels_bytes = serialize(&root).map_err(|e| e.0)?;
    Ok(Some((rels_bytes, media)))
}

#[allow(clippy::type_complexity)]
fn page_geometry(
    document_xml: &[u8],
) -> Result<(Option<Vec<(String, String)>>, Option<Vec<(String, String)>>), String> {
    let root = fromstring(document_xml).map_err(|e| e.0)?;
    let Some(sect) = body_sect(&root) else {
        return Ok((None, None));
    };
    let attrs = |e: Option<&Element>| -> Option<Vec<(String, String)>> {
        e.map(|e| {
            e.attrs
                .iter()
                .map(|a| (format!("w:{}", a.local), a.value.clone()))
                .collect()
        })
    };
    Ok((attrs(sect.find(&w("pgSz"))), attrs(sect.find(&w("pgMar")))))
}
