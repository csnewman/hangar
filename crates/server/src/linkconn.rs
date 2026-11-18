//! Linked places' connections (docs/places.md §4.5): one WebSocket for
//! each linked person and pair of places, dialled by whichever side can
//! reach the other and kept open while both are up. Over it each side
//! sends the person's records as soon as they change, and both exchange
//! them in full on connecting and every hour (`linksync`). Every message
//! is `Sealed`: signed by its sender and sealed to its recipient.
//!
//! Both places of a link try to hold one connection, each dialling while
//! there is none, backing off while it cannot. Two dialled at once settle
//! on the one dialled by the place of lower ID.

use std::collections::HashMap;
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

    fn close(&self, key: Key) {
        if let Some(l) = self.live.lock().unwrap().remove(&key) {
            let _ = l.tx.send(Cmd::Close);
        }
    }
}

/// What a connection carries, whichever side of it this place is.
enum Frame {
    Text(String),
    Ping,
    Pong,
    /// Closing, and why (a refusal's code).
    Close(Option<String>),
}

/// A connection's two directions, as channels, from a WebSocket of either
/// side.
struct Wire {
    rx: mpsc::Receiver<Frame>,
    tx: mpsc::Sender<Frame>,
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
        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel::<Frame>(16);
        tokio::spawn(async move {
            while let Some(m) = stream.next().await {
                let f = match m {
                    Ok(Message::Text(t)) => Frame::Text(t.to_string()),
                    Ok(Message::Ping(_)) => Frame::Ping,
                    Ok(Message::Pong(_)) => Frame::Pong,
                    Ok(Message::Close(c)) => {
                        let _ = in_tx
                            .send(Frame::Close(c.map(|c| c.reason.to_string())))
                            .await;
                        return;
                    }
                    Ok(_) => continue,
                    Err(_) => return,
                };
                if in_tx.send(f).await.is_err() {
                    return;
                }
            }
        });
        tokio::spawn(async move {
            while let Some(f) = out_rx.recv().await {
                let m = match f {
                    Frame::Text(t) => Message::Text(t.into()),
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
        }
    }

    /// The answering side's.
    fn server(ws: axum::extract::ws::WebSocket) -> Wire {
        use axum::extract::ws::{CloseFrame, Message};
        let (mut sink, mut stream) = ws.split();
        let (in_tx, in_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel::<Frame>(16);
        tokio::spawn(async move {
            while let Some(m) = stream.next().await {
                let f = match m {
                    Ok(Message::Text(t)) => Frame::Text(t.to_string()),
                    Ok(Message::Ping(_)) => Frame::Ping,
                    Ok(Message::Pong(_)) => Frame::Pong,
                    Ok(Message::Close(c)) => {
                        let _ = in_tx
                            .send(Frame::Close(c.map(|c| c.reason.to_string())))
                            .await;
                        return;
                    }
                    Ok(_) => continue,
                    Err(_) => return,
                };
                if in_tx.send(f).await.is_err() {
                    return;
                }
            }
        });
        tokio::spawn(async move {
            while let Some(f) = out_rx.recv().await {
                let m = match f {
                    Frame::Text(t) => Message::Text(t.into()),
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
    let links = match async { app.store.begin().await?.links(None).await }.await {
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
        let users = app.store.begin().await?.linked_users().await?;
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
    let mut tx = app.store.begin().await?;
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
                    serve(&app, &place, person, link, wire, held, cmds, true).await;
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
    let mut wire = Wire::client(ws);
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
    Ok(Some(wire))
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
    serve(&app, &place, msg.person, link, wire, held, cmds, false).await;
}

/// Carries a connection until it ends: the person's versions sent on
/// connecting, whenever their records change and every hour; the other's
/// messages answered as they come; pings to keep it, and an end when the
/// other falls silent.
#[allow(clippy::too_many_arguments)]
async fn serve(
    app: &Arc<App>,
    place: &Place,
    person: Uuid,
    link: Link,
    mut wire: Wire,
    held: Held,
    mut cmds: mpsc::UnboundedReceiver<Cmd>,
    dialled: bool,
) {
    let (user, other) = (link.user_id, link.place_id);
    let here = here(app, place);
    let mut exchanged = false;
    record(app, user, other, dialled, exchanged).await;
    let mut recorded = Instant::now();
    if let Err(e) = offer(app, place, &here, person, &link, &wire).await {
        eprintln!("hangar-server: sending to a linked place: {e}");
    }
    let mut ping = tokio::time::interval_at(Instant::now() + PING, PING);
    let mut full = tokio::time::interval_at(Instant::now() + FULL, FULL);
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            f = wire.recv() => match f {
                Some(Frame::Text(t)) => {
                    heard = Instant::now();
                    match answer(app, place, &here, person, &link, &wire, &t).await {
                        Ok(true) => {
                            exchanged = true;
                            record(app, user, other, dialled, exchanged).await;
                            recorded = Instant::now();
                        }
                        Ok(false) => {}
                        Err(e) => eprintln!("hangar-server: a linked place's message: {e}"),
                    }
                }
                Some(Frame::Ping | Frame::Pong) => heard = Instant::now(),
                Some(Frame::Close(_)) | None => break,
            },
            c = cmds.recv() => match c {
                Some(Cmd::Offer) => {
                    if let Err(e) = offer(app, place, &here, person, &link, &wire).await {
                        eprintln!("hangar-server: sending to a linked place: {e}");
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
                if let Err(e) = offer(app, place, &here, person, &link, &wire).await {
                    eprintln!("hangar-server: sending to a linked place: {e}");
                }
            }
        }
    }
    drop(held);
    wire.send(Frame::Close(None)).await;
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
async fn offer(
    app: &App,
    place: &Place,
    here: &Here<'_>,
    person: Uuid,
    link: &Link,
    wire: &Wire,
) -> Result<(), String> {
    let user = link.user_id;
    let (changed, versions) = async {
        let mut tx = app.store.begin().await?;
        tx.lock_link_records(user).await?;
        let changed = crate::linksync::scan(&mut tx, here, user).await?;
        let v = crate::linksync::versions(&mut tx, user).await?;
        tx.commit().await?;
        Ok::<_, StoreError>((changed, v))
    }
    .await
    .map_err(|e| e.to_string())?;
    if changed {
        app.link_conns.offer(user, Some(link.place_id));
    }
    let msg = sealed(place, "versions", person, link, &Versions { versions })?;
    wire.send_sealed(&msg).await;
    Ok(())
}

/// Answers one of the other's messages: its versions with what this place
/// wants and has newer; its answer to this place's with the records it
/// wanted; records with taking them. Gives whether an exchange of records
/// finished.
async fn answer(
    app: &App,
    place: &Place,
    here: &Here<'_>,
    person: Uuid,
    link: &Link,
    wire: &Wire,
    text: &str,
) -> Result<bool, String> {
    let msg: Sealed = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if msg.from != link.place_id || msg.person != person {
        return Err("a message from another place, or for another person".into());
    }
    let user = link.user_id;
    let unread = |r: Refused| format!("it could not be read: {r:?}");
    match msg.kind.as_str() {
        "versions" => {
            let theirs: Versions = msg
                .read(place, "versions", &link.public_key)
                .map_err(unread)?;
            let (changed, ex) = async {
                let mut tx = app.store.begin().await?;
                tx.lock_link_records(user).await?;
                let changed = crate::linksync::scan(&mut tx, here, user).await?;
                let ex = crate::linksync::exchange(&mut tx, here, user, &theirs.versions).await?;
                tx.commit().await?;
                Ok::<_, StoreError>((changed, ex))
            }
            .await
            .map_err(|e| e.to_string())?;
            if changed {
                app.link_conns.offer(user, Some(link.place_id));
            }
            wire.send_sealed(&sealed(place, "exchange", person, link, &ex)?)
                .await;
            Ok(false)
        }
        "exchange" => {
            let ex: Exchange = msg
                .read(place, "exchange", &link.public_key)
                .map_err(unread)?;
            let (taken, wanted) = async {
                let mut tx = app.store.begin().await?;
                tx.lock_link_records(user).await?;
                let taken = crate::linksync::accept(&mut tx, here, user, &ex.records).await?;
                let wanted = crate::linksync::records(&mut tx, here, user, &ex.want).await?;
                tx.commit().await?;
                Ok::<_, StoreError>((taken, wanted))
            }
            .await
            .map_err(|e| e.to_string())?;
            if taken > 0 {
                app.link_conns.offer(user, Some(link.place_id));
            }
            if !wanted.is_empty() {
                let msg = sealed(place, "records", person, link, &Records { records: wanted })?;
                wire.send_sealed(&msg).await;
            }
            Ok(true)
        }
        "records" => {
            let sent: Records = msg
                .read(place, "records", &link.public_key)
                .map_err(unread)?;
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
                app.link_conns.offer(user, Some(link.place_id));
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}
