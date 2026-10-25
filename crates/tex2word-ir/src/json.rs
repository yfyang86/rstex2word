//! A small JSON value type with a parser and a serializer that reproduces
//! Python's `json.dumps` byte-for-byte.
//!
//! The IR's JSON form is a cross-language contract: the Python reference
//! writes it (`Document.to_dict`) into the `.docx` round-trip manifest and the
//! Rust port must read and write the identical text. That rules out a generic
//! serializer — key order, separators, `ensure_ascii` escaping and, above all,
//! float formatting (`float.__repr__`: `5.0`, `1e+16`, `1e-05`) all have to
//! match CPython exactly.

use std::fmt::Write as _;

/// A JSON value. Objects keep insertion order (like a Python `dict`), and
/// integers and floats stay distinct (`5` vs `5.0`) as they do in Python.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// An integer value (Python `int`); a float with an integral value is not
    /// accepted, matching Python's type distinction.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// A numeric value as `f64` (accepts both Python `int` and `float`).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Look up a key in an object (first match, like a Python dict lookup).
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

/// A JSON (de)serialisation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError(pub String);

impl JsonError {
    pub fn new(msg: impl Into<String>) -> Self {
        JsonError(msg.into())
    }
}

impl std::fmt::Display for JsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for JsonError {}

// --------------------------------------------------------------------------- //
// Parsing
// --------------------------------------------------------------------------- //

/// Parse JSON text (as Python's `json.loads` does, including `NaN`/`Infinity`).
pub fn parse(text: &str) -> Result<Value, JsonError> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        src: text,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("extra data"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    src: &'a str,
}

const MAX_DEPTH: usize = 512;

impl Parser<'_> {
    fn err(&self, msg: &str) -> JsonError {
        JsonError::new(format!("{msg} at byte {}", self.i))
    }

    fn ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.s.get(self.i) {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth > MAX_DEPTH {
            return Err(self.err("nesting too deep"));
        }
        match self.s.get(self.i) {
            None => Err(self.err("unexpected end of input")),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b't') if self.eat("true") => Ok(Value::Bool(true)),
            Some(b'f') if self.eat("false") => Ok(Value::Bool(false)),
            Some(b'n') if self.eat("null") => Ok(Value::Null),
            Some(b'N') if self.eat("NaN") => Ok(Value::Float(f64::NAN)),
            Some(b'I') if self.eat("Infinity") => Ok(Value::Float(f64::INFINITY)),
            Some(b'-') if self.s[self.i..].starts_with(b"-Infinity") => {
                self.i += "-Infinity".len();
                Ok(Value::Float(f64::NEG_INFINITY))
            }
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("expecting value")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.i += 1; // '{'
        let mut out = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Value::Object(out));
        }
        loop {
            self.ws();
            if self.s.get(self.i) != Some(&b'"') {
                return Err(self.err("expecting property name"));
            }
            let key = self.string()?;
            self.ws();
            if self.s.get(self.i) != Some(&b':') {
                return Err(self.err("expecting ':'"));
            }
            self.i += 1;
            self.ws();
            let v = self.value(depth + 1)?;
            // Python dict semantics: a repeated key keeps its first position
            // but takes the last value.
            if let Some(slot) = out.iter_mut().find(|(k, _): &&mut (String, Value)| *k == key) {
                slot.1 = v;
            } else {
                out.push((key, v));
            }
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Object(out));
                }
                _ => return Err(self.err("expecting ',' or '}'")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.i += 1; // '['
        let mut out = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Value::Array(out));
        }
        loop {
            self.ws();
            out.push(self.value(depth + 1)?);
            self.ws();
            match self.s.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b']') => {
                    self.i += 1;
                    return Ok(Value::Array(out));
                }
                _ => return Err(self.err("expecting ',' or ']'")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let h = self
            .src
            .get(self.i..self.i + 4)
            .ok_or_else(|| self.err("truncated \\u escape"))?;
        let v = u32::from_str_radix(h, 16).map_err(|_| self.err("invalid \\u escape"))?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let start = self.i;
            while let Some(&b) = self.s.get(self.i) {
                if b == b'"' || b == b'\\' {
                    break;
                }
                self.i += 1;
            }
            out.push_str(&self.src[start..self.i]);
            match self.s.get(self.i) {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => {}
            }
            self.i += 1; // backslash
            let esc = *self.s.get(self.i).ok_or_else(|| self.err("truncated escape"))?;
            self.i += 1;
            match esc {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\u{8}'),
                b'f' => out.push('\u{c}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    let hi = self.hex4()?;
                    let cp = if (0xD800..0xDC00).contains(&hi) && self.s[self.i..].starts_with(b"\\u") {
                        let save = self.i;
                        self.i += 2;
                        let lo = self.hex4()?;
                        if (0xDC00..0xE000).contains(&lo) {
                            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                        } else {
                            self.i = save;
                            hi
                        }
                    } else {
                        hi
                    };
                    // Lone surrogates can't live in a Rust String; Python would
                    // keep them, but they never occur in tex2word output.
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                }
                _ => return Err(self.err("invalid escape")),
            }
        }
    }

    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.i;
        if self.s.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        let mut is_float = false;
        while let Some(&b) = self.s.get(self.i) {
            match b {
                b'0'..=b'9' => {}
                b'.' | b'e' | b'E' => is_float = true,
                b'+' | b'-' if is_float => {}
                _ => break,
            }
            self.i += 1;
        }
        let text = &self.src[start..self.i];
        if is_float {
            text.parse::<f64>()
                .map(Value::Float)
                .map_err(|_| self.err("invalid number"))
        } else {
            match text.parse::<i64>() {
                Ok(v) => Ok(Value::Int(v)),
                // beyond i64: degrade to float rather than fail
                Err(_) => text
                    .parse::<f64>()
                    .map(Value::Float)
                    .map_err(|_| self.err("invalid number")),
            }
        }
    }
}

