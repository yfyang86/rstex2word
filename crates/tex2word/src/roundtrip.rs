//! Round-trip support: the IR manifest embedded in the `.docx` (port of the
//! write/read side of `roundtrip.py`).

use tex2word_backend::package::MANIFEST_PART;
use tex2word_ir::json::{self, DumpOptions, Value};
use tex2word_ir::{pyos, pystr, Document, FromJson, ToJson};
use tex2word_ooxml::zip::Archive;

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
