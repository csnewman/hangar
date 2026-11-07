#![allow(dead_code)]

use hangar_proto::{Change, ChangeSet, Text};

/// xorshift64*: deterministic, so a failing seed can be run again.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 0..n, or 0 when n is 0.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// Pieces of text that cover what positions get wrong: line breaks of each
/// kind, two-byte UTF-8, and characters outside the BMP, which are two
/// UTF-16 units.
const PIECES: &[&str] = &["a", "b", "xyz", " ", "\n", "\r\n", "é", "😀", "\t"];

pub fn string(rng: &mut Rng, max: usize) -> String {
    (0..rng.below(max + 1))
        .map(|_| PIECES[rng.below(PIECES.len())])
        .collect()
}

pub fn text(rng: &mut Rng, max: usize) -> Text {
    Text::from(string(rng, max).as_str())
}

/// Positions in `t` that do not split a surrogate pair.
pub fn boundaries(t: &Text) -> Vec<usize> {
    let u = t.units();
    (0..=u.len())
        .filter(|&i| i == u.len() || !(0xDC00..0xE000).contains(&u[i]))
        .collect()
}

/// Up to `n` replacements in `t`, in any order and possibly overlapping.
pub fn changes(rng: &mut Rng, t: &Text, n: usize) -> Vec<Change> {
    let b = boundaries(t);
    (0..rng.below(n + 1))
        .map(|_| {
            let mut from = b[rng.below(b.len())];
            let mut to = b[rng.below(b.len())];
            if from > to {
                std::mem::swap(&mut from, &mut to);
            }
            if rng.chance(30) {
                to = from;
            }
            let insert = if rng.chance(25) {
                Text::new()
            } else {
                text(rng, 4)
            };
            Change { from, to, insert }
        })
        .collect()
}

/// Up to `n` replacements in `t` in order, none overlapping, so applying
/// them one by one from the end gives what the set should.
pub fn sorted_changes(rng: &mut Rng, t: &Text, n: usize) -> Vec<Change> {
    let b = boundaries(t);
    let mut cuts: Vec<usize> = (0..rng.below(n + 1) * 2)
        .map(|_| b[rng.below(b.len())])
        .collect();
    cuts.sort_unstable();
    cuts.as_chunks::<2>()
        .0
        .iter()
        .map(|&[from, to]| Change {
            from,
            to,
            insert: if rng.chance(25) {
                Text::new()
            } else {
                text(rng, 4)
            },
        })
        .collect()
}

pub fn set(rng: &mut Rng, t: &Text, n: usize) -> ChangeSet {
    ChangeSet::of(changes(rng, t, n), t.len()).expect("changes fit the text")
}
