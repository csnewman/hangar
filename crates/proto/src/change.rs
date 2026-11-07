//! Change sets, ported from `@codemirror/state` (MIT, Marijn Haverbeke and
//! others) so that both ends of the protocol, and a CodeMirror editor, compose
//! and map changes identically: the same inputs give the same sections.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::error::Error;
use crate::text::Text;

/// How [`ChangeSet::map_pos`] treats a position whose surroundings were
/// deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapMode {
    /// Always map to a position, even when its context is gone.
    Simple,
    /// None when a deletion spans the position.
    TrackDel,
    /// None when the character before the position is deleted.
    TrackBefore,
    /// None when the character after the position is deleted.
    TrackAfter,
}

/// One replacement in a text: `from` to `to` becomes `insert`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub from: usize,
    pub to: usize,
    pub insert: Text,
}

/// A set of changes to a text of one particular length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    // Pairs: a length in the text before, then -1 when that stretch is
    // kept, else the length of what replaces it. An insertion is (0, n), a
    // deletion (n, 0).
    sections: Vec<i64>,
    // What each changed section inserts, by section index; a section past
    // the end inserts nothing.
    inserted: Vec<Text>,
}

/// JSON numbers beyond this are not exact in JavaScript.
const MAX_SAFE: u64 = (1 << 53) - 1;

impl ChangeSet {
    /// No change to a text of `len`.
    pub fn empty(len: usize) -> Self {
        let sections = if len > 0 {
            vec![len as i64, -1]
        } else {
            vec![]
        };
        ChangeSet {
            sections,
            inserted: vec![],
        }
    }

    /// The changes, each placed in the text of `len` before any of them,
    /// in any order. Changes out of order, or overlapping, are applied as
    /// if each came after the ones before it in the list.
    pub fn of(changes: impl IntoIterator<Item = Change>, len: usize) -> Result<Self, Error> {
        let length = len as i64;
        let mut b = Builder {
            sections: vec![],
            inserted: vec![],
            pos: 0,
            length,
            total: None,
        };
        for c in changes {
            if c.from > c.to || c.to > len {
                return Err(Error::Range { pos: c.to, len });
            }
            let (from, to) = (c.from as i64, c.to as i64);
            let ins = c.insert.len() as i64;
            if from == to && ins == 0 {
                continue;
            }
            if from < b.pos {
                b.flush(false)?;
            }
            if from > b.pos {
                add_section(&mut b.sections, from - b.pos, -1, false);
            }
            add_section(&mut b.sections, to - from, ins, false);
            add_insert(&mut b.inserted, &b.sections, c.insert);
            b.pos = to;
        }
        let force = b.total.is_none();
        b.flush(force)?;
        Ok(b.total.unwrap_or_default())
    }

    /// The length of the text the changes apply to.
    pub fn len(&self) -> usize {
        self.sections
            .iter()
            .step_by(2)
            .fold(0i64, |n, &len| n.saturating_add(len)) as usize
    }

    /// The length of the text after them.
    pub fn new_len(&self) -> usize {
        self.sections
            .as_chunks::<2>()
            .0
            .iter()
            .map(|s| if s[1] < 0 { s[0] } else { s[1] })
            .fold(0i64, i64::saturating_add) as usize
    }

