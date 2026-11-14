//! A person's keys: SSH keys the server keeps and signs with for them, and
//! the public keys they sign in to the SSH gateway with.

use hangar_suite::{Server, body, status};
use russh::keys::PrivateKey;
use russh::keys::ssh_key::LineEnding;
use russh::keys::ssh_key::private::Ed25519Keypair;
use serde_json::json;

fn key(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

fn client_status<T, E>(r: Result<T, hangar_suite::clientapi::Error<E>>) -> Option<u16> {
    r.err().and_then(|e| e.status()).map(|s| s.as_u16())
}

#[tokio::test]
async fn sign_in_keys_are_added_listed_and_removed() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let c = s.client(&s.token(&admin, "keys").await);
    let k = key(7);
    let public = k.public_key().to_openssh().unwrap();
    let fingerprint = k
        .public_key()
        .fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
        .to_string();

    let added = c
        .add_login_key(&body(json!({ "public_key": format!("{public} laptop") })))
        .await
        .expect("adding a sign-in key")
        .into_inner();
    assert_eq!(added.name, "laptop", "the key's own comment names it");
    assert_eq!(added.fingerprint, fingerprint);
    let listed = c.list_login_keys().await.unwrap().into_inner();
    assert_eq!(listed.len(), 1);
    let profile = admin.get_profile().await.unwrap().into_inner();
    assert_eq!(profile.login_keys.len(), 1);
    assert_eq!(profile.login_keys[0].public_key, public);

    assert_eq!(
        client_status(
            c.add_login_key(&body(json!({ "public_key": public, "name": "again" })))
                .await
        ),
        Some(400),
        "the same key twice"
    );
    assert_eq!(
        client_status(
            c.add_login_key(&body(json!({ "public_key": "not a key" })))
                .await
        ),
        Some(400)
    );
    c.delete_login_key(&added.id).await.expect("removing it");
    assert!(c.list_login_keys().await.unwrap().into_inner().is_empty());
    assert_eq!(
        client_status(c.delete_login_key(&added.id).await),
        Some(404)
    );
}

#[tokio::test]
async fn ssh_keys_are_made_imported_and_removed() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let c = s.client(&s.token(&admin, "keys").await);
    assert!(
        admin.get_profile().await.unwrap().into_inner().secrets,
        "the server keeps no secrets"
    );

    let made = c
        .generate_ssh_key(&body(json!({ "name": "deploy" })))
        .await
        .expect("making a key")
        .into_inner();
    assert!(
        made.public_key.starts_with("ssh-ed25519 AAAA"),
        "{}",
        made.public_key
    );
    assert!(made.public_key.ends_with(" deploy"));
    assert!(made.fingerprint.starts_with("SHA256:"));

    let mine = key(9);
    let private = mine.to_openssh(LineEnding::LF).unwrap().to_string();
    let imported = admin
        .add_ssh_key(&body(json!({ "name": "imported", "private_key": private })))
        .await
        .expect("importing a key")
        .into_inner();
    assert_eq!(
        imported.fingerprint,
        mine.public_key()
            .fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
            .to_string()
    );
    assert_eq!(
        status(
            admin
                .add_ssh_key(&body(json!({ "name": "bad", "private_key": "not a key" })))
                .await
        ),
        Some(400)
    );
    let keys = c.list_ssh_keys().await.unwrap().into_inner();
    assert_eq!(
        keys.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(),
        ["deploy", "imported"]
    );
    c.delete_ssh_key(&made.id).await.expect("removing a key");
    admin
        .delete_ssh_key(&imported.id)
        .await
        .expect("removing the other");
    assert!(c.list_ssh_keys().await.unwrap().into_inner().is_empty());
}
