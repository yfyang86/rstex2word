//! Python `os.path` (POSIX) and text-file reading semantics.
//!
//! Paths flow into the IR and the report (image paths, include resolution),
//! so they are manipulated as strings exactly as `posixpath` does. Text files
//! are read like `open(path, encoding="utf-8")`: universal newlines (`\r\n`
//! and `\r` become `\n`) and a choice of decode-error handling.

use std::fs;
use std::io;
use std::path::Path;

/// `os.path.join(a, *rest)` (posixpath).
pub fn join(a: &str, rest: &[&str]) -> String {
    let mut path = a.to_string();
    for b in rest {
        if b.starts_with('/') {
            path = (*b).to_string();
        } else if path.is_empty() || path.ends_with('/') {
            path.push_str(b);
        } else {
            path.push('/');
            path.push_str(b);
        }
    }
    path
}

/// `os.path.join(a, b)`.
pub fn join2(a: &str, b: &str) -> String {
    join(a, &[b])
}

/// `os.path.dirname(p)`.
pub fn dirname(p: &str) -> String {
    let i = p.rfind('/').map_or(0, |k| k + 1);
    let head = &p[..i];
    if !head.is_empty() && !head.chars().all(|c| c == '/') {
        head.trim_end_matches('/').to_string()
    } else {
        head.to_string()
    }
}

/// `os.path.basename(p)`.
pub fn basename(p: &str) -> &str {
    &p[p.rfind('/').map_or(0, |k| k + 1)..]
}

/// `os.path.splitext(p)` -> `(root, ext)`; leading dots of a name are not an
/// extension (`.bashrc` has none).
pub fn splitext(p: &str) -> (&str, &str) {
    let sep = p.rfind('/').map_or(-1, |k| k as isize);
    let dot = p.rfind('.').map_or(-1, |k| k as isize);
    if dot > sep {
        let mut i = (sep + 1) as usize;
        let dot = dot as usize;
        while i < dot {
            if p.as_bytes()[i] != b'.' {
                return (&p[..dot], &p[dot..]);
            }
            i += 1;
        }
    }
    (p, "")
}

/// `os.path.normpath(p)`.
pub fn normpath(p: &str) -> String {
    if p.is_empty() {
        return ".".into();
    }
    let initial = if p.starts_with("//") && !p.starts_with("///") {
        2
    } else if p.starts_with('/') {
        1
    } else {
        0
    };
    let mut comps: Vec<&str> = Vec::new();
    for c in p.split('/') {
        if c.is_empty() || c == "." {
            continue;
        }
        if c != ".." || (initial == 0 && comps.is_empty()) || comps.last() == Some(&"..") {
            comps.push(c);
        } else if !comps.is_empty() {
            comps.pop();
        }
    }
    let mut out = "/".repeat(initial);
    out.push_str(&comps.join("/"));
    if out.is_empty() {
        ".".into()
    } else {
        out
    }
}

/// `os.path.abspath(p)`.
pub fn abspath(p: &str) -> String {
    if p.starts_with('/') {
        normpath(p)
    } else {
        let cwd = std::env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".into());
        normpath(&join2(&cwd, p))
    }
}

/// `os.path.isfile(p)`.
pub fn isfile(p: &str) -> bool {
    Path::new(p).is_file()
}

/// `os.path.isdir(p)`.
pub fn isdir(p: &str) -> bool {
    Path::new(p).is_dir()
}

/// `os.path.exists(p)`.
pub fn exists(p: &str) -> bool {
    Path::new(p).exists()
}

/// How undecodable UTF-8 is handled (`errors=` of `open`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Errors {
    /// raise (Python's default)
    Strict,
    /// drop the bad bytes
    Ignore,
    /// substitute U+FFFD
    Replace,
}

