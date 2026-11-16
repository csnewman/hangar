//! Environments as Proxmox VE VMs (`runtime: proxmox`): the worker runs
//! beside Proxmox, in a container on one of its nodes, and asks its API for
//! a VM per environment. Design and measurements: docs/proxmox-vms.md.
//!
//! An image becomes a template VM once: its EROFS image, a boot disk of the
//! worker's kernel and agent, and a small empty writable disk, each a disk
//! named by its serial, which the agent's init finds them by. An environment
//! is a linked clone of it, its writable disk grown to the environment's
//! size, with a swap disk of its own. Each boot, the VM's SMBIOS serial says
//! where the worker's hub is, and the hub's secret for the boot is written
//! into the guest through its agent channel (`hangar-agent`'s qga), never
//! into the VM's configuration, which anyone who may see the VM can read.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use serde_json::Value;

use crate::err;
use crate::log::Log;

pub mod client;
pub mod console;

use client::{Client, enc};

/// Where the agent's init finds each disk, by the serial Proxmox gives it.
const UPPER_SERIAL: &str = "hangar-upper";
const IMAGE_SERIAL: &str = "hangar-image";
const BOOT_SERIAL: &str = "hangar-boot";
const SWAP_SERIAL: &str = "hangar-swap";

/// The kernel command line every environment boots with, fixed on the boot
/// disk its template shares with every clone. Headless: a Proxmox VM has no
/// virtual GPU of Hangar's. The guest puts its own firewall up: the VM is
/// on its place's network, among other machines.
const CMDLINE: &str = "console=ttyS0 systemd.show_status=1 mitigations=off \
    memory_hotplug.online_policy=auto-movable \
    hangar.upper=serial:hangar-upper hangar.image=serial:hangar-image \
    hangar.swap=serial:hangar-swap hangar.firewall=1 \
    systemd.unit=multi-user.target systemd.mask=hangar-desktop.service";

/// The form of a template, part of its name: a template made to an older
/// form is left for one of this form.
const TEMPLATE_FORM: &str = "3";

/// The template's writable disk, grown for each environment.
const TEMPLATE_UPPER_GIB: u64 = 1;

/// The tag every VM the worker makes carries.
const TAG: &str = "hangar";

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
    /// Where images and the boot disk are uploaded to, a storage with
    /// import content; empty is storage.
    pub import_storage: String,
    /// The bridge or VNet environments' NICs join.
    pub bridge: String,
    /// The pool every VM joins; empty is none.
    pub pool: String,
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
    /// The templates made, by image and boot.
    templates: tokio::sync::Mutex<HashMap<String, u32>>,
    /// The boot disk's key templates are made with, once one has been.
    boot_key: std::sync::Mutex<Option<String>>,
    log: Log,
}

/// What a VM's agent needs besides its secret, in its SMBIOS serial.
pub type Placed = hangar_hub::Settings;

impl Pve {
    pub fn new(cfg: Config, dir: &Path, log: Log) -> io::Result<Pve> {
        let fp = (!cfg.fingerprint.is_empty()).then_some(cfg.fingerprint.as_str());
        let client = Client::new(&cfg.url, &cfg.token, fp)?;
        Ok(Pve {
            client,
            cfg,
            dir: dir.to_path_buf(),
            templates: Default::default(),
            boot_key: Default::default(),
            log,
        })
    }

    fn node(&self) -> String {
        enc(&self.cfg.node)
    }

