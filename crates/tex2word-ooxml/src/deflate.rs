//! Raw DEFLATE (RFC 1951) exactly as zlib 1.3 produces it at the default
//! level 6 (`deflateInit2(level=-1, method=8, windowBits=-15, memLevel=8)`,
//! what Python's `zipfile` uses), so archives are byte-identical to the
//! reference's.
//!
//! A port of zlib's `deflate_slow` (lazy matching over hash chains, with
//! `fill_window`/`longest_match`) and `trees.c` (block splitting by symbol
//! buffer, dynamic/static/stored block choice, canonical Huffman trees).

const W_BITS: usize = 15;
const W_SIZE: usize = 1 << W_BITS;
const W_MASK: usize = W_SIZE - 1;
const WINDOW_SIZE: usize = 2 * W_SIZE;
const HASH_BITS: u32 = 8 + 7;
const HASH_SIZE: usize = 1 << HASH_BITS;
const HASH_MASK: u32 = (HASH_SIZE - 1) as u32;
const HASH_SHIFT: u32 = HASH_BITS.div_ceil(MIN_MATCH as u32);
const LIT_BUFSIZE: usize = 1 << (8 + 6);
const SYM_END: usize = (LIT_BUFSIZE - 1) * 3;

const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const MAX_DIST: usize = W_SIZE - MIN_LOOKAHEAD;
const WIN_INIT: usize = MAX_MATCH;
const TOO_FAR: usize = 4096;
const NIL: usize = 0;

// level 6
const GOOD_MATCH: usize = 8;
const MAX_LAZY: usize = 16;
const NICE_MATCH: usize = 128;
const MAX_CHAIN: usize = 128;

const LENGTH_CODES: usize = 29;
const LITERALS: usize = 256;
const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
const D_CODES: usize = 30;
const BL_CODES: usize = 19;
const HEAP_SIZE: usize = 2 * L_CODES + 1;
const MAX_BITS: usize = 15;
const MAX_BL_BITS: usize = 7;
const END_BLOCK: usize = 256;
const REP_3_6: usize = 16;
const REPZ_3_10: usize = 17;
const REPZ_11_138: usize = 18;
const BUF_SIZE: i32 = 16;

const STORED_BLOCK: u32 = 0;
const STATIC_TREES: u32 = 1;
const DYN_TREES: u32 = 2;

