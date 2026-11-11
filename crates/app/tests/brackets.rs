//! The bracket at the caret and its partner: just after the caret first,
//! then just before; nested brackets of its kind counted, other kinds not.

use hangar_app::brackets::matching;

#[test]
fn a_bracket_and_its_partner() {
    let t = "f(a[0], (b));";
    assert_eq!(matching(t, 1), Some((1..2, 11..12)), "after the caret: (");
    assert_eq!(matching(t, 12), Some((11..12, 1..2)), "before the caret: )");
    assert_eq!(matching(t, 3), Some((3..4, 5..6)), "[ with its ]");
    assert_eq!(
        matching(t, 8),
        Some((8..9, 10..11)),
        "the inner ( with its own"
    );
    assert_eq!(matching(t, 13), None, "after ; nothing");
    assert_eq!(matching("(()", 0), None, "unclosed");
    assert_eq!(
        matching("é(x)", 2),
        Some((2..3, 4..5)),
        "past a wide character"
    );
}
