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
