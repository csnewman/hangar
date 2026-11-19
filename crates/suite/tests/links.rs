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
    token: String,
}

async fn place() -> Option<Place> {
    let server = Server::start()?;
    let admin = server.admin().await;
    let token = server.token(&admin, "links").await;
    let client = server.client(&token);
    Some(Place {
        server,
        admin,
        client,
        token,
    })
}

/// Links a's admin with b's, as the app does: each makes its half.
async fn link(a: &Place, b: &Place) {
    link_at(a, b, b.server.loopback_url(), a.server.loopback_url()).await
}

/// Links them with the addresses each dials the other at.
async fn link_at(a: &Place, b: &Place, b_at: String, a_at: String) {
    let la = a.client.list_links().await.expect("a's links").into_inner();
    let lb = b.client.list_links().await.expect("b's links").into_inner();
    for (at, other, url) in [(a, &lb, b_at), (b, &la, a_at)] {
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

/// The person's own copy of a pack in a place, made if it is not yet.
async fn own_copy(p: &Place, pack: &str) -> String {
    p.admin
        .list_copies(pack)
        .await
        .expect("the pack's copies")
        .into_inner()
        .into_iter()
        .find(|c| c.personal)
        .expect("one's own copy")
        .id
}

/// A file written again within its path's wait of its last send waits:
/// what reaches the linked place at the end of the wait is the file as it
/// is then, and nothing between.
#[tokio::test]
async fn a_file_written_in_a_burst_is_sent_once_its_wait_ends() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    let pack = a
        .admin
        .create_pack(&body(json!({
            "name": "notes",
            "attach": "owner",
            "paths": [{ "path": "~/notes/", "sensitive": false, "send_every": 3 }],
        })))
        .await
        .expect("a pack")
        .into_inner();
    link(&a, &b).await;
    let rule = eventually("b to hold the pack and its path's wait", || async {
        let k = b.admin.get_pack(&pack.id).await.ok()?.into_inner();
        k.paths.first()?.send_every
    })
    .await;
    assert_eq!(rule, 3);
    let (ca, cb) = (own_copy(&a, &pack.id).await, own_copy(&b, &pack.id).await);
    let (a, b) = (&a, &b);

    put(a, &ca, "~/notes/todo", "one\n").await;
    eventually("b to take the first write", || async {
        (content(b, &cb, "~/notes/todo").await? == "one\n").then_some(())
    })
    .await;
    put(a, &ca, "~/notes/todo", "two\n").await;
    put(a, &ca, "~/notes/todo", "three\n").await;
    let written = std::time::Instant::now();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    assert_eq!(
        content(b, &cb, "~/notes/todo").await.as_deref(),
        Some("one\n"),
        "the later writes wait"
    );
    eventually("b to take the last write once the wait ends", || async {
        (content(b, &cb, "~/notes/todo").await? == "three\n").then_some(())
    })
    .await;
    assert!(
        written.elapsed() > std::time::Duration::from_secs(1),
        "sent at the end of the wait"
    );
}

/// A bandwidth limit set in one place reaches the other, and holds a
/// file's pieces to it: the second piece waits for the first's time.
#[tokio::test]
async fn a_bandwidth_limit_reaches_every_place_and_paces_files() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    link(&a, &b).await;
    a.client
        .set_link_settings(&clientapi::types::LinkSettings { bandwidth: 400_000 })
        .await
        .expect("a limit");
    eventually("b to hold the limit", || async {
        let l = b.client.list_links().await.ok()?.into_inner();
        (l.bandwidth == Some(400_000)).then_some(())
    })
    .await;
    let (ca, cb) = (profile_copy(&a).await, profile_copy(&b).await);
    let big: String = "0123456789abcdef".repeat(100_000);
    let started = std::time::Instant::now();
    put(&a, &ca, "~/.netrc", &big).await;
    eventually("b to take the file", || async {
        (content(&b, &cb, "~/.netrc").await? == big).then_some(())
    })
    .await;
    assert!(
        started.elapsed() > std::time::Duration::from_secs(2),
        "a MiB at 400 kB a second takes over two seconds"
    );

    b.client
        .set_link_settings(&clientapi::types::LinkSettings { bandwidth: 0 })
        .await
        .expect("no limit");
    eventually("a to drop the limit", || async {
        let l = a.client.list_links().await.ok()?.into_inner();
        l.bandwidth.is_none().then_some(())
    })
    .await;
}

/// Text that looks random but is the same every run.
fn text(n: usize, mut x: u64) -> String {
    (0..n)
        .map(|i| {
            if i % 80 == 79 {
                return '\n';
            }
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + (x % 26) as u8)
        })
        .collect()
}

