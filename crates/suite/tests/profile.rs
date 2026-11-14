//! The profile: the keys that follow someone into their environments.

use hangar_suite::{Server, body};
use serde_json::json;

const LOGIN_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIH3jAZFwU1PFjSugz/PCQ+VFAeC/xMMFlB+AXKaIw0Un suite-login-key";

#[tokio::test]
async fn sign_in_and_ssh_keys_are_kept() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let p = bob.get_profile().await.expect("the profile").into_inner();
    assert!(
        p.secrets,
        "the server keeps no SSH keys though it has a key"
    );

    let login = bob
        .add_login_key(&body(json!({ "public_key": LOGIN_KEY })))
        .await
        .expect("adding a sign-in key")
        .into_inner();
    assert_eq!(login.name, "suite-login-key");
    let key = bob
        .add_ssh_key(&body(json!({ "name": "laptop" })))
        .await
        .expect("generating an SSH key")
        .into_inner();
    let p = bob.get_profile().await.unwrap().into_inner();
    assert!(p.login_keys.iter().any(|k| k.id == login.id));
    assert!(p.keys.iter().any(|k| k.id == key.id));
    let shown = serde_json::to_string(&p).unwrap();
    assert!(!shown.contains("PRIVATE KEY"), "a private key is shown");

    bob.delete_login_key(&login.id).await.expect("removing it");
    bob.delete_ssh_key(&key.id).await.expect("removing it");
    let p = bob.get_profile().await.unwrap().into_inner();
    assert!(p.login_keys.is_empty() && p.keys.is_empty());
}
