//! Linked places' connections (docs/places.md §4.5): one WebSocket for
//! each linked person and pair of places, dialled by whichever side can
//! reach the other and kept open while both are up. Over it each side
//! sends the person's records (`linksync`) and their copies' files
//! (`linkfiles`) as soon as they change, and both compare them in full on
//! connecting and every hour; and, at link level 2, the manifests and
//! blobs their repositories lack (`linkimages`). Every message is `Sealed`: signed by its
//! sender and sealed to its recipient; a file goes in binary messages of
//! a piece each, sent by a task of the connection's own, so a large one
//! holds up nothing else.
//!
//! Both places of a link try to hold one connection, each dialling while
//! there is none, backing off while it cannot. Two dialled at once settle
//! on the one dialled by the place of lower ID.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use uuid::Uuid;

use crate::http::App;
use crate::link::{LEVELS, Place, Refused, Sealed};
use crate::linkfiles::{
    Body, Carry, FilePiece, FileVersions, FileWant, Job, LEVEL1_MAX_FILE, MAX_FILE, PIECE,
};
use crate::linkimages::{ImagePiece, Pieces, Wants};
use crate::linksync::{Exchange, Here, Records, Versions};
use crate::store::Error as StoreError;
use crate::store::rows::Link;

/// How often a connection is pinged, and how long it may be silent
/// before it is taken to be gone.
const PING: Duration = Duration::from_secs(30);
const SILENT: Duration = Duration::from_secs(75);

/// How often two connected places exchange every record, though nothing
/// was sent: in case something was missed.
const FULL: Duration = Duration::from_secs(3600);

/// How long connecting and the first messages may take.
const HANDSHAKE: Duration = Duration::from_secs(15);

/// How long a place waits to dial again after failing, at first and at
/// most.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// How often a live connection is written down on its link, for the
/// people's pages to say so.
const RECORD: Duration = Duration::from_secs(60);

/// How often the links are read again, for those another server sharing
/// the store made or removed.
const TIDY: Duration = Duration::from_secs(300);

/// A linked person's account here, and the other place.
type Key = (Uuid, Uuid);

enum Cmd {
    /// Send the person's versions: their records may have changed.
    Offer,
    /// Send the versions of the person's files.
    OfferFiles,
    /// Send these files.
    Files(Vec<Job>),
    /// Bring the person's pending repositories on, asking for what they
    /// lack.
    Images,
    Close,
}

struct Live {
    id: u64,
    dialled_by: Uuid,
    tx: mpsc::UnboundedSender<Cmd>,
}

/// The connections this place holds.
#[derive(Default)]
pub struct Conns {
    live: Mutex<HashMap<Key, Live>>,
    ended: Mutex<HashMap<Key, Arc<Notify>>>,
    next: AtomicU64,
}

/// A connection held, given up when dropped.
struct Held {
    conns: Arc<Conns>,
    key: Key,
    id: u64,
}

impl Drop for Held {
    fn drop(&mut self) {
        let mut live = self.conns.live.lock().unwrap();
        if live.get(&self.key).is_some_and(|l| l.id == self.id) {
            live.remove(&self.key);
        }
        drop(live);
        self.conns.ended(self.key).notify_waiters();
    }
}

impl Conns {
    /// What wakes when the connection for key ends.
    fn ended(&self, key: Key) -> Arc<Notify> {
        self.ended.lock().unwrap().entry(key).or_default().clone()
    }

    fn alive(&self, key: Key) -> bool {
        self.live.lock().unwrap().contains_key(&key)
    }

