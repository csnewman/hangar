//! The store's rows: the Postgres tables' columns, which the JSON file
//! keeps as they are.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `users`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    /// An argon2id hash in PHC string form; None for someone with no
    /// password of Hangar's.
    pub password_hash: Option<String>,
    pub is_admin: bool,
    /// person, or system: Hangar itself acting, whom nobody signs in as.
    pub kind: String,
    pub disabled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// system, light or dark.
    pub theme: String,
}

/// `sessions`: a signed-in browser, by the SHA-256 of its cookie's token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(with = "hex_bytes")]
    pub token_hash: Vec<u8>,
    pub user_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// `access_tokens`, by the SHA-256 of the token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    #[serde(with = "hex_bytes")]
    pub token_hash: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// `audit_events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor_user: Option<Uuid>,
    pub actor_worker: Option<Uuid>,
    pub actor_name: String,
    pub source_ip: String,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub target_name: String,
    /// type:id tags for everything the event concerns.
    pub subjects: Vec<String>,
    pub details: serde_json::Value,
}

/// Bytes as hex in the JSON file.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
        let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
        s.serialize_str(&hex)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let hex = String::deserialize(d)?;
        (0..hex.len())
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(hex.get(i..i + 2).unwrap_or(""), 16)
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
}

/// `workers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worker {
    pub id: Uuid,
    pub name: String,
    #[serde(with = "hex_bytes")]
    pub credential_hash: Vec<u8>,
    /// Its labels, a JSON object of strings.
    pub labels: serde_json::Value,
    pub cpus: i32,
    pub memory_mib: i32,
    /// Environments it reports that have no row: a JSON array of IDs.
    pub unknown: serde_json::Value,
    pub stats: Option<serde_json::Value>,
    pub images: serde_json::Value,
    pub gpu: Option<serde_json::Value>,
    /// Bumped with every change to what it is asked to run.
    pub desired_version: i64,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub monitor: String,
    /// Its build, as it reports it.
    pub version: String,
    /// What it can do beyond its API level (`hangar_api::feature`).
    pub features: Vec<String>,
}

/// `environments`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    pub id: Uuid,
    pub short_id: String,
    pub owner_id: Uuid,
    pub name: String,
    pub template_id: Option<Uuid>,
    pub template_name: String,
    pub template_revision: i64,
    /// What it runs: hangar_api's Spec.
    pub spec: serde_json::Value,
    pub image: String,
    pub image_digest: String,
    pub image_pin_want: Option<String>,
    pub image_drop_rollback: bool,
    pub image_update: Option<serde_json::Value>,
    pub image_rollback: Option<serde_json::Value>,
    pub ports_public: bool,
    pub cpus: i32,
    pub memory_mib: i32,
    /// running, stopped, suspended or deleted.
    pub desired: String,
    pub worker_id: Option<Uuid>,
    pub phase: String,
    pub reason: String,
    pub stats: Option<serde_json::Value>,
    pub progress: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `worker_image_removals`: a copy of an image a worker is to delete.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageRemoval {
    pub worker_id: Uuid,
    pub r#ref: String,
    pub digest: String,
}

/// `worker_environment_removals`: an environment a worker holds and is to
/// forget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentRemoval {
    pub worker_id: Uuid,
    pub environment: String,
}

/// `templates`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Template {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub team_id: Option<Uuid>,
    pub name: String,
    pub description: String,
    /// private or shared.
    pub visibility: String,
    /// What it makes: hangar_api's TemplateSpec.
    pub spec: serde_json::Value,
    /// Bumped whenever its spec changes.
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `teams`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

/// `team_members`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamMember {
    pub team_id: Uuid,
    pub user_id: Uuid,
    /// viewer, member or admin.
    pub role: String,
}

/// `template_collaborators` (who: a user) and `template_team_collaborators`
/// (who: a team).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collaborator {
    pub template_id: Uuid,
    pub who: Uuid,
}

/// `environment_tasks`: what an environment is for, set by its coding agent
/// or its owner. The one not done is its task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub text: String,
    /// agent or user.
    pub set_by: String,
    pub set_at: DateTime<Utc>,
    pub done_at: Option<DateTime<Utc>>,
}

/// `agent_sessions`: a coding agent's session in an environment, as its
/// guest agent last reported it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub environment_id: Uuid,
    pub id: String,
    pub agent: String,
    pub terminal: String,
    pub state: String,
    pub step: String,
    pub message: String,
    pub since: DateTime<Utc>,
    /// When its owner last looked at it.
    pub seen_at: Option<DateTime<Utc>>,
}

/// `editor_sessions`: a ticket or cookie for one environment's hosts, by
/// its SHA-256, opened from a Hangar session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSession {
    #[serde(with = "hex_bytes")]
    pub token_hash: Vec<u8>,
    #[serde(with = "hex_bytes")]
    pub session_hash: Vec<u8>,
    pub environment_id: Uuid,
    /// A ticket, redeemed once for a cookie; else the cookie.
    pub ticket: bool,
    pub expires_at: DateTime<Utc>,
}

