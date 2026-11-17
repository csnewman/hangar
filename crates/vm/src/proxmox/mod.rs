//! Environments as Proxmox VE VMs (`runtime: proxmox`): the worker runs
//! beside Proxmox, in a VM of its own on one of its nodes, and asks its API
//! for a VM per environment. Design and measurements: docs/proxmox-vms.md.
//!
//! What the VMs read and never write is in ISOs, which any number of VMs
//! attach as CD-ROMs at once, with no copy: a release ISO of the worker's
//! kernel and agent (the initramfs, with mke2fs beside it), which every VM
//! boots, and an ISO per image holding its EROFS, which the agent's init
//! mounts as the root's base. An environment's VM is made with the two as
//! CD-ROMs, an empty writable disk (its filesystem made by the agent at its
//! first boot) and a swap disk; each cold start points it at the current
//! release, so it boots the worker's own kernel and agent. Each boot, the
//! VM's SMBIOS serial says where the worker's hub is, and the hub's secret
//! for the boot is written into the guest through its agent channel
//! (`hangar-agent`'s qga), never into the VM's configuration, which anyone
//! who may see the VM can read.
//!
//! Places may share a Proxmox. Everything the worker makes is named with its
//! place's prefix, its VMs carry the prefix as a tag, and the worker removes
//! only what has it.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::err;
use crate::log::Log;

pub mod client;
pub mod console;

use client::{Client, enc};

/// Where the agent's init finds each disk, by the serial Proxmox gives it.
const UPPER_SERIAL: &str = "hangar-upper";
const SWAP_SERIAL: &str = "hangar-swap";

/// The CD-ROMs' slots: a VM's image on scsi0, its release on scsi1, which it
/// boots from. Proxmox puts scsiN at LUN N, which the agent's init finds the
/// image by (`scsi:0`): a CD-ROM's serial does not reach the guest.
const IMAGE_SLOT: &str = "scsi0";
const RELEASE_SLOT: &str = "scsi1";

/// The kernel command line every VM boots with, in the release ISO.
/// Headless: a Proxmox VM has no virtual GPU of Hangar's. The guest puts its
/// own firewall up: the VM is on its place's network, among other machines.
/// Its hub settings come through the guest agent channel (`hangar.hub`). A
/// worker VM boots the same, its agent finding a data disk where the
/// writable disk would be.
pub const CMDLINE: &str = "console=ttyS0 systemd.show_status=1 mitigations=off \
    memory_hotplug.online_policy=auto-movable \
    hangar.upper=serial:hangar-upper hangar.image=scsi:0 \
    hangar.swap=serial:hangar-swap hangar.firewall=1 hangar.hub=channel \
    systemd.unit=multi-user.target systemd.mask=hangar-desktop.service";

/// The tag every VM the worker makes carries, beside its place's prefix.
const TAG: &str = "hangar";

/// How long an ISO the worker has just uploaded or found is kept from the
/// sweep though no VM names it yet: a VM being made may be about to.
const ISO_GRACE: Duration = Duration::from_secs(3600);

/// What a Proxmox place is: its API, and where VMs go.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub url: String,
    /// USER@REALM!ID=SECRET.
    pub token: String,
    /// The node certificate's SHA-256 fingerprint; empty takes one the
    /// system's roots vouch for.
    pub fingerprint: String,
    /// The node the worker makes VMs on: its own.
    pub node: String,
    /// Where VMs' disks are.
    pub storage: String,
    /// Where the release and image ISOs go, a storage holding iso content;
    /// empty is storage.
    pub iso_storage: String,
    /// The bridge or VNet environments' NICs join.
    pub bridge: String,
    /// The pool every VM joins; empty is none.
    pub pool: String,
    /// What the names of everything the worker makes begin with, and the
    /// tag its VMs carry: its place's own (check_prefix).
    pub prefix: String,
    /// mke2fs, static, which the release ISO's initramfs carries for the
    /// agent to make a new writable disk's filesystem with.
    pub mke2fs: PathBuf,
    /// The release ISO environments boot, by its volume's ID, as one is
    /// published with each release; empty, the worker builds and uploads
    /// its own (release).
    pub release_iso: String,
}

