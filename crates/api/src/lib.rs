//! The JSON shapes a worker exchanges with hangar-server.
//!
//! The worker API is level-triggered. The server never sends a worker a
//! command; it publishes the complete set of environments the worker should
//! be holding, and the worker reports the complete set it is actually
//! holding. Either side can drop any number of messages and the next one
//! repairs it.
//!
//! A list or map may be written as null, which reads here as empty. Every
//! enum reads a value it does not know as its Other, so a newer side's
//! values never fail the whole message (tests/enums.rs holds every enum
//! here to that).

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

pub mod monitor;
mod time;

pub use time::rfc3339;

/// Reads null as the type's default, as the server writes an empty list.
fn null<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_zero_i64(n: &i64) -> bool {
    *n == 0
}

fn is_zero_f64(n: &f64) -> bool {
    *n == 0.0
}

/// What the owner of an environment has asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DesiredState {
    Running,
    Stopped,
    /// Keeps a running environment's memory on its worker's disk, and its
    /// processes with it, until it is started again. Stopping a suspended
    /// environment discards the memory; its disks stay as they were, as
    /// after a power cut.
    Suspended,
    /// Held until the worker reports the environment gone. An environment
    /// absent from a desired set is one the server does not know, which a
    /// worker must leave alone.
    Deleted,
    #[serde(other)]
    Other,
}

/// What a worker reports an environment to be doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    /// The server's own, for an environment no worker has reported on yet.
    Pending,
    Starting,
    Running,
    Stopping,
    Stopped,
    /// Being written to disk, and held there. A start resumes it; its
    /// phase is starting while it does.
    Suspending,
    Suspended,
    Failed,
    Deleting,
    /// A phase this side does not know.
    #[serde(other)]
    Other,
}

/// Whether an environment has a graphical desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Display {
    /// Headless: no compositor, no VNC, and less memory spent on both.
    None,
    Desktop,
    #[default]
    #[serde(other)]
    Other,
}

/// What kind of GPU an environment is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Gpu {
    None,
    /// Renders through the host's GPU via virtio-gpu, shared with other
    /// environments.
    Virtual,
    /// A whole physical GPU. It pins the environment's memory and cannot be
    /// suspended.
    Passthrough,
    #[default]
    #[serde(other)]
    Other,
}

/// A git repository cloned into an environment when it is created.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    pub url: String,
    /// What to check out: a branch, tag or commit; empty is the remote's
    /// default branch.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub r#ref: String,
    /// Where in the environment the repository is cloned.
    pub path: String,
    /// A new branch created from ref after cloning.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
}

/// A pack a spec lists, by ID, and the copy of it every environment uses,
/// if the spec pins one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackRef {
    pub pack: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub copy: String,
}

/// What of its owner's an environment is kept from, for code they do not
/// trust. The default keeps nothing from it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Access {
    /// Only the packs its spec lists: none of those that attach themselves
    /// to environments, the profile among them.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_self_attached: bool,
    /// The sensitive files -- credentials -- of its owner's profile and of
    /// its packs are kept from it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_sensitive_files: bool,
    /// Its SSH agent offers none of its owner's keys.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_ssh_keys: bool,
    /// No credential for Hangar's registry.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_registry: bool,
    /// Its Code tab trusts no folder to run its code without the person
    /// saying so.
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_editor_trust: bool,
}

/// A template's spec: an environment's, its paths and branches holding
/// variables a name fills in, and a pattern that name must match.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TemplateSpec {
    #[serde(flatten)]
    pub spec: Spec,
    /// A regular expression the whole name must match; its named groups
    /// are variables too.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name_pattern: String,
    /// What a name that does not match is told.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name_hint: String,
}