    /// True when nothing changes.
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty() || self.sections.len() == 2 && self.sections[1] < 0
    }

    pub fn apply(&self, doc: &Text) -> Result<Text, Error> {
        if self.len() != doc.len() {
            return Err(Error::Length {
                expected: self.len(),
                got: doc.len(),
            });
        }
        let units = doc.units();
        let mut out = Text::with_capacity(self.new_len());
        let mut pos = 0usize;
        for (i, s) in self.sections.as_chunks::<2>().0.iter().enumerate() {
            let (len, ins) = (s[0] as usize, s[1]);
            if ins < 0 {
                out.push_units(&units[pos..pos + len]);
            } else if ins > 0
                && let Some(t) = self.inserted.get(i)
            {
                out.append(t);
            }
            pos += len;
        }
        Ok(out)
    }

    /// The changes that undo these, given the text they were applied to.
    pub fn invert(&self, doc: &Text) -> Result<ChangeSet, Error> {
        if self.len() != doc.len() {
            return Err(Error::Length {
                expected: self.len(),
                got: doc.len(),
            });
        }
        let mut sections = self.sections.clone();
        let mut inserted = vec![];
        let mut pos = 0;
        for i in (0..sections.len()).step_by(2) {
            let (len, ins) = (sections[i], sections[i + 1]);
            if ins >= 0 {
                sections[i] = ins;
                sections[i + 1] = len;
                let index = i >> 1;
                while inserted.len() < index {
                    inserted.push(Text::new());
                }
                inserted.push(if len > 0 {
                    doc.slice(pos as usize, (pos + len) as usize)
                } else {
                    Text::new()
                });
            }
            pos += len;
        }
        Ok(ChangeSet { sections, inserted })
    }

    /// These changes followed by `other`, which applies to the text these
    /// produce, as one set.
    pub fn compose(&self, other: &ChangeSet) -> Result<ChangeSet, Error> {
        if self.new_len() != other.len() {
            return Err(Error::Length {
                expected: other.len(),
                got: self.new_len(),
            });
        }
        if self.is_empty() {
            return Ok(other.clone());
        }
        if other.is_empty() {
            return Ok(self.clone());
        }
        compose_sets(self, other)
    }

    /// These changes moved to apply after `other`, which starts from the
    /// same text. With `before`, these are ordered first where the two
    /// insert at one position; otherwise `other` is. For changes A and B,
    /// `A.compose(B.map(A))` and `B.compose(A.map(B, true))` give the same
    /// text.
    pub fn map(&self, other: &ChangeSet, before: bool) -> Result<ChangeSet, Error> {
        if self.len() != other.len() {
            return Err(Error::Length {
                expected: self.len(),
                got: other.len(),
            });
        }
        if other.is_empty() {
            return Ok(self.clone());
        }
        map_set(self, other, before)
    }

    /// Where `pos` lands after the changes. A negative `assoc` keeps it
    /// before text inserted at it, a positive one after.
    pub fn map_pos(&self, pos: usize, assoc: i32, mode: MapMode) -> Result<Option<usize>, Error> {
        let p = pos as i64;
        let (mut pos_a, mut pos_b) = (0i64, 0i64);
        for s in self.sections.as_chunks::<2>().0 {
            let (len, ins) = (s[0], s[1]);
            let end_a = pos_a + len;
            if ins < 0 {
                if end_a > p {
                    return Ok(Some((pos_b + (p - pos_a)) as usize));
                }
                pos_b += len;
            } else {
                if mode != MapMode::Simple
                    && end_a >= p
                    && (mode == MapMode::TrackDel && pos_a < p && end_a > p
                        || mode == MapMode::TrackBefore && pos_a < p
                        || mode == MapMode::TrackAfter && end_a > p)
                {
                    return Ok(None);
                }
                if end_a > p || end_a == p && assoc < 0 && len == 0 {
                    return Ok(Some(if p == pos_a || assoc < 0 {
                        pos_b as usize
                    } else {
                        (pos_b + ins) as usize
                    }));
                }
                pos_b += ins;
            }
            pos_a = end_a;
        }
        if p > pos_a {
            return Err(Error::Range {
                pos,
                len: pos_a as usize,
            });
        }
        Ok(Some(pos_b as usize))
    }

    /// Calls `f(from_a, to_a, from_b, to_b, inserted)` for each changed
    /// range: `from_a..to_a` in the text before, replaced by `inserted` at
    /// `from_b..to_b` after. Adjacent changes are reported apart only with
    /// `individual`.
    pub fn iter_changes(
        &self,
        individual: bool,
        mut f: impl FnMut(usize, usize, usize, usize, &Text),
    ) {
        let s = &self.sections;
        let (mut pos_a, mut pos_b, mut i) = (0i64, 0i64, 0usize);
        while i < s.len() {
            let (mut len, mut ins) = (s[i], s[i + 1]);
            i += 2;
            if ins < 0 {
                pos_a += len;
                pos_b += len;
                continue;
            }
            let (mut end_a, mut end_b, mut text) = (pos_a, pos_b, Text::new());
            loop {
                end_a += len;
                end_b += ins;
                if ins != 0
                    && let Some(t) = self.inserted.get((i - 2) >> 1)
                {
                    text.append(t);
                }
                if individual || i == s.len() || s[i + 1] < 0 {
                    break;
                }
                len = s[i];
                ins = s[i + 1];
                i += 2;
            }
            f(
                pos_a as usize,
                end_a as usize,
                pos_b as usize,
                end_b as usize,
                &text,
            );
            pos_a = end_a;
            pos_b = end_b;
        }
    }

    /// CodeMirror's JSON form: a number keeps that many units; `[n]`
    /// deletes n; `[n, line, ...]` replaces n with the lines.
    pub fn to_json(&self) -> Value {
        let parts = self
            .sections
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let (len, ins) = (s[0], s[1]);
                if ins < 0 {
                    Value::from(len)
                } else if ins == 0 {
                    Value::from(vec![Value::from(len)])
                } else {
                    let mut part = vec![Value::from(len)];
                    let lines = self.inserted.get(i).map(Text::lines).unwrap_or_default();
                    part.extend(lines.into_iter().map(Value::from));
                    Value::Array(part)
                }
            })
            .collect();
        Value::Array(parts)
    }

    pub fn from_json(json: &Value) -> Result<ChangeSet, Error> {
        let parts = json
            .as_array()
            .ok_or_else(|| Error::Json("not an array".into()))?;
        let mut sections = Vec::with_capacity(parts.len() * 2);
        let mut inserted = vec![];
        for (i, part) in parts.iter().enumerate() {
            if let Some(n) = part.as_u64() {
                sections.push(safe(n)?);
                sections.push(-1);
                continue;
            }
            let bad = || Error::Json(format!("part {i} is not a length or [length, ...lines]"));
            let arr = part.as_array().ok_or_else(bad)?;
            let len = arr.first().and_then(Value::as_u64).ok_or_else(bad)?;
            if arr.len() == 1 {
                sections.push(safe(len)?);
                sections.push(0);
                continue;
            }
            let lines = arr[1..]
                .iter()
                .map(|v| v.as_str().ok_or_else(bad))
                .collect::<Result<Vec<_>, _>>()?;
            let text = Text::from_lines(&lines);
            while inserted.len() < i {
                inserted.push(Text::new());
            }
            sections.push(safe(len)?);
            sections.push(text.len() as i64);
            inserted.push(text);
        }
        Ok(ChangeSet { sections, inserted })
    }
}