    /// Takes the connection for key, dialled by the place given: None when
    /// one dialled by a place of lower ID holds it, which a later one does
    /// not displace; else any other is closed.
    fn take(
        self: &Arc<Self>,
        key: Key,
        dialled_by: Uuid,
    ) -> Option<(Held, mpsc::UnboundedReceiver<Cmd>)> {
        let mut live = self.live.lock().unwrap();
        if live.get(&key).is_some_and(|l| l.dialled_by < dialled_by) {
            return None;
        }
        if let Some(old) = live.remove(&key) {
            let _ = old.tx.send(Cmd::Close);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        live.insert(key, Live { id, dialled_by, tx });
        Some((
            Held {
                conns: self.clone(),
                key,
                id,
            },
            rx,
        ))
    }

    /// Has each of a person's connections, but the one to the place
    /// except, send their versions.
    pub fn offer(&self, user: Uuid, except: Option<Uuid>) {
        for ((u, other), l) in self.live.lock().unwrap().iter() {
            if *u == user && Some(*other) != except {
                let _ = l.tx.send(Cmd::Offer);
            }
        }
    }

    /// Has each of a person's connections, but the one to the place
    /// except, send these files.
    pub fn files(&self, user: Uuid, except: Option<Uuid>, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        for ((u, other), l) in self.live.lock().unwrap().iter() {
            if *u == user && Some(*other) != except {
                let _ = l.tx.send(Cmd::Files(jobs.clone()));
            }
        }
    }

    /// Has each of a person's connections bring their pending
    /// repositories on.
    pub fn images(&self, user: Uuid) {
        for ((u, _), l) in self.live.lock().unwrap().iter() {
            if *u == user {
                let _ = l.tx.send(Cmd::Images);
            }
        }
    }

    /// Has each of a person's connections send the versions of their
    /// files.
    pub fn offer_files(&self, user: Uuid) {
        for ((u, _), l) in self.live.lock().unwrap().iter() {
            if *u == user {
                let _ = l.tx.send(Cmd::OfferFiles);
            }
        }
    }

    fn close(&self, key: Key) {
        if let Some(l) = self.live.lock().unwrap().remove(&key) {
            let _ = l.tx.send(Cmd::Close);
        }
    }
}

/// What a connection carries, whichever side of it this place is.
enum Frame {
    Text(String),
    Binary(Vec<u8>),
    Ping,
    Pong,
    /// Closing, and why (a refusal's code).
    Close(Option<String>),
}

/// A connection's two directions, as channels, from a WebSocket of either
/// side. What arrives is never held up, so two places sending each other
/// files at once both carry on.
struct Wire {
    rx: mpsc::UnboundedReceiver<Frame>,
    tx: mpsc::Sender<Frame>,
    /// The newest link level both places speak, once they have said.
    level: u32,
}

impl Wire {
    /// The dialling side's.
    fn client(
        ws: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Wire {
        use tokio_tungstenite::tungstenite::Message;
        use tokio_tungstenite::tungstenite::protocol::CloseFrame;
        use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
        let (mut sink, mut stream) = ws.split();
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::channel::<Frame>(16);
        tokio::spawn(async move {
            while let Some(m) = stream.next().await {
                let f = match m {
                    Ok(Message::Text(t)) => Frame::Text(t.to_string()),
                    Ok(Message::Binary(b)) => Frame::Binary(b.to_vec()),
                    Ok(Message::Ping(_)) => Frame::Ping,
                    Ok(Message::Pong(_)) => Frame::Pong,
                    Ok(Message::Close(c)) => {
                        let _ = in_tx.send(Frame::Close(c.map(|c| c.reason.to_string())));
                        return;
                    }
                    Ok(_) => continue,
                    Err(_) => return,
                };
                if in_tx.send(f).is_err() {
                    return;
                }
            }
        });
        tokio::spawn(async move {
            while let Some(f) = out_rx.recv().await {
                let m = match f {
                    Frame::Text(t) => Message::Text(t.into()),
                    Frame::Binary(b) => Message::Binary(b.into()),
                    Frame::Ping => Message::Ping(Vec::new().into()),
                    Frame::Pong => Message::Pong(Vec::new().into()),
                    Frame::Close(why) => {
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame {
                                code: CloseCode::Library(4000),
                                reason: why.unwrap_or_default().into(),
                            })))
                            .await;
                        break;
                    }
                };
                if sink.send(m).await.is_err() {
                    return;
                }
            }
            let _ = sink.close().await;
        });
        Wire {
            rx: in_rx,
            tx: out_tx,
            level: LEVELS[0],
        }
    }

    /// The answering side's.
    fn server(ws: axum::extract::ws::WebSocket) -> Wire {
        use axum::extract::ws::{CloseFrame, Message};
        let (mut sink, mut stream) = ws.split();
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, mut out_rx) = mpsc::channel::<Frame>(16);
        tokio::spawn(async move {
            while let Some(m) = stream.next().await {
                let f = match m {
                    Ok(Message::Text(t)) => Frame::Text(t.to_string()),
                    Ok(Message::Binary(b)) => Frame::Binary(b.to_vec()),
                    Ok(Message::Ping(_)) => Frame::Ping,
                    Ok(Message::Pong(_)) => Frame::Pong,
                    Ok(Message::Close(c)) => {
                        let _ = in_tx.send(Frame::Close(c.map(|c| c.reason.to_string())));
                        return;
                    }
                    Err(_) => return,
                };
                if in_tx.send(f).is_err() {
                    return;
                }
            }
        });
        tokio::spawn(async move {
            while let Some(f) = out_rx.recv().await {
                let m = match f {
                    Frame::Text(t) => Message::Text(t.into()),
                    Frame::Binary(b) => Message::Binary(b.into()),
                    Frame::Ping => Message::Ping(Vec::new().into()),
                    Frame::Pong => Message::Pong(Vec::new().into()),
                    Frame::Close(why) => {
                        let _ = sink
                            .send(Message::Close(Some(CloseFrame {
                                code: 4000,
                                reason: why.unwrap_or_default().into(),
                            })))
                            .await;
                        break;
                    }
                };
                if sink.send(m).await.is_err() {
                    return;
                }
            }
            let _ = sink.close().await;
        });
        Wire {
            rx: in_rx,
            tx: out_tx,
            level: LEVELS[0],
        }
    }

    async fn recv(&mut self) -> Option<Frame> {
        self.rx.recv().await
    }

    async fn send(&self, f: Frame) -> bool {
        self.tx.send(f).await.is_ok()
    }

    async fn send_sealed(&self, msg: &Sealed) -> bool {
        match serde_json::to_string(msg) {
            Ok(t) => self.send(Frame::Text(t)).await,
            Err(_) => false,
        }
    }
}

/// What the first two messages carry: the levels each side speaks.
#[derive(Serialize, Deserialize)]
struct Hello {
    levels: [u32; 2],
}

fn speaks(levels: [u32; 2]) -> bool {
    levels[1] >= LEVELS[0] && levels[0] <= LEVELS[1]
}

/// The newest level both places speak, of a place that speaks levels.
fn agreed(levels: [u32; 2]) -> u32 {
    levels[1].min(LEVELS[1])
}

/// The largest file a place speaking level takes.
fn max_file(level: u32) -> u64 {
    if level >= 2 {
        MAX_FILE
    } else {
        LEVEL1_MAX_FILE
    }
}

/// A refusal's code, as a close says it.
fn code(r: &Refused) -> &'static str {
    match r {
        Refused::Elsewhere => "elsewhere",
        Refused::Stale => "stale",
        Refused::NotLinked => "not_linked",
        Refused::Unproven => "unproven",
    }
}

/// A refusal the other place sent, in words.
fn refused(code: &str) -> String {
    match code {
        "not_linked" => "it has not linked back to this place".into(),
        "elsewhere" => "another place answers at its address".into(),
        "stale" => "its clock and this place's differ by more than 5 minutes".into(),
        "unproven" => "it does not hold this place's key as given".into(),
        "levels" => "it speaks link levels this place does not".into(),
        "" => "it closed the connection".into(),
        other => other.into(),
    }
}

fn here<'a>(app: &'a App, place: &Place) -> Here<'a> {
    Here {
        place: place.id,
        sealer: app.cfg.sealer.as_deref(),
        root: &app.root,
        images: app.cfg.registry && app.blobs.is_some(),
    }
}

