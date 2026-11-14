//! Packs and their copies: a user's own files, written and read back, kept
//! from other users.

use hangar_suite::{Server, body, status};
use serde_json::json;

#[tokio::test]
async fn a_users_copy_keeps_their_files() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let alice = s.user(&admin, "alice").await;
    let pack = bob
        .create_pack(&body(json!({
            "name": "dotfiles",
            "paths": [
                { "path": "~/.gitconfig", "sensitive": false },
                { "path": "~/.ssh/config", "sensitive": true },
            ],
        })))
        .await
        .expect("making a pack")
        .into_inner();
    let copy = bob
        .create_copy(&pack.id, &body(json!({ "name": "main" })))
        .await
        .expect("making a copy")
        .into_inner();

    let git = "[user]\n\tname = Bob\n";
    bob.put_copy_file(&copy.id, "~/.gitconfig", &body(json!({ "content": git })))
        .await
        .expect("writing a file");
    let read = bob
        .get_copy_file(&copy.id, "~/.gitconfig")
        .await
        .expect("reading it back")
        .into_inner();
    assert_eq!(read.content, git);
    assert_eq!((read.mode, read.sensitive), (0o644, false));

    bob.put_copy_file(
        &copy.id,
        "~/.ssh/config",
        &body(json!({ "content": "Host *\n" })),
    )
    .await
    .expect("writing a sensitive file");
    let secret = bob
        .get_copy_file(&copy.id, "~/.ssh/config")
        .await
        .unwrap()
        .into_inner();
    assert_eq!((secret.mode, secret.sensitive), (0o600, true));

    let theirs = alice.list_packs().await.unwrap().into_inner();
    assert!(
        theirs.iter().all(|p| p.id != pack.id),
        "alice sees bob's pack"
    );
    assert!(
        matches!(
            status(alice.get_copy_file(&copy.id, "~/.gitconfig").await),
            Some(403 | 404)
        ),
        "alice read bob's file"
    );
    assert!(matches!(
        status(
            alice
                .put_copy_file(&copy.id, "~/.gitconfig", &body(json!({ "content": "x" })))
                .await
        ),
        Some(403 | 404)
    ));
}
