//! An environment is pinned to the copy of its image its writable layer
//! was made over. Upgrading re-pins it to a newer copy, keeping its disks:
//! the files it has changed stay as they are, over the newer image. The
//! writable disk is copied first, into ROLLBACK_DIR, and going back
//! restores that copy and the old pin. Docker's store, on the same disk
//! beside the layer, goes with it into the copy.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use hangar_api::{
    EnvironmentSpec, ImageCopy, ImageRollback, ImageUpdate, ObservedEnvironment, Phase, Step,
};
use serde::{Deserialize, Serialize};

use super::{IMAGE_PIN_FILE, Machine, Stop, store_copy};
use crate::err;
use crate::suspend::SUSPENDED_MARK;

/// In an environment's directory, holds the copy of its writable disk from
/// before its last upgrade and the pin it had then.
const ROLLBACK_DIR: &str = "rollback";

/// How many of the files an upgrade would leave hiding the image's are
/// listed; the rest are counted.
const MAX_CONFLICTS: usize = 50;

/// The record kept in ROLLBACK_DIR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Rollback {
    #[serde(rename = "ref")]
    pub reference: String,
    pub digest: String,
    /// RFC 3339.
    pub at: String,
    pub size_bytes: i64,
}

impl Rollback {
    pub fn copy(&self) -> ImageCopy {
        ImageCopy {
            r#ref: self.reference.clone(),
            digest: self.digest.clone(),
        }
    }
}

pub(crate) fn read_rollback(dir: &Path) -> Option<Rollback> {
    let b = std::fs::read(dir.join(ROLLBACK_DIR).join(IMAGE_PIN_FILE)).ok()?;
    let r: Rollback = serde_json::from_slice(&b).ok()?;
    if r.reference.is_empty() || !dir.join(ROLLBACK_DIR).join("upper.ext4").exists() {
        return None;
    }
    Some(r)
}

impl Machine {
    /// What the environment reports about its image: the pin, a newer copy
    /// to upgrade to and what upgrading would hide, and the rollback kept
    /// from its last upgrade. Checking what an upgrade would hide is
    /// started here, for a stopped environment, since it reads the writable
    /// disk.
    pub(super) fn image_state(self: &Arc<Self>, o: &mut ObservedEnvironment) {
        let Some(rt) = self.rt() else { return };
        let (pin, rb, update, from, phase, checking) = {
            let st = self.st.lock().unwrap();
            (
                st.image.clone(),
                st.rollback.clone(),
                st.update.clone(),
                st.update_from.clone(),
                st.phase,
                st.checking,
            )
        };
        let Some(pin) = pin else { return };
        o.image_pinned = true;
        if let Some(rb) = rb {
            o.image_rollback = Some(ImageRollback {
                digest: rb.digest,
                at: rb.at,
                size_bytes: rb.size_bytes,
            });
        }
        let Some(latest) = rt.store.latest(&pin.r#ref) else {
            return;
        };
        if latest == pin.digest || !self.has_disks() {
            return;
        }
        if let Some(u) = update
            && u.digest == latest
            && from == pin.digest
        {
            o.image_update = Some(u);
            return;
        }
        o.image_update = Some(ImageUpdate {
            digest: latest.clone(),
            ..Default::default()
        });
        if matches!(phase, Phase::Stopped | Phase::Failed) && !checking {
            self.st.lock().unwrap().checking = true;
            let (m, to) = (
                self.clone(),
                ImageCopy {
                    r#ref: pin.r#ref.clone(),
                    digest: latest,
                },
            );
            tokio::spawn(async move {
                m.check_update(pin, to).await;
                m.st.lock().unwrap().checking = false;
            });
        }
    }

