//! What the programs in a terminal say they are doing (OSC 7501's program
//! status), kept as the protocol asks: one record per id, each report
//! replacing its record whole; a clear removes its record and those beneath
//! it ("build" takes "build/test" with it); a record with no app takes its
//! nearest ancestor's. A new prompt, or the program exiting, ends what was
//! working, waiting or idle; what is done or failed stays until the user
//! has seen it. At most 256 records, the one updated longest ago going
//! first.

use crate::terminal::{ProgramStatus, StatusKind, StatusState};

/// How many records are kept.
const KEEP: usize = 256;

/// What a terminal shows of its programs: the record that most needs the
/// user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub state: StatusState,
    pub kind: Option<StatusKind>,
    pub progress: Option<u8>,
    pub app: String,
    pub title: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct Records {
    /// Each record by id, with when it was last updated (a count).
    list: Vec<(ProgramStatus, u64)>,
    updates: u64,
}

/// Whether id is parent's or beneath it.
fn within(id: &str, parent: &str) -> bool {
    parent.is_empty()
        || id == parent
        || id
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl Records {
    /// Takes a report; true if what is kept changed.
    pub fn report(&mut self, r: ProgramStatus) -> bool {
        if r.state == StatusState::Clear {
            let before = self.list.len();
            self.list.retain(|(s, _)| !within(&s.id, &r.id));
            return self.list.len() != before;
        }
        self.updates += 1;
        let at = self.updates;
        match self.list.iter_mut().find(|(s, _)| s.id == r.id) {
            Some(slot) => *slot = (r, at),
            None => {
                if self.list.len() >= KEEP
                    && let Some(oldest) = self
                        .list
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, (_, at))| *at)
                        .map(|(i, _)| i)
                {
                    self.list.remove(oldest);
                }
                self.list.push((r, at));
            }
        }
        true
    }

    /// A new prompt started, or the program exited: what was working,
    /// waiting or idle is over. True if what is kept changed.
    pub fn ended(&mut self) -> bool {
        self.keep(|s| matches!(s, StatusState::Done | StatusState::Error))
    }

    /// The user has seen the terminal: what was done or failed is let go.
    /// True if what is kept changed.
    pub fn seen(&mut self) -> bool {
        self.keep(|s| !matches!(s, StatusState::Done | StatusState::Error))
    }

    /// The terminal was reset: every record is gone.
    pub fn clear(&mut self) -> bool {
        let had = !self.list.is_empty();
        self.list.clear();
        had
    }

    fn keep(&mut self, keep: impl Fn(StatusState) -> bool) -> bool {
        let before = self.list.len();
        self.list.retain(|(s, _)| keep(s.state));
        self.list.len() != before
    }

    /// The record that most needs the user: one waiting on them, then one
    /// that failed, one working, one done, one idle; of those alike, the
    /// last updated.
    pub fn shown(&self) -> Option<Shown> {
        let rank = |s: StatusState| match s {
            StatusState::Blocked => 4,
            StatusState::Error => 3,
            StatusState::Working => 2,
            StatusState::Done => 1,
            _ => 0,
        };
        let (s, _) = self
            .list
            .iter()
            .max_by_key(|(s, at)| (rank(s.state), *at))?;
        Some(Shown {
            state: s.state,
            kind: s.kind,
            progress: s.progress,
            app: self.app_of(s),
            title: s.title.clone(),
            message: s.message.clone(),
        })
    }

    /// A record's app, or its nearest ancestor's.
    fn app_of(&self, s: &ProgramStatus) -> String {
        if !s.app.is_empty() {
            return s.app.clone();
        }
        let mut id = s.id.as_str();
        loop {
            id = match id.rfind('/') {
                Some(i) => &id[..i],
                None if !id.is_empty() => "",
                None => return String::new(),
            };
            if let Some((p, _)) = self.list.iter().find(|(p, _)| p.id == id)
                && !p.app.is_empty()
            {
                return p.app.clone();
            }
        }
    }
}
