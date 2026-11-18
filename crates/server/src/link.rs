//! Linked places (docs/places.md §4): this place's own identity, an
//! Ed25519 key made once and kept, and the hello by which two linked
//! places prove to each other that each holds the key the other was
//! given, over whatever certificate either presents.
//!
//! A hello says who is asking (from), whom (to), for which person, when
//! and a nonce, signed by the asker; the answer signs the same back. The
//! place answering records that the asker reached it, the asker that it
//! reached the other. Every minute, and whenever a link is made, this
//! place says hello to each place its people are linked with.

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

/// This place: its ID, and the key that proves it.
pub struct Place {
    pub id: Uuid,
    pair: Ed25519KeyPair,
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
        Ok(Place { id: kept.id, pair })
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

/// Says hello to every link now, and then every minute or whenever woken.
pub async fn run(store: Store, place: Arc<Place>, wake: Arc<tokio::sync::Notify>) {
    loop {
        reach_all(&store, &place).await;
        tokio::select! {
            _ = tokio::time::sleep(EVERY) => {}
            _ = wake.notified() => {}
        }
    }
}

async fn reach_all(store: &Store, place: &Place) {
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
        let outcome = hello(place, person, &l).await.map(|()| Utc::now());
        (l, outcome)
    });
    for (l, outcome) in futures_util::future::join_all(tries).await {
        let kept = async {
            let mut tx = store.begin().await?;
            tx.set_reached(l.user_id, l.place_id, outcome).await?;
            tx.commit().await
        };
        if let Err(e) = kept.await {
            eprintln!("hangar-server: recording a link: {e}");
        }
    }
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
