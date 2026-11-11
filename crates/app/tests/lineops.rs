//! IntelliJ's line commands on text, written with the selection marked:
//! `|` is the caret, `[` and `]` a selection.

use std::ops::Range;

use hangar_app::lineops::{
    Change, comment_for, delete_lines, duplicate, join_lines, move_lines, toggle_comment,
};

/// Text with its marks taken out, and the selection they marked.
fn marked(s: &str) -> (String, Range<usize>) {
    if let Some(i) = s.find('|') {
        return (s.replacen('|', "", 1), i..i);
    }
    let a = s.find('[').unwrap();
    let b = s.find(']').unwrap() - 1;
    (s.replacen('[', "", 1).replacen(']', "", 1), a..b)
}

/// The text after a change, with its selection marked.
fn shown(text: &str, c: &Change) -> String {
    let after = c.applied(text);
    let s = &c.selection;
    if s.is_empty() {
        format!("{}|{}", &after[..s.start], &after[s.start..])
    } else {
        format!(
            "{}[{}]{}",
            &after[..s.start],
            &after[s.clone()],
            &after[s.end..]
        )
    }
}

fn check(f: impl Fn(&str, Range<usize>) -> Change, before: &str, after: &str) {
    let (text, sel) = marked(before);
    assert_eq!(shown(&text, &f(&text, sel)), after, "from {before:?}");
}

#[test]
fn duplicate_copies_the_line_or_the_selection() {
    check(duplicate, "a\nb|c\nd", "a\nbc\nb|c\nd");
    check(duplicate, "only|", "only\nonly|");
    check(duplicate, "x [ab]y", "x ab[ab]y");
    check(duplicate, "[a\n]b", "a\n[a\n]b");
}

#[test]
fn delete_takes_the_lines_and_keeps_the_column() {
    check(delete_lines, "one\ntw|o\nthree", "one\nth|ree");
    check(delete_lines, "one\nx\ntw|o", "one\nx|");
    check(delete_lines, "one\ntw|o", "on|e");
    check(delete_lines, "a\nlong|er\nb", "a\nb|");
    check(delete_lines, "[a\nb]\nc", "|c");
    check(delete_lines, "a\n[b\n]c", "a\n|c");
    check(delete_lines, "alo|ne", "|");
}

#[test]
fn lines_move_past_their_neighbours() {
    let up = |t: &str, s| move_lines(t, s, true).unwrap();
    let down = |t: &str, s| move_lines(t, s, false).unwrap();
    check(up, "a\nb|b\nc", "b|b\na\nc");
    check(down, "a\nb|b\nc", "a\nc\nb|b");
    check(down, "[a\nb]\nc", "c\n[a\nb]");
    let (text, sel) = marked("fi|rst\nsecond");
    assert!(move_lines(&text, sel, true).is_none(), "the top stays");
    let (text, sel) = marked("first\nsec|ond");
    assert!(move_lines(&text, sel, false).is_none(), "the bottom stays");
}

#[test]
fn comments_toggle_at_the_least_indent() {
    let rust = |t: &str, s| toggle_comment(t, s, comment_for("rust").unwrap());
    check(rust, "    let |x = 1;\nnext", "    // let x = 1;\nnext|");
    check(rust, "    // let |x = 1;\nnext", "    let x = 1;\nnext|");
    check(
        rust,
        "[fn a() {\n    b();\n\n}]",
        "[// fn a() {\n//     b();\n\n// }]",
    );
    check(rust, "[// a\n    // b]", "[a\n    b]");
    check(rust, "[a\n// b]", "[// a\n// // b]");
    assert_eq!(comment_for("python"), Some("#"));
    assert_eq!(comment_for("markdown"), None);
}

#[test]
fn join_brings_the_next_line_up_with_one_space() {
    let join = |t: &str, s| join_lines(t, s).unwrap();
    check(join, "let a =|\n    1;", "let a =| 1;");
    check(join, "a |\n  b", "a |b");
    check(join, "a|\n\nc", "a|\nc");
    let (text, sel) = marked("last|");
    assert!(join_lines(&text, sel).is_none());
}

/// A file's indentation as the status bar says it: tabs where most indented
/// lines start with one, else the step most often seen going one deeper.
#[test]
fn indentation_is_read_from_the_lines() {
    use hangar_app::lineops::{Indent, detect_indent};
    let four = "fn a() {\n    if b {\n        c();\n    }\n}\n";
    assert_eq!(detect_indent(four), Some(Indent::Spaces(4)));
    let two = "a:\n  b:\n    c: 1\n  d: 2\n";
    assert_eq!(detect_indent(two), Some(Indent::Spaces(2)));
    assert_eq!(detect_indent("func a() {\n\tb()\n}\n"), Some(Indent::Tab));
    assert_eq!(detect_indent("one\ntwo\n"), None);
    assert_eq!(Indent::Spaces(4).to_string(), "4 spaces");
}