/// Universal-newline translation: `\r\n` and lone `\r` become `\n`.
pub fn universal_newlines(s: &str) -> String {
    if !s.contains('\r') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// Decode UTF-8 bytes with the given error handling.
pub fn decode_utf8(bytes: &[u8], errors: Errors) -> io::Result<String> {
    match errors {
        Errors::Strict => String::from_utf8(bytes.to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
        Errors::Replace => Ok(String::from_utf8_lossy(bytes).into_owned()),
        Errors::Ignore => {
            let mut out = String::new();
            let mut rest = bytes;
            loop {
                match std::str::from_utf8(rest) {
                    Ok(s) => {
                        out.push_str(s);
                        return Ok(out);
                    }
                    Err(e) => {
                        let good = e.valid_up_to();
                        out.push_str(std::str::from_utf8(&rest[..good]).expect("valid prefix"));
                        let skip = e.error_len().unwrap_or(rest.len() - good);
                        rest = &rest[good + skip..];
                    }
                }
            }
        }
    }
}

/// `open(path, encoding="utf-8", errors=errors).read()`.
pub fn read_text(path: &str, errors: Errors) -> io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(universal_newlines(&decode_utf8(&bytes, errors)?))
}

/// `\today` -> the current date in LaTeX's default English format
/// (`Month D, YYYY`). Honours `SOURCE_DATE_EPOCH` (reproducible builds);
/// otherwise uses the current UTC date.
pub fn today_text() -> String {
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64)
        });
    let (y, m, d) = civil_from_days(secs.div_euclid(86400));
    const MONTHS: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August", "September",
        "October", "November", "December",
    ];
    format!("{} {}, {}", MONTHS[(m - 1) as usize], d, y)
}

/// `datetime.fromtimestamp(secs, UTC).isoformat()` for whole seconds:
/// `YYYY-MM-DDTHH:MM:SS+00:00`.
pub fn utc_isoformat(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86400));
    let t = secs.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+00:00", t / 3600, t / 60 % 60, t % 60)
}

/// The current Unix time in whole seconds.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Python's `str(OSError)` for an I/O error on `path`:
/// `[Errno 2] No such file or directory: 'path'`.
pub fn os_error_str(e: &std::io::Error, path: &str) -> String {
    let repr = crate::json::py_str_repr(path);
    match e.raw_os_error() {
        Some(n) => {
            let msg = match n {
                1 => "Operation not permitted",
                2 => "No such file or directory",
                5 => "Input/output error",
                13 => "Permission denied",
                17 => "File exists",
                20 => "Not a directory",
                21 => "Is a directory",
                24 => "Too many open files",
                28 => "No space left on device",
                30 => "Read-only file system",
                36 => "File name too long",
                _ => return format!("[Errno {n}] {e}: {repr}"),
            };
            format!("[Errno {n}] {msg}: {repr}")
        }
        None => e.to_string(),
    }
}

/// Days since 1970-01-01 -> (year, month, day) in the proleptic Gregorian
/// calendar (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(20455), (2026, 1, 2));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(utc_isoformat(1767312000), "2026-01-02T00:00:00+00:00");
        assert_eq!(utc_isoformat(1767312000 + 3723), "2026-01-02T01:02:03+00:00");
    }

    #[test]
    fn posixpath_semantics() {
        assert_eq!(join("a", &["b", "c"]), "a/b/c");
        assert_eq!(join("a/", &["b"]), "a/b");
        assert_eq!(join("a", &["/abs", "x"]), "/abs/x");
        assert_eq!(join("", &["b"]), "b");
        assert_eq!(dirname("a/b/c.tex"), "a/b");
        assert_eq!(dirname("c.tex"), "");
        assert_eq!(dirname("/c.tex"), "/");
        assert_eq!(dirname("a//b"), "a");
        assert_eq!(basename("a/b/c.tex"), "c.tex");
        assert_eq!(splitext("a/b.tar.gz"), ("a/b.tar", ".gz"));
        assert_eq!(splitext(".bashrc"), (".bashrc", ""));
        assert_eq!(splitext("a.d/b"), ("a.d/b", ""));
        assert_eq!(normpath("a/./b/../c"), "a/c");
        assert_eq!(normpath("../x"), "../x");
        assert_eq!(normpath("/../x"), "/x");
    }

    #[test]
    fn text_reading_semantics() {
        assert_eq!(universal_newlines("a\r\nb\rc\n"), "a\nb\nc\n");
        assert_eq!(decode_utf8(b"a\xffb", Errors::Ignore).unwrap(), "ab");
        assert!(decode_utf8(b"a\xffb", Errors::Strict).is_err());
    }
}
