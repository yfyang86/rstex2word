//! ZIP archives: writing deflated members (as Python's `zipfile` with
//! `ZIP_DEFLATED` and fixed 1980-01-01 timestamps) and reading stored or
//! deflated members.

use crate::deflate::deflate;
use crate::inflate::inflate;

fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (n, slot) in t.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB88320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    t
}

/// CRC-32 (IEEE), as `zlib.crc32`.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(crc_table);
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

fn u16le(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn u32le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// DOS date for 1980-01-01 00:00:00.
const DOS_DATE: u16 = (1 << 5) | 1;
const DOS_TIME: u16 = 0;

/// Write a ZIP archive of `(name, data)` members, deflated, in the given
/// order.
pub fn write(members: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in members {
        let offset = out.len() as u32;
        let comp = deflate(data);
        let crc = crc32(data);
        let flags: u16 = if name.is_ascii() { 0 } else { 0x800 };
        u32le(&mut out, 0x0403_4b50);
        u16le(&mut out, 20); // version needed
        u16le(&mut out, flags);
        u16le(&mut out, 8); // deflated
        u16le(&mut out, DOS_TIME);
        u16le(&mut out, DOS_DATE);
        u32le(&mut out, crc);
        u32le(&mut out, comp.len() as u32);
        u32le(&mut out, data.len() as u32);
        u16le(&mut out, name.len() as u16);
        u16le(&mut out, 0);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&comp);

        u32le(&mut central, 0x0201_4b50);
        // version made by: 2.0 on the host OS (zipfile: 0 on Windows, 3 elsewhere)
        central.push(20);
        central.push(if cfg!(windows) { 0 } else { 3 });
        u16le(&mut central, 20);
        u16le(&mut central, flags);
        u16le(&mut central, 8);
        u16le(&mut central, DOS_TIME);
        u16le(&mut central, DOS_DATE);
        u32le(&mut central, crc);
        u32le(&mut central, comp.len() as u32);
        u32le(&mut central, data.len() as u32);
        u16le(&mut central, name.len() as u16);
        u16le(&mut central, 0); // extra
        u16le(&mut central, 0); // comment
        u16le(&mut central, 0); // disk
        u16le(&mut central, 0); // internal attrs
        u32le(&mut central, 0o600 << 16); // external attrs (rw-------)
        u32le(&mut central, offset);
        central.extend_from_slice(name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    let cd_size = central.len() as u32;
    out.extend_from_slice(&central);
    u32le(&mut out, 0x0605_4b50);
    u16le(&mut out, 0);
    u16le(&mut out, 0);
    u16le(&mut out, members.len() as u16);
    u16le(&mut out, members.len() as u16);
    u32le(&mut out, cd_size);
    u32le(&mut out, cd_offset);
    u16le(&mut out, 0);
    out
}

fn rd16(b: &[u8], i: usize) -> Result<usize, String> {
    b.get(i..i + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]) as usize)
        .ok_or_else(|| "truncated zip".into())
}

fn rd32(b: &[u8], i: usize) -> Result<usize, String> {
    b.get(i..i + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
        .ok_or_else(|| "truncated zip".into())
}

/// Code page 437 (zipfile's default name encoding), bytes 0x80..=0xFF.
#[rustfmt::skip]
const CP437_HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç',
    'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å',
    'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù',
    'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧', 'ƒ',
    'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º',
    '¿', '⌐', '¬', '½', '¼', '¡', '«', '»',
    '░', '▒', '▓', '│', '┤', '╡', '╢', '╖',
    '╕', '╣', '║', '╗', '╝', '╜', '╛', '┐',
    '└', '┴', '┬', '├', '─', '┼', '╞', '╟',
    '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧',
    '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫',
    '╪', '┘', '┌', '█', '▄', '▌', '▐', '▀',
    'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ',
    'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩',
    '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈',
    '°', '∙', '·', '√', 'ⁿ', '²', '■', '\u{a0}',
];

