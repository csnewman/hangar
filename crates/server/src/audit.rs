//! Who did what: every change a person makes, and each decision Hangar
//! makes on its own, as the Go server's internal/audit records them.

use uuid::Uuid;

use crate::store::rows::AuditEvent;
use crate::store::{Result, Tx};

/// Who acts: a person, a worker, or someone not signed in.
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub user: Option<Uuid>,
    pub worker: Option<Uuid>,
    pub name: String,
    pub ip: String,
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
        details: e.details,
    })
    .await
}
