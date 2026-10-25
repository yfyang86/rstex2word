//! Image probing and EMU sizing for embedded graphics (port of
//! `backend/images.py`): intrinsic pixel sizes from PNG/JPEG headers.

pub const EMU_PER_INCH: i64 = 914400;
const PX_PER_INCH: i64 = 96;
pub const EMU_PER_PX: i64 = EMU_PER_INCH / PX_PER_INCH; // 9525

/// max width on a US-Letter page with 1in margins.
const MAX_WIDTH_EMU: f64 = 6.0 * EMU_PER_INCH as f64;

/// An image's format and intrinsic size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub fmt: String,
    pub width_px: i64,
    pub height_px: i64,
}

impl ImageInfo {
    /// Formats Word embeds directly.
    pub fn embeddable(&self) -> bool {
        matches!(self.fmt.as_str(), "png" | "jpg" | "jpeg" | "emf")
    }
}

fn png_size(data: &[u8]) -> Option<(i64, i64)> {
    if data.get(..8)? != b"\x89PNG\r\n\x1a\n" || data.get(12..16)? != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(data.get(16..20)?.try_into().ok()?);
    let h = u32::from_be_bytes(data.get(20..24)?.try_into().ok()?);
    Some((w as i64, h as i64))
}

fn jpeg_size(data: &[u8]) -> Option<(i64, i64)> {
    if data.get(..2)? != b"\xff\xd8" {
        return None;
    }
    let n = data.len();
    let mut i = 2;
    while i + 9 < n {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]);
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]);
            return Some((w as i64, h as i64));
        }
        let seg_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        i += 2 + seg_len;
    }
    None
}

/// [`ImageInfo`] for in-memory image bytes of a known format.
pub fn probe_bytes(data: &[u8], fmt: &str) -> ImageInfo {
    let fmt = fmt.to_lowercase();
    let size = match fmt.as_str() {
        "png" => png_size(data),
        "jpg" | "jpeg" => jpeg_size(data),
        _ => None,
    };
    // unknown intrinsic size (e.g. emf) -> 4 x 3 inches worth of px
    let (w, h) = size.unwrap_or((4 * PX_PER_INCH, 3 * PX_PER_INCH));
    ImageInfo { fmt, width_px: w, height_px: h }
}

/// [`ImageInfo`] for an image file, or `None` if unreadable.
pub fn probe(path: &str) -> Option<ImageInfo> {
    let fmt = match path.rsplit_once('.') {
        Some((_, e)) => tex2word_ir::pystr::lower(e),
        None => String::new(),
    };
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = Vec::with_capacity(65536);
    f.by_ref().take(65536).read_to_end(&mut head).ok()?;
    Some(probe_bytes(&head, &fmt))
}

/// (cx, cy) in EMU, scaled down to fit `max_width_emu` (or the text width).
pub fn emu_size(info: &ImageInfo, max_width_emu: Option<f64>) -> (i64, i64) {
    let limit = max_width_emu.unwrap_or(MAX_WIDTH_EMU);
    let cx = info.width_px * EMU_PER_PX;
    let cy = info.height_px * EMU_PER_PX;
    if cx as f64 > limit {
        let scale = limit / cx as f64;
        return (limit as i64, (cy as f64 * scale) as i64);
    }
    (cx, cy)
}
