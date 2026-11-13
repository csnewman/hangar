//! The writer held to the Go writer's image of the same tree, byte for byte
//! (internal/erofs/golden_test.go makes it), and the reader reading back
//! everything the tree has.

use std::collections::BTreeMap;

use hangar_erofs::{Entry, Image, Kind, Writer};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    perm: u32,
    uid: u32,
    gid: u32,
    mtime: i64,
    nsec: u32,
    text: Option<String>,
    #[serde(default)]
    fill: u64,
    #[serde(default)]
    target: String,
    #[serde(default)]
    major: u32,
    #[serde(default)]
    minor: u32,
    #[serde(default)]
    xattrs: BTreeMap<String, String>,
}

impl Fixture {
    fn kind(&self) -> Option<Kind> {
        Some(match self.kind.as_str() {
            "file" => Kind::Regular,
            "dir" => Kind::Dir,
            "symlink" => Kind::Symlink,
            "char" => Kind::CharDevice,
            "block" => Kind::BlockDevice,
            "fifo" => Kind::Fifo,
            "socket" => Kind::Socket,
            _ => return None,
        })
    }

    fn data(&self) -> Vec<u8> {
        match &self.text {
            Some(t) => t.as_bytes().to_vec(),
            None => (0..self.fill).map(|i| ((i * 7 + 3) % 251) as u8).collect(),
        }
    }
}

fn fixture() -> Vec<Fixture> {
    serde_json::from_str(include_str!("data/tree.json")).unwrap()
}

/// The fixture's tree, as this writer makes it.
fn image() -> Vec<u8> {
    let mut w = Writer::new(Vec::new());
    for f in fixture() {
        if f.kind == "link" {
            w.link(f.path.as_bytes(), f.target.as_bytes()).unwrap();
            continue;
        }
        let data = f.data();
        let e = Entry {
            path: f.path.as_bytes().to_vec(),
            kind: f.kind(),
            perm: f.perm,
            uid: f.uid,
            gid: f.gid,
            mtime: f.mtime,
            nsec: f.nsec,
            size: data.len() as u64,
            target: f.target.as_bytes().to_vec(),
            major: f.major,
            minor: f.minor,
            xattrs: f
                .xattrs
                .iter()
                .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
                .collect(),
        };
        w.add(&e, &mut data.as_slice()).unwrap();
    }
    w.close().unwrap();
    let size = w.size() as usize;
    let mut out = w.into_inner();
    out.resize(size, 0);
    out
}

#[test]
fn the_image_is_the_go_writers_byte_for_byte() {
    let got = image();
    let want = include_bytes!("data/golden.erofs");
    let first = got.iter().zip(want.iter()).position(|(a, b)| a != b);
    assert!(
        got.len() == want.len() && first.is_none(),
        "{} bytes against {}, first differing at {first:?}",
        got.len(),
        want.len()
    );
}

#[test]
fn the_reader_reads_back_the_tree() {
    let img = Image::open(image()).unwrap();
    for f in fixture() {
        let path = f.path.as_bytes();
        let inode = img
            .lookup(path, false)
            .unwrap_or_else(|e| panic!("{}: {e}", f.path));
        if f.kind == "link" {
            let target = img.lookup(f.target.as_bytes(), false).unwrap();
            assert_eq!(inode.nid, target.nid, "{}: the same inode", f.path);
            assert_eq!(inode.nlink, 2);
            continue;
        }
        assert_eq!(Some(inode.kind), f.kind(), "{}", f.path);
        assert_eq!(inode.perm, f.perm, "{}", f.path);
        assert_eq!((inode.uid, inode.gid), (f.uid, f.gid), "{}", f.path);
        assert_eq!((inode.mtime, inode.nsec), (f.mtime, f.nsec), "{}", f.path);
        match inode.kind {
            Kind::Regular => assert_eq!(img.read_all(&inode).unwrap(), f.data(), "{}", f.path),
            Kind::Symlink => {
                assert_eq!(
                    img.readlink(&inode).unwrap(),
                    f.target.as_bytes(),
                    "{}",
                    f.path
                )
            }
            Kind::CharDevice | Kind::BlockDevice => {
                assert_eq!((inode.major, inode.minor), (f.major, f.minor), "{}", f.path)
            }
            _ => {}
        }
        let want: BTreeMap<Vec<u8>, Vec<u8>> = f
            .xattrs
            .iter()
            .filter(|(k, _)| *k != "system.other")
            .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
            .collect();
        assert_eq!(img.xattrs(&inode).unwrap(), want, "{}", f.path);
    }
}

#[test]
fn directories_list_by_name_and_follow_links() {
    let img = Image::open(image()).unwrap();
    let root = img.root().unwrap();
    let names: Vec<Vec<u8>> = img
        .read_dir(&root)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(names.first().map(Vec::as_slice), Some(&b"!bang"[..]));
    let big = img.lookup(b"big", false).unwrap();
    assert_eq!(img.read_dir(&big).unwrap().len(), 200, "over two blocks");
    let followed = img.lookup(b"link-to-small", true).unwrap();
    assert_eq!(img.read_all(&followed).unwrap(), b"hello, erofs\n");
    assert_eq!(
        img.lookup(b"a/b/../b/c.txt", false)
            .map(|i| i.size)
            .unwrap(),
        5,
        "through .."
    );
    assert!(img.lookup(b"no-such", false).is_err());
}

#[test]
fn what_cannot_be_added_is_refused() {
    let mut w = Writer::new(Vec::new());
    let file = |p: &str| Entry {
        path: p.as_bytes().to_vec(),
        kind: Some(Kind::Regular),
        ..Default::default()
    };
    w.add(&file("x"), &mut std::io::empty()).unwrap();
    assert!(w.add(&file("x"), &mut std::io::empty()).is_err(), "twice");
    assert!(
        w.add(&file("x/y"), &mut std::io::empty()).is_err(),
        "under a file"
    );
    assert!(w.link(b"z", b"missing").is_err(), "a link to nothing");
    let long = "n".repeat(256);
    assert!(
        w.add(&file(&long), &mut std::io::empty()).is_err(),
        "a long name"
    );
    let short = Entry {
        size: 10,
        ..file("short")
    };
    assert!(
        w.add(&short, &mut &b"abc"[..]).is_err(),
        "data shorter than said"
    );
}
