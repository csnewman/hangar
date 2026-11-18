//! Relaying through the app (docs/places.md §4.5): for a place the person
//! turned it on for, the app carries its links to the person's other
//! places when neither side of a link reaches the other. The place speaks
//! the link protocol over a WebSocket the app opens to it
//! (`/api/v1/me/links/{place}/relay`), and the app pipes every message as
//! it is to and from the other place's link endpoint. Each message is
//! signed and sealed between the two places, so the app reads none.
//!
//! Off unless asked for, per place, and paused while this Mac's network
//! is expensive or constrained unless the person says otherwise. What it
//! carries is counted per place and month, in relay.json in the app's
//! config folder with the choices.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::AppContext as _;
use serde::{Deserialize, Serialize};
use tungstenite::Message;

use crate::api::Client;

/// The longest one relayed session lasts: then the places try to reach
/// each other again before the app carries on.
const SESSION: Duration = Duration::from_secs(600);

/// How long a link the place holds otherwise, or that failed, is left
/// before the app tries again.
const HELD_WAIT: Duration = Duration::from_secs(60);
const FAILED_WAIT: Duration = Duration::from_secs(20);

/// How long since a place last heard from the other before the link is
/// taken to want carrying.
const SILENT: chrono::TimeDelta = chrono::TimeDelta::seconds(120);

/// What the person chose, and what has been carried this month.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The places relayed, by the app's own ID for them.
    pub places: Vec<String>,
    /// Whether to relay while the network is expensive or constrained.
    pub on_expensive: bool,
    /// The month counted, such as 2026-11.
    pub month: String,
    /// Bytes carried for each place in it.
    pub carried: HashMap<String, u64>,
}

/// One of the app's places as a round of relaying needs it.
#[derive(Clone)]
pub struct PlaceInfo {
    /// The app's ID for it, and its name.
    pub id: String,
    pub name: String,
    pub client: Client,
    /// Its ID as linked places know it.
    pub place_id: String,
    /// Its links: the other place's ID, whether this one reaches it now,
    /// and when it last heard from it.
    pub links: Vec<(String, bool, Option<chrono::DateTime<chrono::Utc>>)>,
}

struct Running {
    stop: Arc<AtomicBool>,
    /// The other place's name, for the page.
    to: String,
}

pub struct Relay {
    file: Option<PathBuf>,
    config: Mutex<Config>,
    /// Sessions running, by place and the other place's ID.
    running: Mutex<HashMap<(String, String), Running>>,
    /// When a link may next be tried.
    next: Mutex<HashMap<(String, String), Instant>>,
}

fn month() -> String {
    chrono::Local::now().format("%Y-%m").to_string()
}

