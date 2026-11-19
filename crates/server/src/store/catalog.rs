//! Templates, their collaborators, and teams with their members: each read
//! and write for both backends. Who may see or change what is the domain's
//! (templates.rs, teams.rs), worked out from these rows.

use std::collections::HashMap;

use sqlx::Row;
use sqlx::postgres::PgRow;
use sqlx::sqlite::SqliteRow;
use uuid::Uuid;

use super::rows::{Collaborator, Team, TeamMember, Template};
use super::{Backend, Result, Tx};

const TEMPLATE_COLUMNS: &str =
    "id, owner_id, team_id, name, description, visibility, spec, revision, created_at, updated_at";

fn template(r: &impl super::Get) -> Result<Template> {
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

fn team(r: &impl super::Get) -> Result<Team> {
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
            Backend::Lite(tx) => sqlx::query(&format!("SELECT {TEMPLATE_COLUMNS} FROM templates"))
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(template)
                .collect(),
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
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {TEMPLATE_COLUMNS} FROM templates WHERE id = $1"
            ))
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| template(&r))
            .transpose(),
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
            Backend::Lite(tx) => {
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
            Backend::Lite(tx) => {
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
            Backend::Lite(tx) => {
                sqlx::query("DELETE FROM templates WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
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
            Backend::Lite(tx) => {
                let pair = |r: &SqliteRow| -> Result<Collaborator> {
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
            Backend::Lite(tx) => {
                sqlx::query("DELETE FROM template_collaborators WHERE template_id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
                sqlx::query(
                    "INSERT INTO template_collaborators (template_id, user_id)
                     SELECT $1, unhex(replace(value, '-', '')) FROM json_each($2)",
                )
                .bind(id)
                .bind(sqlx::types::Json(users))
                .execute(&mut **tx)
                .await?;
                sqlx::query("DELETE FROM template_team_collaborators WHERE template_id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
                sqlx::query(
                    "INSERT INTO template_team_collaborators (template_id, team_id)
                     SELECT $1, unhex(replace(value, '-', '')) FROM json_each($2)",
                )
                .bind(id)
                .bind(sqlx::types::Json(teams))
                .execute(&mut **tx)
                .await?;
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
            Backend::Lite(tx) => sqlx::query(
                "SELECT template_id, count(*) FROM environments WHERE template_id IS NOT NULL GROUP BY template_id",
            )
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|r| Ok((r.try_get(0)?, r.try_get(1)?)))
            .collect()
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
            Backend::Lite(tx) => {
                sqlx::query("SELECT id, slug, name, description, created_at FROM teams")
                    .fetch_all(&mut **tx)
                    .await?
                    .iter()
                    .map(team)
                    .collect()
            }
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
            Backend::Lite(tx) => sqlx::query("SELECT team_id, user_id, role FROM team_members")
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
            Backend::Lite(tx) => {
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
            Backend::Lite(tx) => {
                sqlx::query("UPDATE teams SET name = $2, description = $3 WHERE id = $1")
                    .bind(id)
                    .bind(name)
                    .bind(description)
                    .execute(&mut **tx)
                    .await?;
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
            Backend::Lite(tx) => {
                sqlx::query("DELETE FROM teams WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
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
            Backend::Lite(tx) => {
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
            Backend::Lite(tx) => Ok(sqlx::query(
                "DELETE FROM team_members WHERE team_id = $1 AND user_id = $2",
            )
            .bind(team)
            .bind(user)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                > 0),
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
            Backend::Lite(tx) => {
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
        }
    }
}