fn safe(n: u64) -> Result<i64, Error> {
    if n > MAX_SAFE {
        return Err(Error::Json(format!("length {n} is too large")));
    }
    Ok(n as i64)
}

impl Serialize for ChangeSet {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(s)
    }
}

impl<'de> Deserialize<'de> for ChangeSet {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        ChangeSet::from_json(&v).map_err(D::Error::custom)
    }
}

struct Builder {
    sections: Vec<i64>,
    inserted: Vec<Text>,
    pos: i64,
    length: i64,
    total: Option<ChangeSet>,
}

impl Builder {
    fn flush(&mut self, force: bool) -> Result<(), Error> {
        if !force && self.sections.is_empty() {
            return Ok(());
        }
        if self.pos < self.length {
            add_section(&mut self.sections, self.length - self.pos, -1, false);
        }
        let set = ChangeSet {
            sections: std::mem::take(&mut self.sections),
            inserted: std::mem::take(&mut self.inserted),
        };
        self.total = Some(match self.total.take() {
            Some(t) => t.compose(&set.map(&t, false)?)?,
            None => set,
        });
        self.pos = 0;
        Ok(())
    }
}

fn add_section(sections: &mut Vec<i64>, len: i64, ins: i64, force_join: bool) {
    if len == 0 && ins <= 0 {
        return;
    }
    let n = sections.len();
    if n >= 2 && ins <= 0 && ins == sections[n - 1] {
        sections[n - 2] += len;
    } else if n >= 2 && len == 0 && sections[n - 2] == 0 {
        sections[n - 1] += ins;
    } else if force_join && n >= 2 {
        sections[n - 2] += len;
        sections[n - 1] += ins;
    } else {
        sections.push(len);
        sections.push(ins);
    }
}

fn add_insert(values: &mut Vec<Text>, sections: &[i64], value: Text) {
    if value.is_empty() {
        return;
    }
    let index = (sections.len() - 2) >> 1;
    if index < values.len() {
        if let Some(last) = values.last_mut() {
            last.append(&value);
        }
    } else {
        while values.len() < index {
            values.push(Text::new());
        }
        values.push(value);
    }
}

fn map_set(set_a: &ChangeSet, set_b: &ChangeSet, before: bool) -> Result<ChangeSet, Error> {
    let mismatch = || Error::Length {
        expected: set_a.len(),
        got: set_b.len(),
    };
    let mut sections = vec![];
    let mut insert = vec![];
    let mut a = SectionIter::new(set_a);
    let mut b = SectionIter::new(set_b);
    // Which of A's sections has had its insertion written already, for one
    // that B's changes split.
    let mut inserted: i64 = -1;
    loop {
        if a.done() && b.len != 0 || b.done() && a.len != 0 {
            return Err(mismatch());
        } else if a.ins == -1 && b.ins == -1 {
            let len = a.len.min(b.len);
            add_section(&mut sections, len, -1, false);
            a.forward(len);
            b.forward(len);
        } else if b.ins >= 0
            && (a.ins < 0
                || inserted == a.i as i64
                || a.off == 0 && (b.len < a.len || b.len == a.len && !before))
        {
            // A change in B that comes first: skip it, along with what of A
            // it covers.
            let mut len = b.len;
            add_section(&mut sections, b.ins, -1, false);
            while len != 0 {
                if a.done() {
                    return Err(mismatch());
                }
                let piece = a.len.min(len);
                if a.ins >= 0 && inserted < a.i as i64 && a.len <= piece {
                    add_section(&mut sections, 0, a.ins, false);
                    add_insert(&mut insert, &sections, a.text());
                    inserted = a.i as i64;
                }
                a.forward(piece);
                len -= piece;
            }
            b.next();
        } else if a.ins >= 0 {
            // A's change, up to where the next of B's changes that is not a
            // deletion starts.
            let (mut len, mut left) = (0, a.len);
            while left != 0 {
                if b.ins == -1 {
                    let piece = left.min(b.len);
                    len += piece;
                    left -= piece;
                    b.forward(piece);
                } else if b.ins == 0 && b.len < left {
                    left -= b.len;
                    b.next();
                } else {
                    break;
                }
            }
            add_section(
                &mut sections,
                len,
                if inserted < a.i as i64 { a.ins } else { 0 },
                false,
            );
            if inserted < a.i as i64 {
                add_insert(&mut insert, &sections, a.text());
            }
            inserted = a.i as i64;
            a.forward(a.len - left);
        } else if a.done() && b.done() {
            return Ok(ChangeSet {
                sections,
                inserted: insert,
            });
        } else {
            return Err(mismatch());
        }
    }
}

