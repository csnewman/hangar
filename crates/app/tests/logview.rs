//! A log's text: found through, case aside, and copied from one place in
//! it to another, as the pane's Find and selection use it.

use hangar_app::logview::{Lines, word_at};

fn log(text: &str) -> Lines {
    let mut l = Lines::default();
    l.push(text);
    l
}

#[test]
fn text_is_found_on_every_line_case_aside() {
    let l = log("Started worker\nworker said: ERROR here\nno match\nerror, error\n");
    let found = |t: &str| l.find(t);
    assert_eq!(found("worker"), [(0, 8..14), (1, 0..6)]);
    assert_eq!(found("error"), [(1, 13..18), (3, 0..5), (3, 7..12)]);
    assert!(found("").is_empty());
    assert!(found("absent").is_empty());
}

#[test]
fn a_selection_runs_from_one_place_to_another_either_way() {
    let l = log("first line\nsecond line\nthird\n");
    assert_eq!(l.between((0, 6), (0, 10)), "line");
    assert_eq!(l.between((0, 6), (2, 3)), "line\nsecond line\nthi");
    // Drawn backwards, the same text.
    assert_eq!(l.between((2, 3), (0, 6)), "line\nsecond line\nthi");
    // Past a line's end is its end.
    assert_eq!(l.between((1, 7), (1, 99)), "line");
}

#[test]
fn a_double_click_takes_the_word() {
    assert_eq!(word_at("let worker_id = 42;", 6), 4..13);
    assert_eq!(word_at("let worker_id = 42;", 14), 14..15);
    assert_eq!(word_at("a", 5), 1..1);
}
