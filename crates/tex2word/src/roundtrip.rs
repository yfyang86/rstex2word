//! Round-trip support: the IR manifest embedded in the `.docx` (port of the
//! write/read side of `roundtrip.py`).

use std::sync::LazyLock;

use tex2word_backend::package::MANIFEST_PART;
use tex2word_frontend::docx_reader::read_docx;
use tex2word_ir::json::{self, DumpOptions, Value};
use tex2word_ir::re::Regex;
use tex2word_ir::{pyos, pystr, Block, Document, FromJson, Inline, Paragraph, ToJson};
use tex2word_ooxml::zip::Archive;

use crate::difflib;

pub const MANIFEST_VERSION: i64 = 1;

/// The manifest `generated` time; honours `SOURCE_DATE_EPOCH`.
fn generated_timestamp() -> String {
    if let Ok(epoch) = std::env::var("SOURCE_DATE_EPOCH") {
        let e = pystr::strip(&epoch);
        if !e.is_empty() && e.chars().all(pystr::is_digit) {
            if let Some(secs) = pystr::parse_int(e) {
                return pyos::utc_isoformat(secs);
            }
        }
    }
    pyos::utc_isoformat(pyos::now_secs())
}

/// Serialise the IR + provenance into the manifest JSON bytes.
pub fn build_manifest(doc: &Document) -> Vec<u8> {
    let labels: Vec<(String, Value)> = doc
        .labels
        .iter()
        .map(|(key, info)| {
            (
                key.clone(),
                Value::Object(vec![
                    ("bookmark".into(), Value::Str(info.bookmark.clone())),
                    ("kind".into(), Value::Str(info.kind.clone())),
                    ("counter".into(), Value::Str(info.counter_name.clone())),
                ]),
            )
        })
        .collect();
    let payload = Value::Object(vec![
        ("tool".into(), Value::Str("tex2word".into())),
        ("manifest_version".into(), Value::Int(MANIFEST_VERSION)),
        ("generated".into(), Value::Str(generated_timestamp())),
        ("labels".into(), Value::Object(labels)),
        ("ir".into(), doc.to_json()),
    ]);
    json::dumps(&payload, DumpOptions { ensure_ascii: false, indent: Some(2), sort_keys: true, compact: false }).into_bytes()
}

/// The parsed manifest payload from a `.docx`, or `None` if absent.
pub fn read_manifest(docx_bytes: &[u8]) -> Result<Option<Value>, String> {
    let zf = Archive::new(docx_bytes)?;
    if !zf.names().contains(&MANIFEST_PART) {
        return Ok(None);
    }
    let bytes = zf.read(MANIFEST_PART)?;
    let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    json::parse(&text).map(Some).map_err(|e| e.to_string())
}

/// Reconstruct the IR document from the manifest.
pub fn recover_ir(docx_bytes: &[u8]) -> Result<Option<Document>, String> {
    let Some(payload) = read_manifest(docx_bytes)? else {
        return Ok(None);
    };
    match payload.get("ir") {
        Some(ir) => Document::from_json(ir).map(Some).map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

/// A failure the reference lets escape from `to_latex`: an unreadable
/// package or manifest, or a writer `KeyError` on the manifest path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundtripError(pub String);

impl std::fmt::Display for RoundtripError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RoundtripError {}

fn write(doc: &Document) -> Result<String, RoundtripError> {
    tex2word_latex::write_latex(doc).map_err(|e| RoundtripError(e.0))
}

/// Convert a `.docx` back to LaTeX.
///
/// Prefers the embedded manifest (the exact IR). With `reconcile` the
/// manifest IR is merged with the freshly read document by
/// [`reconcile_blocks`]: an unedited document reconciles to identity, while
/// prose edited in Word is picked up. A foreign `.docx` (no manifest) is read
/// structurally. `Ok(None)` when the document can't be read at all.
pub fn to_latex(docx_bytes: &[u8], reconcile: bool) -> Result<Option<String>, RoundtripError> {
    let manifest_doc = recover_ir(docx_bytes).map_err(RoundtripError)?;
    let Some(manifest_doc) = manifest_doc else {
        return Ok(read_docx(docx_bytes, None)
            .ok()
            .and_then(|d| tex2word_latex::write_latex(&d).ok()));
    };
    if !reconcile {
        return write(&manifest_doc).map(Some);
    }
    // map sanitized bookmarks back to the original label keys (last wins)
    let mut label_map: Vec<(String, String)> = Vec::new();
    for (key, info) in manifest_doc.labels.iter() {
        match label_map.iter_mut().find(|(b, _)| *b == info.bookmark) {
            Some(slot) => slot.1 = key.clone(),
            None => label_map.push((info.bookmark.clone(), key.clone())),
        }
    }
    let current = match read_docx(docx_bytes, Some(&label_map)) {
        Ok(c) => c,
        Err(_) => return write(&manifest_doc).map(Some),
    };
    let merged = reconcile_blocks(&manifest_doc.blocks, &current.blocks);
    let merged_doc = Document {
        blocks: merged,
        ..manifest_doc
    };
    write(&merged_doc).map(Some)
}

// --------------------------------------------------------------------------- //
// Block-level reconciliation (manifest = exact source, current = Word edits)
// --------------------------------------------------------------------------- //

static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s{}]+"));
static CITE_ARTIFACT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\d[^\]]*\]|\(\s*\)"));