/// A large file changed a little goes as what changed: under a limit
/// that holds the whole file to seconds, an append and an edit each
/// arrive in well under one.
#[tokio::test]
async fn a_large_file_changed_a_little_goes_as_what_changed() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    link(&a, &b).await;
    a.client
        .set_link_settings(&clientapi::types::LinkSettings { bandwidth: 300_000 })
        .await
        .expect("a limit");
    let (ca, cb) = (profile_copy(&a).await, profile_copy(&b).await);
    let mut data = text(1_500_000, 9);
    let arrives = |data: String| {
        let (b, cb) = (&b, &cb);
        async move {
            let started = std::time::Instant::now();
            eventually("b to take the file", || async {
                (content(b, cb, "~/.netrc").await? == data).then_some(())
            })
            .await;
            started.elapsed()
        }
    };

    put(&a, &ca, "~/.netrc", &data).await;
    let whole = arrives(data.clone()).await;
    assert!(
        whole > std::time::Duration::from_secs(2),
        "whole: {whole:?}"
    );

    // Each change waits for the limit's last piece to have had its time.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    data.push_str("one more line\n");
    put(&a, &ca, "~/.netrc", &data).await;
    let appended = arrives(data.clone()).await;
    assert!(
        appended < std::time::Duration::from_millis(1500),
        "appended: {appended:?}"
    );

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    data.replace_range(700_000..700_010, "edited in the middle");
    put(&a, &ca, "~/.netrc", &data).await;
    let edited = arrives(data.clone()).await;
    assert!(
        edited < std::time::Duration::from_millis(1500),
        "edited: {edited:?}"
    );
}

/// Bytes that look random but are the same every run.
fn noise(n: usize, mut x: u64) -> Vec<u8> {
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Where a copy's file is in a place's files root.
fn on_disk(p: &Place, copy: &str, path: &str) -> std::path::PathBuf {
    p.server.files_dir().join(copy).join(path)
}

/// Writes a file into a copy as an environment would, larger than the
/// Files page writes: the places compare it once they connect.
fn write_on_disk(p: &Place, copy: &str, path: &str, data: &[u8]) {
    let at = on_disk(p, copy, path);
    std::fs::create_dir_all(at.parent().expect("a directory")).expect("the directory");
    std::fs::write(&at, data).expect("the file");
}

/// Waits up to secs for f, polling.
async fn within<T, F, Fut>(secs: u64, what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = std::time::Instant::now();
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Whether the file at path is data, its size looked at first.
fn holds(path: &std::path::Path, data: &[u8]) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() == data.len() as u64)
        && std::fs::read(path).is_ok_and(|got| got == data)
}

/// A file larger than places once held in memory whole, 64 MiB, goes a
/// piece at a time, read from the file and written as it comes.
#[tokio::test]
async fn a_file_over_64_mib_reaches_the_linked_place() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    let (ca, cb) = (profile_copy(&a).await, profile_copy(&b).await);
    let data = noise(80 << 20, 11);
    write_on_disk(&a, &ca, "~/.claude/big.bin", &data);
    link(&a, &b).await;
    let there = on_disk(&b, &cb, "~/.claude/big.bin");
    within(60, "b to take the large file", || async {
        holds(&there, &data).then_some(())
    })
    .await;
}