/// A member's central-directory metadata.
#[derive(Debug, Clone)]
pub struct Info {
    pub name: String,
    pub method: u16,
    pub compressed_size: usize,
    pub size: usize,
    pub crc: u32,
    offset: usize,
}

/// A ZIP archive opened for reading (`zipfile.ZipFile(io.BytesIO(data))`).
pub struct Archive<'a> {
    data: &'a [u8],
    pub infos: Vec<Info>,
}

impl<'a> Archive<'a> {
    /// Parse the central directory (raises like `zipfile.BadZipFile`).
    pub fn new(data: &'a [u8]) -> Result<Archive<'a>, String> {
        if data.len() < 22 {
            return Err("File is not a zip file".into());
        }
        let lo = data.len().saturating_sub(22 + 65535);
        let eocd = (lo..=data.len() - 22)
            .rev()
            .find(|&i| data[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
            .ok_or("File is not a zip file")?;
        let count = rd16(data, eocd + 10)?;
        let mut p = rd32(data, eocd + 16)?;
        let mut infos = Vec::with_capacity(count);
        for _ in 0..count {
            if rd32(data, p)? != 0x0201_4b50 {
                return Err("Bad magic number for central directory".into());
            }
            let flags = rd16(data, p + 8)?;
            let method = rd16(data, p + 10)? as u16;
            let crc = rd32(data, p + 16)? as u32;
            let compressed_size = rd32(data, p + 20)?;
            let size = rd32(data, p + 24)?;
            let nlen = rd16(data, p + 28)?;
            let elen = rd16(data, p + 30)?;
            let clen = rd16(data, p + 32)?;
            let offset = rd32(data, p + 42)?;
            let raw = data.get(p + 46..p + 46 + nlen).ok_or("truncated zip")?;
            let name = if flags & 0x800 != 0 {
                String::from_utf8_lossy(raw).into_owned()
            } else {
                raw.iter()
                    .map(|&b| {
                        if b < 0x80 {
                            char::from(b)
                        } else {
                            CP437_HIGH[(b - 0x80) as usize]
                        }
                    })
                    .collect()
            };
            infos.push(Info {
                name,
                method,
                compressed_size,
                size,
                crc,
                offset,
            });
            p += 46 + nlen + elen + clen;
        }
        Ok(Archive { data, infos })
    }

    /// `ZipFile.namelist()`.
    pub fn names(&self) -> Vec<&str> {
        self.infos.iter().map(|i| i.name.as_str()).collect()
    }

    /// `ZipFile.read(name)` (the last member of that name wins, as in Python).
    pub fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        let info = self
            .infos
            .iter()
            .rev()
            .find(|i| i.name == name)
            .ok_or_else(|| format!("There is no item named '{name}' in the archive"))?;
        self.read_info(info)
    }

    pub fn read_info(&self, info: &Info) -> Result<Vec<u8>, String> {
        let p = info.offset;
        if rd32(self.data, p)? != 0x0403_4b50 {
            return Err("Bad magic number for file header".into());
        }
        let start = p + 30 + rd16(self.data, p + 26)? + rd16(self.data, p + 28)?;
        let raw = self
            .data
            .get(start..start + info.compressed_size)
            .ok_or("truncated zip member")?;
        let out = match info.method {
            0 => raw.to_vec(),
            8 => inflate(raw)?,
            m => return Err(format!("compression type {m} is not supported")),
        };
        if crc32(&out) != info.crc {
            return Err(format!("Bad CRC-32 for file '{}'", info.name));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read() {
        let members = vec![
            ("[Content_Types].xml".to_string(), b"<Types/>".to_vec()),
            (
                "word/document.xml".to_string(),
                b"<w:document>hello hello hello</w:document>".repeat(50),
            ),
        ];
        let z = write(&members);
        let a = Archive::new(&z).unwrap();
        assert_eq!(a.names(), vec!["[Content_Types].xml", "word/document.xml"]);
        assert_eq!(a.read("word/document.xml").unwrap(), members[1].1);
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }
}
