//! Compute the cached results of the fields tex2word writes (port of
//! `backend/field_results.py`).
//!
//! Numbers in the document are live Word fields (`SEQ` for figures, tables,
//! equations and theorems; `STYLEREF` for the section part of "2.1"; `REF`
//! for cross-references). Word and LibreOffice recompute them, but Protected
//! View, previews, Google Docs import and text extractors show the cached
//! result, which used to be a placeholder ("1" for every number, "0" for every
//! reference).
//!
//! This pass walks the finished XML in reading order, the way Word's field
//! update does:
//!
//! 1. it emulates list numbering from `numbering.xml` (headings and ordered
//!    lists), so `STYLEREF` and `REF \r` know the paragraph numbers;
//! 2. it counts `SEQ` fields (`\s N` resets at each heading of level <= N);
//! 3. it records the text inside each bookmark (and its paragraph number);
//! 4. it fills each `REF` result from the bookmark, in the body and in notes.
//!
//! `PAGEREF` results are left alone (page numbers need layout).
//!
//! lxml lets the Python code keep references to the result `w:t` elements
//! while it walks the tree; here an element is addressed by its path (child
//! indices from the walked root) instead. The pass only rewrites `w:t` text,
//! so the paths stay valid.

use std::collections::HashMap;
use std::sync::LazyLock;

use tex2word_ir::pystr;
use tex2word_ir::re::Regex;
use tex2word_ir::Dict;
use tex2word_ooxml::parse::fromstring;
use tex2word_ooxml::xml::{Element, XmlError};

/// `_qn(tag)`: a `w:` tag in Clark notation.
macro_rules! w {
    ($tag:literal) => {
        concat!(
            "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}",
            $tag
        )
    };
}

static HEADING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Heading([1-9])$"));
static LEVEL_REF_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"%([1-9])"));

/// Child indices from a walked root to one of its descendants.
type Path = Vec<usize>;

/// `root.iter(*tags)` as paths: `root` and its descendants in document order.
fn iter_paths(root: &Element, tags: &[&str]) -> Vec<Path> {
    fn go(e: &Element, tags: &[&str], path: &mut Path, out: &mut Vec<Path>) {
        if e.is_element() && tags.iter().any(|t| e.is(t)) {
            out.push(path.clone());
        }
        for (i, c) in e.children.iter().enumerate() {
            path.push(i);
            go(c, tags, path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    go(root, tags, &mut Vec::new(), &mut out);
    out
}

fn node<'a>(root: &'a Element, path: &[usize]) -> &'a Element {
    path.iter().fold(root, |e, &i| &e.children[i])
}

fn node_mut<'a>(root: &'a mut Element, path: &[usize]) -> &'a mut Element {
    path.iter().fold(root, |e, &i| &mut e.children[i])
}

/// `int(s)` (lxml attribute values and field-code arguments).
fn py_int(s: &str) -> i64 {
    pystr::parse_int(s).unwrap_or_else(|| panic!("invalid literal for int() with base 10: {s:?}"))
}

/// `s.isdigit()`.
fn is_digit_str(s: &str) -> bool {
    !s.is_empty() && s.chars().all(pystr::is_digit)
}

// --------------------------------------------------------------------------- //
// list numbering
// --------------------------------------------------------------------------- //

/// One `w:lvl`: (start, numFmt, lvlText).
type Level = (i64, String, String);

/// `counts[i]` with Python's negative indexing on the nine-slot list.
fn slot(i: i64) -> usize {
    let j = if i < 0 { i + 9 } else { i };
    assert!((0..9).contains(&j), "list index out of range");
    j as usize
}

/// Paragraph numbers from `numbering.xml` (decimal/letter/roman formats,
/// `%n` level text, per-instance start overrides, deeper levels restarting).
struct Numbering {
    levels: HashMap<String, HashMap<i64, Level>>,
    /// numId -> (abstractNumId, {ilvl: start override})
    nums: HashMap<String, (String, HashMap<i64, i64>)>,
    /// running counters per numId (Word counts each w:num instance)
    counters: HashMap<String, [Option<i64>; 9]>,
}