// --------------------------------------------------------------------------- //
// Serialisation (Python json.dumps semantics)
// --------------------------------------------------------------------------- //

/// Options mirroring the `json.dumps` keyword arguments tex2word uses.
#[derive(Debug, Clone, Copy)]
pub struct DumpOptions {
    /// `ensure_ascii` (Python default: true).
    pub ensure_ascii: bool,
    /// `indent` (`None` = single line).
    pub indent: Option<usize>,
    /// `sort_keys`.
    pub sort_keys: bool,
    /// Compact `separators=(",", ":")`. When false the Python defaults apply:
    /// `(", ", ": ")` without indent, `(",", ": ")` with indent.
    pub compact: bool,
}

impl Default for DumpOptions {
    fn default() -> Self {
        DumpOptions {
            ensure_ascii: true,
            indent: None,
            sort_keys: false,
            compact: false,
        }
    }
}

/// `json.dumps(value, **opts)`.
pub fn dumps(value: &Value, opts: DumpOptions) -> String {
    let mut out = String::new();
    write_value(&mut out, value, &opts, 0);
    out
}

fn newline_indent(out: &mut String, opts: &DumpOptions, level: usize) {
    if let Some(n) = opts.indent {
        out.push('\n');
        for _ in 0..n * level {
            out.push(' ');
        }
    }
}

fn write_value(out: &mut String, v: &Value, opts: &DumpOptions, level: usize) {
    let item_sep = if opts.compact || opts.indent.is_some() { "," } else { ", " };
    let key_sep = if opts.compact { ":" } else { ": " };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Value::Float(f) => out.push_str(&py_float_repr(*f)),
        Value::Str(s) => write_str(out, s, opts.ensure_ascii),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (k, item) in items.iter().enumerate() {
                if k > 0 {
                    out.push_str(item_sep);
                }
                newline_indent(out, opts, level + 1);
                write_value(out, item, opts, level + 1);
            }
            newline_indent(out, opts, level);
            out.push(']');
        }
        Value::Object(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut order: Vec<&(String, Value)> = entries.iter().collect();
            if opts.sort_keys {
                // Python sorts str keys by code point; Rust's `str` ordering
                // is byte-wise UTF-8, which orders code points identically.
                order.sort_by(|a, b| a.0.cmp(&b.0));
            }
            out.push('{');
            for (k, (key, item)) in order.into_iter().enumerate() {
                if k > 0 {
                    out.push_str(item_sep);
                }
                newline_indent(out, opts, level + 1);
                write_str(out, key, opts.ensure_ascii);
                out.push_str(key_sep);
                write_value(out, item, opts, level + 1);
            }
            newline_indent(out, opts, level);
            out.push('}');
        }
    }
}

fn write_str(out: &mut String, s: &str, ensure_ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            // ensure_ascii escapes everything outside printable ASCII ' '..'~'
            c if ensure_ascii && !(' '..='~').contains(&c) => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Python's `repr(float)`: the shortest round-tripping digits, printed in
/// fixed notation when the decimal exponent is in `(-4, 16]` (with a trailing
/// `.0` for integral values) and in scientific notation otherwise
/// (`1e+16`, `1.5e-07`).
pub fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    // Rust's `{:e}` yields the shortest round-trip digits, e.g. "1.5e16".
    let sci = format!("{:e}", f.abs());
    let (mant, exp) = sci.split_once('e').expect("LowerExp always has an exponent");
    let exp: i32 = exp.parse().expect("integral exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let decpt = exp + 1; // position of the decimal point within `digits`
    let mut out = String::new();
    if f.is_sign_negative() {
        out.push('-');
    }
    if decpt <= -4 || decpt > 16 {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        let _ = write!(out, "e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    } else if decpt <= 0 {
        out.push_str("0.");
        for _ in 0..-decpt {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        let d = decpt as usize;
        if digits.len() <= d {
            out.push_str(&digits);
            for _ in digits.len()..d {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..d]);
            out.push('.');
            out.push_str(&digits[d..]);
        }
    }
    out
}

// --------------------------------------------------------------------------- //
// Conversion traits
// --------------------------------------------------------------------------- //

/// Types with a JSON form (the IR's `to_dict`).
pub trait ToJson {
    fn to_json(&self) -> Value;
}

/// Types reconstructible from their JSON form (the IR's `from_dict`).
pub trait FromJson: Sized {
    fn from_json(v: &Value) -> Result<Self, JsonError>;
}

impl ToJson for Value {
    fn to_json(&self) -> Value {
        self.clone()
    }
}

impl FromJson for Value {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        Ok(v.clone())
    }
}

impl ToJson for String {
    fn to_json(&self) -> Value {
        Value::Str(self.clone())
    }
}

impl FromJson for String {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        v.as_str()
            .map(str::to_string)
            .ok_or_else(|| JsonError::new("expected a string"))
    }
}

