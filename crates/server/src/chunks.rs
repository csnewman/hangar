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
//!
//! Files are read a window at a time by position (`At`), never whole: a
//! cut looks at most MAX bytes ahead, so a window holding that much past
//! where it starts cuts as the whole file would.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// A chunk is at least this long, but a file's last.
const MIN: usize = 8 << 10;
/// Most chunks are near this long.
const AVG: usize = 32 << 10;
/// No chunk is longer.
const MAX: usize = 128 << 10;

/// How much of a file is read at a time.
const WINDOW: usize = 1 << 20;

/// Before the average, a cut needs 17 of the hash's top bits zero; after
/// it, 13.
const MASK_SMALL: u64 = !0u64 << (64 - 17);
const MASK_LARGE: u64 = !0u64 << (64 - 13);

/// What is read by position: a file, or bytes in memory.
pub trait At {
    /// Fills buf with the bytes from offset on, all of them or an error.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
}

impl At for [u8] {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(io::Error::other)?;
        let src = start
            .checked_add(buf.len())
            .and_then(|end| self.get(start..end))
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

impl At for std::fs::File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset)
    }
}

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

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// Calls f with each chunk of the first len bytes of src, in order, and
/// its offset.
fn each_chunk(src: &(impl At + ?Sized), len: u64, mut f: impl FnMut(u64, &[u8])) -> io::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(WINDOW + MAX);
    let (mut lo, mut read, mut at) = (0usize, 0u64, 0u64);
    loop {
        if buf.len() - lo < MAX && read < len {
            buf.drain(..lo);
            lo = 0;
            let n = usize::try_from((len - read).min(WINDOW as u64)).unwrap_or(WINDOW);
            let start = buf.len();
            buf.resize(start + n, 0);
            src.read_at(&mut buf[start..], read)?;
            read += n as u64;
            continue;
        }
        if lo == buf.len() {
            return Ok(());
        }
        let n = cut(&buf[lo..]);
        f(at, &buf[lo..lo + n]);
        lo += n;
        at += n as u64;
    }
}

/// Calls f with the bytes of src from offset on, len of them, a window
/// at a time.
fn each_window(
    src: &(impl At + ?Sized),
    offset: u64,
    len: u64,
    mut f: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut buf = vec![0u8; WINDOW.min(usize::try_from(len).unwrap_or(WINDOW))];
    let mut done = 0u64;
    while done < len {
        let n = usize::try_from((len - done).min(buf.len() as u64)).unwrap_or(buf.len());
        src.read_at(&mut buf[..n], offset + done)?;
        f(&buf[..n])?;
        done += n as u64;
    }
    Ok(())
}

