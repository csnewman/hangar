//! Who did what: every change a person makes, and each decision Hangar
//! makes on its own, as the Go server's internal/audit records them.

use uuid::Uuid;

use crate::store::rows::AuditEvent;
use crate::store::{Result, Tx};

/// Who acts: a person, a worker, Hangar itself, or someone not signed in.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub user: Option<Uuid>,
    pub worker: Option<Uuid>,
    pub name: String,
    pub ip: String,
    /// How a person signed in, when it is worth saying: an access token's
    /// name.
    pub via: String,
}

/// The users migration 0001 makes for Hangar acting on its own: the
/// control plane, and placement.
pub const SYSTEM_HANGAR: Uuid = Uuid::from_u128(0x00000000_0000_4000_8000_000000000001);
pub const SYSTEM_PLACEMENT: Uuid = Uuid::from_u128(0x00000000_0000_4000_8000_000000000002);

/// Hangar itself acting.
pub fn system() -> Actor {
    Actor {
        user: Some(SYSTEM_HANGAR),
        name: "hangar".into(),
        ..Actor::default()
    }
}

/// Placement deciding.
pub fn placement() -> Actor {
    Actor {
        user: Some(SYSTEM_PLACEMENT),
        name: "hangar:placement".into(),
        ..Actor::default()
    }
}

/// What was done, to what, and what else it concerns.
pub struct Event {
    pub action: &'static str,
    pub target: (&'static str, String, String),
    pub related: Vec<(&'static str, String)>,
    pub details: serde_json::Value,
}

impl Event {
    /// action, done to the target of type, id and name.
    pub fn new(action: &'static str, kind: &'static str, id: impl ToString, name: &str) -> Event {
        Event {
            action,
            target: (kind, id.to_string(), name.to_string()),
            related: Vec::new(),
            details: serde_json::json!({}),
        }
    }

    pub fn related(mut self, kind: &'static str, id: impl ToString) -> Event {
        self.related.push((kind, id.to_string()));
        self
    }

    pub fn details(mut self, details: serde_json::Value) -> Event {
        self.details = details;
        self
    }
}

/// Records e, by actor, in tx: it is kept only if tx commits.
pub async fn record(tx: &mut Tx, actor: &Actor, e: Event) -> Result<()> {
    let mut subjects = vec![format!("{}:{}", e.target.0, e.target.1)];
    subjects.extend(
        e.related
            .iter()
            .filter(|(_, id)| !id.is_empty())
            .map(|(kind, id)| format!("{kind}:{id}")),
    );
    if let Some(u) = actor.user {
        subjects.push(format!("actor:{u}"));
    }
    if let Some(w) = actor.worker {
        subjects.push(format!("worker:{w}"));
    }
    let mut details = e.details;
    if !actor.via.is_empty()
        && let Some(o) = details.as_object_mut()
    {
        o.insert("via".into(), serde_json::json!(actor.via));
    }
    tx.record(AuditEvent {
        id: 0,
        at: chrono::Utc::now(),
        actor_user: actor.user,
        actor_worker: actor.worker,
        actor_name: actor.name.clone(),
        source_ip: actor.ip.clone(),
        action: e.action.to_string(),
        target_type: e.target.0.to_string(),
        target_id: e.target.1,
        target_name: e.target.2,
        subjects,
        details,
    })
    .await
}

/// An event as it is read back, with who acted's kind.
pub struct Entry {
    pub event: AuditEvent,
    pub actor_kind: String,
}

/// Events about every one of subjects, newest first, before an ID if
/// given, at most limit (100 when not given or out of 1 to 500). An
/// administrator sees every event, anyone else those about what they own,
/// about them, or by them.
pub async fn list(
    store: &crate::store::Store,
    p: &crate::users::Principal,
    subjects: &[String],
    before: Option<i64>,
    limit: Option<i64>,
) -> Result<Vec<Entry>> {
    let mine = [
        format!("owner:{}", p.user_id),
        format!("user:{}", p.user_id),
        format!("actor:{}", p.user_id),
    ];
    let q = crate::store::audit::Query {
        subjects,
        any_of: (!p.admin).then_some(&mine[..]),
        before: before.filter(|b| *b > 0).unwrap_or(i64::MAX),
        limit: limit.filter(|l| (1..=500).contains(l)).unwrap_or(100),
    };
    Ok(store
        .begin()
        .await?
        .audit_events(&q)
        .await?
        .into_iter()
        .map(|(event, actor_kind)| Entry { event, actor_kind })
        .collect())
}