/// What an environment is: its own resolved copy of its template's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    #[serde(default)]
    pub image: String,
    #[serde(default)]
    pub cpus: i64,
    #[serde(default)]
    pub memory_mib: i64,
    #[serde(default)]
    pub display: Display,
    #[serde(default)]
    pub gpu: Gpu,
    #[serde(default, deserialize_with = "null")]
    pub repos: Vec<Repo>,
    /// The folders the Code tab opens side by side, its workspace to begin
    /// with.
    #[serde(
        default,
        deserialize_with = "null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub projects: Vec<String>,
    /// The first project: the folder the Code service and a new terminal
    /// start in. A spec saved before projects held only this.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub editor_path: String,
    #[serde(flatten)]
    pub access: Access,
    /// Trusted by the Code tab to run their code beyond the repositories
    /// and the projects, which it trusts anyway.
    #[serde(
        default,
        deserialize_with = "null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub trusted_folders: Vec<String>,
    /// Has the guest map its image's files from the host's page cache,
    /// through a DAX window, rather than read them into its own memory.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dax: bool,
    /// The environment's swap; none is its worker's default, 0 no swap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap_mib: Option<i64>,
    /// Has the guest hand back what its programs leave idle, into swap,
    /// and the cache nothing has touched for a while: experimental, and off
    /// unless asked for, since a program left idle is slow to answer when
    /// next used, while its memory is brought back in.
    #[serde(default, skip_serializing_if = "is_false")]
    pub memory_reclaim: bool,
    /// The packs it has, in order, before those that attach themselves.
    #[serde(
        default,
        deserialize_with = "null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub packs: Vec<PackRef>,
    /// The names of its own web servers its page links to.
    #[serde(
        default,
        deserialize_with = "null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub web_names: Vec<String>,
    /// Which workers may run it: each key a label the worker has, with
    /// this value.
    #[serde(default, deserialize_with = "null")]
    pub placement: BTreeMap<String, String>,
}

impl Spec {
    /// The features a worker needs to run it.
    pub fn needs(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.gpu == Gpu::Virtual {
            out.push(feature::GPU);
        }
        out
    }

    /// The projects, or for a spec saved with only an editor folder, that
    /// folder.
    pub fn project_folders(&self) -> Vec<String> {
        if self.projects.is_empty() && !self.editor_path.is_empty() {
            return vec![self.editor_path.clone()];
        }
        self.projects.clone()
    }

    /// The folders its Code tab trusts to run their code without asking:
    /// none with no_editor_trust; otherwise its repositories, its projects,
    /// and whatever else its template names. With no repositories or
    /// projects the Code tab opens the home folder, ~, which is then
    /// trusted, as the person's own.
    pub fn code_trust(&self) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        if self.access.no_editor_trust {
            return out;
        }
        fn add(out: &mut Vec<String>, p: &str) {
            if !p.is_empty() && !out.iter().any(|o| o == p) {
                out.push(p.to_string());
            }
        }
        for r in &self.repos {
            add(&mut out, &r.path);
        }
        for p in self.project_folders() {
            add(&mut out, &p);
        }
        add(&mut out, &self.editor_path);
        if out.is_empty() {
            add(&mut out, "~");
        }
        for f in &self.trusted_folders {
            add(&mut out, f);
        }
        out
    }
}

/// Where in the guest an environment's projects are kept, as a VS Code
/// workspace file: the worker writes its spec's projects there when it is
/// first set up, and the Code tab's workspace is the file's folders.
pub const WORKSPACE_FILE: &str = "/var/lib/hangar-workspace/projects.code-workspace";

/// A VS Code workspace file's content: its folders.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeWorkspace {
    #[serde(default, deserialize_with = "null")]
    pub folders: Vec<CodeWorkspaceFolder>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeWorkspaceFolder {
    pub path: String,
}

/// What of the files root an environment may reach, as its worker serves
/// it: the copies of packs it uses, by ID, and in each, the paths kept from
/// it as the copy's directory has them: a directory's ending in a slash,
/// and everything under it kept too.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentFiles {
    #[serde(default, deserialize_with = "null")]
    pub sets: Vec<String>,
    #[serde(
        default,
        deserialize_with = "null",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub hidden: BTreeMap<String, Vec<String>>,
    /// How a copy's callers act on its files, where its pack says other
    /// than all_squash.
    #[serde(
        default,
        deserialize_with = "null",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub squash: BTreeMap<String, Squash>,
}

/// Who an environment's callers act as on a pack's files, as an NFS
/// export's options say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Squash {
    /// Every caller is the environment's user, root among them.
    #[default]
    #[serde(rename = "all_squash")]
    All,
    /// Root is nobody, and other callers themselves.
    #[serde(rename = "root_squash")]
    Root,
    /// Every caller is themselves, root included.
    #[serde(rename = "no_root_squash")]
    None,
    #[serde(other)]
    Other,
}