const EXTRA_LBITS: [u32; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const EXTRA_DBITS: [u32; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const EXTRA_BLBITS: [u32; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];
const BL_ORDER: [usize; BL_CODES] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// zlib's static tables (`tr_static_init`).
struct Static {
    ltree_code: [u16; L_CODES + 2],
    ltree_len: [u16; L_CODES + 2],
    dtree_code: [u16; D_CODES],
    dtree_len: [u16; D_CODES],
    dist_code: [u8; 512],
    length_code: [u8; MAX_MATCH - MIN_MATCH + 1],
    base_length: [u32; LENGTH_CODES],
    base_dist: [u32; D_CODES],
}

fn bi_reverse(mut code: u32, mut len: u32) -> u32 {
    let mut res = 0;
    loop {
        res |= code & 1;
        code >>= 1;
        res <<= 1;
        len -= 1;
        if len == 0 {
            break;
        }
    }
    res >> 1
}

/// `gen_codes` over separate code/len arrays.
fn gen_codes(code: &mut [u16], len: &[u16], max_code: usize, bl_count: &[u16; MAX_BITS + 1]) {
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut c: u32 = 0;
    for bits in 1..=MAX_BITS {
        c = (c + bl_count[bits - 1] as u32) << 1;
        next_code[bits] = c as u16;
    }
    for n in 0..=max_code {
        let l = len[n] as usize;
        if l == 0 {
            continue;
        }
        code[n] = bi_reverse(next_code[l] as u32, l as u32) as u16;
        next_code[l] = next_code[l].wrapping_add(1);
    }
}

impl Static {
    fn new() -> Static {
        let mut s = Static {
            ltree_code: [0; L_CODES + 2],
            ltree_len: [0; L_CODES + 2],
            dtree_code: [0; D_CODES],
            dtree_len: [0; D_CODES],
            dist_code: [0; 512],
            length_code: [0; MAX_MATCH - MIN_MATCH + 1],
            base_length: [0; LENGTH_CODES],
            base_dist: [0; D_CODES],
        };
        let mut length = 0usize;
        let mut code = 0;
        while code < LENGTH_CODES - 1 {
            s.base_length[code] = length as u32;
            for _ in 0..(1 << EXTRA_LBITS[code]) {
                s.length_code[length] = code as u8;
                length += 1;
            }
            code += 1;
        }
        s.length_code[length - 1] = code as u8;
        let mut dist = 0usize;
        code = 0;
        while code < 16 {
            s.base_dist[code] = dist as u32;
            for _ in 0..(1 << EXTRA_DBITS[code]) {
                s.dist_code[dist] = code as u8;
                dist += 1;
            }
            code += 1;
        }
        dist >>= 7;
        while code < D_CODES {
            s.base_dist[code] = (dist << 7) as u32;
            for _ in 0..(1 << (EXTRA_DBITS[code] - 7)) {
                s.dist_code[256 + dist] = code as u8;
                dist += 1;
            }
            code += 1;
        }
        let mut bl_count = [0u16; MAX_BITS + 1];
        for n in 0..L_CODES + 2 {
            let l = match n {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
            s.ltree_len[n] = l;
            bl_count[l as usize] += 1;
        }
        let lens = s.ltree_len;
        gen_codes(&mut s.ltree_code, &lens, L_CODES + 1, &bl_count);
        for n in 0..D_CODES {
            s.dtree_len[n] = 5;
            s.dtree_code[n] = bi_reverse(n as u32, 5) as u16;
        }
        s
    }

    fn d_code(&self, dist: usize) -> usize {
        if dist < 256 {
            self.dist_code[dist] as usize
        } else {
            self.dist_code[256 + (dist >> 7)] as usize
        }
    }
}

/// A Huffman tree as zlib's `ct_data` union: `fc` is Freq/Code, `dl`
/// Dad/Len (the fields alias, and zlib relies on that).
struct Tree {
    fc: Vec<u16>,
    dl: Vec<u16>,
    max_code: usize,
}

impl Tree {
    fn new(size: usize) -> Tree {
        Tree {
            fc: vec![0; size],
            dl: vec![0; size],
            max_code: 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Lit,
    Dist,
    Bl,
}

struct State<'a> {
    st: Static,
    input: &'a [u8],
    next_in: usize,
    window: Vec<u8>,
    prev: Vec<u16>,
    head: Vec<u16>,
    ins_h: u32,
    strstart: usize,
    block_start: i64,
    lookahead: usize,
    insert: usize,
    match_length: usize,
    prev_length: usize,
    prev_match: usize,
    match_start: usize,
    match_available: bool,
    high_water: usize,
    // trees
    ltree: Tree,
    dtree: Tree,
    bltree: Tree,
    heap: [usize; HEAP_SIZE],
    heap_len: usize,
    heap_max: usize,
    depth: [u8; HEAP_SIZE],
    bl_count: [u16; MAX_BITS + 1],
    opt_len: u64,
    static_len: u64,
    sym_buf: Vec<u8>,
    sym_next: usize,
    bi_buf: u16,
    bi_valid: i32,
    out: Vec<u8>,
}

impl<'a> State<'a> {
    fn new(input: &'a [u8]) -> State<'a> {
        let mut s = State {
            st: Static::new(),
            input,
            next_in: 0,
            window: vec![0; WINDOW_SIZE],
            prev: vec![0; W_SIZE],
            head: vec![0; HASH_SIZE],
            ins_h: 0,
            strstart: 0,
            block_start: 0,
            lookahead: 0,
            insert: 0,
            match_length: MIN_MATCH - 1,
            prev_length: MIN_MATCH - 1,
            prev_match: 0,
            match_start: 0,
            match_available: false,
            high_water: 0,
            ltree: Tree::new(HEAP_SIZE),
            dtree: Tree::new(2 * D_CODES + 1),
            bltree: Tree::new(2 * BL_CODES + 1),
            heap: [0; HEAP_SIZE],
            heap_len: 0,
            heap_max: 0,
            depth: [0; HEAP_SIZE],
            bl_count: [0; MAX_BITS + 1],
            opt_len: 0,
            static_len: 0,
            sym_buf: vec![0; LIT_BUFSIZE * 3],
            sym_next: 0,
            bi_buf: 0,
            bi_valid: 0,
            out: Vec::new(),
        };
        s.init_block();
        s
    }

    // ---- bit output -------------------------------------------------------

    fn put_short(&mut self, w: u16) {
        self.out.push(w as u8);
        self.out.push((w >> 8) as u8);
    }

    fn send_bits(&mut self, value: u32, length: i32) {
        if self.bi_valid > BUF_SIZE - length {
            self.bi_buf |= ((value as u16 as u32) << self.bi_valid) as u16;
            let b = self.bi_buf;
            self.put_short(b);
            self.bi_buf = ((value as u16) as u32 >> (BUF_SIZE - self.bi_valid) as u32) as u16;
            self.bi_valid += length - BUF_SIZE;
        } else {
            self.bi_buf |= ((value as u16 as u32) << self.bi_valid) as u16;
            self.bi_valid += length;
        }
    }

    fn bi_windup(&mut self) {
        if self.bi_valid > 8 {
            let b = self.bi_buf;
            self.put_short(b);
        } else if self.bi_valid > 0 {
            self.out.push(self.bi_buf as u8);
        }
        self.bi_buf = 0;
        self.bi_valid = 0;
    }

    // ---- trees ------------------------------------------------------------

    fn tree(&mut self, k: Kind) -> &mut Tree {
        match k {
            Kind::Lit => &mut self.ltree,
            Kind::Dist => &mut self.dtree,
            Kind::Bl => &mut self.bltree,
        }
    }

    fn init_block(&mut self) {
        for n in 0..L_CODES {
            self.ltree.fc[n] = 0;
        }
        for n in 0..D_CODES {
            self.dtree.fc[n] = 0;
        }
        for n in 0..BL_CODES {
            self.bltree.fc[n] = 0;
        }
        self.ltree.fc[END_BLOCK] = 1;
        self.opt_len = 0;
        self.static_len = 0;
        self.sym_next = 0;
    }

    fn smaller(tree: &Tree, depth: &[u8; HEAP_SIZE], n: usize, m: usize) -> bool {
        tree.fc[n] < tree.fc[m] || (tree.fc[n] == tree.fc[m] && depth[n] <= depth[m])
    }

    fn pqdownheap(&mut self, k_tree: Kind, mut k: usize) {
        let v = self.heap[k];
        let mut j = k << 1;
        while j <= self.heap_len {
            let tree = match k_tree {
                Kind::Lit => &self.ltree,
                Kind::Dist => &self.dtree,
                Kind::Bl => &self.bltree,
            };
            if j < self.heap_len && Self::smaller(tree, &self.depth, self.heap[j + 1], self.heap[j])
            {
                j += 1;
            }
            if Self::smaller(tree, &self.depth, v, self.heap[j]) {
                break;
            }
            self.heap[k] = self.heap[j];
            k = j;
            j <<= 1;
        }
        self.heap[k] = v;
    }

    /// (static tree lens, extra bits, extra base, elems, max length)
    fn desc(&self, k: Kind) -> (Option<&'static [u16]>, &'static [u32], usize, usize, usize) {
        match k {
            Kind::Lit => (None, &EXTRA_LBITS, LITERALS + 1, L_CODES, MAX_BITS),
            Kind::Dist => (None, &EXTRA_DBITS, 0, D_CODES, MAX_BITS),
            Kind::Bl => (None, &EXTRA_BLBITS, 0, BL_CODES, MAX_BL_BITS),
        }
    }

    fn static_len_of(&self, k: Kind, n: usize) -> Option<u16> {
        match k {
            Kind::Lit => Some(self.st.ltree_len[n]),
            Kind::Dist => Some(self.st.dtree_len[n]),
            Kind::Bl => None,
        }
    }

    fn gen_bitlen(&mut self, k: Kind) {
        let (_, extra, base, _, max_length) = self.desc(k);
        let max_code = self.tree(k).max_code;
        for b in self.bl_count.iter_mut() {
            *b = 0;
        }
        let root = self.heap[self.heap_max];
        self.tree(k).dl[root] = 0;
        let mut overflow = 0i32;
        let mut h = self.heap_max + 1;
        while h < HEAP_SIZE {
            let n = self.heap[h];
            let dad = self.tree(k).dl[n] as usize;
            let mut bits = self.tree(k).dl[dad] as usize + 1;
            if bits > max_length {
                bits = max_length;
                overflow += 1;
            }
            self.tree(k).dl[n] = bits as u16;
            h += 1;
            if n > max_code {
                continue;
            }
            self.bl_count[bits] += 1;
            let xbits = if n >= base { extra[n - base] as u64 } else { 0 };
            let f = self.tree(k).fc[n] as u64;
            self.opt_len = self.opt_len.wrapping_add(f * (bits as u64 + xbits));
            if let Some(sl) = self.static_len_of(k, n) {
                self.static_len = self.static_len.wrapping_add(f * (sl as u64 + xbits));
            }
        }
        if overflow == 0 {
            return;
        }
        loop {
            let mut bits = max_length - 1;
            while self.bl_count[bits] == 0 {
                bits -= 1;
            }
            self.bl_count[bits] -= 1;
            self.bl_count[bits + 1] += 2;
            self.bl_count[max_length] -= 1;
            overflow -= 2;
            if overflow <= 0 {
                break;
            }
        }
        let mut h = HEAP_SIZE;
        let mut bits = max_length;
        while bits != 0 {
            let mut n = self.bl_count[bits] as usize;
            while n != 0 {
                h -= 1;
                let m = self.heap[h];
                if m > max_code {
                    continue;
                }
                if self.tree(k).dl[m] as usize != bits {
                    let len = self.tree(k).dl[m] as u64;
                    let f = self.tree(k).fc[m] as u64;
                    self.opt_len = self
                        .opt_len
                        .wrapping_add((bits as u64).wrapping_sub(len).wrapping_mul(f));
                    self.tree(k).dl[m] = bits as u16;
                }
                n -= 1;
            }
            bits -= 1;
        }
    }

    fn build_tree(&mut self, k: Kind) {
        let (_, _, _, elems, _) = self.desc(k);
        let mut max_code: isize = -1;
        self.heap_len = 0;
        self.heap_max = HEAP_SIZE;
        for n in 0..elems {
            if self.tree(k).fc[n] != 0 {
                self.heap_len += 1;
                self.heap[self.heap_len] = n;
                max_code = n as isize;
                self.depth[n] = 0;
            } else {
                self.tree(k).dl[n] = 0;
            }
        }
        while self.heap_len < 2 {
            let node = if max_code < 2 {
                max_code += 1;
                max_code as usize
            } else {
                0
            };
            self.heap_len += 1;
            self.heap[self.heap_len] = node;
            self.tree(k).fc[node] = 1;
            self.depth[node] = 0;
            self.opt_len = self.opt_len.wrapping_sub(1);
            if let Some(sl) = self.static_len_of(k, node) {
                self.static_len = self.static_len.wrapping_sub(sl as u64);
            }
        }
        self.tree(k).max_code = max_code as usize;
        let mut n = self.heap_len / 2;
        while n >= 1 {
            self.pqdownheap(k, n);
            n -= 1;
        }
        let mut node = elems;
        loop {
            // pqremove
            let n = self.heap[1];
            self.heap[1] = self.heap[self.heap_len];
            self.heap_len -= 1;
            self.pqdownheap(k, 1);
            let m = self.heap[1];
            self.heap_max -= 1;
            self.heap[self.heap_max] = n;
            self.heap_max -= 1;
            self.heap[self.heap_max] = m;
            let freq = self.tree(k).fc[n].wrapping_add(self.tree(k).fc[m]);
            self.tree(k).fc[node] = freq;
            self.depth[node] = self.depth[n].max(self.depth[m]) + 1;
            self.tree(k).dl[n] = node as u16;
            self.tree(k).dl[m] = node as u16;
            self.heap[1] = node;
            node += 1;
            self.pqdownheap(k, 1);
            if self.heap_len < 2 {
                break;
            }
        }
        self.heap_max -= 1;
        self.heap[self.heap_max] = self.heap[1];
        self.gen_bitlen(k);
        let bl_count = self.bl_count;
        let t = self.tree(k);
        let max = t.max_code;
        let (fc, dl) = (&mut t.fc, &t.dl);
        gen_codes(fc, dl, max, &bl_count);
    }

    fn scan_tree(&mut self, k: Kind) {
        let max_code = self.tree(k).max_code;
        let mut prevlen: i32 = -1;
        let mut nextlen = self.tree(k).dl[0] as i32;
        let mut count = 0;
        let (mut max_count, mut min_count) = (7, 4);
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        self.tree(k).dl[max_code + 1] = 0xffff;
        for n in 0..=max_code {
            let curlen = nextlen;
            nextlen = self.tree(k).dl[n + 1] as i32;
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                self.bltree.fc[curlen as usize] += count as u16;
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.bltree.fc[curlen as usize] += 1;
                }
                self.bltree.fc[REP_3_6] += 1;
            } else if count <= 10 {
                self.bltree.fc[REPZ_3_10] += 1;
            } else {
                self.bltree.fc[REPZ_11_138] += 1;
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    fn send_bl_code(&mut self, c: usize) {
        let (code, len) = (self.bltree.fc[c], self.bltree.dl[c]);
        self.send_bits(code as u32, len as i32);
    }

    fn send_tree(&mut self, k: Kind, max_code: usize) {
        let mut prevlen: i32 = -1;
        let mut nextlen = self.tree(k).dl[0] as i32;
        let mut count: i32 = 0;
        let (mut max_count, mut min_count) = (7, 4);
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        for n in 0..=max_code {
            let curlen = nextlen;
            nextlen = self.tree(k).dl[n + 1] as i32;
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                loop {
                    self.send_bl_code(curlen as usize);
                    count -= 1;
                    if count == 0 {
                        break;
                    }
                }
            } else if curlen != 0 {
                if curlen != prevlen {
                    self.send_bl_code(curlen as usize);
                    count -= 1;
                }
                self.send_bl_code(REP_3_6);
                self.send_bits((count - 3) as u32, 2);
            } else if count <= 10 {
                self.send_bl_code(REPZ_3_10);
                self.send_bits((count - 3) as u32, 3);
            } else {
                self.send_bl_code(REPZ_11_138);
                self.send_bits((count - 11) as u32, 7);
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    fn build_bl_tree(&mut self) -> usize {
        self.scan_tree(Kind::Lit);
        self.scan_tree(Kind::Dist);
        self.build_tree(Kind::Bl);
        let mut max_blindex = BL_CODES - 1;
        while max_blindex >= 3 {
            if self.bltree.dl[BL_ORDER[max_blindex]] != 0 {
                break;
            }
            max_blindex -= 1;
        }
        self.opt_len = self
            .opt_len
            .wrapping_add(3 * (max_blindex as u64 + 1) + 5 + 5 + 4);
        max_blindex
    }

    fn send_all_trees(&mut self, lcodes: usize, dcodes: usize, blcodes: usize) {
        self.send_bits((lcodes - 257) as u32, 5);
        self.send_bits((dcodes - 1) as u32, 5);
        self.send_bits((blcodes - 4) as u32, 4);
        for &sym in &BL_ORDER[..blcodes] {
            let l = self.bltree.dl[sym];
            self.send_bits(l as u32, 3);
        }
        self.send_tree(Kind::Lit, lcodes - 1);
        self.send_tree(Kind::Dist, dcodes - 1);
    }

    fn stored_block(&mut self, start: usize, stored_len: usize, last: bool) {
        self.send_bits((STORED_BLOCK << 1) + last as u32, 3);
        self.bi_windup();
        self.put_short(stored_len as u16);
        self.put_short(!(stored_len as u16));
        let bytes = self.window[start..start + stored_len].to_vec();
        self.out.extend_from_slice(&bytes);
    }

    fn compress_block(&mut self, dynamic: bool) {
        let mut sx = 0;
        let send_l = |s: &mut Self, c: usize| {
            let (code, len) = if dynamic {
                (s.ltree.fc[c], s.ltree.dl[c])
            } else {
                (s.st.ltree_code[c], s.st.ltree_len[c])
            };
            s.send_bits(code as u32, len as i32);
        };
        let send_d = |s: &mut Self, c: usize| {
            let (code, len) = if dynamic {
                (s.dtree.fc[c], s.dtree.dl[c])
            } else {
                (s.st.dtree_code[c], s.st.dtree_len[c])
            };
            s.send_bits(code as u32, len as i32);
        };
        if self.sym_next != 0 {
            loop {
                let mut dist = self.sym_buf[sx] as usize;
                dist += (self.sym_buf[sx + 1] as usize) << 8;
                let mut lc = self.sym_buf[sx + 2] as usize;
                sx += 3;
                if dist == 0 {
                    send_l(self, lc);
                } else {
                    let code = self.st.length_code[lc] as usize;
                    send_l(self, code + LITERALS + 1);
                    let extra = EXTRA_LBITS[code];
                    if extra != 0 {
                        lc -= self.st.base_length[code] as usize;
                        self.send_bits(lc as u32, extra as i32);
                    }
                    dist -= 1;
                    let code = self.st.d_code(dist);
                    send_d(self, code);
                    let extra = EXTRA_DBITS[code];
                    if extra != 0 {
                        dist -= self.st.base_dist[code] as usize;
                        self.send_bits(dist as u32, extra as i32);
                    }
                }
                if sx >= self.sym_next {
                    break;
                }
            }
        }
        send_l(self, END_BLOCK);
    }

    /// `_tr_flush_block`.
    fn flush_block(&mut self, buf: Option<usize>, stored_len: usize, last: bool) {
        self.build_tree(Kind::Lit);
        self.build_tree(Kind::Dist);
        let max_blindex = self.build_bl_tree();
        let mut opt_lenb = self.opt_len.wrapping_add(3 + 7) >> 3;
        let static_lenb = self.static_len.wrapping_add(3 + 7) >> 3;
        if static_lenb <= opt_lenb {
            opt_lenb = static_lenb;
        }
        match buf {
            Some(start) if stored_len as u64 + 4 <= opt_lenb => {
                self.stored_block(start, stored_len, last)
            }
            _ if static_lenb == opt_lenb => {
                self.send_bits((STATIC_TREES << 1) + last as u32, 3);
                self.compress_block(false);
            }
            _ => {
                self.send_bits((DYN_TREES << 1) + last as u32, 3);
                let (l, d) = (self.ltree.max_code + 1, self.dtree.max_code + 1);
                self.send_all_trees(l, d, max_blindex + 1);
                self.compress_block(true);
            }
        }
        self.init_block();
        if last {
            self.bi_windup();
        }
    }

    /// `FLUSH_BLOCK_ONLY`.
    fn flush_block_only(&mut self, last: bool) {
        let buf = if self.block_start >= 0 {
            Some(self.block_start as usize)
        } else {
            None
        };
        let len = (self.strstart as i64 - self.block_start) as usize;
        self.flush_block(buf, len, last);
        self.block_start = self.strstart as i64;
    }

    fn tally_lit(&mut self, c: u8) -> bool {
        self.sym_buf[self.sym_next] = 0;
        self.sym_buf[self.sym_next + 1] = 0;
        self.sym_buf[self.sym_next + 2] = c;
        self.sym_next += 3;
        self.ltree.fc[c as usize] += 1;
        self.sym_next == SYM_END
    }

    fn tally_dist(&mut self, distance: usize, length: usize) -> bool {
        self.sym_buf[self.sym_next] = distance as u8;
        self.sym_buf[self.sym_next + 1] = (distance >> 8) as u8;
        self.sym_buf[self.sym_next + 2] = length as u8;
        self.sym_next += 3;
        let dist = distance - 1;
        self.ltree.fc[self.st.length_code[length] as usize + LITERALS + 1] += 1;
        let dc = self.st.d_code(dist);
        self.dtree.fc[dc] += 1;
        self.sym_next == SYM_END
    }

    // ---- matching -----------------------------------------------------------

    fn update_hash(&self, h: u32, c: u8) -> u32 {
        ((h << HASH_SHIFT) ^ c as u32) & HASH_MASK
    }

    /// `INSERT_STRING`: returns the previous head of the chain.
    fn insert_string(&mut self, s: usize) -> usize {
        self.ins_h = self.update_hash(self.ins_h, self.window[s + MIN_MATCH - 1]);
        let head = self.head[self.ins_h as usize];
        self.prev[s & W_MASK] = head;
        self.head[self.ins_h as usize] = s as u16;
        head as usize
    }

    fn slide_hash(&mut self) {
        for p in self.head.iter_mut().chain(self.prev.iter_mut()) {
            let m = *p as usize;
            *p = if m >= W_SIZE {
                (m - W_SIZE) as u16
            } else {
                NIL as u16
            };
        }
    }

    fn fill_window(&mut self) {
        loop {
            let mut more = WINDOW_SIZE - self.lookahead - self.strstart;
            if self.strstart >= W_SIZE + MAX_DIST {
                self.window.copy_within(W_SIZE..W_SIZE + (W_SIZE - more), 0);
                self.match_start = self.match_start.wrapping_sub(W_SIZE);
                self.strstart -= W_SIZE;
                self.block_start -= W_SIZE as i64;
                if self.insert > self.strstart {
                    self.insert = self.strstart;
                }
                self.slide_hash();
                more += W_SIZE;
            }
            let avail = self.input.len() - self.next_in;
            if avail == 0 {
                break;
            }
            let n = avail.min(more);
            let at = self.strstart + self.lookahead;
            self.window[at..at + n].copy_from_slice(&self.input[self.next_in..self.next_in + n]);
            self.next_in += n;
            self.lookahead += n;
            if self.lookahead + self.insert >= MIN_MATCH {
                let mut s = self.strstart - self.insert;
                self.ins_h = self.window[s] as u32;
                self.ins_h = self.update_hash(self.ins_h, self.window[s + 1]);
                while self.insert > 0 {
                    self.ins_h = self.update_hash(self.ins_h, self.window[s + MIN_MATCH - 1]);
                    self.prev[s & W_MASK] = self.head[self.ins_h as usize];
                    self.head[self.ins_h as usize] = s as u16;
                    s += 1;
                    self.insert -= 1;
                    if self.lookahead + self.insert < MIN_MATCH {
                        break;
                    }
                }
            }
            if !(self.lookahead < MIN_LOOKAHEAD && self.next_in < self.input.len()) {
                break;
            }
        }
        if self.high_water < WINDOW_SIZE {
            let curr = self.strstart + self.lookahead;
            if self.high_water < curr {
                let init = (WINDOW_SIZE - curr).min(WIN_INIT);
                self.window[curr..curr + init].fill(0);
                self.high_water = curr + init;
            } else if self.high_water < curr + WIN_INIT {
                let init = (curr + WIN_INIT - self.high_water).min(WINDOW_SIZE - self.high_water);
                let hw = self.high_water;
                self.window[hw..hw + init].fill(0);
                self.high_water += init;
            }
        }
    }

    fn longest_match(&mut self, mut cur_match: usize) -> usize {
        let mut chain_length = MAX_CHAIN;
        let scan0 = self.strstart;
        let mut best_len = self.prev_length;
        let mut nice_match = NICE_MATCH;
        let limit = if self.strstart > MAX_DIST {
            self.strstart - MAX_DIST
        } else {
            NIL
        };
        let strend = self.strstart + MAX_MATCH;
        let w = &self.window;
        let mut scan_end1 = w[scan0 + best_len - 1];
        let mut scan_end = w[scan0 + best_len];
        if self.prev_length >= GOOD_MATCH {
            chain_length >>= 2;
        }
        if nice_match > self.lookahead {
            nice_match = self.lookahead;
        }
        loop {
            let m = cur_match;
            if !(w[m + best_len] != scan_end
                || w[m + best_len - 1] != scan_end1
                || w[m] != w[scan0]
                || w[m + 1] != w[scan0 + 1])
            {
                let mut scan = scan0 + 2;
                let mut mm = m + 2;
                'cmp: loop {
                    for _ in 0..8 {
                        scan += 1;
                        mm += 1;
                        if w[scan] != w[mm] {
                            break 'cmp;
                        }
                    }
                    if scan >= strend {
                        break;
                    }
                }
                let len = MAX_MATCH - (strend - scan);
                if len > best_len {
                    self.match_start = cur_match;
                    best_len = len;
                    if len >= nice_match {
                        break;
                    }
                    scan_end1 = w[scan0 + best_len - 1];
                    scan_end = w[scan0 + best_len];
                }
            }
            cur_match = self.prev[cur_match & W_MASK] as usize;
            if cur_match <= limit {
                break;
            }
            chain_length -= 1;
            if chain_length == 0 {
                break;
            }
        }
        if best_len <= self.lookahead {
            best_len
        } else {
            self.lookahead
        }
    }

    /// `deflate_slow` run to completion with `Z_FINISH`.
    fn deflate_slow(&mut self) {
        loop {
            if self.lookahead < MIN_LOOKAHEAD {
                self.fill_window();
                if self.lookahead == 0 {
                    break;
                }
            }
            let mut hash_head = NIL;
            if self.lookahead >= MIN_MATCH {
                hash_head = self.insert_string(self.strstart);
            }
            self.prev_length = self.match_length;
            self.prev_match = self.match_start;
            self.match_length = MIN_MATCH - 1;
            if hash_head != NIL
                && self.prev_length < MAX_LAZY
                && self.strstart - hash_head <= MAX_DIST
            {
                self.match_length = self.longest_match(hash_head);
                if self.match_length <= 5
                    && self.match_length == MIN_MATCH
                    && self.strstart.wrapping_sub(self.match_start) > TOO_FAR
                {
                    self.match_length = MIN_MATCH - 1;
                }
            }
            if self.prev_length >= MIN_MATCH && self.match_length <= self.prev_length {
                let max_insert = self.strstart + self.lookahead - MIN_MATCH;
                let bflush = self.tally_dist(
                    self.strstart - 1 - self.prev_match,
                    self.prev_length - MIN_MATCH,
                );
                self.lookahead -= self.prev_length - 1;
                self.prev_length -= 2;
                loop {
                    self.strstart += 1;
                    if self.strstart <= max_insert {
                        self.insert_string(self.strstart);
                    }
                    self.prev_length -= 1;
                    if self.prev_length == 0 {
                        break;
                    }
                }
                self.match_available = false;
                self.match_length = MIN_MATCH - 1;
                self.strstart += 1;
                if bflush {
                    self.flush_block_only(false);
                }
            } else if self.match_available {
                let c = self.window[self.strstart - 1];
                if self.tally_lit(c) {
                    self.flush_block_only(false);
                }
                self.strstart += 1;
                self.lookahead -= 1;
            } else {
                self.match_available = true;
                self.strstart += 1;
                self.lookahead -= 1;
            }
        }
        if self.match_available {
            let c = self.window[self.strstart - 1];
            self.tally_lit(c);
            self.match_available = false;
        }
        self.insert = self.strstart.min(MIN_MATCH - 1);
        self.flush_block_only(true);
    }
}

/// Compress `data` to raw DEFLATE, byte-identical to zlib 1.3 at level 6.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut s = State::new(data);
    s.deflate_slow();
    s.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inflate::inflate;

    #[test]
    fn round_trips() {
        let mut data = Vec::new();
        for i in 0..100_000u32 {
            data.extend_from_slice(format!("<w:t>{} {}</w:t>", i % 97, i * 7919 % 1013).as_bytes());
        }
        for sample in [&b""[..], b"a", b"abcabcabcabc", &data[..]] {
            assert_eq!(inflate(&deflate(sample)).unwrap(), sample);
        }
    }

    #[test]
    fn matches_zlib() {
        // zlib.compressobj(-1, zlib.DEFLATED, -15) of b"hello hello hello world\n" * 3
        assert_eq!(deflate(b""), [3, 0]);
        let got = deflate(&b"hello hello hello world\n".repeat(3));
        assert_eq!(
            got,
            [
                203, 72, 205, 201, 201, 87, 200, 64, 34, 203, 243, 139, 114, 82, 184, 50, 72, 20,
                7, 0
            ]
        );
    }
}
