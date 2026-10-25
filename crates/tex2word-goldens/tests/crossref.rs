//! `resolve_crossrefs` reproduces the Python reference on every recorded call.

use tex2word_goldens::{json_diff, load_all};
use tex2word_ir::crossref::resolve_crossrefs;
use tex2word_ir::{ConversionReport, Document, FromJson, ToJson};

#[test]
fn resolve_crossrefs_matches_python() {
    let mut n = 0;
    for case in load_all()
        .into_iter()
        .filter(|c| c.api == "resolve_crossrefs")
    {
        n += 1;
        let input = case.arg("doc").and_then(|v| v.get("@ir")).expect("doc arg");
        let mut doc = Document::from_json(input).expect("input IR");
        let mut report = ConversionReport::new();
        resolve_crossrefs(&mut doc, &mut report);
        let want = case.out.get("ir").expect("recorded IR");
        if let Some(d) = json_diff(want, &doc.to_json(), "") {
            panic!("{} ({:?}): {d}", case.id, case.tests());
        }
    }
    assert!(
        n >= 5,
        "expected recorded resolve_crossrefs cases, found {n}"
    );
}
