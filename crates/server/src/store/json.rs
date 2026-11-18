//! The JSON-file backend, for one server on one machine: the whole state in
//! memory, one transaction at a time, each on a copy that commit swaps in
//! and writes to the file atomically (a temporary file beside it, synced,
//! then renamed over it). One process holds the file: a lock file beside it
//! says which.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedMutexGuard};

use super::rows::{
    AccessToken, AgentSession, AuditEvent, Collaborator, Conflict, Copy, DnsRecord, Environment,
    EnvironmentCopy, EnvironmentRemoval, EnvironmentRouted, HostSession, ImageRemoval,
    ImageRepository, ImageTag, LoginKey, Manifest, ManifestRef, Pack, PackDefault, PackPath,
    RegistryBlob, RegistryUpload, RepositoryBlob, RepositoryCollaborator, ServerKey, Session,
    SignInGrant, SshKey, Task, Team, TeamMember, Template, TlsEntry, TlsLock, User, Worker,
};
use super::rows::{Link, LinkPerson, LinkRecord};
use super::{Error, Result};

/// Everything the server keeps, as the Postgres tables hold it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub users: Vec<User>,
    pub sessions: Vec<Session>,
    pub access_tokens: Vec<AccessToken>,
    pub audit_events: Vec<AuditEvent>,
    /// The ID the next audit event takes, as Postgres's bigserial gives one.
    pub next_audit_id: i64,
    /// The last revision an environment was given (`Tx::revise`).
    pub last_revision: i64,
    pub workers: Vec<Worker>,
    pub environments: Vec<Environment>,
    pub worker_image_removals: Vec<ImageRemoval>,
    pub worker_environment_removals: Vec<EnvironmentRemoval>,
    pub templates: Vec<Template>,
    pub template_collaborators: Vec<Collaborator>,
    pub template_team_collaborators: Vec<Collaborator>,
    pub teams: Vec<Team>,
    pub team_members: Vec<TeamMember>,
    pub environment_tasks: Vec<Task>,
    pub agent_sessions: Vec<AgentSession>,
    pub editor_sessions: Vec<HostSession>,
    pub ssh_keys: Vec<SshKey>,
    pub ssh_login_keys: Vec<LoginKey>,
    pub server_keys: Vec<ServerKey>,
    pub packs: Vec<Pack>,
    pub pack_paths: Vec<PackPath>,
    pub copies: Vec<Copy>,
    pub pack_defaults: Vec<PackDefault>,
    pub environment_copies: Vec<EnvironmentCopy>,
    pub environment_routed: Vec<EnvironmentRouted>,
    pub environment_conflicts: Vec<Conflict>,
    pub image_repositories: Vec<ImageRepository>,
    pub image_repository_collaborators: Vec<RepositoryCollaborator>,
    pub image_repository_team_collaborators: Vec<RepositoryCollaborator>,
    pub registry_blobs: Vec<RegistryBlob>,
    pub image_repository_blobs: Vec<RepositoryBlob>,
    pub image_manifests: Vec<Manifest>,
    pub image_manifest_refs: Vec<ManifestRef>,
    pub image_tags: Vec<ImageTag>,
    pub registry_uploads: Vec<RegistryUpload>,
    pub dns_records: Vec<DnsRecord>,
    pub tls_storage: Vec<TlsEntry>,
    pub tls_locks: Vec<TlsLock>,
    pub sign_in_grants: Vec<SignInGrant>,
    pub link_persons: Vec<LinkPerson>,
    pub links: Vec<Link>,
    pub link_records: Vec<LinkRecord>,
}

impl State {
    /// A state as a new database starts: Hangar's own users and nothing
    /// else, as migration 0001 makes them.
    fn new() -> State {
        let user = |id, username: &str, display_name: &str| User {
            id,
            username: username.into(),
            display_name: display_name.into(),
            password_hash: None,
            is_admin: false,
            kind: "system".into(),
            disabled_at: None,
            created_at: chrono::Utc::now(),
            theme: "system".into(),
        };
        let now = chrono::Utc::now();
        let profile = uuid::Uuid::new_v4();
        State {
            users: vec![
                user(crate::audit::SYSTEM_HANGAR, "hangar", "Hangar"),
                user(
                    crate::audit::SYSTEM_PLACEMENT,
                    "hangar:placement",
                    "Hangar placement",
                ),
            ],
            packs: vec![Pack {
                id: profile,
                name: "Profile".into(),
                description: "Claude's settings and sign-in, git and VS Code settings, and CLI sign-ins, in every environment.".into(),
                owner_id: None,
                team_id: None,
                builtin: true,
                attach: "everyone".into(),
                squash: "all_squash".into(),
                created_at: now,
                updated_at: now,
            }],
            pack_paths: crate::packs::profile_defaults()
                .into_iter()
                .map(|p| PackPath {
                    pack_id: profile,
                    path: p.path,
                    sensitive: p.sensitive,
                })
                .collect(),
            ..State::default()
        }
    }
}

/// The file, open.
pub struct File {
    path: PathBuf,
    state: Arc<Mutex<State>>,
    /// Held while the server runs, so a second one fails to start.
    _lock: std::fs::File,
}

/// A transaction: the state, held, and the copy it works on.
pub struct Tx {
    held: OwnedMutexGuard<State>,
    pub(super) work: State,
    path: PathBuf,
}

impl File {
    /// Opens the state at path, an empty one if there is no file yet.
    pub fn open(path: &Path) -> Result<File> {
        let lock_path = path.with_extension("lock");
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| Error(format!("{}: {e}", lock_path.display())))?;
        lock.try_lock().map_err(|_| {
            Error(format!(
                "{} is held by another server: one server keeps a state file",
                path.display()
            ))
        })?;
        let state = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Error(format!("{}: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::new(),
            Err(e) => return Err(Error(format!("{}: {e}", path.display()))),
        };
        Ok(File {
            path: path.to_path_buf(),
            state: Arc::new(Mutex::new(state)),
            _lock: lock,
        })
    }

    /// Waits for the state, and starts a transaction on a copy of it.
    pub async fn begin(&self) -> Tx {
        let held = self.state.clone().lock_owned().await;
        let work = held.clone();
        Tx {
            held,
            work,
            path: self.path.clone(),
        }
    }
}

impl Tx {
    /// Raises an environment's revision past every other: this file's
    /// transactions take turns, so its revisions are in commit order.
    pub fn revise(&mut self, id: uuid::Uuid) {
        self.work.last_revision += 1;
        let r = self.work.last_revision;
        if let Some(e) = self.work.environments.iter_mut().find(|e| e.id == id) {
            e.revision = r;
        }
    }

    /// Writes the copy to the file, then makes it the state.
    pub fn commit(mut self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.work)
            .map_err(|e| Error(format!("writing the state: {e}")))?;
        let tmp = self.path.with_extension("json.new");
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        };
        write().map_err(|e| Error(format!("{}: {e}", self.path.display())))?;
        *self.held = std::mem::take(&mut self.work);
        Ok(())
    }
}
