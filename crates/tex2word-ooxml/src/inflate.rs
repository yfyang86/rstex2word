//! A DEFLATE (RFC 1951) decompressor: stored, fixed and dynamic blocks.

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
}

impl BitReader<'_> {
    fn need(&mut self, count: u32) -> Result<(), String> {
        while self.n < count {
            let b = *self
                .data
                .get(self.pos)
                .ok_or("unexpected end of deflate stream")?;
            self.pos += 1;
            self.acc |= (b as u64) << self.n;
            self.n += 8;
        }
        Ok(())
    }

    fn bits(&mut self, count: u32) -> Result<u32, String> {
        if count == 0 {
            return Ok(0);
        }
        self.need(count)?;
        let v = (self.acc & ((1u64 << count) - 1)) as u32;
        self.acc >>= count;
        self.n -= count;
        Ok(v)
    }

    fn align(&mut self) {
        let drop = self.n % 8;
        self.acc >>= drop;
        self.n -= drop;
    }
}

/// A canonical Huffman decoding table.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Huffman {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + counts[i - 1];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Huffman { counts, symbols }
    }

    fn decode(&self, r: &mut BitReader) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= r.bits(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("invalid Huffman code".into())
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Decompress a raw DEFLATE stream.
pub fn inflate(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut r = BitReader {
        data,
        pos: 0,
        acc: 0,
        n: 0,
    };
    let mut out: Vec<u8> = Vec::with_capacity(data.len() * 3);
    loop {
        let last = r.bits(1)?;
        match r.bits(2)? {
            0 => {
                r.align();
                let len = r.bits(16)? as usize;
                let nlen = r.bits(16)? as usize;
                if len != (!nlen & 0xFFFF) {
                    return Err("stored block length mismatch".into());
                }
                for _ in 0..len {
                    out.push(r.bits(8)? as u8);
                }
            }
            1 => {
                let mut l = [0u8; 288];
                l[..144].fill(8);
                l[144..256].fill(9);
                l[256..280].fill(7);
                l[280..].fill(8);
                block(
                    &mut r,
                    &mut out,
                    &Huffman::new(&l),
                    &Huffman::new(&[5u8; 30]),
                )?;
            }
            2 => {
                let hlit = r.bits(5)? as usize + 257;
                let hdist = r.bits(5)? as usize + 1;
                let hclen = r.bits(4)? as usize + 4;
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut cl = [0u8; 19];
                for &o in ORDER.iter().take(hclen) {
                    cl[o] = r.bits(3)? as u8;
                }
                let clh = Huffman::new(&cl);
                let mut lengths = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < hlit + hdist {
                    let sym = clh.decode(&mut r)?;
                    let (val, rep) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => (
                            *lengths
                                .get(i.wrapping_sub(1))
                                .ok_or("repeat with no previous length")?,
                            3 + r.bits(2)? as usize,
                        ),
                        17 => (0, 3 + r.bits(3)? as usize),
                        _ => (0, 11 + r.bits(7)? as usize),
                    };
                    if i + rep > lengths.len() {
                        return Err("code lengths overflow".into());
                    }
                    lengths[i..i + rep].fill(val);
                    i += rep;
                }
                block(
                    &mut r,
                    &mut out,
                    &Huffman::new(&lengths[..hlit]),
                    &Huffman::new(&lengths[hlit..]),
                )?;
            }
            _ => return Err("invalid deflate block type".into()),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

fn block(
    r: &mut BitReader,
    out: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), String> {
    loop {
        let sym = lit.decode(r)? as usize;
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let k = sym - 257;
            if k >= 29 {
                return Err("invalid length symbol".into());
            }
            let len = LEN_BASE[k] as usize + r.bits(LEN_EXTRA[k] as u32)? as usize;
            let d = dist.decode(r)? as usize;
            if d >= 30 {
                return Err("invalid distance symbol".into());
            }
            let distance = DIST_BASE[d] as usize + r.bits(DIST_EXTRA[d] as u32)? as usize;
            if distance > out.len() {
                return Err("distance too far back".into());
            }
            let start = out.len() - distance;
            for k in 0..len {
                out.push(out[start + k]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deflate::deflate;

    #[test]
    fn round_trip() {
        let mut data = Vec::new();
        for i in 0..20000u32 {
            data.extend_from_slice(format!("<w:t>{}</w:t>", i % 97).as_bytes());
            data.push((i * 7919 % 251) as u8);
        }
        for d in [
            &data[..],
            b"",
            b"a",
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            let c = deflate(d);
            assert_eq!(inflate(&c).unwrap(), d);
        }
        assert!(deflate(&data).len() < data.len() / 2);
    }
}
