//! Content-defined chunks of a file, so that a file changed a little goes
//! to a linked place as the little that changed (docs/places.md §4.4).
//! The place that has an older copy of a file lists its chunks (`Base`);
//! the place sending the newer one cuts it the same way and sends only the
//! chunks the list lacks, the rest named by where they are in the older
//! copy (`plan`); the place receiving rebuilds the file from the two
//! (`rebuild`). An append is sent as the bytes appended.
//!
//! Chunks are cut by FastCDC (Xia et al., "The Design of Fast
//! Content-Defined Chunking for Data Deduplication Based Storage
//! Systems", 2020): a gear hash over the bytes, a cut where its top bits
//! are zero, with more of them asked for before the average size and
//! fewer after, so most chunks are near it. An insertion moves the cuts
//! near it alone, and the chunks after it are found again. Both places
//! cut alike or the chunks do not match, which costs bytes, not
//! correctness: what is rebuilt is checked against its SHA-256.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// A chunk is at least this long, but a file's last.
const MIN: usize = 8 << 10;
/// Most chunks are near this long.
const AVG: usize = 32 << 10;
/// No chunk is longer.
const MAX: usize = 128 << 10;

/// Before the average, a cut needs 17 of the hash's top bits zero; after
/// it, 13.
const MASK_SMALL: u64 = !0u64 << (64 - 17);
const MASK_LARGE: u64 = !0u64 << (64 - 13);

/// The gear table: 256 fixed pseudo-random words, the same in every
/// place (splitmix64 from a fixed seed).
fn gear() -> &'static [u64; 256] {
    static G: OnceLock<[u64; 256]> = OnceLock::new();
    G.get_or_init(|| {
        let mut x: u64 = 0x6861_6e67_6172_2d31;
        let mut g = [0u64; 256];
        for v in &mut g {
            x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            *v = z ^ (z >> 31);
        }
        g
    })
}

/// Where the first chunk of data ends.
fn cut(data: &[u8]) -> usize {
    let n = data.len();
    if n <= MIN {
        return n;
    }
    let g = gear();
    let (normal, max) = (n.min(AVG), n.min(MAX));
    let mut h: u64 = 0;
    let mut i = MIN;
    while i < normal {
        h = (h << 1).wrapping_add(g[data[i] as usize]);
        if h & MASK_SMALL == 0 {
            return i + 1;
        }
        i += 1;
    }
    while i < max {
        h = (h << 1).wrapping_add(g[data[i] as usize]);
        if h & MASK_LARGE == 0 {
            return i + 1;
        }
        i += 1;
    }
    max
}

/// A chunk's name: the first half of its SHA-256, in hex.
fn name(chunk: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, chunk).as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A file's chunks, in order: each one's length and name.
pub fn chunks(data: &[u8]) -> Vec<(u32, String)> {
    let mut out = vec![];
    let mut rest = data;
    while !rest.is_empty() {
        let n = cut(rest);
        out.push((n as u32, name(&rest[..n])));
        rest = &rest[n..];
    }
    out
}

/// What a place has of a file, for the place sending it to send what it
/// lacks: the copy and path, its content's SHA-256 and size, and its
/// chunks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Base {
    pub copy: String,
    pub path: String,
    pub digest: String,
    pub size: u64,
    pub chunks: Vec<(u32, String)>,
}

impl Base {
    pub fn of(copy: &str, path: &str, data: &[u8]) -> Base {
        Base {
            copy: copy.to_string(),
            path: path.to_string(),
            digest: sha256(data),
            size: data.len() as u64,
            chunks: chunks(data),
        }
    }
}

fn sha256(data: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, data)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// How a file is made from an older copy of it: in order, each part
/// either the bytes of the older copy at an offset (at), or the next
/// bytes sent (no at).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    /// The SHA-256 of the older copy it is made from.
    pub base: String,
    pub parts: Vec<(Option<u64>, u64)>,
    /// How many bytes are sent: the parts without an offset, together.
    pub sent: u64,
}

