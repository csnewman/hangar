//! An image's layers merged into one EROFS image: held to the Go merge's
//! image of the same layers (internal/vm/golden_merge_test.go writes them
//! and its hash), and read back for what each layer changed.

use std::fs::File;
use std::io::Read;

use hangar_erofs::{Image, Kind};
use hangar_images::{EROFS_ALIGN, build_erofs};
use sha2::{Digest, Sha256};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/");

/// The fixture's layers, top first, merged into an image in a scratch file.
fn merged(name: &str) -> std::path::PathBuf {
    let out = std::env::temp_dir().join(format!("hangar-merge-{}-{name}", std::process::id()));
    let mut layers = vec!["top.tar", "bottom.tar"].into_iter();
    build_erofs(&out, || {
        Ok(layers
            .next()
            .map(|l| Box::new(File::open(format!("{DATA}{l}")).unwrap()) as Box<dyn Read>))
    })
    .unwrap();
    out
}

#[test]
fn the_image_is_the_go_merges_byte_for_byte() {
    let out = merged("hash");
    let bytes = std::fs::read(&out).unwrap();
    std::fs::remove_file(&out).unwrap();
    assert_eq!(bytes.len() as u64 % EROFS_ALIGN, 0, "padded to 2 MiB");
    let want = std::fs::read_to_string(format!("{DATA}merged.sha256")).unwrap();
    assert_eq!(hex::encode(Sha256::digest(&bytes)), want.trim());
}

#[test]
fn each_layer_changes_what_it_says() {
    let out = merged("tree");
    let img = Image::open(File::open(&out).unwrap()).unwrap();
    let read = |p: &str| {
        img.lookup(p.as_bytes(), true)
            .and_then(|i| img.read_all(&i))
            .map(|b| String::from_utf8(b).unwrap())
            .unwrap_or_else(|e| format!("<{e}>"))
    };
    let long = format!("{}/{}", "d".repeat(120), "f".repeat(120));
    for (p, want) in [
        ("etc/a", "changed\n"),
        ("etc/kept", "kept\n"),
        ("data/new", "new\n"),
        ("swap", "now a file\n"),
        ("etc/hard", "changed\n"),
        (long.as_str(), "long\n"),
    ] {
        assert_eq!(read(p), want, "{p}");
    }
    for p in [
        "etc/gone",
        "etc/.wh.gone",
        "opt/gonedir",
        "opt/.wh.gonedir",
        "data/old",
        "data/.wh..wh..opq",
        "swap/inside",
    ] {
        assert!(
            img.lookup(p.as_bytes(), false).is_err(),
            "{p} is in the image"
        );
    }
    let bin = img.lookup(b"bin", false).unwrap();
    assert_eq!(img.readlink(&bin).unwrap(), b"usr/bin");
    let owned = img.lookup(b"etc/owned", false).unwrap();
    assert_eq!(
        (owned.uid, owned.gid),
        (3000000, 3000001),
        "owners from PAX"
    );
    let precise = img.lookup(b"etc/precise", false).unwrap();
    assert_eq!(
        (precise.mtime, precise.nsec),
        (1700000000, 123456789),
        "a PAX time"
    );
    let attrs = img.xattrs(&img.lookup(b"etc/a", false).unwrap()).unwrap();
    assert_eq!(
        attrs.get(&b"user.hangar"[..]).map(Vec::as_slice),
        Some(&b"yes"[..])
    );
    let null = img.lookup(b"dev/null", false).unwrap();
    assert_eq!(
        (null.kind, null.major, null.minor),
        (Kind::CharDevice, 1, 3)
    );
    assert_eq!(img.lookup(b"run/fifo", false).unwrap().kind, Kind::Fifo);
    std::fs::remove_file(&out).unwrap();
}

#[test]
fn a_root_no_layer_names_is_dockers() {
    let mut tar = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_ustar();
    h.set_entry_type(tar::EntryType::Directory);
    h.set_mode(0o700);
    h.set_size(0);
    tar.append_data(&mut h, "etc/", std::io::empty()).unwrap();
    let layer = tar.into_inner().unwrap();
    let out = std::env::temp_dir().join(format!("hangar-merge-{}-root", std::process::id()));
    let mut once = Some(layer);
    build_erofs(&out, || {
        Ok(once
            .take()
            .map(|l| Box::new(std::io::Cursor::new(l)) as Box<dyn Read>))
    })
    .unwrap();
    let img = Image::open(File::open(&out).unwrap()).unwrap();
    let root = img.root().unwrap();
    assert_eq!((root.perm, root.uid, root.gid), (0o755, 0, 0));
    assert_eq!(img.lookup(b"etc", false).unwrap().perm, 0o700);
    std::fs::remove_file(&out).unwrap();
}

#[test]
fn a_failed_build_leaves_nothing() {
    let out = std::env::temp_dir().join(format!("hangar-merge-{}-fail", std::process::id()));
    let err = build_erofs(&out, || {
        Err(std::io::Error::other("the registry went away"))
    });
    assert!(err.is_err());
    assert!(!out.exists());
}
