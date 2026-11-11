//! Selecting a word's other occurrences, as IntelliJ's Add Selection for
//! Next Occurrence (⌃G) and Select All Occurrences (⌃⌘G) do: from the word
//! at the caret, whole words only; from text selected, wherever it is.

use std::ops::Range;

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The word the caret is in or just after, in bytes; None between words.
pub fn word_range(text: &str, at: usize) -> Option<Range<usize>> {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(at, |(i, _)| i);
    let end = text[at..]
        .char_indices()
        .find(|(_, c)| !is_word(*c))
        .map_or(text.len(), |(i, _)| at + i);
    (start < end).then_some(start..end)
}

/// Every place needle is found, in order, not overlapping; with whole, only
/// where no word character touches it on either side.
pub fn all(text: &str, needle: &str, whole: bool) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return vec![];
    }
    let mut out = vec![];
    let mut from = 0;
    while let Some(i) = text[from..].find(needle) {
        let r = from + i..from + i + needle.len();
        let alone = !whole
            || (!text[..r.start].chars().next_back().is_some_and(is_word)
                && !text[r.end..].chars().next().is_some_and(is_word));
        if alone {
            out.push(r.clone());
            from = r.end;
        } else {
            from = r.start + needle.chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// The next place needle is found after `after`, round to the start once
/// the end is reached, leaving out the places taken.
pub fn next(
    text: &str,
    needle: &str,
    after: usize,
    taken: &[Range<usize>],
    whole: bool,
) -> Option<Range<usize>> {
    let found = all(text, needle, whole);
    let free = |r: &&Range<usize>| !taken.contains(r);
    found
        .iter()
        .filter(free)
        .find(|r| r.start >= after)
        .or_else(|| found.iter().find(free))
        .cloned()
}

/// The selections Add Selection for Next Occurrence has made: the first the
/// one it started from, which the editor keeps as its own, and whether they
/// are whole words, as when it started from the word at the caret.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Occurrences {
    pub ranges: Vec<Range<usize>>,
    pub whole: bool,
}

/// What the editor is to do with its selections.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Select this alone.
    Select(Range<usize>),
    /// Add this to the selections.
    Add(Range<usize>),
    /// Select the first alone, then add the rest, in order.
    Set(Vec<Range<usize>>),
}

impl Occurrences {
    /// Add Selection for Next Occurrence, with the editor's own selection
    /// active: the word at the caret first, then each next place its text
    /// is found. A selection other than the one this started from starts
    /// it again.
    pub fn next(&mut self, text: &str, active: Range<usize>) -> Option<Step> {
        if active.is_empty() {
            let word = word_range(text, active.start)?;
            *self = Occurrences {
                ranges: vec![word.clone()],
                whole: true,
            };
            return Some(Step::Select(word));
        }
        if self.ranges.first() != Some(&active) {
            *self = Occurrences {
                ranges: vec![active.clone()],
                whole: false,
            };
        }
        let after = self.ranges.last().map_or(active.end, |r| r.end);
        let found = next(text, text.get(active)?, after, &self.ranges, self.whole)?;
        self.ranges.push(found.clone());
        Some(Step::Add(found))
    }

    /// Unselect Occurrence: the last added taken away.
    pub fn unselect(&mut self, active: Range<usize>) -> Option<Step> {
        if self.ranges.len() < 2 || self.ranges.first() != Some(&active) {
            return None;
        }
        self.ranges.pop();
        Some(Step::Set(self.ranges.clone()))
    }

    /// Select All Occurrences of the selection's text, or of the word at
    /// the caret. The nearest after the editor's own selection comes last,
    /// as the view goes to each as it is added.
    pub fn all(&mut self, text: &str, active: Range<usize>) -> Option<Step> {
        let (active, whole) = if active.is_empty() {
            (word_range(text, active.start)?, true)
        } else {
            let whole = self.ranges.first() == Some(&active) && self.whole;
            (active, whole)
        };
        let found = all(text, text.get(active.clone())?, whole);
        let after = found.iter().filter(|r| r.start >= active.end);
        let before = found.iter().filter(|r| r.end <= active.start);
        let mut others: Vec<Range<usize>> = after.chain(before).cloned().collect();
        self.ranges = std::iter::once(active.clone())
            .chain(others.iter().cloned())
            .collect();
        self.whole = whole;
        others.reverse();
        Some(Step::Set(std::iter::once(active).chain(others).collect()))
    }
}
