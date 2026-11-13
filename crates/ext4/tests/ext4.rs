//! Filesystems mkfs.ext4 makes of a tree, read back and compared with the
//! tree, across mkfs's options: 32 and 64-bit, 1 KiB blocks, inline data,
//! hashed directories or not, small inodes.

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use hangar_ext4::{Fs, Kind};

/// mkfs.ext4: on PATH, or the one dev/krun-build.sh builds on a Mac.
fn mkfs() -> Option<PathBuf> {
    for p in [
        "mkfs.ext4",
        "/sbin/mkfs.ext4",
        "/usr/sbin/mkfs.ext4",
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../out/krun/bin/mkfs.ext4"),
    ] {
        let found = Command::new(p).arg("-V").output().is_ok();
        if found {
            return Some(PathBuf::from(p));
        }
    }
    None
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| ((i * 13 + i / 1000) as u8) ^ seed).collect()
}

/// A tree to put in a filesystem.
fn source(dir: &Path) {
    let write = |p: &str, data: &[u8], mode: u32| {
        let full = dir.join(p);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, data).unwrap();
        fs::set_permissions(&full, fs::Permissions::from_mode(mode)).unwrap();
    };
    write("etc/empty", b"", 0o644);
    write("etc/small", b"small\n", 0o600);
    write("etc/block", &pattern(4096, 1), 0o644);
    write("etc/big", &pattern((5 << 20) + 33, 2), 0o755);
    write("usr/bin/sudo", b"sudo", 0o4755);
    fs::create_dir_all(dir.join("tmp")).unwrap();
    fs::set_permissions(dir.join("tmp"), fs::Permissions::from_mode(0o1777)).unwrap();
    symlink("../usr/bin/sudo", dir.join("etc/short-link")).unwrap();
    symlink("./".repeat(60) + "small", dir.join("etc/long-link")).unwrap();
    symlink("/etc/small", dir.join("abs-link")).unwrap();
    fs::hard_link(dir.join("etc/small"), dir.join("etc/hard")).unwrap();
    // Enough entries to need several blocks, and a hashed index.
    for i in 0..1500 {
        write(
            &format!("many/entry-{i:04}-{}", "x".repeat(i % 30)),
            i.to_string().as_bytes(),
            0o644,
        );
    }
    let fifo = std::ffi::CString::new(dir.join("etc/fifo").as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is a valid NUL-terminated path.
    unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) };
}

/// A filesystem of dir, made with mkfs.ext4's options.
fn image(mkfs: &Path, dir: &Path, out: &Path, opts: &[&str]) -> Fs<fs::File> {
    let f = fs::File::create(out).unwrap();
    f.set_len(256 << 20).unwrap();
    drop(f);
    let res = Command::new(mkfs)
        .args(["-q", "-F", "-d"])
        .arg(dir)
        .args(opts)
        .arg(out)
        .output()
        .unwrap();
    assert!(
        res.status.success(),
        "mkfs.ext4 {opts:?}: {}",
        String::from_utf8_lossy(&res.stderr)
    );
    Fs::open(fs::File::open(out).unwrap()).unwrap()
}

/// Compares the filesystem with the tree it was made of.
fn check(fsys: &Fs<fs::File>, dir: &Path, rel: &Path) {
    let full = dir.join(rel);
    let meta = fs::symlink_metadata(&full).unwrap();
    let name = rel.as_os_str().as_bytes();
    let inode = fsys
        .lookup(name, false)
        .unwrap_or_else(|e| panic!("{}: {e}", rel.display()));
    // The root's owner and mode are mkfs's, not the tree's.
    if !rel.as_os_str().is_empty() {
        assert_eq!(inode.perm, meta.mode() & 0o7777, "{}", rel.display());
        assert_eq!(
            (inode.uid, inode.gid),
            (meta.uid(), meta.gid()),
            "{}",
            rel.display()
        );
    }
    let ft = meta.file_type();
    if ft.is_file() {
        assert_eq!(inode.kind, Kind::Regular);
        assert_eq!(
            fsys.read_all(&inode).unwrap(),
            fs::read(&full).unwrap(),
            "{}",
            rel.display()
        );
    } else if ft.is_symlink() {
        assert_eq!(
            fsys.readlink(&inode).unwrap(),
            fs::read_link(&full).unwrap().as_os_str().as_bytes(),
            "{}",
            rel.display()
        );
    } else if ft.is_dir() {
        let mut got: Vec<Vec<u8>> = fsys
            .read_dir(&inode)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .filter(|n| !(rel.as_os_str().is_empty() && n == b"lost+found"))
            .collect();
        got.sort();
        let mut want: Vec<Vec<u8>> = fs::read_dir(&full)
            .unwrap()
            .map(|e| e.unwrap().file_name().as_bytes().to_vec())
            .collect();
        want.sort();
        assert_eq!(got, want, "{}", rel.display());
        for n in want {
            check(fsys, dir, &rel.join(std::ffi::OsStr::from_bytes(&n)));
        }
    }
}

