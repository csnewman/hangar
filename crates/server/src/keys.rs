//! People's SSH keys, kept sealed to sign for them in their environments;
//! the public keys they sign in to the SSH gateway with; and the gateway's
//! own host key (internal/profile's store, internal/sshgw's HostKey).

use chrono::{DateTime, Utc};
use rand::RngCore;
use russh::keys::ssh_key::{HashAlg, LineEnding, PrivateKey, PublicKey};
use uuid::Uuid;

use crate::audit::{self, Event};
use crate::fail::{self, Fail};
use crate::seal::Sealer;
use crate::store::keys::Added;
use crate::store::rows::{LoginKey, ServerKey, SshKey, User};
use crate::store::{Error as StoreError, Result as StoreResult, Store};
use crate::users::Principal;

/// Why an SSH key cannot be kept.
pub const NO_SECRET_KEY: &str =
    "the server has no secret key (HANGAR_SECRET_KEY_FILE), so it cannot keep SSH keys";

/// A key as its owner sees it.
pub struct View {
    pub id: Uuid,
    pub name: String,
    pub public_key: String,
    /// SHA256:..., as ssh-keygen -l prints it.
    pub fingerprint: String,
    pub created_at: DateTime<Utc>,
}

/// A public key's SHA256 fingerprint.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// A public key in authorized_keys form, with no comment.
fn authorized(key: &PublicKey) -> String {
    let mut k = key.clone();
    k.set_comment("");
    k.to_openssh().unwrap_or_default().trim().to_string()
}

/// A public key and its comment from a line in authorized_keys form.
fn parse_authorized(line: &str) -> Option<(PublicKey, String)> {
    let entry: russh::keys::ssh_key::authorized_keys::Entry = line.trim().parse().ok()?;
    let key = entry.public_key().clone();
    let comment = key.comment().as_str_lossy().to_string();
    Some((key, comment))
}

fn view_ssh(k: &SshKey) -> View {
    View {
        id: k.id,
        name: k.name.clone(),
        public_key: k.public_key.clone(),
        fingerprint: parse_authorized(&k.public_key)
            .map(|(key, _)| fingerprint(&key))
            .unwrap_or_default(),
        created_at: k.created_at,
    }
}

fn view_login(k: &LoginKey) -> View {
    View {
        id: k.id,
        name: k.name.clone(),
        public_key: k.public_key.clone(),
        fingerprint: k.fingerprint.clone(),
        created_at: k.created_at,
    }
}

/// A person's SSH keys, oldest first.
pub async fn ssh_keys(store: &Store, p: &Principal) -> StoreResult<Vec<View>> {
    Ok(store
        .begin()
        .await?
        .ssh_keys(p.user_id)
        .await?
        .iter()
        .map(view_ssh)
        .collect())
}

/// Keeps an SSH key for p: the private key given, in OpenSSH or PEM form,
/// or a new ed25519 one. A key with a passphrase is refused: the server
/// signs with no one there to type it.
pub async fn add_ssh_key(
    store: &Store,
    sealer: Option<&Sealer>,
    p: &Principal,
    ip: &str,
    name: &str,
    private: Option<&str>,
) -> fail::Result<View> {
    let name = name.trim().to_string();
    if name.is_empty() || name.chars().count() > 100 {
        return fail::invalid("a key needs a name of up to 100 characters");
    }
    let Some(sealer) = sealer else {
        return Err(Fail::Invalid(NO_SECRET_KEY.into()));
    };
    let (private, public) = match private {
        Some(text) => {
            if text.contains("ENCRYPTED")
                || PrivateKey::from_openssh(text).is_ok_and(|k| k.is_encrypted())
            {
                return fail::invalid("the key has a passphrase; remove it first (ssh-keygen -p)");
            }
            let key = russh::keys::decode_secret_key(text, None)
                .or_else(|e| fail::invalid(format!("not a private key: {e}")))?;
            (text.as_bytes().to_vec(), key.public_key().clone())
        }
        None => {
            let mut seed = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut seed);
            let mut key = PrivateKey::from(
                russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&seed),
            );
            key.set_comment(name.as_str());
            let pem = key
                .to_openssh(LineEnding::LF)
                .map_err(|e| StoreError(e.to_string()))?;
            (pem.as_bytes().to_vec(), key.public_key().clone())
        }
    };
    let fp = fingerprint(&public);
    let k = SshKey {
        id: Uuid::new_v4(),
        user_id: p.user_id,
        name: name.clone(),
        public_key: format!("{} {name}", authorized(&public)),
        private_key: sealer.seal(&private, &format!("{}/ssh/{fp}", p.user_id)),
        created_at: Utc::now(),
    };
    let mut tx = store.begin().await?;
    tx.insert_ssh_key(&k).await?;
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("ssh_key.add", "ssh_key", k.id, &name)
            .related("owner", p.user_id)
            .details(serde_json::json!({ "fingerprint": fp })),
    )
    .await?;
    tx.commit().await?;
    Ok(view_ssh(&k))
}