impl Relay {
    /// The app's relay, its choices read from the config folder.
    pub fn global() -> &'static Arc<Relay> {
        static R: OnceLock<Arc<Relay>> = OnceLock::new();
        R.get_or_init(|| {
            let file = crate::store::Dirs::user().map(|d| d.config.join("relay.json"));
            Arc::new(Relay::open(file))
        })
    }

    pub fn open(file: Option<PathBuf>) -> Relay {
        let config = file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Relay {
            file,
            config: Mutex::new(config),
            running: Mutex::new(HashMap::new()),
            next: Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }

    fn save(&self, c: &Config) {
        if let Some(f) = &self.file {
            if let Some(dir) = f.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(b) = serde_json::to_vec_pretty(c) {
                let tmp = f.with_extension("json.tmp");
                if std::fs::write(&tmp, b).is_ok() {
                    let _ = std::fs::rename(tmp, f);
                }
            }
        }
    }

    /// Turns relaying a place on or off; off stops its sessions.
    pub fn set_place(&self, place: &str, on: bool) {
        let mut c = self.config.lock().unwrap();
        c.places.retain(|p| p != place);
        if on {
            c.places.push(place.to_string());
        }
        self.save(&c);
        drop(c);
        if !on {
            self.stop(|(p, _)| p == place);
        }
    }

    pub fn set_on_expensive(&self, on: bool) {
        let mut c = self.config.lock().unwrap();
        c.on_expensive = on;
        self.save(&c);
    }

    /// What was carried for a place this month, in bytes.
    pub fn carried(&self, place: &str) -> u64 {
        let c = self.config.lock().unwrap();
        if c.month != month() {
            return 0;
        }
        c.carried.get(place).copied().unwrap_or(0)
    }

    fn add(&self, place: &str, n: u64) {
        let mut c = self.config.lock().unwrap();
        let now = month();
        if c.month != now {
            c.month = now;
            c.carried.clear();
        }
        *c.carried.entry(place.to_string()).or_default() += n;
    }

    /// The other places a place's links are carried to now, by name.
    pub fn relaying(&self, place: &str) -> Vec<String> {
        self.running
            .lock()
            .unwrap()
            .iter()
            .filter(|((p, _), _)| p == place)
            .map(|(_, r)| r.to.clone())
            .collect()
    }

    /// Whether relaying waits for a cheaper network.
    pub fn paused(&self) -> bool {
        !self.config.lock().unwrap().on_expensive && crate::netpath::expensive()
    }

    fn stop(&self, which: impl Fn(&(String, String)) -> bool) {
        for (k, r) in self.running.lock().unwrap().iter() {
            if which(k) {
                r.stop.store(true, Ordering::Relaxed);
            }
        }
    }

    /// One round: carries each relayed place's links that neither side
    /// reaches, to the app's other places, and stops what should not run.
    pub fn tick(self: &Arc<Self>, places: &[PlaceInfo]) {
        let c = self.config();
        self.save(&c);
        if self.paused() {
            self.stop(|_| true);
            return;
        }
        self.stop(|(p, _)| !c.places.contains(p));
        for x in places.iter().filter(|x| c.places.contains(&x.id)) {
            for (other, reaches, heard) in &x.links {
                let Some(y) = places.iter().find(|y| &y.place_id == other) else {
                    continue;
                };
                let key = (x.id.clone(), other.clone());
                let back = (y.id.clone(), x.place_id.clone());
                let silent = heard.is_none_or(|h| chrono::Utc::now() - h > SILENT);
                let wants = !reaches && silent;
                {
                    let running = self.running.lock().unwrap();
                    if !wants || running.contains_key(&key) || running.contains_key(&back) {
                        continue;
                    }
                }
                if self
                    .next
                    .lock()
                    .unwrap()
                    .get(&key)
                    .is_some_and(|t| Instant::now() < *t)
                {
                    continue;
                }
                self.start(key, x, y);
            }
        }
    }

    fn start(self: &Arc<Self>, key: (String, String), x: &PlaceInfo, y: &PlaceInfo) {
        let stop = Arc::new(AtomicBool::new(false));
        self.running.lock().unwrap().insert(
            key.clone(),
            Running {
                stop: stop.clone(),
                to: y.name.clone(),
            },
        );
        let me = self.clone();
        let (from, to) = (
            x.client.link_relay_socket(&key.1),
            y.client.link_connect_socket(),
        );
        let place = x.id.clone();
        std::thread::spawn(move || {
            let r = pipe(&me, &place, from, to, &stop);
            let wait = match &r {
                Err(why) if why == "held" => HELD_WAIT,
                _ => FAILED_WAIT,
            };
            me.next
                .lock()
                .unwrap()
                .insert(key.clone(), Instant::now() + wait);
            me.running.lock().unwrap().remove(&key);
            let c = me.config();
            me.save(&c);
        });
    }
}

/// How often relaying looks at the person's places.
const TICK: Duration = Duration::from_secs(20);

