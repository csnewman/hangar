//! Linked places (docs/places.md §4): this place's own identity, an
//! Ed25519 key made once and kept, and the hello by which two linked
//! places prove to each other that each holds the key the other was
//! given, over whatever certificate either presents.
//!
//! A hello says who is asking (from), whom (to), for which person, when
//! and a nonce, signed by the asker; the answer signs the same back. The
//! place answering records that the asker reached it, the asker that it
//! reached the other. Every minute, whenever a link is made and whenever a
//! linked person's records change, this place says hello to each place
//! its people are linked with and exchanges their records with it
//! (`linksync`), in messages signed by the sender and sealed to the
//! recipient's key (`Sealed`), so neither a proxy that ends TLS nor one
//! in between can read or change them.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use chrono::Utc;
use rand::RngCore;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::seal::Sealer;
use crate::store::rows::{Link, ServerKey};
use crate::store::{Error as StoreError, Store};

/// The link protocol's levels this place speaks, oldest and newest.
pub const LEVELS: [u32; 2] = [1, 1];

/// How often every link is said hello to.
const EVERY: Duration = Duration::from_secs(60);

/// How long a hello may take.
const WITHIN: Duration = Duration::from_secs(15);

/// How far a hello's time may be from this place's clock.
const SKEW: i64 = 300;

/// This place: its ID, and the key that proves it, whose X25519 form
/// (`x_secret`) is what messages to it are sealed to.
pub struct Place {
    pub id: Uuid,
    pair: Ed25519KeyPair,
    x_secret: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct Kept {
    id: Uuid,
    seed: String,
}

impl Place {
    /// This place's identity: made once and kept, sealed when the server
    /// has a secret key, so every server sharing the store is the same
    /// place.
    pub async fn load(store: &Store, sealer: Option<&Sealer>) -> Result<Place, String> {
        const NAME: &str = "link-place";
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        let fresh = serde_json::to_vec(&Kept {
            id: Uuid::new_v4(),
            seed: hex(&seed),
        })
        .map_err(|e| e.to_string())?;
        let value = match sealer {
            Some(s) => s.seal(&fresh, NAME),
            None => fresh,
        };
        let k = async {
            let mut tx = store.begin().await?;
            let k = tx
                .server_key(&ServerKey {
                    name: NAME.into(),
                    value,
                    sealed: sealer.is_some(),
                    created_at: Utc::now(),
                })
                .await?;
            tx.commit().await?;
            Ok::<_, StoreError>(k)
        }
        .await
        .map_err(|e| e.to_string())?;
        let plain = if k.sealed {
            sealer
                .ok_or("this place's key is sealed, and the server has no secret key to open it")?
                .open(&k.value, NAME)?
        } else {
            k.value
        };
        let kept: Kept = serde_json::from_slice(&plain).map_err(|e| e.to_string())?;
        let seed = crate::seal::hex(&kept.seed)
            .filter(|s| s.len() == 32)
            .ok_or("this place's key is not 32 bytes")?;
        let pair = Ed25519KeyPair::from_seed_unchecked(&seed).map_err(|e| e.to_string())?;
        // An Ed25519 key's scalar is the first half of its seed's SHA-512,
        // which, clamped, is the same key on the Montgomery curve.
        let mut x_secret = [0u8; 32];
        x_secret
            .copy_from_slice(&ring::digest::digest(&ring::digest::SHA512, &seed).as_ref()[..32]);
        Ok(Place {
            id: kept.id,
            pair,
            x_secret,
        })
    }

    /// Its public key, base64.
    pub fn public_key(&self) -> String {
        b64(self.pair.public_key().as_ref())
    }