impl Numbering {
    fn new(numbering_xml: &[u8]) -> Result<Numbering, XmlError> {
        let root = fromstring(numbering_xml)?;
        let mut levels = HashMap::new();
        for an in root.iter(Some(w!("abstractNum"))) {
            let mut lv: HashMap<i64, Level> = HashMap::new();
            for lvl in an.iter(Some(w!("lvl"))) {
                let ilvl = py_int(lvl.get(w!("ilvl")).unwrap_or("0"));
                let start = lvl.find(w!("start"));
                let fmt = lvl.find(w!("numFmt"));
                let text = lvl.find(w!("lvlText"));
                lv.insert(
                    ilvl,
                    (
                        start.map_or(1, |s| py_int(s.get(w!("val")).unwrap_or("1"))),
                        fmt.map_or("decimal", |f| f.get(w!("val")).unwrap_or("decimal"))
                            .to_string(),
                        text.map_or("", |t| t.get(w!("val")).unwrap_or(""))
                            .to_string(),
                    ),
                );
            }
            levels.insert(an.get(w!("abstractNumId")).unwrap_or("").to_string(), lv);
        }
        let mut nums = HashMap::new();
        for num in root.iter(Some(w!("num"))) {
            let aid = num.find(w!("abstractNumId"));
            let mut overrides = HashMap::new();
            for ov in num.iter(Some(w!("lvlOverride"))) {
                if let Some(so) = ov.find(w!("startOverride")) {
                    overrides.insert(
                        py_int(ov.get(w!("ilvl")).unwrap_or("0")),
                        py_int(so.get(w!("val")).unwrap_or("1")),
                    );
                }
            }
            if let Some(aid) = aid {
                nums.insert(
                    num.get(w!("numId")).unwrap_or("").to_string(),
                    (aid.get(w!("val")).unwrap_or("").to_string(), overrides),
                );
            }
        }
        Ok(Numbering {
            levels,
            nums,
            counters: HashMap::new(),
        })
    }

    /// Number the next paragraph of `num_id` at `ilvl`; returns
    /// `(full text, this level's own number)`, e.g. `("2.1", "1")`.
    fn advance(&mut self, num_id: &str, ilvl: i64) -> Option<(String, String)> {
        let (aid, overrides) = self.nums.get(num_id)?;
        let no_levels = HashMap::new();
        let levels = self.levels.get(aid).unwrap_or(&no_levels);
        let start_of = |k: i64| levels.get(&k).map_or(1, |l| l.0);
        let counts = self.counters.entry(num_id.to_string()).or_insert([None; 9]);
        let start = overrides
            .get(&ilvl)
            .copied()
            .unwrap_or_else(|| start_of(ilvl));
        let i = slot(ilvl);
        counts[i] = Some(match counts[i] {
            None => start,
            Some(c) => c + 1,
        });
        for deeper in ilvl + 1..9 {
            counts[slot(deeper)] = None;
        }
        for upper in 0..ilvl {
            // an unnumbered upper level counts as its start
            if counts[slot(upper)].is_none() {
                counts[slot(upper)] = Some(start_of(upper));
            }
        }
        let (fmt, text) = match levels.get(&ilvl) {
            Some((_, f, t)) => (f.clone(), t.clone()),
            None => ("decimal".to_string(), format!("%{}", ilvl + 1)),
        };
        if fmt == "bullet" || fmt == "none" {
            return None;
        }
        let counts = *counts;
        let full = LEVEL_REF_RE.sub_fn(&text, |m| {
            let k = py_int(m.group_str(1)) - 1;
            let f = levels.get(&k).map_or("decimal", |l| l.1.as_str());
            format_number(counts[slot(k)].unwrap_or(0), f)
        });
        Some((full, format_number(counts[slot(ilvl)].unwrap_or(0), &fmt)))
    }
}

/// `_format(n, fmt)`.
fn format_number(n: i64, fmt: &str) -> String {
    match fmt {
        "lowerLetter" => letters(n).to_lowercase(),
        "upperLetter" => letters(n),
        "lowerRoman" => roman(n).to_lowercase(),
        "upperRoman" => roman(n),
        _ => n.to_string(),
    }
}

fn letters(mut n: i64) -> String {
    let mut out = String::new();
    while n > 0 {
        let r = (n - 1).rem_euclid(26);
        n = (n - 1).div_euclid(26);
        out.insert(0, (b'A' + r as u8) as char);
    }
    if out.is_empty() {
        "A".to_string()
    } else {
        out
    }
}

