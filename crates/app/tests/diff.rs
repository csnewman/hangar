use std::collections::HashSet;

use hangar_app::diff::{Kind, Row, Side, counts, rows};

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

fn pair(row: &Row) -> (Option<usize>, Option<usize>, Kind) {
    match row {
        Row::Pair { left, right, kind } => (
            left.as_ref().map(|s| s.line),
            right.as_ref().map(|s| s.line),
            *kind,
        ),
        Row::Fold { .. } => panic!("a fold: {row:?}"),
    }
}

#[test]
fn a_change_is_paired_and_the_unchanged_lines_around_it_folded() {
    let old = numbered(20);
    let new = old.replace("line 10\n", "line ten\n");
    let r = rows(&old, &new, 3, &HashSet::new());
    // 3 context, the change, 3 context, with a fold either side.
    assert_eq!(r[0], Row::Fold { hidden: 6, id: 1 });
    assert_eq!(pair(&r[1]), (Some(7), Some(7), Kind::Same));
    assert_eq!(pair(&r[4]), (Some(10), Some(10), Kind::Changed));
    match &r[4] {
        Row::Pair { left, right, .. } => {
            assert_eq!(left.as_ref().unwrap().text, "line 10");
            assert_eq!(right.as_ref().unwrap().text, "line ten");
        }
        _ => unreachable!(),
    }
    assert_eq!(pair(&r[7]), (Some(13), Some(13), Kind::Same));
    assert_eq!(r[8], Row::Fold { hidden: 7, id: 14 });
    assert_eq!(r.len(), 9);
    assert_eq!(counts(&old, &new), (1, 1));
}

#[test]
fn an_opened_fold_shows_its_lines() {
    let old = numbered(20);
    let new = old.replace("line 10\n", "line ten\n");
    let r = rows(&old, &new, 3, &HashSet::from([1]));
    assert_eq!(pair(&r[0]), (Some(1), Some(1), Kind::Same));
    assert!(matches!(r.last(), Some(Row::Fold { id: 14, .. })));
}

#[test]
fn lines_only_one_side_has_are_against_a_gap() {
    let r = rows("a\nb\n", "a\nx\ny\nb\n", 3, &HashSet::new());
    assert_eq!(
        r.iter().map(pair).collect::<Vec<_>>(),
        [
            (Some(1), Some(1), Kind::Same),
            (None, Some(2), Kind::Added),
            (None, Some(3), Kind::Added),
            (Some(2), Some(4), Kind::Same),
        ]
    );
    let r = rows("a\nb\nc\n", "a\nc\n", 3, &HashSet::new());
    assert_eq!(pair(&r[1]), (Some(2), None, Kind::Deleted));
}

#[test]
fn a_new_file_is_all_added_and_an_unchanged_one_shown_whole() {
    let r = rows("", "one\ntwo\n", 3, &HashSet::new());
    assert!(r.iter().all(|row| pair(row).2 == Kind::Added));
    let same = numbered(30);
    let r = rows(&same, &same, 3, &HashSet::new());
    assert_eq!(r.len(), 30);
}

#[test]
fn tabs_are_spaced() {
    let r = rows("", "\tx\r\n", 3, &HashSet::new());
    match &r[0] {
        Row::Pair {
            right: Some(Side { text, .. }),
            ..
        } => assert_eq!(text, "    x"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_changed_lines_words_are_marked_on_each_side() {
    use hangar_app::diff::inline;
    let (old, new) = ("let total = sum(a);", "let total = sum(a, b);");
    let (l, r) = inline(old, new);
    let words = |text: &str, rs: &[std::ops::Range<usize>]| -> Vec<String> {
        rs.iter().map(|r| text[r.clone()].to_string()).collect()
    };
    assert!(l.is_empty());
    assert_eq!(words(new, &r), [", b"]);

    // Only the word that differs, the punctuation around it in common.
    let (l, r) = inline("fn record(amount: i64)", "fn record(amount: u64)");
    assert_eq!(words("fn record(amount: i64)", &l), ["i64"]);
    assert_eq!(words("fn record(amount: u64)", &r), ["u64"]);
    assert_eq!(inline("same", "same"), (vec![], vec![]));
}

#[test]
fn syntax_colours_are_split_by_line_with_tabs_widened() {
    use gpui::{HighlightStyle, hsla};
    use hangar_app::diff::{line_styles, merge};
    let red = HighlightStyle {
        color: Some(hsla(0., 1., 0.5, 1.)),
        ..Default::default()
    };
    // One style across a line break, one after a tab.
    let text = "ab\r\ncd\n\tef";
    let styles = vec![(1..6, red), (8..10, red)];
    let lines = line_styles(text, &styles);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], [(1..2, red)]);
    assert_eq!(lines[1], [(0..2, red)]);
    // "\tef" is shown as four spaces then "ef": bytes 1..3 become 4..6.
    assert_eq!(lines[2], [(4..6, red)]);

    // A changed word's background laid over the colours, cut where
    // either starts or ends.
    let bg = hsla(0.3, 1., 0.5, 0.3);
    let changed: Vec<std::ops::Range<usize>> = std::iter::once(2..6).collect();
    let merged = merge(&[(0..4, red)], &changed, bg);
    let on = HighlightStyle {
        background_color: Some(bg),
        ..red
    };
    let only_bg = HighlightStyle {
        background_color: Some(bg),
        ..Default::default()
    };
    assert_eq!(merged, [(0..2, red), (2..4, on), (4..6, only_bg)]);
}

#[test]
fn a_changed_blocks_lines_pair_with_the_ones_most_like_them() {
    use hangar_app::diff::align;
    let old = ["    println!(\"{}\", ledger.total());"];
    let new = [
        "    ledger.record(-3);",
        "    println!(\"total: {}\", ledger.total());",
    ];
    // The new line inserted stands alone, the println pairs with its own.
    assert_eq!(align(&old, &new), [(None, Some(0)), (Some(0), Some(1))]);

    // Lines alike in nothing still stand side by side, in order.
    assert_eq!(
        align(&["alpha();"], &["zzz", "yyy"]),
        [(Some(0), Some(0)), (None, Some(1))]
    );
    assert_eq!(align(&["a", "b"], &[]), [(Some(0), None), (Some(1), None)]);
}
