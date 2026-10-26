//! An XML parser producing the [`Element`] tree as `lxml.etree.fromstring`
//! does, and failing with lxml's `XMLSyntaxError` text.
//!
//! The control flow is a port of libxml2 2.14's `parser.c` (the version lxml
//! 6.0 bundles), working on bytes with libxml2's cursor, line and column
//! bookkeeping, so that a malformed document fails with the same first error
//! at the same position: `"<message>, line L, column C (<string>, line L)"`.
//! libxml2 keeps going after an error, but lxml reports the first one, so
//! this parser stops there. Warnings are not errors and are ignored.
//!
//! Tree building follows libxml2's SAX2 + lxml: whitespace text kept, CDATA
//! merged into text, line endings and attribute whitespace normalized,
//! comments and processing instructions inside the root kept as nodes,
//! every namespace declaration kept on the element that makes it.
//! Internal DTD subsets are skipped (entity declarations aren't expanded).

use crate::xml::{Attr, Element, Kind, XmlError, XML_NS};

const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";
/// libxml2's depth limit without `XML_PARSE_HUGE`
const MAX_DEPTH: usize = 256;
/// zero bytes after the input, standing in for libxml2's NUL terminator
const PAD: usize = 16;

/// The first error: libxml2's message (with its trailing newline) and the
/// input line/column when it was raised.
struct Stop {
    msg: String,
    line: i64,
    col: i64,
}

type R<T> = Result<T, Stop>;

impl Stop {
    /// lxml's `str(XMLSyntaxError)`.
    fn lxml_message(&self) -> String {
        let msg = self.msg.strip_suffix('\n').unwrap_or(&self.msg);
        let mut out = msg.to_string();
        if self.line > 0 {
            if self.col > 0 {
                out.push_str(&format!(", line {}, column {}", self.line, self.col));
            } else {
                out.push_str(&format!(", line {}", self.line));
            }
        }
        out.push_str(&format!(" (<string>, line {})", self.line));
        out
    }
}

fn is_blank(c: u8) -> bool {
    matches!(c, 0x20 | 0x09 | 0x0A | 0x0D)
}

fn is_byte_char(c: u8) -> bool {
    c >= 0x20 || matches!(c, 0x09 | 0x0A | 0x0D)
}

