mod support;

use hangar_proto::collab::{Authority, Client, Update};
use hangar_proto::{ChangeSet, Text};
use support::Rng;

struct Editor {
    client: Client,
    text: Text,
}

impl Editor {
    fn new(id: &str, authority: &Authority) -> Self {
        Editor {
            client: Client::new(id, authority.version()),
            text: authority.text().clone(),
        }
    }

    fn edit(&mut self, rng: &mut Rng) {
        let set = support::set(rng, &self.text, 3);
        self.text = set.apply(&self.text).unwrap();
        self.client.local(set);
    }

    fn push(&mut self, authority: &mut Authority) -> bool {
        let pending = self.client.pending().to_vec();
        !pending.is_empty()
            && authority
                .push(self.client.version(), self.client.id(), &pending)
                .unwrap()
    }

    fn receive(&mut self, version: u64, updates: &[Update]) {
        if let Some(change) = self.client.receive(version, updates).unwrap() {
            self.text = change.apply(&self.text).unwrap();
        }
    }

    fn catch_up(&mut self, authority: &Authority) {
        let version = self.client.version();
        let updates = authority.since(version).unwrap().to_vec();
        self.receive(version, &updates);
    }
}

#[test]
fn editors_converge() {
    for seed in 0..500 {
        let mut rng = Rng::new(seed);
        let mut authority = Authority::new(support::text(&mut rng, 10));
        let mut editors: Vec<Editor> = ["a", "b", "c"]
            .iter()
            .map(|id| Editor::new(id, &authority))
            .collect();
        for _ in 0..60 {
            let e = rng.below(editors.len() + 1);
            if e == editors.len() {
                // The file changed on disk: the service's own change.
                let set = support::set(&mut rng, authority.text(), 2);
                let v = authority.version();
                assert!(authority.push(v, "disk", &[set]).unwrap());
                continue;
            }
            let editor = &mut editors[e];
            match rng.below(4) {
                0 | 1 => editor.edit(&mut rng),
                2 => {
                    editor.push(&mut authority);
                }
                _ => {
                    // Only part of what has happened has reached it.
                    let version = editor.client.version();
                    let all = authority.since(version).unwrap();
                    let n = rng.below(all.len() + 1);
                    let some = all[..n].to_vec();
                    editor.receive(version, &some);
                }
            }
        }
        for _ in 0..20 {
            for editor in &mut editors {
                editor.catch_up(&authority);
                editor.push(&mut authority);
            }
        }
        for editor in &mut editors {
            editor.catch_up(&authority);
            assert!(editor.client.pending().is_empty(), "seed {seed}");
            assert_eq!(&editor.text, authority.text(), "seed {seed}");
        }
    }
}

#[test]
fn a_push_behind_is_refused() {
    let mut authority = Authority::new(Text::from("abc"));
    let one = ChangeSet::empty(3);
    let grow = ChangeSet::of(
        [hangar_proto::Change {
            from: 3,
            to: 3,
            insert: Text::from("d"),
        }],
        3,
    )
    .unwrap();
    assert!(authority.push(0, "a", &[grow]).unwrap());
    assert!(!authority.push(0, "b", &[one]).unwrap());
    assert_eq!(authority.version(), 1);
    assert_eq!(authority.text().to_string(), "abcd");
}

#[test]
fn a_change_that_does_not_fit_takes_nothing() {
    let mut authority = Authority::new(Text::from("abc"));
    let fits = ChangeSet::empty(3);
    let does_not = ChangeSet::empty(7);
    assert!(authority.push(0, "a", &[fits, does_not]).is_err());
    assert_eq!(authority.version(), 0);
}

#[test]
fn updates_received_twice_are_skipped() {
    let mut authority = Authority::new(Text::from("abc"));
    let mut a = Editor::new("a", &authority);
    let mut b = Editor::new("b", &authority);
    b.edit(&mut Rng::new(7));
    assert!(b.push(&mut authority));
    let updates = authority.since(0).unwrap().to_vec();
    a.receive(0, &updates);
    a.receive(0, &updates);
    assert_eq!(a.text, *authority.text());
    assert_eq!(a.client.version(), authority.version());
}

#[test]
fn a_gap_is_an_error() {
    let mut client = Client::new("a", 2);
    assert!(client.receive(5, &[]).is_err());
}

#[test]
fn forgotten_updates_mean_opening_again() {
    let mut authority = Authority::new(Text::from("abc"));
    for v in 0..4 {
        assert!(authority.push(v, "a", &[ChangeSet::empty(3)]).unwrap());
    }
    authority.forget_before(2);
    assert!(authority.since(1).is_none());
    assert_eq!(authority.since(2).unwrap().len(), 2);
    assert_eq!(authority.since(4).unwrap().len(), 0);
}
