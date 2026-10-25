//! A DEFLATE (RFC 1951) compressor: LZ77 with hash chains and the fixed
//! Huffman code. Output needn't match zlib byte for byte -- only the
//! decompressed parts matter -- but it must be valid and compact enough.

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn bits(&mut self, value: u32, count: u32) {
        self.acc |= (value as u64) << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Huffman codes are sent most-significant bit first.
    fn code(&mut self, code: u32, len: u32) {
        let mut rev = 0;
        for i in 0..len {
            rev |= ((code >> i) & 1) << (len - 1 - i);
        }
        self.bits(rev, len);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

fn lit(w: &mut BitWriter, sym: u32) {
    match sym {
        0..=143 => w.code(0x30 + sym, 8),
        144..=255 => w.code(0x190 + sym - 144, 9),
        256..=279 => w.code(sym - 256, 7),
        _ => w.code(0xC0 + sym - 280, 8),
    }
}

const LEN_BASE: [u32; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u32; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn emit_match(w: &mut BitWriter, len: usize, dist: usize) {
    let len = len as u32;
    let li = LEN_BASE.iter().rposition(|&b| b <= len).unwrap();
    lit(w, 257 + li as u32);
    w.bits(len - LEN_BASE[li], LEN_EXTRA[li]);
    let dist = dist as u32;
    let di = DIST_BASE.iter().rposition(|&b| b <= dist).unwrap();
    w.code(di as u32, 5);
    w.bits(dist - DIST_BASE[di], DIST_EXTRA[di]);
}

const WINDOW: usize = 32768;
const HASH_BITS: u32 = 15;
const MAX_CHAIN: usize = 64;

/// Raw DEFLATE-compress `data` (one final fixed-Huffman block).
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter { out: Vec::with_capacity(data.len() / 2 + 16), acc: 0, n: 0 };
    w.bits(1, 1); // BFINAL
    w.bits(1, 2); // BTYPE = fixed Huffman
    let n = data.len();
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; n.max(1)];
    let hash = |i: usize| -> usize {
        let v = (data[i] as u32) | ((data[i + 1] as u32) << 8) | ((data[i + 2] as u32) << 16);
        (v.wrapping_mul(2654435761) >> (32 - HASH_BITS)) as usize
    };
    let mut i = 0;
    while i < n {
        let mut best_len = 0;
        let mut best_dist = 0;
        if i + 2 < n {
            let h = hash(i);
            let mut cand = head[h];
            let mut chain = 0;
            while cand != usize::MAX && i - cand <= WINDOW && chain < MAX_CHAIN {
                let max = (n - i).min(258);
                let mut l = 0;
                while l < max && data[cand + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - cand;
                    if l == max {
                        break;
                    }
                }
                cand = prev[cand];
                chain += 1;
            }
        }
        let step = if best_len >= 3 {
            emit_match(&mut w, best_len, best_dist);
            best_len
        } else {
            lit(&mut w, data[i] as u32);
            1
        };
        for k in i..i + step {
            if k + 2 < n {
                let h = hash(k);
                prev[k] = head[h];
                head[h] = k;
            }
        }
        i += step;
    }
    lit(&mut w, 256);
    w.finish()
}
