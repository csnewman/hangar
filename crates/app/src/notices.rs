//! Notifications: what has come to need the user, in every place, as the
//! design's bell lists it. An environment whose agent comes to ask or to
//! finish, or that fails to start, and a place that asks to be signed in
//! again, each make a notice, found by comparing the places with how they
//! were. What already needs the user when a place is first seen is listed
//! too, as not new.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::model::{self, Card, Rank, Tone};
use crate::place::{Connection, PlaceConfig, PlaceState};

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub place: String,
    /// The environment it is about, if any.
    pub env: Option<String>,
    pub tone: Tone,
    /// The environment's title, or the place's name.
    pub title: String,
    /// What happened: "Claude is asking", "failed to start".
    pub what: String,
    pub detail: String,
    pub at: DateTime<Utc>,
    pub read: bool,
}

/// How many notices are kept.
const KEEP: usize = 50;

#[derive(Default)]
pub struct Notices {
    /// Each environment's rank when last seen.
    ranks: HashMap<(String, String), Rank>,
    /// The places asking to be signed in, when last seen.
    signed_out: HashSet<String>,
    /// The places seen at least once.
    known: HashSet<String>,
    list: Vec<Notice>,
}

impl Notices {
    /// Newest first.
    pub fn list(&self) -> &[Notice] {
        &self.list
    }

    pub fn unread(&self) -> usize {
        self.list.iter().filter(|n| !n.read).count()
    }

    pub fn read_all(&mut self) {
        for n in &mut self.list {
            n.read = true;
        }
    }

    /// Takes the places as they now are, returning the notices that are
    /// news: changes since a place was first seen.
    pub fn observe(
        &mut self,
        places: &[(PlaceConfig, PlaceState)],
        now: DateTime<Utc>,
    ) -> Vec<Notice> {
        let mut news = vec![];
        for (config, state) in places {
            let first = self.known.insert(config.id.clone());
            let signed_out = state.connection == Connection::SignIn;
            if signed_out && self.signed_out.insert(config.id.clone()) {
                let n = Notice {
                    place: config.id.clone(),
                    env: None,
                    tone: Tone::Busy,
                    title: config.name.clone(),
                    what: "sign in again".into(),
                    detail: "Its environments show what was last known".into(),
                    at: now,
                    read: false,
                };
                self.add(n.clone());
                if !first {
                    news.push(n);
                }
            } else if !signed_out {
                self.signed_out.remove(&config.id);
            }
            for e in &state.environments {
                let card = model::card(config, e, now);
                let key = (config.id.clone(), e.id.clone());
                let was = self.ranks.insert(key, card.rank);
                if was == Some(card.rank) {
                    continue;
                }
                let Some(n) = notice(&card, now) else {
                    continue;
                };
                self.add(n.clone());
                if !first {
                    news.push(n);
                }
            }
        }
        news
    }

    fn add(&mut self, n: Notice) {
        // One notice per environment: a newer one takes its place.
        self.list
            .retain(|o| !(o.place == n.place && o.env == n.env && o.env.is_some()));
        self.list.insert(0, n);
        self.list.truncate(KEEP);
    }
}

/// The notice an environment's card is worth, if it needs the user.
fn notice(c: &Card, now: DateTime<Utc>) -> Option<Notice> {
    let first_line = c.lines.first().map(|l| l.text.clone()).unwrap_or_default();
    // "Claude is asking: …? · 3m": what comes before the colon or the time.
    let lead = |s: &str| {
        s.split([':', '·'])
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let (what, detail) = match c.rank {
        Rank::Asking => (lead(&c.summary), first_line),
        Rank::Finished => (
            lead(&c.summary),
            c.lines.get(1).map(|l| l.text.clone()).unwrap_or_default(),
        ),
        Rank::Failed => ("failed to start".into(), first_line),
        _ => return None,
    };
    Some(Notice {
        place: c.place.clone(),
        env: Some(c.env.clone()),
        tone: c.tone,
        title: c.title.clone(),
        what,
        detail,
        at: now,
        read: false,
    })
}
