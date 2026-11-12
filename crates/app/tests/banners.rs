//! What needs deciding about an environment: its image's newer build, an
//! upgrade to keep or roll back, a change waiting, and how it differs from
//! its template, each with what can be done now.

use chrono::Utc;
use hangar_app::api::{Details, Environment, ImageRollback, ImageUpdate};
use hangar_app::banners::{Act, Does, Kind, Tone, banners};

fn env(phase: &str, desired: &str) -> Environment {
    Environment {
        id: "e1".into(),
        short_id: "abc123".into(),
        name: "ledger-1".into(),
        owner: "alice".into(),
        template: "Rust service".into(),
        phase: phase.into(),
        desired: desired.into(),
        reason: None,
        progress: None,
        worker: Some("w1".into()),
        worker_online: Some(true),
        cpus: Some(2),
        memory_mib: Some(4096),
        editor_path: None,
        ports_public: false,
        display: None,
        image_update: None,
        web_names: vec![],
        details: Details {
            image: Some("base:ubuntu".into()),
            image_digest: Some("sha256:aaaaaaaaaaaaaaaa".into()),
            template_id: Some("t1".into()),
            ..Default::default()
        },
        task: None,
        agents: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn newer(checked: bool, conflicts: &[&str], more: i64) -> Option<ImageUpdate> {
    Some(ImageUpdate {
        digest: "sha256:bbbbbbbbbbbbbbbb".into(),
        checked,
        conflicts: conflicts.iter().map(|c| c.to_string()).collect(),
        more_conflicts: more,
        ..Default::default()
    })
}

fn upgrade(force: bool) -> Does {
    Does::Image {
        change: "upgrade",
        force,
    }
}

#[test]
fn nothing_to_decide_says_nothing() {
    assert!(banners(&env("running", "running")).is_empty());
}

#[test]
fn a_newer_build_is_upgraded_to_once_stopped_and_compared() {
    let mut e = env("running", "running");
    e.image_update = newer(false, &[], 0);
    let b = &banners(&e)[0];
    assert_eq!(
        (b.kind, b.tone, b.dismissible),
        (Kind::Image, Tone::Warn, true)
    );
    assert!(b.text.starts_with(
        "A newer build of base:ubuntu is ready (bbbbbbbbbbbb; this has aaaaaaaaaaaa)."
    ));
    assert!(
        b.text
            .ends_with("Stop the environment to check what upgrading would leave hidden.")
    );
    assert_eq!(
        b.acts,
        [Act {
            label: "Upgrade at Next Start",
            does: upgrade(false),
            ready: false,
            why: Some("Stop the environment first"),
        }]
    );

    let mut e = env("stopped", "stopped");
    e.image_update = newer(true, &[], 0);
    let b = &banners(&e)[0];
    assert!(
        b.text
            .ends_with("None of its changes hide a file the newer build changes.")
    );
    assert!(b.acts[0].ready && b.acts[0].does == upgrade(false));
}

#[test]
fn files_an_upgrade_would_hide_ask_for_it_anyway() {
    let mut e = env("stopped", "stopped");
    e.image_update = newer(true, &["/etc/app.conf", "/usr/lib/x.so"], 3);
    let b = &banners(&e)[0];
    assert!(
        b.text
            .ends_with("5 files it has changed would hide the newer build's.")
    );
    assert_eq!(b.files, ["/etc/app.conf", "/usr/lib/x.so", "and 3 more"]);
    assert_eq!(b.acts[0].label, "Upgrade Anyway");
    assert_eq!(b.acts[0].does, upgrade(true));
}

#[test]
fn a_change_waiting_is_all_that_is_said_of_the_image() {
    let mut e = env("stopped", "stopped");
    e.image_update = newer(true, &[], 0);
    e.details.image_change = Some("upgrade".into());
    let list = banners(&e);
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0].text,
        "Its next start upgrades base:ubuntu to bbbbbbbbbbbb, keeping a copy of its writable disk to roll back to."
    );
    assert_eq!(list[0].acts[0].label, "Cancel");
    assert!(!list[0].dismissible);
}

#[test]
fn an_upgrade_is_kept_or_rolled_back() {
    let mut e = env("running", "running");
    e.details.image_rollback = Some(ImageRollback {
        digest: "sha256:aaaaaaaaaaaaaaaa".into(),
        at: Utc::now(),
        size_bytes: 3 * 1024 * 1024,
    });
    let b = &banners(&e)[0];
    let labels: Vec<(&str, bool)> = b.acts.iter().map(|a| (a.label, a.ready)).collect();
    assert_eq!(labels, [("Roll Back", false), ("Keep Upgrade", true)]);
}

#[test]
fn how_it_differs_from_its_template_is_said_in_words() {
    let mut e = env("stopped", "stopped");
    e.details.template_changes = vec!["cpus".into(), "dax".into(), "image".into()];
    let b = &banners(&e)[0];
    assert_eq!(b.kind, Kind::Template);
    assert_eq!(
        b.text,
        "Changed from its template, Rust service: vCPUs, image mapping, image."
    );
    assert_eq!(b.acts[0].does, Does::Reset);
    assert!(b.acts[0].ready);
}
