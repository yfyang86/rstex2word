//! An XML element tree with lxml's semantics, and the OOXML builders of the
//! Python reference's `backend/ooxml.py`.
//!
//! Elements are owned values (no parent pointers). Tags and attribute names
//! use Clark notation (`{uri}local`). Each element remembers the namespace
//! declarations it carries (`nsdefs`) and the prefix it was created with;
//! the serializer resolves prefixes against the declarations in scope, as
//! libxml2 does.
//!
//! lxml builds trees by appending elements that were created with the full
//! namespace map; on append it strips the child's declarations that its new
//! ancestors already make. Elements built with [`el`]/[`sub`] are flagged
//! `strip_redundant`, and the serializer drops such redundant declarations
//! when it writes them -- the same output. Parsed elements keep every
//! declaration, as libxml2 does.

use std::fmt;

/// The OOXML namespace map (`ooxml.NS`), in declaration order.
pub const NS: &[(&str, &str)] = &[
    (
        "w",
        "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
    ),
    (
        "m",
        "http://schemas.openxmlformats.org/officeDocument/2006/math",
    ),
    (
        "r",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    ),
    (
        "wp",
        "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
    ),
    ("a", "http://schemas.openxmlformats.org/drawingml/2006/main"),
    (
        "pic",
        "http://schemas.openxmlformats.org/drawingml/2006/picture",
    ),
    (
        "ct",
        "http://schemas.openxmlformats.org/package/2006/content-types",
    ),
    (
        "rel",
        "http://schemas.openxmlformats.org/package/2006/relationships",
    ),
];

/// The `xml:` namespace.
pub const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// lxml's `ValueError` for strings that aren't XML compatible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlError(pub String);

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for XmlError {}

/// The node kind: lxml keeps comments and processing instructions in the
/// child list, with their own tails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Element,
    /// `text` holds the comment body
    Comment,
    /// `local` holds the target, `text` the data
    Pi,
}

/// One attribute: namespace URI (`None` = no namespace), local name, value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    pub ns: Option<String>,
    pub local: String,
    /// the prefix it was written with (a hint; the serializer resolves it)
    pub prefix: Option<String>,
    pub value: String,
}

/// An element (or comment / processing instruction) of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub kind: Kind,
    pub ns: Option<String>,
    pub local: String,
    /// the prefix the tag was created/parsed with (a hint)
    pub prefix: Option<String>,
    /// namespace declarations carried by this element, in order
    pub nsdefs: Vec<(Option<String>, String)>,
    /// drop declarations already in scope when serializing (built nodes)
    pub strip_redundant: bool,
    pub attrs: Vec<Attr>,
    pub text: Option<String>,
    pub tail: Option<String>,
    pub children: Vec<Element>,
}

/// Split a Clark-notation name into (namespace, local).
pub fn split_clark(name: &str) -> (Option<&str>, &str) {
    if let Some(rest) = name.strip_prefix('{') {
        if let Some((ns, local)) = rest.split_once('}') {
            return (Some(ns), local);
        }
    }
    (None, name)
}

fn ns_uri(prefix: &str) -> &'static str {
    NS.iter()
        .find(|(p, _)| *p == prefix)
        .map(|(_, u)| *u)
        .unwrap_or_else(|| panic!("unknown OOXML prefix {prefix:?}"))
}

fn ns_prefix(uri: &str) -> Option<&'static str> {
    NS.iter().find(|(_, u)| *u == uri).map(|(p, _)| *p)
}

/// Resolve a `prefix:local` tag into Clark notation (`w` when no prefix).
pub fn qn(tag: &str) -> String {
    let (prefix, local) = match tag.split_once(':') {
        Some((p, l)) if !l.is_empty() => (p, l),
        _ => ("w", tag),
    };
    format!("{{{}}}{local}", ns_uri(prefix))
}

impl Element {
    /// A new element for a Clark-notation tag with no namespace
    /// declarations (`etree.Element(tag)` / `SubElement(parent, tag)`).
    pub fn new(tag: &str) -> Element {
        let (ns, local) = split_clark(tag);
        Element {
            kind: Kind::Element,
            ns: ns.map(str::to_string),
            local: local.to_string(),
            prefix: ns.and_then(ns_prefix).map(str::to_string),
            nsdefs: Vec::new(),
            strip_redundant: true,
            attrs: Vec::new(),
            text: None,
            tail: None,
            children: Vec::new(),
        }
    }