    fn import_storage(&self) -> &str {
        if self.cfg.import_storage.is_empty() {
            &self.cfg.storage
        } else {
            &self.cfg.import_storage
        }
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

    /// The template for an image (base, its EROFS file, named by key) and
    /// the worker's kernel and agent, made if it is not there yet.
    pub async fn template(
        &self,
        key: &str,
        base: &Path,
        kernel: &Path,
        agent: &Path,
    ) -> io::Result<u32> {
        let boot_key = short_hash(&format!(
            "{}{}{CMDLINE}",
            crate::suspend::fingerprint(kernel),
            crate::suspend::fingerprint(agent)
        ));
        let name = format!("hangar-tpl{TEMPLATE_FORM}-{}-{boot_key}", short_hash(key));
        *self.boot_key.lock().unwrap() = Some(boot_key.clone());
        let mut made = self.templates.lock().await;
        if let Some(id) = made.get(&name) {
            return Ok(*id);
        }
        if let Some(id) = self.find_vm(&name).await? {
            made.insert(name, id);
            return Ok(id);
        }
        self.log.info(
            "making a proxmox template",
            &[("template", &name), ("image", &key)],
        );
        let image = format!("hangar-image-{}.raw", short_hash(key));
        self.upload_once(base, &image).await?;
        let boot_file = self.dir.join(format!("boot-{boot_key}.raw"));
        build_boot_disk(&boot_file, kernel, agent).await?;
        let boot = format!("hangar-boot-{boot_key}.raw");
        self.upload_once(&boot_file, &boot).await?;
        let upper_file = self.dir.join("upper.raw");
        crate::provision::ensure_disk(
            &upper_file,
            UPPER_SERIAL,
            TEMPLATE_UPPER_GIB,
            &crate::provision::find_mkfs()?,
        )
        .await?;
        let upper = "hangar-upper.raw";
        self.upload_once(&upper_file, upper).await?;

        let (store, imports) = (&self.cfg.storage, self.import_storage());
        let disk = |name: &str, serial: &str, extra: &str| {
            format!("{store}:0,import-from={imports}:import/{name},serial={serial}{extra}")
        };
        let id = self.next_id().await?;
        let mut form = vec![
            ("vmid", id.to_string()),
            ("name", name.clone()),
            ("tags", format!("{TAG};hangar-template")),
            ("ostype", "l26".into()),
            ("cpu", "host".into()),
            ("cores", "1".into()),
            // Memory hotplug, which Proxmox has only with NUMA: the VM
            // boots with STATIC_MIB of RAM and the rest as DIMMs, which the
            // guest finds through ACPI, and its memory is resized while it
            // runs by plugging and unplugging them.
            ("memory", STATIC_MIB.to_string()),
            ("numa", "1".into()),
            ("hotplug", "network,disk,usb,memory".into()),
            ("agent", "1".into()),
            ("serial0", "socket".into()),
            // A guest's reboot stops the VM, and the worker boots it
            // again, with a new secret.
            ("reboot", "0".into()),
            ("boot", "order=virtio2".into()),
            ("net0", format!("virtio,bridge={}", self.cfg.bridge)),
            ("virtio0", disk(upper, UPPER_SERIAL, ",discard=on")),
            ("virtio1", disk(&image, IMAGE_SERIAL, ",ro=1")),
            ("virtio2", disk(&boot, BOOT_SERIAL, "")),
            ("description", format!("Hangar's template for image {key}")),
        ];
        if !self.cfg.pool.is_empty() {
            form.push(("pool", self.cfg.pool.clone()));
        }
        let form: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let upid = self
            .client
            .post(&format!("/nodes/{}/qemu", self.node()), &form)
            .await?;
        self.wait(&upid, Duration::from_secs(600)).await?;
        let upid = self
            .client
            .post(&format!("/nodes/{}/qemu/{id}/template", self.node()), &[])
            .await?;
        self.wait(&upid, Duration::from_secs(300)).await?;
        made.insert(name, id);
        Ok(id)
    }

    /// Removes this node's templates of another form or boot disk than the
    /// ones this worker makes, and the uploads of boot disks no template
    /// has left.
    /// Proxmox refuses to remove a template whose disks a linked clone
    /// still uses (on LVM-thin or ZFS; on btrfs a clone stands alone),
    /// which is then kept until a sweep after its last environment goes.
    pub async fn sweep(&self) {
        let Some(boot_key) = self.boot_key.lock().unwrap().clone() else {
            return;
        };
        let Ok(all) = self.client.get("/cluster/resources?type=vm").await else {
            return;
        };
        let current = format!("hangar-tpl{TEMPLATE_FORM}-");
        let mut boots_kept = std::collections::HashSet::from([boot_key.clone()]);
        for v in all.as_array().into_iter().flatten() {
            let (Some(name), Some(id)) = (v["name"].as_str(), v["vmid"].as_u64()) else {
                continue;
            };
            let ours = v["node"] == self.cfg.node.as_str()
                && v["template"].as_u64() == Some(1)
                && name.starts_with("hangar-tpl")
                && v["tags"]
                    .as_str()
                    .is_some_and(|t| t.split(';').any(|t| t == TAG));
            if !ours {
                continue;
            }
            let boot = name.rsplit('-').next().unwrap_or_default().to_string();
            if name.starts_with(&current) && boot == boot_key {
                continue;
            }
            match self.destroy(id as u32).await {
                Ok(()) => {
                    self.log.info(
                        "removed a superseded proxmox template",
                        &[("template", &name)],
                    );
                    self.templates.lock().await.remove(name);
                }
                Err(e) => {
                    self.log.debug(
                        "keeping a superseded proxmox template",
                        &[("template", &name), ("err", &e)],
                    );
                    boots_kept.insert(boot);
                }
            }
        }
        let path = format!(
            "/nodes/{}/storage/{}/content?content=import",
            self.node(),
            enc(self.import_storage())
        );
        let Ok(have) = self.client.get(&path).await else {
            return;
        };
        for v in have.as_array().into_iter().flatten() {
            let Some(volid) = v["volid"].as_str() else {
                continue;
            };
            let Some(key) = volid
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_prefix("hangar-boot-"))
                .and_then(|f| f.strip_suffix(".raw"))
            else {
                continue;
            };
            if boots_kept.contains(key) {
                continue;
            }
            let path = format!(
                "/nodes/{}/storage/{}/content/{}",
                self.node(),
                enc(self.import_storage()),
                enc(volid)
            );
            if let Ok(upid) = self.client.delete(&path, &[]).await {
                let _ = self.wait(&upid, Duration::from_secs(120)).await;
            }
            let _ = std::fs::remove_file(self.dir.join(format!("boot-{key}.raw")));
        }
    }