#[test]
fn filesystems_read_back_as_they_were_made() {
    let Some(mkfs) = mkfs() else {
        eprintln!("no mkfs.ext4: skipped");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("hangar-ext4-{}", std::process::id()));
    let dir = tmp.join("src");
    source(&dir);
    for (name, opts) in [
        ("defaults", &[][..]),
        ("32-bit", &["-O", "^64bit"][..]),
        ("1 KiB blocks", &["-b", "1024"][..]),
        ("inline data", &["-O", "inline_data"][..]),
        ("no dir index", &["-O", "^dir_index"][..]),
        ("128-byte inodes", &["-I", "128"][..]),
    ] {
        let out = tmp.join(format!("{}.ext4", name.replace(' ', "-")));
        let fsys = image(&mkfs, &dir, &out, opts);
        check(&fsys, &dir, Path::new(""));
        let hard = fsys.lookup(b"etc/hard", false).unwrap();
        let small = fsys.lookup(b"etc/small", false).unwrap();
        assert_eq!(hard.ino, small.ino, "{name}: a hard link is one inode");
        assert_eq!(small.nlink, 2, "{name}");
        assert_eq!(
            fsys.lookup(b"abs-link", true).unwrap().ino,
            small.ino,
            "{name}: an absolute link resolves in the filesystem's root"
        );
        assert_eq!(
            fsys.read_all(&fsys.lookup(b"etc/short-link", true).unwrap())
                .unwrap(),
            b"sudo",
            "{name}: through a link"
        );
        assert!(
            fsys.lookup(b"etc/small/x", false).is_err(),
            "{name}: through a file"
        );
        let fifo = fsys.lookup(b"etc/fifo", false).unwrap();
        assert_eq!(fifo.kind, Kind::Fifo, "{name}");
        fs::remove_file(&out).unwrap();
    }
    fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn what_is_not_ext4_is_refused() {
    assert!(Fs::open(&[0u8; 4096][..]).is_err());
}

/// What overlayfs marks in an upper directory: a whiteout, a character
/// device 0:0, and an opaque directory, marked by a trusted attribute. Both
/// need root to make, on Linux.
#[test]
fn overlays_marks_read_back() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("making whiteouts and trusted attributes needs root: skipped");
            return;
        }
        let Some(mkfs) = mkfs() else {
            eprintln!("no mkfs.ext4: skipped");
            return;
        };
        let tmp = std::env::temp_dir().join(format!("hangar-ext4-marks-{}", std::process::id()));
        let dir = tmp.join("src");
        fs::create_dir_all(dir.join("upper/opaque")).unwrap();
        let c = |p: &str| std::ffi::CString::new(dir.join(p).as_os_str().as_bytes()).unwrap();
        // SAFETY: valid paths and buffers throughout.
        unsafe {
            assert_eq!(
                libc::mknod(c("upper/gone").as_ptr(), libc::S_IFCHR | 0o600, 0),
                0
            );
            assert_eq!(
                libc::mknod(
                    c("upper/tty").as_ptr(),
                    libc::S_IFCHR | 0o620,
                    libc::makedev(4, 300)
                ),
                0
            );
            let opaque = c("upper/opaque");
            let y = b"y";
            assert_eq!(
                libc::setxattr(
                    opaque.as_ptr(),
                    c"trusted.overlay.opaque".as_ptr(),
                    y.as_ptr().cast(),
                    1,
                    0
                ),
                0
            );
            let big = vec![b'v'; 3000];
            libc::setxattr(
                opaque.as_ptr(),
                c"user.big".as_ptr(),
                big.as_ptr().cast(),
                big.len(),
                0,
            );
        }
        let fsys = image(&mkfs, &dir, &tmp.join("fs.ext4"), &[]);
        let gone = fsys.lookup(b"upper/gone", false).unwrap();
        assert_eq!(
            (gone.kind, gone.major, gone.minor),
            (Kind::CharDevice, 0, 0),
            "the whiteout"
        );
        let tty = fsys.lookup(b"upper/tty", false).unwrap();
        assert_eq!((tty.major, tty.minor), (4, 300), "a device");
        let attrs = fsys
            .xattrs(&fsys.lookup(b"upper/opaque", false).unwrap())
            .unwrap();
        assert_eq!(
            attrs.get(&b"trusted.overlay.opaque"[..]).map(Vec::as_slice),
            Some(&b"y"[..])
        );
        assert_eq!(
            attrs.get(&b"user.big"[..]).map(Vec::len),
            Some(3000),
            "in the attribute block"
        );
        fs::remove_dir_all(&tmp).unwrap();
    }
}