fn compose_sets(set_a: &ChangeSet, set_b: &ChangeSet) -> Result<ChangeSet, Error> {
    let mut sections = vec![];
    let mut insert = vec![];
    let mut a = SectionIter::new(set_a);
    let mut b = SectionIter::new(set_b);
    let mut open = false;
    loop {
        if a.done() && b.done() {
            return Ok(ChangeSet {
                sections,
                inserted: insert,
            });
        } else if a.ins == 0 {
            // A deletion in A.
            add_section(&mut sections, a.len, 0, open);
            a.next();
        } else if b.len == 0 && !b.done() {
            // An insertion in B.
            add_section(&mut sections, 0, b.ins, open);
            add_insert(&mut insert, &sections, b.text());
            b.next();
        } else if a.done() || b.done() {
            return Err(Error::Length {
                expected: set_b.len(),
                got: set_a.new_len(),
            });
        } else {
            let len = a.len2().min(b.len);
            let before = sections.len();
            if a.ins == -1 {
                let ins_b = if b.ins == -1 {
                    -1
                } else if b.off != 0 {
                    0
                } else {
                    b.ins
                };
                add_section(&mut sections, len, ins_b, open);
                if ins_b != 0 {
                    add_insert(&mut insert, &sections, b.text());
                }
            } else if b.ins == -1 {
                add_section(&mut sections, if a.off != 0 { 0 } else { a.len }, len, open);
                add_insert(&mut insert, &sections, a.text_bit(len));
            } else {
                add_section(
                    &mut sections,
                    if a.off != 0 { 0 } else { a.len },
                    if b.off != 0 { 0 } else { b.ins },
                    open,
                );
                if b.off == 0 {
                    add_insert(&mut insert, &sections, b.text());
                }
            }
            open = (a.ins > len || b.ins >= 0 && b.len > len) && (open || sections.len() > before);
            a.forward2(len);
            b.forward(len);
        }
    }
}

struct SectionIter<'a> {
    set: &'a ChangeSet,
    i: usize,
    len: i64,
    // -2 once past the last section.
    ins: i64,
    off: i64,
}

impl<'a> SectionIter<'a> {
    fn new(set: &'a ChangeSet) -> Self {
        let mut it = SectionIter {
            set,
            i: 0,
            len: 0,
            ins: 0,
            off: 0,
        };
        it.next();
        it
    }

    fn next(&mut self) {
        let s = &self.set.sections;
        if self.i < s.len() {
            self.len = s[self.i];
            self.ins = s[self.i + 1];
            self.i += 2;
        } else {
            self.len = 0;
            self.ins = -2;
        }
        self.off = 0;
    }

    fn done(&self) -> bool {
        self.ins == -2
    }

    fn len2(&self) -> i64 {
        if self.ins < 0 { self.len } else { self.ins }
    }

    fn text(&self) -> Text {
        let index = self.i.saturating_sub(2) >> 1;
        self.set.inserted.get(index).cloned().unwrap_or_default()
    }

    fn text_bit(&self, len: i64) -> Text {
        let index = self.i.saturating_sub(2) >> 1;
        match self.set.inserted.get(index) {
            Some(t) => t.slice(self.off as usize, (self.off + len) as usize),
            None => Text::new(),
        }
    }

    fn forward(&mut self, len: i64) {
        if len == self.len {
            self.next();
        } else {
            self.len -= len;
            self.off += len;
        }
    }

    fn forward2(&mut self, len: i64) {
        if self.ins == -1 {
            self.forward(len);
        } else if len == self.ins {
            self.next();
        } else {
            self.ins -= len;
            self.off += len;
        }
    }
}