/// Relays for as long as the app runs: a round every TICK, reading the
/// relayed places' links only while some place is relayed.
pub fn watch(places: gpui::Entity<crate::places::Places>, cx: &mut gpui::App) {
    let places = places.downgrade();
    cx.spawn(async move |cx| {
        loop {
            let relay = Relay::global().clone();
            let sources: Option<Vec<(String, String, Client)>> = places
                .update(cx, |p, _| {
                    p.list()
                        .iter()
                        .filter_map(|(c, _)| Some((c.id.clone(), c.name.clone(), p.client(&c.id)?)))
                        .collect()
                })
                .ok();
            let Some(sources) = sources else { return };
            let wanted =
                !relay.config().places.is_empty() || !relay.running.lock().unwrap().is_empty();
            if wanted {
                cx.background_spawn(async move {
                    let infos: Vec<PlaceInfo> = std::thread::scope(|s| {
                        let reads: Vec<_> = sources
                            .into_iter()
                            .map(|(id, name, client)| {
                                s.spawn(move || {
                                    let l = client.links().ok()?;
                                    let now = chrono::Utc::now();
                                    Some(PlaceInfo {
                                        place_id: l.place.id.clone(),
                                        links: l
                                            .items
                                            .iter()
                                            .map(|x| {
                                                let reaches = x.reach_error.is_none()
                                                    && x.reached_at
                                                        .is_some_and(|t| now - t < SILENT);
                                                (x.place.clone(), reaches, x.heard_at)
                                            })
                                            .collect(),
                                        id,
                                        name,
                                        client,
                                    })
                                })
                            })
                            .collect();
                        reads
                            .into_iter()
                            .filter_map(|r| r.join().ok().flatten())
                            .collect()
                    });
                    relay.tick(&infos);
                })
                .await;
            }
            cx.background_executor().timer(TICK).await;
        }
    })
    .detach();
}

/// Carries every message between the two sockets until either closes, the
/// session's time is up, or it is stopped. An error says why it ended:
/// "held" when the place holds the link otherwise.
fn pipe(
    relay: &Relay,
    place: &str,
    (from_url, from_auth): (String, String),
    (to_url, to_auth): (String, String),
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut a = crate::ws::connect(&from_url, &from_auth).map_err(String::from)?;
    let mut b = crate::ws::connect(&to_url, &to_auth).map_err(String::from)?;
    let started = Instant::now();
    let result = loop {
        if stop.load(Ordering::Relaxed) || started.elapsed() > SESSION {
            break Ok(());
        }
        if let Err(e) = carry(relay, place, &mut a, &mut b) {
            break Err(e);
        }
        if let Err(e) = carry(relay, place, &mut b, &mut a) {
            break Err(e);
        }
    };
    let _ = a.close(None);
    let _ = b.close(None);
    let _ = a.flush();
    let _ = b.flush();
    result
}

/// Moves what one socket has to the other, if anything.
fn carry(
    relay: &Relay,
    place: &str,
    from: &mut crate::ws::Socket,
    to: &mut crate::ws::Socket,
) -> Result<(), String> {
    match from.read() {
        Ok(m @ (Message::Text(_) | Message::Binary(_))) => {
            relay.add(place, m.len() as u64);
            to.send(m).map_err(|e| crate::ws::lost(&e))
        }
        Ok(Message::Close(frame)) => {
            let why = frame
                .as_ref()
                .map(|f| f.reason.to_string())
                .unwrap_or_default();
            let _ = to.close(frame);
            Err(if why.is_empty() { "closed".into() } else { why })
        }
        Ok(_) => Ok(()),
        Err(e) if crate::ws::idle(&e) => Ok(()),
        Err(e) => Err(crate::ws::lost(&e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_and_carried_bytes_are_kept() {
        let dir = std::env::temp_dir().join(format!("hangar-relay-{}", std::process::id()));
        let file = dir.join("relay.json");
        let r = Relay::open(Some(file.clone()));
        r.set_place("home", true);
        r.set_on_expensive(true);
        r.add("home", 1500);
        r.add("home", 500);
        let c = r.config();
        r.save(&c);
        let again = Relay::open(Some(file));
        assert_eq!(again.config().places, vec!["home".to_string()]);
        assert!(again.config().on_expensive);
        assert_eq!(again.carried("home"), 2000);
        again.set_place("home", false);
        assert!(again.config().places.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