    fn sign(&self, statement: &str) -> String {
        b64(self.pair.sign(statement.as_bytes()).as_ref())
    }
}

/// Whether signature (base64) is public_key's (base64) over statement.
fn verify(public_key: &str, statement: &str, signature: &str) -> bool {
    let engine = base64::engine::general_purpose::STANDARD;
    let (Ok(key), Ok(sig)) = (engine.decode(public_key), engine.decode(signature)) else {
        return false;
    };
    UnparsedPublicKey::new(&ED25519, key)
        .verify(statement.as_bytes(), &sig)
        .is_ok()
}

/// Whether s is an Ed25519 public key in base64.
pub fn is_public_key(s: &str) -> bool {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .is_ok_and(|k| k.len() == 32)
}

/// A hello: who asks whom, for which person, when, signed by the asker.
#[derive(Debug, Serialize, Deserialize)]
pub struct Hello {
    pub from: Uuid,
    pub to: Uuid,
    pub person: Uuid,
    pub time: i64,
    pub nonce: String,
    pub signature: String,
}

/// The answer: the levels the place answering speaks, and its signature
/// over the same hello.
#[derive(Debug, Serialize, Deserialize)]
pub struct Answer {
    pub levels: [u32; 2],
    pub signature: String,
}

impl Hello {
    /// What is signed: the asker's (hello) or the answer's (answer).
    fn statement(&self, kind: &str) -> String {
        format!(
            "hangar-link-v1 {kind}\n{}\n{}\n{}\n{}\n{}",
            self.from, self.to, self.person, self.time, self.nonce
        )
    }
}

/// Why a hello was refused, as an API error's code and words.
#[derive(Debug, PartialEq)]
pub enum Refused {
    /// Not addressed to this place.
    Elsewhere,
    /// Its time is too far from this place's clock.
    Stale,
    /// The person has no account here linked with the asker.
    NotLinked,
    /// The signature is not the key this place was given for the asker.
    Unproven,
}

/// Answers a hello another place sent, and records that it reached this
/// one. When this place had not reached it, wake has this place try again
/// at once, as the other is plainly there.
pub async fn answer(
    store: &Store,
    place: &Place,
    wake: &tokio::sync::Notify,
    h: &Hello,
) -> Result<Answer, Refused> {
    if h.to != place.id {
        return Err(Refused::Elsewhere);
    }
    if (Utc::now().timestamp() - h.time).abs() > SKEW {
        return Err(Refused::Stale);
    }
    let found = async {
        let mut tx = store.begin().await?;
        let Some(user) = tx.user_by_person(h.person).await? else {
            return Ok(None);
        };
        let link = tx.link(user, h.from).await?;
        Ok::<_, StoreError>(link)
    }
    .await
    .map_err(|e| {
        eprintln!("hangar-server: a linked place's hello: {e}");
        Refused::NotLinked
    })?;
    let Some(link) = found else {
        return Err(Refused::NotLinked);
    };
    if !verify(&link.public_key, &h.statement("hello"), &h.signature) {
        return Err(Refused::Unproven);
    }
    let heard = async {
        let mut tx = store.begin().await?;
        tx.set_heard(link.user_id, link.place_id, Utc::now())
            .await?;
        tx.commit().await
    };
    if let Err(e) = heard.await {
        eprintln!("hangar-server: recording a linked place's hello: {e}");
    }
    if link.reached_at.is_none() || !link.reach_error.is_empty() {
        wake.notify_one();
    }
    Ok(Answer {
        levels: LEVELS,
        signature: place.sign(&h.statement("answer")),
    })
}

/// Says hello to a linked place, for person: Ok when it answered as the
/// key this place was given for it, else why not, in words.
pub async fn hello(place: &Place, person: Uuid, link: &Link) -> Result<(), String> {
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);
    let mut h = Hello {
        from: place.id,
        to: link.place_id,
        person,
        time: Utc::now().timestamp(),
        nonce: b64(&nonce),
        signature: String::new(),
    };
    h.signature = place.sign(&h.statement("hello"));
    let url = format!("{}/api/link/v1/hello", link.url.trim_end_matches('/'));
    let resp = client()?
        .post(&url)
        .json(&h)
        .send()
        .await
        .map_err(|e| words(&e))?;
    let status = resp.status();
    if !status.is_success() {
        #[derive(Deserialize)]
        struct Error {
            #[serde(default)]
            code: String,
            #[serde(default)]
            error: String,
        }
        let e: Error = resp.json().await.unwrap_or(Error {
            code: String::new(),
            error: String::new(),
        });
        return Err(match e.code.as_str() {
            "not_linked" => "it has not linked back to this place".into(),
            "elsewhere" => "another place answers at its address".into(),
            "stale" => "its clock and this place's differ by more than 5 minutes".into(),
            "unproven" => "it does not hold this place's key as given".into(),
            _ if !e.error.is_empty() => e.error,
            _ => format!("it answered {status}"),
        });
    }
    let a: Answer = resp.json().await.map_err(|e| e.to_string())?;
    if !verify(&link.public_key, &h.statement("answer"), &a.signature) {
        return Err("it answered with a key other than the one this place was given".into());
    }
    if a.levels[1] < LEVELS[0] || a.levels[0] > LEVELS[1] {
        return Err(format!(
            "it speaks link levels {}-{}, this place {}-{}",
            a.levels[0], a.levels[1], LEVELS[0], LEVELS[1]
        ));
    }
    Ok(())
}