/// Stable prose: `Text` through styling wrappers, excluding math, citations,
/// refs and footnotes (which the reader renders differently).
fn prose_text(inlines: &[Inline]) -> String {
    let mut out = String::new();
    for n in inlines {
        match n {
            Inline::Text(t) => out.push_str(&t.value),
            Inline::Emphasis(e) => out.push_str(&prose_text(&e.inlines)),
            Inline::Link(l) => out.push_str(&prose_text(&l.inlines)),
            Inline::Colored(c) => out.push_str(&prose_text(&c.inlines)),
            Inline::FontSize(f) => out.push_str(&prose_text(&f.inlines)),
            _ => {}
        }
    }
    out
}

fn norm(s: &str) -> String {
    let s = CITE_ARTIFACT.sub("", s);
    pystr::lower(&WS.sub("", &s)).replace(':', "").replace('_', "")
}

const MATH_SYNONYMS: [(&str, &str); 8] = [
    (r"\displaystyle", ""),
    (r"\rightarrow", r"\to"),
    (r"\colon", ":"),
    (r"\begin{matrix}", ""),
    (r"\end{matrix}", ""),
    (r"\left\|", r"\left|"),
    (r"\right\|", r"\right|"),
    (r"\|", "|"),
];

/// A display-math fingerprint stable across the reader's OMML->LaTeX spelling.
fn math_sig(latex: &str) -> String {
    let mut latex = latex.to_string();
    for (a, b) in MATH_SYNONYMS {
        latex = latex.replace(a, b);
    }
    norm(&latex)
}

fn paragraphs_prose<'a>(blocks: impl Iterator<Item = &'a Block>) -> Vec<String> {
    blocks
        .filter_map(|b| match b {
            Block::Paragraph(p) => Some(prose_text(&p.inlines)),
            _ => None,
        })
        .collect()
}

/// A normalized (kind, text) key robust to lossy round-tripping.
fn block_signature(block: &Block) -> (String, String) {
    match block {
        Block::MathBlock(m) => ("math".into(), math_sig(&m.latex)),
        Block::Heading(h) => (format!("h{}", h.level), norm(&prose_text(&h.inlines))),
        Block::Paragraph(p) => ("p".into(), norm(&prose_text(&p.inlines))),
        Block::ItemList(l) => {
            let text = paragraphs_prose(l.items.iter().flat_map(|it| it.blocks.iter())).join(" ");
            ("list".into(), norm(&text))
        }
        Block::Table(t) => {
            let text =
                paragraphs_prose(t.rows.iter().flat_map(|r| r.cells.iter()).flat_map(|c| c.blocks.iter())).join(" ");
            ("table".into(), norm(&text))
        }
        Block::Figure(f) => {
            let cap = norm(&prose_text(f.caption.as_deref().unwrap_or(&[])));
            let img = f.image.as_ref().map(|i| i.path.as_str()).unwrap_or("");
            ("figure".into(), cap + img)
        }
        other => (other.type_name().to_string(), String::new()),
    }
}

fn is_para(b: &Block) -> bool {
    matches!(b, Block::Paragraph(_))
}

/// Merge the exact `original` (manifest) blocks with `current` (edited), a
/// manifest-biased anchored merge over `difflib` opcodes:
///
/// - `equal` -> the original block (with review comments from the matching
///   read-back paragraph grafted on);
/// - `replace` of one paragraph by one paragraph -> the edit (pure prose) or
///   an inline merge (mixed paragraph); any other `replace` keeps the
///   original;
/// - `insert` -> kept only when the whole run is paragraphs;
/// - `delete` -> paragraphs dropped, non-paragraph blocks kept.
pub fn reconcile_blocks(original: &[Block], current: &[Block]) -> Vec<Block> {
    let osig: Vec<_> = original.iter().map(block_signature).collect();
    let csig: Vec<_> = current.iter().map(block_signature).collect();
    let mut out = Vec::new();
    for op in difflib::get_opcodes(&osig, &csig) {
        let (orig_run, cur_run) = (&original[op.i1..op.i2], &current[op.j1..op.j2]);
        match op.tag {
            "equal" => out.extend(orig_run.iter().zip(cur_run).map(|(o, c)| graft_comments(o, c))),
            "delete" => out.extend(orig_run.iter().filter(|b| !is_para(b)).cloned()),
            "insert" => {
                if !cur_run.is_empty() && cur_run.iter().all(is_para) {
                    out.extend(cur_run.iter().cloned());
                }
            }
            _ => {
                if orig_run.len() == 1 && cur_run.len() == 1 && is_para(&orig_run[0]) && is_para(&cur_run[0]) {
                    let pure_prose = match &orig_run[0] {
                        Block::Paragraph(p) => !has_unreliable_inline(&p.inlines),
                        _ => false,
                    };
                    if pure_prose {
                        out.push(cur_run[0].clone());
                    } else {
                        out.push(reconcile_inline(&orig_run[0], &cur_run[0]));
                    }
                } else {
                    out.extend(orig_run.iter().cloned());
                }
            }
        }
    }
    out
}