/// `ssh_keys`: a person's own SSH key, its private half sealed, which
/// signs for them through their environments' agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    /// In authorized_keys form, with the name as its comment.
    pub public_key: String,
    #[serde(with = "hex_bytes")]
    pub private_key: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

/// `ssh_login_keys`: a public key its owner signs in to the SSH gateway
/// with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub public_key: String,
    pub fingerprint: String,
    pub created_at: DateTime<Utc>,
}

/// `server_keys`: a key of the server's own, such as the SSH gateway's
/// host key, sealed when the server has a secret key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerKey {
    pub name: String,
    #[serde(with = "hex_bytes")]
    pub value: Vec<u8>,
    pub sealed: bool,
    pub created_at: DateTime<Utc>,
}

/// `packs`: a named list of paths, a person's, a team's, or built in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pack {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub owner_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub builtin: bool,
    /// listed, owner, team or everyone.
    pub attach: String,
    /// all_squash, root_squash or no_root_squash.
    pub squash: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `pack_paths`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackPath {
    pub pack_id: Uuid,
    pub path: String,
    pub sensitive: bool,
}

/// `copies`: one set of a pack's files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Copy {
    pub id: Uuid,
    pub pack_id: Uuid,
    pub owner_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    /// A person's own, one each.
    pub personal: bool,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `pack_defaults`: the copy a person uses of a pack unless told otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackDefault {
    pub user_id: Uuid,
    pub pack_id: Uuid,
    pub copy_id: Uuid,
}

/// `environment_copies`: the copy an environment uses of a pack, chosen on
/// it or kept from when it was first given one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentCopy {
    pub environment_id: Uuid,
    pub pack_id: Uuid,
    pub copy_id: Uuid,
    pub chosen: bool,
}

/// `environment_routed`: a copy an environment's agent routes to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentRouted {
    pub environment_id: Uuid,
    pub copy_id: Uuid,
}

/// `environment_conflicts`: a path an environment had a file of its own
/// at, differing from its copy's, when the path became shared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    pub environment_id: Uuid,
    pub copy_id: Uuid,
    pub path: String,
    pub found_at: DateTime<Utc>,
    /// shared or environment, until the environment's agent has done it.
    pub resolution: Option<String>,
}

/// `image_repositories`: a repository of the registry, a person's or a
/// team's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageRepository {
    pub id: Uuid,
    pub path: String,
    pub owner_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    /// private or shared.
    pub visibility: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `image_repository_collaborators` (who: a user) and
/// `image_repository_team_collaborators` (who: a team).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepositoryCollaborator {
    pub repository_id: Uuid,
    pub who: Uuid,
}

/// `registry_blobs`: a blob the blob store holds, by digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryBlob {
    pub digest: String,
    pub size: i64,
    pub created_at: DateTime<Utc>,
}

/// `image_repository_blobs`: a blob a repository may use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryBlob {
    pub repository_id: Uuid,
    pub digest: String,
    pub created_at: DateTime<Utc>,
}

/// `image_manifests`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub repository_id: Uuid,
    pub digest: String,
    pub media_type: String,
    pub artifact_type: String,
    /// The manifest it refers to, by digest.
    pub subject: Option<String>,
    pub annotations: Option<serde_json::Value>,
    #[serde(with = "hex_bytes")]
    pub content: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

/// `image_manifest_refs`: a blob or manifest a manifest names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestRef {
    pub repository_id: Uuid,
    pub digest: String,
    pub r#ref: String,
}

/// `image_tags`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageTag {
    pub repository_id: Uuid,
    pub tag: String,
    pub digest: String,
    pub updated_at: DateTime<Utc>,
}

/// `registry_uploads`: an upload begun, its chunks in the blob store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryUpload {
    pub id: Uuid,
    pub repository_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// `dns_records`: a record the zone serves beyond those its configuration
/// implies, an ACME challenge's TXT record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsRecord {
    /// Fully qualified, lower case, without the trailing dot.
    pub name: String,
    pub r#type: String,
    pub value: String,
    /// Seconds.
    pub ttl: i32,
    pub created_at: DateTime<Utc>,
}

/// `tls_storage`: what certificate management keeps, by key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsEntry {
    pub key: String,
    #[serde(with = "hex_bytes")]
    pub value: Vec<u8>,
    pub modified: DateTime<Utc>,
}

/// `tls_locks`: held while a certificate is obtained or renewed, so two
/// servers do not both do it; one whose holder died expires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsLock {
    pub name: String,
    pub expires_at: DateTime<Utc>,
}

/// `sign_in_grants`: an app signing in through the browser, waiting for a
/// person to approve it and then to collect its token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignInGrant {
    pub id: Uuid,
    /// The SHA-256 of the device code the app holds.
    #[serde(with = "hex_bytes")]
    pub device_hash: Vec<u8>,
    /// XXXX-XXXX.
    pub user_code: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub approved_by: Option<Uuid>,
    pub denied: bool,
}
