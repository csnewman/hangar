//! `hangar oci`: two built trees as an OCI layout, checked by unpacking it
//! the way a worker does, through its own merge into an EROFS image.

use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::Digest;

/// A time with nanoseconds, which a layer has to carry.
fn when() -> SystemTime {
    UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789)
}

fn file(root: &Path, rel: &str, content: &str, at: Option<SystemTime>) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    if let Some(t) = at {
        File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }
}

/// Gives a symlink itself, not what it names, when()'s time.
fn same_time(link: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let t = when().duration_since(UNIX_EPOCH).unwrap();
    let ts = libc::timespec {
        tv_sec: t.as_secs() as libc::time_t,
        tv_nsec: t.subsec_nanos() as _,
    };
    let c = std::ffi::CString::new(link.as_os_str().as_bytes()).unwrap();
    let r = unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            [ts, ts].as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    assert_eq!(r, 0, "{}", std::io::Error::last_os_error());
}

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hangar-cli-oci-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&d).unwrap();
    d
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn blob(layout: &Path, digest: &str) -> PathBuf {
    layout
        .join("blobs/sha256")
        .join(digest.trim_start_matches("sha256:"))
}

fn ungzip(path: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(File::open(path).unwrap())
        .read_to_end(&mut out)
        .unwrap();
    out
}

/// The paths a layer's tar holds, with a hard link's target.
fn entries(tar: &[u8]) -> Vec<(String, Option<String>)> {
    let mut a = tar::Archive::new(tar);
    a.entries()
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let path = String::from_utf8_lossy(&e.path_bytes()).into_owned();
            let link = e
                .header()
                .entry_type()
                .is_hard_link()
                .then(|| String::from_utf8_lossy(&e.link_name_bytes().unwrap()).into_owned());
            (path, link)
        })
        .collect()
}

