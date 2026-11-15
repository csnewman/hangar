//! Workers: registering with the bootstrap token, what they offer, and
//! being revoked and removed.

use hangar_suite::{Server, eventually, frontendapi, status, why};

/// The worker named name, once the server lists it online and it has
/// reported what it offers. Asking for its desired state is enough to be
/// online, and a worker asks before its first report; every report says
/// its version.
async fn online(admin: &frontendapi::Client, name: &str) -> frontendapi::types::Worker {
    eventually(&format!("{name} online"), || async {
        match admin.list_workers().await {
            Ok(ws) => ws
                .into_inner()
                .into_iter()
                .find(|w| w.name == name && w.online && w.version.is_some()),
            Err(e) => {
                eprintln!("listing workers: {}", why(&e));
                None
            }
        }
    })
    .await
}

#[tokio::test]
async fn a_worker_registers_and_offers_its_machine() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let _w = s.worker("w1");
    let w = online(&admin, "w1").await;
    assert_eq!(w.labels.get("runtime").map(String::as_str), Some("fake"));
    assert!(w.capacity.cpus > 0 && w.capacity.memory_mib > 0, "{w:?}");
    assert_eq!(
        serde_json::to_value(w.monitor).unwrap(),
        serde_json::json!("krun")
    );
    assert!(!w.revoked);
    let one = admin
        .get_worker(&w.id)
        .await
        .expect("the worker")
        .into_inner();
    assert_eq!(one.name, "w1");
}

#[tokio::test]
async fn a_wrong_bootstrap_token_is_rejected() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let mut w = s.worker_with_token("w-bad", "not-the-bootstrap-token");
    let out = w.ended().await.expect("the worker stops");
    assert!(
        out.unwrap_err().contains("rejected"),
        "it stopped for another reason"
    );
    let listed = admin.list_workers().await.unwrap().into_inner();
    assert!(listed.iter().all(|w| w.name != "w-bad"), "{listed:?}");
}

#[tokio::test]
async fn a_revoked_worker_is_cut_off_and_then_removed() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let mut w = s.worker("w2");
    let id = online(&admin, "w2").await.id;
    assert_eq!(
        status(admin.delete_worker(&id).await),
        Some(409),
        "removed before it was revoked"
    );
    admin.revoke_worker(&id).await.expect("revoking it");
    let out = w.ended().await.expect("the worker stops");
    assert!(out.unwrap_err().contains("rejected"));
    let revoked = admin.get_worker(&id).await.unwrap().into_inner();
    assert!(revoked.revoked);
    admin.delete_worker(&id).await.expect("removing it");
    assert_eq!(status(admin.get_worker(&id).await), Some(404));
}

#[tokio::test]
async fn only_an_administrator_sees_workers() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let bob = s.user(&admin, "bob").await;
    assert_eq!(status(bob.list_workers().await), Some(403));
}