/// Whether prefix can name a place's things on Proxmox: 1 to 20 lowercase
/// letters, digits and hyphens, starting with a letter and not ending with
/// a hyphen, which VM names, tags and storage file names all take.
pub fn check_prefix(prefix: &str) -> Result<(), String> {
    let ok = (1..=20).contains(&prefix.len())
        && prefix.starts_with(|c: char| c.is_ascii_lowercase())
        && !prefix.ends_with('-')
        && prefix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "proxmox prefix {prefix:?}: 1 to 20 lowercase letters, digits and hyphens, \
             starting with a letter and not ending with a hyphen"
        ))
    }
}

/// One Proxmox node, as the worker uses it.
/// The RAM a VM boots with under memory hotplug, which it always has
/// (qemu-server's static memory).
pub const STATIC_MIB: u32 = 1024;

/// Every memory size a VM can have under hotplug: STATIC_MIB, then whole
/// DIMMs, 512 MiB each for the first 32 and each 32 after twice the size of
/// the 32 before (qemu-server's foreach_dimm), to its last.
fn memory_sizes() -> impl Iterator<Item = u32> {
    (0..8 * 32u32).scan(STATIC_MIB, |at, i| {
        *at += 512 << (i / 32);
        Some(*at)
    })
}

/// The memory size a VM can have nearest mib: mib rounded up to whole
/// DIMMs, at least STATIC_MIB and at most the largest size within max.
pub fn fit_memory(mib: u32, max: u32) -> u32 {
    let cap = std::iter::once(STATIC_MIB)
        .chain(memory_sizes())
        .take_while(|&s| s <= max.max(STATIC_MIB))
        .last()
        .unwrap_or(STATIC_MIB);
    if mib <= STATIC_MIB {
        return STATIC_MIB;
    }
    memory_sizes().find(|&s| s >= mib).unwrap_or(cap).min(cap)
}

/// What Proxmox measures of a running VM.
#[derive(Debug, Clone, Copy)]
pub struct VmStats {
    /// Of its vCPUs together.
    pub cpu_percent: f64,
    /// The VM's process on the host, in bytes.
    pub host_memory: u64,
    /// What the storage holds of its writable and swap disks, in bytes,
    /// when asked.
    pub upper_used: Option<u64>,
    pub swap_used: Option<u64>,
}

pub struct Pve {
    client: Client,
    cfg: Config,
    /// Where the worker builds what it uploads.
    dir: PathBuf,
    /// The release ISO VMs boot, by name, once the worker has made one.
    release: std::sync::Mutex<Option<String>>,
    /// The ISOs the worker has uploaded or found lately, by name, when:
    /// the sweep leaves them for ISO_GRACE.
    seen: std::sync::Mutex<HashMap<String, Instant>>,
    /// One upload of a file at a time.
    uploading: tokio::sync::Mutex<()>,
    log: Log,
}

/// What a VM's agent needs besides its secret, in its SMBIOS serial.
pub type Placed = hangar_hub::Settings;

impl Pve {
    pub fn new(cfg: Config, dir: &Path, log: Log) -> io::Result<Pve> {
        check_prefix(&cfg.prefix).map_err(err)?;
        let fp = (!cfg.fingerprint.is_empty()).then_some(cfg.fingerprint.as_str());
        let client = Client::new(&cfg.url, &cfg.token, fp)?;
        Ok(Pve {
            client,
            cfg,
            dir: dir.to_path_buf(),
            release: Default::default(),
            seen: Default::default(),
            uploading: Default::default(),
            log,
        })
    }

    fn node(&self) -> String {
        enc(&self.cfg.node)
    }

    fn iso_storage(&self) -> &str {
        if self.cfg.iso_storage.is_empty() {
            &self.cfg.storage
        } else {
            &self.cfg.iso_storage
        }
    }

