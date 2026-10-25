//! An XML parser producing the [`Element`] tree as `lxml.etree.fromstring`
//! does: whitespace text kept, CDATA merged into text, line endings and
//! attribute whitespace normalized (XML 1.0 §2.11, §3.3.3), comments and
//! processing instructions inside the root kept as nodes, every namespace
//! declaration kept on the element that makes it.

use crate::xml::{Attr, Element, Kind, XmlError, XML_NS};

fn err(msg: impl Into<String>) -> XmlError {
    XmlError(msg.into())
}

struct P<'a> {
    s: &'a str,
    i: usize,
}

impl<'a> P<'a> {
    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.rest().starts_with(lit) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.rest().chars().next() {
            if matches!(c, ' ' | '\t' | '\n' | '\r') {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    fn until(&mut self, end: &str) -> Result<&'a str, XmlError> {
        match self.rest().find(end) {
            Some(k) => {
                let out = &self.rest()[..k];
                self.i += k + end.len();
                Ok(out)
            }
            None => Err(err(format!("unterminated construct, expected {end:?}"))),
        }
    }

    fn name(&mut self) -> Result<&'a str, XmlError> {
        let start = self.i;
        for (k, c) in self.rest().char_indices() {
            let ok = c.is_alphanumeric() || matches!(c, '_' | ':' | '-' | '.') || (c as u32) >= 0x80;
            if !ok {
                self.i = start + k;
                break;
            }
            self.i = start + k + c.len_utf8();
        }
        if self.i == start {
            return Err(err("expected a name"));
        }
        Ok(&self.s[start..self.i])
    }
}

/// Decode entity and character references in `raw` (after newline
/// normalization); `attr` also maps literal whitespace to spaces.
fn decode(raw: &str, attr: bool) -> Result<String, XmlError> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '\r' => {
                // \r\n and lone \r -> \n
                if let Some((_, '\n')) = chars.peek() {
                    chars.next();
                }
                out.push(if attr { ' ' } else { '\n' });
            }
            '\n' | '\t' if attr => out.push(' '),
            '&' => {
                let rest = &raw[i + 1..];
                let end = rest.find(';').ok_or_else(|| err("unterminated entity reference"))?;
                let name = &rest[..end];
                let ch = match name {
                    "lt" => '<',
                    "gt" => '>',
                    "amp" => '&',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => {
                        let cp = if let Some(h) = name.strip_prefix("#x") {
                            u32::from_str_radix(h, 16).ok()
                        } else if let Some(d) = name.strip_prefix('#') {
                            d.parse::<u32>().ok()
                        } else {
                            return Err(err(format!("undefined entity &{name};")));
                        };
                        cp.and_then(char::from_u32).ok_or_else(|| err(format!("invalid character reference &{name};")))?
                    }
                };
                out.push(ch);
                for _ in 0..name.chars().count() + 1 {
                    chars.next();
                }
            }
            '<' if attr => return Err(err("'<' in attribute value")),
            c => out.push(c),
        }
    }
    Ok(out)
}

type Scope = Vec<(Option<String>, String)>;

fn resolve(scope: &Scope, prefix: Option<&str>) -> Option<String> {
    if prefix == Some("xml") {
        return Some(XML_NS.to_string());
    }
    scope
        .iter()
        .rev()
        .find(|(p, _)| p.as_deref() == prefix)
        .map(|(_, u)| u.clone())
        .filter(|u| !u.is_empty())
}

/// `etree.fromstring(data)`: parse a document and return its root element.
pub fn fromstring(data: &[u8]) -> Result<Element, XmlError> {
    let text = std::str::from_utf8(data).map_err(|e| err(format!("invalid UTF-8: {e}")))?;
    parse_str(text)
}

/// Parse a document held in a string.
pub fn parse_str(text: &str) -> Result<Element, XmlError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut p = P { s: text, i: 0 };
    // prolog
    loop {
        p.skip_ws();
        if p.eat("<?") {
            p.until("?>")?;
        } else if p.eat("<!--") {
            p.until("-->")?;
        } else if p.rest().starts_with("<!DOCTYPE") {
            skip_doctype(&mut p)?;
        } else {
            break;
        }
    }
    if !p.rest().starts_with('<') {
        return Err(err("Start tag expected, '<' not found"));
    }
    let mut scope: Scope = Vec::new();
    let root = parse_element(&mut p, &mut scope)?;
    // epilog: only comments/PIs/whitespace allowed
    loop {
        p.skip_ws();
        if p.eat("<?") {
            p.until("?>")?;
        } else if p.eat("<!--") {
            p.until("-->")?;
        } else if p.rest().is_empty() {
            break;
        } else {
            return Err(err("Extra content at the end of the document"));
        }
    }
    Ok(root)
}

fn skip_doctype(p: &mut P) -> Result<(), XmlError> {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    for (k, c) in p.rest().char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '[') => depth += 1,
            (None, ']') => depth -= 1,
            (None, '>') if depth == 0 => {
                p.i += k + 1;
                return Ok(());
            }
            _ => {}
        }
    }
    Err(err("unterminated DOCTYPE"))
}