    /// The VMID of the VM on this node named name, if there is one.
    async fn find_vm(&self, name: &str) -> io::Result<Option<u32>> {
        let all = self.client.get("/cluster/resources?type=vm").await?;
        Ok(all.as_array().and_then(|vms| {
            vms.iter()
                .find(|v| v["name"] == name && v["node"] == self.cfg.node.as_str())
                .and_then(|v| v["vmid"].as_u64())
                .map(|n| n as u32)
        }))
    }

    /// Uploads file as name into the import storage, unless it is there.
    async fn upload_once(&self, file: &Path, name: &str) -> io::Result<()> {
        let path = format!(
            "/nodes/{}/storage/{}/content?content=import",
            self.node(),
            enc(self.import_storage())
        );
        let have = self.client.get(&path).await?;
        let volid = format!("{}:import/{name}", self.import_storage());
        if have
            .as_array()
            .is_some_and(|c| c.iter().any(|v| v["volid"] == volid.as_str()))
        {
            return Ok(());
        }
        self.log.info(
            "uploading to proxmox",
            &[("file", &file.display()), ("as", &name)],
        );
        let upid = self
            .client
            .upload(&self.cfg.node, self.import_storage(), "import", file, name)
            .await?;
        self.client
            .wait(&self.cfg.node, &upid, Duration::from_secs(1800))
            .await
    }

