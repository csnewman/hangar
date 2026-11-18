//! Linked places share a person's records: their theme, sign-in keys,
//! SSH keys and own packs, made, changed or deleted in either place, reach
//! the other, and two packs of one name settle the same way in both.

use hangar_suite::{Server, body, clientapi, eventually, frontendapi};
use serde_json::json;

const LOGIN_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIH3jAZFwU1PFjSugz/PCQ+VFAeC/xMMFlB+AXKaIw0Un";

struct Place {
    server: Server,
    admin: frontendapi::Client,
    client: clientapi::Client,
}

async fn place() -> Option<Place> {
    let server = Server::start()?;
    let admin = server.admin().await;
    let client = server.client(&server.token(&admin, "links").await);
    Some(Place {
        server,
        admin,
        client,
    })
}

/// Links a's admin with b's, as the app does: each makes its half.
async fn link(a: &Place, b: &Place) {
    let la = a.client.list_links().await.expect("a's links").into_inner();
    let lb = b.client.list_links().await.expect("b's links").into_inner();
    for (at, other, url) in [
        (a, &lb, b.server.loopback_url()),
        (b, &la, a.server.loopback_url()),
    ] {
        at.client
            .put_link(
                &other.place.id,
                &body(json!({
                    "public_key": other.place.public_key,
                    "url": url,
                    "person": la.person,
                })),
            )
            .await
            .expect("linking");
    }
}

async fn pack_names(p: &Place) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = p
        .admin
        .list_packs()
        .await
        .expect("packs")
        .into_inner()
        .into_iter()
        .filter(|k| !k.builtin)
        .map(|k| (k.id, k.name))
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn a_persons_records_reach_their_linked_place() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    link(&a, &b).await;

    a.admin
        .set_preferences(&frontendapi::types::Preferences {
            theme: frontendapi::types::Theme::Dark,
        })
        .await
        .expect("a theme");
    let login = a
        .client
        .add_login_key(&body(
            json!({ "public_key": format!("{LOGIN_KEY} laptop") }),
        ))
        .await
        .expect("a sign-in key")
        .into_inner();
    let ssh = a
        .client
        .generate_ssh_key(&body(json!({ "name": "github" })))
        .await
        .expect("an SSH key")
        .into_inner();
    let pack = a
        .admin
        .create_pack(&body(json!({
            "name": "dotfiles",
            "attach": "owner",
            "paths": [
                { "path": "~/.gitconfig", "sensitive": false },
                { "path": "~/.ssh/config", "sensitive": true },
            ],
        })))
        .await
        .expect("a pack")
        .into_inner();

    eventually("b to take the theme", || async {
        let me = b.admin.get_me().await.ok()?.into_inner();
        (me.theme == frontendapi::types::Theme::Dark).then_some(())
    })
    .await;
    let theirs = eventually("b to take the sign-in key", || async {
        let keys = b.client.list_login_keys().await.ok()?.into_inner().items;
        keys.into_iter()
            .find(|k| k.fingerprint == login.fingerprint)
    })
    .await;
    assert_eq!(theirs.name, "laptop");
    eventually("b to take the SSH key", || async {
        let keys = b.client.list_ssh_keys().await.ok()?.into_inner().items;
        keys.into_iter().find(|k| k.public_key == ssh.public_key)
    })
    .await;
    let theirs = eventually("b to take the pack", || async {
        b.admin
            .get_pack(&pack.id)
            .await
            .ok()
            .map(|p| p.into_inner())
    })
    .await;
    assert_eq!(theirs.name, "dotfiles");
    let mut paths: Vec<(String, bool)> = theirs
        .paths
        .into_iter()
        .map(|p| (p.path, p.sensitive))
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            ("~/.gitconfig".to_string(), false),
            ("~/.ssh/config".to_string(), true),
        ]
    );

    // Changed in b, the change reaches a; deleted in a, the deletion
    // reaches b.
    b.admin
        .update_pack(
            &pack.id,
            &body(json!({
                "name": "dotfiles",
                "description": "from b",
                "attach": "owner",
                "paths": [
                    { "path": "~/.gitconfig", "sensitive": false },
                    { "path": "~/.ssh/config", "sensitive": true },
                ],
            })),
        )
        .await
        .expect("changing the pack in b");
    eventually("a to take b's change", || async {
        let p = a.admin.get_pack(&pack.id).await.ok()?.into_inner();
        (p.description == "from b").then_some(())
    })
    .await;
    a.admin.delete_pack(&pack.id).await.expect("deleting in a");
    a.client
        .delete_login_key(&login.id)
        .await
        .expect("removing the sign-in key in a");
    eventually("b to delete the pack and the sign-in key", || async {
        let gone = b.admin.get_pack(&pack.id).await.is_err();
        let keys = b.client.list_login_keys().await.ok()?.into_inner().items;
        (gone && keys.is_empty()).then_some(())
    })
    .await;
}

#[tokio::test]
async fn packs_of_one_name_settle_the_same_in_both() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    for p in [&a, &b] {
        p.admin
            .create_pack(&body(json!({ "name": "notes", "paths": [] })))
            .await
            .expect("a pack");
    }
    link(&a, &b).await;
    let settled = eventually("both to hold both packs, named alike", || async {
        let (na, nb) = (pack_names(&a).await, pack_names(&b).await);
        (na.len() == 2 && na == nb).then_some(na)
    })
    .await;
    let mut names: Vec<&str> = settled.iter().map(|(_, n)| n.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["notes", "notes (2)"]);
}

/// A place that cannot reach the other is heard as soon as it changes all
/// the same: the place that reaches it holds a connection open, over which
/// it sends its changes.
#[tokio::test]
async fn a_place_that_cannot_dial_out_is_heard_at_once() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    let la = a.client.list_links().await.expect("a's links").into_inner();
    let lb = b.client.list_links().await.expect("b's links").into_inner();
    a.client
        .put_link(
            &lb.place.id,
            &body(json!({
                "public_key": lb.place.public_key,
                "url": b.server.loopback_url(),
                "person": la.person,
            })),
        )
        .await
        .expect("a links to b");
    // b has a at an address that answers nothing.
    b.client
        .put_link(
            &la.place.id,
            &body(json!({
                "public_key": la.place.public_key,
                "url": "http://127.0.0.1:9",
                "person": la.person,
            })),
        )
        .await
        .expect("b links to a");
    eventually("a to reach b", || async {
        let l = a.client.list_links().await.ok()?.into_inner();
        l.items.into_iter().next()?.synced_at.map(|_| ())
    })
    .await;

    let start = std::time::Instant::now();
    let key = b
        .client
        .add_login_key(&body(
            json!({ "public_key": format!("{LOGIN_KEY} from-b") }),
        ))
        .await
        .expect("a sign-in key in b")
        .into_inner();
    eventually("a to hear of b's change", || async {
        let keys = a.client.list_login_keys().await.ok()?.into_inner().items;
        keys.into_iter().find(|k| k.fingerprint == key.fingerprint)
    })
    .await;
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "heard in {:?}, not at a's next round a minute on",
        start.elapsed()
    );
    let b_link = b.client.list_links().await.expect("b's links").into_inner();
    assert!(
        b_link.items[0].reached_at.is_none() && b_link.items[0].heard_at.is_some(),
        "b never reached a itself; a's connection is the one"
    );
}
