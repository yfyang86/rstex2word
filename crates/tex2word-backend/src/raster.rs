//! Rasterising vector figures (port of `backend/raster.py`). The reference
//! uses the optional `pdf` extra (pypdfium2 + Pillow); the core install has no
//! PDF rasteriser, so these report "unsupported" and callers degrade to a
//! placeholder + warning exactly as the reference does without the extra.

pub const DEFAULT_DPI: u32 = 200;

/// True if a PDF rasteriser backend is available (never, in the core build).
pub fn has_pdf_support() -> bool {
    false
}

/// Render the first page of a PDF to PNG; `None` without a backend.
pub fn rasterize_pdf(_path: &str, _dpi: u32) -> Option<(Vec<u8>, i64, i64)> {
    None
}

/// Rasterise a vector image of the given format, or `None` if unsupported.
pub fn rasterize(path: &str, fmt: &str, dpi: u32) -> Option<(Vec<u8>, i64, i64)> {
    if fmt == "pdf" {
        return rasterize_pdf(path, dpi);
    }
    None // EPS/PS need Ghostscript; not bundled
}
