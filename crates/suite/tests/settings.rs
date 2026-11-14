//! What an environment is made with and how it follows its template: a
//! machine and repositories of its own, its name, and the settings its
//! template has changed since.

use hangar_suite::{Server, body, frontendapi, machine, status};
use serde_json::json;

async fn template(c: &frontendapi::Client, t: serde_json::Value) -> frontendapi::types::Template {
    c.create_template(&body(t)).await.unwrap().into_inner()
}

async fn environment(
    c: &frontendapi::Client,
    e: serde_json::Value,
) -> frontendapi::types::Environment {
    c.create_environment(&body(e)).await.unwrap().into_inner()
}

fn spec_json(e: &frontendapi::types::Environment) -> serde_json::Value {
    serde_json::to_value(&e.spec).unwrap()
}

#[tokio::test]
async fn an_environment_takes_a_machine_and_repositories_of_its_own() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let mut m = machine("With a repository", "private");
    m["spec"]["repos"] = json!([{ "url": "https://example.com/a.git", "path": "/src/a" }]);
    let t = template(&admin, m).await;

    let plain = environment(&admin, json!({ "template_id": t.id, "name": "plain" })).await;
    assert_eq!(spec_json(&plain)["repos"][0]["path"], json!("/src/a"));
    assert_eq!(spec_json(&plain)["projects"], json!(["/src/a"]));

    let own = environment(
        &admin,
        json!({
            "template_id": t.id,
            "name": "own",
            "settings": { "cpus": 2, "memory_mib": 2048, "display": "none", "gpu": "none" },
            "repos": [{ "url": "https://example.com/b.git", "path": "/src/b" }],
        }),
    )
    .await;
    assert_eq!((own.cpus, own.memory_mib), (2, 2048));
    assert_eq!(
        spec_json(&own)["repos"],
        json!([{ "url": "https://example.com/b.git", "path": "/src/b" }]),
        "the template's repositories kept"
    );
    assert_eq!(spec_json(&own)["projects"], json!(["/src/b"]));
    let mut changes: Vec<String> = own
        .template_changes
        .iter()
        .map(|c| {
            serde_json::to_value(c)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    changes.sort();
    assert_eq!(changes, ["cpus", "memory", "projects", "repos"]);

    let passthrough = json!({
        "template_id": t.id,
        "name": "gpu",
        "settings": { "cpus": 1, "memory_mib": 1024, "display": "none", "gpu": "passthrough" },
    });
    assert_eq!(
        status(admin.create_environment(&body(passthrough)).await),
        Some(400),
        "a GPU passed through for an environment its template gives none"
    );
    let unknown = json!({ "template_id": "00000000-0000-4000-8000-000000000999", "name": "x" });
    assert_eq!(
        status(admin.create_environment(&body(unknown)).await),
        Some(400)
    );
}

#[tokio::test]
async fn names_are_their_owners_whatever_the_case() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let mut m = machine("Named", "shared");
    m["spec"]["repos"] = json!([{
        "url": "https://example.com/a.git",
        "path": "/src/{name:lower}-{short_id}",
        "branch": "work/{owner}/{name}",
    }]);
    let t = template(&admin, m).await;
    let dev = environment(&admin, json!({ "template_id": t.id, "name": "Dev" })).await;
    assert!(
        dev.short_id.len() == 6
            && dev
                .short_id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        "short ID {:?}",
        dev.short_id
    );
    let repo = &spec_json(&dev)["repos"][0];
    assert_eq!(repo["path"], json!(format!("/src/dev-{}", dev.short_id)));
    assert_eq!(repo["branch"], json!("work/admin/Dev"));
    assert_eq!(
        status(
            admin
                .create_environment(&body(json!({ "template_id": t.id, "name": "dev" })))
                .await
        ),
        Some(409),
        "a second environment of the same name in another case"
    );
    let bobs = environment(&bob, json!({ "template_id": t.id, "name": "dev" })).await;
    assert_ne!(bobs.short_id, dev.short_id);
}

#[tokio::test]
async fn an_environment_follows_its_template_when_reset() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let t = template(&admin, machine("Followed", "private")).await;
    let e = environment(&admin, json!({ "template_id": t.id, "name": "follows" })).await;
    assert!(e.template_changes.is_empty());
    assert_ne!(e.template_updated, Some(true));

    let mut bigger = machine("Followed", "private");
    bigger["spec"]["memory_mib"] = json!(2048);
    admin
        .update_template(&t.id, &body(bigger.clone()))
        .await
        .unwrap();
    let e = admin.get_environment(&e.id).await.unwrap().into_inner();
    assert_eq!(
        serde_json::to_value(&e.template_changes).unwrap(),
        json!(["memory"])
    );
    assert_eq!(e.template_updated, Some(true));

    let reset = admin
        .reset_environment_to_template(&e.id)
        .await
        .expect("resetting an environment not yet placed")
        .into_inner();
    assert_eq!(reset.memory_mib, 2048);
    assert!(reset.template_changes.is_empty());
    assert_ne!(reset.template_updated, Some(true));

    let mut other = bigger;
    other["spec"]["image"] = json!("ghcr.io/csnewman/hangar/base:ubuntu-26.04");
    admin.update_template(&t.id, &body(other)).await.unwrap();
    assert_eq!(
        status(admin.reset_environment_to_template(&e.id).await),
        Some(409),
        "reset onto another image"
    );
}
