//! The store of image copies: laid out as the Go worker lays it, so either
//! reads the other's; one fetch shared by everyone asking for a copy at
//! once; current, newest and oldest copies; pruning; and the EROFS image of
//! a local build.

mod common;

use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use common::{Sign, scratch, serve, store, want_image};
use hangar_images::{
    Auth, EROFS_ALIGN, Image, ImageCopy, Store, build_of, read_image_info, read_image_info_erofs,
};

fn copy(reference: &str, digest: &str) -> ImageCopy {
    ImageCopy {
        reference: reference.into(),
        digest: digest.into(),
    }
}

/// A copy as the Go worker leaves one: its reference, digest and mark.
fn go_copy(dir: &std::path::Path, name: &str, c: &ImageCopy, age: u64) {
    let d = dir.join(name);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("ref"), &c.reference).unwrap();
    fs::write(d.join("digest"), &c.digest).unwrap();
    fs::write(d.join("rootfs-is-image"), b"").unwrap();
    fs::write(d.join("image.erofs"), vec![0u8; 4096]).unwrap();
    let at = std::time::SystemTime::now() - Duration::from_secs(age);
    fs::File::options()
        .write(true)
        .open(d.join("ref"))
        .unwrap()
        .set_modified(at)
        .unwrap();
}

#[test]
fn copies_are_named_as_the_go_workers_are() {
    let s = Store::open(&scratch("names"), HashMap::new(), HashMap::new()).unwrap();
    // Two copies the Go worker made on a Mac.
    for (digest, name) in [
        (
            "sha256:8cb9ddb6ee97fcc9e7c571ecb6f5b34dce91aa8596beb2d64ff5bc601164554c",
            "ghcr.io_csnewman_hangar_base_ubuntu-26.04-13ad779b",
        ),
        (
            "sha256:76840584313c38b9d1f9bd1b3e00124b73e2a0028bbc4f75cce8e4c05ec189d0",
            "ghcr.io_csnewman_hangar_base_ubuntu-26.04-27906532",
        ),
    ] {
        let c = copy("ghcr.io/csnewman/hangar/base:ubuntu-26.04", digest);
        assert_eq!(s.path(&c).file_name().unwrap(), name);
    }
}

