//! Pulls from a small registry speaking just enough of the distribution
//! API: signing in with a token from its realm or with Basic credentials,
//! a multi-platform index, gzip and zstd layers, and blobs sent on to a
//! store that must not be given the registry's credentials.

mod common;

use std::collections::HashMap;

use common::{Sign, digest, scratch, serve, store, want_image};
use hangar_images::{Auth, Client, HangarRegistry, Progress, Reference, Stage};

#[test]
fn a_pull_signs_in_picks_its_platform_and_makes_the_image() {
    let s = store();
    let index = s.index.clone();
    let dir = scratch("bearer");
    let want = want_image(&s, &dir);
    let (addr, seen) = serve(s, Sign::Bearer);
    let pw = dir.join("password");
    std::fs::write(&pw, "secret\n").unwrap();
    let auth = HashMap::from([(
        addr.clone(),
        Auth {
            username: "user".into(),
            password_file: pw,
        },
    )]);
    let client = Client::new(auth, None);
    let r = Reference::parse(&format!("{addr}/test/app:v1")).unwrap();

    assert_eq!(client.resolve(&r).unwrap(), index, "the index's digest");

    let mut said: Vec<Progress> = vec![];
    let out = dir.join("image.erofs");
    client
        .pull(&r, &index, &dir.join("blobs"), &out, &mut |p| said.push(p))
        .unwrap();
    assert_eq!(
        std::fs::read(&out).unwrap(),
        want,
        "the image the layers make"
    );

    let last_download = said.iter().rfind(|p| p.stage == Stage::Download).unwrap();
    assert_eq!(last_download.done, last_download.total);
    let last = said.last().unwrap();
    assert_eq!((last.stage, last.layer, last.layers), (Stage::Unpack, 2, 2));
    assert_eq!(last.done, last.total);

    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .all(|(_, p, _)| !p.contains("/manifests/sha256:")
                || !p.ends_with(&digest(br#"{"not":"fetched"}"#))),
        "another platform's manifest is not fetched"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn hangars_registry_is_reached_through_the_server_with_the_workers_credential() {
    let s = store();
    let index = s.index.clone();
    let dir = scratch("hangar");
    let want = want_image(&s, &dir);
    let (addr, _) = serve(s, Sign::Basic);
    let hangar = HangarRegistry {
        host: "Registry.Hangar.Test".into(),
        url: format!("http://{addr}/api/worker/v1/registry"),
        credential: "hgw_cred".into(),
    };
    let client = Client::new(HashMap::new(), Some(hangar));
    let r = Reference::parse("registry.hangar.test/test/app:v1").unwrap();
    assert_eq!(client.resolve(&r).unwrap(), index);
    let out = dir.join("image.erofs");
    client
        .pull(&r, &index, &dir.join("blobs"), &out, &mut |_| {})
        .unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), want);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn what_cannot_be_pulled_says_why() {
    let s = store();
    let dir = scratch("fail");
    let (addr, _) = serve(s, Sign::Bearer);
    let pw = dir.join("password");
    std::fs::write(&pw, "wrong").unwrap();
    let auth = HashMap::from([(
        addr.clone(),
        Auth {
            username: "user".into(),
            password_file: pw.clone(),
        },
    )]);
    let r = Reference::parse(&format!("{addr}/test/app:v1")).unwrap();
    let err = Client::new(auth, None).resolve(&r).unwrap_err().to_string();
    assert!(err.contains("token"), "a refused sign-in: {err}");

    std::fs::write(&pw, "secret").unwrap();
    let auth = HashMap::from([(
        addr.clone(),
        Auth {
            username: "user".into(),
            password_file: pw,
        },
    )]);
    let client = Client::new(auth, None);
    let missing = Reference::parse(&format!("{addr}/test/app:v2")).unwrap();
    let err = client.resolve(&missing).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{err}");
    let wrong = format!("sha256:{}", "0".repeat(64));
    let err = client
        .pull(
            &r,
            &wrong,
            &dir.join("blobs"),
            &dir.join("x.erofs"),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(err.to_string().contains("MANIFEST_UNKNOWN"), "{err}");
    assert!(!dir.join("x.erofs").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn references_read_as_dockers() {
    let r = |s: &str| Reference::parse(s).map(|r| r.to_string());
    assert_eq!(r("ubuntu").unwrap(), "docker.io/library/ubuntu:latest");
    assert_eq!(r("ubuntu:24.04").unwrap(), "docker.io/library/ubuntu:24.04");
    assert_eq!(r("csnewman/app").unwrap(), "docker.io/csnewman/app:latest");
    assert_eq!(
        r("index.docker.io/library/ubuntu").unwrap(),
        "docker.io/library/ubuntu:latest"
    );
    assert_eq!(
        r("ghcr.io/csnewman/hangar/base:ubuntu-26.04").unwrap(),
        "ghcr.io/csnewman/hangar/base:ubuntu-26.04"
    );
    assert_eq!(
        r("localhost:5000/app").unwrap(),
        "localhost:5000/app:latest"
    );
    assert_eq!(r("localhost/app").unwrap(), "localhost/app:latest");
    let d = format!("sha256:{}", "a".repeat(64));
    assert_eq!(
        r(&format!("app:v1@{d}")).unwrap(),
        format!("docker.io/library/app@{d}"),
        "a digest wins"
    );
    assert!(r("App").is_err(), "capitals");
    assert!(r("app:bad tag").is_err());
    assert!(r("app@sha256:short").is_err());
    let local = Reference::parse("127.0.0.1:5000/app").unwrap();
    assert!(local.is_local());
    assert!(!Reference::parse("ghcr.io/x/y").unwrap().is_local());
    assert_eq!(
        Reference::parse("ubuntu").unwrap().api_host(),
        "registry-1.docker.io"
    );
}