/// Keeps every link connected, and sends each change to a linked person's
/// records over their connections as soon as its write commits (the
/// store's Records signal).
pub async fn run(app: Arc<App>, place: Arc<Place>) {
    let mut keepers: HashMap<Key, tokio::task::JoinHandle<()>> = HashMap::new();
    loop {
        keep_links(&app, &place, &mut keepers).await;
        for user in scan_all(&app, &place).await {
            app.link_conns.offer(user, None);
        }
        tokio::select! {
            _ = app.store.signals.records.notified() => {}
            _ = app.links_wake.notified() => {}
            _ = tokio::time::sleep(TIDY) => {}
        }
    }
}

/// Keeps a task holding every link's connection, and none for a link that
/// has gone.
async fn keep_links(
    app: &Arc<App>,
    place: &Arc<Place>,
    keepers: &mut HashMap<Key, tokio::task::JoinHandle<()>>,
) {
    let links = match async { app.store.read().await?.links(None).await }.await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("hangar-server: reading links: {e}");
            return;
        }
    };
    let keys: std::collections::HashSet<Key> =
        links.iter().map(|l| (l.user_id, l.place_id)).collect();
    keepers.retain(|key, task| {
        let keep = keys.contains(key) && !task.is_finished();
        if !keep {
            task.abort();
            app.link_conns.close(*key);
        }
        keep
    });
    for key in keys {
        keepers
            .entry(key)
            .or_insert_with(|| tokio::spawn(keep(app.clone(), place.clone(), key)));
    }
}

/// Notices changes to every linked person's records; gives the people
/// whose records changed.
async fn scan_all(app: &App, place: &Place) -> Vec<Uuid> {
    let here = here(app, place);
    let scanned = async {
        let users = app.store.read().await?.linked_users().await?;
        let mut changed = vec![];
        for user in users {
            if scan(app, &here, user).await? {
                changed.push(user);
            }
        }
        Ok::<_, StoreError>(changed)
    };
    match scanned.await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hangar-server: looking for changes to linked records: {e}");
            vec![]
        }
    }
}

async fn scan(app: &App, here: &Here<'_>, user: Uuid) -> Result<bool, StoreError> {
    let mut tx = app.store.begin().await?;
    tx.lock_link_records(user).await?;
    let changed = crate::linksync::scan(&mut tx, here, user).await?;
    tx.commit().await?;
    Ok(changed)
}

/// The link as it stands, and its person.
async fn read_link(app: &App, (user, other): Key) -> Result<Option<(Uuid, Link)>, StoreError> {
    let mut tx = app.store.read().await?;
    let Some(link) = tx.link(user, other).await? else {
        return Ok(None);
    };
    Ok(Some((tx.person(user).await?, link)))
}

/// Holds a link's connection for as long as the link lasts: waits while
/// there is one, dials while there is none, backing off while it fails.
async fn keep(app: Arc<App>, place: Arc<Place>, key: Key) {
    let mut wait = RETRY_FIRST;
    loop {
        let ended = app.link_conns.ended(key);
        let gone = ended.notified();
        tokio::pin!(gone);
        gone.as_mut().enable();
        if app.link_conns.alive(key) {
            gone.await;
            wait = RETRY_FIRST;
            continue;
        }
        let (person, link) = match read_link(&app, key).await {
            Ok(Some(f)) => f,
            Ok(None) => return,
            Err(e) => {
                eprintln!("hangar-server: reading a link: {e}");
                tokio::time::sleep(RETRY_MAX).await;
                continue;
            }
        };
        let failed = match dial(&place, person, &link).await {
            Ok(Some(wire)) => match app.link_conns.take(key, place.id) {
                Some((held, cmds)) => {
                    serve(&app, place.clone(), person, link, wire, held, cmds, true).await;
                    wait = RETRY_FIRST;
                    None
                }
                // One the other dialled holds it.
                None => {
                    wire.send(Frame::Close(Some("held".into()))).await;
                    None
                }
            },
            // The other holds a connection for the link already.
            Ok(None) => None,
            Err(why) => Some(why),
        };
        if let Some(why) = failed {
            let kept = async {
                let mut tx = app.store.begin().await?;
                tx.set_reached(key.0, key.1, Err(why)).await?;
                tx.commit().await
            };
            if let Err(e) = kept.await {
                eprintln!("hangar-server: recording a link: {e}");
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = &mut gone => {}
        }
        wait = (wait * 2).min(RETRY_MAX);
    }
}

/// Dials a linked place and says who this place is: the connection, once
/// the other has answered as the key it was given for it; None when it
/// holds a connection for the link already; else why not, in words.
async fn dial(place: &Place, person: Uuid, link: &Link) -> Result<Option<Wire>, String> {
    use tokio_tungstenite::Connector;
    let base = link.url.trim_end_matches('/');
    let url = match base.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}/api/link/v1/connect"),
        Some(("http", rest)) => format!("ws://{rest}/api/link/v1/connect"),
        _ => return Err(format!("{base} is not an http or https address")),
    };
    let connector = if url.starts_with("wss:") {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(crate::ports::AnyCertificate(provider)))
            .with_no_client_auth();
        Connector::Rustls(Arc::new(tls))
    } else {
        Connector::Plain
    };
    let dialled = tokio::time::timeout(
        HANDSHAKE,
        tokio_tungstenite::connect_async_tls_with_config(url, None, true, Some(connector)),
    )
    .await
    .map_err(|_| "no response".to_string())?;
    let ws = match dialled {
        Ok((ws, _)) => ws,
        Err(tokio_tungstenite::tungstenite::Error::Io(e)) => {
            return Err(format!("could not connect: {e}"));
        }
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            return Err(format!("it answered {}", r.status()));
        }
        Err(e) => return Err(e.to_string()),
    };
    introduce(place, person, link, Wire::client(ws)).await
}

