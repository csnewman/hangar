//! Packs and their copies: what a pack may name, who sees and changes it,
//! a copy's files, the copy an environment uses, and a conflict its guest
//! finds, resolved by its owner.

use hangar_suite::worker::CONFLICTED;
use hangar_suite::{Server, body, eventually, machine, status};
use serde_json::json;

fn pack(name: &str, paths: serde_json::Value) -> serde_json::Value {
    json!({ "name": name, "paths": paths })
}

#[tokio::test]
async fn a_pack_names_paths_it_may() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    for bad in [
        json!([{ "path": "relative/path", "sensitive": false }]),
        json!([{ "path": "/proc/self", "sensitive": false }]),
        json!([{ "path": "!~/.config/x/", "sensitive": false }]),
        json!([{ "path": "~/../escape", "sensitive": false }]),
    ] {
        assert_eq!(
            status(admin.create_pack(&body(pack("Bad", bad.clone()))).await),
            Some(400),
            "a pack of {bad}"
        );
    }
    let k = admin
        .create_pack(&body(pack(
            "Tools",
            json!([
                { "path": "~/.config/tool/", "sensitive": false },
                { "path": "!~/.config/tool/cache/", "sensitive": true },
                { "path": "~/.config/tool/token", "sensitive": true },
            ]),
        )))
        .await
        .expect("making a pack")
        .into_inner();
    assert_eq!(k.owner.as_deref(), Some("admin"));
    let paths: Vec<_> = k
        .paths
        .iter()
        .map(|p| (p.path.as_str(), p.sensitive))
        .collect();
    assert_eq!(
        paths,
        [
            ("~/.config/tool/", false),
            ("!~/.config/tool/cache/", false),
            ("~/.config/tool/token", true),
        ],
        "kept in order, an exclusion never sensitive"
    );
    assert_eq!(
        status(admin.create_pack(&body(pack("tools", json!([])))).await),
        Some(400),
        "a second pack of the same name"
    );
    let listed = admin.list_packs().await.unwrap().into_inner();
    assert!(listed[0].builtin, "the built-in profile first");
    assert!(listed.iter().any(|p| p.id == k.id));
}

#[tokio::test]
async fn a_copys_files_are_kept_and_changed() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let k = admin
        .create_pack(&body(pack(
            "Dotfiles",
            json!([{ "path": "~/.dot/", "sensitive": false }, { "path": "~/.secret", "sensitive": true }]),
        )))
        .await
        .unwrap()
        .into_inner();
    let copies = admin.list_copies(&k.id).await.unwrap().into_inner();
    assert_eq!(copies.len(), 1, "a person's own copy, made when asked for");
    assert!(copies[0].personal && copies[0].can_write);
    let mine = &copies[0].id;

    let put = |path: &'static str, content: &'static str| {
        let admin = &admin;
        async move {
            admin
                .put_copy_file(mine, path, &body(json!({ "content": content })))
                .await
        }
    };
    let f = put("~/.dot/rc", "set -o vi\n")
        .await
        .expect("writing a file")
        .into_inner();
    assert_eq!(
        (f.size, f.mode, f.shared, f.sensitive),
        (10, 0o644, true, false)
    );
    let secret = put("~/.secret", "hunter2").await.unwrap().into_inner();
    assert_eq!((secret.mode, secret.sensitive), (0o600, true));
    assert_eq!(
        status(put("~/.elsewhere", "x").await),
        Some(400),
        "a file the pack does not share"
    );

    let read = admin
        .get_copy_file(mine, "~/.dot/rc")
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read.content, "set -o vi\n");
    admin
        .set_copy_file_mode(mine, "~/.dot/rc", &body(json!({ "mode": 0o600 })))
        .await
        .expect("changing its mode");
    let listing = admin.get_copy(mine).await.unwrap().into_inner();
    assert_eq!(
        listing
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.mode))
            .collect::<Vec<_>>(),
        [("~/.dot/rc", 0o600), ("~/.secret", 0o600)]
    );
    admin
        .delete_copy_file(mine, "~/.dot/rc")
        .await
        .expect("deleting it");
    assert_eq!(
        status(admin.get_copy_file(mine, "~/.dot/rc").await),
        Some(404)
    );

    // Another person neither sees the pack nor reaches the copy.
    assert_eq!(status(bob.get_pack(&k.id).await), Some(404));
    assert_eq!(status(bob.get_copy(mine).await), Some(404));

    admin.delete_pack(&k.id).await.expect("deleting the pack");
    assert_eq!(status(admin.get_copy(mine).await), Some(404));
}