/// A file that stops coming part way, its sender stopped, comes on from
/// where it stopped once the sender is back: the receiving place's file
/// coming in grows on, and no other is started.
#[tokio::test]
async fn a_file_that_stopped_coming_comes_on_from_where_it_stopped() {
    let (Some(mut a), Some(b)) = (place().await, place().await) else {
        return;
    };
    a.client
        .set_link_settings(&clientapi::types::LinkSettings {
            bandwidth: 1_500_000,
        })
        .await
        .expect("a limit");
    let (ca, cb) = (profile_copy(&a).await, profile_copy(&b).await);
    let data = noise(20 << 20, 12);
    write_on_disk(&a, &ca, "~/.claude/big.bin", &data);
    link(&a, &b).await;
    let incoming = b.server.files_dir().join(".links/incoming");
    let coming = || {
        std::fs::read_dir(&incoming)
            .map(|d| {
                d.flatten()
                    .filter_map(|e| Some((e.file_name(), e.metadata().ok()?.len())))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let (name, had) = within(30, "the file to start coming", || async {
        coming().into_iter().find(|(_, len)| *len >= 3 << 20)
    })
    .await;
    a.server.restart_with(&[]);
    let there = on_disk(&b, &cb, "~/.claude/big.bin");
    let grew = std::cell::Cell::new(false);
    within(60, "b to take the rest of the file", || async {
        for (n, len) in coming() {
            assert_eq!(n, name, "the file started coming again from its start");
            grew.set(grew.get() || len > had);
        }
        holds(&there, &data).then_some(())
    })
    .await;
    assert!(
        grew.get(),
        "what came before the stop was kept and added to"
    );
}

/// The copy a person uses of a pack by default follows them: chosen in
/// one place, it is chosen in the other; put back to their own there, it
/// is put back here.
#[tokio::test]
async fn a_default_copy_follows_the_person() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    link(&a, &b).await;
    let profile = |p: &Place| {
        let admin = p.admin.clone();
        async move {
            admin
                .list_packs()
                .await
                .expect("packs")
                .into_inner()
                .into_iter()
                .find(|k| k.builtin)
                .expect("the profile")
        }
    };
    let pa = profile(&a).await;
    let work = a
        .admin
        .create_copy(&pa.id, &body(json!({ "name": "work" })))
        .await
        .expect("a shared copy")
        .into_inner();
    a.admin
        .set_default_copy(&pa.id, &body(json!({ "copy_id": work.id })))
        .await
        .expect("choosing it");
    eventually("b to use the shared copy by default", || async {
        (profile(&b).await.default_copy_id.as_deref() == Some(work.id.as_str())).then_some(())
    })
    .await;
    let pb = profile(&b).await;
    b.admin
        .set_default_copy(&pb.id, &body(json!({})))
        .await
        .expect("putting it back");
    eventually("a to use its own again", || async {
        profile(&a).await.default_copy_id.is_none().then_some(())
    })
    .await;
}

/// Two packs of one name made apart, merged in one place: one pack in
/// both, with both's paths and files; of a file both had, the one changed
/// later, the other kept in the history of the place that merged.
#[tokio::test]
async fn packs_of_one_name_merge_in_every_place() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    let mut made = vec![];
    for (p, dir, file) in [(&a, "~/notes/", "~/notes/a"), (&b, "~/docs/", "~/docs/b")] {
        let k = p
            .admin
            .create_pack(&body(json!({
                "name": "notes",
                "attach": "owner",
                "paths": [
                    { "path": dir, "sensitive": false },
                    { "path": "~/both", "sensitive": false },
                ],
            })))
            .await
            .expect("a pack")
            .into_inner();
        let c = own_copy(p, &k.id).await;
        put(p, &c, file, "its own\n").await;
        put(p, &c, "~/both", &format!("from {file}\n")).await;
        made.push(k.id);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    link(&a, &b).await;
    let renamed = eventually("a to hold both, one marked as named alike", || async {
        let packs = a.admin.list_packs().await.ok()?.into_inner();
        let r = packs.iter().find(|k| k.same_name.is_some())?.clone();
        (packs.iter().filter(|k| !k.builtin).count() == 2).then_some(r)
    })
    .await;
    let kept = renamed.same_name.clone().expect("the other");
    let kept_name = a.admin.get_pack(&kept).await.expect("it").into_inner().name;
    assert_eq!(kept_name, "notes");
    // Both copies' files reach a before it merges.
    let (ra, ka) = (own_copy(&a, &renamed.id).await, own_copy(&a, &kept).await);
    eventually("a to hold both packs' files", || async {
        content(&a, &ra, "~/both").await?;
        content(&a, &ka, "~/both").await.map(|_| ())
    })
    .await;

    let merged = a
        .admin
        .merge_pack(&renamed.id)
        .await
        .expect("merging")
        .into_inner();
    assert_eq!(merged.id, kept);
    for p in [&a, &b] {
        let pack = eventually("one pack with both's paths and files", || async {
            let packs = p.admin.list_packs().await.ok()?.into_inner();
            let ours: Vec<_> = packs.into_iter().filter(|k| !k.builtin).collect();
            (ours.len() == 1 && ours[0].id == kept && ours[0].paths.len() == 3).then_some(())?;
            let c = own_copy(p, &kept).await;
            content(p, &c, "~/notes/a").await?;
            content(p, &c, "~/docs/b").await?;
            (content(p, &c, "~/both").await? == "from ~/docs/b\n").then_some(c)
        })
        .await;
        let _ = pack;
    }
    let c = own_copy(&a, &kept).await;
    let h = a
        .admin
        .list_copy_history(&c)
        .await
        .expect("history")
        .into_inner();
    assert!(
        h.iter()
            .any(|r| r.path == "~/both" && r.merged == Some(true)),
        "the earlier kept: {h:?}"
    );
}

/// A pack kept apart from one of its name is offered for merging in
/// neither place.
#[tokio::test]
async fn a_pack_kept_apart_is_kept_apart_everywhere() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    for p in [&a, &b] {
        p.admin
            .create_pack(&body(json!({ "name": "tools", "paths": [] })))
            .await
            .expect("a pack");
    }
    link(&a, &b).await;
    let renamed = eventually("b to mark one", || async {
        let packs = b.admin.list_packs().await.ok()?.into_inner();
        packs.into_iter().find(|k| k.same_name.is_some())
    })
    .await;
    b.admin
        .keep_pack_apart(&renamed.id)
        .await
        .expect("keeping it apart");
    eventually("a to mark neither", || async {
        let packs = a.admin.list_packs().await.ok()?.into_inner();
        let ours: Vec<_> = packs.into_iter().filter(|k| !k.builtin).collect();
        (ours.len() == 2 && ours.iter().all(|k| k.same_name.is_none())).then_some(())
    })
    .await;
}

/// The client API, as the app reads it: the packs, a pack's copies and a
/// copy's history, and a path's send rule changed.
#[tokio::test]
async fn the_client_api_reads_and_changes_packs() {
    let Some(a) = place().await else {
        return;
    };
    let made = a
        .admin
        .create_pack(&body(
            json!({ "name": "notes", "paths": [{ "path": "~/notes/", "sensitive": false }] }),
        ))
        .await
        .expect("a pack")
        .into_inner();
    let packs = a.client.list_packs().await.expect("packs").into_inner();
    assert!(packs.items.iter().any(|k| k.builtin));
    let k = packs
        .items
        .iter()
        .find(|k| k.id == made.id)
        .expect("the pack");
    let copies = a
        .client
        .list_copies(&k.id)
        .await
        .expect("copies")
        .into_inner();
    let own = copies.items.iter().find(|c| c.personal).expect("one's own");
    let h = a
        .client
        .list_copy_history(&own.id)
        .await
        .expect("history")
        .into_inner();
    assert!(h.items.is_empty());
    let changed = a
        .client
        .update_pack(
            &k.id,
            &body(json!({
                "name": k.name,
                "paths": [{ "path": "~/notes/", "sensitive": false, "send_every": 0 }],
            })),
        )
        .await
        .expect("changing its path")
        .into_inner();
    assert_eq!(changed.paths[0].send_every, Some(0));
}

/// A person's own template follows them: made in one place it is in the
/// other with its ID, the profile it lists being that place's own; changed
/// there it changes here; deleted, it goes from both.
#[tokio::test]
async fn a_persons_template_follows_them() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    link(&a, &b).await;
    let profile = |p: &Place| {
        let admin = p.admin.clone();
        async move {
            admin
                .list_packs()
                .await
                .expect("packs")
                .into_inner()
                .into_iter()
                .find(|k| k.builtin)
                .expect("the profile")
                .id
        }
    };
    let (pa, pb) = (profile(&a).await, profile(&b).await);
    let mut t = hangar_suite::machine("Mine", "private");
    t["spec"]["packs"] = json!([{ "pack": pa }]);
    t["spec"]["access"] = json!({ "no_registry": true });
    let made = a
        .admin
        .create_template(&body(t))
        .await
        .expect("a template")
        .into_inner();
    let there = eventually("b to hold the template", || async {
        b.admin
            .get_template(&made.id)
            .await
            .ok()
            .map(|t| t.into_inner())
    })
    .await;
    assert_eq!(there.name, "Mine");
    assert_eq!(there.spec.packs[0].pack, pb, "b's own profile");
    assert_eq!(there.spec.access.and_then(|x| x.no_registry), Some(true));

    let mut changed = hangar_suite::machine("Mine", "private");
    changed["description"] = json!("changed in b");
    changed["spec"]["packs"] = json!([{ "pack": pb }]);
    b.admin
        .update_template(&made.id, &body(changed))
        .await
        .expect("changing it in b");
    eventually("a to take the change", || async {
        let t = a.admin.get_template(&made.id).await.ok()?.into_inner();
        (t.description == "changed in b" && t.spec.packs.first()?.pack == pa).then_some(())
    })
    .await;

    a.admin
        .delete_template(&made.id)
        .await
        .expect("deleting it");
    eventually("b to delete it", || async {
        b.admin.get_template(&made.id).await.is_err().then_some(())
    })
    .await;
}