/// Why a link could not be made.
#[derive(Debug)]
pub enum Unmade {
    /// The user is linked as another person already.
    Differs,
    /// Another user here is that person already.
    Taken,
    Store(StoreError),
}

impl From<StoreError> for Unmade {
    fn from(e: StoreError) -> Unmade {
        Unmade::Store(e)
    }
}

/// Links a person's account here with another place, as the person given,
/// or changes the link; gives it as kept.
pub async fn put(
    store: &Store,
    p: &crate::users::Principal,
    ip: &str,
    link: Link,
    person: Uuid,
) -> Result<Link, Unmade> {
    let mut tx = store.begin().await?;
    let user = p.user_id;
    let others = tx
        .links(Some(user))
        .await?
        .into_iter()
        .any(|l| l.place_id != link.place_id);
    if others && tx.person(user).await? != person {
        return Err(Unmade::Differs);
    }
    if !tx.set_person(user, person).await? {
        return Err(Unmade::Taken);
    }
    tx.put_link(&link).await?;
    crate::audit::record(
        &mut tx,
        &p.actor(ip),
        crate::audit::Event::new("link.put", "place", link.place_id, &link.name)
            .related("user", user)
            .details(serde_json::json!({ "url": link.url, "person": person })),
    )
    .await?;
    let kept = tx.link(user, link.place_id).await?;
    tx.commit().await?;
    Ok(kept.unwrap_or(link))
}

/// Unlinks a person's account here from a place; false if it was not
/// linked.
pub async fn remove(
    store: &Store,
    p: &crate::users::Principal,
    ip: &str,
    place: Uuid,
) -> Result<bool, StoreError> {
    let mut tx = store.begin().await?;
    let Some(link) = tx.link(p.user_id, place).await? else {
        return Ok(false);
    };
    tx.delete_link(p.user_id, place).await?;
    crate::audit::record(
        &mut tx,
        &p.actor(ip),
        crate::audit::Event::new("link.delete", "place", place, &link.name)
            .related("user", p.user_id),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// A failed request in words.
fn words(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        return "no response".into();
    }
    if e.is_connect() {
        let mut why = e.to_string();
        let mut src = std::error::Error::source(e);
        while let Some(s) = src {
            why = s.to_string();
            src = s.source();
        }
        return format!("could not connect: {why}");
    }
    e.to_string()
}

fn client() -> Result<reqwest::Client, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(crate::ports::AnyCertificate(provider)))
        .with_no_client_auth();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .connect_timeout(Duration::from_secs(10))
        .timeout(WITHIN)
        .user_agent(concat!("hangar-server/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

/// How often this place looks for changes to its linked people's records,
/// to send them on.
const SCAN: Duration = Duration::from_secs(5);

/// Keeps every link: looks for changes to linked people's records every
/// few seconds, and says hello to each linked place and exchanges records
/// with it every minute, whenever a change is found, and whenever woken.
pub async fn run(app: Arc<crate::http::App>, place: Arc<Place>) {
    let mut due = tokio::time::Instant::now();
    let mut woke = true;
    loop {
        let changed = scan_all(&app, &place).await;
        if changed || woke || tokio::time::Instant::now() >= due {
            reach_all(&app, &place).await;
            due = tokio::time::Instant::now() + EVERY;
        }
        woke = tokio::select! {
            _ = tokio::time::sleep(SCAN) => false,
            _ = app.links_wake.notified() => true,
        };
    }
}

fn here<'a>(app: &'a crate::http::App, place: &Place) -> crate::linksync::Here<'a> {
    crate::linksync::Here {
        place: place.id,
        sealer: app.cfg.sealer.as_deref(),
        root: &app.root,
    }
}

/// Notices changes to every linked person's records; gives whether there
/// were any.
async fn scan_all(app: &crate::http::App, place: &Place) -> bool {
    let here = here(app, place);
    let scanned = async {
        let users = app.store.begin().await?.linked_users().await?;
        let mut changed = false;
        for user in users {
            let mut tx = app.store.begin().await?;
            tx.lock_link_records(user).await?;
            changed |= crate::linksync::scan(&mut tx, &here, user).await?;
            tx.commit().await?;
        }
        Ok::<_, StoreError>(changed)
    };
    match scanned.await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("hangar-server: looking for changes to linked records: {e}");
            false
        }
    }
}

