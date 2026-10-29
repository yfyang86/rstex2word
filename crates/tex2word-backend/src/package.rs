//! OPC package assembly (port of `backend/package.py`): a deterministic ZIP
//! of XML parts joined by relationship files (fixed timestamps, sorted
//! members).

use crate::data::*;

pub use crate::data::{MANIFEST_PART, MANIFEST_REL_TYPE};

/// Accumulates parts and writes the `.docx`.
#[derive(Debug, Clone, Default)]
pub struct DocxPackage {
    pub document_xml: Vec<u8>,
    pub styles_xml: Vec<u8>,
    pub numbering_xml: Vec<u8>,
    /// extra `word/_rels/document.xml.rels` relationship lines (raw XML)
    pub document_rels: Vec<String>,
    /// media parts: archive path -> bytes, in insertion order
    pub media: Vec<(String, Vec<u8>)>,
    /// round-trip IR manifest (JSON bytes), or `None`
    pub manifest: Option<Vec<u8>>,
    pub footnotes: Option<Vec<u8>>,
    pub endnotes: Option<Vec<u8>>,
    pub comments: Option<Vec<u8>>,
    /// `word/theme/theme1.xml` from a `--reference-doc`
    pub theme: Option<Vec<u8>>,
    /// carried header/footer parts (archive path -> bytes)
    pub header_footer_parts: Vec<(String, Vec<u8>)>,
    /// carried header/footer sub-resources (their _rels + media)
    pub extra_parts: Vec<(String, Vec<u8>)>,
}

/// Insert into an ordered map (`dict[key] = value`).
fn put(parts: &mut Vec<(String, Vec<u8>)>, key: &str, value: Vec<u8>) {
    match parts.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => parts.push((key.to_string(), value)),
    }
}

impl DocxPackage {
    fn content_types(&self) -> String {
        let mut body = CONTENT_TYPES_HEAD.to_string();
        if self.theme.is_some() {
            body.push_str(THEME_OVERRIDE);
        }
        let mut hf: Vec<&String> = self.header_footer_parts.iter().map(|(k, _)| k).collect();
        hf.sort();
        hf.dedup();
        for arc in hf {
            let base = arc.rsplit('/').next().unwrap_or(arc);
            let kind = if base.starts_with("header") {
                "header"
            } else {
                "footer"
            };
            body.push_str(&format!(
                "  <Override PartName=\"/{arc}\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.{kind}+xml\"/>\n"
            ));
        }
        if self.footnotes.is_some() {
            body.push_str(FOOTNOTES_OVERRIDE);
        }
        if self.endnotes.is_some() {
            body.push_str(ENDNOTES_OVERRIDE);
        }
        if self.comments.is_some() {
            body.push_str(COMMENTS_OVERRIDE);
        }
        if self.manifest.is_some() {
            body.push_str(MANIFEST_OVERRIDE);
        }
        body + CONTENT_TYPES_TAIL
    }