    /// What a file of this place's in the ISO storage is named, from its
    /// kind and key.
    fn iso_name(&self, kind: &str, key: &str) -> String {
        format!("{}-{kind}-{key}.iso", self.cfg.prefix)
    }

    fn iso_volid(&self, name: &str) -> String {
        format!("{}:iso/{name}", self.iso_storage())
    }

    /// Checks the API answers and the node is there.
    pub async fn check(&self) -> io::Result<String> {
        let v = self.client.get("/version").await?;
        self.client
            .get(&format!("/nodes/{}/status", self.node()))
            .await
            .map_err(|e| err(format!("proxmox node {}: {e}", self.cfg.node)))?;
        Ok(v["version"].as_str().unwrap_or_default().to_string())
    }

    /// The node's CPUs and memory, which its VMs share.
    pub async fn resources(&self) -> io::Result<hangar_api::Resources> {
        let st = self
            .client
            .get(&format!("/nodes/{}/status", self.node()))
            .await?;
        Ok(hangar_api::Resources {
            cpus: st["cpuinfo"]["cpus"].as_i64().unwrap_or(1),
            memory_mib: st["memory"]["total"].as_i64().unwrap_or(0) >> 20,
        })
    }

    async fn wait(&self, upid: &Value, within: Duration) -> io::Result<()> {
        match upid.as_str() {
            Some(u) if !u.is_empty() => self.client.wait(&self.cfg.node, u, within).await,
            _ => Ok(()),
        }
    }

