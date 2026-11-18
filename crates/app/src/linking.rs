//! Linking places (docs/places.md §4) as the app does it: what each of the
//! person's places says of its links, read into a state per place, and the
//! calls that link places together or unlink one. The app has a token for
//! every place, so it introduces them to each other itself, each place
//! making its own half of every link.

use chrono::{DateTime, Utc};
use hangar_openapi::clientapi::types;

/// How long after a place last reached another it still counts as
/// reaching it: places say hello to each other every minute.
pub const FRESH_SECONDS: i64 = 150;

/// One of the app's places, and what it says of its links.
#[derive(Debug, Clone)]
pub struct Known {
    /// The app's ID for the place.
    pub id: String,
    pub name: String,
    /// "proxmox" for a Hangar the app made, else "server".
    pub kind: &'static str,
    /// Where the app reaches it.
    pub url: String,
    /// The user's name there, when known.
    pub username: Option<String>,
    pub links: Result<types::Links, String>,
}

impl Known {
    /// Its ID as linked places know it.
    pub fn place_id(&self) -> Option<&str> {
        self.links.as_ref().ok().map(|l| l.place.id.as_str())
    }

    /// Where linked places reach it: its own public address, else the
    /// app's.
    pub fn link_url(&self) -> String {
        self.links
            .as_ref()
            .ok()
            .and_then(|l| l.place.url.clone())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| self.url.clone())
    }

    fn linked(&self) -> bool {
        self.links.as_ref().is_ok_and(|l| !l.items.is_empty())
    }
}

/// Where a place stands among the person's linked places.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Its links could not be read, and why.
    Unknown(String),
    /// Its administrators do not allow personal links.
    NotAllowed,
    NotLinked,
    /// Linked, and it reaches a linked place or one reaches it.
    Linked,
    /// Linked, and its records were lately exchanged with every linked
    /// place it reaches.
    UpToDate,
    /// Linked, but an exchange of records failed, and why.
    Behind(String),
    /// Linked, but it reaches no linked place and none reaches it, and
    /// why, as its last try said.
    Waiting(String),
}

/// One link of a place's, as the page shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkRow {
    /// The app's name for the other place, else the name it was linked as.
    pub name: String,
    pub reached: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub heard: Option<DateTime<Utc>>,
    /// When its records were last exchanged with the other place, or why
    /// they could not be.
    pub synced: Option<DateTime<Utc>>,
    pub sync_error: Option<String>,
}

/// A place as the page shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub state: State,
    /// The linked places it reached lately, by name, as it or they say.
    pub reaches: Vec<String>,
    /// The linked places that reached it lately, by name, as it or they
    /// say.
    pub reached_by: Vec<String>,
    pub links: Vec<LinkRow>,
}

fn fresh(at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    at.is_some_and(|t| (now - t).num_seconds() <= FRESH_SECONDS)
}

/// The app's name for the place a link names, else the name it was
/// linked as.
fn name_of(known: &[Known], place: &str, fallback: &str) -> String {
    known
        .iter()
        .find(|k| k.place_id() == Some(place))
        .map(|k| k.name.clone())
        .unwrap_or_else(|| fallback.to_string())
}

/// Each place's row, in the order given.
pub fn rows(known: &[Known], now: DateTime<Utc>) -> Vec<Row> {
    known
        .iter()
        .map(|k| {
            let mut row = Row {
                id: k.id.clone(),
                name: k.name.clone(),
                kind: k.kind,
                state: State::NotLinked,
                reaches: vec![],
                reached_by: vec![],
                links: vec![],
            };
            let l = match &k.links {
                Err(e) => {
                    row.state = State::Unknown(e.clone());
                    return row;
                }
                Ok(l) => l,
            };
            for link in &l.items {
                let name = name_of(known, &link.place, &link.name);
                let reached = fresh(link.reached_at, now) && link.reach_error.is_none();
                if reached && !row.reaches.contains(&name) {
                    row.reaches.push(name.clone());
                }
                if fresh(link.heard_at, now) && !row.reached_by.contains(&name) {
                    row.reached_by.push(name.clone());
                }
                row.links.push(LinkRow {
                    name,
                    reached: link.reached_at,
                    error: link.reach_error.clone(),
                    heard: link.heard_at,
                    synced: link.synced_at,
                    sync_error: link.sync_error.clone(),
                });
            }
            // What the other places say of it: one that reached it was
            // reached by it, and one it reached heard it.
            for other in known {
                let Ok(lo) = &other.links else { continue };
                if lo.place.id == l.place.id {
                    continue;
                }
                for link in lo.items.iter().filter(|o| o.place == l.place.id) {
                    if fresh(link.reached_at, now)
                        && link.reach_error.is_none()
                        && !row.reached_by.contains(&other.name)
                    {
                        row.reached_by.push(other.name.clone());
                    }
                    if fresh(link.heard_at, now) && !row.reaches.contains(&other.name) {
                        row.reaches.push(other.name.clone());
                    }
                }
            }
            row.state = if !l.allowed && l.items.is_empty() {
                State::NotAllowed
            } else if l.items.is_empty() {
                State::NotLinked
            } else if !row.reaches.is_empty() || !row.reached_by.is_empty() {
                let reached: Vec<&LinkRow> = row
                    .links
                    .iter()
                    .filter(|l| fresh(l.reached, now) && l.error.is_none())
                    .collect();
                if let Some(e) = reached.iter().find_map(|l| l.sync_error.clone()) {
                    State::Behind(e)
                } else if !reached.is_empty() && reached.iter().all(|l| fresh(l.synced, now)) {
                    State::UpToDate
                } else {
                    State::Linked
                }
            } else {
                State::Waiting(
                    row.links
                        .iter()
                        .find_map(|l| l.error.clone())
                        .unwrap_or_else(|| "not reached yet".into()),
                )
            };
            row
        })
        .collect()
}

