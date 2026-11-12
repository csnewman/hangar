//! Language servers' snippets (LSP's snippet syntax): a completion's text
//! with its tab stops ($1, ${2:default}, ${3|one,two|}) and variables
//! ($TM_FILENAME, ${VAR:default}), put in as the text it reads as, each
//! placeholder its default, the first choice of a choice, a variable its
//! default (the editor knows none), and where the first stop is, which
//! the editor selects once the completion is taken: $1, else $0.

use std::ops::Range;

/// A snippet as plain text, and its first stop's place in it.
pub fn expand(snippet: &str) -> (String, Option<Range<usize>>) {
    let mut out = String::new();
    let mut stops: Vec<(u32, Range<usize>)> = Vec::new();
    let chars: Vec<char> = snippet.chars().collect();
    let mut i = 0;
    body(&chars, &mut i, &mut out, &mut stops, None);
    let first = stops
        .iter()
        .filter(|(n, _)| *n > 0)
        .min_by_key(|(n, _)| *n)
        .or_else(|| stops.iter().find(|(n, _)| *n == 0))
        .map(|(_, r)| r.clone());
    (out, first)
}

/// Reads until the end, or the brace closing a placeholder.
fn body(
    c: &[char],
    i: &mut usize,
    out: &mut String,
    stops: &mut Vec<(u32, Range<usize>)>,
    until: Option<char>,
) {
    while *i < c.len() {
        let ch = c[*i];
        if Some(ch) == until {
            return;
        }
        match ch {
            '\\' if *i + 1 < c.len() && matches!(c[*i + 1], '$' | '}' | '\\' | ',' | '|') => {
                out.push(c[*i + 1]);
                *i += 2;
            }
            '$' => dollar(c, i, out, stops),
            _ => {
                out.push(ch);
                *i += 1;
            }
        }
    }
}

/// What follows a dollar: a stop or a variable, bare or braced.
fn dollar(c: &[char], i: &mut usize, out: &mut String, stops: &mut Vec<(u32, Range<usize>)>) {
    *i += 1;
    let start = out.len();
    let digits = |i: &mut usize| -> Option<u32> {
        let from = *i;
        while *i < c.len() && c[*i].is_ascii_digit() {
            *i += 1;
        }
        (from < *i)
            .then(|| c[from..*i].iter().collect::<String>().parse().ok())
            .flatten()
    };
    let name = |i: &mut usize| -> String {
        let from = *i;
        while *i < c.len() && (c[*i].is_ascii_alphanumeric() || c[*i] == '_') {
            *i += 1;
        }
        c[from..*i].iter().collect()
    };
    if *i < c.len() && c[*i].is_ascii_digit() {
        if let Some(n) = digits(i) {
            stops.push((n, start..start));
        }
        return;
    }
    if *i < c.len() && c[*i] != '{' {
        // A variable, which the editor has no value for.
        let v = name(i);
        if v.is_empty() {
            out.push('$');
        }
        return;
    }
    if *i >= c.len() {
        out.push('$');
        return;
    }
    // ${...}
    *i += 1;
    let n = digits(i);
    let v = if n.is_none() { name(i) } else { String::new() };
    match c.get(*i) {
        Some('}') => *i += 1,
        Some(':') => {
            *i += 1;
            body(c, i, out, stops, Some('}'));
            *i += 1;
        }
        Some('|') if n.is_some() => {
            *i += 1;
            let mut first = String::new();
            let mut taking = true;
            while *i < c.len() && c[*i] != '|' {
                match c[*i] {
                    '\\' if *i + 1 < c.len() => {
                        if taking {
                            first.push(c[*i + 1]);
                        }
                        *i += 2;
                        continue;
                    }
                    ',' => taking = false,
                    ch if taking => first.push(ch),
                    _ => {}
                }
                *i += 1;
            }
            out.push_str(&first);
            // "|}".
            *i += 2;
        }
        _ => {
            // Something not understood: kept as it reads.
            out.push_str("${");
            out.push_str(&v);
            return;
        }
    }
    if let Some(n) = n {
        stops.push((n, start..out.len()));
    }
}
