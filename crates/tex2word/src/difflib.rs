//! `difflib.SequenceMatcher(a=..., b=..., autojunk=False).get_opcodes()`
//! (no junk heuristic), for the block reconciliation.

use std::collections::HashMap;
use std::hash::Hash;

/// One opcode: `(tag, i1, i2, j1, j2)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opcode {
    pub tag: &'static str,
    pub i1: usize,
    pub i2: usize,
    pub j1: usize,
    pub j2: usize,
}

struct Matcher<'a, T> {
    a: &'a [T],
    b: &'a [T],
    b2j: HashMap<&'a T, Vec<usize>>,
}

impl<'a, T: Eq + Hash> Matcher<'a, T> {
    fn new(a: &'a [T], b: &'a [T]) -> Self {
        let mut b2j: HashMap<&T, Vec<usize>> = HashMap::new();
        for (j, x) in b.iter().enumerate() {
            b2j.entry(x).or_default().push(j);
        }
        Matcher { a, b, b2j }
    }

    fn find_longest_match(
        &self,
        alo: usize,
        ahi: usize,
        blo: usize,
        bhi: usize,
    ) -> (usize, usize, usize) {
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for i in alo..ahi {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(js) = self.b2j.get(&self.a[i]) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = if j == 0 {
                        0
                    } else {
                        j2len.get(&(j - 1)).copied().unwrap_or(0)
                    } + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }
        while besti > alo && bestj > blo && self.a[besti - 1] == self.b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && self.a[besti + bestsize] == self.b[bestj + bestsize]
        {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    }

    fn matching_blocks(&self) -> Vec<(usize, usize, usize)> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0, la, 0, lb)];
        let mut blocks = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let (i, j, k) = self.find_longest_match(alo, ahi, blo, bhi);
            if k > 0 {
                blocks.push((i, j, k));
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        blocks.sort();
        let (mut i1, mut j1, mut k1) = (0, 0, 0);
        let mut out = Vec::new();
        for (i2, j2, k2) in blocks {
            if i1 + k1 == i2 && j1 + k1 == j2 {
                k1 += k2;
            } else {
                if k1 > 0 {
                    out.push((i1, j1, k1));
                }
                (i1, j1, k1) = (i2, j2, k2);
            }
        }
        if k1 > 0 {
            out.push((i1, j1, k1));
        }
        out.push((la, lb, 0));
        out
    }
}

/// `SequenceMatcher(a=a, b=b, autojunk=False).get_opcodes()`.
pub fn get_opcodes<T: Eq + Hash>(a: &[T], b: &[T]) -> Vec<Opcode> {
    let m = Matcher::new(a, b);
    let (mut i, mut j) = (0, 0);
    let mut answer = Vec::new();
    for (ai, bj, size) in m.matching_blocks() {
        let tag = if i < ai && j < bj {
            "replace"
        } else if i < ai {
            "delete"
        } else if j < bj {
            "insert"
        } else {
            ""
        };
        if !tag.is_empty() {
            answer.push(Opcode {
                tag,
                i1: i,
                i2: ai,
                j1: j,
                j2: bj,
            });
        }
        i = ai + size;
        j = bj + size;
        if size > 0 {
            answer.push(Opcode {
                tag: "equal",
                i1: ai,
                i2: i,
                j1: bj,
                j2: j,
            });
        }
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_difflib() {
        // SequenceMatcher(a="abxcd", b="abcd", autojunk=False).get_opcodes()
        let a: Vec<char> = "abxcd".chars().collect();
        let b: Vec<char> = "abcd".chars().collect();
        let ops: Vec<_> = get_opcodes(&a, &b)
            .iter()
            .map(|o| (o.tag, o.i1, o.i2, o.j1, o.j2))
            .collect();
        assert_eq!(
            ops,
            vec![
                ("equal", 0, 2, 0, 2),
                ("delete", 2, 3, 2, 2),
                ("equal", 3, 5, 2, 4)
            ]
        );
        // SequenceMatcher(a="qabxcd", b="abycdf", autojunk=False).get_opcodes()
        let a: Vec<char> = "qabxcd".chars().collect();
        let b: Vec<char> = "abycdf".chars().collect();
        let ops: Vec<_> = get_opcodes(&a, &b)
            .iter()
            .map(|o| (o.tag, o.i1, o.i2, o.j1, o.j2))
            .collect();
        assert_eq!(
            ops,
            vec![
                ("delete", 0, 1, 0, 0),
                ("equal", 1, 3, 0, 2),
                ("replace", 3, 4, 2, 3),
                ("equal", 4, 6, 3, 5),
                ("insert", 6, 6, 5, 6)
            ]
        );
    }
}