    /// A new element carrying the full OOXML namespace map (`ooxml.el`
    /// without attributes).
    pub fn with_ns(tag: &str) -> Element {
        let mut e = Element::new(tag);
        e.nsdefs = NS
            .iter()
            .map(|(p, u)| (Some(p.to_string()), u.to_string()))
            .collect();
        e
    }

    /// A comment node.
    pub fn comment(text: &str) -> Element {
        Element {
            kind: Kind::Comment,
            text: Some(text.to_string()),
            ..Element::new("")
        }
    }

    /// A processing-instruction node.
    pub fn pi(target: &str, data: &str) -> Element {
        Element {
            kind: Kind::Pi,
            text: Some(data.to_string()),
            ..Element::new(target)
        }
    }

    pub fn is_element(&self) -> bool {
        self.kind == Kind::Element
    }

    /// The Clark-notation tag (`{uri}local`).
    pub fn tag(&self) -> String {
        match &self.ns {
            Some(ns) => format!("{{{ns}}}{}", self.local),
            None => self.local.clone(),
        }
    }

    /// True if the tag equals the Clark-notation name.
    pub fn is(&self, clark: &str) -> bool {
        let (ns, local) = split_clark(clark);
        self.is_element() && self.local == local && self.ns.as_deref() == ns
    }

    /// `element.set(key, value)`; the key is Clark notation or plain.
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let (ns, local) = split_clark(key);
        let value = value.into();
        if let Some(a) = self
            .attrs
            .iter_mut()
            .find(|a| a.local == local && a.ns.as_deref() == ns)
        {
            a.value = value;
            return;
        }
        let prefix = match ns {
            Some(XML_NS) => Some("xml".to_string()),
            Some(u) => ns_prefix(u).map(str::to_string),
            None => None,
        };
        self.attrs.push(Attr {
            ns: ns.map(str::to_string),
            local: local.to_string(),
            prefix,
            value,
        });
    }

    /// `element.get(key)`.
    pub fn get(&self, key: &str) -> Option<&str> {
        let (ns, local) = split_clark(key);
        self.attrs
            .iter()
            .find(|a| a.local == local && a.ns.as_deref() == ns)
            .map(|a| a.value.as_str())
    }

    /// Remove an attribute (`del element.attrib[key]`).
    pub fn remove_attr(&mut self, key: &str) -> Option<String> {
        let (ns, local) = split_clark(key);
        let i = self
            .attrs
            .iter()
            .position(|a| a.local == local && a.ns.as_deref() == ns)?;
        Some(self.attrs.remove(i).value)
    }

    /// `element.append(child)`.
    pub fn append(&mut self, child: Element) {
        self.children.push(child);
    }

    /// `element.insert(index, child)` (an index past the end appends).
    pub fn insert(&mut self, index: usize, child: Element) {
        let i = index.min(self.children.len());
        self.children.insert(i, child);
    }

    /// `len(element)`: the number of children (comments and PIs included).
    pub fn len(&self) -> usize {
        self.children.len()
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// `element.find(tag)`: the first child element with the Clark tag.
    pub fn find(&self, clark: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.is(clark))
    }

    pub fn find_mut(&mut self, clark: &str) -> Option<&mut Element> {
        self.children.iter_mut().find(|c| c.is(clark))
    }

    /// `element.findall(tag)`.
    pub fn findall(&self, clark: &str) -> Vec<&Element> {
        self.children.iter().filter(|c| c.is(clark)).collect()
    }

    /// `element.find("a/b/c")` over Clark-notation steps.
    pub fn find_path(&self, steps: &[&str]) -> Option<&Element> {
        let mut cur = self;
        for s in steps {
            cur = cur.find(s)?;
        }
        Some(cur)
    }

    pub fn find_path_mut(&mut self, steps: &[&str]) -> Option<&mut Element> {
        let mut cur = self;
        for s in steps {
            cur = cur.find_mut(s)?;
        }
        Some(cur)
    }

    /// The child elements (comments and PIs skipped).
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter(|c| c.is_element())
    }

    /// `element.iter(tag)`: this element and its descendants in document
    /// order, filtered by Clark tag (`None` = every element).
    pub fn iter(&self, clark: Option<&str>) -> Vec<&Element> {
        let mut out = Vec::new();
        fn go<'a>(e: &'a Element, clark: Option<&str>, out: &mut Vec<&'a Element>) {
            if e.is_element() && clark.is_none_or(|c| e.is(c)) {
                out.push(e);
            }
            for c in &e.children {
                go(c, clark, out);
            }
        }
        go(self, clark, &mut out);
        out
    }

    /// `element.iter()` over every node: this one and its descendants in
    /// document order, comments and processing instructions included.
    pub fn iter_nodes(&self) -> Vec<&Element> {
        let mut out = Vec::new();
        fn go<'a>(e: &'a Element, out: &mut Vec<&'a Element>) {
            out.push(e);
            for c in &e.children {
                go(c, out);
            }
        }
        go(self, &mut out);
        out
    }

    /// Apply `f` to this element and every descendant element, in document
    /// order (a mutable `iter()`).
    pub fn for_each_mut(&mut self, f: &mut dyn FnMut(&mut Element)) {
        if self.is_element() {
            f(self);
        }
        for c in self.children.iter_mut() {
            c.for_each_mut(f);
        }
    }

    /// The concatenated text of this element and its descendants
    /// (`"".join(element.itertext())`).
    pub fn itertext(&self) -> String {
        let mut out = String::new();
        fn go(e: &Element, out: &mut String, top: bool) {
            if e.is_element() {
                if let Some(t) = &e.text {
                    out.push_str(t);
                }
                for c in &e.children {
                    go(c, out, false);
                }
            }
            if !top {
                if let Some(t) = &e.tail {
                    out.push_str(t);
                }
            }
        }
        go(self, &mut out, true);
        out
    }
}

