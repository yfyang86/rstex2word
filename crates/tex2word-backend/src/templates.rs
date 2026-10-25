//! Packaged reference templates (port of `templates/__init__.py`): the
//! bundled `styles.xml` and the language/font edits applied to it.

use tex2word_ooxml::parse::fromstring;
use tex2word_ooxml::xml::{serialize, Element};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn w(name: &str) -> String {
    format!("{{{W}}}{name}")
}

/// The curated reference `styles.xml` shipped with the package.
pub fn load_styles_xml() -> Vec<u8> {
    include_bytes!("../data/styles.xml").to_vec()
}

fn default_rpr(root: &mut Element) -> Option<&mut Element> {
    root.find_path_mut(&[&w("docDefaults"), &w("rPrDefault"), &w("rPr")])
}

/// Set the document default language in the styles docDefaults `w:lang`
/// (`w:val` = Latin proofing language, `w:eastAsia` = CJK). A CJK `lang`
/// fills both halves unless `east_asia` overrides it. Unchanged bytes when
/// the structure isn't found.
pub fn apply_language(styles_xml: &[u8], lang: Option<&str>, east_asia: Option<&str>) -> Vec<u8> {
    let lang = lang.filter(|l| !l.is_empty());
    let mut east_asia = east_asia.map(str::to_string);
    if let Some(l) = lang {
        let primary = l.split('-').next().unwrap_or("").to_lowercase();
        if east_asia.is_none() && matches!(primary.as_str(), "zh" | "ja" | "ko") {
            east_asia = Some(l.to_string());
        }
    }
    let east_asia = east_asia.filter(|e| !e.is_empty());
    if lang.is_none() && east_asia.is_none() {
        return styles_xml.to_vec();
    }
    let Ok(mut root) = fromstring(styles_xml) else {
        return styles_xml.to_vec();
    };
    let Some(rpr) = default_rpr(&mut root) else {
        return styles_xml.to_vec();
    };
    if rpr.find(&w("lang")).is_none() {
        rpr.append(Element::new(&w("lang"))); // last child (lang is late in CT_RPr)
    }
    let lang_el = rpr.find_mut(&w("lang")).unwrap();
    if let Some(l) = lang {
        lang_el.set(&w("val"), l);
    }
    if let Some(e) = &east_asia {
        lang_el.set(&w("eastAsia"), e.as_str());
    }
    serialize(&root).unwrap_or_else(|_| styles_xml.to_vec())
}

/// Heading/title styles that pick up the CJK *sans* font.
const SANS_CJK_STYLES: &[&str] = &["Title", "Subtitle", "Heading1", "Heading2", "Heading3", "Heading4", "Heading5"];

/// Set/merge `w:rFonts` (first child of `w:rPr`).
fn set_rfonts(rpr: &mut Element, attrs: &[(&str, Option<&str>)]) {
    if rpr.find(&w("rFonts")).is_none() {
        rpr.insert(0, Element::new(&w("rFonts")));
    }
    let rfonts = rpr.find_mut(&w("rFonts")).unwrap();
    for (key, val) in attrs {
        if let Some(v) = val.filter(|v| !v.is_empty()) {
            rfonts.set(&w(key), v);
        }
    }
}

/// Apply XeLaTeX/xeCJK font choices to the styles `docDefaults` (Latin and
/// East-Asian defaults) and to the heading (`cjk_sans`) and Source Code
/// (`cjk_mono`) styles. Unchanged bytes when the structure isn't present.
pub fn apply_fonts(
    styles_xml: &[u8],
    main: Option<&str>,
    cjk_main: Option<&str>,
    cjk_sans: Option<&str>,
    cjk_mono: Option<&str>,
) -> Vec<u8> {
    fn nz(x: Option<&str>) -> Option<&str> {
        x.filter(|s| !s.is_empty())
    }
    let (main, cjk_main, cjk_sans, cjk_mono) = (nz(main), nz(cjk_main), nz(cjk_sans), nz(cjk_mono));
    if main.is_none() && cjk_main.is_none() && cjk_sans.is_none() && cjk_mono.is_none() {
        return styles_xml.to_vec();
    }
    let Ok(mut root) = fromstring(styles_xml) else {
        return styles_xml.to_vec();
    };
    if main.is_some() || cjk_main.is_some() {
        if let Some(rpr) = default_rpr(&mut root) {
            set_rfonts(rpr, &[("ascii", main), ("hAnsi", main), ("eastAsia", cjk_main)]);
        }
    }
    if cjk_sans.is_some() || cjk_mono.is_some() {
        // styles_by_id: the last style with a given id wins (dict semantics)
        let style_index = |root: &Element, sid: &str| -> Option<usize> {
            root.children
                .iter()
                .enumerate()
                .filter(|(_, s)| s.is(&w("style")) && s.get(&w("styleId")) == Some(sid))
                .map(|(i, _)| i)
                .last()
        };
        let apply = |root: &mut Element, sid: &str, font: &str| {
            if let Some(i) = style_index(root, sid) {
                let style = &mut root.children[i];
                if style.find(&w("rPr")).is_none() {
                    style.append(Element::new(&w("rPr"))); // rPr is late in CT_Style
                }
                set_rfonts(style.find_mut(&w("rPr")).unwrap(), &[("eastAsia", Some(font))]);
            }
        };
        if let Some(f) = cjk_sans {
            for sid in SANS_CJK_STYLES {
                apply(&mut root, sid, f);
            }
        }
        if let Some(f) = cjk_mono {
            apply(&mut root, "SourceCode", f);
        }
    }
    serialize(&root).unwrap_or_else(|_| styles_xml.to_vec())
}