    /// Makes an environment's VM: a linked clone of template, named for it,
    /// with its writable disk grown to upper_gib and swap of its own.
    pub async fn create(
        &self,
        template: u32,
        environment: &str,
        name: &str,
        upper_gib: u64,
        swap_mib: u64,
    ) -> io::Result<u32> {
        let mut tries = 0;
        let id = loop {
            let id = self.next_id().await?;
            let mut form = vec![
                ("newid", id.to_string()),
                ("name", vm_name(name)),
                ("full", "0".to_string()),
            ];
            if !self.cfg.pool.is_empty() {
                form.push(("pool", self.cfg.pool.clone()));
            }
            let form: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
            match self
                .client
                .post(
                    &format!("/nodes/{}/qemu/{template}/clone", self.node()),
                    &form,
                )
                .await
            {
                Ok(upid) => {
                    self.wait(&upid, Duration::from_secs(600)).await?;
                    break id;
                }
                // Another took the VMID between asking and cloning.
                Err(e) if tries < 5 && e.to_string().contains("already exists") => tries += 1,
                Err(e) => return Err(e),
            }
        };
        let swap_gib = swap_mib.div_ceil(1024).max(1);
        let form = [
            ("tags", format!("{TAG};hangar-env")),
            ("description", format!("Hangar environment {environment}")),
            (
                "virtio3",
                format!("{}:{swap_gib},serial={SWAP_SERIAL}", self.cfg.storage),
            ),
        ];
        let form: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let upid = self.client.post(&self.vm(id, "config"), &form).await?;
        self.wait(&upid, Duration::from_secs(300)).await?;
        let size = format!("{}G", upper_gib.max(TEMPLATE_UPPER_GIB));
        let upid = self
            .client
            .put(
                &self.vm(id, "resize"),
                &[("disk", "virtio0"), ("size", size.as_str())],
            )
            .await?;
        self.wait(&upid, Duration::from_secs(300)).await?;
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

    /// Starts a VM, or resumes it from a hibernate, saying in its SMBIOS
    /// serial where its hub is, then hands it secret through its agent
    /// channel once its agent answers there.
    pub async fn start(&self, id: u32, placed: &Placed, secret: &str) -> io::Result<()> {
        let mut placed = placed.clone();
        placed.secret.clear();
        let serial = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&placed).map_err(io::Error::other)?);
        let conf = self.client.get(&self.vm(id, "config")).await?;
        let uuid = conf["smbios1"]
            .as_str()
            .unwrap_or_default()
            .split(',')
            .find(|p| p.starts_with("uuid="))
            .map(str::to_string);
        let smbios = match uuid {
            Some(u) => format!("{u},serial={serial},base64=1"),
            None => format!("serial={serial},base64=1"),
        };
        if conf["lock"] != "suspended" {
            self.client
                .put(&self.vm(id, "config"), &[("smbios1", smbios.as_str())])
                .await?;
        }
        let upid = self.client.post(&self.vm(id, "status/start"), &[]).await?;
        self.wait(&upid, Duration::from_secs(300)).await?;
        self.give_secret(id, secret).await
    }

    /// Writes the hub's secret into the guest once its agent answers on the
    /// agent channel.
    async fn give_secret(&self, id: u32, secret: &str) -> io::Result<()> {
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
                &[("file", "/dev/hangar/hub-secret"), ("content", secret)],
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
            v.swap_used = self.volume_used(&conf, "virtio3").await;
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
    pub async fn hibernate(&self, id: u32) -> io::Result<()> {
        let upid = self
            .client
            .post(&self.vm(id, "status/suspend"), &[("todisk", "1")])
            .await?;
        self.wait(&upid, Duration::from_secs(900)).await
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

/// A VM name, which Proxmox takes as a DNS name.
fn vm_name(name: &str) -> String {
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
    format!("hangar-{}", if n.is_empty() { "env" } else { n })
        .chars()
        .take(63)
        .collect()
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

/// Builds the boot disk every clone of a template shares: a FAT disk with
/// syslinux, the kernel, and the agent as the initramfs, booting with
/// CMDLINE. SeaBIOS boots it, faster than OVMF does (docs/proxmox-vms.md).
async fn build_boot_disk(out: &Path, kernel: &Path, agent: &Path) -> io::Result<()> {
    if out.exists() {
        return Ok(());
    }
    let dir = out.with_extension("d");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    crate::write_initrd(agent, &dir.join("initrd.img"))?;
    std::fs::copy(kernel, dir.join("vmlinuz"))?;
    std::fs::write(
        dir.join("syslinux.cfg"),
        format!(
            "DEFAULT hangar\nPROMPT 0\nTIMEOUT 0\nLABEL hangar\n  LINUX /vmlinuz\n  INITRD /initrd.img\n  APPEND {CMDLINE}\n"
        ),
    )?;
    let img = dir.join("boot.img");
    let img_s = img.to_string_lossy().to_string();
    let run = |prog: &str, args: Vec<String>| {
        let prog = prog.to_string();
        async move {
            let out = tokio::process::Command::new(&prog)
                .args(&args)
                .output()
                .await
                .map_err(|e| {
                    err(format!(
                        "{prog}: {e} (the worker needs dosfstools, syslinux and mtools)"
                    ))
                })?;
            if !out.status.success() {
                return Err(err(format!(
                    "{prog}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )));
            }
            Ok(())
        }
    };
    run(
        "mkfs.fat",
        vec![
            "-n".into(),
            "HANGARBOOT".into(),
            "-C".into(),
            img_s.clone(),
            "65536".into(),
        ],
    )
    .await?;
    run("syslinux", vec!["--install".into(), img_s.clone()]).await?;
    for f in ["vmlinuz", "initrd.img", "syslinux.cfg"] {
        run(
            "mcopy",
            vec![
                "-i".into(),
                img_s.clone(),
                dir.join(f).to_string_lossy().to_string(),
                "::/".into(),
            ],
        )
        .await?;
    }
    std::fs::rename(&img, out)?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