/// Says who this place is over a connection to a linked place, as the
/// side that dialled: the connection, once the other has answered as the
/// key given for it; None when it holds a connection for the link
/// already; else why not, in words.
async fn introduce(
    place: &Place,
    person: Uuid,
    link: &Link,
    mut wire: Wire,
) -> Result<Option<Wire>, String> {
    let hello = Sealed::make(
        place,
        "connect",
        link.place_id,
        &link.public_key,
        person,
        None,
        &Hello { levels: LEVELS },
    )?;
    if !wire.send_sealed(&hello).await {
        return Err("it closed the connection".into());
    }
    let answer = match tokio::time::timeout(HANDSHAKE, wire.recv()).await {
        Err(_) => return Err("no response".into()),
        Ok(Some(Frame::Text(t))) => t,
        Ok(Some(Frame::Close(Some(why)))) if why == "held" => return Ok(None),
        Ok(Some(Frame::Close(why))) => return Err(refused(why.as_deref().unwrap_or(""))),
        Ok(_) => return Err("it closed the connection".into()),
    };
    let answer: Sealed = serde_json::from_str(&answer).map_err(|e| e.to_string())?;
    if answer.from != link.place_id || answer.nonce != hello.nonce {
        return Err("it answered another message".into());
    }
    let theirs: Hello = answer
        .read(place, "connected", &link.public_key)
        .map_err(|r| refused(code(&r)))?;
    if !speaks(theirs.levels) {
        return Err(refused("levels"));
    }
    wire.level = agreed(theirs.levels);
    Ok(Some(wire))
}

/// Carries a link over a connection the person's app opened to this
/// place and pipes to the other place's link endpoint, for two places
/// neither of which reaches the other: this place dials through it as it
/// would directly, so the app carries what it cannot read. A place
/// holding a connection for the link already refuses one (held).
pub async fn relay(app: Arc<App>, user: Uuid, other: Uuid, socket: axum::extract::ws::WebSocket) {
    let wire = Wire::server(socket);
    let Some(place) = app.place.clone() else {
        wire.send(Frame::Close(Some("this place has no link key".into())))
            .await;
        return;
    };
    let key = (user, other);
    if app.link_conns.alive(key) {
        wire.send(Frame::Close(Some("held".into()))).await;
        return;
    }
    let (person, link) = match read_link(&app, key).await {
        Ok(Some(f)) => f,
        _ => {
            wire.send(Frame::Close(Some("not_linked".into()))).await;
            return;
        }
    };
    match introduce(&place, person, &link, wire).await {
        Ok(Some(wire)) => match app.link_conns.take(key, place.id) {
            Some((held, cmds)) => {
                serve(&app, place.clone(), person, link, wire, held, cmds, true).await;
            }
            None => {
                wire.send(Frame::Close(Some("held".into()))).await;
            }
        },
        Ok(None) => {}
        Err(why) => eprintln!("hangar-server: relaying a link through the app: {why}"),
    }
}

/// Takes a connection another place dialled: once its first message shows
/// it is a place a person here is linked with, as the key given for it,
/// this place answers and holds the connection.
pub async fn accept(app: Arc<App>, socket: axum::extract::ws::WebSocket) {
    let mut wire = Wire::server(socket);
    let Some(place) = app.place.clone() else {
        wire.send(Frame::Close(Some("this place has no link key".into())))
            .await;
        return;
    };
    let first = match tokio::time::timeout(HANDSHAKE, wire.recv()).await {
        Ok(Some(Frame::Text(t))) => t,
        _ => return,
    };
    let close = |why: &str| Frame::Close(Some(why.to_string()));
    let Ok(msg) = serde_json::from_str::<Sealed>(&first) else {
        wire.send(close("unproven")).await;
        return;
    };
    let link = match crate::link::admit(&app.store, msg.from, msg.person).await {
        Ok(l) => l,
        Err(r) => {
            wire.send(close(code(&r))).await;
            return;
        }
    };
    let theirs: Hello = match msg.read(&place, "connect", &link.public_key) {
        Ok(h) => h,
        Err(r) => {
            wire.send(close(code(&r))).await;
            return;
        }
    };
    if !speaks(theirs.levels) {
        wire.send(close("levels")).await;
        return;
    }
    wire.level = agreed(theirs.levels);
    let Some((held, cmds)) = app.link_conns.take((link.user_id, msg.from), msg.from) else {
        wire.send(close("held")).await;
        return;
    };
    let answer = match Sealed::make(
        &place,
        "connected",
        msg.from,
        &link.public_key,
        msg.person,
        Some(&msg.nonce),
        &Hello { levels: LEVELS },
    ) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("hangar-server: answering a linked place: {e}");
            return;
        }
    };
    if !wire.send_sealed(&answer).await {
        return;
    }
    serve(&app, place, msg.person, link, wire, held, cmds, false).await;
}

