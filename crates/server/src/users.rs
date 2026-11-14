//! Who can use Hangar: accounts, how they sign in, the sessions that carry
//! a signed-in browser, and the access tokens tools sign in with. Hashes
//! and secrets are the Go server's (internal/users), so each server accepts
//! the other's.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use chrono::{Duration, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::audit::{self, Actor, Event};
use crate::store::rows::{AccessToken, Session, User};
use crate::store::{Result, Store};

/// How long a session lasts without being used.
pub const SESSION_LIFETIME: Duration = Duration::days(14);

/// How often a session in use has its expiry pushed back.
const SESSION_REFRESH: Duration = Duration::hours(1);

/// What every access token starts with.
pub const TOKEN_PREFIX: &str = "hgr_";

/// Who is signed in.
#[derive(Debug, Clone)]
pub struct Principal {
    pub user_id: Uuid,
    pub username: String,
}

impl Principal {
    fn of(u: &User) -> Principal {
        Principal {
            user_id: u.id,
            username: u.username.clone(),
        }
    }

    /// The principal acting, for the audit log.
    pub fn actor(&self, ip: &str) -> Actor {
        Actor {
            user: Some(self.user_id),
            worker: None,
            name: self.username.clone(),
            ip: ip.to_string(),
        }
    }
}

fn argon() -> Argon2<'static> {
    let params = Params::new(19 * 1024, 2, 1, Some(32)).expect("argon2 parameters");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// An argon2id hash of password in PHC string form.
pub fn hash_password(password: &str) -> String {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let salt = SaltString::encode_b64(&salt).expect("a salt");
    argon()
        .hash_password(password.as_bytes(), &salt)
        .expect("hashing a password")
        .to_string()
}

/// Whether password is the one hash was made of.
pub fn check_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| argon().verify_password(password.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

/// A hash no password matches, checked when there is no such user, so
/// a missing user takes as long to refuse as a wrong password.
fn dummy_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| hash_password("hangar-no-such-user"))
}

fn secret() -> String {
    let mut raw = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut raw);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

fn sha256(s: &str) -> Vec<u8> {
    Sha256::digest(s.as_bytes()).to_vec()
}

