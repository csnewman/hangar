//! Templates, their collaborators, and teams with their members: each read
//! and write for both backends. Who may see or change what is the domain's
//! (templates.rs, teams.rs), worked out from these rows.

use std::collections::HashMap;

use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use super::rows::{Collaborator, Team, TeamMember, Template};
use super::{Backend, Result, Tx};

const TEMPLATE_COLUMNS: &str =
    "id, owner_id, team_id, name, description, visibility, spec, revision, created_at, updated_at";

fn template(r: &PgRow) -> Result<Template> {
    Ok(Template {
        id: r.try_get("id")?,
        owner_id: r.try_get("owner_id")?,
        team_id: r.try_get("team_id")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        visibility: r.try_get("visibility")?,
        spec: r.try_get("spec")?,
        revision: r.try_get("revision")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

fn team(r: &PgRow) -> Result<Team> {
    Ok(Team {
        id: r.try_get("id")?,
        slug: r.try_get("slug")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        created_at: r.try_get("created_at")?,
    })
}

/// Whether e is a unique index refusing a row.
fn taken(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.is_unique_violation())
}

impl Tx {
    pub async fn templates(&mut self) -> Result<Vec<Template>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!("SELECT {TEMPLATE_COLUMNS} FROM templates"))
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(template)
                .collect(),
            Backend::Json(tx) => Ok(tx.work.templates.clone()),
        }
    }

    pub async fn template(&mut self, id: Uuid) -> Result<Option<Template>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {TEMPLATE_COLUMNS} FROM templates WHERE id = $1 FOR UPDATE"
            ))
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| template(&r))
            .transpose(),
            Backend::Json(tx) => Ok(tx.work.templates.iter().find(|t| t.id == id).cloned()),
        }
    }

    /// Adds a template; false if its owner or team has one by its name.
    pub async fn insert_template(&mut self, t: &Template) -> Result<bool> {
        self.signal(super::signals::Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => {
                let r = sqlx::query(&format!(
                    "INSERT INTO templates ({TEMPLATE_COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
                ))
                .bind(t.id)
                .bind(t.owner_id)
                .bind(t.team_id)
                .bind(&t.name)
                .bind(&t.description)
                .bind(&t.visibility)
                .bind(&t.spec)
                .bind(t.revision)
                .bind(t.created_at)
                .bind(t.updated_at)
                .execute(&mut **tx)
                .await;
                match r {
                    Ok(_) => Ok(true),
                    Err(e) if taken(&e) => Ok(false),
                    Err(e) => Err(e.into()),
                }
            }
            Backend::Json(tx) => {
                if tx.work.templates.iter().any(|x| same_name(x, t)) {
                    return Ok(false);
                }
                tx.work.templates.push(t.clone());
                Ok(true)
            }
        }
    }

    /// Writes a template's every column; false if its owner or team has
    /// another by its name.
    pub async fn update_template(&mut self, t: &Template) -> Result<bool> {
        self.signal(super::signals::Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => {
                let r = sqlx::query(
                    "UPDATE templates SET owner_id = $2, team_id = $3, name = $4, description = $5,
                        visibility = $6, spec = $7, revision = $8, updated_at = $9 WHERE id = $1",
                )
                .bind(t.id)
                .bind(t.owner_id)
                .bind(t.team_id)
                .bind(&t.name)
                .bind(&t.description)
                .bind(&t.visibility)
                .bind(&t.spec)
                .bind(t.revision)
                .bind(t.updated_at)
                .execute(&mut **tx)
                .await;
                match r {
                    Ok(_) => Ok(true),
                    Err(e) if taken(&e) => Ok(false),
                    Err(e) => Err(e.into()),
                }
            }
            Backend::Json(tx) => {
                if tx
                    .work
                    .templates
                    .iter()
                    .any(|x| x.id != t.id && same_name(x, t))
                {
                    return Ok(false);
                }
                if let Some(x) = tx.work.templates.iter_mut().find(|x| x.id == t.id) {
                    *x = t.clone();
                }
                Ok(true)
            }
        }
    }

    /// Deletes a template, and its collaborators with it; its environments
    /// keep its name and lose its ID.
    pub async fn delete_template(&mut self, id: Uuid) -> Result<()> {
        self.signal(super::signals::Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM templates WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => {
                tx.work.templates.retain(|t| t.id != id);
                tx.work
                    .template_collaborators
                    .retain(|c| c.template_id != id);
                tx.work
                    .template_team_collaborators
                    .retain(|c| c.template_id != id);
                for e in tx
                    .work
                    .environments
                    .iter_mut()
                    .filter(|e| e.template_id == Some(id))
                {
                    e.template_id = None;
                }
            }
        }
        Ok(())
    }

    /// Every template's collaborating users and teams.
    pub async fn collaborators(&mut self) -> Result<(Vec<Collaborator>, Vec<Collaborator>)> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let pair = |r: &PgRow| -> Result<Collaborator> {
                    Ok(Collaborator {
                        template_id: r.try_get(0)?,
                        who: r.try_get(1)?,
                    })
                };
                let users = sqlx::query("SELECT template_id, user_id FROM template_collaborators")
                    .fetch_all(&mut **tx)
                    .await?
                    .iter()
                    .map(pair)
                    .collect::<Result<_>>()?;
                let teams =
                    sqlx::query("SELECT template_id, team_id FROM template_team_collaborators")
                        .fetch_all(&mut **tx)
                        .await?
                        .iter()
                        .map(pair)
                        .collect::<Result<_>>()?;
                Ok((users, teams))
            }
            Backend::Json(tx) => Ok((
                tx.work.template_collaborators.clone(),
                tx.work.template_team_collaborators.clone(),
            )),
        }
    }

    /// Makes users and teams a template's collaborators, in place of any
    /// it had.
    pub async fn set_collaborators(
        &mut self,
        id: Uuid,
        users: &[Uuid],
        teams: &[Uuid],
    ) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM template_collaborators WHERE template_id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
                sqlx::query(
                    "INSERT INTO template_collaborators (template_id, user_id) SELECT $1, unnest($2::uuid[])",
                )
                .bind(id)
                .bind(users)
                .execute(&mut **tx)
                .await?;
                sqlx::query("DELETE FROM template_team_collaborators WHERE template_id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
                sqlx::query(
                    "INSERT INTO template_team_collaborators (template_id, team_id) SELECT $1, unnest($2::uuid[])",
                )
                .bind(id)
                .bind(teams)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                tx.work
                    .template_collaborators
                    .retain(|c| c.template_id != id);
                tx.work
                    .template_team_collaborators
                    .retain(|c| c.template_id != id);
                for u in users {
                    tx.work.template_collaborators.push(Collaborator {
                        template_id: id,
                        who: *u,
                    });
                }
                for t in teams {
                    tx.work.template_team_collaborators.push(Collaborator {
                        template_id: id,
                        who: *t,
                    });
                }
            }
        }
        Ok(())
    }

    /// How many environments each template made, by template.
    pub async fn template_environments(&mut self) -> Result<HashMap<Uuid, i64>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT template_id, count(*) FROM environments WHERE template_id IS NOT NULL GROUP BY template_id",
            )
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
            .collect(),
            Backend::Json(tx) => {
                let mut out = HashMap::new();
                for t in tx.work.environments.iter().filter_map(|e| e.template_id) {
                    *out.entry(t).or_insert(0) += 1;
                }
                Ok(out)
            }
        }
    }

    pub async fn teams(&mut self) -> Result<Vec<Team>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("SELECT id, slug, name, description, created_at FROM teams")
                    .fetch_all(&mut **tx)
                    .await?
                    .iter()
                    .map(team)
                    .collect()
            }
            Backend::Json(tx) => Ok(tx.work.teams.clone()),
        }
    }

    pub async fn team_members(&mut self) -> Result<Vec<TeamMember>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query("SELECT team_id, user_id, role FROM team_members")
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(|r| {
                    Ok(TeamMember {
                        team_id: r.try_get(0)?,
                        user_id: r.try_get(1)?,
                        role: r.try_get(2)?,
                    })
                })
                .collect(),
            Backend::Json(tx) => Ok(tx.work.team_members.clone()),
        }
    }

    /// Holds a team's row until the transaction ends.
    pub async fn lock_team(&mut self, id: Uuid) -> Result<()> {
        if let Backend::Pg(tx) = &mut self.inner {
            sqlx::query("SELECT 1 FROM teams WHERE id = $1 FOR UPDATE")
                .bind(id)
                .execute(&mut **tx)
                .await?;
        }
        Ok(())
    }

    /// Adds a team; false if one has its slug.
    pub async fn insert_team(&mut self, t: &Team) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let r = sqlx::query(
                    "INSERT INTO teams (id, slug, name, description, created_at) VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(t.id)
                .bind(&t.slug)
                .bind(&t.name)
                .bind(&t.description)
                .bind(t.created_at)
                .execute(&mut **tx)
                .await;
                match r {
                    Ok(_) => Ok(true),
                    Err(e) if taken(&e) => Ok(false),
                    Err(e) => Err(e.into()),
                }
            }
            Backend::Json(tx) => {
                if tx
                    .work
                    .teams
                    .iter()
                    .any(|x| x.slug.to_lowercase() == t.slug.to_lowercase())
                {
                    return Ok(false);
                }
                tx.work.teams.push(t.clone());
                Ok(true)
            }
        }
    }

    pub async fn update_team(&mut self, id: Uuid, name: &str, description: &str) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("UPDATE teams SET name = $2, description = $3 WHERE id = $1")
                    .bind(id)
                    .bind(name)
                    .bind(description)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => {
                if let Some(t) = tx.work.teams.iter_mut().find(|t| t.id == id) {
                    t.name = name.to_string();
                    t.description = description.to_string();
                }
            }
        }
        Ok(())
    }

    /// Deletes a team, its members and its standing as a collaborator.
    pub async fn delete_team(&mut self, id: Uuid) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM teams WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => {
                tx.work.teams.retain(|t| t.id != id);
                tx.work.team_members.retain(|m| m.team_id != id);
                tx.work.template_team_collaborators.retain(|c| c.who != id);
            }
        }
        Ok(())
    }

    /// Gives a user role in a team, joining them to it if they are not in it.
    pub async fn set_team_member(&mut self, team: Uuid, user: Uuid, role: &str) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO team_members (team_id, user_id, role) VALUES ($1, $2, $3)
                     ON CONFLICT (team_id, user_id) DO UPDATE SET role = excluded.role",
                )
                .bind(team)
                .bind(user)
                .bind(role)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                match tx
                    .work
                    .team_members
                    .iter_mut()
                    .find(|m| m.team_id == team && m.user_id == user)
                {
                    Some(m) => m.role = role.to_string(),
                    None => tx.work.team_members.push(TeamMember {
                        team_id: team,
                        user_id: user,
                        role: role.to_string(),
                    }),
                }
            }
        }
        Ok(())
    }

    /// Takes a user out of a team; false if they were not in it.
    pub async fn remove_team_member(&mut self, team: Uuid, user: Uuid) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query(
                "DELETE FROM team_members WHERE team_id = $1 AND user_id = $2",
            )
            .bind(team)
            .bind(user)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                > 0),
            Backend::Json(tx) => {
                let before = tx.work.team_members.len();
                tx.work
                    .team_members
                    .retain(|m| !(m.team_id == team && m.user_id == user));
                Ok(tx.work.team_members.len() < before)
            }
        }
    }

    /// How many image repositories, packs and shared copies each team owns.
    pub async fn team_holdings(&mut self) -> Result<HashMap<Uuid, (i64, i64, i64)>> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let mut out: HashMap<Uuid, (i64, i64, i64)> = HashMap::new();
                for (i, table) in ["image_repositories", "packs", "copies"].iter().enumerate() {
                    let rows = sqlx::query(&format!(
                        "SELECT team_id, count(*) FROM {table} WHERE team_id IS NOT NULL GROUP BY team_id"
                    ))
                    .fetch_all(&mut **tx)
                    .await?;
                    for r in rows {
                        let (team, n): (Uuid, i64) = (r.try_get(0)?, r.try_get(1)?);
                        let e = out.entry(team).or_default();
                        match i {
                            0 => e.0 = n,
                            1 => e.1 = n,
                            _ => e.2 = n,
                        }
                    }
                }
                Ok(out)
            }
            Backend::Json(_) => Ok(HashMap::new()),
        }
    }
}

/// Whether two templates clash by name: the same owner's own, or the same
/// team's.
fn same_name(a: &Template, b: &Template) -> bool {
    a.name == b.name
        && match (a.team_id, b.team_id) {
            (Some(x), Some(y)) => x == y,
            (None, None) => a.owner_id == b.owner_id,
            _ => false,
        }
}
