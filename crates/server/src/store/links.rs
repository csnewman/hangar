//! Linked places: who a person is across places, and the places their
//! account here is linked with, for both backends.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::rows::{Link, LinkPerson};
use super::{Backend, Result, Tx};

const LINK_COLUMNS: &str =
    "user_id, place_id, name, public_key, url, created_at, reached_at, reach_error, heard_at";

fn link(r: &sqlx::postgres::PgRow) -> Result<Link> {
    Ok(Link {
        user_id: r.try_get("user_id")?,
        place_id: r.try_get("place_id")?,
        name: r.try_get("name")?,
        public_key: r.try_get("public_key")?,
        url: r.try_get("url")?,
        created_at: r.try_get("created_at")?,
        reached_at: r.try_get("reached_at")?,
        reach_error: r.try_get("reach_error")?,
        heard_at: r.try_get("heard_at")?,
    })
}

impl Tx {
    /// The person a user is: the ID their links share, their own unless
    /// a link gave them another.
    pub async fn person(&mut self, user: Uuid) -> Result<Uuid> {
        let found = match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query_scalar("SELECT person_id FROM link_persons WHERE user_id = $1")
                    .bind(user)
                    .fetch_optional(&mut **tx)
                    .await?
            }
            Backend::Json(tx) => tx
                .work
                .link_persons
                .iter()
                .find(|p| p.user_id == user)
                .map(|p| p.person_id),
        };
        Ok(found.unwrap_or(user))
    }

    /// Makes a user the person given; false when another user here is
    /// that person already.
    pub async fn set_person(&mut self, user: Uuid, person: Uuid) -> Result<bool> {
        if let Some(other) = self.user_by_person(person).await?
            && other != user
        {
            return Ok(false);
        }
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO link_persons (user_id, person_id) VALUES ($1, $2)
                     ON CONFLICT (user_id) DO UPDATE SET person_id = EXCLUDED.person_id",
                )
                .bind(user)
                .bind(person)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                let persons = &mut tx.work.link_persons;
                persons.retain(|p| p.user_id != user);
                persons.push(LinkPerson {
                    user_id: user,
                    person_id: person,
                });
            }
        }
        Ok(true)
    }

    /// The user here who is a person: the one given that person's ID by a
    /// link, else the one whose own ID it is and who has not been given
    /// another.
    pub async fn user_by_person(&mut self, person: Uuid) -> Result<Option<Uuid>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT user_id FROM link_persons WHERE person_id = $1
                 UNION ALL
                 SELECT id FROM users u WHERE id = $1
                   AND NOT EXISTS (SELECT 1 FROM link_persons p WHERE p.user_id = u.id)
                 LIMIT 1",
            )
            .bind(person)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Json(tx) => {
                let w = &tx.work;
                if let Some(p) = w.link_persons.iter().find(|p| p.person_id == person) {
                    return Ok(Some(p.user_id));
                }
                Ok(w.users
                    .iter()
                    .find(|u| u.id == person && !w.link_persons.iter().any(|p| p.user_id == u.id))
                    .map(|u| u.id))
            }
        }
    }

    /// A user's links, oldest first; every user's when user is None.
    pub async fn links(&mut self, user: Option<Uuid>) -> Result<Vec<Link>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {LINK_COLUMNS} FROM links WHERE $1::uuid IS NULL OR user_id = $1
                 ORDER BY created_at, place_id"
            ))
            .bind(user)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(link)
            .collect(),
            Backend::Json(tx) => {
                let mut out: Vec<Link> = tx
                    .work
                    .links
                    .iter()
                    .filter(|l| user.is_none_or(|u| l.user_id == u))
                    .cloned()
                    .collect();
                out.sort_by_key(|l| (l.created_at, l.place_id));
                Ok(out)
            }
        }
    }

    /// A user's link with a place.
    pub async fn link(&mut self, user: Uuid, place: Uuid) -> Result<Option<Link>> {
        Ok(self
            .links(Some(user))
            .await?
            .into_iter()
            .find(|l| l.place_id == place))
    }

    /// Makes a user's link with a place, or replaces its name, key and
    /// address, which starts what is known of reaching it afresh.
    pub async fn put_link(&mut self, l: &Link) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO links (user_id, place_id, name, public_key, url, created_at)
                     VALUES ($1, $2, $3, $4, $5, $6)
                     ON CONFLICT (user_id, place_id) DO UPDATE SET name = EXCLUDED.name,
                       public_key = EXCLUDED.public_key, url = EXCLUDED.url,
                       reached_at = NULL, reach_error = '', heard_at = NULL",
                )
                .bind(l.user_id)
                .bind(l.place_id)
                .bind(&l.name)
                .bind(&l.public_key)
                .bind(&l.url)
                .bind(l.created_at)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                let links = &mut tx.work.links;
                match links
                    .iter_mut()
                    .find(|o| o.user_id == l.user_id && o.place_id == l.place_id)
                {
                    Some(o) => {
                        o.name = l.name.clone();
                        o.public_key = l.public_key.clone();
                        o.url = l.url.clone();
                        o.reached_at = None;
                        o.reach_error = String::new();
                        o.heard_at = None;
                    }
                    None => links.push(Link {
                        reached_at: None,
                        reach_error: String::new(),
                        heard_at: None,
                        ..l.clone()
                    }),
                }
            }
        }
        Ok(())
    }

    /// Deletes a user's link with a place; false if there was none.
    pub async fn delete_link(&mut self, user: Uuid, place: Uuid) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query(
                "DELETE FROM links WHERE user_id = $1 AND place_id = $2",
            )
            .bind(user)
            .bind(place)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                == 1),
            Backend::Json(tx) => {
                let before = tx.work.links.len();
                tx.work
                    .links
                    .retain(|l| !(l.user_id == user && l.place_id == place));
                Ok(tx.work.links.len() != before)
            }
        }
    }

    /// Records an attempt to reach a linked place: when it answered, or
    /// why it did not.
    pub async fn set_reached(
        &mut self,
        user: Uuid,
        place: Uuid,
        outcome: std::result::Result<DateTime<Utc>, String>,
    ) -> Result<()> {
        let (at, error) = match outcome {
            Ok(at) => (Some(at), String::new()),
            Err(e) => (None, e),
        };
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "UPDATE links SET reached_at = COALESCE($3, reached_at), reach_error = $4
                     WHERE user_id = $1 AND place_id = $2",
                )
                .bind(user)
                .bind(place)
                .bind(at)
                .bind(&error)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                if let Some(l) = tx
                    .work
                    .links
                    .iter_mut()
                    .find(|l| l.user_id == user && l.place_id == place)
                {
                    if at.is_some() {
                        l.reached_at = at;
                    }
                    l.reach_error = error;
                }
            }
        }
        Ok(())
    }

    /// Records a linked place reaching this one.
    pub async fn set_heard(&mut self, user: Uuid, place: Uuid, at: DateTime<Utc>) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("UPDATE links SET heard_at = $3 WHERE user_id = $1 AND place_id = $2")
                    .bind(user)
                    .bind(place)
                    .bind(at)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => {
                if let Some(l) = tx
                    .work
                    .links
                    .iter_mut()
                    .find(|l| l.user_id == user && l.place_id == place)
                {
                    l.heard_at = Some(at);
                }
            }
        }
        Ok(())
    }
}