fn roman(mut n: i64) -> String {
    const VALS: [(i64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (v, s) in VALS {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    if out.is_empty() {
        "0".to_string()
    } else {
        out
    }
}

// --------------------------------------------------------------------------- //
// fields
// --------------------------------------------------------------------------- //

struct Field {
    /// path of the first result `w:t` (from the walked root)
    result_t: Option<Path>,
    kind: String,
    args: Vec<String>,
}

impl Field {
    fn new(code: &str, result_t: Option<Path>) -> Field {
        let parts = pystr::split_ws(pystr::strip(code));
        Field {
            result_t,
            kind: parts.first().map(|k| pystr::upper(k)).unwrap_or_default(),
            args: parts.iter().skip(1).map(|a| a.to_string()).collect(),
        }
    }

    fn has(&self, switch: &str) -> bool {
        self.args.iter().any(|a| a == switch)
    }

    /// `args.index(switch)`, or `None`.
    fn index(&self, switch: &str) -> Option<usize> {
        self.args.iter().position(|a| a == switch)
    }

    /// The digit argument after `switch` (`\s 1`, `\r 1`), if any.
    fn number_after(&self, switch: &str) -> Option<i64> {
        let i = self.index(switch)?;
        self.args
            .get(i + 1)
            .filter(|a| is_digit_str(a))
            .map(|a| py_int(a))
    }
}

#[derive(PartialEq)]
enum State {
    Code,
    Result,
}

/// The complex fields of the paragraph at `p_path` under `root`, each with
/// its first result `w:t`.
fn paragraph_fields(root: &Element, p_path: &[usize]) -> Vec<Field> {
    let p = node(root, p_path);
    let mut out = Vec::new();
    let mut state: Option<State> = None;
    let mut code: Vec<String> = Vec::new();
    let mut result_t: Option<Path> = None;
    for rel in iter_paths(p, &[w!("fldChar"), w!("instrText"), w!("t")]) {
        let e = node(p, &rel);
        if e.is(w!("fldChar")) {
            match e.get(w!("fldCharType")) {
                Some("begin") => {
                    state = Some(State::Code);
                    code = Vec::new();
                    result_t = None;
                }
                Some("separate") if state == Some(State::Code) => state = Some(State::Result),
                Some("end") if state.is_some() => {
                    out.push(Field::new(&code.concat(), result_t.take()));
                    state = None;
                }
                _ => {}
            }
        } else if e.is(w!("instrText")) && state == Some(State::Code) {
            code.push(e.text.clone().unwrap_or_default());
        } else if e.is(w!("t")) && state == Some(State::Result) && result_t.is_none() {
            result_t = Some([p_path, &rel[..]].concat());
        }
    }
    out
}

fn style_of(p: &Element) -> &str {
    p.find_path(&[w!("pPr"), w!("pStyle")])
        .map_or("", |ps| ps.get(w!("val")).unwrap_or(""))
}

fn num_of(p: &Element) -> Option<(String, i64)> {
    let numpr = p.find_path(&[w!("pPr"), w!("numPr")])?;
    let nid = numpr.find(w!("numId"));
    let ilvl = numpr.find(w!("ilvl"));
    let nid = nid?;
    Some((
        nid.get(w!("val")).unwrap_or("").to_string(),
        ilvl.map_or(0, |i| py_int(i.get(w!("val")).unwrap_or("0"))),
    ))
}

/// Set the cached result of every SEQ/STYLEREF/REF field (in place).
///
/// `body` is the `w:body` built by the document writer, `numbering_xml` the
/// `word/numbering.xml` bytes the package will carry (with the list
/// restarts), and `notes` the `w:footnote`/`w:endnote` elements, whose `REF`
/// results are filled too.
pub fn fill_field_results<'a>(
    body: &mut Element,
    numbering_xml: &[u8],
    notes: impl IntoIterator<Item = &'a mut Element>,
) -> Result<(), XmlError> {
    let mut numbering = Numbering::new(numbering_xml)?;
    let mut seq: HashMap<String, i64> = HashMap::new();
    // counter -> heading level that resets it (\s N)
    let mut seq_reset: Dict<i64> = Dict::new();
    // level -> latest heading's number text
    let mut heading_numbers: HashMap<i64, String> = HashMap::new();
    let mut bookmark_text: HashMap<String, Vec<String>> = HashMap::new();
    let mut bookmark_number: HashMap<String, String> = HashMap::new();
    // (None = body / Some(i) = notes[i], field)
    let mut refs: Vec<(Option<usize>, Field)> = Vec::new();

    for pp in iter_paths(body, &[w!("p")]) {
        let p = node(body, &pp);
        let mut number: Option<(String, String)> = None;
        if let Some((num_id, ilvl)) = num_of(p) {
            number = numbering.advance(&num_id, ilvl);
        }
        if let Some(hm) = HEADING_RE.search(style_of(p)) {
            let level = py_int(hm.group_str(1));
            for (counter, reset_level) in seq_reset.iter() {
                if level <= *reset_level {
                    seq.insert(counter.clone(), 0);
                }
            }
            heading_numbers.retain(|&k, _| k <= level);
            if let Some((full, _)) = &number {
                heading_numbers.insert(level, full.clone());
            }
        }
        for f in paragraph_fields(body, &pp) {
            if f.kind == "SEQ" && !f.args.is_empty() {
                let name = f.args[0].clone();
                if f.has("\\s") {
                    if let Some(n) = f.number_after("\\s") {
                        seq_reset.insert(name.clone(), n);
                    }
                }
                if let Some(n) = f.number_after("\\r") {
                    seq.insert(name.clone(), n); // explicit reset
                } else if !f.has("\\c") {
                    *seq.entry(name.clone()).or_insert(0) += 1;
                }
                let value = seq.get(&name).copied().unwrap_or(0).to_string();
                set(body, &f, &value);
            } else if f.kind == "STYLEREF" && !f.args.is_empty() {
                let level = if is_digit_str(&f.args[0]) {
                    py_int(&f.args[0])
                } else {
                    1
                };
                let value = heading_numbers.get(&level).map_or("0", String::as_str);
                let value = pystr::rstrip_chars(value, ".").to_string();
                set(body, &f, &value);
            } else if f.kind == "REF" && !f.args.is_empty() {
                refs.push((None, f));
            }
        }
        // bookmarks: their text (after this paragraph's fields are filled) and
        // the paragraph's own number (for REF \r)
        let p = node(body, &pp);
        let mut open_marks: Dict<String> = Dict::new();
        for rel in iter_paths(
            p,
            &[
                w!("bookmarkStart"),
                w!("bookmarkEnd"),
                w!("t"),
                w!("instrText"),
            ],
        ) {
            let e = node(p, &rel);
            if e.is(w!("bookmarkStart")) {
                let name = e.get(w!("name")).unwrap_or("").to_string();
                open_marks.insert(e.get(w!("id")).unwrap_or(""), name.clone());
                bookmark_text.entry(name.clone()).or_default();
                if let Some((full, _)) = &number {
                    bookmark_number.insert(name, full.clone());
                }
            } else if e.is(w!("bookmarkEnd")) {
                open_marks.remove(e.get(w!("id")).unwrap_or(""));
            } else if e.is(w!("t")) {
                for name in open_marks.values() {
                    bookmark_text
                        .get_mut(name)
                        .expect("opened bookmark has a text list")
                        .push(e.text.clone().unwrap_or_default());
                }
            }
        }
    }
    let mut notes: Vec<&mut Element> = notes.into_iter().collect();
    for (ni, note) in notes.iter().enumerate() {
        for pp in iter_paths(note, &[w!("p")]) {
            refs.extend(
                paragraph_fields(note, &pp)
                    .into_iter()
                    .filter(|f| f.kind == "REF" && !f.args.is_empty())
                    .map(|f| (Some(ni), f)),
            );
        }
    }
    for (src, f) in &refs {
        let root: &mut Element = match src {
            None => &mut *body,
            Some(i) => &mut *notes[*i],
        };
        let mark = &f.args[0];
        if f.has("\\r") || f.has("\\n") || f.has("\\w") {
            if let Some(value) = bookmark_number.get(mark) {
                set(root, f, pystr::rstrip_chars(value, "."));
            }
            continue;
        }
        let text = bookmark_text
            .get(mark)
            .map(|t| t.concat())
            .unwrap_or_default();
        let text = pystr::strip(&text);
        if !text.is_empty() {
            set(root, f, text);
        }
    }
    Ok(())
}

/// `_set(f, value)`: write a non-empty result into the field's `w:t`.
fn set(root: &mut Element, f: &Field, value: &str) {
    if let Some(path) = &f.result_t {
        if !value.is_empty() {
            node_mut(root, path).text = Some(value.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    //! Port of `tests/test_field_results.py`. The Python tests convert `SRC`
    //! end to end; here the input is the body/footnote the Python 1.1.0
    //! `DocumentWriter` builds for that source (`testdata/field_results/`,
    //! captured just before `fill_field_results`), and the expected output is
    //! what Python's pass makes of it. The synthetic cases at the end cover
    //! the edge paths, with expected texts computed by Python.

    use super::*;
    use crate::package::numbering_xml;
    use tex2word_ir::re::Flags;
    use tex2word_ooxml::xml::{tostring, ToString};

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!("../testdata/field_results/", $name))
        };
    }

    static PARA_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::with_flags(r"<w:p[ >].*?</w:p>", Flags::S));
    static T_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<w:t(?: [^>]*)?>([^<]*)</w:t>"));

    /// `_paras(xml)`: the non-empty paragraph texts (of the UTF-8 part, as
    /// the Python test reads it; `xml` is lxml's default ASCII output).
    fn paras(xml: &str) -> Vec<String> {
        let utf8 = ToString {
            utf8: true,
            ..ToString::default()
        };
        let xml = tostring(&fromstring(xml.as_bytes()).unwrap(), utf8).unwrap();
        let mut out = Vec::new();
        for p in PARA_RE.findall(&xml) {
            let t = T_RE.findall(p).concat().replace('\u{a0}', " ");
            let t = pystr::strip(&t);
            if !t.is_empty() {
                out.push(t.to_string());
            }
        }
        out
    }

    fn ser(e: &Element) -> String {
        tostring(e, ToString::default()).unwrap()
    }

    /// Fill a captured body + footnote; returns their serialization.
    fn fill_fixture(body: &str, footnote: &str) -> (String, String) {
        let mut body = fromstring(body.as_bytes()).unwrap();
        let mut notes = vec![fromstring(footnote.as_bytes()).unwrap()];
        // the source's one enumerate list has its own instance
        fill_field_results(&mut body, &numbering_xml(&[(100, 0, 1)]), &mut notes).unwrap();
        (ser(&body), ser(&notes[0]))
    }

    #[test]
    fn cached_numbers_and_references_are_real() {
        let (body, footnote) = fill_fixture(
            fixture!("src_flat_body_before.xml"),
            fixture!("src_flat_footnote_before.xml"),
        );
        assert_eq!(body, fixture!("src_flat_body_after.xml"));
        assert_eq!(footnote, fixture!("src_flat_footnote_after.xml"));
        let ps = paras(&body);
        let has = |line: &str| ps.iter().any(|p| p == line);
        assert!(has(
            "See 2, 2.1, Figure 2, Table 1, (2), Lemma 2.2 and item 2."
        ));
        for line in [
            "Figure 1: First",
            "Figure 2: Second",
            "Table 1: Tab",
            "(1)",
            "(3)",
            "Theorem 2.1. A",
            "Lemma 2.2. B",
            "Theorem A.1. C",
            "Refs: A, (3), 1, 2.1.",
        ] {
            assert!(has(line), "{line}");
        }
        assert!(paras(&footnote).iter().any(|p| p == "Also 2.1."));
    }

    #[test]
    fn per_section_numbering_caches_section_numbers() {
        let (body, footnote) = fill_fixture(
            fixture!("src_by_section_body_before.xml"),
            fixture!("src_by_section_footnote_before.xml"),
        );
        assert_eq!(body, fixture!("src_by_section_body_after.xml"));
        assert_eq!(footnote, fixture!("src_by_section_footnote_after.xml"));
        let ps = paras(&body);
        let has = |line: &str| ps.iter().any(|p| p == line);
        assert!(has("Figure 2.1: Second") && has("Table 2.1: Tab"));
        assert!(has(
            "See 2, 2.1, Figure 2.1, Table 2.1, (2.1), Lemma 2.2 and item 2."
        ));
    }

    #[test]
    fn each_ordered_list_restarts_numbering_part() {
        // the numbering half of `test_each_ordered_list_restarts` (the writer
        // half -- one instance per list -- belongs to `document.rs`)
        let base = numbering_xml(&[]);
        assert_eq!(base, crate::data::NUMBERING_XML.as_bytes());
        let xml =
            String::from_utf8(numbering_xml(&[(100, 0, 1), (101, 0, 1), (102, 0, 5)])).unwrap();
        assert!(xml.contains(r#"<w:startOverride w:val="5"/>"#));
        // Python: numbering_xml([(100, 0, 1), (101, 1, 5), (102, 0, 3)])
        let expected = String::from_utf8(base).unwrap().replace(
            "</w:numbering>",
            concat!(
                r#"  <w:num w:numId="100"><w:abstractNumId w:val="1"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="1"/></w:lvlOverride></w:num>"#,
                "\n",
                r#"  <w:num w:numId="101"><w:abstractNumId w:val="1"/><w:lvlOverride w:ilvl="1"><w:startOverride w:val="5"/></w:lvlOverride></w:num>"#,
                "\n",
                r#"  <w:num w:numId="102"><w:abstractNumId w:val="1"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="3"/></w:lvlOverride></w:num>"#,
                "\n</w:numbering>"
            ),
        );
        assert_eq!(
            numbering_xml(&[(100, 0, 1), (101, 1, 5), (102, 0, 3)]),
            expected.into_bytes()
        );
    }

    fn texts(e: &Element) -> Vec<String> {
        e.iter(Some(w!("t")))
            .into_iter()
            .map(|t| t.text.clone().unwrap_or_default())
            .collect()
    }

    fn check(
        numbering: &[u8],
        body: &str,
        notes: &[&str],
        expect: &[&str],
        expect_notes: &[&[&str]],
    ) {
        let ns = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
        let mut b = fromstring(format!("<w:body {ns}>{body}</w:body>").as_bytes()).unwrap();
        let mut ns_: Vec<Element> = notes
            .iter()
            .map(|n| fromstring(n.as_bytes()).unwrap())
            .collect();
        fill_field_results(&mut b, numbering, ns_.iter_mut()).unwrap();
        assert_eq!(texts(&b), expect);
        let got: Vec<Vec<String>> = ns_.iter().map(texts).collect();
        assert_eq!(got, expect_notes);
    }

    #[test]
    fn list_formats_and_paragraph_numbers() {
        check(
            NUMBERING_CUSTOM.as_bytes(),
            BODY_LIST_FORMATS,
            NOTES_LIST_FORMATS,
            EXPECT_LIST_FORMATS,
            EXPECT_NOTES_LIST_FORMATS,
        );
    }

    #[test]
    fn seq_and_styleref_switches() {
        check(
            &numbering_xml(&[(100, 0, 1), (101, 1, 4)]),
            BODY_SEQ_STYLEREF,
            NOTES_SEQ_STYLEREF,
            EXPECT_SEQ_STYLEREF,
            EXPECT_NOTES_SEQ_STYLEREF,
        );
    }

    #[test]
    fn bookmark_text_notes_and_odd_fields() {
        check(
            &numbering_xml(&[]),
            BODY_BOOKMARKS_NOTES,
            NOTES_BOOKMARKS_NOTES,
            EXPECT_BOOKMARKS_NOTES,
            EXPECT_NOTES_BOOKMARKS_NOTES,
        );
    }

    #[test]
    fn number_formats() {
        assert_eq!(letters(0), "A");
        assert_eq!(letters(26), "Z");
        assert_eq!(letters(27), "AA");
        assert_eq!(letters(702), "ZZ");
        assert_eq!(letters(703), "AAA");
        assert_eq!(roman(0), "0");
        assert_eq!(roman(1994), "MCMXCIV");
        assert_eq!(format_number(4, "lowerRoman"), "iv");
        assert_eq!(format_number(-3, "decimal"), "-3");
    }

    // generated by synth.py (expected values from Python 1.1.0)
    // list_formats
    const BODY_LIST_FORMATS: &str = r##"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:bookmarkStart w:id="1" w:name="m1"/><w:r><w:t>a</w:t></w:r><w:bookmarkEnd w:id="1"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr><w:bookmarkStart w:id="2" w:name="m2"/><w:r><w:t>b</w:t></w:r><w:bookmarkEnd w:id="2"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="2"/><w:numId w:val="1"/></w:numPr></w:pPr><w:bookmarkStart w:id="3" w:name="m3"/><w:r><w:t>c</w:t></w:r><w:bookmarkEnd w:id="3"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="2"/><w:numId w:val="1"/></w:numPr></w:pPr><w:bookmarkStart w:id="4" w:name="m4"/><w:r><w:t>c2</w:t></w:r><w:bookmarkEnd w:id="4"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="1"/></w:numPr></w:pPr><w:bookmarkStart w:id="5" w:name="m5"/><w:r><w:t>b2</w:t></w:r><w:bookmarkEnd w:id="5"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="4"/><w:numId w:val="1"/></w:numPr></w:pPr><w:bookmarkStart w:id="6" w:name="m6"/><w:r><w:t>bullet</w:t></w:r><w:bookmarkEnd w:id="6"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="3"/><w:numId w:val="2"/></w:numPr></w:pPr><w:bookmarkStart w:id="7" w:name="m7"/><w:r><w:t>deep</w:t></w:r><w:bookmarkEnd w:id="7"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="3"/><w:numId w:val="2"/></w:numPr></w:pPr><w:bookmarkStart w:id="8" w:name="m8"/><w:r><w:t>deep2</w:t></w:r><w:bookmarkEnd w:id="8"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:bookmarkStart w:id="10" w:name="m9"/><w:r><w:t>big</w:t></w:r><w:bookmarkEnd w:id="10"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="3"/></w:numPr></w:pPr><w:bookmarkStart w:id="11" w:name="m10"/><w:r><w:t>zero</w:t></w:r><w:bookmarkEnd w:id="11"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="2"/><w:numId w:val="3"/></w:numPr></w:pPr><w:bookmarkStart w:id="12" w:name="m11"/><w:r><w:t>zero2</w:t></w:r><w:bookmarkEnd w:id="12"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="4"/></w:numPr></w:pPr><w:bookmarkStart w:id="13" w:name="m12"/><w:r><w:t>noabs</w:t></w:r><w:bookmarkEnd w:id="13"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="99"/></w:numPr></w:pPr><w:bookmarkStart w:id="14" w:name="m13"/><w:r><w:t>nonum</w:t></w:r><w:bookmarkEnd w:id="14"/></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m1 \r \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m2 \r \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m3 \n</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m4 \w</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m5 \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m6 \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m7 \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m8 \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m9 \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m10 \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m11 \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m12 \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m13 \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF nothere \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF m1 \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"##;
    const NOTES_LIST_FORMATS: &[&str] = &[];
    const EXPECT_LIST_FORMATS: &[&str] = &[
        r##"a"##,
        r##"b"##,
        r##"c"##,
        r##"c2"##,
        r##"b2"##,
        r##"bullet"##,
        r##"deep"##,
        r##"deep2"##,
        r##"big"##,
        r##"zero"##,
        r##"zero2"##,
        r##"noabs"##,
        r##"nonum"##,
        r##"I"##,
        r##"I(a)"##,
        r##"I.a.i"##,
        r##"I.a.ii"##,
        r##"I(b)"##,
        r##" "##,
        r##"[AA]"##,
        r##"[AB]"##,
        r##"II"##,
        r##"0"##,
        r##"0.a.i"##,
        r##"1"##,
        r##" "##,
        r##" "##,
        r##"a"##,
    ];
    const EXPECT_NOTES_LIST_FORMATS: &[&[&str]] = &[];
    // seq_styleref
    const BODY_SEQ_STYLEREF: &str = r##"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 1 \n</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Figure \* ARABIC</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Heading1"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:t>One</w:t></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 1 \n</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t>.</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Figure \r 1 \s 1 \* ARABIC</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 1 \n</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t>.</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Figure \s 1 \* ARABIC</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Table \* ARABIC</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Table \c</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Other \c</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">seq Table</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Table \r 7</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Table \r x</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Heading2"/><w:numPr><w:ilvl w:val="1"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:t>Sub</w:t></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 2</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF x \s</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 3</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Figure \s 1</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Unnumbered</w:t></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 1</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Figure \s 1</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Table</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Heading1"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:t>Two</w:t></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 1 \n</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Figure \s 1</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Table</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Thm \s 2</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Heading2"/><w:numPr><w:ilvl w:val="1"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:t>Two A</w:t></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Thm \s 2</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Thm \s 2</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 2 \n</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Heading3"/><w:numPr><w:ilvl w:val="2"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:t>Deep</w:t></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Thm \s 2</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 2</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 3</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:pPr><w:pStyle w:val="Heading1"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="4"/></w:numPr></w:pPr><w:r><w:t>App</w:t></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 1 \n</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Figure \s 1</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="100"/></w:numPr></w:pPr><w:bookmarkStart w:id="21" w:name="li1"/><w:r><w:t>x</w:t></w:r><w:bookmarkEnd w:id="21"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="100"/></w:numPr></w:pPr><w:bookmarkStart w:id="22" w:name="li2"/><w:r><w:t>y</w:t></w:r><w:bookmarkEnd w:id="22"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="101"/></w:numPr></w:pPr><w:bookmarkStart w:id="23" w:name="li3"/><w:r><w:t>z</w:t></w:r><w:bookmarkEnd w:id="23"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="100"/></w:numPr></w:pPr><w:bookmarkStart w:id="24" w:name="li4"/><w:r><w:t>w</w:t></w:r><w:bookmarkEnd w:id="24"/></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:bookmarkStart w:id="25" w:name="li5"/><w:r><w:t>shared</w:t></w:r><w:bookmarkEnd w:id="25"/></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF li2 \r \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF li3 \r \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF li4 \r \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF li5 \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">PAGEREF li1 \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"></w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve">keep</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"##;
    const NOTES_SEQ_STYLEREF: &[&str] = &[];
    const EXPECT_SEQ_STYLEREF: &[&str] = &[
        r##"0"##,
        r##"1"##,
        r##"One"##,
        r##"1"##,
        r##"."##,
        r##"1"##,
        r##"1"##,
        r##"."##,
        r##"2"##,
        r##"1"##,
        r##"1"##,
        r##"0"##,
        r##"2"##,
        r##"7"##,
        r##"8"##,
        r##"Sub"##,
        r##"1.1"##,
        r##"1"##,
        r##"0"##,
        r##"3"##,
        r##"Unnumbered"##,
        r##"1"##,
        r##"1"##,
        r##"9"##,
        r##"Two"##,
        r##"2"##,
        r##"1"##,
        r##"10"##,
        r##"1"##,
        r##"Two A"##,
        r##"1"##,
        r##"2"##,
        r##"2.1"##,
        r##"Deep"##,
        r##"3"##,
        r##"2.1"##,
        r##"2.1.1"##,
        r##"App"##,
        r##"A"##,
        r##"1"##,
        r##"x"##,
        r##"y"##,
        r##"z"##,
        r##"w"##,
        r##"shared"##,
        r##"2"##,
        r##"(d)"##,
        r##"(a)"##,
        r##"1"##,
        r##" "##,
        r##" "##,
        r##" "##,
        r##" "##,
        r##"keep"##,
    ];
    const EXPECT_NOTES_SEQ_STYLEREF: &[&[&str]] = &[];
    // bookmarks_notes
    const BODY_BOOKMARKS_NOTES: &str = r##"<w:p><w:pPr><w:pStyle w:val="Heading1"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="3"/></w:numPr></w:pPr><w:r><w:t>H</w:t></w:r></w:p><w:p><w:r><w:t>Figure </w:t></w:r><w:bookmarkStart w:id="1" w:name="fig_a"/><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 1 \n</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t>.</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Figure \s 1</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:bookmarkEnd w:id="1"/><w:r><w:t>: cap</w:t></w:r></w:p><w:p><w:bookmarkStart w:id="2" w:name="eq"/><w:r><w:t>(</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Equation</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t>)</w:t></w:r><w:bookmarkEnd w:id="2"/></w:p><w:p><w:bookmarkStart w:id="3" w:name="ws"/><w:r><w:t>  </w:t></w:r><w:bookmarkEnd w:id="3"/></w:p><w:p><w:bookmarkStart w:id="4" w:name="open"/><w:r><w:t>never </w:t></w:r><w:r><w:t>closed</w:t></w:r></w:p><w:p><w:r><w:t>after </w:t></w:r><w:bookmarkStart w:id="5" w:name="ov"/><w:r><w:t>outer </w:t></w:r><w:bookmarkStart w:id="6" w:name="inner"/><w:r><w:t>in</w:t></w:r><w:bookmarkEnd w:id="6"/><w:r><w:t> tail</w:t></w:r><w:bookmarkEnd w:id="5"/></w:p><w:p><w:bookmarkStart w:id="7" w:name="refin"/><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF eq \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve">0</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:bookmarkEnd w:id="7"/></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF fig_a \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve">0</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF eq \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF ws \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve">0</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF open \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF ov \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF inner \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF refin \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF nope \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve">0</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF fig_a \p</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve">0</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF eq \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>SEQ Outer</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Inner</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t>o</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>stray</w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>SE</w:instrText></w:r><w:r><w:instrText>Q Split</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t> </w:t></w:r><w:r><w:t>2nd</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Box</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:txbxContent><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Box</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:bookmarkStart w:id="8" w:name="bx"/><w:r><w:t>box</w:t></w:r><w:bookmarkEnd w:id="8"/></w:p></w:txbxContent></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Box</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF bx \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p></w:tc></w:tr></w:tbl>"##;
    const NOTES_BOOKMARKS_NOTES: &[&str] = &[
        r##"<w:footnote xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:id="1"><w:p><w:r><w:t>See </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF fig_a \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve">0</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF eq \r</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">SEQ Figure</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">STYLEREF 1</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p></w:footnote>"##,
        r##"<w:endnote xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" w:id="1"><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF inner \h</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve">0</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve">REF ov</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t xml:space="preserve"> </w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p></w:endnote>"##,
    ];
    const EXPECT_BOOKMARKS_NOTES: &[&str] = &[
        r##"H"##,
        r##"Figure "##,
        r##"1"##,
        r##"."##,
        r##"1"##,
        r##": cap"##,
        r##"("##,
        r##"1"##,
        r##")"##,
        r##"  "##,
        r##"never "##,
        r##"closed"##,
        r##"after "##,
        r##"outer "##,
        r##"in"##,
        r##" tail"##,
        r##"(1)"##,
        r##"1.1"##,
        r##"(1)"##,
        r##"0"##,
        r##"never closed"##,
        r##"outer in tail"##,
        r##"in"##,
        r##"0"##,
        r##"0"##,
        r##"1.1"##,
        r##"1"##,
        r##"o"##,
        r##"stray"##,
        r##"1"##,
        r##"2nd"##,
        r##"1"##,
        r##"3"##,
        r##"box"##,
        r##"4"##,
        r##"boxbox"##,
    ];
    const EXPECT_NOTES_BOOKMARKS_NOTES: &[&[&str]] = &[
        &[r##"See "##, r##"1.1"##, r##" "##, r##" "##, r##" "##],
        &[r##"in"##, r##"outer in tail"##],
    ];
    const NUMBERING_CUSTOM: &str = r##"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
    <w:abstractNum w:abstractNumId="7">
    <w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="upperRoman"/><w:lvlText w:val="%1."/></w:lvl>
    <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1(%2)"/></w:lvl>
    <w:lvl w:ilvl="2"><w:start w:val="1"/><w:numFmt w:val="lowerRoman"/><w:lvlText w:val="%1.%2.%3"/></w:lvl>
    <w:lvl w:ilvl="3"><w:numFmt w:val="upperLetter"/><w:lvlText w:val="[%4]"/></w:lvl>
    <w:lvl w:ilvl="4"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="o"/></w:lvl>
    </w:abstractNum>
    <w:num w:numId="1"><w:abstractNumId w:val="7"/></w:num>
    <w:num w:numId="2"><w:abstractNumId w:val="7"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="49"/></w:lvlOverride><w:lvlOverride w:ilvl="3"><w:startOverride w:val="27"/></w:lvlOverride></w:num>
    <w:num w:numId="3"><w:abstractNumId w:val="7"/><w:lvlOverride w:ilvl="0"><w:startOverride w:val="0"/></w:lvlOverride></w:num>
    <w:num w:numId="4"><w:abstractNumId w:val="99"/></w:num>
    </w:numbering>"##;
}