#[tokio::test]
async fn a_teams_pack_goes_by_role() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let carol = s.user(&admin, "carol").await;
    let team = admin
        .create_team(&body(json!({ "name": "Core", "slug": "core" })))
        .await
        .unwrap()
        .into_inner();
    let bob_id = bob.get_me().await.unwrap().into_inner().id;
    admin
        .set_team_member(&team.id, &bob_id, &body(json!({ "role": "viewer" })))
        .await
        .unwrap();
    let mut input = pack(
        "Core tools",
        json!([{ "path": "~/.core/", "sensitive": false }]),
    );
    input["team_id"] = json!(team.id);
    input["attach"] = json!("team");
    let k = admin
        .create_pack(&body(input.clone()))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(k.team.as_deref(), Some("core"));
    let as_bob = bob
        .get_pack(&k.id)
        .await
        .expect("a member sees the team's pack")
        .into_inner();
    assert!(!as_bob.can_change, "a viewer may change it");
    assert_eq!(
        status(bob.update_pack(&k.id, &body(input)).await),
        Some(403)
    );
    assert_eq!(status(carol.get_pack(&k.id).await), Some(404));
    let mut everyone = pack("Everyone's", json!([]));
    everyone["attach"] = json!("everyone");
    assert_eq!(status(bob.create_pack(&body(everyone)).await), Some(403));
}

#[tokio::test]
async fn an_environment_uses_a_copy_and_its_conflicts_are_resolved() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let w = s.worker("w");
    let t = admin
        .create_template(&body(machine("Packs", "private")))
        .await
        .unwrap()
        .into_inner();
    let e = admin
        .create_environment(&body(json!({ "template_id": t.id, "name": "packs" })))
        .await
        .unwrap()
        .into_inner();
    let packs = admin
        .list_environment_packs(&e.id)
        .await
        .unwrap()
        .into_inner();
    let profile = packs.iter().find(|p| p.pack.builtin).expect("the profile");
    assert_eq!(
        serde_json::to_value(profile.from).unwrap(),
        json!("default")
    );
    assert!(profile.copy.personal);

    let shared = admin
        .create_copy(&profile.pack.id, &body(json!({ "name": "Shared profile" })))
        .await
        .unwrap()
        .into_inner();
    admin
        .choose_environment_copy(
            &e.id,
            &profile.pack.id,
            &body(json!({ "copy_id": shared.id })),
        )
        .await
        .expect("choosing a copy");
    let packs = admin
        .list_environment_packs(&e.id)
        .await
        .unwrap()
        .into_inner();
    let now = packs.iter().find(|p| p.pack.builtin).unwrap();
    assert_eq!(now.copy.id, shared.id);
    assert_eq!(
        serde_json::to_value(now.from).unwrap(),
        json!("environment")
    );

    // Running, its guest finds its own ~/.gitconfig at odds with the copy's.
    eventually("the environment running", || async {
        let x = admin.get_environment(&e.id).await.ok()?.into_inner();
        (serde_json::to_value(x.phase).ok()? == json!("running")).then_some(())
    })
    .await;
    let conflict = eventually("the guest's conflict", || async {
        admin
            .list_environment_conflicts(&e.id)
            .await
            .ok()?
            .into_inner()
            .into_iter()
            .find(|c| c.path == CONFLICTED)
    })
    .await;
    assert_eq!(conflict.copy_id, shared.id);
    admin
        .resolve_environment_conflict(
            &e.id,
            &body(
                json!({ "copy_id": conflict.copy_id, "path": CONFLICTED, "resolution": "shared" }),
            ),
        )
        .await
        .expect("resolving it");
    eventually("the guest told, and the conflict gone", || async {
        let told = w
            .profile(&e.id)
            .iter()
            .any(|m| m["type"] == "resolve" && m["resolution"] == "shared");
        let left = admin
            .list_environment_conflicts(&e.id)
            .await
            .ok()?
            .into_inner();
        (told && left.is_empty()).then_some(())
    })
    .await;
}
