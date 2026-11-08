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
