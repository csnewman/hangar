//! What a layer hides of an upgrade: paths it replaces or deletes that the
//! two copies of the image differ in, and opaque directories whose entries
//! they differ in.

use std::collections::BTreeMap;

use hangar_erofs::{Entry, Image, Kind, Writer};
use hangar_layercheck::{Found, Layer, compare};

/// An image of files: (path, kind, perm, data or target).
fn image(files: &[(&str, Kind, u32, &str)]) -> Image<Vec<u8>> {
    let mut w = Writer::new(Vec::new());
    for (path, kind, perm, data) in files {
        let mut e = Entry {
            path: path.as_bytes().to_vec(),
            kind: Some(*kind),
            perm: *perm,
            ..Default::default()
        };
        match kind {
            Kind::Regular => e.size = data.len() as u64,
            Kind::Symlink => e.target = data.as_bytes().to_vec(),
            _ => {}
        }
        w.add(&e, &mut data.as_bytes()).unwrap();
    }
    w.close().unwrap();
    let size = w.size() as usize;
    let mut out = w.into_inner();
    out.resize(size, 0);
    Image::open(out).unwrap()
}

fn reg(path: &'static str, data: &'static str) -> (&'static str, Kind, u32, &'static str) {
    (path, Kind::Regular, 0o644, data)
}

fn link(path: &'static str, target: &'static str) -> (&'static str, Kind, u32, &'static str) {
    (path, Kind::Symlink, 0o777, target)
}

/// An upper directory: its paths, each a directory or not, and those
/// overlayfs takes as opaque.
struct MapLayer {
    paths: BTreeMap<String, bool>,
    opaque: Vec<&'static str>,
}

impl MapLayer {
    fn new(files: &[&str], dirs: &[&str], opaque: Vec<&'static str>) -> MapLayer {
        let mut paths = BTreeMap::new();
        let given = dirs
            .iter()
            .map(|d| (d, true))
            .chain(files.iter().map(|f| (f, false)));
        for (path, dir) in given {
            paths.insert(path.to_string(), dir);
            // Its parents are directories too.
            let mut p = path.to_string();
            while let Some(i) = p.rfind('/') {
                p.truncate(i);
                paths.insert(p.clone(), true);
            }
        }
        MapLayer { paths, opaque }
    }
}

impl Layer for MapLayer {
    fn read_dir(&self, rel: &[u8]) -> std::io::Result<Vec<(Vec<u8>, bool)>> {
        let rel = String::from_utf8_lossy(rel);
        Ok(self
            .paths
            .iter()
            .filter_map(|(p, dir)| {
                let parent = p.rfind('/').map(|i| &p[..i]).unwrap_or("");
                let name = p.rsplit('/').next().unwrap();
                (parent == rel).then(|| (name.as_bytes().to_vec(), *dir))
            })
            .collect())
    }

    fn opaque(&self, rel: &[u8]) -> std::io::Result<bool> {
        Ok(self.opaque.contains(&&*String::from_utf8_lossy(rel)))
    }
}

#[test]
fn what_the_layer_hides_of_what_changed() {
    let older = image(&[
        reg("etc/changed", "old"),
        reg("etc/same", "same"),
        reg("etc/gone", "old"),
        reg("etc/kept", "kept"),
        reg("etc/mode", "x"),
        link("etc/link", "a"),
        reg("opt/opaque/a", "a"),
        reg("opt/still/a", "a"),
        reg("var/lib/dpkg/status", "old packages"),
        reg("tmp/scratch", "old"),
        reg("usr/lib/thing", "old"),
        link("lib", "usr/lib"),
    ]);
    let newer = image(&[
        reg("etc/changed", "new"),
        reg("etc/same", "same"),
        reg("etc/kept", "kept"),
        ("etc/mode", Kind::Regular, 0o600, "x"),
        link("etc/link", "b"),
        reg("etc/added", "new"),
        reg("opt/opaque/a", "a"),
        reg("opt/opaque/b", "b"),
        reg("opt/still/a", "a"),
        reg("var/lib/dpkg/status", "new packages"),
        reg("tmp/scratch", "new"),
        reg("usr/lib/thing", "new"),
        link("lib", "usr/lib"),
    ]);
    // Whiteouts (etc/gone, etc/kept) are files in the layer like any other.
    let layer = MapLayer::new(
        &[
            "etc/changed",
            "etc/same",
            "etc/gone",
            "etc/kept",
            "etc/mode",
            "etc/link",
            "etc/mine",
            "var/lib/dpkg/status",
            "tmp/scratch",
            // Through a symlink in the image, as the guest resolves it.
            "lib/thing",
        ],
        &["opt/opaque", "opt/still"],
        vec!["opt/opaque", "opt/still"],
    );
    let mut found = compare(&layer, &older, &newer).unwrap();
    found.conflicts.sort();
    assert_eq!(
        found.conflicts,
        [
            "/etc/changed",
            "/etc/gone",
            "/etc/link",
            "/etc/mode",
            "/lib/thing",
            "/opt/opaque/",
            "/var/lib/dpkg/status",
        ]
    );
    assert!(found.packages, "the package database");

    let quiet = MapLayer::new(&["etc/same", "etc/mine"], &[], vec![]);
    assert_eq!(compare(&quiet, &older, &newer).unwrap(), Found::default());
}

/// The layer as an environment's disk keeps it: the upper directory of an
/// ext4 filesystem, made by mkfs.ext4 (on PATH, or the one
/// dev/krun-build.sh builds on a Mac).
#[test]
fn an_ext4_disks_upper_directory_is_the_layer() {
    let Some(mkfs) = [
        "mkfs.ext4",
        "/sbin/mkfs.ext4",
        "/usr/sbin/mkfs.ext4",
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../out/krun/bin/mkfs.ext4"),
    ]
    .into_iter()
    .find(|p| std::process::Command::new(p).arg("-V").output().is_ok()) else {
        eprintln!("no mkfs.ext4: skipped");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("hangar-layercheck-{}", std::process::id()));
    let src = tmp.join("src");
    for (p, data) in [
        ("upper/etc/changed", "mine"),
        ("upper/etc/same", "mine"),
        ("upper/etc/mine", "mine"),
        ("upper/var/log/syslog", "ignored"),
        ("docker/whatever", "not the layer"),
    ] {
        let full = src.join(p);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, data).unwrap();
    }
    let disk = tmp.join("upper.ext4");
    std::fs::File::create(&disk)
        .unwrap()
        .set_len(64 << 20)
        .unwrap();
    let out = std::process::Command::new(mkfs)
        .args(["-q", "-F", "-d"])
        .arg(&src)
        .arg(&disk)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let fs = hangar_ext4::Fs::open(std::fs::File::open(&disk).unwrap()).unwrap();
    let layer = hangar_layercheck::Ext4Layer::new(&fs, b"upper");

    let older = image(&[
        reg("etc/changed", "old"),
        reg("etc/same", "same"),
        reg("var/log/syslog", "a"),
    ]);
    let newer = image(&[
        reg("etc/changed", "new"),
        reg("etc/same", "same"),
        reg("var/log/syslog", "b"),
    ]);
    let found = compare(&layer, &older, &newer).unwrap();
    assert_eq!(found.conflicts, ["/etc/changed"]);
    assert!(!layer.opaque(b"etc").unwrap(), "no mark");
    std::fs::remove_dir_all(&tmp).unwrap();
}