/// `ooxml.el(tag, *children, **attrs)`: an element with the full namespace
/// map; attribute keys are `prefix:local` (namespaced) or plain.
pub fn el(tag: &str, attrs: &[(&str, &str)]) -> Element {
    let mut e = Element::with_ns(&qn(tag));
    set_attrs(&mut e, attrs);
    e
}

/// `ooxml.sub(parent, tag, **attrs)`: append a new child and return it.
pub fn sub<'a>(parent: &'a mut Element, tag: &str, attrs: &[(&str, &str)]) -> &'a mut Element {
    parent.append(el(tag, attrs));
    parent.children.last_mut().unwrap()
}

/// `ooxml._set_attrs`.
pub fn set_attrs(e: &mut Element, attrs: &[(&str, &str)]) {
    for (k, v) in attrs {
        if k.contains(':') {
            e.set(&qn(k), *v);
        } else {
            e.set(k, *v);
        }
    }
}

/// `ooxml._XML_ILLEGAL`: the characters XML 1.0 forbids (C0 controls except
/// tab/newline/CR, and U+FFFE/U+FFFF).
fn xml_illegal(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}')
}

/// `ooxml.xml_safe(text)`: `text` without the control characters XML cannot
/// carry.
pub fn xml_safe(text: &str) -> String {
    if text.contains(xml_illegal) {
        text.replace(xml_illegal, "")
    } else {
        text.to_string()
    }
}

/// `ooxml.text_el(tag, text, **attrs)`.
pub fn text_el(tag: &str, text: &str, attrs: &[(&str, &str)]) -> Element {
    let mut e = el(tag, attrs);
    e.text = Some(xml_safe(text));
    e
}

/// `ooxml.preserve_space(node)`: `xml:space="preserve"`.
pub fn preserve_space(e: &mut Element) -> &mut Element {
    e.set(&format!("{{{XML_NS}}}space"), "preserve");
    e
}

/// `ooxml.serialize(root)`: `etree.tostring(root, xml_declaration=True,
/// encoding="UTF-8", standalone=True)`.
pub fn serialize(root: &Element) -> Result<Vec<u8>, XmlError> {
    let mut out = String::from("<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n");
    write_node(root, &mut out, &mut Vec::new(), false, true)?;
    Ok(out.into_bytes())
}

/// `etree.tostring(e)` options.
#[derive(Debug, Clone, Copy, Default)]
pub struct ToString {
    /// `encoding="UTF-8"`/`"unicode"` (raw) vs the default ASCII output
    /// (non-ASCII as character references)
    pub utf8: bool,
    /// prepend `<?xml version='1.0' encoding='UTF-8' standalone='yes'?>`
    pub declaration: bool,
    /// include the element's tail (lxml's default)
    pub with_tail: bool,
}

/// `etree.tostring(e, ...)` as a string.
pub fn tostring(e: &Element, opts: ToString) -> Result<String, XmlError> {
    let mut out = String::new();
    if opts.declaration {
        out.push_str("<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n");
    }
    write_node(e, &mut out, &mut Vec::new(), !opts.utf8, opts.with_tail)?;
    Ok(out)
}