/// Carries a connection until it ends: the person's versions of records
/// and files sent on connecting, whenever their records change and every
/// hour, and their files as they change; the other's messages answered as
/// they come; pings to keep it, and an end when the other falls silent.
#[allow(clippy::too_many_arguments)]
async fn serve(
    app: &Arc<App>,
    place: Arc<Place>,
    person: Uuid,
    link: Link,
    mut wire: Wire,
    held: Held,
    mut cmds: mpsc::UnboundedReceiver<Cmd>,
    dialled: bool,
) {
    let (user, other) = (link.user_id, link.place_id);
    let here = here(app, &place);
    let mut exchanged = false;
    record(app, user, other, dialled, exchanged).await;
    let mut recorded = Instant::now();
    let max = max_file(wire.level);
    let asked = tokio::sync::Mutex::new(HashSet::new());
    let out = outbox(
        app.clone(),
        place.clone(),
        person,
        link.clone(),
        wire.tx.clone(),
        max,
    );
    let c = Conn {
        app,
        place: &place,
        here: &here,
        person,
        link: &link,
        wire: &wire,
        out: &out,
        max,
        level: wire.level,
        asked: &asked,
    };
    if let Err(e) = offer(&c).await {
        eprintln!("hangar-server: sending to a linked place: {e}");
    }
    // The side that dialled lists its files; the other answers with what
    // it wants and has newer, which covers both ways.
    if dialled && let Err(e) = offer_files(&c).await {
        eprintln!("hangar-server: sending files' versions to a linked place: {e}");
    }
    if let Err(e) = images(&c).await {
        eprintln!("hangar-server: asking a linked place for images: {e}");
    }
    let mut ping = tokio::time::interval_at(Instant::now() + PING, PING);
    let mut full = tokio::time::interval_at(Instant::now() + FULL, FULL);
    let mut heard = Instant::now();
    let mut rx = std::mem::replace(&mut wire.rx, mpsc::unbounded_channel().1);
    loop {
        let c = Conn {
            app,
            place: &place,
            here: &here,
            person,
            link: &link,
            wire: &wire,
            out: &out,
            max,
            level: wire.level,
            asked: &asked,
        };
        tokio::select! {
            f = rx.recv() => match f {
                Some(Frame::Text(t)) => {
                    heard = Instant::now();
                    match answer(&c, &t).await {
                        Ok(true) => {
                            exchanged = true;
                            record(app, user, other, dialled, exchanged).await;
                            recorded = Instant::now();
                        }
                        Ok(false) => {}
                        Err(e) => eprintln!("hangar-server: a linked place's message: {e}"),
                    }
                }
                Some(Frame::Binary(b)) => {
                    heard = Instant::now();
                    if let Err(e) = binary(&c, &b).await {
                        eprintln!("hangar-server: a linked place's file: {e}");
                    }
                }
                Some(Frame::Ping | Frame::Pong) => heard = Instant::now(),
                Some(Frame::Close(_)) | None => break,
            },
            cmd = cmds.recv() => match cmd {
                Some(Cmd::Offer) => {
                    if let Err(e) = offer(&c).await {
                        eprintln!("hangar-server: sending to a linked place: {e}");
                    }
                }
                Some(Cmd::OfferFiles) => {
                    if let Err(e) = offer_files(&c).await {
                        eprintln!("hangar-server: sending files' versions to a linked place: {e}");
                    }
                }
                Some(Cmd::Files(jobs)) => {
                    for j in jobs {
                        let _ = out.send(j);
                    }
                }
                Some(Cmd::Images) => {
                    if let Err(e) = images(&c).await {
                        eprintln!("hangar-server: asking a linked place for images: {e}");
                    }
                }
                Some(Cmd::Close) | None => break,
            },
            _ = ping.tick() => {
                if heard.elapsed() > SILENT || !wire.send(Frame::Ping).await {
                    break;
                }
                if recorded.elapsed() >= RECORD {
                    record(app, user, other, dialled, exchanged).await;
                    recorded = Instant::now();
                }
            }
            _ = full.tick() => {
                if let Err(e) = offer(&c).await {
                    eprintln!("hangar-server: sending to a linked place: {e}");
                }
                if dialled && let Err(e) = offer_files(&c).await {
                    eprintln!("hangar-server: sending files' versions to a linked place: {e}");
                }
                // What the other place lacked an hour ago it may have since.
                asked.lock().await.clear();
                if let Err(e) = images(&c).await {
                    eprintln!("hangar-server: asking a linked place for images: {e}");
                }
            }
        }
    }
    drop(held);
    wire.send(Frame::Close(None)).await;
}

/// What a connection's messages are answered with.
struct Conn<'a> {
    app: &'a App,
    place: &'a Place,
    here: &'a Here<'a>,
    person: Uuid,
    link: &'a Link,
    wire: &'a Wire,
    out: &'a Outbox,
    /// The largest file the other place takes.
    max: u64,
    /// The link level both places speak.
    level: u32,
    /// The manifests and blobs asked of the other place, by digest.
    asked: &'a tokio::sync::Mutex<HashSet<String>>,
}

/// The files a connection is to send, each with when it was asked for.
struct Outbox {
    files: mpsc::UnboundedSender<(Job, Instant)>,
    images: mpsc::UnboundedSender<crate::linkimages::Job>,
}

impl Outbox {
    /// Asks for a file to be sent; false once the connection has ended.
    fn send(&self, j: Job) -> bool {
        self.files.send((j, Instant::now())).is_ok()
    }

    /// Asks for a repository's manifest or blob to be sent.
    fn image(&self, j: crate::linkimages::Job) -> bool {
        self.images.send(j).is_ok()
    }
}