/// Inline nodes whose rendering injects prose or is unmergeable.
fn is_opaque(n: &Inline) -> bool {
    matches!(n, Inline::Cite(_) | Inline::Ref(_) | Inline::RawInline(_))
}

/// Inline nodes the reader renders differently from the manifest.
fn is_unreliable(n: &Inline) -> bool {
    matches!(
        n,
        Inline::Math(_)
            | Inline::Cite(_)
            | Inline::Ref(_)
            | Inline::Footnote(_)
            | Inline::Endnote(_)
            | Inline::Image(_)
            | Inline::RawInline(_)
            | Inline::IndexEntry(_)
    )
}

fn wrapper_inlines(n: &Inline) -> Option<&[Inline]> {
    match n {
        Inline::Emphasis(e) => Some(&e.inlines),
        Inline::Link(l) => Some(&l.inlines),
        Inline::Colored(c) => Some(&c.inlines),
        Inline::FontSize(f) => Some(&f.inlines),
        _ => None,
    }
}

fn contains(inlines: &[Inline], pred: fn(&Inline) -> bool) -> bool {
    inlines.iter().any(|n| pred(n) || wrapper_inlines(n).is_some_and(|inner| contains(inner, pred)))
}

fn has_unreliable_inline(inlines: &[Inline]) -> bool {
    contains(inlines, is_unreliable)
}

fn is_merge_semantic(n: &Inline) -> bool {
    matches!(n, Inline::Math(_) | Inline::Footnote(_) | Inline::Endnote(_) | Inline::Image(_))
}

/// Prose segments around the top-level semantic nodes.
fn split_semantic(inlines: &[Inline]) -> (Vec<Vec<Inline>>, Vec<Inline>) {
    let mut segments: Vec<Vec<Inline>> = vec![Vec::new()];
    let mut sem = Vec::new();
    for n in inlines {
        if is_merge_semantic(n) {
            sem.push(n.clone());
            segments.push(Vec::new());
        } else {
            segments.last_mut().unwrap().push(n.clone());
        }
    }
    (segments, sem)
}

fn sem_key(n: &Inline) -> (String, String) {
    match n {
        Inline::Math(m) => ("math".into(), math_sig(&m.latex)),
        Inline::Image(i) => ("img".into(), i.path.clone()),
        Inline::Footnote(f) => ("fn".into(), norm(&prose_text(&f.inlines))),
        Inline::Endnote(e) => ("fn".into(), norm(&prose_text(&e.inlines))),
        _ => ("fn".into(), String::new()),
    }
}

/// Merge a prose edit into a mixed paragraph, keeping the manifest's math,
/// footnotes and images; anything risky keeps the manifest paragraph.
fn reconcile_inline(original: &Block, current: &Block) -> Block {
    let (Block::Paragraph(o), Block::Paragraph(c)) = (original, current) else {
        return original.clone();
    };
    if contains(&o.inlines, is_opaque) {
        return original.clone();
    }
    let (o_prose, o_sem) = split_semantic(&o.inlines);
    let (c_prose, c_sem) = split_semantic(&c.inlines);
    if o_sem.len() != c_sem.len() || o_prose.len() != c_prose.len() {
        return original.clone();
    }
    if o_sem.iter().zip(&c_sem).any(|(a, b)| sem_key(a) != sem_key(b)) {
        return original.clone();
    }
    let mut merged = Vec::new();
    for (i, o_seg) in o_prose.iter().enumerate() {
        let c_seg = &c_prose[i];
        if norm(&prose_text(o_seg)) == norm(&prose_text(c_seg)) {
            merged.extend(o_seg.iter().cloned());
        } else {
            merged.extend(c_seg.iter().cloned());
        }
        if i < o_sem.len() {
            merged.push(o_sem[i].clone());
        }
    }
    Block::Paragraph(Paragraph {
        inlines: merged,
        ..o.clone()
    })
}

/// Keep the original block, adding review comments from the matching
/// read-back paragraph that the manifest doesn't already carry.
fn graft_comments(original: &Block, current: &Block) -> Block {
    let (Block::Paragraph(o), Block::Paragraph(c)) = (original, current) else {
        return original.clone();
    };
    let have: Vec<&str> = o
        .inlines
        .iter()
        .filter_map(|n| match n {
            Inline::Comment(cm) => Some(cm.text.as_str()),
            _ => None,
        })
        .collect();
    let notes: Vec<Inline> = c
        .inlines
        .iter()
        .filter(|n| matches!(n, Inline::Comment(cm) if !have.contains(&cm.text.as_str())))
        .cloned()
        .collect();
    if notes.is_empty() {
        return original.clone();
    }
    let mut inlines = o.inlines.clone();
    inlines.extend(notes);
    Block::Paragraph(Paragraph {
        inlines,
        ..o.clone()
    })
}