/// `IS_CHAR`: the XML 1.0 `Char` production.
fn is_char(c: i32) -> bool {
    matches!(c, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

fn is_ascii_letter(c: u8) -> bool {
    c.is_ascii_alphabetic()
}

/// `xmlIsNameStartChar` (XML 1.0 5th edition rules).
fn is_name_start(c: i32) -> bool {
    c != ' ' as i32
        && c != '>' as i32
        && c != '/' as i32
        && (matches!(c, 0x61..=0x7A | 0x41..=0x5A)
            || c == '_' as i32
            || c == ':' as i32
            || matches!(
                c,
                0xC0..=0xD6
                    | 0xD8..=0xF6
                    | 0xF8..=0x2FF
                    | 0x370..=0x37D
                    | 0x37F..=0x1FFF
                    | 0x200C..=0x200D
                    | 0x2070..=0x218F
                    | 0x2C00..=0x2FEF
                    | 0x3001..=0xD7FF
                    | 0xF900..=0xFDCF
                    | 0xFDF0..=0xFFFD
                    | 0x10000..=0xEFFFF
            ))
}

/// `xmlIsNameChar`.
fn is_name_char(c: i32) -> bool {
    c != ' ' as i32
        && c != '>' as i32
        && c != '/' as i32
        && (is_name_start(c)
            || matches!(c, 0x30..=0x39)
            || c == '-' as i32
            || c == '.' as i32
            || c == 0xB7
            || matches!(c, 0x300..=0x36F | 0x203F..=0x2040))
}

/// `%.50s`: at most 50 bytes.
fn trunc50(b: &[u8]) -> String {
    String::from_utf8_lossy(&b[..b.len().min(50)]).into_owned()
}

fn push_char(buf: &mut Vec<u8>, c: i32) {
    let ch = char::from_u32(c as u32).unwrap_or('\u{FFFD}');
    let mut tmp = [0u8; 4];
    buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
}

/// An open element: the node being built and what its end tag needs.
struct Frame {
    elem: Element,
    local: String,
    prefix: Option<String>,
    line: i64,
    nb_ns: usize,
}

struct Parser {
    b: Vec<u8>,
    cur: usize,
    end: usize,
    line: i64,
    col: i64,
    /// declared namespaces: (prefix, uri, id of the declaring element)
    ns: Vec<(Option<String>, String, usize)>,
    element_id: usize,
    stack: Vec<Frame>,
    root: Option<Element>,
    /// an encoding was fixed by a BOM
    has_encoding: bool,
    /// `ctxt->encoding` is set (an encoding declaration was read)
    declared_encoding: bool,
}

/// `etree.fromstring(data)`: parse a document and return its root element.
pub fn fromstring(data: &[u8]) -> Result<Element, XmlError> {
    let mut p = Parser::new(data);
    match p.document() {
        Ok(()) => Ok(p.root.expect("a parsed document has a root")),
        Err(stop) => Err(XmlError(stop.lxml_message())),
    }
}

/// Parse a document held in a string.
pub fn parse_str(text: &str) -> Result<Element, XmlError> {
    fromstring(text.as_bytes())
}

impl Parser {
    fn new(data: &[u8]) -> Parser {
        let mut b = Vec::with_capacity(data.len() + PAD);
        b.extend_from_slice(data);
        b.extend_from_slice(&[0; PAD]);
        Parser {
            b,
            cur: 0,
            end: data.len(),
            line: 1,
            col: 1,
            ns: Vec::new(),
            element_id: 0,
            stack: Vec::new(),
            root: None,
            has_encoding: false,
            declared_encoding: false,
        }
    }

    // ---- errors ----------------------------------------------------------

    fn stop(&self, msg: impl Into<String>) -> Stop {
        Stop {
            msg: msg.into(),
            line: self.line,
            col: self.col,
        }
    }

    /// `xmlFatalErr(ctxt, code, NULL)`: the code's standard message.
    fn fatal<T>(&self, errmsg: &str) -> R<T> {
        Err(self.stop(format!("{errmsg}\n")))
    }

    /// `xmlFatalErr(ctxt, code, info)`.
    fn fatal_info<T>(&self, errmsg: &str, info: &str) -> R<T> {
        Err(self.stop(format!("{errmsg}: {info}\n")))
    }

    /// Any error with a preformatted message (fatal or namespace error).
    fn err<T>(&self, msg: impl Into<String>) -> R<T> {
        Err(self.stop(msg))
    }

    fn encoding_error<T>(&self) -> R<T> {
        self.fatal("Invalid bytes in character encoding")
    }

    // ---- cursor macros ---------------------------------------------------

    fn raw(&self) -> u8 {
        self.b[self.cur]
    }

    fn nxt(&self, n: usize) -> u8 {
        self.b.get(self.cur + n).copied().unwrap_or(0)
    }

    fn cmp(&self, s: &[u8]) -> bool {
        self.b[self.cur..].starts_with(s)
    }

    /// `SKIP(n)`: ASCII without newlines.
    fn skip(&mut self, n: usize) {
        self.cur += n;
        self.col += n as i64;
    }

    /// `NEXT1`.
    fn next1(&mut self) {
        self.col += 1;
        self.cur += 1;
    }

    /// `NEXTL(l)`.
    fn nextl(&mut self, l: usize) {
        if self.b[self.cur] == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        self.cur += l;
    }

    /// `xmlNextChar`.
    fn next(&mut self) -> R<()> {
        if self.cur >= self.end {
            return Ok(());
        }
        let avail = self.end - self.cur;
        let c = self.b[self.cur];
        if c < 0x80 {
            if c == b'\n' {
                self.cur += 1;
                self.line += 1;
                self.col = 1;
            } else if c == b'\r' {
                self.cur += if self.b[self.cur + 1] == b'\n' { 2 } else { 1 };
                self.line += 1;
                self.col = 1;
            } else {
                self.cur += 1;
                self.col += 1;
            }
            return Ok(());
        }
        self.col += 1;
        let c1 = self.b[self.cur + 1];
        if avail < 2 || (c1 & 0xC0) != 0x80 {
            return self.encoding_error();
        }
        if c < 0xE0 {
            if c < 0xC2 {
                return self.encoding_error();
            }
            self.cur += 2;
        } else {
            let val = ((c as u32) << 8) | c1 as u32;
            if avail < 3 || (self.b[self.cur + 2] & 0xC0) != 0x80 {
                return self.encoding_error();
            }
            if c < 0xF0 {
                if val < 0xE0A0 || (0xEDA0..0xEE00).contains(&val) {
                    return self.encoding_error();
                }
                self.cur += 3;
            } else {
                if avail < 4 || (self.b[self.cur + 3] & 0xC0) != 0x80 {
                    return self.encoding_error();
                }
                if !(0xF090..0xF490).contains(&val) {
                    return self.encoding_error();
                }
                self.cur += 4;
            }
        }
        Ok(())
    }

    /// `xmlCurrentChar`: the code point at the cursor and its byte length.
    /// Like libxml2 it steps over the `\r` of a `\r\n` pair.
    fn current_char(&mut self) -> R<(i32, usize)> {
        let avail = self.end.saturating_sub(self.cur);
        let c = self.b[self.cur];
        if c < 0x80 {
            if c < 0x20 {
                if c == b'\r' {
                    if self.b[self.cur + 1] == b'\n' {
                        self.cur += 1;
                    }
                    return Ok((b'\n' as i32, 1));
                }
                if c == 0 {
                    if self.cur >= self.end {
                        return Ok((0, 0));
                    }
                    return self.fatal_info("Invalid character", "Char 0x0 out of allowed range\n");
                }
            }
            return Ok((c as i32, 1));
        }
        if avail < 2 {
            return Ok((0, 0));
        }
        let c1 = self.b[self.cur + 1];
        if (c1 & 0xC0) != 0x80 {
            return self.encoding_error();
        }
        if c < 0xE0 {
            if c < 0xC2 {
                return self.encoding_error();
            }
            let val = ((c as i32 & 0x1F) << 6) | (c1 as i32 & 0x3F);
            return Ok((val, 2));
        }
        if avail < 3 {
            return Ok((0, 0));
        }
        let c2 = self.b[self.cur + 2];
        if (c2 & 0xC0) != 0x80 {
            return self.encoding_error();
        }
        if c < 0xF0 {
            let val = ((c as i32 & 0xF) << 12) | ((c1 as i32 & 0x3F) << 6) | (c2 as i32 & 0x3F);
            if val < 0x800 || (0xD800..0xE000).contains(&val) {
                return self.encoding_error();
            }
            return Ok((val, 3));
        }
        if avail < 4 {
            return Ok((0, 0));
        }
        let c3 = self.b[self.cur + 3];
        if (c3 & 0xC0) != 0x80 {
            return self.encoding_error();
        }
        let val = ((c as i32 & 0x0F) << 18)
            | ((c1 as i32 & 0x3F) << 12)
            | ((c2 as i32 & 0x3F) << 6)
            | (c3 as i32 & 0x3F);
        if !(0x10000..0x110000).contains(&val) {
            return self.encoding_error();
        }
        Ok((val, 4))
    }

    /// `xmlSkipBlankChars`: returns how many were skipped.
    fn skip_blanks(&mut self) -> usize {
        let mut n = 0;
        while is_blank(self.b[self.cur]) {
            if self.b[self.cur] == b'\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            self.cur += 1;
            n += 1;
        }
        n
    }

    fn slice(&self, from: usize, to: usize) -> String {
        String::from_utf8_lossy(&self.b[from..to]).into_owned()
    }

    // ---- tree building (SAX2) ---------------------------------------------

    fn characters(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if let Some(top) = self.stack.last_mut() {
            let e = &mut top.elem;
            let slot = match e.children.last_mut() {
                Some(last) => &mut last.tail,
                None => &mut e.text,
            };
            slot.get_or_insert_with(String::new).push_str(s);
        }
    }

    fn add_node(&mut self, node: Element) {
        if let Some(top) = self.stack.last_mut() {
            top.elem.children.push(node);
        }
    }

    fn close_element(&mut self) {
        let frame = self.stack.pop().expect("an open element");
        for _ in 0..frame.nb_ns {
            self.ns.pop();
        }
        match self.stack.last_mut() {
            Some(parent) => parent.elem.children.push(frame.elem),
            None => self.root = Some(frame.elem),
        }
    }

    // ---- names ------------------------------------------------------------

    /// `xmlParseName`.
    fn parse_name(&mut self) -> R<Option<String>> {
        let start = self.cur;
        let mut i = start;
        let c = self.b[i];
        if is_ascii_letter(c) || c == b'_' || c == b':' {
            i += 1;
            while {
                let c = self.b[i];
                c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b':' | b'.')
            } {
                i += 1;
            }
            let c = self.b[i];
            if c > 0 && c < 0x80 {
                let count = i - start;
                self.cur = i;
                self.col += count as i64;
                return Ok(Some(self.slice(start, i)));
            }
        }
        self.parse_name_complex()
    }

    fn parse_name_complex(&mut self) -> R<Option<String>> {
        let mut len = 0;
        let (mut c, mut l) = self.current_char()?;
        if !is_name_start(c) {
            return Ok(None);
        }
        len += l;
        self.nextl(l);
        (c, l) = self.current_char()?;
        while is_name_char(c) {
            len += l;
            self.nextl(l);
            (c, l) = self.current_char()?;
        }
        let end = if self.b[self.cur] == b'\n' && self.cur > 0 && self.b[self.cur - 1] == b'\r' {
            self.cur - 1
        } else {
            self.cur
        };
        Ok(Some(self.slice(end - len, end)))
    }

    /// `xmlParseNCName`.
    fn parse_ncname(&mut self) -> R<Option<String>> {
        let start = self.cur;
        let mut i = start;
        let e = self.end;
        let c = self.b[i];
        if (is_ascii_letter(c) || c == b'_') && i < e {
            i += 1;
            while {
                let c = self.b[i];
                (c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')) && i < e
            } {
                i += 1;
            }
            if i < e {
                let c = self.b[i];
                if c > 0 && c < 0x80 {
                    let count = i - start;
                    self.cur = i;
                    self.col += count as i64;
                    return Ok(Some(self.slice(start, i)));
                }
            }
        }
        self.parse_ncname_complex()
    }

    fn parse_ncname_complex(&mut self) -> R<Option<String>> {
        let start = self.cur;
        let mut len = 0;
        let (mut c, mut l) = self.current_char()?;
        if !is_name_start(c) || c == ':' as i32 {
            return Ok(None);
        }
        while is_name_char(c) && c != ':' as i32 {
            len += l;
            self.nextl(l);
            (c, l) = self.current_char()?;
        }
        Ok(Some(self.slice(start, start + len)))
    }

    /// `xmlParseNmtoken` (only its cursor movement matters here).
    fn parse_nmtoken(&mut self) -> R<()> {
        let (mut c, mut l) = self.current_char()?;
        while is_name_char(c) {
            self.nextl(l);
            (c, l) = self.current_char()?;
        }
        Ok(())
    }

    /// `xmlParseQNameHashed`: (local name, prefix).
    fn parse_qname(&mut self) -> R<(Option<String>, Option<String>)> {
        let start = self.cur;
        let mut prefix = None;
        let mut local = self.parse_ncname()?;
        let mut is_ncname = false;
        if local.is_some() {
            is_ncname = true;
            if self.raw() == b':' {
                self.next()?;
                prefix = local;
                local = self.parse_ncname()?;
            }
        }
        if local.is_none() || self.raw() == b':' {
            if !is_ncname && self.raw() != b':' {
                return Ok((None, None));
            }
            self.parse_nmtoken()?;
            let name = self.slice(start, self.cur);
            return self.err(format!("Failed to parse QName '{name}'\n"));
        }
        Ok((local, prefix))
    }

    /// The name in an end tag matches `other` (`xmlParseNameAndCompare`):
    /// `Ok(None)` on a match, else the name found (`"unparsable"` if none).
    fn parse_name_and_compare(&mut self, other: &str) -> R<Option<String>> {
        let o = other.as_bytes();
        let mut i = self.cur;
        let mut k = 0;
        while self.b[i] != 0 && k < o.len() && self.b[i] == o[k] {
            i += 1;
            k += 1;
        }
        if k == o.len() && (self.b[i] == b'>' || is_blank(self.b[i])) {
            self.col += (i - self.cur) as i64;
            self.cur = i;
            return Ok(None);
        }
        match self.parse_name()? {
            Some(n) if n == other => Ok(None),
            Some(n) => Ok(Some(n)),
            None => Ok(Some("unparsable".into())),
        }
    }

    /// `xmlParseQNameAndCompare`.
    fn parse_qname_and_compare(&mut self, name: &str, prefix: &str) -> R<Option<String>> {
        let mut i = self.cur;
        let p = prefix.as_bytes();
        let mut k = 0;
        while self.b[i] != 0 && k < p.len() && self.b[i] == p[k] {
            i += 1;
            k += 1;
        }
        if k == p.len() && self.b[i] == b':' {
            i += 1;
            let n = name.as_bytes();
            let mut k = 0;
            while self.b[i] != 0 && k < n.len() && self.b[i] == n[k] {
                i += 1;
                k += 1;
            }
            if k == n.len() && (self.b[i] == b'>' || is_blank(self.b[i])) {
                self.col += (i - self.cur) as i64;
                self.cur = i;
                return Ok(None);
            }
        }
        match self.parse_qname()? {
            (None, _) => Ok(Some("unparsable".into())),
            (Some(l), p2) => {
                if l == name && p2.as_deref() == Some(prefix) {
                    Ok(None)
                } else {
                    Ok(Some(l))
                }
            }
        }
    }

    // ---- references ---------------------------------------------------------

    /// `xmlParseCharRef`.
    fn parse_char_ref(&mut self) -> R<i32> {
        let mut val: i32 = 0;
        let mut count = 0;
        if self.raw() == b'&' && self.nxt(1) == b'#' && self.nxt(2) == b'x' {
            self.skip(3);
            while self.raw() != b';' {
                let was = count;
                count += 1;
                if was > 20 {
                    count = 0;
                }
                let c = self.raw();
                if c.is_ascii_digit() {
                    val = val * 16 + (c - b'0') as i32;
                } else if (b'a'..=b'f').contains(&c) && count < 20 {
                    val = val * 16 + (c - b'a') as i32 + 10;
                } else if (b'A'..=b'F').contains(&c) && count < 20 {
                    val = val * 16 + (c - b'A') as i32 + 10;
                } else {
                    return self.fatal("CharRef: invalid hexadecimal value");
                }
                if val > 0x110000 {
                    val = 0x110000;
                }
                self.next()?;
                count += 1;
            }
            if self.raw() == b';' {
                self.col += 1;
                self.cur += 1;
            }
        } else if self.raw() == b'&' && self.nxt(1) == b'#' {
            self.skip(2);
            while self.raw() != b';' {
                let was = count;
                count += 1;
                if was > 20 {
                    count = 0;
                }
                let c = self.raw();
                if c.is_ascii_digit() {
                    val = val * 10 + (c - b'0') as i32;
                } else {
                    return self.fatal("CharRef: invalid decimal value");
                }
                if val > 0x110000 {
                    val = 0x110000;
                }
                self.next()?;
                count += 1;
            }
            if self.raw() == b';' {
                self.col += 1;
                self.cur += 1;
            }
        } else {
            if self.raw() == b'&' {
                self.skip(1);
            }
            return self.fatal("CharRef: invalid value");
        }
        if val >= 0x110000 {
            return self.err("xmlParseCharRef: character reference out of bounds\n");
        }
        if !is_char(val) {
            return self.err(format!("xmlParseCharRef: invalid xmlChar value {val}\n"));
        }
        Ok(val)
    }

    /// `xmlParseEntityRefInternal` + `xmlLookupGeneralEntity`: the text of
    /// a predefined entity (no others are declared).
    fn parse_entity_ref(&mut self) -> R<&'static str> {
        self.next()?; // '&'
        let Some(name) = self.parse_name()? else {
            return self.err("xmlParseEntityRef: no name\n");
        };
        if self.raw() != b';' {
            return self.fatal("EntityRef: expecting ';'");
        }
        self.next()?;
        match name.as_str() {
            "lt" => Ok("<"),
            "gt" => Ok(">"),
            "amp" => Ok("&"),
            "apos" => Ok("'"),
            "quot" => Ok("\""),
            _ => self.err(format!("Entity '{name}' not defined\n")),
        }
    }

    /// `xmlParseReference` in content.
    fn parse_reference(&mut self) -> R<()> {
        if self.nxt(1) == b'#' {
            let v = self.parse_char_ref()?;
            let mut buf = Vec::new();
            push_char(&mut buf, v);
            self.characters(&String::from_utf8_lossy(&buf));
            return Ok(());
        }
        let text = self.parse_entity_ref()?;
        self.characters(text);
        Ok(())
    }

    // ---- attributes -----------------------------------------------------------

    /// `xmlUTF8MultibyteLen`.
    fn utf8_multibyte_len(&self, err_msg: &str) -> R<usize> {
        let s = &self.b[self.cur..];
        let (c, c1) = (s[0], s[1]);
        if (c1 & 0xC0) != 0x80 {
            return self.encoding_error();
        }
        if c < 0xE0 {
            if c < 0xC2 {
                return self.encoding_error();
            }
            return Ok(2);
        }
        let c2 = s[2];
        if (c2 & 0xC0) != 0x80 {
            return self.encoding_error();
        }
        if c < 0xF0 {
            if (c == 0xE0 && c1 < 0xA0) || (c == 0xED && c1 >= 0xA0) {
                return self.encoding_error();
            }
            if c == 0xEF && c1 == 0xBF && c2 >= 0xBE {
                return self.err(err_msg.to_string());
            }
            return Ok(3);
        }
        if (s[3] & 0xC0) != 0x80 {
            return self.encoding_error();
        }
        if (c == 0xF0 && c1 < 0x90) || (c >= 0xF4 && (c > 0xF4 || c1 >= 0x90)) {
            return self.encoding_error();
        }
        Ok(4)
    }

    /// `xmlParseAttValueInternal` (no DTD, so never normalized).
    fn parse_att_value(&mut self) -> R<String> {
        let quote = self.raw();
        if quote != b'"' && quote != b'\'' {
            return self.fatal("AttValue: \" or ' expected");
        }
        self.nextl(1);
        let mut buf: Vec<u8> = Vec::new();
        loop {
            if self.cur >= self.end {
                return self.err("AttValue: ' expected\n");
            }
            let c = self.raw();
            if c >= 0x80 {
                let l = self.utf8_multibyte_len("invalid character in attribute value\n")?;
                buf.extend_from_slice(&self.b[self.cur..self.cur + l]);
                self.nextl(l);
            } else if c != b'&' {
                if c > 0x20 {
                    if c == quote {
                        break;
                    }
                    if c == b'<' {
                        return self.fatal("Unescaped '<' not allowed in attributes values");
                    }
                    buf.push(c);
                } else if !is_byte_char(c) {
                    return self.err("invalid character in attribute value\n");
                } else {
                    buf.push(b' ');
                    if c == 0xD && self.nxt(1) == 0xA {
                        self.cur += 1;
                    }
                }
                self.nextl(1);
            } else if self.nxt(1) == b'#' {
                let v = self.parse_char_ref()?;
                push_char(&mut buf, v);
            } else {
                let text = self.parse_entity_ref()?;
                buf.extend_from_slice(text.as_bytes());
            }
        }
        self.nextl(1);
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// `xmlParseAttribute2`: (local name, prefix, value).
    fn parse_attribute(&mut self) -> R<(String, Option<String>, String)> {
        let (name, prefix) = self.parse_qname()?;
        let Some(name) = name else {
            return self.err("error parsing attribute name\n");
        };
        self.skip_blanks();
        if self.raw() != b'=' {
            return self.err(format!(
                "Specification mandates value for attribute {name}\n"
            ));
        }
        self.next()?;
        self.skip_blanks();
        let value = self.parse_att_value()?;
        Ok((name, prefix, value))
    }

    fn ns_lookup(&self, prefix: Option<&str>) -> Option<&(Option<String>, String, usize)> {
        self.ns
            .iter()
            .rev()
            .find(|(p, _, _)| p.as_deref() == prefix)
    }

    /// `xmlParserNsPush`: `Ok(true)` when pushed.
    fn ns_push(&mut self, prefix: Option<&str>, uri: &str) -> R<bool> {
        if prefix == Some("xml") {
            return Ok(false);
        }
        if let Some((_, _, id)) = self.ns_lookup(prefix) {
            if *id == self.element_id {
                return match prefix {
                    None => self.err("Attribute xmlns redefined\n"),
                    Some(p) => self.err(format!("Attribute xmlns:{p} redefined\n")),
                };
            }
        }
        self.ns
            .push((prefix.map(str::to_string), uri.to_string(), self.element_id));
        Ok(true)
    }

    /// `xmlParseStartTag2`: builds the element; returns it with its local
    /// name, prefix and the number of namespaces it declared.
    fn parse_start_tag(&mut self) -> R<(Element, String, Option<String>, usize)> {
        self.next1(); // '<'
        self.element_id += 1;
        let (local, prefix) = self.parse_qname()?;
        let Some(local) = local else {
            return self.err("StartTag: invalid element name\n");
        };
        self.skip_blanks();
        let mut nb_ns = 0;
        let mut nsdefs: Vec<(Option<String>, String)> = Vec::new();
        let mut atts: Vec<(String, Option<String>, String)> = Vec::new();
        while self.raw() != b'>'
            && !(self.raw() == b'/' && self.nxt(1) == b'>')
            && is_byte_char(self.raw())
        {
            let (aname, aprefix, value) = self.parse_attribute()?;
            if aname == "xmlns" && aprefix.is_none() {
                let uri = value;
                if !uri.is_empty() {
                    if uri == XML_NS {
                        return self.err("xml namespace URI cannot be the default namespace\n");
                    }
                    if uri == XMLNS_NS {
                        return self.err("reuse of the xmlns namespace name is forbidden\n");
                    }
                }
                if self.ns_push(None, &uri)? {
                    nb_ns += 1;
                    nsdefs.push((None, uri));
                }
            } else if aprefix.as_deref() == Some("xmlns") {
                let uri = value;
                if aname == "xml" {
                    if uri != XML_NS {
                        return self.err("xml namespace prefix mapped to wrong URI\n");
                    }
                } else if uri == XML_NS {
                    return self.err("xml namespace URI mapped to wrong prefix\n");
                } else if aname == "xmlns" {
                    return self.err("redefinition of the xmlns prefix is forbidden\n");
                } else if uri == XMLNS_NS {
                    return self.err("reuse of the xmlns namespace name is forbidden\n");
                } else if uri.is_empty() {
                    return self.err(format!(
                        "xmlns:{aname}: Empty XML namespace is not allowed\n"
                    ));
                } else if self.ns_push(Some(&aname), &uri)? {
                    nb_ns += 1;
                    nsdefs.push((Some(aname), uri));
                }
            } else {
                atts.push((aname, aprefix, value));
            }
            if self.raw() == b'>' || (self.raw() == b'/' && self.nxt(1) == b'>') {
                break;
            }
            if self.skip_blanks() == 0 {
                return self.err("attributes construct error\n");
            }
        }
        // resolve attribute namespaces
        let mut attrs: Vec<Attr> = Vec::with_capacity(atts.len());
        for (aname, aprefix, value) in atts {
            let ns = match aprefix.as_deref() {
                None => None,
                Some("xml") => Some(XML_NS.to_string()),
                Some(p) => match self.ns_lookup(Some(p)) {
                    Some((_, uri, _)) => Some(uri.clone()),
                    None => {
                        return self.err(format!(
                            "Namespace prefix {p} for {aname} on {local} is not defined\n"
                        ));
                    }
                },
            };
            attrs.push(Attr {
                ns,
                local: aname,
                prefix: aprefix,
                value,
            });
        }
        // unique attribute names
        for j in 1..attrs.len() {
            let a = &attrs[j];
            if let Some(prev) = attrs[..j]
                .iter()
                .find(|p| p.local == a.local && p.ns == a.ns)
            {
                if prev.prefix == a.prefix {
                    return match &a.prefix {
                        None => self.err(format!("Attribute {} redefined\n", a.local)),
                        Some(p) => self.err(format!("Attribute {p}:{} redefined\n", a.local)),
                    };
                }
                return self.err(format!(
                    "Namespaced Attribute {} in '{}' redefined\n",
                    a.local,
                    a.ns.as_deref().unwrap_or("(null)")
                ));
            }
        }
        let uri = match prefix.as_deref() {
            Some("xml") => Some(XML_NS.to_string()),
            p => self
                .ns_lookup(p)
                .map(|(_, u, _)| u.clone())
                .filter(|u| !u.is_empty()),
        };
        if let (Some(p), None) = (&prefix, &uri) {
            return self.err(format!("Namespace prefix {p} on {local} is not defined\n"));
        }
        let elem = Element {
            kind: Kind::Element,
            ns: uri,
            local: local.clone(),
            prefix: prefix.clone(),
            nsdefs,
            strip_redundant: false,
            attrs,
            text: None,
            tail: None,
            children: Vec::new(),
        };
        Ok((elem, local, prefix, nb_ns))
    }

    // ---- elements and content -------------------------------------------------

    /// `xmlParseElementStart`: `Ok(true)` for an empty element (already
    /// closed), `Ok(false)` when its content follows.
    fn parse_element_start(&mut self) -> R<bool> {
        if self.stack.len() > MAX_DEPTH {
            return self.err(format!(
                "Excessive depth in document: {} use XML_PARSE_HUGE option\n",
                self.stack.len()
            ));
        }
        let line = self.line;
        let (elem, local, prefix, nb_ns) = self.parse_start_tag()?;
        self.stack.push(Frame {
            elem,
            local: local.clone(),
            prefix,
            line,
            nb_ns,
        });
        if self.raw() == b'/' && self.nxt(1) == b'>' {
            self.skip(2);
            self.close_element();
            return Ok(true);
        }
        if self.raw() == b'>' {
            self.next1();
            return Ok(false);
        }
        self.err(format!(
            "Couldn't find end of Start Tag {local} line {line}\n"
        ))
    }

    /// `xmlParseElementEnd` / `xmlParseEndTag2`.
    fn parse_element_end(&mut self) -> R<()> {
        if self.raw() != b'<' || self.nxt(1) != b'/' {
            return self.fatal("EndTag: '</' not found");
        }
        self.skip(2);
        let (local, prefix, line) = {
            let top = self.stack.last().expect("an open element");
            (top.local.clone(), top.prefix.clone(), top.line)
        };
        let mismatch = match &prefix {
            None => self.parse_name_and_compare(&local)?,
            Some(p) => self.parse_qname_and_compare(&local, p)?,
        };
        self.skip_blanks();
        if !is_byte_char(self.raw()) || self.raw() != b'>' {
            return self.fatal("expected '>'");
        }
        self.next1();
        if let Some(name) = mismatch {
            return self.err(format!(
                "Opening and ending tag mismatch: {local} line {line} and {name}\n"
            ));
        }
        self.close_element();
        Ok(())
    }

    /// `xmlParseElement` for the root, with `xmlParseContentInternal`.
    fn parse_element(&mut self) -> R<()> {
        if self.parse_element_start()? {
            return Ok(());
        }
        let old = self.stack.len();
        while self.cur < self.end {
            let c = self.raw();
            if c == b'<' && self.nxt(1) == b'?' {
                self.parse_pi(true)?;
            } else if self.cmp(b"<![CDATA[") {
                self.parse_cdsect()?;
            } else if self.cmp(b"<!--") {
                self.parse_comment(true)?;
            } else if c == b'<' {
                if self.nxt(1) == b'/' {
                    if self.stack.len() <= old {
                        break;
                    }
                    self.parse_element_end()?;
                } else {
                    self.parse_element_start()?;
                }
            } else if c == b'&' {
                self.parse_reference()?;
            } else {
                self.parse_char_data()?;
            }
        }
        if self.cur >= self.end {
            let top = self.stack.last().expect("an open element");
            return self.err(format!(
                "Premature end of data in tag {} line {}\n",
                top.local, top.line
            ));
        }
        self.parse_element_end()
    }

    /// `xmlParseCharDataInternal` (not partial).
    fn parse_char_data(&mut self) -> R<()> {
        let mut line = self.line;
        let mut col = self.col;
        let mut i = self.cur;
        loop {
            // get_more_space
            loop {
                while self.b[i] == 0x20 {
                    i += 1;
                    self.col += 1;
                }
                if self.b[i] == 0xA {
                    while self.b[i] == 0xA {
                        self.line += 1;
                        self.col = 1;
                        i += 1;
                    }
                    continue;
                }
                break;
            }
            if self.b[i] == b'<' {
                let tmp = self.cur;
                self.cur = i;
                if i > tmp {
                    let s = self.slice(tmp, i);
                    self.characters(&s);
                }
                return Ok(());
            }
            // get_more
            loop {
                let mut ccol = self.col;
                while char_data_ok(self.b[i]) {
                    i += 1;
                    ccol += 1;
                }
                self.col = ccol;
                if self.b[i] == 0xA {
                    while self.b[i] == 0xA {
                        self.line += 1;
                        self.col = 1;
                        i += 1;
                    }
                    continue;
                }
                if self.b[i] == b']' {
                    if self.b[i + 1] == b']' && self.b[i + 2] == b'>' {
                        return self.fatal("Sequence ']]>' not allowed in content");
                    }
                    i += 1;
                    self.col += 1;
                    continue;
                }
                break;
            }
            if i > self.cur {
                let s = self.slice(self.cur, i);
                self.cur = i;
                self.characters(&s);
                line = self.line;
                col = self.col;
            }
            self.cur = i;
            if self.b[i] == 0xD && self.b[i + 1] == 0xA {
                self.cur = i + 1;
                i += 2;
                self.line += 1;
                self.col = 1;
                if char_data_continue(self.b[i]) {
                    continue;
                }
                break;
            }
            if self.b[i] == b'<' || self.b[i] == b'&' {
                return Ok(());
            }
            i = self.cur;
            if !char_data_continue(self.b[i]) {
                break;
            }
        }
        self.line = line;
        self.col = col;
        self.parse_char_data_complex()
    }

    /// `xmlParseCharDataComplex`.
    fn parse_char_data_complex(&mut self) -> R<()> {
        let mut buf: Vec<u8> = Vec::new();
        let (mut cur, mut l) = self.current_char()?;
        while cur != b'<' as i32 && cur != b'&' as i32 && is_char(cur) {
            if cur == b']' as i32 && self.nxt(1) == b']' && self.nxt(2) == b'>' {
                return self.fatal("Sequence ']]>' not allowed in content");
            }
            push_char(&mut buf, cur);
            self.nextl(l);
            (cur, l) = self.current_char()?;
        }
        if !buf.is_empty() {
            let s = String::from_utf8_lossy(&buf).into_owned();
            self.characters(&s);
        }
        if self.cur < self.end {
            if cur == 0 && self.raw() != 0 {
                return self.err(format!(
                    "Incomplete UTF-8 sequence starting with {:02X}\n",
                    self.raw()
                ));
            } else if cur != b'<' as i32 && cur != b'&' as i32 && cur != b']' as i32 {
                return self.err(format!("PCDATA invalid Char value {cur}\n"));
            }
        }
        Ok(())
    }

    /// `xmlParseCDSect` (CDATA becomes text: lxml's `strip_cdata`).
    fn parse_cdsect(&mut self) -> R<()> {
        self.skip(9);
        let (mut r, mut rl) = self.current_char()?;
        if !is_char(r) {
            return self.fatal("Unregistered error message");
        }
        self.nextl(rl);
        let (mut s, mut sl) = self.current_char()?;
        if !is_char(s) {
            return self.fatal("Unregistered error message");
        }
        self.nextl(sl);
        let (mut cur, mut l) = self.current_char()?;
        let mut buf: Vec<u8> = Vec::new();
        while is_char(cur) && (r != b']' as i32 || s != b']' as i32 || cur != b'>' as i32) {
            push_char(&mut buf, r);
            r = s;
            rl = sl;
            s = cur;
            sl = l;
            self.nextl(l);
            (cur, l) = self.current_char()?;
        }
        let _ = (rl, sl);
        if cur != b'>' as i32 {
            return self.err(format!("CData section not finished\n{}\n", trunc50(&buf)));
        }
        self.nextl(l);
        let text = String::from_utf8_lossy(&buf).into_owned();
        self.characters(&text);
        Ok(())
    }

    /// `xmlParseComment`; `keep` adds it to the tree (inside the root).
    fn parse_comment(&mut self, keep: bool) -> R<()> {
        self.skip(4);
        let mut buf: Option<Vec<u8>> = None;
        let mut i = self.cur;
        loop {
            if self.b[i] == 0xA {
                while self.b[i] == 0xA {
                    self.line += 1;
                    self.col = 1;
                    i += 1;
                }
            }
            // get_more
            'get_more: loop {
                let mut ccol = self.col;
                while {
                    let c = self.b[i];
                    (c > b'-' && c <= 0x7F) || (0x20..b'-').contains(&c) || c == 0x09
                } {
                    i += 1;
                    ccol += 1;
                }
                self.col = ccol;
                if self.b[i] == 0xA {
                    while self.b[i] == 0xA {
                        self.line += 1;
                        self.col = 1;
                        i += 1;
                    }
                    continue 'get_more;
                }
                if i > self.cur {
                    buf.get_or_insert_with(Vec::new)
                        .extend_from_slice(&self.b[self.cur..i]);
                }
                self.cur = i;
                if self.b[i] == 0xA {
                    i += 1;
                    self.line += 1;
                    self.col = 1;
                }
                if self.b[i] == 0xD && self.b[i + 1] == 0xA {
                    self.cur = i + 1;
                    i += 2;
                    self.line += 1;
                    self.col = 1;
                    continue 'get_more;
                }
                i = self.cur;
                if self.b[i] == b'-' {
                    if self.b[i + 1] == b'-' {
                        if self.b[i + 2] == b'>' {
                            self.skip(3);
                            if keep {
                                let text = buf
                                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                                    .unwrap_or_default();
                                self.add_node(Element::comment(&text));
                            }
                            return Ok(());
                        }
                        return match &buf {
                            Some(b) => self.err(format!(
                                "Double hyphen within comment: <!--{}\n",
                                trunc50(b)
                            )),
                            None => self.err("Double hyphen within comment\n"),
                        };
                    }
                    i += 1;
                    self.col += 1;
                    continue 'get_more;
                }
                break;
            }
            let c = self.b[i];
            if !((0x20..=0x7F).contains(&c) || c == 0x09 || c == 0x0A) {
                break;
            }
        }
        self.parse_comment_complex(buf.unwrap_or_default(), keep)
    }

    /// `xmlParseCommentComplex`.
    fn parse_comment_complex(&mut self, mut buf: Vec<u8>, keep: bool) -> R<()> {
        let (mut q, mut ql) = self.current_char()?;
        if q == 0 {
            return self.err("Comment not terminated\n");
        }
        if !is_char(q) {
            return self.err(format!("xmlParseComment: invalid xmlChar value {q}\n"));
        }
        self.nextl(ql);
        let (mut r, mut rl) = self.current_char()?;
        if r == 0 {
            return self.err("Comment not terminated\n");
        }
        if !is_char(r) {
            return self.err(format!("xmlParseComment: invalid xmlChar value {r}\n"));
        }
        self.nextl(rl);
        let (mut cur, mut l) = self.current_char()?;
        if cur == 0 {
            return self.err("Comment not terminated\n");
        }
        while is_char(cur) && (cur != b'>' as i32 || r != b'-' as i32 || q != b'-' as i32) {
            if r == b'-' as i32 && q == b'-' as i32 {
                return self.fatal("Comment must not contain '--' (double-hyphen)");
            }
            push_char(&mut buf, q);
            q = r;
            ql = rl;
            r = cur;
            rl = l;
            self.nextl(l);
            (cur, l) = self.current_char()?;
        }
        let _ = (ql, rl);
        if cur == 0 {
            return self.err(format!("Comment not terminated \n<!--{}\n", trunc50(&buf)));
        }
        if !is_char(cur) {
            return self.err(format!("xmlParseComment: invalid xmlChar value {cur}\n"));
        }
        self.next()?;
        if keep {
            let text = String::from_utf8_lossy(&buf).into_owned();
            self.add_node(Element::comment(&text));
        }
        Ok(())
    }

    /// `xmlParsePI`; `keep` adds it to the tree (inside the root).
    fn parse_pi(&mut self, keep: bool) -> R<()> {
        self.skip(2);
        let target = self.parse_name()?;
        if let Some(name) = &target {
            let lower = name.to_ascii_lowercase();
            if lower.starts_with("xml") {
                if name == "xml" {
                    return self.err("XML declaration allowed only at the start of the document\n");
                } else if name.len() == 3 {
                    return self.fatal("Invalid PI name");
                }
            }
            if name.contains(':') {
                return self.err(format!("colons are forbidden from PI names '{name}'\n"));
            }
        }
        let Some(target) = target else {
            return self.fatal("xmlParsePI : no target name");
        };
        if self.raw() == b'?' && self.nxt(1) == b'>' {
            self.skip(2);
            if keep {
                self.add_node(Element::pi(&target, ""));
            }
            return Ok(());
        }
        if self.skip_blanks() == 0 {
            return self.err(format!("ParsePI: PI {target} space expected\n"));
        }
        let mut buf: Vec<u8> = Vec::new();
        let (mut cur, mut l) = self.current_char()?;
        while is_char(cur) && (cur != b'?' as i32 || self.nxt(1) != b'>') {
            push_char(&mut buf, cur);
            self.nextl(l);
            (cur, l) = self.current_char()?;
        }
        if cur != b'?' as i32 {
            return self.err(format!("ParsePI: PI {target} never end ...\n"));
        }
        self.skip(2);
        if keep {
            let data = String::from_utf8_lossy(&buf).into_owned();
            self.add_node(Element::pi(&target, &data));
        }
        Ok(())
    }

    /// `xmlParseMisc`.
    fn parse_misc(&mut self) -> R<()> {
        loop {
            self.skip_blanks();
            if self.raw() == b'<' && self.nxt(1) == b'?' {
                self.parse_pi(false)?;
            } else if self.cmp(b"<!--") {
                self.parse_comment(false)?;
            } else {
                return Ok(());
            }
        }
    }

    // ---- prolog -----------------------------------------------------------------

    /// `xmlParseVersionNum`.
    fn parse_version_num(&mut self) -> R<Option<String>> {
        let start = self.cur;
        if !self.raw().is_ascii_digit() {
            return Ok(None);
        }
        self.next()?;
        if self.raw() != b'.' {
            return Ok(None);
        }
        self.next()?;
        while self.raw().is_ascii_digit() {
            self.next()?;
        }
        Ok(Some(self.slice(start, self.cur)))
    }

    /// The quoted value of a declaration pseudo-attribute, parsed by `f`.
    fn parse_quoted<T>(&mut self, f: impl FnOnce(&mut Self) -> R<T>) -> R<T> {
        let q = self.raw();
        if q == b'"' || q == b'\'' {
            self.next()?;
            let v = f(self)?;
            if self.raw() != q {
                return self.fatal("String not closed expecting \" or '");
            }
            self.next()?;
            Ok(v)
        } else {
            self.fatal("String not started expecting ' or \"")
        }
    }

    /// `xmlParseEncName`.
    fn parse_enc_name(&mut self) -> R<String> {
        let start = self.cur;
        if !is_ascii_letter(self.raw()) {
            return self.fatal("Invalid XML encoding name");
        }
        self.next()?;
        while {
            let c = self.raw();
            c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')
        } {
            self.next()?;
        }
        Ok(self.slice(start, self.cur))
    }

    /// `xmlParseXMLDecl`.
    fn parse_xml_decl(&mut self) -> R<()> {
        self.skip(5);
        self.skip_blanks();
        // VersionInfo
        let mut version = None;
        if self.cmp(b"version") {
            self.skip(7);
            self.skip_blanks();
            if self.raw() != b'=' {
                return self.fatal("expected '='");
            }
            self.next()?;
            self.skip_blanks();
            version = self.parse_quoted(|p| p.parse_version_num())?;
        }
        match version {
            None => return self.fatal("Malformed declaration expecting version"),
            Some(v) if v != "1.0" && !v.starts_with("1.") => {
                return self.err(format!("Unsupported version '{v}'\n"));
            }
            _ => {}
        }
        if !is_blank(self.raw()) {
            if self.raw() == b'?' && self.nxt(1) == b'>' {
                self.skip(2);
                return Ok(());
            }
            return self.err("Blank needed here\n");
        }
        // EncodingDecl
        self.skip_blanks();
        if self.cmp(b"encoding") {
            self.skip(8);
            self.skip_blanks();
            if self.raw() != b'=' {
                return self.fatal("expected '='");
            }
            self.next()?;
            self.skip_blanks();
            let enc = self.parse_quoted(|p| p.parse_enc_name())?;
            self.set_declared_encoding(&enc)?;
            self.declared_encoding = true;
        }
        if self.declared_encoding && !is_blank(self.raw()) {
            if self.raw() == b'?' && self.nxt(1) == b'>' {
                self.skip(2);
                return Ok(());
            }
            return self.err("Blank needed here\n");
        }
        self.skip_blanks();
        // SDDecl
        if self.cmp(b"standalone") {
            self.skip(10);
            self.skip_blanks();
            if self.raw() != b'=' {
                return self.fatal("expected '='");
            }
            self.next()?;
            self.skip_blanks();
            self.parse_quoted(|p| {
                if p.raw() == b'n' && p.nxt(1) == b'o' {
                    p.skip(2);
                } else if p.raw() == b'y' && p.nxt(1) == b'e' && p.nxt(2) == b's' {
                    p.skip(3);
                } else {
                    return p.fatal("standalone accepts only 'yes' or 'no'");
                }
                Ok(())
            })?;
        }
        self.skip_blanks();
        if self.raw() == b'?' && self.nxt(1) == b'>' {
            self.skip(2);
            return Ok(());
        }
        self.fatal("parsing XML declaration: '?>' expected")
    }

    /// Re-decode the unread input from Latin-1 (`xmlSwitchEncoding`).
    fn switch_to_latin1(&mut self) {
        let rest: String = self.b[self.cur..self.end]
            .iter()
            .map(|&c| c as char)
            .collect();
        self.b.truncate(self.cur);
        self.b.extend_from_slice(rest.as_bytes());
        self.end = self.b.len();
        self.b.extend_from_slice(&[0; PAD]);
    }

    /// `xmlSetDeclaredEncoding` for the encodings libxml2 handles itself.
    fn set_declared_encoding(&mut self, name: &str) -> R<()> {
        if self.has_encoding {
            return Ok(()); // a BOM decided (a mismatch is only a warning)
        }
        let upper = name.to_ascii_uppercase();
        match upper.as_str() {
            "UTF-8" | "UTF8" | "ASCII" | "US-ASCII" => Ok(()),
            "ISO-8859-1" | "ISO-LATIN-1" | "ISO_8859-1" | "LATIN1" | "L1" => {
                self.switch_to_latin1();
                Ok(())
            }
            _ => self.fatal_info("Unsupported encoding", name),
        }
    }

    /// `xmlDetectEncoding`: a UTF-8 BOM is skipped, UTF-16 is transcoded.
    fn detect_encoding(&mut self) {
        if self.end < 4 {
            return;
        }
        let b = &self.b;
        let (utf16, le, bom) = match (b[0], b[1], b[2], b[3]) {
            (0xEF, 0xBB, 0xBF, _) => {
                self.cur += 3;
                self.has_encoding = true;
                return;
            }
            (0xFE, 0xFF, _, _) => (true, false, 2),
            (0xFF, 0xFE, _, _) => (true, true, 2),
            (0x00, 0x3C, 0x00, 0x3F) => (true, false, 0),
            (0x3C, 0x00, 0x3F, 0x00) => (true, true, 0),
            _ => (false, false, 0),
        };
        if utf16 {
            let units: Vec<u16> = self.b[bom..self.end]
                .chunks(2)
                .map(|c| {
                    let (a, b) = (c[0], *c.get(1).unwrap_or(&0));
                    if le {
                        u16::from_le_bytes([a, b])
                    } else {
                        u16::from_be_bytes([a, b])
                    }
                })
                .collect();
            let text = String::from_utf16_lossy(&units);
            self.b = text.into_bytes();
            self.end = self.b.len();
            self.b.extend_from_slice(&[0; PAD]);
            self.has_encoding = true;
        }
    }

    /// `xmlParseExternalID` (the literals are only skipped).
    fn parse_external_id(&mut self) -> R<bool> {
        let system = self.cmp(b"SYSTEM");
        if !system && !self.cmp(b"PUBLIC") {
            return Ok(false);
        }
        self.skip(6);
        if self.skip_blanks() == 0 {
            let kw = if system { "SYSTEM" } else { "PUBLIC" };
            return self.err(format!("Space required after '{kw}'\n"));
        }
        let literal = |p: &mut Self, what: &str| -> R<()> {
            let q = p.raw();
            if q != b'"' && q != b'\'' {
                return p.fatal(what);
            }
            p.next()?;
            while p.raw() != q {
                if p.cur >= p.end {
                    return p.fatal("Unfinished System or Public ID \" or ' expected");
                }
                p.next()?;
            }
            p.next()
        };
        if !system {
            literal(self, "PUBLIC, the Public Identifier is missing")?;
            if self.skip_blanks() == 0 {
                return self.err("Space required after the Public Identifier\n");
            }
        }
        literal(self, "SYSTEM or PUBLIC, the URI is missing")?;
        Ok(true)
    }

    /// `xmlParseDocTypeDecl` + `xmlParseInternalSubset` (declarations are
    /// skipped, not interpreted).
    fn parse_doctype(&mut self) -> R<()> {
        self.skip(9);
        if self.skip_blanks() == 0 {
            return self.err("Space required after 'DOCTYPE'\n");
        }
        if self.parse_name()?.is_none() {
            return self.err("xmlParseDocTypeDecl : no DOCTYPE name !\n");
        }
        self.skip_blanks();
        self.parse_external_id()?;
        self.skip_blanks();
        if self.raw() != b'[' && self.raw() != b'>' {
            return self.fatal("DOCTYPE improperly terminated");
        }
        if self.raw() == b'[' {
            self.next()?;
            self.skip_blanks();
            while self.raw() != b']' {
                if self.raw() == b'<' && (self.nxt(1) == b'!' || self.nxt(1) == b'?') {
                    self.skip_markup_decl()?;
                } else {
                    return self.fatal("Content error in the internal subset");
                }
                self.skip_blanks();
            }
            self.next()?;
            self.skip_blanks();
        }
        if self.raw() != b'>' {
            return self.fatal("DOCTYPE improperly terminated");
        }
        self.next()
    }

    fn skip_markup_decl(&mut self) -> R<()> {
        if self.cmp(b"<!--") {
            return self.parse_comment(false);
        }
        if self.cmp(b"<?") {
            return self.parse_pi(false);
        }
        let mut quote = 0u8;
        loop {
            if self.cur >= self.end {
                return self.fatal("Content error in the internal subset");
            }
            let c = self.raw();
            if quote != 0 {
                if c == quote {
                    quote = 0;
                }
            } else if c == b'"' || c == b'\'' {
                quote = c;
            } else if c == b'>' {
                return self.next();
            }
            self.next()?;
        }
    }

    /// `xmlParseDocument`.
    fn document(&mut self) -> R<()> {
        self.detect_encoding();
        if self.raw() == 0 {
            return self.fatal("Document is empty");
        }
        if self.cmp(b"<?xml") && is_blank(self.nxt(5)) {
            self.parse_xml_decl()?;
            self.skip_blanks();
        }
        self.parse_misc()?;
        if self.cmp(b"<!DOCTYPE") {
            self.parse_doctype()?;
            self.parse_misc()?;
        }
        if self.raw() != b'<' {
            return self.err("Start tag expected, '<' not found\n");
        }
        self.parse_element()?;
        self.parse_misc()?;
        if self.cur < self.end {
            return self.fatal("Extra content at the end of the document");
        }
        Ok(())
    }
}

/// `test_char_data`: ASCII text bytes other than `<`, `&`, `]` and line ends.
fn char_data_ok(c: u8) -> bool {
    c == 0x09 || ((0x20..0x80).contains(&c) && c != b'<' && c != b'&' && c != b']')
}

/// The fast path's loop condition.
fn char_data_continue(c: u8) -> bool {
    (0x20..=0x7F).contains(&c) || c == 0x09 || c == 0x0A
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::{tostring, ToString};

    #[test]
    fn round_trips_like_lxml() {
        let src = "<?xml version=\"1.0\"?>\n<!-- c --><r xmlns=\"urn:d\" xmlns:w=\"urn:w\"><w:a xmlns:w=\"urn:w\" w:x=\"1&#10;2\n3\t4\">t&amp;<![CDATA[<c>]]>\r\nu</w:a><?pi data?><!--k-->tail<b/></r>";
        let r = parse_str(src).unwrap();
        let opts = ToString {
            utf8: true,
            declaration: true,
            with_tail: true,
        };
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
        assert!(tostring(&r, opts)
            .unwrap()
            .ends_with("<w:new w:v=\"1\"/><ns0:q xmlns:ns0=\"urn:zz\"/></r>"));
    }

    #[test]
    fn errors_like_lxml() {
        let e = fromstring(b"<w:document><unclosed>").unwrap_err();
        assert_eq!(
            e.0,
            "Namespace prefix w on document is not defined, line 1, column 12 (<string>, line 1)"
        );
        let e = fromstring(b"<a>\n<b>\n</c>\n</a>").unwrap_err();
        assert_eq!(
            e.0,
            "Opening and ending tag mismatch: b line 2 and c, line 3, column 5 (<string>, line 3)"
        );
    }
}