/// The worker API's level, which a worker and a server compare before
/// anything else. It is raised whenever one side comes to need something of
/// the other that an older one does not do. A worker sends its level with
/// every request (LEVEL_HEADER); a server answers every request with the
/// levels it serves (LEVELS_HEADER) and refuses a worker outside them, and a
/// worker refuses a server whose newest level is older than its own. So a
/// server is never older than a worker it works with: a deployment upgrades
/// its servers first.
pub const API_LEVEL: u32 = 1;

/// The oldest level this side's server serves: older workers are refused,
/// and are to be upgraded.
pub const MIN_API_LEVEL: u32 = 1;

/// The request header that carries the worker's API level.
pub const LEVEL_HEADER: &str = "hangar-worker-api";

/// The response header that carries the levels a server serves, "MIN-MAX".
pub const LEVELS_HEADER: &str = "hangar-worker-api-levels";

/// The levels of the worker API a server serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Levels {
    pub min: u32,
    pub max: u32,
}

/// Why a worker and a server cannot work together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mismatch {
    /// The worker's level is older than the server serves.
    WorkerTooOld,
    /// The worker's level is newer than the server serves.
    ServerTooOld,
}

impl Levels {
    /// What this side's server serves.
    pub const SERVED: Levels = Levels {
        min: MIN_API_LEVEL,
        max: API_LEVEL,
    };

    /// Reads LEVELS_HEADER's value.
    pub fn parse(s: &str) -> Option<Levels> {
        let (min, max) = s.trim().split_once('-')?;
        let l = Levels {
            min: min.parse().ok()?,
            max: max.parse().ok()?,
        };
        (l.min <= l.max).then_some(l)
    }

    /// LEVELS_HEADER's value.
    pub fn header(&self) -> String {
        format!("{}-{}", self.min, self.max)
    }

    /// Why a worker at level cannot work with a server serving these, if
    /// it cannot.
    pub fn refuses(&self, level: u32) -> Option<Mismatch> {
        if level < self.min {
            Some(Mismatch::WorkerTooOld)
        } else if level > self.max {
            Some(Mismatch::ServerTooOld)
        } else {
            None
        }
    }
}

/// Why this worker cannot work with the server that answered with levels
/// (LEVELS_HEADER's value, None for a server that sent none), if it cannot.
pub fn incompatible(levels: Option<&str>) -> Option<String> {
    let Some(l) = levels.and_then(Levels::parse) else {
        return Some(format!(
            "the server does not say which worker API levels it serves, so it is older than \
             this worker (level {API_LEVEL}): upgrade the server"
        ));
    };
    match l.refuses(API_LEVEL)? {
        Mismatch::WorkerTooOld => Some(format!(
            "the server serves worker API levels {} to {}, and this worker speaks {API_LEVEL}: \
             upgrade the worker",
            l.min, l.max
        )),
        Mismatch::ServerTooOld => Some(format!(
            "the server serves worker API levels {} to {}, older than this worker's \
             {API_LEVEL}: upgrade the server",
            l.min, l.max
        )),
    }
}

/// A worker's answer to a stream the server opens over its tunnel: the
/// stream's first line from the worker, after the server's header and
/// before anything of the stream's own. ok, or why the worker cannot serve
/// it, as a code (STREAM_*) and a message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamAnswer {
    #[serde(default, skip_serializing_if = "is_false")]
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
}

/// The worker serves no streams of the kind the header names.
pub const STREAM_UNKNOWN_KIND: &str = "unknown_kind";

/// The environment is not running on the worker, or its guest did not
/// answer.
pub const STREAM_UNREACHABLE: &str = "unreachable";

impl StreamAnswer {
    pub fn ok() -> StreamAnswer {
        StreamAnswer {
            ok: true,
            ..StreamAnswer::default()
        }
    }

    pub fn refused(code: &str, error: impl Into<String>) -> StreamAnswer {
        StreamAnswer {
            ok: false,
            error: error.into(),
            code: code.to_string(),
        }
    }
}

/// What a worker can do that its API level does not promise, by name, as
/// it reports them (`WorkerStatus::features`). The server places an
/// environment only on a worker with every feature its spec needs
/// (`Spec::needs`).
pub mod feature {
    /// It can suspend its environments.
    pub const SUSPEND: &str = "suspend";
    /// It gives environments a virtual GPU.
    pub const GPU: &str = "gpu";
}

/// The monitors a worker runs environments under.
pub const MONITOR_CLOUD_HYPERVISOR: &str = "cloud-hypervisor";
pub const MONITOR_KRUN: &str = "krun";

/// An amount of CPU and memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    pub cpus: i64,
    pub memory_mib: i64,
}