/// Makes the administrator named username, with password, unless there is
/// a user by that name; says whether it did.
pub async fn ensure_admin(store: &Store, username: &str, password: &str) -> Result<bool> {
    let mut tx = store.begin().await?;
    if tx.person_by_name(username).await?.is_some() {
        return Ok(false);
    }
    let u = User {
        id: Uuid::new_v4(),
        username: username.to_string(),
        display_name: String::new(),
        password_hash: Some(hash_password(password)),
        is_admin: true,
        kind: "person".into(),
        disabled_at: None,
        created_at: Utc::now(),
        theme: "system".into(),
    };
    tx.insert_user(&u).await?;
    let system = Actor {
        name: "hangar".into(),
        ..Actor::default()
    };
    audit::record(
        &mut tx,
        &system,
        Event::new("user.create", "user", u.id, username)
            .details(serde_json::json!({ "admin": true })),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Signs in username with password, giving the session's token and the
/// user; None for a wrong name or password, or a disabled user.
pub async fn login(
    store: &Store,
    ip: &str,
    username: &str,
    password: &str,
) -> Result<Option<(String, User)>> {
    let mut tx = store.begin().await?;
    let found = tx.person_by_name(username).await?;
    let hash = found
        .as_ref()
        .and_then(|u| u.password_hash.clone())
        .unwrap_or_else(|| dummy_hash().to_string());
    let ok = check_password(&hash, password);
    let user = match found {
        Some(u) if ok && u.password_hash.is_some() && u.disabled_at.is_none() => u,
        found => {
            let reason = if ok && found.is_some() {
                "disabled"
            } else {
                "bad credentials"
            };
            let id = found.map(|u| u.id.to_string()).unwrap_or_default();
            let who = Actor {
                name: username.to_string(),
                ip: ip.to_string(),
                ..Actor::default()
            };
            audit::record(
                &mut tx,
                &who,
                Event::new("auth.login_failed", "user", id, username)
                    .details(serde_json::json!({ "reason": reason })),
            )
            .await?;
            tx.commit().await?;
            return Ok(None);
        }
    };
    let token = secret();
    let now = Utc::now();
    tx.insert_session(&Session {
        token_hash: sha256(&token),
        user_id: user.id,
        created_at: now,
        last_seen_at: now,
        expires_at: now + SESSION_LIFETIME,
    })
    .await?;
    audit::record(
        &mut tx,
        &Principal::of(&user).actor(ip),
        Event::new("auth.login", "user", user.id, &user.username),
    )
    .await?;
    tx.commit().await?;
    Ok(Some((token, user)))
}

/// Who a session's token signs in, pushing its expiry back when it has not
/// been for a while.
pub async fn session(store: &Store, token: &str) -> Result<Option<Principal>> {
    if token.is_empty() {
        return Ok(None);
    }
    let hash = sha256(token);
    let now = Utc::now();
    let mut tx = store.begin().await?;
    let Some((user, s)) = tx.session(&hash, now).await? else {
        return Ok(None);
    };
    if now - s.last_seen_at > SESSION_REFRESH {
        tx.touch_session(&hash, now, now + SESSION_LIFETIME).await?;
        tx.commit().await?;
    }
    Ok(Some(Principal::of(&user)))
}

/// Who an access token signs in.
pub async fn token(store: &Store, token: &str) -> Result<Option<Principal>> {
    if !token.starts_with(TOKEN_PREFIX) {
        return Ok(None);
    }
    let now = Utc::now();
    let mut tx = store.begin().await?;
    let Some((user, t)) = tx.token(&sha256(token), now).await? else {
        return Ok(None);
    };
    if t.last_used_at
        .is_none_or(|at| now - at > Duration::minutes(1))
    {
        tx.touch_token(t.id, now).await?;
        tx.commit().await?;
    }
    Ok(Some(Principal::of(&user)))
}

/// Ends the session whose token this is.
pub async fn logout(store: &Store, p: &Principal, ip: &str, token: &str) -> Result<()> {
    let mut tx = store.begin().await?;
    if let Some(user) = tx.delete_session(&sha256(token)).await? {
        audit::record(
            &mut tx,
            &p.actor(ip),
            Event::new("auth.logout", "user", user, &p.username),
        )
        .await?;
    }
    tx.commit().await
}

/// The user signed in as p.
pub async fn me(store: &Store, p: &Principal) -> Result<Option<User>> {
    store.begin().await?.user(p.user_id).await
}

/// Makes an access token for p, lasting days, or until revoked; gives the
/// token, shown this once, and its record.
pub async fn create_token(
    store: &Store,
    p: &Principal,
    ip: &str,
    name: &str,
    days: Option<u64>,
) -> Result<(String, AccessToken)> {
    let token = format!("{TOKEN_PREFIX}{}", secret());
    let now = Utc::now();
    let t = AccessToken {
        id: Uuid::new_v4(),
        user_id: p.user_id,
        name: name.to_string(),
        token_hash: sha256(&token),
        created_at: now,
        last_used_at: None,
        expires_at: days.map(|d| now + Duration::days(d as i64)),
    };
    let mut tx = store.begin().await?;
    tx.insert_token(&t).await?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("token.create", "token", t.id, name)
            .related("user", p.user_id)
            .related("owner", p.user_id),
    )
    .await?;
    tx.commit().await?;
    Ok((token, t))
}

/// p's access tokens, newest first.
pub async fn tokens(store: &Store, p: &Principal) -> Result<Vec<AccessToken>> {
    store.begin().await?.tokens_of(p.user_id).await
}

/// Revokes one of p's access tokens; false if p has none by that ID.
pub async fn revoke_token(store: &Store, p: &Principal, ip: &str, id: Uuid) -> Result<bool> {
    let mut tx = store.begin().await?;
    let Some(t) = tx.delete_token(p.user_id, id).await? else {
        return Ok(false);
    };
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("token.revoke", "token", t.id, &t.name)
            .related("user", p.user_id)
            .related("owner", p.user_id),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Whether token is the access token t keeps the hash of.
pub fn is_token(token: &str, t: &AccessToken) -> bool {
    sha256(token) == t.token_hash
}