#[test]
fn built_trees_become_a_layout_a_worker_unpacks() {
    let root = scratch();
    let minimal = root.join("minimal/rootfs");
    let base = root.join("base/rootfs");
    for tree in [&minimal, &base] {
        file(tree, "usr/bin/git", "git", Some(when()));
        file(tree, "usr/lib/libsame.so", "the same library", Some(when()));
        let vi = tree.join("usr/bin/vi");
        std::os::unix::fs::symlink("vim", &vi).unwrap();
        same_time(&vi);
    }
    file(&minimal, "etc/os-release", "minimal", None);
    file(&minimal, "usr/share/gone.txt", "removed by base", None);
    file(
        &minimal,
        "usr/share/gonedir/a.txt",
        "removed with its directory",
        None,
    );
    file(&base, "etc/os-release", "base", None);
    file(&base, "usr/bin/labwc", "labwc", None);
    file(&base, "usr/bin/x", "linked", None);
    fs::hard_link(base.join("usr/bin/x"), base.join("usr/bin/y")).unwrap();
    fs::create_dir_all(base.join("usr/share")).unwrap();

    let layout = root.join("oci");
    let out = Command::new(env!("CARGO_BIN_EXE_hangar"))
        .args(["oci", "-i"])
        .arg(&root)
        .arg("-o")
        .arg(&layout)
        .args(["-arch", "arm64"])
        .output()
        .expect("running hangar oci");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let layout_file = json(&layout.join("oci-layout"));
    assert_eq!(layout_file["imageLayoutVersion"], "1.0.0");
    let index = json(&layout.join("index.json"));
    let mut manifests = std::collections::BTreeMap::new();
    for d in index["manifests"].as_array().unwrap() {
        assert_eq!(d["platform"]["architecture"], "arm64");
        let name = d["annotations"]["org.opencontainers.image.ref.name"]
            .as_str()
            .unwrap()
            .to_string();
        manifests.insert(name, json(&blob(&layout, d["digest"].as_str().unwrap())));
    }
    let layers = |tier: &str| -> Vec<String> {
        manifests[tier]["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["digest"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(layers("minimal").len(), 1);
    assert_eq!(layers("base").len(), 2);
    assert_eq!(
        layers("base")[0],
        layers("minimal")[0],
        "base shares minimal's layer"
    );

    // Each configuration's diff IDs are its layers' tars' digests.
    let config = json(&blob(
        &layout,
        manifests["base"]["config"]["digest"].as_str().unwrap(),
    ));
    for (l, id) in layers("base")
        .iter()
        .zip(config["rootfs"]["diff_ids"].as_array().unwrap())
    {
        let tar = ungzip(&blob(&layout, l));
        let digest = format!("sha256:{:x}", sha2::Sha256::digest(&tar));
        assert_eq!(id.as_str().unwrap(), digest);
    }

    // Base's own layer has what it changes, and only that.
    let top = entries(&ungzip(&blob(&layout, &layers("base")[1])));
    let paths: Vec<&str> = top.iter().map(|(p, _)| p.as_str()).collect();
    for want in [
        "etc/os-release",
        "usr/bin/labwc",
        "usr/share/.wh.gone.txt",
        "usr/share/.wh.gonedir",
    ] {
        assert!(paths.contains(&want), "{want} missing from {paths:?}");
    }
    for unchanged in ["usr/bin/git", "usr/lib/libsame.so", "usr/bin/vi"] {
        assert!(!paths.contains(&unchanged), "{unchanged} is in {paths:?}");
    }
    assert!(
        top.iter()
            .any(|(p, l)| p == "usr/bin/y" && l.as_deref() == Some("usr/bin/x")),
        "{top:?}"
    );

    // Unpacked as a worker unpacks it, top layer first, each image is its
    // tree.
    let unpack = |tier: &str| {
        let image = root.join(format!("{tier}.erofs"));
        let mut tars: Vec<Vec<u8>> = layers(tier)
            .iter()
            .rev()
            .map(|l| ungzip(&blob(&layout, l)))
            .collect();
        tars.reverse();
        hangar_images::build_erofs(&image, || {
            Ok(tars
                .pop()
                .map(|t| Box::new(std::io::Cursor::new(t)) as Box<dyn Read>))
        })
        .expect("building the image");
        hangar_erofs::Image::open(File::open(&image).unwrap()).unwrap()
    };
    let read = |img: &hangar_erofs::Image<File>, p: &str| {
        img.lookup(p.as_bytes(), false)
            .ok()
            .map(|i| String::from_utf8(img.read_all(&i).unwrap()).unwrap())
    };

    let img = unpack("minimal");
    assert_eq!(read(&img, "etc/os-release").as_deref(), Some("minimal"));
    assert_eq!(
        read(&img, "usr/share/gone.txt").as_deref(),
        Some("removed by base")
    );

    let img = unpack("base");
    assert_eq!(read(&img, "etc/os-release").as_deref(), Some("base"));
    assert_eq!(read(&img, "usr/bin/labwc").as_deref(), Some("labwc"));
    assert_eq!(
        read(&img, "usr/lib/libsame.so").as_deref(),
        Some("the same library")
    );
    assert_eq!(read(&img, "usr/share/gone.txt"), None);
    assert!(img.lookup(b"usr/share/gonedir", false).is_err());
    let git = img.lookup(b"usr/bin/git", false).unwrap();
    assert_eq!(img.read_all(&git).unwrap(), b"git");
    assert_eq!((git.mtime, git.nsec), (1_700_000_000, 123_456_789));
    let src = fs::metadata(minimal.join("usr/bin/git")).unwrap();
    assert_eq!(git.perm, src.mode() & 0o7777);
    let vi = img.lookup(b"usr/bin/vi", false).unwrap();
    assert_eq!(img.readlink(&vi).unwrap(), b"vim");
    let (x, y) = (
        img.lookup(b"usr/bin/x", false).unwrap(),
        img.lookup(b"usr/bin/y", false).unwrap(),
    );
    assert_eq!(x.nid, y.nid, "a hard link stays one file");
    assert_eq!(x.nlink, 2);

    let _ = fs::remove_dir_all(&root);
}
