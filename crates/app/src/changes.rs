//! The changes in a file against git's index, as the gutter marks them and
//! IntelliJ rolls them back or stages them alone: runs of lines added,
//! changed or taken away, by line.

use std::ops::Range;

use similar::{DiffOp, TextDiff};

/// What a change did to its lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Added,
    Modified,
    Deleted,
}

/// One change: its lines in the index's copy and in the file, 0-based and
/// half open. A deletion has no lines in the file: its range is empty, at
/// the line after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old: Range<usize>,
    pub new: Range<usize>,
}

impl Hunk {
    pub fn kind(&self) -> Kind {
        match (self.old.is_empty(), self.new.is_empty()) {
            (true, _) => Kind::Added,
            (_, true) => Kind::Deleted,
            _ => Kind::Modified,
        }
    }

    /// Whether the change is on a line of the file: its lines, or for a
    /// deletion the line it is marked on.
    pub fn covers(&self, line: usize) -> bool {
        if self.new.is_empty() {
            line == self.new.start
        } else {
            self.new.contains(&line)
        }
    }
}

/// Each line with its ending.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// The changes from base to text, each a run of lines unequal on either
/// side.
pub fn hunks(base: &str, text: &str) -> Vec<Hunk> {
    let diff = TextDiff::from_lines(base, text);
    let mut out: Vec<Hunk> = vec![];
    for op in diff.ops() {
        if let DiffOp::Equal { .. } = op {
            continue;
        }
        let (old, new) = (op.old_range(), op.new_range());
        match out.last_mut() {
            Some(h) if h.old.end == old.start && h.new.end == new.start => {
                h.old.end = old.end;
                h.new.end = new.end;
            }
            _ => out.push(Hunk { old, new }),
        }
    }
    out
}

/// The file with a change rolled back: the index's lines in its place.
pub fn rolled_back(base: &str, text: &str, h: &Hunk) -> String {
    let (b, t) = (lines(base), lines(text));
    let mut out = String::with_capacity(text.len());
    out.extend(t[..h.new.start.min(t.len())].iter().copied());
    out.extend(
        b[h.old.start.min(b.len())..h.old.end.min(b.len())]
            .iter()
            .copied(),
    );
    out.extend(t[h.new.end.min(t.len())..].iter().copied());
    out
}

/// What the index holds once the change alone is staged: the file's lines
/// in place of the index's.
pub fn staged(base: &str, text: &str, h: &Hunk) -> String {
    let (b, t) = (lines(base), lines(text));
    let mut out = String::with_capacity(base.len());
    out.extend(b[..h.old.start.min(b.len())].iter().copied());
    out.extend(
        t[h.new.start.min(t.len())..h.new.end.min(t.len())]
            .iter()
            .copied(),
    );
    out.extend(b[h.old.end.min(b.len())..].iter().copied());
    out
}

/// The index's lines a change replaced, for showing what was there.
pub fn before(base: &str, h: &Hunk) -> String {
    let b = lines(base);
    b[h.old.start.min(b.len())..h.old.end.min(b.len())].concat()
}
