//! IntelliJ's line commands on a file's text: Duplicate Line or Selection,
//! Delete Line, Move Line Up and Down, Comment with Line Comment and Join
//! Lines. Each is one replacement of a run of the text, in byte offsets,
//! with where the selection goes after it, so the editor makes it as one
//! change, sent and undone as one.

use std::ops::Range;

/// What a command does: the bytes of the text it replaces, what goes there,
/// and the selection after, in the new text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub replace: Range<usize>,
    pub with: String,
    pub selection: Range<usize>,
}

impl Change {
    /// The text after the change.
    pub fn applied(&self, text: &str) -> String {
        format!(
            "{}{}{}",
            &text[..self.replace.start],
            self.with,
            &text[self.replace.end..]
        )
    }
}

/// The lines a selection touches, from the first one's start to the end of
/// the last one's text, its newline left out. A selection ending at the very
/// start of a line does not touch it.
pub fn lines(text: &str, sel: &Range<usize>) -> Range<usize> {
    let start = text[..sel.start].rfind('\n').map_or(0, |i| i + 1);
    let mut last = sel.end;
    if sel.end > sel.start && text[..sel.end].ends_with('\n') {
        last -= 1;
    }
    let end = text[last..].find('\n').map_or(text.len(), |i| last + i);
    start..end
}

/// The caret's column, in bytes, from the start of its line.
fn column(text: &str, at: usize) -> usize {
    at - text[..at].rfind('\n').map_or(0, |i| i + 1)
}

/// A place on the line starting at start, at a column, kept within the line
/// and on a character boundary.
fn at_column(text: &str, start: usize, column: usize) -> usize {
    let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let mut at = (start + column).min(end);
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// With no selection, the caret's line is copied below it, the caret going
/// with the copy; a selection is copied after itself, the copy selected.
pub fn duplicate(text: &str, sel: Range<usize>) -> Change {
    if sel.is_empty() {
        let l = lines(text, &sel);
        let line = &text[l.clone()];
        let caret = sel.start + line.len() + 1;
        return Change {
            replace: l.end..l.end,
            with: format!("\n{line}"),
            selection: caret..caret,
        };
    }
    let copy = text[sel.clone()].to_string();
    let len = copy.len();
    Change {
        replace: sel.end..sel.end,
        with: copy,
        selection: sel.end..sel.end + len,
    }
}

/// The lines the selection touches go, newline and all; the caret keeps its
/// column on the line that takes their place.
pub fn delete_lines(text: &str, sel: Range<usize>) -> Change {
    let l = lines(text, &sel);
    let col = column(text, sel.start);
    if l.end < text.len() {
        // The line after moves up into their place.
        let replace = l.start..l.end + 1;
        let after = Change {
            replace: replace.clone(),
            with: String::new(),
            selection: 0..0,
        }
        .applied(text);
        let caret = at_column(&after, l.start, col);
        Change {
            replace,
            with: String::new(),
            selection: caret..caret,
        }
    } else if l.start > 0 {
        // The last lines: the one before them is the caret's.
        let replace = l.start - 1..l.end;
        let after = &text[..l.start - 1];
        let prev = after.rfind('\n').map_or(0, |i| i + 1);
        let caret = at_column(after, prev, col);
        Change {
            replace,
            with: String::new(),
            selection: caret..caret,
        }
    } else {
        Change {
            replace: l,
            with: String::new(),
            selection: 0..0,
        }
    }
}

/// The lines the selection touches change places with the line above or
/// below them, the selection going with them; None at the top or bottom.
pub fn move_lines(text: &str, sel: Range<usize>, up: bool) -> Option<Change> {
    let l = lines(text, &sel);
    let block = &text[l.clone()];
    if up {
        if l.start == 0 {
            return None;
        }
        let prev_start = text[..l.start - 1].rfind('\n').map_or(0, |i| i + 1);
        let prev = &text[prev_start..l.start - 1];
        let by = prev.len() + 1;
        Some(Change {
            replace: prev_start..l.end,
            with: format!("{block}\n{prev}"),
            selection: sel.start - by..sel.end - by,
        })
    } else {
        if l.end >= text.len() {
            return None;
        }
        let next_end = text[l.end + 1..]
            .find('\n')
            .map_or(text.len(), |i| l.end + 1 + i);
        let next = &text[l.end + 1..next_end];
        let by = next.len() + 1;
        Some(Change {
            replace: l.start..next_end,
            with: format!("{next}\n{block}"),
            selection: sel.start + by..sel.end + by,
        })
    }
}

/// A language's line comment, None for one without.
pub fn comment_for(language: &str) -> Option<&'static str> {
    Some(match language {
        "rust" | "go" | "typescript" | "tsx" | "javascript" | "json" | "css" | "c" | "cpp"
        | "java" => "//",
        "python" | "toml" | "yaml" | "bash" | "ruby" | "make" => "#",
        _ => return None,
    })
}

/// The lines the selection touches are commented out with prefix, at the
/// least indented one's indent, or, when every line with text is already,
/// uncommented. With no selection the caret goes to the next line, as in
/// IntelliJ, so a run of lines is commented a key press each; a selection
/// becomes the lines it touched.
pub fn toggle_comment(text: &str, sel: Range<usize>, prefix: &str) -> Change {
    let l = lines(text, &sel);
    let block = &text[l.clone()];
    let rows: Vec<&str> = block.split('\n').collect();
    let filled: Vec<&&str> = rows.iter().filter(|r| !r.trim().is_empty()).collect();
    let commented = !filled.is_empty() && filled.iter().all(|r| r.trim_start().starts_with(prefix));
    let with: Vec<String> = if commented {
        rows.iter()
            .map(|r| {
                let indent = r.len() - r.trim_start().len();
                match r[indent..].strip_prefix(prefix) {
                    Some(rest) => {
                        let rest = rest.strip_prefix(' ').unwrap_or(rest);
                        format!("{}{rest}", &r[..indent])
                    }
                    None => r.to_string(),
                }
            })
            .collect()
    } else {
        let indent = filled
            .iter()
            .map(|r| r.len() - r.trim_start().len())
            .min()
            .unwrap_or(0);
        rows.iter()
            .map(|r| {
                if r.trim().is_empty() {
                    r.to_string()
                } else {
                    format!("{}{prefix} {}", &r[..indent], &r[indent..])
                }
            })
            .collect()
    };
    let with = with.join("\n");
    let change = Change {
        replace: l.clone(),
        with: with.clone(),
        selection: 0..0,
    };
    let after = change.applied(text);
    let selection = if sel.is_empty() {
        let end = l.start + with.len();
        if end < after.len() {
            let caret = at_column(&after, end + 1, column(text, sel.start));
            caret..caret
        } else {
            end..end
        }
    } else {
        l.start..l.start + with.len()
    };
    Change {
        selection,
        ..change
    }
}

/// The line after the selection's last is joined to it, its indent gone
/// and one space between them; None on the last line.
pub fn join_lines(text: &str, sel: Range<usize>) -> Option<Change> {
    let l = lines(text, &sel);
    if l.end >= text.len() {
        return None;
    }
    let next = &text[l.end + 1..];
    let indent = next.len() - next.trim_start_matches([' ', '\t']).len();
    let rest_blank = next[indent..].starts_with('\n') || next[indent..].is_empty();
    let line_ends_blank = text[l.clone()].ends_with([' ', '\t']) || l.is_empty();
    let with = if rest_blank || line_ends_blank {
        ""
    } else {
        " "
    };
    Some(Change {
        replace: l.end..l.end + 1 + indent,
        with: with.to_string(),
        selection: l.end..l.end,
    })
}