/// One call linking or unlinking: at the place the app knows by `at`.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Link with the place of ID `other`, as person.
    Put {
        at: String,
        other: String,
        name: String,
        public_key: String,
        url: String,
        person: String,
    },
    /// Unlink from the place of ID `other`.
    Delete { at: String, other: String },
}

/// The person the places are linked as: the one the linked among them
/// are already, else the first place's.
pub fn person(selected: &[&Known]) -> Result<String, String> {
    let mut linked: Vec<(&str, &str)> = vec![];
    for k in selected {
        if let Ok(l) = &k.links
            && !l.items.is_empty()
            && !linked.iter().any(|(_, p)| *p == l.person)
        {
            linked.push((&k.name, &l.person));
        }
    }
    match linked.as_slice() {
        [] => selected
            .first()
            .and_then(|k| k.links.as_ref().ok())
            .map(|l| l.person.clone())
            .ok_or_else(|| "Choose at least two places.".to_string()),
        [(_, p)] => Ok(p.to_string()),
        [(a, _), (b, _), ..] => Err(format!(
            "{a} and {b} are linked as different people. Unlink one of them first."
        )),
    }
}

/// The calls linking every place selected with every other, as one
/// person: none for a link a place has already as it should be.
pub fn link_steps(selected: &[&Known]) -> Result<Vec<Step>, String> {
    if selected.len() < 2 {
        return Err("Choose at least two places.".into());
    }
    for k in selected {
        match &k.links {
            Err(e) => return Err(format!("{}: {e}", k.name)),
            Ok(l) if !l.allowed => {
                return Err(format!(
                    "{}: its administrators do not allow personal links.",
                    k.name
                ));
            }
            Ok(_) => {}
        }
    }
    let person = person(selected)?;
    let mut steps = vec![];
    for x in selected {
        let Ok(lx) = &x.links else { continue };
        for y in selected {
            let Ok(ly) = &y.links else { continue };
            if ly.place.id == lx.place.id {
                continue;
            }
            let url = y.link_url();
            let current = lx.items.iter().any(|l| {
                l.place == ly.place.id && l.public_key == ly.place.public_key && l.url == url
            });
            if current && lx.person == person {
                continue;
            }
            steps.push(Step::Put {
                at: x.id.clone(),
                other: ly.place.id.clone(),
                name: y.name.clone(),
                public_key: ly.place.public_key.clone(),
                url,
                person: person.clone(),
            });
        }
    }
    Ok(steps)
}

/// The calls unlinking a place from every place: its own links, and the
/// links the other places the app knows have with it.
pub fn unlink_steps(target: &Known, known: &[Known]) -> Vec<Step> {
    let Ok(lt) = &target.links else {
        return vec![];
    };
    let mut steps: Vec<Step> = lt
        .items
        .iter()
        .map(|l| Step::Delete {
            at: target.id.clone(),
            other: l.place.clone(),
        })
        .collect();
    for k in known {
        if k.id == target.id {
            continue;
        }
        if let Ok(lk) = &k.links
            && lk.items.iter().any(|l| l.place == lt.place.id)
        {
            steps.push(Step::Delete {
                at: k.id.clone(),
                other: lt.place.id.clone(),
            });
        }
    }
    steps
}

/// Whether any of the places is linked.
pub fn any_linked(known: &[Known]) -> bool {
    known.iter().any(Known::linked)
}