/// The body of every non-2xx response: a message for people and a code
/// for programs (the server's `http::code`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    pub error: String,
    #[serde(default)]
    pub code: String,
}

/// Sent once, with the bootstrap token, to exchange it for a credential of
/// the worker's own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterWorker {
    pub name: String,
    pub labels: BTreeMap<String, String>,
}

/// The reply to RegisterWorker. The credential is shown once; the server
/// keeps only its hash.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerCredential {
    pub id: String,
    pub credential: String,
}

/// Everything the server wants a worker to hold. Version increases whenever
/// the set changes, and a worker passes back the last one it saw so the
/// server can hold the request until there is something new.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DesiredSet {
    pub version: i64,
    #[serde(default, deserialize_with = "null")]
    pub environments: Vec<EnvironmentSpec>,
    /// Copies of images the worker should delete from its store. One an
    /// environment still uses is kept, and stays asked for.
    #[serde(default, deserialize_with = "null")]
    pub remove_images: Vec<ImageCopy>,
    /// Environments the worker runs that the server has no record of, which
    /// it should power off and delete, disks and all.
    #[serde(default, deserialize_with = "null")]
    pub remove_environments: Vec<String>,
    /// The host of Hangar's own registry, when it has one. An image named
    /// on it is pulled through the server's worker API, at REGISTRY_PATH,
    /// with the worker's own credential.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub registry: String,
}

/// Where the server serves its registry to workers: the distribution API's
/// /v2/ is beneath it.
pub const REGISTRY_PATH: &str = "/api/worker/v1/registry";

/// One environment as a worker needs to see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentSpec {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub desired: DesiredState,
    #[serde(default)]
    pub spec: Spec,
    /// The copy of its image the environment boots from at its next start,
    /// keeping its disks: a newer copy to upgrade to, or the one its
    /// rollback copy of the writable disk was taken from, to go back to.
    /// The empty digest names a copy taken before digests were recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_digest: Option<String>,
    /// Discards the copy of the writable disk kept from before an upgrade,
    /// keeping the upgrade.
    #[serde(default, skip_serializing_if = "is_false")]
    pub drop_rollback: bool,
}

/// A newer copy of an environment's image than the one it is pinned to,
/// held by its worker, and what upgrading to it would do to the changes in
/// its writable layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageUpdate {
    pub digest: String,
    /// The writable layer has been compared with both copies, which happens
    /// while the environment is stopped.
    pub checked: bool,
    /// Files the environment has changed, added or deleted that the newer
    /// copy also changes; more_conflicts counts those left out of the list.
    #[serde(
        default,
        deserialize_with = "null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub conflicts: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub more_conflicts: i64,
    /// One of them is the package manager's database.
    #[serde(default, skip_serializing_if = "is_false")]
    pub packages: bool,
    /// Why the comparison could not be made.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// What an environment's next start does to its image.
pub const IMAGE_CHANGE_UPGRADE: &str = "upgrade";
pub const IMAGE_CHANGE_ROLLBACK: &str = "rollback";

/// The copy of an environment's writable disk kept from before it was
/// upgraded, which going back to digest restores.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRollback {
    pub digest: String,
    /// RFC 3339.
    pub at: String,
    pub size_bytes: i64,
}

/// Everything a worker is holding, and what it can offer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub capacity: Resources,
    pub labels: BTreeMap<String, String>,
    pub environments: Vec<ObservedEnvironment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<WorkerStats>,
    pub images: Vec<LocalImage>,
    /// What environments with a virtual GPU render with here; none on a
    /// worker that offers none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuInfo>,
    /// What runs its environments.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub monitor: String,
    /// The worker's build, for people.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// What it can do beyond what its API level promises (`feature`).
    #[serde(
        default,
        deserialize_with = "null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub features: Vec<String>,
}

/// What a worker's virtual GPUs render with, as its GPU backend found when
/// the worker started.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuInfo {
    /// What the worker's configuration asks for: auto, hardware or
    /// software. device is the GPU it names, if any.
    pub renderer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gl: Option<GpuRenderer>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub gl_error: String,
    /// The devices Venus chooses among.
    #[serde(
        default,
        deserialize_with = "null",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub vulkan: Vec<GpuDevice>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub vulkan_error: String,
    /// Why the worker offers no virtual GPU.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unavailable: String,
}