async fn reach_all(app: &crate::http::App, place: &Place) {
    let store = &app.store;
    let links = async {
        let mut tx = store.begin().await?;
        let links = tx.links(None).await?;
        let mut out = Vec::new();
        for l in links {
            let person = tx.person(l.user_id).await?;
            out.push((person, l));
        }
        Ok::<_, StoreError>(out)
    };
    let links = match links.await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("hangar-server: reading links: {e}");
            return;
        }
    };
    let tries = links.into_iter().map(|(person, l)| async move {
        let reached = hello(place, person, &l).await.map(|()| Utc::now());
        let synced = match &reached {
            Ok(_) => Some(sync(app, place, person, &l).await.map(|()| Utc::now())),
            Err(_) => None,
        };
        (l, reached, synced)
    });
    for (l, reached, synced) in futures_util::future::join_all(tries).await {
        let kept = async {
            let mut tx = store.begin().await?;
            tx.set_reached(l.user_id, l.place_id, reached).await?;
            if let Some(synced) = synced {
                tx.set_synced(l.user_id, l.place_id, synced).await?;
            }
            tx.commit().await
        };
        if let Err(e) = kept.await {
            eprintln!("hangar-server: recording a link: {e}");
        }
    }
}

/// Exchanges a person's records with a linked place: sends every version
/// this place has, takes the records it has newer, and sends those it
/// wants.
async fn sync(
    app: &crate::http::App,
    place: &Place,
    person: Uuid,
    link: &Link,
) -> Result<(), String> {
    use crate::linksync::{Exchange, Records, Versions};
    let here = here(app, place);
    let user = link.user_id;
    let ours = async {
        let mut tx = app.store.begin().await?;
        tx.lock_link_records(user).await?;
        crate::linksync::scan(&mut tx, &here, user).await?;
        let v = crate::linksync::versions(&mut tx, user).await?;
        tx.commit().await?;
        Ok::<_, StoreError>(v)
    }
    .await
    .map_err(|e| e.to_string())?;
    let msg = Sealed::make(
        place,
        "versions",
        link.place_id,
        &link.public_key,
        person,
        None,
        &Versions { versions: ours },
    )?;
    let answer = post(link, "sync", &msg).await?.ok_or("it gave no answer")?;
    if answer.from != link.place_id || answer.person != person || answer.nonce != msg.nonce {
        return Err("it answered another message".into());
    }
    let ex: Exchange = answer
        .read(place, "exchange", &link.public_key)
        .map_err(|r| format!("its answer could not be read: {r:?}"))?;
    let (taken, wanted) = async {
        let mut tx = app.store.begin().await?;
        tx.lock_link_records(user).await?;
        let taken = crate::linksync::accept(&mut tx, &here, user, &ex.records).await?;
        let wanted = crate::linksync::records(&mut tx, &here, user, &ex.want).await?;
        tx.commit().await?;
        Ok::<_, StoreError>((taken, wanted))
    }
    .await
    .map_err(|e| e.to_string())?;
    if taken > 0 {
        // What came may be news to the other linked places.
        app.links_wake.notify_one();
    }
    if !wanted.is_empty() {
        let msg = Sealed::make(
            place,
            "records",
            link.place_id,
            &link.public_key,
            person,
            None,
            &Records { records: wanted },
        )?;
        post(link, "push", &msg).await?;
    }
    Ok(())
}

/// Answers another place's versions of a person's records: what this
/// place wants, and the records it has newer, sealed to it.
pub async fn answer_sync(
    app: &crate::http::App,
    place: &Place,
    msg: &Sealed,
) -> Result<Sealed, Refused> {
    use crate::linksync::Versions;
    let link = admit(&app.store, msg.from, msg.person).await?;
    let theirs: Versions = msg.read(place, "versions", &link.public_key)?;
    let here = here(app, place);
    let user = link.user_id;
    let ex = async {
        let mut tx = app.store.begin().await?;
        tx.lock_link_records(user).await?;
        crate::linksync::scan(&mut tx, &here, user).await?;
        let ex = crate::linksync::exchange(&mut tx, &here, user, &theirs.versions).await?;
        tx.set_heard(user, link.place_id, Utc::now()).await?;
        tx.commit().await?;
        Ok::<_, StoreError>(ex)
    }
    .await
    .map_err(|e| {
        eprintln!("hangar-server: answering a linked place: {e}");
        Refused::NotLinked
    })?;
    Sealed::make(
        place,
        "exchange",
        msg.from,
        &link.public_key,
        msg.person,
        Some(&msg.nonce),
        &ex,
    )
    .map_err(|e| {
        eprintln!("hangar-server: answering a linked place: {e}");
        Refused::Unproven
    })
}