    fn document_rels_xml(&self) -> String {
        let mut rels: Vec<String> = self.document_rels.clone();
        if self.theme.is_some() {
            rels.push("<Relationship Id=\"rIdTheme\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme\" Target=\"theme/theme1.xml\"/>".into());
        }
        if self.footnotes.is_some() {
            rels.push("<Relationship Id=\"rIdFootnotes\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes\" Target=\"footnotes.xml\"/>".into());
        }
        if self.endnotes.is_some() {
            rels.push("<Relationship Id=\"rIdEndnotes\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/endnotes\" Target=\"endnotes.xml\"/>".into());
        }
        if self.comments.is_some() {
            rels.push("<Relationship Id=\"rIdComments\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments\" Target=\"comments.xml\"/>".into());
        }
        if self.manifest.is_some() {
            rels.push(format!(
                "<Relationship Id=\"rIdManifest\" Type=\"{MANIFEST_REL_TYPE}\" Target=\"tex2word/manifest.json\"/>"
            ));
        }
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
             <Relationship Id=\"rIdStyles\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
             <Relationship Id=\"rIdNumbering\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>\
             <Relationship Id=\"rIdSettings\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/settings\" Target=\"settings.xml\"/>\
             {}</Relationships>",
            rels.concat()
        )
    }

    /// Every part (archive path -> bytes), in the reference's dict order.
    pub fn parts(&self) -> Vec<(String, Vec<u8>)> {
        let mut parts: Vec<(String, Vec<u8>)> = vec![
            (
                "[Content_Types].xml".into(),
                self.content_types().into_bytes(),
            ),
            ("_rels/.rels".into(), ROOT_RELS.as_bytes().to_vec()),
            ("docProps/core.xml".into(), CORE_PROPS.as_bytes().to_vec()),
            ("word/document.xml".into(), self.document_xml.clone()),
            ("word/styles.xml".into(), self.styles_xml.clone()),
            ("word/numbering.xml".into(), self.numbering_xml.clone()),
            ("word/settings.xml".into(), SETTINGS.as_bytes().to_vec()),
            (
                "word/_rels/document.xml.rels".into(),
                self.document_rels_xml().into_bytes(),
            ),
        ];
        for (k, v) in self
            .media
            .iter()
            .chain(&self.header_footer_parts)
            .chain(&self.extra_parts)
        {
            put(&mut parts, k, v.clone());
        }
        if let Some(t) = &self.theme {
            put(&mut parts, "word/theme/theme1.xml", t.clone());
        }
        if let Some(f) = &self.footnotes {
            put(&mut parts, "word/footnotes.xml", f.clone());
        }
        if let Some(e) = &self.endnotes {
            put(&mut parts, "word/endnotes.xml", e.clone());
        }
        if let Some(c) = &self.comments {
            put(&mut parts, "word/comments.xml", c.clone());
        }
        if let Some(m) = &self.manifest {
            put(&mut parts, MANIFEST_PART, m.clone());
        }
        parts
    }

    /// The `.docx` bytes (members sorted by name).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut parts = self.parts();
        parts.sort_by(|a, b| a.0.cmp(&b.0));
        tex2word_ooxml::zip::write(&parts)
    }

    /// Write the `.docx` to `path`.
    pub fn write(&self, path: &str) -> std::io::Result<()> {
        std::fs::write(path, self.to_bytes())
    }
}

/// `numbering_xml(restarts)`: `word/numbering.xml`; `restarts` adds one
/// `w:num` per ordered list as `(numId, ilvl, start)`, so each list restarts
/// its numbering (lists sharing one instance continue across the document in
/// Word). An empty slice is Python's `None`.
pub fn numbering_xml(restarts: &[(i64, i64, i64)]) -> Vec<u8> {
    let mut xml = NUMBERING_XML.to_string();
    if !restarts.is_empty() {
        let mut extra = String::new();
        for (num_id, ilvl, start) in restarts {
            extra.push_str(&format!(
                "  <w:num w:numId=\"{num_id}\"><w:abstractNumId w:val=\"{DECIMAL_ABSTRACT_ID}\"/>\
                 <w:lvlOverride w:ilvl=\"{ilvl}\"><w:startOverride w:val=\"{start}\"/>\
                 </w:lvlOverride></w:num>\n"
            ));
        }
        xml = xml.replace("</w:numbering>", &(extra + "</w:numbering>"));
    }
    xml.into_bytes()
}

/// numIds of the numbering scheme.
pub const BULLET_NUM_ID: i64 = 1;
pub const DECIMAL_NUM_ID: i64 = 2;
pub const HEADING_NUM_ID: i64 = 3;
pub const HEADING_APPENDIX_NUM_ID: i64 = 4;
pub const PART_NUM_ID: i64 = 5;
/// abstract list definition of ordered (enumerate) lists
pub const DECIMAL_ABSTRACT_ID: i64 = 1;
/// first numId handed out for restartable ordered-list instances
pub const FIRST_LIST_NUM_ID: i64 = 100;