    /// A free VMID.
    async fn next_id(&self) -> io::Result<u32> {
        let v = self.client.get("/cluster/nextid").await?;
        v.as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| v.as_u64().map(|n| n as u32))
            .ok_or_else(|| err(format!("proxmox gave no free VMID: {v}")))
    }

    /// The release ISO, which VMs boot: the one configured (release_iso),
    /// or the worker's kernel, and its agent as the initramfs with mke2fs
    /// beside it, booting with CMDLINE, made and uploaded once per kernel,
    /// agent and mke2fs. Its volume's ID.
    pub async fn release(&self, kernel: &Path, agent: &Path) -> io::Result<String> {
        if !self.cfg.release_iso.is_empty() {
            let name = self.cfg.release_iso.rsplit('/').next().unwrap_or_default();
            *self.release.lock().unwrap() = Some(name.to_string());
            return Ok(self.cfg.release_iso.clone());
        }
        let key = short_hash(&format!(
            "{}{}{}{CMDLINE}",
            crate::suspend::fingerprint(kernel),
            crate::suspend::fingerprint(agent),
            crate::suspend::fingerprint(&self.cfg.mke2fs)
        ));
        let name = self.iso_name("release", &key);
        let file = self.dir.join(&name);
        build_release_iso(&file, kernel, agent, &self.cfg.mke2fs).await?;
        let uploaded = self.ensure_iso(&file, &name).await?;
        let first = self.release.lock().unwrap().replace(name.clone()) != Some(name.clone());
        if first {
            // Built-up files of older releases go; their ISOs go in the
            // sweep, once no VM boots them.
            for e in std::fs::read_dir(&self.dir).into_iter().flatten().flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n != name && n.starts_with(&format!("{}-release-", self.cfg.prefix)) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        if uploaded {
            self.log.info("made a release ISO", &[("iso", &name)]);
        }
        Ok(self.iso_volid(&name))
    }

    /// The ISO of an image (base, its EROFS file, named by key), uploaded if
    /// it is not there yet; its volume's ID. Proxmox takes any file named
    /// .iso as one: it is never booted, only read as a CD-ROM.
    pub async fn image(&self, key: &str, base: &Path) -> io::Result<String> {
        let name = self.iso_name("image", &short_hash(key));
        self.ensure_iso(base, &name).await?;
        Ok(self.iso_volid(&name))
    }

    /// Uploads file as name into the ISO storage, unless it is there;
    /// whether it uploaded it.
    async fn ensure_iso(&self, file: &Path, name: &str) -> io::Result<bool> {
        let _one = self.uploading.lock().await;
        self.seen
            .lock()
            .unwrap()
            .insert(name.to_string(), Instant::now());
        if self.isos().await?.contains(&self.iso_volid(name)) {
            return Ok(false);
        }
        self.log.info(
            "uploading to proxmox",
            &[("file", &file.display()), ("as", &name)],
        );
        let upid = self
            .client
            .upload(&self.cfg.node, self.iso_storage(), "iso", file, name)
            .await?;
        self.client
            .wait(&self.cfg.node, &upid, Duration::from_secs(1800))
            .await?;
        self.seen
            .lock()
            .unwrap()
            .insert(name.to_string(), Instant::now());
        Ok(true)
    }

    /// The volume IDs of the ISOs in the ISO storage.
    async fn isos(&self) -> io::Result<HashSet<String>> {
        let path = format!(
            "/nodes/{}/storage/{}/content?content=iso",
            self.node(),
            enc(self.iso_storage())
        );
        let have = self.client.get(&path).await?;
        Ok(have
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["volid"].as_str().map(str::to_string))
            .collect())
    }

    /// This place's VMs, on any node: those in its pool carrying its prefix
    /// as a tag, as (node, VMID).
    async fn own_vms(&self) -> io::Result<Vec<(String, u64)>> {
        let all = self.client.get("/cluster/resources?type=vm").await?;
        Ok(all
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| {
                v["tags"]
                    .as_str()
                    .is_some_and(|t| t.split(';').any(|t| t == self.cfg.prefix))
                    && (self.cfg.pool.is_empty() || v["pool"] == self.cfg.pool.as_str())
            })
            .filter_map(|v| Some((v["node"].as_str()?.to_string(), v["vmid"].as_u64()?)))
            .collect())
    }

    /// Removes this place's ISOs that none of its VMs names, but the current
    /// release and any the worker has uploaded or found within ISO_GRACE.
    /// Only names with the place's prefix are touched: places sharing a
    /// Proxmox leave each other's alone. A VM whose config cannot be read
    /// stops the sweep, as what it names is not known.
    pub async fn sweep(&self) {
        if let Err(e) = self.try_sweep().await {
            self.log.debug("sweeping proxmox ISOs", &[("err", &e)]);
        }
    }

    async fn try_sweep(&self) -> io::Result<()> {
        let mut named = HashSet::new();
        for (node, id) in self.own_vms().await? {
            let conf = self
                .client
                .get(&format!("/nodes/{}/qemu/{id}/config", enc(&node)))
                .await?;
            for (_, v) in conf.as_object().into_iter().flatten() {
                if let Some(volid) = v.as_str().and_then(|s| s.split(',').next()) {
                    named.insert(volid.to_string());
                }
            }
        }
        let current = self.release.lock().unwrap().clone();
        let mine = |n: &str| {
            [("release"), ("image")]
                .iter()
                .any(|k| n.starts_with(&format!("{}-{k}-", self.cfg.prefix)))
                && n.ends_with(".iso")
        };
        for volid in self.isos().await? {
            let name = volid.rsplit('/').next().unwrap_or_default().to_string();
            let recent = self
                .seen
                .lock()
                .unwrap()
                .get(&name)
                .is_some_and(|t| t.elapsed() < ISO_GRACE);
            if !mine(&name) || named.contains(&volid) || recent || current.as_ref() == Some(&name) {
                continue;
            }
            let path = format!(
                "/nodes/{}/storage/{}/content/{}",
                self.node(),
                enc(self.iso_storage()),
                enc(&volid)
            );
            let upid = self.client.delete(&path, &[]).await?;
            self.wait(&upid, Duration::from_secs(120)).await?;
            self.seen.lock().unwrap().remove(&name);
            self.log
                .info("removed an ISO no VM uses", &[("iso", &name)]);
        }
        Ok(())
    }

    /// Makes an environment's VM: the image and release ISOs as CD-ROMs, an
    /// empty writable disk of upper_gib, whose filesystem its agent makes,
    /// and a swap disk, named and tagged for this place.
    pub async fn create(
        &self,
        environment: &str,
        name: &str,
        image: &str,
        release: &str,
        upper_gib: u64,
        swap_mib: u64,
    ) -> io::Result<u32> {
        let store = &self.cfg.storage;
        let swap_gib = swap_mib.div_ceil(1024).max(1);
        let mut tries = 0;
        let id = loop {
            let id = self.next_id().await?;
            let mut form = vec![
                ("vmid", id.to_string()),
                ("name", vm_name(&self.cfg.prefix, name)),
                ("tags", format!("{TAG};{};hangar-env", self.cfg.prefix)),
                ("description", format!("Hangar environment {environment}")),
                ("ostype", "l26".into()),
                ("cpu", "host".into()),
                ("cores", "1".into()),
                // Memory hotplug, which Proxmox has only with NUMA: the VM
                // boots with STATIC_MIB of RAM and the rest as DIMMs, which
                // the guest finds through ACPI, and its memory is resized
                // while it runs by plugging and unplugging them.
                ("memory", STATIC_MIB.to_string()),
                ("numa", "1".into()),
                ("hotplug", "network,disk,usb,memory".into()),
                ("agent", "1".into()),
                ("serial0", "socket".into()),
                // A guest's reboot stops the VM, and the worker boots it
                // again, with a new secret.
                ("reboot", "0".into()),
                // One controller: each CD-ROM's scsiN is its LUN.
                ("scsihw", "virtio-scsi-pci".into()),
                (IMAGE_SLOT, format!("{image},media=cdrom")),
                (RELEASE_SLOT, format!("{release},media=cdrom")),
                (
                    "virtio0",
                    format!(
                        "{store}:{},serial={UPPER_SERIAL},discard=on",
                        upper_gib.max(1)
                    ),
                ),
                (
                    "virtio1",
                    format!("{store}:{swap_gib},serial={SWAP_SERIAL}"),
                ),
                ("vmstatestorage", store.clone()),
                // No network card: each start plugs one in (start).
                ("boot", format!("order={RELEASE_SLOT}")),
            ];
            if !self.cfg.pool.is_empty() {
                form.push(("pool", self.cfg.pool.clone()));
            }
            let form: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
            match self
                .client
                .post(&format!("/nodes/{}/qemu", self.node()), &form)
                .await
            {
                Ok(upid) => {
                    self.wait(&upid, Duration::from_secs(600)).await?;
                    break id;
                }
                // Another took the VMID between asking and making the VM.
                Err(e) if tries < 5 && e.to_string().contains("already exists") => tries += 1,
                Err(e) => return Err(e),
            }
        };
        // Proxmox's own firewall for the VM, which applies where the
        // datacenter's firewall is on: nothing inbound but replies, DHCP
        // and neighbour discovery, no router advertisements out, and no MAC
        // but the VM's own. Not an address filter: the network's DHCP gives
        // the address. The guest's own firewall stands either way.
        self.client
            .put(
                &self.vm(id, "firewall/options"),
                &[
                    ("enable", "1"),
                    ("policy_in", "DROP"),
                    ("policy_out", "ACCEPT"),
                    ("dhcp", "1"),
                    ("ndp", "1"),
                    ("radv", "0"),
                    ("macfilter", "1"),
                    ("ipfilter", "0"),
                ],
            )
            .await?;
        Ok(id)
    }

    fn vm(&self, id: u32, rest: &str) -> String {
        format!("/nodes/{}/qemu/{id}/{rest}", self.node())
    }

    /// Sizes a VM that is not running: its memory, as fit_memory makes
    /// it, and its vCPUs.
    pub async fn size(&self, id: u32, memory_mib: u32, cpus: u32) -> io::Result<()> {
        let m = fit_memory(memory_mib, u32::MAX).to_string();
        let c = cpus.max(1).to_string();
        self.client
            .put(&self.vm(id, "config"), &[("memory", &m), ("cores", &c)])
            .await
            .map(|_| ())
    }

    /// Asks a running VM to have mib MiB of memory (fit_memory's), which
    /// Proxmox plugs in or unplugs as DIMMs in a task of its own: memory
    /// says how far it has got. A DIMM the guest cannot give back stays,
    /// and the rest of the change waits for the VM's next start.
    pub async fn set_memory(&self, id: u32, mib: u32) -> io::Result<()> {
        let m = mib.to_string();
        self.client
            .post(&self.vm(id, "config"), &[("memory", &m)])
            .await
            .map(|_| ())
    }

    /// The memory a VM has, in MiB: under hotplug, what is plugged in.
    pub async fn memory(&self, id: u32) -> io::Result<u32> {
        let conf = self.client.get(&self.vm(id, "config")).await?;
        let m = match &conf["memory"] {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s
                .split(',')
                .map(|p| p.strip_prefix("current=").unwrap_or(p))
                .find_map(|p| p.parse().ok()),
            _ => None,
        };
        m.map(|m| m as u32)
            .ok_or_else(|| err("the VM's config has no memory"))
    }

    /// Starts a VM, or resumes it from a hibernate, then gives it its hub
    /// settings, secret and all, through its agent channel once its agent
    /// answers there: nothing of them goes in the VM's configuration, which
    /// anyone who may see the VM can read. A cold start boots the release
    /// ISO given and reads the image ISO given (the environment's pin may
    /// have moved); a resume keeps the ones it was hibernated with.
    pub async fn start(
        &self,
        id: u32,
        placed: &Placed,
        secret: &str,
        image: &str,
        release: &str,
    ) -> io::Result<()> {
        let mut placed = placed.clone();
        placed.secret = secret.to_string();
        let settings = serde_json::to_string(&placed).map_err(io::Error::other)?;
        let conf = self.client.get(&self.vm(id, "config")).await?;
        let resuming = conf["lock"] == "suspended";
        // A cold start's network card is plugged in once the VM runs: one
        // there from the start has SeaBIOS run its iPXE ROM, which waits
        // for a key for most of a second. It keeps its MAC, and so its
        // DHCP lease.
        let mac = network_mac(&conf).unwrap_or_else(|| derived_mac(&placed.environment));
        if !resuming {
            let image = format!("{image},media=cdrom");
            let release = format!("{release},media=cdrom");
            let mut form = vec![
                (IMAGE_SLOT, image.as_str()),
                (RELEASE_SLOT, release.as_str()),
            ];
            if conf["net0"].is_string() {
                form.push(("delete", "net0"));
            }
            self.client.put(&self.vm(id, "config"), &form).await?;
        }
        let upid = self.client.post(&self.vm(id, "status/start"), &[]).await?;
        self.wait(&upid, Duration::from_secs(300)).await?;
        if !resuming {
            let net = format!("virtio={mac},bridge={},firewall=1", self.cfg.bridge);
            self.client
                .put(&self.vm(id, "config"), &[("net0", net.as_str())])
                .await?;
        }
        self.give_settings(id, &settings).await
    }

    /// Gives a running VM's agent the hub's settings again, with the secret
    /// it booted with, as for a worker taking back a VM another left
    /// running: the agent dials the hub they name at once.
    pub async fn rejoin(&self, id: u32, placed: &Placed, secret: &str) -> io::Result<()> {
        let mut placed = placed.clone();
        placed.secret = secret.to_string();
        let settings = serde_json::to_string(&placed).map_err(io::Error::other)?;
        self.give_settings(id, &settings).await
    }

    /// Writes the hub's settings into the guest once its agent answers on
    /// the agent channel.
    async fn give_settings(&self, id: u32, settings: &str) -> io::Result<()> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
        loop {
            if self
                .client
                .post(&self.vm(id, "agent/ping"), &[])
                .await
                .is_ok()
            {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                return Err(err("the VM's agent never answered on its agent channel"));
            }
            if !self.running(id).await? {
                return Err(err("the VM stopped while booting"));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        self.client
            .post(
                &self.vm(id, "agent/file-write"),
                &[("file", "/dev/hangar/hub.json"), ("content", settings)],
            )
            .await
            .map(|_| ())
    }

    /// Whether a VM is running.
    pub async fn running(&self, id: u32) -> io::Result<bool> {
        let st = self.client.get(&self.vm(id, "status/current")).await?;
        Ok(st["status"] == "running")
    }

    /// What Proxmox knows of a running VM: its status/current.
    pub async fn status(&self, id: u32) -> io::Result<Value> {
        self.client.get(&self.vm(id, "status/current")).await
    }

    /// What Proxmox measures of a running VM, and with disks, what its
    /// storage holds of its writable and swap disks.
    pub async fn stats(&self, id: u32, disks: bool) -> io::Result<VmStats> {
        let st = self.status(id).await?;
        let mut v = VmStats {
            cpu_percent: st["cpu"].as_f64().unwrap_or_default() * 100.0,
            host_memory: st["memhost"]
                .as_u64()
                .or_else(|| st["mem"].as_u64())
                .unwrap_or_default(),
            upper_used: None,
            swap_used: None,
        };
        if disks {
            let conf = self.client.get(&self.vm(id, "config")).await?;
            v.upper_used = self.volume_used(&conf, "virtio0").await;
            v.swap_used = self.volume_used(&conf, "virtio1").await;
        }
        Ok(v)
    }

    /// What a storage has allocated to the volume in a VM's disk slot.
    async fn volume_used(&self, conf: &Value, slot: &str) -> Option<u64> {
        let volid = conf[slot].as_str()?.split(',').next()?;
        let (storage, _) = volid.split_once(':')?;
        let path = format!(
            "/nodes/{}/storage/{}/content/{}",
            self.node(),
            enc(storage),
            enc(volid)
        );
        self.client.get(&path).await.ok()?["used"].as_u64()
    }

    /// Asks a VM to shut down, through its agent or ACPI.
    pub async fn shutdown(&self, id: u32) -> io::Result<()> {
        self.client
            .post(&self.vm(id, "status/shutdown"), &[("timeout", "60")])
            .await
            .map(|_| ())
    }

    /// Stops a VM at once, as pulling its power.
    pub async fn stop(&self, id: u32) -> io::Result<()> {
        let upid = self.client.post(&self.vm(id, "status/stop"), &[]).await?;
        self.wait(&upid, Duration::from_secs(120)).await
    }

    /// Hibernates a VM: its memory to disk, resumed by its next start.
    /// The task waits for the VM's config lock, which the task resuming it
    /// holds for a while after the VM runs; a lock not got is tried again.
    pub async fn hibernate(&self, id: u32) -> io::Result<()> {
        let mut tries = 0;
        loop {
            let upid = self
                .client
                .post(&self.vm(id, "status/suspend"), &[("todisk", "1")])
                .await?;
            match self.wait(&upid, Duration::from_secs(900)).await {
                Err(e) if tries < 5 && e.to_string().contains("can't lock file") => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                r => return r,
            }
        }
    }

    /// Destroys a VM and its disks, stopping it first.
    pub async fn destroy(&self, id: u32) -> io::Result<()> {
        if self.running(id).await.unwrap_or(false) {
            let _ = self.stop(id).await;
        }
        let path = format!("/nodes/{}/qemu/{id}", self.node());
        match self
            .client
            .delete(
                &path,
                &[("purge", "1"), ("destroy-unreferenced-disks", "1")],
            )
            .await
        {
            Ok(upid) => self.wait(&upid, Duration::from_secs(600)).await,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) if e.to_string().contains("does not exist") => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// The MAC of a VM's network card, if its config has one.
fn network_mac(conf: &Value) -> Option<String> {
    conf["net0"]
        .as_str()?
        .split(',')
        .filter_map(|p| p.split_once('=').map(|(_, v)| v))
        .find(|v| v.len() == 17 && v.matches(':').count() == 5)
        .map(str::to_string)
}

/// A MAC for an environment's network card, the same each time: a locally
/// administered one, from its ID.
fn derived_mac(environment: &str) -> String {
    let h = ring::digest::digest(&ring::digest::SHA256, environment.as_bytes());
    let b = h.as_ref();
    format!(
        "02:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        b[0], b[1], b[2], b[3], b[4]
    )
}

/// A VM's name, which Proxmox takes as a DNS name: the place's prefix,
/// then the environment's name.
fn vm_name(prefix: &str, name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let n = n.trim_matches('-');
    format!("{prefix}-{}", if n.is_empty() { "env" } else { n })
        .chars()
        .take(63)
        .collect::<String>()
        .trim_end_matches('-')
        .to_string()
}

/// Twelve hex digits of a string's SHA-256.
fn short_hash(s: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, s.as_bytes())
        .as_ref()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Where a system's isolinux keeps the El Torito boot image and the module
/// it loads, by distribution (Debian and Ubuntu's first).
const ISOLINUX_BIN: [&str; 3] = [
    "/usr/lib/ISOLINUX/isolinux.bin",
    "/usr/share/syslinux/isolinux.bin",
    "/usr/lib/syslinux/isolinux.bin",
];
const LDLINUX_C32: [&str; 3] = [
    "/usr/lib/syslinux/modules/bios/ldlinux.c32",
    "/usr/share/syslinux/ldlinux.c32",
    "/usr/lib/syslinux/ldlinux.c32",
];

/// Builds the release ISO at out, unless it is there: isolinux booting the
/// kernel with CMDLINE, its initramfs the agent with mke2fs beside it.
/// SeaBIOS boots it from a CD-ROM on virtio-scsi.
async fn build_release_iso(
    out: &Path,
    kernel: &Path,
    agent: &Path,
    mke2fs: &Path,
) -> io::Result<()> {
    if out.exists() {
        return Ok(());
    }
    let need = "the worker needs xorriso and isolinux";
    let find = |paths: &[&str]| {
        paths
            .iter()
            .map(Path::new)
            .find(|p| p.is_file())
            .map(Path::to_path_buf)
            .ok_or_else(|| err(format!("no {} ({need})", paths[0])))
    };
    let (isolinux, ldlinux) = (find(&ISOLINUX_BIN)?, find(&LDLINUX_C32)?);
    let mke2fs_bin = std::fs::read(mke2fs).map_err(|e| {
        err(format!(
            "mke2fs for the release ISO, {}: {e}",
            mke2fs.display()
        ))
    })?;
    let dir = out.with_extension("d");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("isolinux"))?;
    std::fs::copy(&isolinux, dir.join("isolinux/isolinux.bin"))?;
    std::fs::copy(&ldlinux, dir.join("isolinux/ldlinux.c32"))?;
    std::fs::write(
        dir.join("isolinux/isolinux.cfg"),
        format!(
            "DEFAULT hangar\nPROMPT 0\nTIMEOUT 0\nLABEL hangar\n  LINUX /vmlinuz\n  INITRD /initrd.img\n  APPEND {CMDLINE}\n"
        ),
    )?;
    std::fs::copy(kernel, dir.join("vmlinuz"))?;
    crate::write_initrd_with(
        agent,
        &dir.join("initrd.img"),
        &[("mke2fs", 0o755, &mke2fs_bin)],
    )?;
    let tmp = out.with_extension("iso.new");
    let made = tokio::process::Command::new("xorriso")
        .args(["-as", "mkisofs", "-quiet", "-V", "HANGAR", "-R", "-J"])
        .args(["-b", "isolinux/isolinux.bin", "-c", "isolinux/boot.cat"])
        .args([
            "-no-emul-boot",
            "-boot-load-size",
            "4",
            "-boot-info-table",
            "-o",
        ])
        .arg(&tmp)
        .arg(&dir)
        .output()
        .await
        .map_err(|e| err(format!("xorriso: {e} ({need})")))?;
    let _ = std::fs::remove_dir_all(&dir);
    if !made.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(format!(
            "xorriso: {}",
            String::from_utf8_lossy(&made.stderr).trim()
        )));
    }
    std::fs::rename(&tmp, out)
}