    /// Compares the environment's writable layer with the copy of its
    /// image it is pinned to and a newer one, while it is stopped. It
    /// stands down if the environment starts first. The newer copy is
    /// fetched before the writable disk is taken, so a long pull holds up
    /// no start.
    async fn check_update(self: &Arc<Self>, from: ImageCopy, to: ImageCopy) {
        let Some(rt) = self.rt() else { return };
        let checked = |u: ImageUpdate| {
            let mut st = self.st.lock().unwrap();
            st.update = Some(u);
            st.update_from = from.digest.clone();
        };
        let failed = |e: &dyn std::fmt::Display| ImageUpdate {
            digest: to.digest.clone(),
            checked: true,
            error: e.to_string(),
            ..Default::default()
        };
        let (store, sc) = (rt.store.clone(), store_copy(&to));
        if let Err(e) = tokio::task::spawn_blocking(move || store.get(&sc, &mut |_| {}))
            .await
            .map_err(io::Error::other)
            .and_then(|r| r)
        {
            self.log.warn(
                "fetching a newer copy of the image",
                &[("image", &to.r#ref), ("err", &e)],
            );
            checked(failed(&e));
            rt.notify();
            return;
        }
        let Ok(_disk) = self.disk.try_lock() else {
            return;
        };
        if !matches!(
            self.st.lock().unwrap().phase,
            Phase::Stopped | Phase::Failed
        ) {
            return;
        }
        let (store, dir, f, t) = (
            rt.store.clone(),
            self.dir.clone(),
            store_copy(&from),
            store_copy(&to),
        );
        let compared = tokio::task::spawn_blocking(move || compare_images(&store, &dir, &f, &t))
            .await
            .map_err(io::Error::other)
            .and_then(|r| r);
        let u = match compared {
            Ok(found) => {
                let mut u = ImageUpdate {
                    digest: to.digest.clone(),
                    checked: true,
                    packages: found.packages,
                    ..Default::default()
                };
                let mut conflicts = found.conflicts;
                if conflicts.len() > MAX_CONFLICTS {
                    u.more_conflicts = (conflicts.len() - MAX_CONFLICTS) as i64;
                    conflicts.truncate(MAX_CONFLICTS);
                }
                u.conflicts = conflicts;
                u
            }
            Err(e) => {
                self.log.warn(
                    "comparing the writable layer with a newer copy of the image",
                    &[("image", &to.r#ref), ("err", &e)],
                );
                failed(&e)
            }
        };
        checked(u);
        rt.notify();
    }

    /// Re-pins a stopped environment as its spec asks, before it boots:
    /// back to the copy its rollback was taken from, restoring that
    /// writable disk, or on to another copy, keeping a copy of the writable
    /// disk to roll back to.
    pub(super) async fn change_image(self: &Arc<Self>, spec: &EnvironmentSpec) -> Result<(), Stop> {
        let (pin, rb) = {
            let mut st = self.st.lock().unwrap();
            st.update = None;
            (st.image.clone(), st.rollback.clone())
        };
        let (Some(want_digest), Some(pin)) = (&spec.pin_digest, pin) else {
            return Ok(());
        };
        if *want_digest == pin.digest {
            return Ok(());
        }
        // A suspended machine's memory refers to the files it had.
        if self.dir.join(SUSPENDED_MARK).exists() {
            return Ok(());
        }
        let want = ImageCopy {
            r#ref: pin.r#ref.clone(),
            digest: want_digest.clone(),
        };
        if let Some(rb) = rb
            && rb.copy() == want
        {
            return self.restore_rollback(&rb).map_err(Stop::Failed);
        }
        let rt = self.rt().ok_or(Stop::Cancelled)?;
        let (_, never) = tokio::sync::watch::channel(false);
        self.fetch(&rt, &want, &never).await?;
        self.snapshot(&pin)
            .await
            .map_err(|e| err(format!("copying its writable disk to roll back to: {e}")))?;
        self.write_pin(&want)?;
        self.log.info(
            "upgraded the image",
            &[
                ("image", &want.r#ref),
                ("from", &pin.digest),
                ("to", &want.digest),
            ],
        );
        Ok(())
    }

    /// Copies the writable disk into ROLLBACK_DIR, with the pin it was made
    /// over, replacing any rollback from an earlier upgrade.
    async fn snapshot(self: &Arc<Self>, pin: &ImageCopy) -> io::Result<()> {
        let dst = self.dir.join(ROLLBACK_DIR);
        let tmp = self.dir.join(format!("{ROLLBACK_DIR}.tmp"));
        remove_all(&tmp)?;
        // The rollback being replaced is one upgrade further back, and would
        // double the space needed.
        remove_all(&dst)?;
        self.st.lock().unwrap().rollback = None;
        std::fs::create_dir_all(&tmp)?;
        self.step_to(Step::Snapshot, "copying its writable disk to roll back to");
        let (src, out, me) = (
            self.dir.join("upper.ext4"),
            tmp.join("upper.ext4"),
            self.clone(),
        );
        let copied = tokio::task::spawn_blocking(move || {
            copy_sparse(&src, &out, &mut |done, total| {
                me.measure_to(done, total, "bytes")
            })
        })
        .await
        .map_err(io::Error::other)
        .and_then(|r| r);
        let size = match copied {
            Ok(n) => n,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(e);
            }
        };
        let rb = Rollback {
            reference: pin.r#ref.clone(),
            digest: pin.digest.clone(),
            at: hangar_api::rfc3339(SystemTime::now()),
            size_bytes: size as i64,
        };
        let written = std::fs::write(
            tmp.join(IMAGE_PIN_FILE),
            serde_json::to_vec(&rb).map_err(io::Error::other)?,
        )
        .and_then(|_| std::fs::rename(&tmp, &dst));
        if let Err(e) = written {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        self.st.lock().unwrap().rollback = Some(rb);
        Ok(())
    }

    /// Puts back the writable disk and pin kept from before the last
    /// upgrade, dropping what was written since.
    fn restore_rollback(&self, rb: &Rollback) -> io::Result<()> {
        self.step_to(
            Step::Disks,
            "restoring the writable disk from before the upgrade",
        );
        std::fs::rename(
            self.dir.join(ROLLBACK_DIR).join("upper.ext4"),
            self.dir.join("upper.ext4"),
        )
        .map_err(|e| err(format!("restoring its writable disk: {e}")))?;
        self.write_pin(&rb.copy())?;
        self.st.lock().unwrap().rollback = None;
        if let Err(e) = std::fs::remove_dir_all(self.dir.join(ROLLBACK_DIR)) {
            self.log.warn("removing the rollback", &[("err", &e)]);
        }
        self.log.info(
            "rolled the image back",
            &[("image", &rb.reference), ("to", &rb.digest)],
        );
        if let Some(rt) = self.rt() {
            rt.prune();
            rt.notify();
        }
        Ok(())
    }

    /// Pins the environment to c.
    fn write_pin(&self, c: &ImageCopy) -> io::Result<()> {
        crate::suspend::write_atomic(
            &self.dir.join(IMAGE_PIN_FILE),
            &serde_json::to_vec(c).map_err(io::Error::other)?,
        )?;
        self.st.lock().unwrap().image = Some(c.clone());
        self.notify();
        Ok(())
    }

    /// Discards the rollback when the spec asks, keeping the upgrade, so
    /// the copy of the image it was taken from can go.
    pub(super) async fn drop_rollback(&self, spec: &EnvironmentSpec) {
        let rb = self.st.lock().unwrap().rollback.clone();
        let Some(rb) = rb else { return };
        if !spec.drop_rollback {
            return;
        }
        let removed = {
            let _disk = self.disk.lock().await;
            remove_all(&self.dir.join(ROLLBACK_DIR))
        };
        if let Err(e) = removed {
            self.log.warn("discarding the rollback", &[("err", &e)]);
            return;
        }
        self.st.lock().unwrap().rollback = None;
        self.log.info(
            "kept the upgrade and discarded its rollback",
            &[("image", &rb.reference)],
        );
        if let Some(rt) = self.rt() {
            rt.prune();
            rt.notify();
        }
    }
}

fn remove_all(p: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(p) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// What the writable layer in dir would hide of the change from one copy
/// to the other. Each copy is read through its EROFS image, made for one
/// that has not booted yet, and the writable disk as it is, unmounted.
fn compare_images(
    store: &hangar_images::Store,
    dir: &Path,
    from: &hangar_images::ImageCopy,
    to: &hangar_images::ImageCopy,
) -> io::Result<hangar_layercheck::Found> {
    let open = |c: &hangar_images::ImageCopy,
                what: &str|
     -> io::Result<hangar_erofs::Image<std::fs::File>> {
        let img = store
            .get(c, &mut |_| {})
            .and_then(|i| store.erofs(&i))
            .and_then(std::fs::File::open)
            .and_then(hangar_erofs::Image::open);
        img.map_err(|e| err(format!("{what}: {e}")))
    };
    let older = open(from, "the copy it is pinned to")?;
    let newer = open(to, "the newer copy")?;
    let disk = std::fs::File::open(dir.join("upper.ext4"))
        .and_then(hangar_ext4::Fs::open)
        .map_err(|e| err(format!("its writable disk: {e}")))?;
    let layer = hangar_layercheck::Ext4Layer::new(&disk, b"upper");
    hangar_layercheck::compare(&layer, &older, &newer)
}

/// Copies a sparse disk image, only the parts that hold data, reporting how
/// much of that is done, and gives the bytes copied.
#[cfg(target_os = "linux")]
pub fn copy_sparse(src: &Path, dst: &Path, progress: &mut dyn FnMut(i64, i64)) -> io::Result<u64> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let input = std::fs::File::open(src)?;
    let meta = input.metadata()?;
    let out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(dst)?;
    out.set_len(meta.size())?;
    let (fi, fo) = (input.as_raw_fd(), out.as_raw_fd());
    let total = (meta.blocks() * 512) as i64;
    let size = meta.size() as i64;
    let mut done: i64 = 0;
    const CHUNK: i64 = 64 << 20;
    let mut off: i64 = 0;
    while off < size {
        // SAFETY: lseek on an open descriptor.
        let data = unsafe { libc::lseek(fi, off, libc::SEEK_DATA) };
        if data < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ENXIO) {
                break;
            }
            return Err(e);
        }
        // SAFETY: lseek on an open descriptor.
        let hole = unsafe { libc::lseek(fi, data, libc::SEEK_HOLE) };
        if hole < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut at = data;
        while at < hole {
            let (mut r, mut w) = (at, at);
            // SAFETY: both descriptors are open, and the offsets are ours.
            let n = unsafe {
                libc::copy_file_range(fi, &mut r, fo, &mut w, (hole - at).min(CHUNK) as usize, 0)
            };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            // Gigabytes through the page cache would fragment the host's
            // free memory, and guests started meanwhile would find no 2 MB
            // blocks for theirs, so each chunk is written out and dropped
            // from it.
            // SAFETY: calls on open descriptors over the range just copied.
            unsafe {
                if libc::sync_file_range(
                    fo,
                    at,
                    n as i64,
                    libc::SYNC_FILE_RANGE_WAIT_BEFORE
                        | libc::SYNC_FILE_RANGE_WRITE
                        | libc::SYNC_FILE_RANGE_WAIT_AFTER,
                ) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                libc::posix_fadvise(fo, at, n as i64, libc::POSIX_FADV_DONTNEED);
                libc::posix_fadvise(fi, at, n as i64, libc::POSIX_FADV_DONTNEED);
            }
            at += n as i64;
            done += n as i64;
            progress(done, total.max(done));
        }
        off = hole;
    }
    out.sync_all()?;
    progress(done, done);
    Ok(done as u64)
}

/// clonefile's flag from sys/clonefile.h: a link is cloned as itself.
#[cfg(target_os = "macos")]
const CLONE_NOFOLLOW: u32 = 0x0001;

/// Clones src to dst on APFS: the copy shares src's blocks until either is
/// written, so it takes no time or space to make.
#[cfg(target_os = "macos")]
pub fn copy_sparse(src: &Path, dst: &Path, progress: &mut dyn FnMut(i64, i64)) -> io::Result<u64> {
    let size = std::fs::metadata(src)?.len();
    let (s, d) = (crate::fs_cstr(src)?, crate::fs_cstr(dst)?);
    // SAFETY: both are valid NUL-terminated paths.
    if unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), CLONE_NOFOLLOW) } < 0 {
        return Err(err(format!(
            "cloning {}: {}",
            src.display(),
            io::Error::last_os_error()
        )));
    }
    progress(size as i64, size as i64);
    Ok(size)
}
