//! Linked places (docs/places.md §4): this place's own identity, an
//! Ed25519 key and an ID made once and kept, linking a person's account
//! here with another place, and the messages linked places send each
//! other (`Sealed`): signed by the sender and sealed to the recipient's
//! key, so neither a proxy that ends TLS nor anyone between can read or
//! change them, whatever certificate either presents. The connections
//! that carry them are `linkconn`'s.

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

/// Why a message from another place was refused.
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
/// one-time key of the sender's, base64, and the sealed bytes.
fn seal(to_public_key: &str, aad: &str, plain: &[u8]) -> Result<(String, Vec<u8>), String> {
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
    Ok((b64(&eph.to_bytes()), buf))
}

impl Place {
    /// Opens what was sealed to this place, bound to aad.
    fn open(&self, eph: &str, sealed: &[u8], aad: &str) -> Option<Vec<u8>> {
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
        let mut buf = sealed.to_vec();
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

    /// What is signed of a binary message: the same, its sealed bytes'
    /// SHA-256 marked as a binary message's.
    fn binary_statement(&self, sealed: &[u8]) -> String {
        let digest = ring::digest::digest(&ring::digest::SHA256, sealed);
        format!(
            "{}\n{}\nbinary {}",
            self.aad(),
            self.eph,
            hex(digest.as_ref())
        )
    }

    /// A message's header, unsealed and unsigned, for a payload to be
    /// sealed into.
    fn header(place: &Place, kind: &str, to: Uuid, person: Uuid, nonce: Option<&str>) -> Sealed {
        let nonce = match nonce {
            Some(n) => n.to_string(),
            None => {
                let mut n = [0u8; 16];
                rand::thread_rng().fill_bytes(&mut n);
                b64(&n)
            }
        };
        Sealed {
            from: place.id,
            to,
            person,
            time: Utc::now().timestamp(),
            nonce,
            kind: kind.to_string(),
            eph: String::new(),
            data: String::new(),
            signature: String::new(),
        }
    }

    /// Whether it is a message of this kind to place, recent, signed by
    /// the key given over statement.
    fn check(
        &self,
        place: &Place,
        kind: &str,
        from_key: &str,
        statement: &str,
    ) -> Result<(), Refused> {
        if self.to != place.id {
            return Err(Refused::Elsewhere);
        }
        if self.kind != kind || (Utc::now().timestamp() - self.time).abs() > SKEW {
            return Err(Refused::Stale);
        }
        if !verify(from_key, statement, &self.signature) {
            return Err(Refused::Unproven);
        }
        Ok(())
    }

    /// A message carrying bytes beside its payload, as a binary frame:
    /// the header's length (four bytes, big-endian) and the header, a
    /// message whose data is empty, then the sealed bytes, which open to
    /// the payload's length (the same way), the payload and the bytes.
    #[allow(clippy::too_many_arguments)]
    pub fn make_binary(
        place: &Place,
        kind: &str,
        to: Uuid,
        to_key: &str,
        person: Uuid,
        payload: &impl Serialize,
        bytes: &[u8],
    ) -> Result<Vec<u8>, String> {
        let mut s = Sealed::header(place, kind, to, person, None);
        let json = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
        let mut plain = Vec::with_capacity(4 + json.len() + bytes.len());
        plain.extend_from_slice(&(json.len() as u32).to_be_bytes());
        plain.extend_from_slice(&json);
        plain.extend_from_slice(bytes);
        let sealed;
        (s.eph, sealed) = seal(to_key, &s.aad(), &plain)?;
        s.signature = place.sign(&s.binary_statement(&sealed));
        let head = serde_json::to_vec(&s).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(4 + head.len() + sealed.len());
        out.extend_from_slice(&(head.len() as u32).to_be_bytes());
        out.extend_from_slice(&head);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// A binary frame's header and its sealed bytes, unchecked.
    pub fn parse_binary(frame: &[u8]) -> Option<(Sealed, &[u8])> {
        let n = u32::from_be_bytes(frame.get(..4)?.try_into().ok()?) as usize;
        let head = frame.get(4..4 + n)?;
        Some((serde_json::from_slice(head).ok()?, &frame[4 + n..]))
    }

    /// What a binary message carries, its payload and bytes, once it is
    /// shown to be a message of this kind to place from the place whose
    /// key is given, and recent.
    pub fn read_binary<T: serde::de::DeserializeOwned>(
        &self,
        sealed: &[u8],
        place: &Place,
        kind: &str,
        from_key: &str,
    ) -> Result<(T, Vec<u8>), Refused> {
        self.check(place, kind, from_key, &self.binary_statement(sealed))?;
        let plain = place
            .open(&self.eph, sealed, &self.aad())
            .ok_or(Refused::Unproven)?;
        let n = plain
            .get(..4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_be_bytes)
            .ok_or(Refused::Unproven)? as usize;
        let json = plain.get(4..4 + n).ok_or(Refused::Unproven)?;
        let payload = serde_json::from_slice(json).map_err(|_| Refused::Unproven)?;
        Ok((payload, plain[4 + n..].to_vec()))
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
        let mut s = Sealed::header(place, kind, to, person, nonce);
        let plain = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
        let sealed;
        (s.eph, sealed) = seal(to_key, &s.aad(), &plain)?;
        s.data = b64(&sealed);
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
        self.check(place, kind, from_key, &self.statement())?;
        let sealed = base64::engine::general_purpose::STANDARD
            .decode(&self.data)
            .map_err(|_| Refused::Unproven)?;
        let plain = place
            .open(&self.eph, &sealed, &self.aad())
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
