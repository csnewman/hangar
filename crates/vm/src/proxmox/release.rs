//! A worker VM's release: the ISO it boots, which its environments boot
//! too, and moving to the release the server names. The worker VM's agent
//! runs nothing from the ISO once it has started (it copies the worker onto
//! the data disk), so the ISO in its CD-ROM can be changed as it runs,
//! which Proxmox does the moment the VM's config says another.

use std::io;
use std::time::{Duration, Instant};

use hangar_api::{RELEASE_ISO_FILE, Release};
use hangar_images::{Client as Images, Reference};

use super::{Pve, RELEASE_SLOT, client::enc, short_hash};
use crate::err;

/// Where the worker VM's SMBIOS UUID is, which its config's smbios1 has.
const PRODUCT_UUID: &str = "/sys/class/dmi/id/product_uuid";

/// Names the release a worker VM was moved to, in the worker's directory,
/// until it runs as that release: one that boots and runs as another (its
/// worker crashing, and the agent running the one before) is not tried
/// again, which would reboot the VM over and over.
const MOVING_TO: &str = "moving-to-release";

/// How long a release that could not be fetched or put in place waits to
/// be tried again.
const RETRY_AFTER: Duration = Duration::from_secs(3600);

impl Pve {
    /// The worker VM's VMID: the VM of the place's (in its pool, with its
    /// prefix's tag) on its node whose SMBIOS UUID is this machine's.
    pub async fn own_vm(&self) -> io::Result<u32> {
        if !self.cfg.worker_vm {
            return Err(err("the worker is not configured to run in a worker VM"));
        }
        self.own
            .get_or_try_init(|| async {
                let uuid = std::fs::read_to_string(PRODUCT_UUID)
                    .map_err(|e| err(format!("this machine's SMBIOS UUID: {e}")))?
                    .trim()
                    .to_ascii_lowercase();
                for (node, id) in self.own_vms().await? {
                    if node != self.cfg.node {
                        continue;
                    }
                    let conf = self
                        .client
                        .get(&format!("/nodes/{}/qemu/{id}/config", enc(&node)))
                        .await?;
                    let theirs = conf["smbios1"]
                        .as_str()
                        .and_then(|s| s.split(',').find_map(|p| p.strip_prefix("uuid=")))
                        .map(str::to_ascii_lowercase);
                    if theirs.as_deref() == Some(uuid.as_str()) {
                        return Ok(id as u32);
                    }
                }
                Err(err(format!(
                    "no VM on node {} in the place's pool with its prefix's tag has this machine's SMBIOS UUID: the worker VM must be one",
                    self.cfg.node
                )))
            })
            .await
            .copied()
    }

    /// The release ISO the worker VM boots, by its volume's ID.
    pub(super) async fn own_release(&self) -> io::Result<String> {
        let id = self.own_vm().await?;
        let conf = self.client.get(&self.vm(id, "config")).await?;
        conf[RELEASE_SLOT]
            .as_str()
            .and_then(|s| s.split(',').next())
            .filter(|v| v.contains(":iso/"))
            .map(str::to_string)
            .ok_or_else(|| err(format!("the worker VM boots no ISO from {RELEASE_SLOT}")))
    }

    /// Moves the worker VM to release when it runs another (running, the
    /// worker's own build), once at a time: see `upgrade`. A release the VM
    /// was moved to and does not run as is not tried again, and one that
    /// failed waits RETRY_AFTER.
    pub async fn follow(&self, release: &Release, running: &str, images: Images) {
        if !self.cfg.worker_vm || release.version.is_empty() {
            return;
        }
        let Ok(mut following) = self.following.try_lock() else {
            return;
        };
        let moving = self.dir.join(MOVING_TO);
        let moved_to = std::fs::read_to_string(&moving).ok();
        if release.version == running {
            if moved_to.is_some() {
                let _ = std::fs::remove_file(&moving);
                self.log.info(
                    "the worker VM runs its new release",
                    &[("version", &running)],
                );
            }
            return;
        }
        if moved_to.as_deref().map(str::trim) == Some(release.version.as_str()) {
            if !self
                .said_stuck
                .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                self.log.warn(
                    "the worker VM was moved to the server's release and runs another; it is not moved again until the server names another",
                    &[("release", &release.version), ("running", &running)],
                );
            }
            return;
        }
        if let Some((v, at)) = following.as_ref()
            && *v == release.version
            && at.elapsed() < RETRY_AFTER
        {
            return;
        }
        *following = Some((release.version.clone(), Instant::now()));
        self.log.info(
            "moving the worker VM to the server's release",
            &[("from", &running), ("to", &release.version)],
        );
        let _ = std::fs::create_dir_all(&self.dir);
        if let Err(e) = std::fs::write(&moving, &release.version) {
            self.log.warn(
                "could not record the release the worker VM is moved to; not moving it",
                &[("err", &e)],
            );
            return;
        }
        match self.upgrade(release, images).await {
            Ok(()) => self.log.info(
                "the worker VM reboots into its new release",
                &[("release", &release.version)],
            ),
            Err(e) => {
                let _ = std::fs::remove_file(&moving);
                self.log.warn(
                    "could not move the worker VM to the server's release",
                    &[("release", &release.version), ("err", &e)],
                );
            }
        }
    }

    /// Puts release's ISO in the worker VM's boot CD-ROM and has Proxmox
    /// reboot it: the ISO fetched from the image the release names
    /// (`RELEASE_ISO_FILE`) and uploaded under the place's prefix, unless
    /// it is there already. The reboot shuts the VM down through its agent,
    /// which stops the worker, leaving environments running for the next
    /// to take back.
    async fn upgrade(&self, release: &Release, images: Images) -> io::Result<()> {
        let id = self.own_vm().await?;
        let name = self.iso_name("release", &release_key(&release.version));
        let volid = self.iso_volid(&name);
        if self.isos().await?.contains(&volid) {
            self.seen
                .lock()
                .unwrap()
                .insert(name.clone(), Instant::now());
        } else {
            let (file, scratch) = (self.dir.join(&name), self.dir.join("release-blobs"));
            let r = Reference::parse(&release.iso_image)
                .map_err(|e| err(format!("release image {}: {e}", release.iso_image)))?;
            let (f, s) = (file.clone(), scratch.clone());
            let fetched = tokio::task::spawn_blocking(move || {
                let digest = images.resolve(&r)?;
                images.fetch_file(&r, &digest, RELEASE_ISO_FILE, &s, &f)
            })
            .await
            .map_err(io::Error::other)?;
            let _ = std::fs::remove_dir_all(&scratch);
            fetched.map_err(|e| err(format!("fetching {}: {e}", release.iso_image)))?;
            let uploaded = self.ensure_iso(&file, &name).await;
            let _ = std::fs::remove_file(&file);
            uploaded?;
        }
        let cdrom = format!("{volid},media=cdrom");
        self.client
            .put(&self.vm(id, "config"), &[(RELEASE_SLOT, cdrom.as_str())])
            .await?;
        self.client
            .post(&self.vm(id, "status/reboot"), &[])
            .await
            .map(|_| ())
    }
}

/// What a release's ISO is named by: a version that is a commit's hash, as
/// published releases' are, by its first 12 digits; another made of what a
/// file name takes, as it is; anything else by its hash.
fn release_key(version: &str) -> String {
    let v = version.to_ascii_lowercase();
    if v.len() >= 12 && v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return v[..12].to_string();
    }
    if (1..=32).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
    {
        return v;
    }
    short_hash(version)
}