pub async fn delete_ssh_key(store: &Store, p: &Principal, ip: &str, id: Uuid) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    let Some(name) = tx.delete_ssh_key(p.user_id, id).await? else {
        return Err(Fail::NotFound("no such key".into()));
    };
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("ssh_key.delete", "ssh_key", id, &name).related("owner", p.user_id),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The keys a person signs in to the gateway with, oldest first.
pub async fn login_keys(store: &Store, p: &Principal) -> StoreResult<Vec<View>> {
    Ok(store
        .begin()
        .await?
        .login_keys(p.user_id)
        .await?
        .iter()
        .map(view_login)
        .collect())
}

/// Lets p sign in with a public key in authorized_keys form, named name or
/// else by its own comment.
pub async fn add_login_key(
    store: &Store,
    p: &Principal,
    ip: &str,
    name: &str,
    public: &str,
) -> fail::Result<View> {
    let Some((key, comment)) = parse_authorized(public) else {
        return fail::invalid("not a public key in authorized_keys form (ssh-ed25519 AAAA...)");
    };
    let mut name = name.trim().to_string();
    if name.is_empty() {
        name = comment.trim().to_string();
    }
    if name.is_empty() || name.chars().count() > 100 {
        return fail::invalid("a key needs a name of up to 100 characters");
    }
    let k = LoginKey {
        id: Uuid::new_v4(),
        user_id: p.user_id,
        name: name.clone(),
        public_key: authorized(&key),
        fingerprint: fingerprint(&key),
        created_at: Utc::now(),
    };
    let mut tx = store.begin().await?;
    if let Added::Taken = tx.insert_login_key(&k).await? {
        return fail::invalid("that key is already added");
    }
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("login_key.add", "ssh_key", k.id, &name)
            .related("owner", p.user_id)
            .details(serde_json::json!({ "fingerprint": k.fingerprint })),
    )
    .await?;
    tx.commit().await?;
    Ok(view_login(&k))
}

pub async fn delete_login_key(
    store: &Store,
    p: &Principal,
    ip: &str,
    id: Uuid,
) -> fail::Result<()> {
    let mut tx = store.begin().await?;
    let Some(name) = tx.delete_login_key(p.user_id, id).await? else {
        return Err(Fail::NotFound("no such key".into()));
    };
    audit::record(
        &mut tx,
        &p.actor(ip),
        Event::new("login_key.delete", "ssh_key", id, &name).related("owner", p.user_id),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The enabled people who sign in with a key.
pub async fn login_key_owners(store: &Store, key: &PublicKey) -> StoreResult<Vec<User>> {
    store
        .begin()
        .await?
        .login_key_owners(&fingerprint(key))
        .await
}

/// The SSH gateway's host key: made once and kept, sealed when the server
/// has a secret key, so every server sharing the store presents the same
/// one and clients are not told it changed.
pub async fn host_key(store: &Store, sealer: Option<&Sealer>) -> Result<PrivateKey, String> {
    const NAME: &str = "ssh-gateway-host-key";
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    let mut fresh = PrivateKey::from(russh::keys::ssh_key::private::Ed25519Keypair::from_seed(
        &seed,
    ));
    fresh.set_comment("hangar");
    let pem = fresh
        .to_openssh(LineEnding::LF)
        .map_err(|e| e.to_string())?;
    let value = match sealer {
        Some(s) => s.seal(pem.as_bytes(), NAME),
        None => pem.as_bytes().to_vec(),
    };
    let found = async {
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
    };
    let k = found.await.map_err(|e| e.to_string())?;
    let pem = if k.sealed {
        let Some(s) = sealer else {
            return Err(
                "the SSH gateway's host key is sealed, and the server has no secret key to open it"
                    .into(),
            );
        };
        s.open(&k.value, NAME)?
    } else {
        k.value
    };
    let text = String::from_utf8(pem).map_err(|e| e.to_string())?;
    russh::keys::decode_secret_key(&text, None)
        .map_err(|e| format!("the SSH gateway's host key: {e}"))
}

/// A public key in known_hosts form without the host name.
pub fn host_key_line(key: &PrivateKey) -> String {
    authorized(key.public_key())
}