/// Takes the records another place sent for a person.
pub async fn accept_push(
    app: &crate::http::App,
    place: &Place,
    msg: &Sealed,
) -> Result<(), Refused> {
    use crate::linksync::Records;
    let link = admit(&app.store, msg.from, msg.person).await?;
    let sent: Records = msg.read(place, "records", &link.public_key)?;
    let here = here(app, place);
    let user = link.user_id;
    let taken = async {
        let mut tx = app.store.begin().await?;
        tx.lock_link_records(user).await?;
        let taken = crate::linksync::accept(&mut tx, &here, user, &sent.records).await?;
        tx.commit().await?;
        Ok::<_, StoreError>(taken)
    }
    .await
    .map_err(|e| {
        eprintln!("hangar-server: taking a linked place's records: {e}");
        Refused::NotLinked
    })?;
    if taken > 0 {
        app.links_wake.notify_one();
    }
    Ok(())
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A place's public key (base64 Ed25519) as the X25519 key messages to it
/// are sealed to.
fn x_public(public_key: &str) -> Option<curve25519_dalek::montgomery::MontgomeryPoint> {
    let bytes: [u8; 32] = base64::engine::general_purpose::STANDARD
        .decode(public_key)
        .ok()?
        .try_into()
        .ok()?;
    curve25519_dalek::edwards::CompressedEdwardsY(bytes)
        .decompress()
        .map(|p| p.to_montgomery())
}

/// The key a message is sealed with: HKDF-SHA256 of the X25519 secret the
/// sender's one-time key and the recipient's share, bound to both.
fn box_key(
    shared: curve25519_dalek::montgomery::MontgomeryPoint,
    eph: &[u8; 32],
    to: &[u8; 32],
) -> Result<ring::aead::LessSafeKey, String> {
    if shared.to_bytes() == [0u8; 32] {
        return Err("a key that agrees on nothing".into());
    }
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(eph);
    salt.extend_from_slice(to);
    let prk = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, &salt).extract(&shared.to_bytes());
    let okm = prk
        .expand(&[b"hangar-link-v1 box"], &ring::aead::CHACHA20_POLY1305)
        .map_err(|_| "deriving a key".to_string())?;
    Ok(ring::aead::LessSafeKey::new(ring::aead::UnboundKey::from(
        okm,
    )))
}

/// Seals plain to the place whose public key is given, bound to aad: a
/// one-time key of the sender's, and the sealed bytes, both base64.
fn seal(to_public_key: &str, aad: &str, plain: &[u8]) -> Result<(String, String), String> {
    let to = x_public(to_public_key).ok_or("the place's key is not an Ed25519 public key")?;
    let mut eph_secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut eph_secret);
    let eph = curve25519_dalek::constants::X25519_BASEPOINT.mul_clamped(eph_secret);
    let key = box_key(to.mul_clamped(eph_secret), &eph.to_bytes(), &to.to_bytes())?;
    let mut buf = plain.to_vec();
    key.seal_in_place_append_tag(
        ring::aead::Nonce::assume_unique_for_key([0u8; 12]),
        ring::aead::Aad::from(aad.as_bytes()),
        &mut buf,
    )
    .map_err(|_| "sealing".to_string())?;
    Ok((b64(&eph.to_bytes()), b64(&buf)))
}

impl Place {
    /// Opens what was sealed to this place, bound to aad.
    fn open(&self, eph: &str, sealed: &str, aad: &str) -> Option<Vec<u8>> {
        let engine = base64::engine::general_purpose::STANDARD;
        let eph: [u8; 32] = engine.decode(eph).ok()?.try_into().ok()?;
        let eph = curve25519_dalek::montgomery::MontgomeryPoint(eph);
        let me = curve25519_dalek::constants::X25519_BASEPOINT.mul_clamped(self.x_secret);
        let key = box_key(
            eph.mul_clamped(self.x_secret),
            &eph.to_bytes(),
            &me.to_bytes(),
        )
        .ok()?;
        let mut buf = engine.decode(sealed).ok()?;
        let plain = key
            .open_in_place(
                ring::aead::Nonce::assume_unique_for_key([0u8; 12]),
                ring::aead::Aad::from(aad.as_bytes()),
                &mut buf,
            )
            .ok()?;
        Some(plain.to_vec())
    }
}

