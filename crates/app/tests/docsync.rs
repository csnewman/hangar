//! A shared document's sync, against the authority the Code service runs:
//! editors typing at once agree, undo takes back only the user's own
//! changes, and selections keep their place through others'.

use std::time::{Duration, Instant};

use gpui_component::input::Rope;
use hangar_app::docedit::{DocSync, apply, map_selection};
use hangar_proto::collab::Authority;

/// One editor: its text and its sync.
struct Editor {
    id: &'static str,
    text: Rope,
    sync: DocSync,
}

impl Editor {
    fn open(id: &'static str, auth: &Authority) -> Editor {
        Editor {
            id,
            text: Rope::from_str(&auth.text().to_string()),
            sync: DocSync::new(id, auth.version()),
        }
    }

    /// Types: del bytes at `at` become ins.
    fn edit(&mut self, at: usize, del: usize, ins: &str, now: Instant) {
        let before = self.text.clone();
        self.text.remove(at..at + del);
        self.text.insert(at, ins);
        self.sync
            .typed(&before, &self.text, at, at + del, at + ins.len(), now);
    }

    fn push(&self, auth: &mut Authority) {
        auth.push(self.sync.version(), self.id, self.sync.pending())
            .unwrap();
    }

    /// Takes what the authority has that this has not.
    fn pull(&mut self, auth: &Authority) {
        let updates = auth.since(self.sync.version()).unwrap().to_vec();
        if let Some(change) = self.sync.receive(self.sync.version(), &updates).unwrap() {
            apply(&change, &mut self.text);
        }
    }

    fn undo(&mut self) -> bool {
        match self.sync.undo(&self.text) {
            Some(change) => {
                apply(&change, &mut self.text);
                true
            }
            None => false,
        }
    }

    fn redo(&mut self) -> bool {
        match self.sync.redo(&self.text) {
            Some(change) => {
                apply(&change, &mut self.text);
                true
            }
            None => false,
        }
    }
}

fn authority(text: &str) -> Authority {
    Authority::new(ropey::Rope::from_str(text))
}

#[test]
fn editors_typing_at_once_agree() {
    let mut auth = authority("fn post() {}\n");
    let (mut a, mut b) = (Editor::open("a", &auth), Editor::open("b", &auth));
    let t = Instant::now();
    a.edit(0, 0, "pub ", t);
    b.edit(12, 0, " // v2", t);
    // b pushes on a version a has moved past; the authority rebases it.
    a.push(&mut auth);
    b.push(&mut auth);
    a.pull(&auth);
    b.pull(&auth);
    assert_eq!(auth.text().to_string(), "pub fn post() {} // v2\n");
    assert_eq!(a.text.to_string(), auth.text().to_string());
    assert_eq!(b.text.to_string(), auth.text().to_string());
    assert!(a.sync.pending().is_empty() && b.sync.pending().is_empty());
}

#[test]
fn text_beyond_one_unit_is_counted_in_utf16() {
    let mut auth = authority("é😀x\n");
    let (mut a, mut b) = (Editor::open("a", &auth), Editor::open("b", &auth));
    // After the emoji: byte 6, UTF-16 unit 3.
    a.edit(6, 0, "→", Instant::now());
    a.push(&mut auth);
    b.pull(&auth);
    assert_eq!(b.text.to_string(), "é😀→x\n");
}

#[test]
fn undo_takes_back_only_your_own_changes() {
    let mut auth = authority("one\ntwo\n");
    let (mut a, mut b) = (Editor::open("a", &auth), Editor::open("b", &auth));
    a.edit(3, 0, "!", Instant::now());
    a.push(&mut auth);
    b.pull(&auth);
    // Someone else adds a line above a's change.
    b.edit(0, 0, "zero\n", Instant::now());
    b.push(&mut auth);
    a.pull(&auth);
    assert_eq!(a.text.to_string(), "zero\none!\ntwo\n");

    assert!(a.undo(), "a has its own change to undo");
    assert_eq!(a.text.to_string(), "zero\none\ntwo\n", "b's line stays");
    assert!(!a.undo(), "and nothing else");
    a.push(&mut auth);
    b.pull(&auth);
    assert_eq!(b.text.to_string(), "zero\none\ntwo\n");

    assert!(a.redo());
    assert_eq!(a.text.to_string(), "zero\none!\ntwo\n");
    a.push(&mut auth);
    b.pull(&auth);
    assert_eq!(b.text.to_string(), auth.text().to_string());
}

#[test]
fn typing_close_together_is_one_step() {
    let auth = authority("\n");
    let mut a = Editor::open("a", &auth);
    let t = Instant::now();
    a.edit(0, 0, "a", t);
    a.edit(1, 0, "b", t + Duration::from_millis(100));
    a.edit(2, 0, "c", t + Duration::from_secs(5));
    assert_eq!(a.text.to_string(), "abc\n");
    assert!(a.undo());
    assert_eq!(
        a.text.to_string(),
        "ab\n",
        "the late keystroke is a step of its own"
    );
    assert!(a.undo());
    assert_eq!(a.text.to_string(), "\n", "the two together are one");
}

#[test]
fn a_selection_keeps_its_place() {
    let mut auth = authority("hello world\n");
    let (mut a, mut b) = (Editor::open("a", &auth), Editor::open("b", &auth));
    b.edit(0, 0, ">> ", Instant::now());
    b.push(&mut auth);
    let updates = auth.since(a.sync.version()).unwrap().to_vec();
    let change = a.sync.receive(a.sync.version(), &updates).unwrap().unwrap();
    assert_eq!(
        map_selection(&change, 6..11),
        9..14,
        "a range keeps the word it covered"
    );
    assert_eq!(
        map_selection(&change, 6..6),
        9..9,
        "a caret moves with its text"
    );
    assert_eq!(
        map_selection(&change, 0..0),
        3..3,
        "a caret where text is inserted stays after it"
    );
}
