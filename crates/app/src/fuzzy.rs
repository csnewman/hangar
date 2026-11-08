//! Matching what is typed against names, as IntelliJ's Search Everywhere
//! does: the query's characters in order, not necessarily together, case
//! ignored. Runs of characters together, and characters starting a word,
//! a path's part or a capitalised part, count for more.

/// The match's score, higher being better, and the byte offsets in text of
/// the characters matched; None when the query is not in text.
pub fn score(query: &str, text: &str) -> Option<(i64, Vec<usize>)> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if q.is_empty() {
        return Some((0, vec![]));
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    // Greedy from each place the first character could match, best kept:
    // a later start often gives a tighter run ("rate" in
    // "src/ratelimit.rs" rather than r…a…t…e scattered).
    let mut best: Option<(i64, Vec<usize>)> = None;
    for start in 0..chars.len() {
        if !eq(chars[start].1, q[0]) {
            continue;
        }
        let mut at = vec![];
        let mut i = start;
        for &want in &q {
            while i < chars.len() && !eq(chars[i].1, want) {
                i += 1;
            }
            if i == chars.len() {
                break;
            }
            at.push(i);
            i += 1;
        }
        if at.len() < q.len() {
            break;
        }
        let s = rate(&chars, &at);
        if best.as_ref().is_none_or(|(b, _)| s > *b) {
            best = Some((s, at.iter().map(|&i| chars[i].0).collect()));
        }
    }
    best
}

fn eq(c: char, want: char) -> bool {
    c.to_lowercase().eq(std::iter::once(want))
}

/// What matching these characters is worth.
fn rate(chars: &[(usize, char)], at: &[usize]) -> i64 {
    let mut s = 0i64;
    for (k, &i) in at.iter().enumerate() {
        s += 1;
        if k > 0 && at[k - 1] + 1 == i {
            s += 6;
        }
        let prev = i.checked_sub(1).map(|p| chars[p].1);
        let starts = match prev {
            None => true,
            Some(p) => !p.is_alphanumeric() || (p.is_lowercase() && chars[i].1.is_uppercase()),
        };
        if starts {
            s += 8;
        }
    }
    // Shorter texts, and matches nearer their start, first.
    s * 100 - chars.len() as i64 - at[0] as i64
}