/// An administrator asks a worker to delete a copy of an image it holds
/// and an environment it runs that the server has no record of; the
/// worker's desired set says so. A copy it does not hold, and an
/// environment it does not report, are refused.
#[tokio::test]
async fn a_worker_is_asked_to_remove_an_image_and_a_stray() {
    use hangar_api::{
        API_LEVEL, DesiredSet, ImageState, LEVEL_HEADER, LocalImage, ObservedEnvironment, Phase,
        RegisterWorker, Resources, WorkerCredential, WorkerStatus,
    };
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let http = s.http();
    let cred: WorkerCredential = http
        .post(format!("{}/api/worker/v1/register", s.url()))
        .header(LEVEL_HEADER, API_LEVEL)
        .bearer_auth(hangar_suite::BOOTSTRAP_TOKEN)
        .json(&RegisterWorker {
            name: "w1".into(),
            labels: Default::default(),
        })
        .send()
        .await
        .expect("registering")
        .json()
        .await
        .expect("the credential");
    let stray = "6f1c3a52-9d0e-4b8a-a0f2-3c5d7e9f1b24";
    let (image, digest) = ("example.com/tools:1", format!("sha256:{}", "a".repeat(64)));
    let report = WorkerStatus {
        capacity: Resources {
            cpus: 4,
            memory_mib: 8192,
        },
        labels: Default::default(),
        environments: vec![ObservedEnvironment {
            id: stray.into(),
            phase: Phase::Running,
            reason: String::new(),
            progress: None,
            stats: None,
            image_digest: String::new(),
            image_pinned: false,
            image_update: None,
            image_rollback: None,
            guest_agent: None,
        }],
        stats: None,
        images: vec![LocalImage {
            r#ref: image.into(),
            digest: digest.clone(),
            size_bytes: 1 << 20,
            state: ImageState::Ready,
            environments: Vec::new(),
            current: true,
        }],
        gpu: None,
        monitor: String::new(),
        version: String::new(),
        features: Vec::new(),
    };
    let r = http
        .put(format!("{}/api/worker/v1/status", s.url()))
        .header(LEVEL_HEADER, API_LEVEL)
        .bearer_auth(&cred.credential)
        .json(&report)
        .send()
        .await
        .expect("reporting");
    assert!(r.status().is_success(), "{}", r.status());
    let w = admin
        .get_worker(&cred.id)
        .await
        .expect("the worker")
        .into_inner();
    assert_eq!(
        serde_json::to_value(&w.unknown).unwrap(),
        serde_json::json!([stray])
    );

    let copy = |digest: &str| frontendapi::types::ImageCopy {
        ref_: image.parse().unwrap(),
        digest: digest.into(),
    };
    admin
        .remove_worker_image(&cred.id, &copy(&digest))
        .await
        .expect("removing the image");
    let other = format!("sha256:{}", "b".repeat(64));
    assert_eq!(
        status(admin.remove_worker_image(&cred.id, &copy(&other)).await),
        Some(404)
    );
    let envs = |ids: &[&str]| frontendapi::types::RemoveUnknownEnvironments {
        environments: ids.iter().map(|i| i.to_string()).collect(),
    };
    admin
        .remove_unknown_environments(&cred.id, &envs(&[stray]))
        .await
        .expect("removing the stray");
    assert_eq!(
        status(
            admin
                .remove_unknown_environments(&cred.id, &envs(&["not-reported"]))
                .await
        ),
        Some(400)
    );
    assert_eq!(
        status(
            admin
                .remove_unknown_environments(
                    "00000000-0000-4000-8000-0000000000ff",
                    &envs(&[stray])
                )
                .await
        ),
        Some(404)
    );

    let set: DesiredSet = http
        .get(format!("{}/api/worker/v1/desired", s.url()))
        .header(LEVEL_HEADER, API_LEVEL)
        .bearer_auth(&cred.credential)
        .send()
        .await
        .expect("the desired set")
        .json()
        .await
        .expect("a desired set");
    assert_eq!(set.remove_images.len(), 1, "{set:?}");
    assert_eq!(set.remove_images[0].r#ref, image);
    assert_eq!(set.remove_images[0].digest, digest);
    assert_eq!(set.remove_environments, [stray]);
    let w = admin
        .get_worker(&cred.id)
        .await
        .expect("the worker")
        .into_inner();
    assert_eq!(w.pending_removals.len(), 1, "{w:?}");
}

/// A worker and a server compare worker API levels before anything else:
/// the server answers every request with the levels it serves, and refuses
/// a worker that names none or one outside them, as a conflict that names
/// which of the two to upgrade.
#[tokio::test]
async fn a_worker_outside_the_served_levels_is_refused() {
    use hangar_api::{API_LEVEL, LEVEL_HEADER, LEVELS_HEADER, Levels, RegisterWorker};
    let Some(s) = Server::start() else { return };
    let http = s.http();
    let register = |level: Option<String>| {
        let mut r = http
            .post(format!("{}/api/worker/v1/register", s.url()))
            .bearer_auth(hangar_suite::BOOTSTRAP_TOKEN)
            .json(&RegisterWorker {
                name: "w".into(),
                labels: Default::default(),
            });
        if let Some(l) = level {
            r = r.header(LEVEL_HEADER, l);
        }
        r.send()
    };
    for (level, code) in [
        (None, "worker_too_old"),
        (Some("0".to_string()), "worker_too_old"),
        (Some((API_LEVEL + 1).to_string()), "server_too_old"),
    ] {
        let r = register(level.clone()).await.unwrap();
        assert_eq!(r.status(), 409, "level {level:?}");
        let levels = r.headers()[LEVELS_HEADER].to_str().unwrap().to_string();
        assert_eq!(Levels::parse(&levels), Some(Levels::SERVED));
        let body: serde_json::Value = r.json().await.unwrap();
        assert_eq!(body["code"], code, "level {level:?}: {body}");
    }
    let r = register(Some(API_LEVEL.to_string())).await.unwrap();
    assert_eq!(r.status(), 201);
    assert!(r.headers().contains_key(LEVELS_HEADER));
}

/// A worker reports its build and what it can do, which its page shows.
#[tokio::test]
async fn a_worker_reports_its_version_and_features() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let _w = s.worker("w");
    let w = eventually("w reporting", || async {
        let ws = admin.list_workers().await.ok()?.into_inner();
        ws.into_iter()
            .find(|w| w.name == "w" && w.version.is_some())
    })
    .await;
    assert_eq!(w.version.as_deref(), Some(hangar_worker::VERSION));
    assert_eq!(w.features, ["suspend"]);
}
