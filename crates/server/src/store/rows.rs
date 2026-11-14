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
    pub suspends: bool,
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
