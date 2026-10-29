//! Helpers shared by the acceptance tests (`tikz_paper`, `amsart_paper`).
#![allow(dead_code)] // each test crate uses its own subset

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use tex2word::{convert_source, ConversionResult, ConvertOptions};
use tex2word_ooxml::zip::Archive;

/// `tests/fixtures/<name>`.
pub fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// True when this machine can compile and rasterise TikZ.
pub fn tikz_tools() -> bool {
    let ok = !tex2word_backend::tikz::available_engines().is_empty()
        && tex2word_backend::raster::has_pdf_support();
    if !ok && std::env::var_os("T2W_REQUIRE_TIKZ").is_some() {
        panic!("T2W_REQUIRE_TIKZ is set but no TeX engine + pdftoppm was found");
    }
    ok
}

/// Every file under `dir` (relative paths), to prove conversion leaves the
/// source tree untouched.
pub fn listing(dir: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p.strip_prefix(dir).unwrap().to_path_buf());
            }
        }
    }
    out
}

pub fn convert_dir(dir: &Path) -> ConversionResult {
    let src = std::fs::read_to_string(dir.join("main.tex")).unwrap();
    let before = listing(dir);
    let r = convert_source(&src, dir.to_str().unwrap(), &ConvertOptions::default()).unwrap();
    assert_eq!(
        listing(dir),
        before,
        "conversion wrote into the source tree"
    );
    assert_eq!(
        tex2word_validate::validate_docx(&r.docx).unwrap(),
        Vec::<String>::new()
    );
    r
}

/// `(media part names, drawing widths in EMU, document.xml)`.
pub fn package_parts(docx: &[u8]) -> (Vec<String>, Vec<i64>, String) {
    let zip = Archive::new(docx).unwrap();
    let media = zip
        .names()
        .into_iter()
        .filter(|n| n.starts_with("word/media/"))
        .map(str::to_string)
        .collect();
    let xml = String::from_utf8(zip.read("word/document.xml").unwrap()).unwrap();
    let widths = xml
        .match_indices("<wp:extent cx=\"")
        .map(|(i, m)| {
            let rest = &xml[i + m.len()..];
            rest[..rest.find('"').unwrap()].parse().unwrap()
        })
        .collect();
    (media, widths, xml)
}

pub fn warnings(r: &ConversionResult) -> Vec<String> {
    r.report
        .warnings()
        .iter()
        .map(|e| format!("{}: {}", e.construct, e.message))
        .collect()
}

/// The document's visible text: every `w:t`, joined by spaces.
pub fn body_text(xml: &str) -> String {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<w:t") {
        let after = &rest[i + 4..];
        // `<w:t>` or `<w:t xml:space=...>`, not `<w:tab/>`, `<w:tbl>`, ...
        if !(after.starts_with('>') || after.starts_with(' ')) {
            rest = after;
            continue;
        }
        let open = after.find('>').unwrap();
        let close = after.find("</w:t>").unwrap();
        out.push(&after[open + 1..close]);
        rest = &after[close..];
    }
    out.join(" ")
}

/// The IR as JSON, for substring checks.
pub fn ir_json(r: &ConversionResult) -> String {
    use tex2word_ir::ToJson;
    tex2word_ir::json::dumps(&r.document.to_json(), Default::default())
}