fn parse_element(p: &mut P, scope: &mut Scope) -> Result<Element, XmlError> {
    if !p.eat("<") {
        return Err(err("expected '<'"));
    }
    let qname = p.name()?.to_string();
    let mut raw_attrs: Vec<(String, String)> = Vec::new();
    loop {
        p.skip_ws();
        if p.eat("/>") || p.rest().starts_with('>') {
            break;
        }
        let name = p.name()?.to_string();
        p.skip_ws();
        if !p.eat("=") {
            return Err(err(format!("attribute {name} without value")));
        }
        p.skip_ws();
        let q = p.rest().chars().next().ok_or_else(|| err("unexpected end"))?;
        if q != '"' && q != '\'' {
            return Err(err("AttValue: \" or ' expected"));
        }
        p.i += 1;
        let raw = p.until(&q.to_string())?;
        if raw_attrs.iter().any(|(n, _)| *n == name) {
            return Err(err(format!("Attribute {name} redefined")));
        }
        raw_attrs.push((name, decode(raw, true)?));
    }
    let self_closing = p.s[..p.i].ends_with("/>");
    if !self_closing && !p.eat(">") {
        return Err(err("expected '>'"));
    }
    let mark = scope.len();
    let mut nsdefs = Vec::new();
    for (n, v) in &raw_attrs {
        if n == "xmlns" {
            nsdefs.push((None, v.clone()));
        } else if let Some(pfx) = n.strip_prefix("xmlns:") {
            nsdefs.push((Some(pfx.to_string()), v.clone()));
        }
    }
    scope.extend(nsdefs.iter().cloned());
    let (prefix, local) = match qname.split_once(':') {
        Some((pf, l)) => (Some(pf.to_string()), l.to_string()),
        None => (None, qname.clone()),
    };
    let ns = resolve(scope, prefix.as_deref());
    if prefix.is_some() && ns.is_none() {
        return Err(err(format!("Namespace prefix {} on {local} is not defined", prefix.unwrap())));
    }
    let mut attrs = Vec::new();
    for (n, v) in raw_attrs {
        if n == "xmlns" || n.starts_with("xmlns:") {
            continue;
        }
        let (apfx, alocal) = match n.split_once(':') {
            Some((pf, l)) => (Some(pf.to_string()), l.to_string()),
            None => (None, n),
        };
        let ans = match &apfx {
            Some(pf) => Some(resolve(scope, Some(pf)).ok_or_else(|| err(format!("Namespace prefix {pf} not defined")))?),
            None => None,
        };
        attrs.push(Attr {
            ns: ans,
            local: alocal,
            prefix: apfx,
            value: v,
        });
    }
    let mut e = Element {
        kind: Kind::Element,
        ns,
        local,
        prefix,
        nsdefs,
        strip_redundant: false,
        attrs,
        text: None,
        tail: None,
        children: Vec::new(),
    };
    if !self_closing {
        parse_content(p, scope, &mut e, &qname)?;
    }
    scope.truncate(mark);
    Ok(e)
}

/// Append character data to the element's text or its last child's tail.
fn push_text(e: &mut Element, s: &str) {
    if s.is_empty() {
        return;
    }
    let slot = match e.children.last_mut() {
        Some(last) => &mut last.tail,
        None => &mut e.text,
    };
    slot.get_or_insert_with(String::new).push_str(s);
}

fn parse_content(p: &mut P, scope: &mut Scope, e: &mut Element, qname: &str) -> Result<(), XmlError> {
    loop {
        let rest = p.rest();
        if rest.is_empty() {
            return Err(err(format!("Premature end of data in tag {qname}")));
        }
        if rest.starts_with("</") {
            p.i += 2;
            let end = p.name()?;
            if end != qname {
                return Err(err(format!("Opening and ending tag mismatch: {qname} and {end}")));
            }
            p.skip_ws();
            if !p.eat(">") {
                return Err(err("expected '>'"));
            }
            return Ok(());
        }
        if rest.starts_with("<!--") {
            p.i += 4;
            let body = p.until("-->")?;
            e.children.push(Element::comment(body));
        } else if rest.starts_with("<![CDATA[") {
            p.i += 9;
            let body = p.until("]]>")?;
            push_text(e, &body.replace("\r\n", "\n").replace('\r', "\n"));
        } else if rest.starts_with("<?") {
            p.i += 2;
            let target = p.name()?.to_string();
            let body = p.until("?>")?;
            let data = body.trim_start_matches([' ', '\t', '\n', '\r']);
            e.children.push(Element::pi(&target, data));
        } else if rest.starts_with('<') {
            let child = parse_element(p, scope)?;
            e.children.push(child);
        } else {
            let k = rest.find('<').unwrap_or(rest.len());
            let raw = &rest[..k];
            p.i += k;
            push_text(e, &decode(raw, false)?);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::{tostring, ToString};

    #[test]
    fn round_trips_like_lxml() {
        let src = "<?xml version=\"1.0\"?>\n<!-- c --><r xmlns=\"urn:d\" xmlns:w=\"urn:w\"><w:a xmlns:w=\"urn:w\" w:x=\"1&#10;2\n3\t4\">t&amp;<![CDATA[<c>]]>\r\nu</w:a><?pi data?><!--k-->tail<b/></r>";
        let r = parse_str(src).unwrap();
        let opts = ToString { utf8: true, declaration: true, with_tail: true };
        assert_eq!(
            tostring(&r, opts).unwrap(),
            "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n<r xmlns=\"urn:d\" xmlns:w=\"urn:w\"><w:a xmlns:w=\"urn:w\" w:x=\"1&#10;2 3 4\">t&amp;&lt;c&gt;\nu</w:a><?pi data?><!--k-->tail<b/></r>"
        );
        assert_eq!(r.children[0].get("{urn:w}x"), Some("1\n2 3 4"));
        let mut r = r;
        let mut n = Element::new("{urn:w}new");
        n.set("{urn:w}v", "1");
        r.append(n);
        r.append(Element::new("{urn:zz}q"));
        assert!(tostring(&r, opts).unwrap().ends_with("<w:new w:v=\"1\"/><ns0:q xmlns:ns0=\"urn:zz\"/></r>"));
    }
}