/// lxml rejects NULs, control characters other than tab/newline/CR, and
/// U+FFFE/U+FFFF.
pub fn check_xml_string(s: &str) -> Result<(), XmlError> {
    if s.chars().any(|c| {
        (c < ' ' && !matches!(c, '\t' | '\n' | '\r')) || c == '\u{fffe}' || c == '\u{ffff}'
    }) {
        return Err(XmlError(
            "All strings must be XML compatible: Unicode or ASCII, no NULL bytes or control characters".into(),
        ));
    }
    Ok(())
}

fn push_char_ref(out: &mut String, c: char) {
    out.push_str(&format!("&#{};", c as u32));
}

fn escape_text(s: &str, out: &mut String, ascii: bool) -> Result<(), XmlError> {
    check_xml_string(s)?;
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#13;"),
            c if ascii && !c.is_ascii() => push_char_ref(out, c),
            c => out.push(c),
        }
    }
    Ok(())
}

fn escape_attr(s: &str, out: &mut String, ascii: bool) -> Result<(), XmlError> {
    check_xml_string(s)?;
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c if ascii && !c.is_ascii() => push_char_ref(out, c),
            c => out.push(c),
        }
    }
    Ok(())
}

/// In-scope declarations: a stack of (prefix, uri).
type Scope = Vec<(Option<String>, String)>;

fn lookup_prefix<'a>(scope: &'a Scope, prefix: Option<&str>) -> Option<&'a str> {
    scope
        .iter()
        .rev()
        .find(|(p, _)| p.as_deref() == prefix)
        .map(|(_, u)| u.as_str())
}

/// The prefix to write for `uri` (element tags may use the default
/// namespace; attributes need a prefix). Declares one when none is in scope.
fn resolve_prefix(
    scope: &mut Scope,
    decls: &mut Vec<(Option<String>, String)>,
    uri: &str,
    hint: Option<&str>,
    attribute: bool,
) -> Option<String> {
    if uri == XML_NS {
        return Some("xml".into());
    }
    if let Some(h) = hint {
        if lookup_prefix(scope, Some(h)) == Some(uri) {
            return Some(h.to_string());
        }
    }
    if !attribute && hint.is_none() && lookup_prefix(scope, None) == Some(uri) {
        return None;
    }
    // any visible declaration of the uri (nearest first)
    let found = scope
        .iter()
        .rev()
        .find(|(p, u)| {
            u == uri
                && (p.is_some() || !attribute)
                && lookup_prefix(scope, p.as_deref()) == Some(uri)
        })
        .map(|(p, _)| p.clone());
    if let Some(p) = found {
        return p;
    }
    // declare a new prefix (lxml names them ns0, ns1, ...)
    let mut n = 0;
    let p = loop {
        let cand = format!("ns{n}");
        if lookup_prefix(scope, Some(&cand)).is_none() {
            break cand;
        }
        n += 1;
    };
    decls.push((Some(p.clone()), uri.to_string()));
    scope.push((Some(p.clone()), uri.to_string()));
    Some(p)
}

fn write_node(
    e: &Element,
    out: &mut String,
    scope: &mut Scope,
    ascii: bool,
    with_tail: bool,
) -> Result<(), XmlError> {
    match e.kind {
        Kind::Comment => {
            out.push_str("<!--");
            let t = e.text.as_deref().unwrap_or("");
            check_xml_string(t)?;
            if ascii {
                escape_comment_ascii(t, out);
            } else {
                out.push_str(t);
            }
            out.push_str("-->");
        }
        Kind::Pi => {
            out.push_str("<?");
            out.push_str(&e.local);
            if let Some(d) = e.text.as_deref().filter(|d| !d.is_empty()) {
                check_xml_string(d)?;
                out.push(' ');
                out.push_str(d);
            }
            out.push_str("?>");
        }
        Kind::Element => write_element(e, out, scope, ascii)?,
    }
    if with_tail {
        if let Some(t) = &e.tail {
            escape_text(t, out, ascii)?;
        }
    }
    Ok(())
}

fn escape_comment_ascii(t: &str, out: &mut String) {
    for c in t.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            push_char_ref(out, c);
        }
    }
}

