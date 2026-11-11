//! A file's changes against git's index: runs of lines added, changed and
//! taken away; one rolled back, and one staged alone.

use hangar_app::changes::{Hunk, Kind, before, hunks, rolled_back, staged};

const BASE: &str = "a\nb\nc\nd\ne\n";
const TEXT: &str = "a\nB\nc\nnew\nd\n";

#[test]
fn changes_are_found_by_line() {
    let h = hunks(BASE, TEXT);
    assert_eq!(
        h,
        [
            Hunk {
                old: 1..2,
                new: 1..2
            },
            Hunk {
                old: 3..3,
                new: 3..4
            },
            Hunk {
                old: 4..5,
                new: 5..5
            },
        ]
    );
    let kinds: Vec<Kind> = h.iter().map(Hunk::kind).collect();
    assert_eq!(kinds, [Kind::Modified, Kind::Added, Kind::Deleted]);
    assert!(h[2].covers(5), "a deletion is marked on the line after");
    assert!(hunks(BASE, BASE).is_empty());
}

#[test]
fn one_change_rolled_back_or_staged_alone() {
    let h = hunks(BASE, TEXT);
    assert_eq!(rolled_back(BASE, TEXT, &h[0]), "a\nb\nc\nnew\nd\n");
    assert_eq!(rolled_back(BASE, TEXT, &h[1]), "a\nB\nc\nd\n");
    assert_eq!(rolled_back(BASE, TEXT, &h[2]), "a\nB\nc\nnew\nd\ne\n");
    assert_eq!(staged(BASE, TEXT, &h[0]), "a\nB\nc\nd\ne\n");
    assert_eq!(staged(BASE, TEXT, &h[2]), "a\nb\nc\nd\n");
    assert_eq!(before(BASE, &h[0]), "b\n");
}