/// The SHA-256 of src's first len bytes, in hex.
pub fn sha256(src: &(impl At + ?Sized), len: u64) -> io::Result<String> {
    let mut h = ring::digest::Context::new(&ring::digest::SHA256);
    each_window(src, 0, len, |b| {
        h.update(b);
        Ok(())
    })?;
    Ok(hex(h.finish().as_ref()))
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
    /// The base of src's first len bytes, read once.
    pub fn of(copy: &str, path: &str, src: &(impl At + ?Sized), len: u64) -> io::Result<Base> {
        let mut h = ring::digest::Context::new(&ring::digest::SHA256);
        let mut chunks = vec![];
        each_chunk(src, len, |_, c| {
            h.update(c);
            chunks.push((c.len() as u32, name(c)));
        })?;
        Ok(Base {
            copy: copy.to_string(),
            path: path.to_string(),
            digest: hex(h.finish().as_ref()),
            size: len,
            chunks,
        })
    }
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

impl Delta {
    /// Where the bytes it sends are in the file it makes, in order: each
    /// range's offset and length.
    pub fn sent_ranges(&self) -> Vec<(u64, u64)> {
        let mut out = vec![];
        let mut at = 0u64;
        for (from, len) in &self.parts {
            if from.is_none() {
                out.push((at, *len));
            }
            at += len;
        }
        out
    }
}

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

/// How to send src's first len bytes to a place that has base, read once;
/// the bytes it sends are `sent_ranges` of src. None when it would send
/// nearly the whole file anyway.
pub fn plan(base: &Base, src: &(impl At + ?Sized), len: u64) -> io::Result<Option<Delta>> {
    let mut at = HashMap::new();
    let mut offset = 0u64;
    for (n, name) in &base.chunks {
        at.entry((name.as_str(), *n)).or_insert(offset);
        offset += u64::from(*n);
    }
    let mut parts: Vec<(Option<u64>, u64)> = vec![];
    let mut sent = 0u64;
    // The older copy's size of the new file's start, hashed on the way,
    // for an append.
    let mut head = ring::digest::Context::new(&ring::digest::SHA256);
    each_chunk(src, len, |off, chunk| {
        if off < base.size {
            let n = usize::try_from(base.size - off).map_or(chunk.len(), |n| n.min(chunk.len()));
            head.update(&chunk[..n]);
        }
        let n = chunk.len() as u64;
        match at.get(&(name(chunk).as_str(), chunk.len() as u32)) {
            Some(o) if o + n <= base.size => push(&mut parts, Some(*o), n),
            _ => {
                push(&mut parts, None, n);
                sent += n;
            }
        }
    })?;
    if base.size > 0 && base.size <= len && hex(head.finish().as_ref()) == base.digest {
        // Appended to: the older copy, then what follows it.
        parts.clear();
        push(&mut parts, Some(0), base.size);
        push(&mut parts, None, len - base.size);
        sent = len - base.size;
    }
    if sent * 10 > len * 9 {
        return Ok(None);
    }
    Ok(Some(Delta {
        base: base.digest.clone(),
        parts,
        sent,
    }))
}

/// Writes the file a delta makes from base (the older copy, base_len
/// bytes) and the bytes sent (sent_len) to out, size bytes, giving its
/// SHA-256; none when they do not fit it, and then what was written is
/// to be thrown away.
pub fn rebuild(
    delta: &Delta,
    base: &(impl At + ?Sized),
    base_len: u64,
    sent: &(impl At + ?Sized),
    sent_len: u64,
    size: u64,
    out: &mut impl Write,
) -> io::Result<Option<String>> {
    if sent_len != delta.sent || sha256(base, base_len)? != delta.base {
        return Ok(None);
    }
    let mut h = ring::digest::Context::new(&ring::digest::SHA256);
    let (mut next, mut written) = (0u64, 0u64);
    for (at, len) in &delta.parts {
        let Some(end) = written.checked_add(*len).filter(|e| *e <= size) else {
            return Ok(None);
        };
        let mut copy = |b: &[u8]| {
            h.update(b);
            out.write_all(b)
        };
        match at {
            Some(at) => {
                if at.checked_add(*len).is_none_or(|e| e > base_len) {
                    return Ok(None);
                }
                each_window(base, *at, *len, &mut copy)?;
            }
            None => {
                if next + len > sent_len {
                    return Ok(None);
                }
                each_window(sent, next, *len, &mut copy)?;
                next += len;
            }
        }
        written = end;
    }
    Ok((next == sent_len && written == size).then(|| hex(h.finish().as_ref())))
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

    fn base(data: &[u8]) -> Base {
        Base::of("profile", "~/f", data, data.len() as u64).unwrap()
    }

    /// The bytes a delta sends of new.
    fn sent(delta: &Delta, new: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        for (at, len) in delta.sent_ranges() {
            out.extend_from_slice(&new[at as usize..(at + len) as usize]);
        }
        out
    }

    fn round_trip(old: &[u8], new: &[u8]) -> u64 {
        let delta = plan(&base(old), new, new.len() as u64)
            .unwrap()
            .expect("worth a delta");
        let s = sent(&delta, new);
        let mut made = vec![];
        let digest = rebuild(
            &delta,
            old,
            old.len() as u64,
            &s[..],
            s.len() as u64,
            new.len() as u64,
            &mut made,
        )
        .unwrap()
        .expect("rebuilt");
        assert_eq!(made, new);
        assert_eq!(digest, sha256(new, new.len() as u64).unwrap());
        delta.sent
    }

    /// The chunks of data cut from it whole, as one cut each in turn.
    fn whole_cuts(data: &[u8]) -> Vec<usize> {
        let mut out = vec![];
        let mut rest = data;
        while !rest.is_empty() {
            let n = cut(rest);
            out.push(n);
            rest = &rest[n..];
        }
        out
    }

    #[test]
    fn chunks_are_cut_within_their_bounds_and_cover_the_file() {
        let data = noise(2 << 20, 1);
        let cs = base(&data).chunks;
        let total: u64 = cs.iter().map(|(n, _)| u64::from(*n)).sum();
        assert_eq!(total, data.len() as u64);
        for (n, _) in &cs[..cs.len() - 1] {
            assert!((MIN..=MAX).contains(&(*n as usize)), "{n}");
        }
        let avg = data.len() / cs.len();
        assert!((AVG / 2..AVG * 2).contains(&avg), "average {avg}");
    }

    #[test]
    fn a_file_read_a_window_at_a_time_cuts_as_it_does_whole() {
        let data = noise(5 * WINDOW + 12_345, 8);
        let mut windowed = vec![];
        each_chunk(&data[..], data.len() as u64, |_, c| windowed.push(c.len())).unwrap();
        assert_eq!(windowed, whole_cuts(&data));
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
        let old = noise(3 << 20, 3);
        let mut new = old.clone();
        new.splice(
            2_500_000..2_500_010,
            b"inserted, a little longer".iter().copied(),
        );
        let sent = round_trip(&old, &new);
        assert!(sent < 3 * MAX as u64, "sent {sent}");
    }

    #[test]
    fn a_file_unlike_its_base_goes_whole() {
        let old = noise(1 << 20, 4);
        let new = noise(1 << 20, 5);
        assert!(
            plan(&base(&old), &new[..], new.len() as u64)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_delta_against_another_base_rebuilds_nothing() {
        let old = noise(1 << 20, 6);
        let mut new = old.clone();
        new.extend_from_slice(b"tail");
        let delta = plan(&base(&old), &new[..], new.len() as u64)
            .unwrap()
            .expect("a delta");
        let s = sent(&delta, &new);
        let other = noise(1 << 20, 7);
        let rebuilt = rebuild(
            &delta,
            &other[..],
            other.len() as u64,
            &s[..],
            s.len() as u64,
            new.len() as u64,
            &mut vec![],
        )
        .unwrap();
        assert!(rebuilt.is_none());
    }
}