/// A message between linked places: who sends it to whom, for which
/// person, when, its kind, and what it carries sealed to the recipient,
/// all signed by the sender. An answer has the nonce of what it answers.
#[derive(Debug, Serialize, Deserialize)]
pub struct Sealed {
    pub from: Uuid,
    pub to: Uuid,
    pub person: Uuid,
    pub time: i64,
    pub nonce: String,
    pub kind: String,
    pub eph: String,
    pub data: String,
    pub signature: String,
}

impl Sealed {
    /// What the sealed data is bound to.
    fn aad(&self) -> String {
        format!(
            "hangar-link-v1 {}\n{}\n{}\n{}\n{}\n{}",
            self.kind, self.from, self.to, self.person, self.time, self.nonce
        )
    }

    /// What is signed: the same, the one-time key, and the data's SHA-256.
    fn statement(&self) -> String {
        let digest = ring::digest::digest(&ring::digest::SHA256, self.data.as_bytes());
        format!("{}\n{}\n{}", self.aad(), self.eph, hex(digest.as_ref()))
    }

    /// A message from place to the place `to` (whose key is given), for
    /// person, carrying payload; an answer gives the nonce it answers.
    pub fn make(
        place: &Place,
        kind: &str,
        to: Uuid,
        to_key: &str,
        person: Uuid,
        nonce: Option<&str>,
        payload: &impl Serialize,
    ) -> Result<Sealed, String> {
        let nonce = match nonce {
            Some(n) => n.to_string(),
            None => {
                let mut n = [0u8; 16];
                rand::thread_rng().fill_bytes(&mut n);
                b64(&n)
            }
        };
        let mut s = Sealed {
            from: place.id,
            to,
            person,
            time: Utc::now().timestamp(),
            nonce,
            kind: kind.to_string(),
            eph: String::new(),
            data: String::new(),
            signature: String::new(),
        };
        let plain = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
        (s.eph, s.data) = seal(to_key, &s.aad(), &plain)?;
        s.signature = place.sign(&s.statement());
        Ok(s)
    }

    /// What it carries, once it is shown to be a message of this kind to
    /// place from the place whose key is given, and recent.
    pub fn read<T: serde::de::DeserializeOwned>(
        &self,
        place: &Place,
        kind: &str,
        from_key: &str,
    ) -> Result<T, Refused> {
        if self.to != place.id {
            return Err(Refused::Elsewhere);
        }
        if self.kind != kind || (Utc::now().timestamp() - self.time).abs() > SKEW {
            return Err(Refused::Stale);
        }
        if !verify(from_key, &self.statement(), &self.signature) {
            return Err(Refused::Unproven);
        }
        let plain = place
            .open(&self.eph, &self.data, &self.aad())
            .ok_or(Refused::Unproven)?;
        serde_json::from_slice(&plain).map_err(|_| Refused::Unproven)
    }
}

/// The user a message is for, and their link with the place sending it:
/// the person's account here linked with that place.
pub async fn admit(store: &Store, from: Uuid, person: Uuid) -> Result<Link, Refused> {
    let found = async {
        let mut tx = store.begin().await?;
        let Some(user) = tx.user_by_person(person).await? else {
            return Ok(None);
        };
        tx.link(user, from).await
    }
    .await
    .map_err(|e: StoreError| {
        eprintln!("hangar-server: a linked place's message: {e}");
        Refused::NotLinked
    })?;
    found.ok_or(Refused::NotLinked)
}

/// Posts a message to a linked place, giving its answer.
pub async fn post(link: &Link, path: &str, msg: &Sealed) -> Result<Option<Sealed>, String> {
    let url = format!("{}/api/link/v1/{path}", link.url.trim_end_matches('/'));
    let resp = client()?
        .post(&url)
        .json(msg)
        .send()
        .await
        .map_err(|e| words(&e))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }
    if !status.is_success() {
        #[derive(Deserialize)]
        struct Error {
            #[serde(default)]
            error: String,
        }
        let e = resp
            .json::<Error>()
            .await
            .map(|e| e.error)
            .unwrap_or_default();
        return Err(if e.is_empty() {
            format!("it answered {status}")
        } else {
            e
        });
    }
    resp.json().await.map(Some).map_err(|e| e.to_string())
}