/// A pack can be kept from a linked place: left out, the place hides it
/// and loses its files, and later changes do not reach it; put back, it
/// has both again.
#[tokio::test]
async fn a_pack_is_kept_from_a_place_it_is_not_for() {
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    let pack = a
        .admin
        .create_pack(&body(json!({
            "name": "secrets",
            "attach": "owner",
            "paths": [{ "path": "~/.secrets", "sensitive": true }],
        })))
        .await
        .expect("a pack")
        .into_inner();
    link(&a, &b).await;
    let ca = own_copy(&a, &pack.id).await;
    put(&a, &ca, "~/.secrets", "one\n").await;
    let cb = eventually("b to take the pack and its file", || async {
        let cb = b
            .admin
            .list_copies(&pack.id)
            .await
            .ok()?
            .into_inner()
            .into_iter()
            .find(|c| c.personal)?
            .id;
        (content(&b, &cb, "~/.secrets").await? == "one\n").then_some(cb)
    })
    .await;
    let here = a
        .client
        .list_links()
        .await
        .expect("a's links")
        .into_inner()
        .place
        .id;

    a.admin
        .set_pack_places(&pack.id, &body(json!({ "places": [here] })))
        .await
        .expect("keeping it from b");
    eventually("b to hide the pack and drop its files", || async {
        let gone = b.admin.get_pack(&pack.id).await.is_err();
        let files = b.server.files_dir().join(&cb);
        (gone && !files.exists()).then_some(())
    })
    .await;
    put(&a, &ca, "~/.secrets", "two\n").await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(
        !b.server.files_dir().join(&cb).exists(),
        "a change made while b is left out stays away from it"
    );

    a.admin
        .set_pack_places(&pack.id, &body(json!({ "places": [] })))
        .await
        .expect("putting b back");
    eventually("b to hold the pack and the file again", || async {
        b.admin.get_pack(&pack.id).await.ok()?;
        (content(&b, &cb, "~/.secrets").await? == "two\n").then_some(())
    })
    .await;
}

