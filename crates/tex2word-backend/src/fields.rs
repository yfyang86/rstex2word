//! Word field codes and bookmarks: SEQ / REF / PAGEREF (port of
//! `backend/fields.py`). A complex field is `fldChar begin` -> `instrText` ->
//! `fldChar separate` -> a cached result run -> `fldChar end`.

use std::cell::Cell;

use tex2word_ooxml::xml::{el, preserve_space, qn, sub, text_el, xml_safe, Element};

thread_local! {
    static BOOKMARK_IDS: Cell<u64> = const { Cell::new(1) };
}

/// `w:bookmarkStart` with the next document-wide id.
pub fn bookmark_start(name: &str) -> Element {
    let bid = BOOKMARK_IDS.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    el(
        "w:bookmarkStart",
        &[("w:id", &bid.to_string()), ("w:name", name)],
    )
}

/// The `w:bookmarkEnd` matching a start element.
pub fn bookmark_end_for(start: &Element) -> Element {
    let bid = start.get(&qn("w:id")).unwrap_or("").to_string();
    el("w:bookmarkEnd", &[("w:id", &bid)])
}

/// Reset the bookmark-id counter (per document, for deterministic ids).
pub fn reset_bookmark_ids() {
    BOOKMARK_IDS.with(|c| c.set(1));
}

pub(crate) fn instr_run(code: &str) -> Element {
    let mut r = el("w:r", &[]);
    let instr = sub(&mut r, "w:instrText", &[]);
    preserve_space(instr);
    instr.text = Some(xml_safe(code));
    r
}

pub(crate) fn fldchar(kind: &str) -> Element {
    let mut r = el("w:r", &[]);
    sub(&mut r, "w:fldChar", &[("w:fldCharType", kind)]);
    r
}

/// A complex field as a list of runs; `cached` is the placeholder result
/// shown until Word refreshes fields.
pub fn field(code: &str, cached: &str) -> Vec<Element> {
    let mut runs = vec![fldchar("begin"), instr_run(code), fldchar("separate")];
    let mut result = el("w:r", &[]);
    let mut t = text_el("w:t", if cached.is_empty() { " " } else { cached }, &[]);
    preserve_space(&mut t);
    result.append(t);
    runs.push(result);
    runs.push(fldchar("end"));
    runs
}

/// A hidden `{ XE "term" }` index-entry field (no visible result).
pub fn index_entry(term: &str) -> Vec<Element> {
    vec![
        fldchar("begin"),
        instr_run(&format!("XE \"{term}\"")),
        fldchar("end"),
    ]
}

pub fn seq_field(counter: &str, cached: &str) -> Vec<Element> {
    field(&format!("SEQ {counter} \\* ARABIC"), cached)
}

/// Runs for a live number: flat `SEQ` or per-section `N.M`.
///
/// With `by_section` the number is `STYLEREF 1 \n` (the paragraph number
/// of the nearest Heading 1) + `.` + `SEQ counter \s 1` (a counter that
/// resets at each Heading 1). `restart` adds `\r 1` to the first number
/// of a section: Word resets on `\s` alone, LibreOffice only on `\r`.
pub fn number_field(counter: &str, by_section: bool, restart: bool) -> Vec<Element> {
    if !by_section {
        return seq_field(counter, "1");
    }
    let mut runs = field("STYLEREF 1 \\n", "1");
    let mut dot = el("w:r", &[]);
    dot.append(text_el("w:t", ".", &[]));
    runs.push(dot);
    let reset = if restart { " \\r 1" } else { "" };
    runs.extend(field(
        &format!("SEQ {counter}{reset} \\s 1 \\* ARABIC"),
        "1",
    ));
    runs
}

/// `REF bookmark \h` (`\r \h` inserts the paragraph/list number).
pub fn ref_field(bookmark: &str, cached: &str, paragraph_number: bool) -> Vec<Element> {
    let switches = if paragraph_number { "\\r \\h" } else { "\\h" };
    field(&format!("REF {bookmark} {switches}"), cached)
}

pub fn pageref_field(bookmark: &str, cached: &str) -> Vec<Element> {
    field(&format!("PAGEREF {bookmark} \\h"), cached)
}
