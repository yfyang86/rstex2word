//! Rasterising vector figures (port of `backend/raster.py`).
//!
//! The Python reference's optional `pdf` extra bundles pypdfium2; a published
//! Rust crate can't bundle a PDF renderer the same way, so this build drives
//! system tools when they are available and keeps the reference's graceful
//! degradation (placeholder + warning) when they are not:
//!
//! * PDF — `pdftoppm` (poppler) at the requested dpi; on macOS `sips` is a
//!   fallback (it renders a PDF's first page, though without dpi control).
//! * EPS/PS — Ghostscript (`gs`), as in the reference's optional path.

use std::process::Command;
use std::sync::OnceLock;

pub const DEFAULT_DPI: u32 = 200;

/// How the first page of a PDF can be turned into PNG on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PdfBackend {
    Pdftoppm,
    #[cfg(target_os = "macos")]
    Sips,
}

fn which(name: &str) -> Option<String> {
    // parity knob: the Python core install has no rasteriser / TeX engine;
    // the golden suite pins that environment even on dev machines that do.
    if std::env::var_os("TEX2WORD_NO_EXTERNAL_TOOLS").is_some() {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if let Ok(meta) = std::fs::metadata(&cand) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                    return Some(cand.to_string_lossy().into_owned());
                }
            }
            #[cfg(not(unix))]
            {
                if meta.is_file() {
                    return Some(cand.to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

fn pdf_backend() -> Option<PdfBackend> {
    static B: OnceLock<Option<PdfBackend>> = OnceLock::new();
    *B.get_or_init(|| {
        if which("pdftoppm").is_some() {
            Some(PdfBackend::Pdftoppm)
        } else {
            #[cfg(target_os = "macos")]
            {
                if which("sips").is_some() {
                    return Some(PdfBackend::Sips);
                }
            }
            None
        }
    })
}

/// True if a PDF rasteriser backend is available.
pub fn has_pdf_support() -> bool {
    pdf_backend().is_some()
}

fn has_gs() -> bool {
    static G: OnceLock<bool> = OnceLock::new();
    *G.get_or_init(|| which("gs").is_some())
}

/// PNG IHDR: width/height are bytes 16..24, big-endian.
fn png_size(data: &[u8]) -> Option<(i64, i64)> {
    if data.len() < 24 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(data[16..20].try_into().ok()?) as i64;
    let h = u32::from_be_bytes(data[20..24].try_into().ok()?) as i64;
    Some((w, h))
}

fn run_quiet(mut cmd: Command) -> bool {
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// Render the first page of a PDF to PNG: `(png bytes, w, h)`.
pub fn rasterize_pdf(path: &str, dpi: u32) -> Option<(Vec<u8>, i64, i64)> {
    match pdf_backend()? {
        PdfBackend::Pdftoppm => {
            let dir = std::path::Path::new(path).parent()?;
            let base = dir.join(format!("tw-raster-{}", std::process::id()));
            let mut cmd = Command::new(which("pdftoppm")?);
            cmd.arg("-png")
                .arg("-r")
                .arg(dpi.to_string())
                .arg("-singlefile")
                .arg(path)
                .arg(&base);
            if !run_quiet(cmd) {
                return None;
            }
            let png =
                std::fs::read(dir.join(format!("tw-raster-{}.png", std::process::id()))).ok()?;
            let (w, h) = png_size(&png)?;
            Some((png, w, h))
        }
        #[cfg(target_os = "macos")]
        PdfBackend::Sips => {
            // sips renders the first page at the PDF's natural scale; the dpi
            // hint can't be honoured, so cap the long side as a stand-in.
            let dir = std::path::Path::new(path).parent()?;
            let out = dir.join(format!("tw-raster-{}.png", std::process::id()));
            let max_px = (dpi as u64).clamp(72, 1200).to_string();
            let mut cmd = Command::new(which("sips")?);
            cmd.arg("-s")
                .arg("format")
                .arg("png")
                .arg("-Z")
                .arg(max_px)
                .arg(path)
                .arg(&out);
            if !run_quiet(cmd) {
                return None;
            }
            let png = std::fs::read(&out).ok()?;
            let (w, h) = png_size(&png)?;
            Some((png, w, h))
        }
    }
}

/// Rasterise a vector image of the given format, or `None` if unsupported.
pub fn rasterize(path: &str, fmt: &str, dpi: u32) -> Option<(Vec<u8>, i64, i64)> {
    match fmt {
        "pdf" => rasterize_pdf(path, dpi),
        "eps" | "ps" if has_gs() => {
            let dir = std::path::Path::new(path).parent()?;
            let out = dir.join(format!("tw-raster-{}.png", std::process::id()));
            let mut cmd = Command::new(which("gs")?);
            cmd.arg("-dNOPAUSE")
                .arg("-dBATCH")
                .arg("-dSAFER")
                .arg("-sDEVICE=png16m")
                .arg("-r")
                .arg(dpi.to_string())
                .arg("-sOutputFile")
                .arg(&out)
                .arg(path);
            if !run_quiet(cmd) {
                return None;
            }
            let png = std::fs::read(&out).ok()?;
            let (w, h) = png_size(&png)?;
            Some((png, w, h))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_size_parses_ihdr() {
        // 1x1 PNG header
        let png: &[u8] = &[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0,
            0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0,
        ];
        assert_eq!(png_size(png), Some((1, 1)));
        assert_eq!(png_size(b"not a png"), None);
    }

    #[test]
    fn pdf_tools_probe_is_consistent() {
        // on dev machines pdftoppm exists; either way the two views agree
        assert_eq!(has_pdf_support(), pdf_backend().is_some());
    }
}
