//! A JSON state file: every row as the Postgres tables hold it. A server
//! given one imports it into its SQLite database (`lite::open`), and a new
//! database starts with the rows of `State::new`.

use serde::{Deserialize, Serialize};

use super::rows::{
    AccessToken, AgentSession, AuditEvent, Collaborator, Conflict, Copy, DnsRecord, Environment,
    EnvironmentCopy, EnvironmentRemoval, EnvironmentRouted, HostSession, ImageRemoval,
    ImageRepository, ImageTag, LoginKey, Manifest, ManifestRef, Pack, PackDefault, PackPath,
    RegistryBlob, RegistryUpload, RepositoryBlob, RepositoryCollaborator, ServerKey, Session,
    SignInGrant, SshKey, Task, Team, TeamMember, Template, TlsEntry, TlsLock, User, Worker,
};
use super::rows::{Link, LinkPerson, LinkRecord, LinkSettings};

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
    /// The last revision an environment was given.
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
    pub link_settings: Vec<LinkSettings>,
}

impl State {
    /// A state as a new database starts: Hangar's own users and the
    /// profile pack, as Postgres's migrations make them.
    pub fn new() -> State {
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
                same_name: None,
                merged: vec![],
                places: vec![],
                away: false,
            }],
            pack_paths: crate::packs::profile_defaults()
                .into_iter()
                .map(|p| PackPath {
                    pack_id: profile,
                    path: p.path,
                    sensitive: p.sensitive,
                    send_every: p.send_every,
                })
                .collect(),
            ..State::default()
        }
    }
}