#[test]
fn a_go_workers_store_opens_as_it_left_it() {
    let dir = scratch("open");
    let r = "ghcr.io/x/base:latest";
    let (old, new) = (copy(r, "sha256:old"), copy(r, "sha256:new"));
    go_copy(&dir, "base-old", &old, 3600);
    go_copy(&dir, "base-new", &new, 60);
    fs::create_dir_all(dir.join("base-x.fetching")).unwrap();
    let s = Store::open(&dir, HashMap::new(), HashMap::new()).unwrap();
    assert!(
        !dir.join("base-x.fetching").exists(),
        "a fetch cut short is started again"
    );

    assert_eq!(s.newest(r).as_deref(), Some("sha256:new"));
    assert_eq!(s.oldest(r).as_deref(), Some("sha256:old"));
    assert_eq!(
        s.latest(r).as_deref(),
        Some("sha256:new"),
        "the newest, until looked up"
    );
    let list = s.list();
    assert_eq!(list.len(), 2);
    assert_eq!(
        list.iter()
            .map(|l| (l.digest.as_str(), l.current, l.state))
            .collect::<Vec<_>>(),
        [
            ("sha256:new", true, "ready"),
            ("sha256:old", false, "ready")
        ]
    );
    let got = s.get(&old, &mut |_| panic!("held, not fetched")).unwrap();
    assert_eq!(
        got,
        Image {
            base: None,
            copy: dir.join("base-old")
        },
        "a pulled copy is its EROFS image"
    );

    // The old copy goes once nothing uses it; the current one stays.
    let (removed, errs) = s.prune(|c| c == &old);
    assert!(removed.is_empty() && errs.is_empty(), "in use");
    let (removed, _) = s.prune(|_| false);
    assert_eq!(removed, std::slice::from_ref(&old));
    assert!(!dir.join("base-old").exists());
    assert_eq!(s.list().len(), 1);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn askers_at_once_share_one_pull_laid_out_as_gos() {
    let reg = store();
    let index = reg.index.clone();
    let dir = scratch("get");
    let want = want_image(&reg, &dir);
    let (addr, seen) = serve(reg, Sign::Bearer);
    let pw = dir.join("password");
    fs::write(&pw, "secret").unwrap();
    let auth = HashMap::from([(
        addr.clone(),
        Auth {
            username: "user".into(),
            password_file: pw,
        },
    )]);
    let s = Arc::new(Store::open(&dir.join("images"), HashMap::new(), auth).unwrap());
    let r = format!("{addr}/test/app:v1");
    assert_eq!(s.current(&r).unwrap(), index);
    let c = copy(&r, &index);

    let askers: Vec<_> = (0..2)
        .map(|_| {
            let (s, c) = (s.clone(), c.clone());
            std::thread::spawn(move || {
                let mut heard = 0;
                let img = s.get(&c, &mut |_| heard += 1).unwrap();
                (img, heard)
            })
        })
        .collect();
    let got: Vec<(Image, usize)> = askers.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(got[0].0, got[1].0);
    // Signed in, each: the first ask of each pull is turned away until it is.
    let fetched = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, p, auth)| {
            m == "GET" && auth.is_some() && p.ends_with(&format!("/manifests/{index}"))
        })
        .count();
    assert_eq!(fetched, 1, "one pull");

    let copy_dir = &got[0].0.copy;
    assert_eq!(fs::read_to_string(copy_dir.join("ref")).unwrap(), r);
    assert_eq!(fs::read_to_string(copy_dir.join("digest")).unwrap(), index);
    assert!(copy_dir.join("rootfs-is-image").exists());
    assert!(
        !copy_dir.join("blobs").exists(),
        "the blobs go once it is made"
    );
    assert_eq!(fs::read(copy_dir.join("image.erofs")).unwrap(), want);
    let listed = s.list();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].current && listed[0].size_bytes > 0);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_local_builds_copy_has_its_image_built_once() {
    let dir = scratch("local");
    let rootfs = dir.join("rootfs");
    fs::create_dir_all(rootfs.join("usr/share/hangar")).unwrap();
    fs::write(
        rootfs.join("usr/share/hangar/image.json"),
        br#"{"desktop":true,"build":"b-42"}"#,
    )
    .unwrap();
    fs::write(rootfs.join("hello"), b"hello\n").unwrap();
    assert_eq!(build_of(&rootfs).unwrap(), "build:b-42");
    let info = read_image_info(&rootfs).unwrap().unwrap();
    assert!(info.desktop);

    let unbuilt = dir.join("plain");
    fs::create_dir_all(&unbuilt).unwrap();
    let id = build_of(&unbuilt).unwrap();
    assert!(
        id.starts_with("local:") && id.len() == "local:".len() + 16,
        "{id}"
    );
    assert!(build_of(&dir.join("missing")).is_err());

    let s = Store::open(&dir.join("images"), HashMap::new(), HashMap::new()).unwrap();
    let copy_dir = dir.join("images/local-copy");
    fs::create_dir_all(&copy_dir).unwrap();
    let img = Image {
        base: Some(rootfs.clone()),
        copy: copy_dir.clone(),
    };
    let built = s.erofs(&img).unwrap();
    assert_eq!(built, copy_dir.join("image.erofs"));
    assert_eq!(fs::metadata(&built).unwrap().len() % EROFS_ALIGN, 0);
    let info = read_image_info_erofs(&built).unwrap().unwrap();
    assert_eq!(
        info.build, "b-42",
        "read from the image, as a pulled copy's is"
    );
    let before = fs::metadata(&built).unwrap().modified().unwrap();
    s.erofs(&img).unwrap();
    assert_eq!(
        fs::metadata(&built).unwrap().modified().unwrap(),
        before,
        "built once"
    );
    fs::remove_dir_all(&dir).unwrap();
}