/// How to send data to a place that has base: the delta and the bytes it
/// sends. None when it would send nearly the whole file anyway.
pub fn plan(base: &Base, data: &[u8]) -> Option<(Delta, Vec<u8>)> {
    /// Adds a part, joined to the one before when they run on.
    fn push(parts: &mut Vec<(Option<u64>, u64)>, at: Option<u64>, len: u64) {
        if len == 0 {
            return;
        }
        if let Some(last) = parts.last_mut() {
            let runs_on = match (last.0, at) {
                (Some(a), Some(b)) => a + last.1 == b,
                (None, None) => true,
                _ => false,
            };
            if runs_on {
                last.1 += len;
                return;
            }
        }
        parts.push((at, len));
    }
    let mut parts: Vec<(Option<u64>, u64)> = vec![];
    let mut sent = vec![];
    let size = base.size as usize;
    if size <= data.len() && size > 0 && sha256(&data[..size]) == base.digest {
        // Appended to: the older copy, then what follows it.
        push(&mut parts, Some(0), base.size);
        push(&mut parts, None, (data.len() - size) as u64);
        sent.extend_from_slice(&data[size..]);
    } else {
        let mut at = HashMap::new();
        let mut offset = 0u64;
        for (len, name) in &base.chunks {
            at.entry((name.as_str(), *len)).or_insert(offset);
            offset += u64::from(*len);
        }
        let mut rest = data;
        while !rest.is_empty() {
            let n = cut(rest);
            let chunk = &rest[..n];
            match at.get(&(name(chunk).as_str(), n as u32)) {
                Some(o) if o + n as u64 <= base.size => push(&mut parts, Some(*o), n as u64),
                _ => {
                    push(&mut parts, None, n as u64);
                    sent.extend_from_slice(chunk);
                }
            }
            rest = &rest[n..];
        }
    }
    if sent.len() as u64 * 10 > data.len() as u64 * 9 {
        return None;
    }
    let delta = Delta {
        base: base.digest.clone(),
        parts,
        sent: sent.len() as u64,
    };
    Some((delta, sent))
}

/// The file a delta makes from base (the older copy) and the bytes sent;
/// none when they do not fit it.
pub fn rebuild(delta: &Delta, base: &[u8], sent: &[u8], size: u64) -> Option<Vec<u8>> {
    if sha256(base) != delta.base || sent.len() as u64 != delta.sent {
        return None;
    }
    let mut out = Vec::with_capacity(usize::try_from(size).ok()?);
    let mut next = 0usize;
    for (at, len) in &delta.parts {
        let len = usize::try_from(*len).ok()?;
        match at {
            Some(at) => {
                let at = usize::try_from(*at).ok()?;
                out.extend_from_slice(base.get(at..at.checked_add(len)?)?);
            }
            None => {
                out.extend_from_slice(sent.get(next..next.checked_add(len)?)?);
                next += len;
            }
        }
        if out.len() as u64 > size {
            return None;
        }
    }
    (next == sent.len() && out.len() as u64 == size).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes that look random but are the same every run.
    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    fn round_trip(old: &[u8], new: &[u8]) -> u64 {
        let base = Base::of("profile", "~/f", old);
        let (delta, sent) = plan(&base, new).expect("worth a delta");
        let made = rebuild(&delta, old, &sent, new.len() as u64).expect("rebuilt");
        assert_eq!(made, new);
        delta.sent
    }

    #[test]
    fn chunks_are_cut_within_their_bounds_and_cover_the_file() {
        let data = noise(2 << 20, 1);
        let cs = chunks(&data);
        let total: u64 = cs.iter().map(|(n, _)| u64::from(*n)).sum();
        assert_eq!(total, data.len() as u64);
        for (n, _) in &cs[..cs.len() - 1] {
            assert!((MIN..=MAX).contains(&(*n as usize)), "{n}");
        }
        let avg = data.len() / cs.len();
        assert!((AVG / 2..AVG * 2).contains(&avg), "average {avg}");
    }

    #[test]
    fn an_append_sends_what_was_appended() {
        let old = noise(1 << 20, 2);
        let mut new = old.clone();
        new.extend_from_slice(b"one more line\n");
        assert_eq!(round_trip(&old, &new), 14);
    }

    #[test]
    fn an_edit_sends_the_chunks_around_it() {
        let old = noise(1 << 20, 3);
        let mut new = old.clone();
        new.splice(
            500_000..500_010,
            b"inserted, a little longer".iter().copied(),
        );
        let sent = round_trip(&old, &new);
        assert!(sent < 3 * MAX as u64, "sent {sent}");
    }

    #[test]
    fn a_file_unlike_its_base_goes_whole() {
        let base = Base::of("profile", "~/f", &noise(1 << 20, 4));
        assert!(plan(&base, &noise(1 << 20, 5)).is_none());
    }

    #[test]
    fn a_delta_against_another_base_rebuilds_nothing() {
        let old = noise(1 << 20, 6);
        let mut new = old.clone();
        new.extend_from_slice(b"tail");
        let (delta, sent) = plan(&Base::of("profile", "~/f", &old), &new).expect("a delta");
        let other = noise(1 << 20, 7);
        assert!(rebuild(&delta, &other, &sent, new.len() as u64).is_none());
    }
}