/// An OpenGL renderer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuRenderer {
    pub vendor: String,
    pub renderer: String,
    pub version: String,
    /// Whether it renders on the host's CPU.
    pub software: bool,
}

/// A Vulkan device.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuDevice {
    pub name: String,
    pub software: bool,
}

/// One environment as the worker finds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservedEnvironment {
    pub id: String,
    pub phase: Phase,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// How far a starting environment has got.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<EnvironmentStats>,
    /// The copy of its image it boots from, fixed when it first starts.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub image_digest: String,
    /// image_digest is known, which it is once the environment has first
    /// started, even when it is the empty digest of a copy taken before
    /// digests were recorded.
    #[serde(default, skip_serializing_if = "is_false")]
    pub image_pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_update: Option<ImageUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_rollback: Option<ImageRollback>,
}

/// One step of starting an environment. They happen in the order declared;
/// one with nothing to do is passed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// Fetches the image from its registry, or copies it from the worker's
    /// own source for it.
    Download,
    /// Applies the image's layers into its root filesystem.
    Unpack,
    /// Copies the writable disk before it is upgraded, to roll back to.
    Snapshot,
    /// Makes the environment's disks, the first time.
    Disks,
    /// Boots or resumes the machine, until its agent answers.
    Boot,
    /// Waits for the guest's network and name resolver.
    Network,
    /// Waits for the owner's profile to reach the guest.
    Profile,
    /// Applies the template: the hostname and the clones.
    Workspace,
    /// A step this side does not know, after those it does.
    #[serde(other)]
    Other,
}

/// The step a starting environment is on and, where it can be measured,
/// how much of it is done: done of total in unit, "bytes" or "objects",
/// going at rate a second lately. A zero total cannot be measured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub step: Step,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub done: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub total: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub unit: String,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub rate: f64,
}

/// What an environment is using, measured by its worker. Rates are per
/// second, averaged since the previous measurement.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentStats {
    pub cpu_percent: f64,
    pub support_cpus: f64,
    pub gpu_cpus: f64,
    pub memory_used_mib: i64,
    pub memory_total_mib: i64,
    pub memory_cached_mib: i64,
    pub memory_locked_mib: i64,
    pub host_memory_mib: i64,
    pub monitor_memory_mib: i64,
    pub support_memory_mib: i64,
    pub machine_memory_mib: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub machine_memory_min_mib: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub machine_memory_max_mib: i64,
    pub swap_total_mib: i64,
    pub swap_used_mib: i64,
    pub swap_compressed_mib: i64,
    pub swap_pool_mib: i64,
    pub swap_disk_bytes: i64,
    pub swap_in_bps: f64,
    pub swap_out_bps: f64,
    pub disk_used_bytes: i64,
    pub disk_read_bps: f64,
    pub disk_write_bps: f64,
    pub net_rx_bps: f64,
    pub net_tx_bps: f64,
}

/// The load on a worker's machine as a whole.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerStats {
    pub cpu_percent: f64,
    pub support_cpus: f64,
    pub gpu_cpus: f64,
    pub load1: f64,
    pub memory_used_mib: i64,
    pub memory_total_mib: i64,
    pub guest_memory_mib: i64,
    pub monitor_memory_mib: i64,
    pub support_memory_mib: i64,
    pub swap_used_mib: i64,
    pub swap_total_mib: i64,
    pub guest_swap_mib: i64,
    pub guest_swap_pool_mib: i64,
    pub guest_swap_disk_bytes: i64,
    pub disk_used_bytes: i64,
    pub disk_total_bytes: i64,
}

/// One copy of an image in a worker's store: the reference a template
/// gives, and the digest of what it named when the copy was taken.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ImageCopy {
    pub r#ref: String,
    pub digest: String,
}

/// Whether a copy of an image can be booted from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageState {
    #[default]
    Ready,
    /// Being pulled or copied in.
    Fetching,
    #[serde(other)]
    Other,
}

/// A copy of an image a worker holds in its own store.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalImage {
    pub r#ref: String,
    /// The manifest's digest for an image pulled from a registry, and
    /// "build:" and the build's ID for one copied from a local build.
    pub digest: String,
    pub size_bytes: i64,
    pub state: ImageState,
    /// The environments on the worker using it.
    pub environments: Vec<String>,
    /// Whether the copy is of what the reference named when the worker last
    /// looked. New environments are made from the current copy.
    pub current: bool,
}