/// Two linked places neither of which reaches the other are carried
/// through a client that reaches both, as the app relays them: one
/// place's relay socket piped to the other's link endpoint carries a pack
/// made in one, and its file, to the other.
#[tokio::test]
async fn a_relayed_link_carries_what_neither_place_can() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let (Some(a), Some(b)) = (place().await, place().await) else {
        return;
    };
    // Each dials the other at an address that reaches nothing.
    link_at(
        &a,
        &b,
        "http://127.0.0.1:9".into(),
        "http://127.0.0.1:9".into(),
    )
    .await;
    let lb = b.client.list_links().await.expect("b's links").into_inner();
    let mut req = format!(
        "ws://127.0.0.1:{}/api/v1/me/links/{}/relay",
        a.server.port(),
        lb.place.id
    )
    .into_client_request()
    .expect("a request");
    req.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", a.token).parse().expect("a header"),
    );
    let (from, _) = tokio_tungstenite::connect_async(req)
        .await
        .expect("a's relay socket");
    let (to, _) = tokio_tungstenite::connect_async(format!(
        "ws://127.0.0.1:{}/api/link/v1/connect",
        b.server.port()
    ))
    .await
    .expect("b's link endpoint");
    let (mut fw, mut fr) = from.split();
    let (mut tw, mut tr) = to.split();
    let pipe = tokio::spawn(async move {
        loop {
            tokio::select! {
                m = fr.next() => match m {
                    Some(Ok(m @ (Message::Text(_) | Message::Binary(_)))) => { if tw.send(m).await.is_err() { break } }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                },
                m = tr.next() => match m {
                    Some(Ok(m @ (Message::Text(_) | Message::Binary(_)))) => { if fw.send(m).await.is_err() { break } }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                },
            }
        }
    });

    let pack = a
        .admin
        .create_pack(&body(json!({
            "name": "carried",
            "attach": "owner",
            "paths": [{ "path": "~/.carried", "sensitive": false }],
        })))
        .await
        .expect("a pack")
        .into_inner();
    let ca = own_copy(&a, &pack.id).await;
    put(&a, &ca, "~/.carried", "over the relay\n").await;
    eventually("b to take the pack and its file over the relay", || async {
        b.admin.get_pack(&pack.id).await.ok()?;
        let cb = b
            .admin
            .list_copies(&pack.id)
            .await
            .ok()?
            .into_inner()
            .into_iter()
            .find(|c| c.personal)?
            .id;
        (content(&b, &cb, "~/.carried").await? == "over the relay\n").then_some(())
    })
    .await;
    pipe.abort();
}