fn write_element(
    e: &Element,
    out: &mut String,
    scope: &mut Scope,
    ascii: bool,
) -> Result<(), XmlError> {
    let mark = scope.len();
    // this element's own declarations (redundant ones dropped for built nodes)
    let mut decls: Vec<(Option<String>, String)> = Vec::new();
    for (p, u) in &e.nsdefs {
        if e.strip_redundant && lookup_prefix(scope, p.as_deref()) == Some(u.as_str()) {
            continue;
        }
        decls.push((p.clone(), u.clone()));
    }
    scope.extend(decls.iter().cloned());
    let tag_prefix = match &e.ns {
        Some(u) => resolve_prefix(scope, &mut decls, u, e.prefix.as_deref(), false),
        None => {
            // an unqualified tag under a default namespace needs xmlns=""
            if lookup_prefix(scope, None).is_some_and(|u| !u.is_empty()) {
                decls.push((None, String::new()));
                scope.push((None, String::new()));
            }
            None
        }
    };
    let mut attr_names: Vec<String> = Vec::with_capacity(e.attrs.len());
    for a in &e.attrs {
        let name = match &a.ns {
            Some(u) => {
                let p = resolve_prefix(scope, &mut decls, u, a.prefix.as_deref(), true);
                format!("{}:{}", p.unwrap_or_default(), a.local)
            }
            None => a.local.clone(),
        };
        attr_names.push(name);
    }
    out.push('<');
    if let Some(p) = &tag_prefix {
        out.push_str(p);
        out.push(':');
    }
    out.push_str(&e.local);
    for (p, u) in &decls {
        match p {
            Some(p) => {
                out.push_str(" xmlns:");
                out.push_str(p);
            }
            None => out.push_str(" xmlns"),
        }
        out.push_str("=\"");
        escape_attr(u, out, ascii)?;
        out.push('"');
    }
    for (a, name) in e.attrs.iter().zip(&attr_names) {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        escape_attr(&a.value, out, ascii)?;
        out.push('"');
    }
    if e.text.is_none() && e.children.is_empty() {
        out.push_str("/>");
    } else {
        out.push('>');
        if let Some(t) = &e.text {
            escape_text(t, out, ascii)?;
        }
        for c in &e.children {
            write_node(c, out, scope, ascii, true)?;
        }
        out.push_str("</");
        if let Some(p) = &tag_prefix {
            out.push_str(p);
            out.push(':');
        }
        out.push_str(&e.local);
        out.push('>');
    }
    scope.truncate(mark);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECLS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:m="http://schemas.openxmlformats.org/officeDocument/2006/math" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture" xmlns:ct="http://schemas.openxmlformats.org/package/2006/content-types" xmlns:rel="http://schemas.openxmlformats.org/package/2006/relationships""#;

    #[test]
    fn matches_lxml_output() {
        let mut body = el("w:body", &[]);
        body.append(el("w:p", &[]));
        let mut d = el("w:document", &[]);
        d.append(body);
        assert_eq!(
            String::from_utf8(serialize(&d).unwrap()).unwrap(),
            format!("<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n<w:document {DECLS}><w:body><w:p/></w:body></w:document>")
        );
        let mut r = el("w:r", &[]);
        let t = sub(&mut r, "w:t", &[]);
        t.text = Some("a < b & \"c\" 'd' >".into());
        preserve_space(t);
        sub(&mut r, "w:x", &[("w:val", "1"), ("plain", "q\"<&>\n\t")]);
        assert_eq!(
            String::from_utf8(serialize(&r).unwrap()).unwrap(),
            format!("<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n<w:r {DECLS}><w:t xml:space=\"preserve\">a &lt; b &amp; \"c\" 'd' &gt;</w:t><w:x w:val=\"1\" plain=\"q&quot;&lt;&amp;&gt;&#10;&#9;\"/></w:r>")
        );
        let mut e = el("w:a", &[]);
        e.text = Some(String::new());
        assert!(String::from_utf8(serialize(&e).unwrap())
            .unwrap()
            .ends_with("></w:a>"));
        e.text = Some("\u{c}".into());
        assert!(serialize(&e).is_err());
        assert_eq!(
            xml_safe("a\u{0}b\u{8}\t\n\r\u{b}\u{c}\u{e}\u{1f} \u{fffe}\u{ffff}\u{fffd}c"),
            "ab\t\n\r \u{fffd}c"
        );
        assert_eq!(text_el("w:t", "x\u{1}y", &[]).text.as_deref(), Some("xy"));
        let mut x = Element::new("a");
        x.text = Some("é\r".into());
        assert_eq!(
            tostring(&x, ToString::default()).unwrap(),
            "<a>&#233;&#13;</a>"
        );
    }
}
