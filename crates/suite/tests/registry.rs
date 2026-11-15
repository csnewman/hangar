//! The server's own container registry: a push makes a repository its
//! pusher's, others reach it only as its visibility and collaborators say,
//! the web UI lists it, a template names it only if its saver may pull
//! it, and a worker pulls it with its own credential.

use base64::Engine;
use hangar_suite::{Server, body, eventually, machine, status};
use serde_json::json;
use sha2::Digest;

fn digest(b: &[u8]) -> String {
    format!("sha256:{}", hex(&sha2::Sha256::digest(b)))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn basic(user: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"))
    )
}

/// Pushes a tiny image to repo as user, with its config and one layer
/// uploaded monolithically, giving the manifest's digest.
async fn push(
    http: &reqwest::Client,
    base: &str,
    auth: &str,
    repo: &str,
    tag: &str,
) -> Result<String, u16> {
    let config = br#"{"architecture":"arm64","os":"linux"}"#.to_vec();
    let layer = b"a layer of the suite's".to_vec();
    for blob in [&config, &layer] {
        let r = http
            .post(format!(
                "{base}/v2/{repo}/blobs/uploads/?digest={}",
                digest(blob)
            ))
            .header("authorization", auth)
            .header("content-type", "application/octet-stream")
            .body(blob.clone())
            .send()
            .await
            .unwrap();
        if r.status().as_u16() != 201 {
            return Err(r.status().as_u16());
        }
    }
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": { "mediaType": "application/vnd.oci.image.config.v1+json", "digest": digest(&config), "size": config.len() },
        "layers": [{ "mediaType": "application/vnd.oci.image.layer.v1.tar", "digest": digest(&layer), "size": layer.len() }],
    })
    .to_string();
    let r = http
        .put(format!("{base}/v2/{repo}/manifests/{tag}"))
        .header("authorization", auth)
        .header("content-type", "application/vnd.oci.image.manifest.v1+json")
        .body(manifest.clone())
        .send()
        .await
        .unwrap();
    match r.status().as_u16() {
        201 => Ok(digest(manifest.as_bytes())),
        s => Err(s),
    }
}

async fn pull(http: &reqwest::Client, base: &str, auth: &str, repo: &str, tag: &str) -> u16 {
    http.get(format!("{base}/v2/{repo}/manifests/{tag}"))
        .header("authorization", auth)
        .header("accept", "application/vnd.oci.image.manifest.v1+json")
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test]
async fn a_repository_is_its_pushers() {
    let Some(s) = Server::start() else { return };
    let registry = format!("registry.hangar.localhost:{}", s.port());
    let base = format!("http://{registry}");
    let http = s.http_to(&["registry.hangar.localhost"]);
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    let admin_auth = basic("admin", &s.token(&admin, "registry").await);
    let bob_auth = basic("bob", &s.token(&bob, "registry").await);

    let refused = http.get(format!("{base}/v2/")).send().await.unwrap();
    assert_eq!(refused.status().as_u16(), 401);
    assert!(refused.headers().contains_key("www-authenticate"));

    let manifest = push(&http, &base, &admin_auth, "admin/tool", "v1")
        .await
        .expect("pushing");
    assert_eq!(
        pull(&http, &base, &admin_auth, "admin/tool", "v1").await,
        200
    );
    assert_eq!(
        pull(&http, &base, &admin_auth, "admin/tool", &manifest).await,
        200
    );
    assert_eq!(
        pull(&http, &base, &bob_auth, "admin/tool", "v1").await,
        404,
        "bob pulled a private repository"
    );
    assert_eq!(
        push(&http, &base, &bob_auth, "admin/bobs", "v1").await,
        Err(403),
        "bob pushed to admin's namespace"
    );

    // The web UI lists it, and sharing it lets bob pull.
    let repos = admin.list_image_repositories().await.unwrap().into_inner();
    let repo = repos
        .iter()
        .find(|r| r.path == "admin/tool")
        .expect("the repository listed");
    assert_eq!(repo.name, format!("{registry}/admin/tool"));
    let one = admin
        .get_image_repository(&repo.id)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(one.tags.len(), 1);
    admin
        .update_image_repository(&repo.id, &body(json!({ "visibility": "shared" })))
        .await
        .expect("sharing it");
    assert_eq!(pull(&http, &base, &bob_auth, "admin/tool", "v1").await, 200);

    // A template names it only if its saver may pull it.
    let mut t = machine("From the registry", "private");
    t["spec"]["image"] = json!(format!("{registry}/admin/tool:v1"));
    bob.create_template(&body(t.clone()))
        .await
        .expect("a template of a shared image");
    admin
        .update_image_repository(&repo.id, &body(json!({ "visibility": "private" })))
        .await
        .unwrap();
    t["name"] = json!("Another");
    assert_eq!(
        status(bob.create_template(&body(t)).await),
        Some(400),
        "a template of an image its saver cannot pull"
    );

    // A worker pulls with its own credential, through the worker API.
    let w = s.worker("w");
    let credential = eventually("the worker registered", || async { w.credential() }).await;
    let worker = s.http();
    let r = worker
        .get(format!(
            "{}/api/worker/v1/registry/v2/admin/tool/manifests/v1",
            s.url()
        ))
        .header("authorization", basic("worker", &credential))
        .header("accept", "application/vnd.oci.image.manifest.v1+json")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "a worker pulled a private image");

    admin
        .delete_image_tag(&repo.id, "v1")
        .await
        .expect("deleting the tag");
    assert_eq!(
        pull(&http, &base, &admin_auth, "admin/tool", "v1").await,
        404
    );
}
