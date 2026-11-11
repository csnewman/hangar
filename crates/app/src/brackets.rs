//! The bracket at the caret and its partner, as IntelliJ frames them: a
//! bracket just after the caret, else just before it, and the one closing
//! or opening it, counting brackets of its kind on the way.

use std::ops::Range;

const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// How far from the caret a partner is looked for, in bytes.
const REACH: usize = 200_000;

/// The bracket at the caret and its partner, in bytes, the caret's first;
/// None when neither side of the caret is a bracket, or it has no partner
/// within reach.
pub fn matching(text: &str, at: usize) -> Option<(Range<usize>, Range<usize>)> {
    let at = at.min(text.len());
    if !text.is_char_boundary(at) {
        return None;
    }
    let after = text[at..].chars().next().map(|c| (at, c));
    let before = text[..at]
        .chars()
        .next_back()
        .map(|c| (at - c.len_utf8(), c));
    [after, before]
        .into_iter()
        .flatten()
        .find_map(|(i, c)| partner(text, i, c).map(|p| (i..i + c.len_utf8(), p)))
}

/// The partner of the bracket c at i.
fn partner(text: &str, i: usize, c: char) -> Option<Range<usize>> {
    let (open, close) = PAIRS.into_iter().find(|(o, cl)| c == *o || c == *cl)?;
    let mut depth = 0usize;
    if c == open {
        let end = (i + REACH).min(text.len());
        let end = (end..=text.len()).find(|e| text.is_char_boundary(*e))?;
        for (j, ch) in text[i..end].char_indices() {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return Some(i + j..i + j + ch.len_utf8());
                }
            }
        }
    } else {
        let start = i.saturating_sub(REACH);
        let start = (0..=start).rev().find(|s| text.is_char_boundary(*s))?;
        for (j, ch) in text[start..i + c.len_utf8()].char_indices().rev() {
            if ch == close {
                depth += 1;
            } else if ch == open {
                depth -= 1;
                if depth == 0 {
                    return Some(start + j..start + j + ch.len_utf8());
                }
            }
        }
    }
    None
}