/// Sends files over a connection, a piece at a time, each file read as
/// its turn comes, so one changed while it waits goes as it is then, and
/// one asked for again while it waits goes once.
fn outbox(
    app: Arc<App>,
    place: Arc<Place>,
    person: Uuid,
    link: Link,
    tx: mpsc::Sender<Frame>,
    max: u64,
) -> Outbox {
    let (jobs_tx, mut jobs) = mpsc::unbounded_channel::<(Job, Instant)>();
    let (images_tx, mut images) = mpsc::unbounded_channel::<crate::linkimages::Job>();
    tokio::spawn(async move {
        let mut queue: VecDeque<(Job, Instant)> = VecDeque::new();
        // Manifests and blobs go after files, which people work with.
        let mut image_queue: VecDeque<crate::linkimages::Job> = VecDeque::new();
        let add_image = |q: &mut VecDeque<crate::linkimages::Job>, j: crate::linkimages::Job| {
            if !q.iter().any(|x| x.repo == j.repo && x.digest == j.digest) {
                q.push_back(j);
            }
        };
        let mut pacer = Pacer::default();
        // The files sent whole: the version of each, and when its last
        // piece went. Asked for again before then (on taking its version,
        // and on comparing versions), the same version is not sent again;
        // asked for after, it is: the other place does not have it, as
        // when it could not take a shared copy's file before the copy.
        let mut whole: HashMap<(String, String), (String, Instant)> = HashMap::new();
        let add = |queue: &mut VecDeque<(Job, Instant)>, (j, asked): (Job, Instant)| match queue
            .iter_mut()
            .find(|(q, _)| q.copy == j.copy && q.path == j.path)
        {
            Some((q, at)) => {
                q.send = q.send.and(j.send);
                *at = (*at).max(asked);
            }
            None => queue.push_back((j, asked)),
        };
        loop {
            if queue.is_empty() && image_queue.is_empty() {
                tokio::select! {
                    j = jobs.recv() => match j {
                        Some(j) => add(&mut queue, j),
                        None => return,
                    },
                    j = images.recv() => match j {
                        Some(j) => add_image(&mut image_queue, j),
                        None => return,
                    },
                }
            }
            while let Ok(j) = jobs.try_recv() {
                add(&mut queue, j);
            }
            while let Ok(j) = images.try_recv() {
                add_image(&mut image_queue, j);
            }
            let Some((job, asked)) = queue.pop_front() else {
                if let Some(job) = image_queue.pop_front() {
                    pacer.read(&app, link.user_id).await;
                    match send_image(&app, &place, person, &link, &tx, &mut pacer, &job).await {
                        Ok(()) => {}
                        Err(e) if e.is_empty() => return,
                        Err(e) => {
                            eprintln!("hangar-server: sending an image to a linked place: {e}")
                        }
                    }
                }
                continue;
            };
            pacer.read(&app, link.user_id).await;
            match crate::linkfiles::read(&app, link.user_id, &job, link.place_id, max).await {
                Ok(Some((piece, _)))
                    if piece.content
                        && piece.delta.is_none()
                        && whole
                            .get(&(job.copy.clone(), job.path.clone()))
                            .is_some_and(|(d, sent)| {
                                *d == piece.version.digest && asked < *sent
                            }) => {}
                Ok(Some((piece, body))) => {
                    let sent_whole = (piece.content && piece.delta.is_none())
                        .then(|| piece.version.digest.clone());
                    match send_file(&place, person, &link, &tx, &mut pacer, piece, body).await {
                        Ok(()) => {
                            if let Some(digest) = sent_whole {
                                whole.insert(
                                    (job.copy.clone(), job.path.clone()),
                                    (digest, Instant::now()),
                                );
                            }
                        }
                        Err(e) if e.is_empty() => return,
                        Err(e) => {
                            eprintln!("hangar-server: sending a file to a linked place: {e}")
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => eprintln!("hangar-server: reading a file for a linked place: {e}"),
            }
        }
    });
    Outbox {
        files: jobs_tx,
        images: images_tx,
    }
}

/// Sends a repository's manifest or blob in pieces, each read as its turn
/// comes; an empty error when the connection has ended.
async fn send_image(
    app: &App,
    place: &Place,
    person: Uuid,
    link: &Link,
    tx: &mpsc::Sender<Frame>,
    pacer: &mut Pacer,
    job: &crate::linkimages::Job,
) -> Result<(), String> {
    let Some((piece, source)) = crate::linkimages::read(app, link.user_id, job).await? else {
        return Ok(());
    };
    let mut pieces = Pieces::new(piece, source, PIECE);
    while let Some((piece, bytes)) = pieces.next().await? {
        let frame = Sealed::make_binary(
            place,
            "image",
            link.place_id,
            &link.public_key,
            person,
            &piece,
            &bytes,
        )?;
        pacer.wait(frame.len()).await;
        if tx.send(Frame::Binary(frame)).await.is_err() {
            return Err(String::new());
        }
    }
    Ok(())
}

/// How often a connection's file sender reads the person's bandwidth
/// limit again.
const BANDWIDTH_READ: Duration = Duration::from_secs(2);

/// Keeps a connection's files to the person's bandwidth limit: each piece
/// waits until the bytes sent before it have had their time.
#[derive(Default)]
struct Pacer {
    /// Bytes a second; 0 is no limit.
    bandwidth: i64,
    read: Option<Instant>,
    next: Option<Instant>,
}

impl Pacer {
    async fn read(&mut self, app: &App, user: Uuid) {
        if self.read.is_some_and(|r| r.elapsed() < BANDWIDTH_READ) {
            return;
        }
        let read = async {
            let mut tx = app.store.read().await?;
            tx.link_bandwidth(user).await
        };
        match read.await {
            Ok(b) => self.bandwidth = b,
            Err(e) => eprintln!("hangar-server: reading a link's bandwidth limit: {e}"),
        }
        self.read = Some(Instant::now());
    }

    async fn wait(&mut self, bytes: usize) {
        if self.bandwidth <= 0 {
            self.next = None;
            return;
        }
        if let Some(next) = self.next {
            tokio::time::sleep_until(next).await;
        }
        let start = self.next.map_or(Instant::now(), |n| n.max(Instant::now()));
        self.next = Some(start + Duration::from_secs_f64(bytes as f64 / self.bandwidth as f64));
    }
}

/// Sends a file in pieces, or its version alone, each piece read from the
/// file as its turn comes; an empty error when the connection has ended.
/// A resumed file starts where the other place's holding ends.
async fn send_file(
    place: &Place,
    person: Uuid,
    link: &Link,
    tx: &mpsc::Sender<Frame>,
    pacer: &mut Pacer,
    mut piece: FilePiece,
    body: Body,
) -> Result<(), String> {
    let total = if piece.content { piece.total() } else { 0 };
    let mut offset = body.from.min(total);
    loop {
        let n = usize::try_from((total - offset).min(PIECE as u64)).unwrap_or(PIECE);
        let bytes = if n == 0 {
            vec![]
        } else {
            tokio::task::block_in_place(|| body.read(offset, n))
                .map_err(|e| format!("reading {}: {e}", piece.path))?
        };
        if bytes.len() != n {
            return Err(format!("{} is shorter than its version", piece.path));
        }
        piece.offset = offset;
        let frame = Sealed::make_binary(
            place,
            "file",
            link.place_id,
            &link.public_key,
            person,
            &piece,
            &bytes,
        )?;
        pacer.wait(frame.len()).await;
        if tx.send(Frame::Binary(frame)).await.is_err() {
            return Err(String::new());
        }
        // The first piece says how the file is made; the rest carry bytes.
        piece.delta = None;
        offset += n as u64;
        if offset >= total {
            return Ok(());
        }
    }
}

/// Takes a piece of a file the other place sent: written to
/// `.links/incoming/` as it comes, and once the file is whole, taken.
/// What is taken goes on to the person's other places.
async fn binary(c: &Conn<'_>, frame: &[u8]) -> Result<(), String> {
    let (msg, body) = Sealed::parse_binary(frame).ok_or("a message that cannot be read")?;
    if msg.from != c.link.place_id || msg.person != c.person {
        return Err("a message from another place, or for another person".into());
    }
    let unread = |r| format!("it could not be read: {r:?}");
    match msg.kind.as_str() {
        "file" => {
            let (p, bytes): (FilePiece, Vec<u8>) = msg
                .read_binary(body, c.place, "file", &c.link.public_key)
                .map_err(unread)?;
            piece(c, p, bytes).await
        }
        "image" if c.level >= 2 => {
            let (p, bytes): (ImagePiece, Vec<u8>) = msg
                .read_binary(body, c.place, "image", &c.link.public_key)
                .map_err(unread)?;
            let user = c.link.user_id;
            if crate::linkimages::receive(c.app, user, c.link.place_id, p, &bytes).await? {
                images(c).await?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Brings the person's pending repositories as far as what is here lets,
/// and asks the other place for what they lack, at link level 2.
async fn images(c: &Conn<'_>) -> Result<(), String> {
    if c.level < 2 || !c.here.images {
        return Ok(());
    }
    let mut asked = c.asked.lock().await;
    let wants =
        crate::linkimages::reconcile(c.app, c.link.user_id, c.link.place_id, &mut asked).await?;
    drop(asked);
    if !wants.is_empty() {
        let msg = sealed(c.place, "image_want", c.person, c.link, &Wants { wants })?;
        c.wire.send_sealed(&msg).await;
    }
    Ok(())
}

/// Takes a piece of a file the other place sent.
async fn piece(c: &Conn<'_>, p: FilePiece, bytes: Vec<u8>) -> Result<(), String> {
    let (app, user, from) = (c.app, c.link.user_id, c.link.place_id);
    if !p.content {
        return taken(c, &p, None).await;
    }
    if p.version.size > MAX_FILE || p.total() > MAX_FILE {
        return Ok(());
    }
    let Some(came) =
        tokio::task::block_in_place(|| app.link_files.receive(&app.root, user, from, p, &bytes))?
    else {
        return Ok(());
    };
    let Some(rel) = crate::linkfiles::rebuild(app, user, &came).await? else {
        // This place's copy is not the one the delta is from: the file is
        // asked for whole.
        let want = FileWant {
            want: vec![(came.piece.copy.clone(), came.piece.path.clone())],
            ..FileWant::default()
        };
        let msg = sealed(c.place, "file_want", c.person, c.link, &want)?;
        c.wire.send_sealed(&msg).await;
        return Ok(());
    };
    let done = taken(c, &came.piece, Some(&rel)).await;
    let _ = tokio::task::block_in_place(|| app.root.remove(&rel));
    done
}

/// Takes a file the other place sent, its content at rel in the files
/// root (none for its version alone), and answers what taking it found.
async fn taken(c: &Conn<'_>, p: &FilePiece, rel: Option<&str>) -> Result<(), String> {
    let user = c.link.user_id;
    let t = crate::linkfiles::take(c.app, c.place.id, user, c.link.place_id, p, rel).await?;
    for j in t.reply {
        let _ = c.out.send(j);
    }
    if !t.want.is_empty() {
        let msg = sealed(c.place, "file_want", c.person, c.link, &t.want)?;
        c.wire.send_sealed(&msg).await;
    }
    c.app.link_conns.files(user, Some(c.link.place_id), t.relay);
    c.app.link_conns.files(user, None, t.found);
    Ok(())
}

/// Sends the versions of the person's files, having looked for changes
/// to them first; a change found goes to their other connections too.
async fn offer_files(c: &Conn<'_>) -> Result<(), String> {
    let user = c.link.user_id;
    let (versions, found) =
        crate::linkfiles::versions(c.app, c.place.id, user, c.link.place_id, c.max).await?;
    c.app.link_conns.files(user, Some(c.link.place_id), found);
    let msg = sealed(c.place, "file_versions", c.person, c.link, &versions)?;
    c.wire.send_sealed(&msg).await;
    Ok(())
}

/// Writes down that the link is connected: reached by the side that
/// dialled, heard from by the other (whose own failures to dial no longer
/// matter), and its records exchanged while it lasts once they first
/// were.
async fn record(app: &App, user: Uuid, other: Uuid, dialled: bool, exchanged: bool) {
    let now = Utc::now();
    let kept = async {
        let mut tx = app.store.begin().await?;
        if dialled {
            tx.set_reached(user, other, Ok(now)).await?;
        } else {
            tx.set_heard(user, other, now).await?;
            tx.set_reached(user, other, Err(String::new())).await?;
        }
        if exchanged {
            tx.set_synced(user, other, Ok(now)).await?;
        }
        tx.commit().await
    };
    if let Err(e) = kept.await {
        eprintln!("hangar-server: recording a link: {e}");
    }
}

fn sealed<T: Serialize>(
    place: &Place,
    kind: &str,
    person: Uuid,
    link: &Link,
    payload: &T,
) -> Result<Sealed, String> {
    Sealed::make(
        place,
        kind,
        link.place_id,
        &link.public_key,
        person,
        None,
        payload,
    )
}

/// Sends the person's versions, having noticed any change first; a change
/// found goes to their other connections too.
async fn offer(c: &Conn<'_>) -> Result<(), String> {
    let user = c.link.user_id;
    let (changed, versions) = async {
        let mut tx = c.app.store.begin().await?;
        tx.lock_link_records(user).await?;
        let changed = crate::linksync::scan(&mut tx, c.here, user).await?;
        let v = crate::linksync::versions(&mut tx, user).await?;
        tx.commit().await?;
        Ok::<_, StoreError>((changed, v))
    }
    .await
    .map_err(|e| e.to_string())?;
    if changed {
        c.app.link_conns.offer(user, Some(c.link.place_id));
    }
    let versions = versions
        .into_iter()
        .filter(|v| crate::linksync::for_level(&v.kind, c.level))
        .collect();
    let msg = sealed(
        c.place,
        "versions",
        c.person,
        c.link,
        &Versions { versions },
    )?;
    c.wire.send_sealed(&msg).await;
    Ok(())
}

/// Answers one of the other's messages: its versions with what this place
/// wants and has newer; its answer to this place's with the records it
/// wanted; records with taking them; its files' versions with the files
/// this place wants and those it has newer; files wanted with sending
/// them. Gives whether an exchange of records finished.
async fn answer(c: &Conn<'_>, text: &str) -> Result<bool, String> {
    let msg: Sealed = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if msg.from != c.link.place_id || msg.person != c.person {
        return Err("a message from another place, or for another person".into());
    }
    let (app, here, user, key) = (c.app, c.here, c.link.user_id, &c.link.public_key);
    let unread = |r: Refused| format!("it could not be read: {r:?}");
    match msg.kind.as_str() {
        "versions" => {
            let theirs: Versions = msg.read(c.place, "versions", key).map_err(unread)?;
            let (changed, ex) = async {
                let mut tx = app.store.begin().await?;
                tx.lock_link_records(user).await?;
                let changed = crate::linksync::scan(&mut tx, here, user).await?;
                let mut ex =
                    crate::linksync::exchange(&mut tx, here, user, &theirs.versions).await?;
                tx.commit().await?;
                ex.records
                    .retain(|r| crate::linksync::for_level(&r.version.kind, c.level));
                Ok::<_, StoreError>((changed, ex))
            }
            .await
            .map_err(|e| e.to_string())?;
            if changed {
                app.link_conns.offer(user, Some(c.link.place_id));
            }
            c.wire
                .send_sealed(&sealed(c.place, "exchange", c.person, c.link, &ex)?)
                .await;
            Ok(false)
        }
        "exchange" => {
            let ex: Exchange = msg.read(c.place, "exchange", key).map_err(unread)?;
            let (taken, wanted) = async {
                let mut tx = app.store.begin().await?;
                tx.lock_link_records(user).await?;
                let taken = crate::linksync::accept(&mut tx, here, user, &ex.records).await?;
                let mut wanted = crate::linksync::records(&mut tx, here, user, &ex.want).await?;
                tx.commit().await?;
                wanted.retain(|r| crate::linksync::for_level(&r.version.kind, c.level));
                Ok::<_, StoreError>((taken, wanted))
            }
            .await
            .map_err(|e| e.to_string())?;
            if taken > 0 {
                app.link_conns.offer(user, Some(c.link.place_id));
                // A copy or pack taken may hold files to compare, and a
                // repository manifests and blobs to ask for.
                app.link_conns.offer_files(user);
                app.link_conns.images(user);
            }
            if !wanted.is_empty() {
                let msg = sealed(
                    c.place,
                    "records",
                    c.person,
                    c.link,
                    &Records { records: wanted },
                )?;
                c.wire.send_sealed(&msg).await;
            }
            Ok(true)
        }
        "records" => {
            let sent: Records = msg.read(c.place, "records", key).map_err(unread)?;
            let taken = async {
                let mut tx = app.store.begin().await?;
                tx.lock_link_records(user).await?;
                let taken = crate::linksync::accept(&mut tx, here, user, &sent.records).await?;
                tx.commit().await?;
                Ok::<_, StoreError>(taken)
            }
            .await
            .map_err(|e| e.to_string())?;
            if taken > 0 {
                app.link_conns.offer(user, Some(c.link.place_id));
                app.link_conns.offer_files(user);
                app.link_conns.images(user);
            }
            Ok(true)
        }
        "file_versions" => {
            let theirs: FileVersions = msg.read(c.place, "file_versions", key).map_err(unread)?;
            let found =
                crate::linkfiles::compare(app, c.place.id, user, c.link.place_id, &theirs).await?;
            if !found.want.is_empty() {
                let msg = sealed(c.place, "file_want", c.person, c.link, &found.want)?;
                c.wire.send_sealed(&msg).await;
            }
            for j in found.send {
                let _ = c.out.send(j);
            }
            app.link_conns
                .files(user, Some(c.link.place_id), found.found);
            Ok(false)
        }
        "file_want" => {
            let w: FileWant = msg.read(c.place, "file_want", key).map_err(unread)?;
            for (copy, path) in w.want {
                let held = w.held.iter().find(|h| h.copy == copy && h.path == path);
                let base = w.bases.iter().find(|b| b.copy == copy && b.path == path);
                let send = match (held, base) {
                    (Some(h), _) => Carry::After {
                        digest: h.digest.clone(),
                        from: h.have,
                    },
                    (None, Some(b)) => Carry::Delta(Arc::new(b.clone())),
                    (None, None) => Carry::Whole,
                };
                let _ = c.out.send(Job { copy, path, send });
            }
            Ok(false)
        }
        "image_want" if c.level >= 2 => {
            let w: Wants = msg.read(c.place, "image_want", key).map_err(unread)?;
            let jobs = crate::linkimages::jobs(app, user, &w.wants)
                .await
                .map_err(|e| e.to_string())?;
            for j in jobs {
                let _ = c.out.image(j);
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}
