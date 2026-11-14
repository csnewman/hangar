//! The audit log read back, for both backends: events newest first,
//! narrowed by the things they concern.

use sqlx::Row;

use super::rows::AuditEvent;
use super::{Backend, Result, Tx};

/// Which events to read.
pub struct Query<'a> {
    /// Tags every event must have, as type:id.
    pub subjects: &'a [String],
    /// Tags of which every event must have at least one, if given.
    pub any_of: Option<&'a [String]>,
    /// Only events older than this ID.
    pub before: i64,
    pub limit: i64,
}

impl Tx {
    /// The events q asks for, newest first, each with who acted's kind:
    /// worker, anonymous, or the acting user's (person or system).
    pub async fn audit_events(&mut self, q: &Query<'_>) -> Result<Vec<(AuditEvent, String)>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT e.id, e.at, e.actor_user, e.actor_worker, e.actor_name, e.source_ip, e.action,
                    e.target_type, e.target_id, e.target_name, e.subjects, e.details,
                    CASE WHEN e.actor_worker IS NOT NULL THEN 'worker'
                         WHEN e.actor_user IS NULL THEN 'anonymous'
                         ELSE coalesce(u.kind, 'person') END AS kind
                 FROM audit_events e
                 LEFT JOIN users u ON u.id = e.actor_user
                 WHERE e.id < $1 AND e.subjects @> $2
                    AND ($3::text[] IS NULL OR e.subjects && $3)
                 ORDER BY e.id DESC
                 LIMIT $4",
            )
            .bind(q.before)
            .bind(q.subjects)
            .bind(q.any_of)
            .bind(q.limit)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|r| {
                Ok((
                    AuditEvent {
                        id: r.try_get("id")?,
                        at: r.try_get("at")?,
                        actor_user: r.try_get("actor_user")?,
                        actor_worker: r.try_get("actor_worker")?,
                        actor_name: r.try_get("actor_name")?,
                        source_ip: r.try_get("source_ip")?,
                        action: r.try_get("action")?,
                        target_type: r.try_get("target_type")?,
                        target_id: r.try_get("target_id")?,
                        target_name: r.try_get("target_name")?,
                        subjects: r.try_get("subjects")?,
                        details: r.try_get("details")?,
                    },
                    r.try_get("kind")?,
                ))
            })
            .collect(),
            Backend::Json(tx) => {
                let w = &tx.work;
                let mut out: Vec<(AuditEvent, String)> = w
                    .audit_events
                    .iter()
                    .filter(|e| e.id < q.before)
                    .filter(|e| q.subjects.iter().all(|s| e.subjects.contains(s)))
                    .filter(|e| q.any_of.is_none_or(|any| any.iter().any(|s| e.subjects.contains(s))))
                    .map(|e| {
                        let kind = if e.actor_worker.is_some() {
                            "worker".to_string()
                        } else if let Some(u) = e.actor_user {
                            w.users
                                .iter()
                                .find(|x| x.id == u)
                                .map_or("person".to_string(), |x| x.kind.clone())
                        } else {
                            "anonymous".to_string()
                        };
                        (e.clone(), kind)
                    })
                    .collect();
                out.sort_by(|a, b| b.0.id.cmp(&a.0.id));
                out.truncate(q.limit.max(0) as usize);
                Ok(out)
            }
        }
    }
}
