//! Linked places share a person's records: their theme, sign-in keys,
//! SSH keys and own packs, made, changed or deleted in either place, reach
//! the other, and two packs of one name settle the same way in both. And
//! their copies' files: written, changed or deleted in either place, they
//! reach the other; of two changes made apart the later wins, the other
//! kept in its place's history, from which it is restored.

use hangar_suite::{Server, body, clientapi, eventually, frontendapi, machine};
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

/// The person's own copy of the built-in profile in a place.
async fn profile_copy(p: &Place) -> String {
    let packs = p.admin.list_packs().await.expect("packs").into_inner();
    let profile = packs.iter().find(|k| k.builtin).expect("the profile");
    let copies = p
        .admin
        .list_copies(&profile.id)
        .await
        .expect("the profile's copies")
        .into_inner();
    copies
        .into_iter()
        .find(|c| c.personal)
        .expect("one's own copy")
        .id
}

async fn put(p: &Place, copy: &str, path: &str, content: &str) {
    p.admin
        .put_copy_file(copy, path, &body(json!({ "content": content })))
        .await
        .expect("writing a file");
}

/// A copy's file's content, if it has the file.
async fn content(p: &Place, copy: &str, path: &str) -> Option<String> {
    p.admin
        .get_copy_file(copy, path)
        .await
        .ok()
        .map(|f| f.into_inner().content)
}

#[tokio::test]
async fn a_persons_files_reach_their_linked_place() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    link(&a, &b).await;
    let (ca, cb) = (profile_copy(&a).await, profile_copy(&b).await);

    put(&a, &ca, "~/.gitconfig", "[user]\n\tname = a\n").await;
    eventually("b to take a's file", || async {
        (content(&b, &cb, "~/.gitconfig").await? == "[user]\n\tname = a\n").then_some(())
    })
    .await;
    put(&b, &cb, "~/.gitconfig", "[user]\n\tname = b\n").await;
    eventually("a to take b's change", || async {
        (content(&a, &ca, "~/.gitconfig").await? == "[user]\n\tname = b\n").then_some(())
    })
    .await;
    let big: String = "0123456789abcdef".repeat(100_000);
    put(&a, &ca, "~/.netrc", &big).await;
    eventually("b to take a file of several pieces", || async {
        (content(&b, &cb, "~/.netrc").await? == big).then_some(())
    })
    .await;
    a.admin
        .delete_copy_file(&ca, "~/.gitconfig")
        .await
        .expect("deleting it in a");
    eventually("b to delete it", || async {
        content(&b, &cb, "~/.gitconfig")
            .await
            .is_none()
            .then_some(())
    })
    .await;
    for p in [&a, &b] {
        let copy = if std::ptr::eq(p, &a) { &ca } else { &cb };
        let h = p.admin.list_copy_history(copy).await.expect("history");
        assert!(h.into_inner().is_empty(), "nothing was changed apart");
    }

    // A shared copy reaches b with its files.
    let packs = a.admin.list_packs().await.expect("packs").into_inner();
    let profile = packs.iter().find(|k| k.builtin).expect("the profile");
    let work = a
        .admin
        .create_copy(&profile.id, &body(json!({ "name": "work" })))
        .await
        .expect("a shared copy")
        .into_inner();
    put(&a, &work.id, "~/.npmrc", "registry=https://example.test/\n").await;
    eventually("b to take the shared copy and its file", || async {
        let c = b.admin.get_copy(&work.id).await.ok()?.into_inner();
        (c.copy.name == "work").then_some(())?;
        (content(&b, &work.id, "~/.npmrc").await? == "registry=https://example.test/\n")
            .then_some(())
    })
    .await;
}

/// Two places that had the same file before they were linked: the one
/// changed later wins in both, and the other is kept in its place's
/// history, from which restoring it makes it win in both.
#[tokio::test]
async fn a_file_changed_apart_is_kept_and_restored() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    let (ca, cb) = (profile_copy(&a).await, profile_copy(&b).await);
    put(&a, &ca, "~/.gitconfig", "from a\n").await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    put(&b, &cb, "~/.gitconfig", "from b\n").await;
    link(&a, &b).await;

    eventually("a to take b's later file", || async {
        (content(&a, &ca, "~/.gitconfig").await? == "from b\n").then_some(())
    })
    .await;
    let kept = eventually("a to keep its own in its history", || async {
        let h = a.admin.list_copy_history(&ca).await.ok()?.into_inner();
        h.into_iter().find(|r| r.path == "~/.gitconfig")
    })
    .await;
    assert!(!kept.by_here, "replaced by b's change");
    assert!(kept.by.is_some(), "b named");
    assert_eq!(
        content(&b, &cb, "~/.gitconfig").await.as_deref(),
        Some("from b\n")
    );
    assert!(
        b.admin
            .list_copy_history(&cb)
            .await
            .expect("b's history")
            .into_inner()
            .is_empty(),
        "b's won, so b kept nothing"
    );

    a.admin
        .restore_copy_file(&ca, &kept.id)
        .await
        .expect("restoring it");
    eventually("both to have a's again", || async {
        let (x, y) = (
            content(&a, &ca, "~/.gitconfig").await?,
            content(&b, &cb, "~/.gitconfig").await?,
        );
        (x == "from a\n" && y == "from a\n").then_some(())
    })
    .await;
    let h = a
        .admin
        .list_copy_history(&ca)
        .await
        .expect("a's history")
        .into_inner();
    assert_eq!(h.len(), 1, "what the restore replaced, kept in its stead");
    assert!(h[0].by_here);
}

/// A file an environment writes reaches the linked place as soon as its
/// worker says the copy changed: here written into the files root as the
/// worker's NFS server would, and said as the worker would.
#[tokio::test]
async fn a_file_an_environment_writes_reaches_the_linked_place() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    link(&a, &b).await;
    let w = a.server.worker("w");
    let t = a
        .admin
        .create_template(&body(machine("Linked", "private")))
        .await
        .expect("a template")
        .into_inner();
    let e = a
        .admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "linked" })))
        .await
        .expect("an environment")
        .into_inner();
    eventually("the environment running", || async {
        let x = a.admin.get_environment(&e.id).await.ok()?.into_inner();
        (serde_json::to_value(x.phase).ok()? == json!("running")).then_some(())
    })
    .await;
    let (ca, cb) = (profile_copy(&a).await, profile_copy(&b).await);
    let dir = a.server.files_dir().join(&ca).join("~");
    std::fs::create_dir_all(&dir).expect("the copy's home");
    std::fs::write(dir.join(".gitconfig"), "written in an environment\n").expect("the file");

    let credential = w.credential().expect("the worker's credential");
    let told = reqwest::Client::new()
        .post(format!(
            "{}/api/worker/v1/environments/{}/files/changed",
            a.server.loopback_url(),
            e.id
        ))
        .bearer_auth(credential)
        .header(hangar_api::LEVEL_HEADER, hangar_api::API_LEVEL.to_string())
        .json(&json!({ "sets": [ca] }))
        .send()
        .await
        .expect("telling the server");
    assert_eq!(told.status().as_u16(), 204);
    eventually("b to take the environment's file", || async {
        (content(&b, &cb, "~/.gitconfig").await? == "written in an environment\n").then_some(())
    })
    .await;
}
