//! EditorConfig (editorconfig.org), as the web editor reads it: the
//! .editorconfig files from a file's folder up to the root, or to one that
//! says root = true, each a list of sections whose glob names the files its
//! properties are for, a nearer file's and a later section's winning. What
//! the editor takes from them is how to indent.

use std::collections::HashMap;

use crate::lineops::Indent;

/// One .editorconfig file: whether it is the last looked at, and its
/// sections, each a glob and its properties (names and values lower case).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    pub root: bool,
    pub sections: Vec<(String, HashMap<String, String>)>,
}

/// Reads an .editorconfig file: INI, with ; and # comments.
pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(glob) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            out.sections.push((glob.to_string(), HashMap::new()));
            continue;
        }
        let Some(at) = line.find(['=', ':']) else {
            continue;
        };
        let key = line[..at].trim().to_lowercase();
        let value = line[at + 1..].trim().to_lowercase();
        match out.sections.last_mut() {
            Some((_, props)) => {
                props.insert(key, value);
            }
            None if key == "root" => out.root = value == "true",
            None => {}
        }
    }
    out
}

/// Whether a section's glob names a path from its .editorconfig's folder:
/// a glob with no slash matches a file's name in any folder below; *, **,
/// ?, [...], [!...], {a,b} and {1..9} as the specification has them.
pub fn matches(glob: &str, path: &str) -> bool {
    let glob = if !glob.contains('/') {
        format!("**/{glob}")
    } else {
        glob.strip_prefix('/').unwrap_or(glob).to_string()
    };
    let path: Vec<char> = path.chars().collect();
    expand(&glob)
        .iter()
        .any(|g| matched(&g.chars().collect::<Vec<_>>(), &path))
}

/// A glob's braces spelled out: {a,b} as each, {1..3} as each number.
fn expand(glob: &str) -> Vec<String> {
    let Some(open) = glob.find('{') else {
        return vec![glob.to_string()];
    };
    // The brace closing it, nested ones counted.
    let mut depth = 0;
    let mut close = None;
    for (i, c) in glob[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(close) = close else {
        return vec![glob.to_string()];
    };
    let (head, inner, tail) = (&glob[..open], &glob[open + 1..close], &glob[close + 1..]);
    let choices: Vec<String> = match inner.split_once("..") {
        Some((a, b)) if a.parse::<i64>().is_ok() && b.parse::<i64>().is_ok() => {
            let (a, b) = (a.parse::<i64>().unwrap(), b.parse::<i64>().unwrap());
            let (lo, hi) = (a.min(b), a.max(b));
            if hi - lo > 1000 {
                return vec![glob.to_string()];
            }
            (lo..=hi).map(|n| n.to_string()).collect()
        }
        _ if inner.contains(',') => split_top(inner),
        _ => return vec![format!("{head}\\{{{inner}\\}}{tail}")],
    };
    choices
        .into_iter()
        .flat_map(|c| expand(&format!("{head}{c}{tail}")))
        .collect()
}

/// A brace's choices, split at its own commas, not nested ones.
fn split_top(inner: &str) -> Vec<String> {
    let (mut out, mut depth, mut start) = (vec![], 0, 0);
    for (i, c) in inner.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(inner[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(inner[start..].to_string());
    out
}

fn matched(p: &[char], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some('*') if p.get(1) == Some(&'*') => {
            let rest = &p[2..];
            // **/ matches no folders too.
            if rest.first() == Some(&'/') && matched(&rest[1..], s) {
                return true;
            }
            (0..=s.len()).any(|i| matched(rest, &s[i..]))
        }
        Some('*') => {
            let rest = &p[1..];
            for i in 0..=s.len() {
                if matched(rest, &s[i..]) {
                    return true;
                }
                if i < s.len() && s[i] == '/' {
                    break;
                }
            }
            false
        }
        Some('?') => s.first().is_some_and(|c| *c != '/') && matched(&p[1..], &s[1..]),
        Some('[') => match p.iter().skip(1).position(|c| *c == ']') {
            Some(n) => {
                let set = &p[1..1 + n];
                let (not, set) = match set.first() {
                    Some('!') => (true, &set[1..]),
                    _ => (false, set),
                };
                let Some(&c) = s.first() else {
                    return false;
                };
                let mut found = false;
                let mut i = 0;
                while i < set.len() {
                    if i + 2 < set.len() && set[i + 1] == '-' {
                        found |= set[i] <= c && c <= set[i + 2];
                        i += 3;
                    } else {
                        found |= set[i] == c;
                        i += 1;
                    }
                }
                found != not && c != '/' && matched(&p[2 + n..], &s[1..])
            }
            None => s.first() == Some(&'[') && matched(&p[1..], &s[1..]),
        },
        Some('\\') if p.len() > 1 => s.first() == Some(&p[1]) && matched(&p[2..], &s[1..]),
        Some(c) => s.first() == Some(c) && matched(&p[1..], &s[1..]),
    }
}

/// The folders whose .editorconfig files count for a file, nearest first:
/// its own, then each above it to the root.
pub fn folders(abs: &str) -> Vec<String> {
    let mut out = vec![];
    let mut d = abs.rsplit_once('/').map_or("", |(d, _)| d).to_string();
    while out.len() < 24 {
        out.push(if d.is_empty() { "/".into() } else { d.clone() });
        if d.is_empty() {
            break;
        }
        d = d.rsplit_once('/').map_or("", |(p, _)| p).to_string();
    }
    out
}

/// How the files say a file is indented, from each folder's file nearest
/// first (None where a folder has none): the properties of the sections
/// naming it, from the outermost that counts inward; None when they say
/// nothing of indenting.
pub fn indent(abs: &str, files: &[(String, Option<Parsed>)]) -> Option<Indent> {
    let top = files
        .iter()
        .position(|(_, f)| f.as_ref().is_some_and(|f| f.root))
        .unwrap_or(files.len().saturating_sub(1));
    let mut props: HashMap<String, String> = HashMap::new();
    for (dir, f) in files[..=top.min(files.len().saturating_sub(1))]
        .iter()
        .rev()
    {
        let Some(f) = f else { continue };
        let rel = if dir == "/" {
            &abs[1..]
        } else {
            abs.strip_prefix(&format!("{dir}/")).unwrap_or(abs)
        };
        for (glob, p) in &f.sections {
            if matches(glob, rel) {
                props.extend(p.clone());
            }
        }
    }
    let number = |k: &str| {
        props
            .get(k)
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| (1..=16).contains(n))
    };
    let size = match props.get("indent_size").map(String::as_str) {
        Some("tab") => number("tab_width"),
        _ => number("indent_size"),
    };
    match props.get("indent_style").map(String::as_str) {
        Some("tab") => Some(Indent::Tab),
        Some("space") => Some(Indent::Spaces(size.unwrap_or(4))),
        _ => size.map(Indent::Spaces),
    }
}