impl ToJson for bool {
    fn to_json(&self) -> Value {
        Value::Bool(*self)
    }
}

impl FromJson for bool {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        v.as_bool().ok_or_else(|| JsonError::new("expected a bool"))
    }
}

impl ToJson for i64 {
    fn to_json(&self) -> Value {
        Value::Int(*self)
    }
}

impl FromJson for i64 {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        match v {
            Value::Int(i) => Ok(*i),
            Value::Bool(b) => Ok(*b as i64), // Python bool is an int subclass
            _ => Err(JsonError::new("expected an integer")),
        }
    }
}

impl ToJson for f64 {
    fn to_json(&self) -> Value {
        Value::Float(*self)
    }
}

impl FromJson for f64 {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        v.as_f64().ok_or_else(|| JsonError::new("expected a number"))
    }
}

impl<T: ToJson> ToJson for Option<T> {
    fn to_json(&self) -> Value {
        match self {
            Some(v) => v.to_json(),
            None => Value::Null,
        }
    }
}

impl<T: FromJson> FromJson for Option<T> {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        if v.is_null() {
            Ok(None)
        } else {
            T::from_json(v).map(Some)
        }
    }
}

impl<T: ToJson> ToJson for Vec<T> {
    fn to_json(&self) -> Value {
        Value::Array(self.iter().map(ToJson::to_json).collect())
    }
}

impl<T: FromJson> FromJson for Vec<T> {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        v.as_array()
            .ok_or_else(|| JsonError::new("expected an array"))?
            .iter()
            .map(T::from_json)
            .collect()
    }
}

impl<T: ToJson> ToJson for Box<T> {
    fn to_json(&self) -> Value {
        (**self).to_json()
    }
}

impl<T: FromJson> FromJson for Box<T> {
    fn from_json(v: &Value) -> Result<Self, JsonError> {
        T::from_json(v).map(Box::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_matches_python() {
        let cases = [
            (5.0, "5.0"),
            (0.5, "0.5"),
            (-0.0, "-0.0"),
            (0.1, "0.1"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (1e15, "1000000000000000.0"),
            (1234567.0, "1234567.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.25e-7, "1.25e-07"),
            (914400.0, "914400.0"),
            (2743200.0 / 3.0, "914400.0"),
            (1.0 / 3.0, "0.3333333333333333"),
            (123456789012345680.0, "1.2345678901234568e+17"),
        ];
        for (f, want) in cases {
            assert_eq!(py_float_repr(f), want, "repr({f:?})");
        }
    }

    /// Expected strings spell a JSON `\u` escape as `@u` (so no escape
    /// sequence appears literally in this source file).
    fn esc(s: &str) -> String {
        s.replace("@u", "\\u")
    }

    #[test]
    fn dumps_matches_python_layouts() {
        let v = parse(r#"{"b": [1, 2.0, "é\n"], "a": {}, "c": []}"#).unwrap();
        // json.dumps(v)
        assert_eq!(
            dumps(&v, DumpOptions::default()),
            esc(r#"{"b": [1, 2.0, "@u00e9\n"], "a": {}, "c": []}"#)
        );
        // json.dumps(v, ensure_ascii=False, indent=2, sort_keys=True)
        let pretty = DumpOptions {
            ensure_ascii: false,
            indent: Some(2),
            sort_keys: true,
            compact: false,
        };
        assert_eq!(
            dumps(&v, pretty),
            "{\n  \"a\": {},\n  \"b\": [\n    1,\n    2.0,\n    \"é\\n\"\n  ],\n  \"c\": []\n}"
        );
        // separators=(",", ":")
        let compact = DumpOptions {
            compact: true,
            ..DumpOptions::default()
        };
        assert_eq!(
            dumps(&v, compact),
            esc(r#"{"b":[1,2.0,"@u00e9\n"],"a":{},"c":[]}"#)
        );
    }

    #[test]
    fn astral_chars_escape_as_surrogate_pairs() {
        let v = Value::Str("𝔤\u{7f}".into());
        assert_eq!(dumps(&v, DumpOptions::default()), esc(r#""@ud835@udd24@u007f""#));
        assert_eq!(parse(&esc(r#""@ud835@udd24""#)).unwrap(), Value::Str("𝔤".into()));
    }

    #[test]
    fn ints_and_floats_stay_distinct() {
        assert_eq!(parse("5").unwrap(), Value::Int(5));
        assert_eq!(parse("5.0").unwrap(), Value::Float(5.0));
        assert_eq!(parse("1e3").unwrap(), Value::Float(1000.0));
    }
}
