//! Selecting a word's other occurrences: the word at the caret, every place
//! text is found (whole words, or anywhere), and the next free one, round
//! to the top.

use hangar_app::occur::{all, next, word_range};

#[test]
fn the_word_at_the_caret() {
    let t = "let mut ledger = Ledger::new();";
    assert_eq!(word_range(t, 10), Some(8..14), "inside it");
    assert_eq!(word_range(t, 14), Some(8..14), "just after it");
    assert_eq!(word_range(t, 15), None, "between words");
    assert_eq!(
        word_range("naïve x", 3),
        Some(0..6),
        "past a character's middle"
    );
}

#[test]
fn whole_words_or_anywhere() {
    let t = "ledger.record(ledgers); my_ledger; ledger";
    assert_eq!(all(t, "ledger", true), [0..6, 35..41]);
    assert_eq!(all(t, "ledger", false), [0..6, 14..20, 27..33, 35..41]);
    assert_eq!(all("aaaa", "aa", false), [0..2, 2..4], "not overlapping");
    assert!(all(t, "", false).is_empty());
}

#[test]
fn the_next_free_one_round_to_the_top() {
    let t = "a x a x a";
    let first = vec![std::ops::Range { start: 0, end: 1 }];
    assert_eq!(next(t, "a", 1, &first, true), Some(4..5));
    assert_eq!(
        next(t, "a", 9, &[0..1, 4..5, 8..9], true),
        None,
        "all taken"
    );
    assert_eq!(
        next(t, "a", 9, &[4..5, 8..9], true),
        Some(0..1),
        "round to the top"
    );
}

/// ⌃G from the caret: the word, then each next whole word, round to the
/// top; ⌃⇧G takes the last back; a selection made elsewhere starts again,
/// matching anywhere.
#[test]
fn next_and_unselect_from_the_caret() {
    use hangar_app::occur::{Occurrences, Step};
    let t = "let ledger = 1;\nledger + ledgers;\nprint(ledger);";
    let mut o = Occurrences::default();
    assert_eq!(o.next(t, 5..5), Some(Step::Select(4..10)));
    assert_eq!(o.next(t, 4..10), Some(Step::Add(16..22)));
    assert_eq!(
        o.next(t, 4..10),
        Some(Step::Add(40..46)),
        "not inside ledgers"
    );
    assert_eq!(o.next(t, 4..10), None, "every one taken");
    assert_eq!(o.unselect(4..10), Some(Step::Set(vec![4..10, 16..22])));
    assert_eq!(o.next(t, 4..10), Some(Step::Add(40..46)));

    // Selected by hand, from the second: anywhere, round to the top.
    let mut o = Occurrences::default();
    assert_eq!(o.next(t, 16..22), Some(Step::Add(25..31)), "inside ledgers");
    assert_eq!(o.next(t, 16..22), Some(Step::Add(40..46)));
    assert_eq!(
        o.next(t, 16..22),
        Some(Step::Add(4..10)),
        "round to the top"
    );
}

/// ⌃⌘G: every whole word like the one at the caret, the nearest after it
/// added last.
#[test]
fn all_from_the_caret() {
    use hangar_app::occur::{Occurrences, Step};
    let t = "let ledger = 1;\nledger + ledgers;\nprint(ledger);";
    let mut o = Occurrences::default();
    assert_eq!(
        o.all(t, 17..17),
        Some(Step::Set(vec![16..22, 4..10, 40..46]))
    );
    assert_eq!(o.ranges, [16..22, 40..46, 4..10]);
    assert!(o.whole);
}
