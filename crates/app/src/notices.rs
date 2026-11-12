//! Notifications: what has come to need the user, in every place, as the
//! design's bell lists it. An environment whose agent comes to ask or to
//! finish, that fails to start or comes up, or whose image has a newer
//! copy, and a place that asks to be signed in again, each make a notice,
//! found by comparing the places with how they were. What already needs
//! the user when a place is first seen is listed too, as not new.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::model::{self, Card, Rank, Tone};
use crate::place::{Connection, PlaceConfig, PlaceState};
use crate::settings::Event;

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub event: Event,
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
    /// Each environment's newer image, when last seen.
    images: HashMap<(String, String), String>,
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
                    event: Event::SignIn,
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
                let newer = e.image_update.as_ref().map(|u| u.digest.clone());
                let image_was = match &newer {
                    Some(d) => self.images.insert(key.clone(), d.clone()),
                    None => self.images.remove(&key),
                };
                if let Some(d) = &newer
                    && image_was.as_ref() != Some(d)
                {
                    let n = Notice {
                        event: Event::Image,
                        place: card.place.clone(),
                        env: Some(card.env.clone()),
                        tone: Tone::Idle,
                        title: card.title.clone(),
                        what: "a newer image is available".into(),
                        detail: "Upgrade it from its page in the browser".into(),
                        at: now,
                        read: false,
                    };
                    self.add(n.clone());
                    if !first {
                        news.push(n);
                    }
                }
                let was = self.ranks.insert(key, card.rank);
                if was == Some(card.rank) {
                    continue;
                }
                let Some(n) = notice(&card, was, now) else {
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
        // One notice per environment, and one of its image: a newer one
        // takes its place.
        let image = n.event == Event::Image;
        self.list.retain(|o| {
            !(o.place == n.place
                && o.env == n.env
                && o.env.is_some()
                && (o.event == Event::Image) == image)
        });
        self.list.insert(0, n);
        self.list.truncate(KEEP);
    }
}

/// The notice an environment's card is worth, if it needs the user or has
/// come up from was.
fn notice(c: &Card, was: Option<Rank>, now: DateTime<Utc>) -> Option<Notice> {
    let first_line = c.lines.first().map(|l| l.text.clone()).unwrap_or_default();
    // "Claude is asking: …? · 3m": what comes before the colon or the time.
    let lead = |s: &str| {
        s.split([':', '·'])
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let (event, what, detail) = match c.rank {
        Rank::Asking => (Event::Asking, lead(&c.summary), first_line),
        Rank::Finished => (
            Event::Finished,
            lead(&c.summary),
            c.lines.get(1).map(|l| l.text.clone()).unwrap_or_default(),
        ),
        Rank::Failed => (Event::Failed, "failed to start".into(), first_line),
        Rank::Running | Rank::Working | Rank::Busy if was == Some(Rank::Starting) => {
            (Event::Ready, "is ready".into(), String::new())
        }
        _ => return None,
    };
    Some(Notice {
        event,
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
