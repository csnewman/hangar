//! CSV and TSV as a table, as the web editor shows them: the rows of the
//! text as RFC 4180 reads them (quoted fields, doubled quotes, line breaks
//! inside quotes), the first as the header.

/// The field separator of a file, by its name: a tab for TSV, a comma for
/// CSV; None for one that is neither.
pub fn separator(path: &str) -> Option<char> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if name.ends_with(".tsv") || name.ends_with(".tab") {
        Some('\t')
    } else if name.ends_with(".csv") {
        Some(',')
    } else {
        None
    }
}

/// The rows of the text, each its fields.
pub fn rows(text: &str, sep: char) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = vec![];
    let mut row: Vec<String> = vec![];
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => quoted = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            c if c == sep => row.push(std::mem::take(&mut field)),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                out.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        out.push(row);
    }
    out
}

/// Each column's width in characters: its longest field, between 3 and 40.
pub fn widths(rows: &[Vec<String>]) -> Vec<usize> {
    let mut out: Vec<usize> = vec![];
    for row in rows.iter().take(2000) {
        for (i, f) in row.iter().enumerate() {
            let n = f.chars().count().clamp(3, 40);
            match out.get_mut(i) {
                Some(w) => *w = (*w).max(n),
                None => out.push(n),
            }
        }
    }
    out
}
