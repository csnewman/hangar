//! The registry's repositories, blobs, manifests, tags and uploads, for
//! both backends.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::json::State;
use super::rows::{
    ImageRepository, ImageTag, Manifest, ManifestRef, RegistryUpload, RepositoryBlob,
    RepositoryCollaborator,
};
use super::{Backend, Result, Tx};

const REPO_COLUMNS: &str =
    "id, path, owner_id, team_id, visibility, description, created_at, updated_at";
const MANIFEST_COLUMNS: &str =
    "repository_id, digest, media_type, artifact_type, subject, annotations, content, created_at";

fn repo(r: &sqlx::postgres::PgRow) -> Result<ImageRepository> {
    Ok(ImageRepository {
        id: r.try_get("id")?,
        path: r.try_get("path")?,
        owner_id: r.try_get("owner_id")?,
        team_id: r.try_get("team_id")?,
        visibility: r.try_get("visibility")?,
        description: r.try_get("description")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

fn manifest(r: &sqlx::postgres::PgRow) -> Result<Manifest> {
    Ok(Manifest {
        repository_id: r.try_get("repository_id")?,
        digest: r.try_get("digest")?,
        media_type: r.try_get("media_type")?,
        artifact_type: r.try_get("artifact_type")?,
        subject: r.try_get("subject")?,
        annotations: r.try_get("annotations")?,
        content: r.try_get("content")?,
        created_at: r.try_get("created_at")?,
    })
}

/// Removes a repository from the JSON state, with what Postgres deletes
/// with it.
fn json_delete_repo(w: &mut State, id: Uuid) {
    w.image_repositories.retain(|r| r.id != id);
    w.image_repository_collaborators
        .retain(|c| c.repository_id != id);
    w.image_repository_team_collaborators
        .retain(|c| c.repository_id != id);
    w.image_repository_blobs.retain(|b| b.repository_id != id);
    w.image_manifests.retain(|m| m.repository_id != id);
    w.image_manifest_refs.retain(|r| r.repository_id != id);
    w.image_tags.retain(|t| t.repository_id != id);
    w.registry_uploads.retain(|u| u.repository_id != id);
}

/// Removes a manifest from the JSON state, with its references and tags.
fn json_delete_manifest(w: &mut State, repo: Uuid, digest: &str) {
    w.image_manifests
        .retain(|m| !(m.repository_id == repo && m.digest == digest));
    w.image_manifest_refs
        .retain(|r| !(r.repository_id == repo && r.digest == digest));
    w.image_tags
        .retain(|t| !(t.repository_id == repo && t.digest == digest));
}

impl Tx {
    pub async fn image_repositories(&mut self) -> Result<Vec<ImageRepository>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {REPO_COLUMNS} FROM image_repositories ORDER BY path"
            ))
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(repo)
            .collect(),
            Backend::Json(tx) => {
                let mut out = tx.work.image_repositories.clone();
                out.sort_by(|a, b| a.path.cmp(&b.path));
                Ok(out)
            }
        }
    }

    /// Makes a repository; false when one has its path already.
    pub async fn insert_image_repository(&mut self, r: &ImageRepository) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query(
                "INSERT INTO image_repositories (id, path, owner_id, team_id, visibility, description, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (path) DO NOTHING",
            )
            .bind(r.id)
            .bind(&r.path)
            .bind(r.owner_id)
            .bind(r.team_id)
            .bind(&r.visibility)
            .bind(&r.description)
            .bind(r.created_at)
            .bind(r.updated_at)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                == 1),
            Backend::Json(tx) => {
                if tx.work.image_repositories.iter().any(|o| o.path == r.path) {
                    return Ok(false);
                }
                tx.work.image_repositories.push(r.clone());
                Ok(true)
            }
        }
    }

    pub async fn lock_image_repository(&mut self, id: Uuid) -> Result<()> {
        if let Backend::Pg(tx) = &mut self.inner {
            sqlx::query("SELECT 1 FROM image_repositories WHERE id = $1 FOR UPDATE")
                .bind(id)
                .execute(&mut **tx)
                .await?;
        }
        Ok(())
    }

    /// Writes a repository's visibility, description and when it changed.
    pub async fn save_image_repository(&mut self, r: &ImageRepository) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "UPDATE image_repositories SET visibility = $2, description = $3, updated_at = $4 WHERE id = $1",
                )
                .bind(r.id)
                .bind(&r.visibility)
                .bind(&r.description)
                .bind(r.updated_at)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                for o in tx
                    .work
                    .image_repositories
                    .iter_mut()
                    .filter(|o| o.id == r.id)
                {
                    o.visibility = r.visibility.clone();
                    o.description = r.description.clone();
                    o.updated_at = r.updated_at;
                }
            }
        }
        Ok(())
    }

    pub async fn delete_image_repository(&mut self, id: Uuid) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM image_repositories WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => json_delete_repo(&mut tx.work, id),
        }
        Ok(())
    }

    /// Every repository's collaborators: people, then teams.
    pub async fn repository_collaborators(
        &mut self,
    ) -> Result<(Vec<RepositoryCollaborator>, Vec<RepositoryCollaborator>)> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let rows = |r: &sqlx::postgres::PgRow| -> Result<RepositoryCollaborator> {
                    Ok(RepositoryCollaborator {
                        repository_id: r.try_get(0)?,
                        who: r.try_get(1)?,
                    })
                };
                let users = sqlx::query(
                    "SELECT repository_id, user_id FROM image_repository_collaborators",
                )
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(rows)
                .collect::<Result<_>>()?;
                let teams = sqlx::query(
                    "SELECT repository_id, team_id FROM image_repository_team_collaborators",
                )
                .fetch_all(&mut **tx)
                .await?
                .iter()
                .map(rows)
                .collect::<Result<_>>()?;
                Ok((users, teams))
            }
            Backend::Json(tx) => Ok((
                tx.work.image_repository_collaborators.clone(),
                tx.work.image_repository_team_collaborators.clone(),
            )),
        }
    }

    pub async fn set_repository_collaborators(
        &mut self,
        id: Uuid,
        users: &[Uuid],
        teams: &[Uuid],
    ) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                for q in [
                    "DELETE FROM image_repository_collaborators WHERE repository_id = $1",
                    "DELETE FROM image_repository_team_collaborators WHERE repository_id = $1",
                ] {
                    sqlx::query(q).bind(id).execute(&mut **tx).await?;
                }
                sqlx::query(
                    "INSERT INTO image_repository_collaborators (repository_id, user_id) SELECT $1, unnest($2::uuid[])",
                )
                .bind(id)
                .bind(users)
                .execute(&mut **tx)
                .await?;
                sqlx::query(
                    "INSERT INTO image_repository_team_collaborators (repository_id, team_id) SELECT $1, unnest($2::uuid[])",
                )
                .bind(id)
                .bind(teams)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                let w = &mut tx.work;
                w.image_repository_collaborators
                    .retain(|c| c.repository_id != id);
                w.image_repository_team_collaborators
                    .retain(|c| c.repository_id != id);
                w.image_repository_collaborators
                    .extend(users.iter().map(|u| RepositoryCollaborator {
                        repository_id: id,
                        who: *u,
                    }));
                w.image_repository_team_collaborators
                    .extend(teams.iter().map(|t| RepositoryCollaborator {
                        repository_id: id,
                        who: *t,
                    }));
            }
        }
        Ok(())
    }

    /// The size of a blob a repository may use.
    pub async fn repository_blob(&mut self, repo: Uuid, digest: &str) -> Result<Option<i64>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT b.size FROM image_repository_blobs rb JOIN registry_blobs b ON b.digest = rb.digest
                 WHERE rb.repository_id = $1 AND rb.digest = $2",
            )
            .bind(repo)
            .bind(digest)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Json(tx) => {
                let w = &tx.work;
                Ok(w.image_repository_blobs
                    .iter()
                    .any(|b| b.repository_id == repo && b.digest == digest)
                    .then(|| w.registry_blobs.iter().find(|b| b.digest == digest).map(|b| b.size))
                    .flatten())
            }
        }
    }

    /// Holds a blob's digest until the transaction ends, so no collection
    /// deletes it from under an upload, or the other way round.
    pub async fn lock_blob(&mut self, digest: &str) -> Result<()> {
        if let Backend::Pg(tx) = &mut self.inner {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtext('registry.blob:' || $1))")
                .bind(digest)
                .execute(&mut **tx)
                .await?;
        }
        Ok(())
    }

    /// Records a blob in the blob store, and that a repository may use it.
    pub async fn add_blob(
        &mut self,
        repo: Uuid,
        digest: &str,
        size: i64,
        now: DateTime<Utc>,
    ) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("INSERT INTO registry_blobs (digest, size, created_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                    .bind(digest)
                    .bind(size)
                    .bind(now)
                    .execute(&mut **tx)
                    .await?;
                sqlx::query(
                    "INSERT INTO image_repository_blobs (repository_id, digest, created_at) VALUES ($1, $2, $3)
                     ON CONFLICT DO NOTHING",
                )
                .bind(repo)
                .bind(digest)
                .bind(now)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                let w = &mut tx.work;
                if !w.registry_blobs.iter().any(|b| b.digest == digest) {
                    w.registry_blobs.push(super::rows::RegistryBlob {
                        digest: digest.to_string(),
                        size,
                        created_at: now,
                    });
                }
                if !w
                    .image_repository_blobs
                    .iter()
                    .any(|b| b.repository_id == repo && b.digest == digest)
                {
                    w.image_repository_blobs.push(RepositoryBlob {
                        repository_id: repo,
                        digest: digest.to_string(),
                        created_at: now,
                    });
                }
            }
        }
        Ok(())
    }

    /// Lets a repository use a blob another has, if the store has it.
    pub async fn mount_blob(&mut self, repo: Uuid, digest: &str, now: DateTime<Utc>) -> Result<()> {
        let size = match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query_scalar::<_, i64>("SELECT size FROM registry_blobs WHERE digest = $1")
                    .bind(digest)
                    .fetch_optional(&mut **tx)
                    .await?
            }
            Backend::Json(tx) => tx
                .work
                .registry_blobs
                .iter()
                .find(|b| b.digest == digest)
                .map(|b| b.size),
        };
        if let Some(size) = size {
            self.add_blob(repo, digest, size, now).await?;
        }
        Ok(())
    }

    /// Whether a manifest in a repository names a blob or manifest.
    pub async fn blob_used(&mut self, repo: Uuid, digest: &str) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM image_manifest_refs WHERE repository_id = $1 AND ref = $2)",
            )
            .bind(repo)
            .bind(digest)
            .fetch_one(&mut **tx)
            .await?),
            Backend::Json(tx) => Ok(tx
                .work
                .image_manifest_refs
                .iter()
                .any(|r| r.repository_id == repo && r.r#ref == digest)),
        }
    }

    /// Stops a repository using a blob; false if it did not.
    pub async fn remove_repository_blob(&mut self, repo: Uuid, digest: &str) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query(
                "DELETE FROM image_repository_blobs WHERE repository_id = $1 AND digest = $2",
            )
            .bind(repo)
            .bind(digest)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                > 0),
            Backend::Json(tx) => {
                let w = &mut tx.work.image_repository_blobs;
                let before = w.len();
                w.retain(|b| !(b.repository_id == repo && b.digest == digest));
                Ok(w.len() != before)
            }
        }
    }

    /// The first of digests the repository has no blob of, and the first
    /// it has no manifest of.
    pub async fn missing(
        &mut self,
        repo: Uuid,
        blobs: &[String],
        manifests: &[String],
    ) -> Result<(Option<String>, Option<String>)> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                let blob: Option<String> = sqlx::query_scalar(
                    "SELECT d FROM unnest($2::text[]) d WHERE NOT EXISTS
                        (SELECT 1 FROM image_repository_blobs WHERE repository_id = $1 AND digest = d) LIMIT 1",
                )
                .bind(repo)
                .bind(blobs)
                .fetch_optional(&mut **tx)
                .await?;
                let manifest: Option<String> = sqlx::query_scalar(
                    "SELECT d FROM unnest($2::text[]) d WHERE NOT EXISTS
                        (SELECT 1 FROM image_manifests WHERE repository_id = $1 AND digest = d) LIMIT 1",
                )
                .bind(repo)
                .bind(manifests)
                .fetch_optional(&mut **tx)
                .await?;
                Ok((blob, manifest))
            }
            Backend::Json(tx) => {
                let w = &tx.work;
                let blob = blobs
                    .iter()
                    .find(|d| {
                        !w.image_repository_blobs
                            .iter()
                            .any(|b| b.repository_id == repo && &b.digest == *d)
                    })
                    .cloned();
                let manifest = manifests
                    .iter()
                    .find(|d| {
                        !w.image_manifests
                            .iter()
                            .any(|m| m.repository_id == repo && &m.digest == *d)
                    })
                    .cloned();
                Ok((blob, manifest))
            }
        }
    }

    /// A repository's manifest by digest, or by tag.
    pub async fn manifest(
        &mut self,
        repo: Uuid,
        digest: Option<&str>,
        tag: Option<&str>,
    ) -> Result<Option<Manifest>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {} FROM image_manifests m WHERE m.repository_id = $1 AND (m.digest = $2 OR m.digest =
                    (SELECT t.digest FROM image_tags t WHERE t.repository_id = $1 AND t.tag = $3))",
                MANIFEST_COLUMNS
                    .split(", ")
                    .map(|c| format!("m.{c}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .bind(repo)
            .bind(digest)
            .bind(tag)
            .fetch_optional(&mut **tx)
            .await?
            .map(|r| manifest(&r))
            .transpose(),
            Backend::Json(tx) => {
                let w = &tx.work;
                let want = match (digest, tag) {
                    (Some(d), _) => Some(d.to_string()),
                    (None, Some(t)) => w
                        .image_tags
                        .iter()
                        .find(|x| x.repository_id == repo && x.tag == t)
                        .map(|x| x.digest.clone()),
                    _ => None,
                };
                Ok(want.and_then(|d| {
                    w.image_manifests
                        .iter()
                        .find(|m| m.repository_id == repo && m.digest == d)
                        .cloned()
                }))
            }
        }
    }

    /// Keeps a manifest and what it names; false if the repository had it.
    pub async fn insert_manifest(&mut self, m: &Manifest, refs: &[String]) -> Result<bool> {
        let done = match &mut self.inner {
            Backend::Pg(tx) => {
                let n = sqlx::query(&format!(
                    "INSERT INTO image_manifests ({MANIFEST_COLUMNS}) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                     ON CONFLICT DO NOTHING"
                ))
                .bind(m.repository_id)
                .bind(&m.digest)
                .bind(&m.media_type)
                .bind(&m.artifact_type)
                .bind(&m.subject)
                .bind(&m.annotations)
                .bind(&m.content)
                .bind(m.created_at)
                .execute(&mut **tx)
                .await?
                .rows_affected();
                if n > 0 {
                    sqlx::query(
                        "INSERT INTO image_manifest_refs (repository_id, digest, ref)
                         SELECT $1, $2, unnest($3::text[]) ON CONFLICT DO NOTHING",
                    )
                    .bind(m.repository_id)
                    .bind(&m.digest)
                    .bind(refs)
                    .execute(&mut **tx)
                    .await?;
                }
                n > 0
            }
            Backend::Json(tx) => {
                let w = &mut tx.work;
                if w.image_manifests
                    .iter()
                    .any(|o| o.repository_id == m.repository_id && o.digest == m.digest)
                {
                    false
                } else {
                    w.image_manifests.push(m.clone());
                    for r in refs {
                        let x = ManifestRef {
                            repository_id: m.repository_id,
                            digest: m.digest.clone(),
                            r#ref: r.clone(),
                        };
                        if !w.image_manifest_refs.contains(&x) {
                            w.image_manifest_refs.push(x);
                        }
                    }
                    true
                }
            }
        };
        Ok(done)
    }

    pub async fn set_tag(
        &mut self,
        repo: Uuid,
        tag: &str,
        digest: &str,
        now: DateTime<Utc>,
    ) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query(
                    "INSERT INTO image_tags (repository_id, tag, digest, updated_at) VALUES ($1, $2, $3, $4)
                     ON CONFLICT (repository_id, tag) DO UPDATE SET digest = excluded.digest, updated_at = excluded.updated_at",
                )
                .bind(repo)
                .bind(tag)
                .bind(digest)
                .bind(now)
                .execute(&mut **tx)
                .await?;
            }
            Backend::Json(tx) => {
                let w = &mut tx.work.image_tags;
                w.retain(|t| !(t.repository_id == repo && t.tag == tag));
                w.push(ImageTag {
                    repository_id: repo,
                    tag: tag.to_string(),
                    digest: digest.to_string(),
                    updated_at: now,
                });
            }
        }
        Ok(())
    }

    /// Deletes a manifest by digest, with its tags; false if there was none.
    pub async fn delete_manifest(&mut self, repo: Uuid, digest: &str) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query(
                "DELETE FROM image_manifests WHERE repository_id = $1 AND digest = $2",
            )
            .bind(repo)
            .bind(digest)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                > 0),
            Backend::Json(tx) => {
                let had = tx
                    .work
                    .image_manifests
                    .iter()
                    .any(|m| m.repository_id == repo && m.digest == digest);
                json_delete_manifest(&mut tx.work, repo, digest);
                Ok(had)
            }
        }
    }

    pub async fn delete_tag(&mut self, repo: Uuid, tag: &str) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query(
                "DELETE FROM image_tags WHERE repository_id = $1 AND tag = $2",
            )
            .bind(repo)
            .bind(tag)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                > 0),
            Backend::Json(tx) => {
                let w = &mut tx.work.image_tags;
                let before = w.len();
                w.retain(|t| !(t.repository_id == repo && t.tag == tag));
                Ok(w.len() != before)
            }
        }
    }

    /// A repository's tags, or every repository's with none.
    pub async fn image_tags(&mut self, repo: Option<Uuid>) -> Result<Vec<ImageTag>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(
                "SELECT repository_id, tag, digest, updated_at FROM image_tags WHERE $1::uuid IS NULL OR repository_id = $1",
            )
            .bind(repo)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|r| {
                Ok(ImageTag {
                    repository_id: r.try_get(0)?,
                    tag: r.try_get(1)?,
                    digest: r.try_get(2)?,
                    updated_at: r.try_get(3)?,
                })
            })
            .collect(),
            Backend::Json(tx) => Ok(tx
                .work
                .image_tags
                .iter()
                .filter(|t| repo.is_none_or(|r| t.repository_id == r))
                .cloned()
                .collect()),
        }
    }

    /// The manifests of a repository whose subject is a digest.
    pub async fn referrers(&mut self, repo: Uuid, subject: &str) -> Result<Vec<Manifest>> {
        match &mut self.inner {
            Backend::Pg(tx) => sqlx::query(&format!(
                "SELECT {MANIFEST_COLUMNS} FROM image_manifests WHERE repository_id = $1 AND subject = $2 ORDER BY digest"
            ))
            .bind(repo)
            .bind(subject)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(manifest)
            .collect(),
            Backend::Json(tx) => {
                let mut out: Vec<Manifest> = tx
                    .work
                    .image_manifests
                    .iter()
                    .filter(|m| m.repository_id == repo && m.subject.as_deref() == Some(subject))
                    .cloned()
                    .collect();
                out.sort_by(|a, b| a.digest.cmp(&b.digest));
                Ok(out)
            }
        }
    }

    pub async fn touch_image_repository(&mut self, id: Uuid, now: DateTime<Utc>) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("UPDATE image_repositories SET updated_at = $2 WHERE id = $1")
                    .bind(id)
                    .bind(now)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => {
                for r in tx.work.image_repositories.iter_mut().filter(|r| r.id == id) {
                    r.updated_at = now;
                }
            }
        }
        Ok(())
    }

    pub async fn insert_upload(&mut self, u: &RegistryUpload) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("INSERT INTO registry_uploads (id, repository_id, created_at) VALUES ($1, $2, $3)")
                    .bind(u.id)
                    .bind(u.repository_id)
                    .bind(u.created_at)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => tx.work.registry_uploads.push(u.clone()),
        }
        Ok(())
    }

    pub async fn upload_exists(&mut self, id: Uuid, repo: Uuid) -> Result<bool> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM registry_uploads WHERE id = $1 AND repository_id = $2)",
            )
            .bind(id)
            .bind(repo)
            .fetch_one(&mut **tx)
            .await?),
            Backend::Json(tx) => Ok(tx.work.registry_uploads.iter().any(|u| u.id == id && u.repository_id == repo)),
        }
    }

    pub async fn delete_upload(&mut self, id: Uuid) -> Result<()> {
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM registry_uploads WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Json(tx) => tx.work.registry_uploads.retain(|u| u.id != id),
        }
        Ok(())
    }

    /// Deletes the uploads begun before a time, giving their IDs.
    pub async fn prune_uploads(&mut self, before: DateTime<Utc>) -> Result<Vec<Uuid>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM registry_uploads WHERE created_at < $1 RETURNING id",
            )
            .bind(before)
            .fetch_all(&mut **tx)
            .await?),
            Backend::Json(tx) => {
                let w = &mut tx.work.registry_uploads;
                let gone: Vec<Uuid> = w
                    .iter()
                    .filter(|u| u.created_at < before)
                    .map(|u| u.id)
                    .collect();
                w.retain(|u| u.created_at >= before);
                Ok(gone)
            }
        }
    }

    /// Deletes the manifests nothing keeps -- no tag, no newer than
    /// cutoff, none an environment is pinned to, nothing kept naming
    /// them -- and the blob uses they leave unused, giving how many
    /// manifests went.
    pub async fn collect_manifests(&mut self, cutoff: DateTime<Utc>) -> Result<u64> {
        const KEEP: &str = "
            WITH RECURSIVE edges (repository_id, parent, child) AS (
                SELECT repository_id, digest, ref FROM image_manifest_refs
                UNION ALL
                SELECT repository_id, subject, digest FROM image_manifests WHERE subject IS NOT NULL
            ), keep (repository_id, digest) AS (
                SELECT repository_id, digest FROM image_tags
                UNION
                SELECT repository_id, digest FROM image_manifests
                WHERE created_at > $1 OR digest IN (SELECT image_digest FROM environments WHERE image_digest <> '')
                UNION
                SELECT e.repository_id, e.child FROM edges e
                JOIN keep k ON k.repository_id = e.repository_id AND k.digest = e.parent
            )";
        match &mut self.inner {
            Backend::Pg(tx) => {
                let n = sqlx::query(&format!(
                    "{KEEP} DELETE FROM image_manifests m
                     WHERE NOT EXISTS (SELECT 1 FROM keep k WHERE k.repository_id = m.repository_id AND k.digest = m.digest)"
                ))
                .bind(cutoff)
                .execute(&mut **tx)
                .await?
                .rows_affected();
                sqlx::query(&format!(
                    "{KEEP} DELETE FROM image_repository_blobs rb WHERE rb.created_at <= $1
                     AND NOT EXISTS (SELECT 1 FROM keep k WHERE k.repository_id = rb.repository_id AND k.digest = rb.digest)"
                ))
                .bind(cutoff)
                .execute(&mut **tx)
                .await?;
                Ok(n)
            }
            Backend::Json(tx) => {
                let w = &mut tx.work;
                let pinned: HashSet<&str> = w
                    .environments
                    .iter()
                    .filter(|e| !e.image_digest.is_empty())
                    .map(|e| e.image_digest.as_str())
                    .collect();
                let mut edges: HashMap<(Uuid, String), Vec<String>> = HashMap::new();
                for r in &w.image_manifest_refs {
                    edges
                        .entry((r.repository_id, r.digest.clone()))
                        .or_default()
                        .push(r.r#ref.clone());
                }
                for m in &w.image_manifests {
                    if let Some(s) = &m.subject {
                        edges
                            .entry((m.repository_id, s.clone()))
                            .or_default()
                            .push(m.digest.clone());
                    }
                }
                let mut keep: HashSet<(Uuid, String)> = HashSet::new();
                let mut todo: Vec<(Uuid, String)> = w
                    .image_tags
                    .iter()
                    .map(|t| (t.repository_id, t.digest.clone()))
                    .chain(
                        w.image_manifests
                            .iter()
                            .filter(|m| m.created_at > cutoff || pinned.contains(m.digest.as_str()))
                            .map(|m| (m.repository_id, m.digest.clone())),
                    )
                    .collect();
                while let Some(k) = todo.pop() {
                    if !keep.insert(k.clone()) {
                        continue;
                    }
                    for child in edges.get(&k).into_iter().flatten() {
                        todo.push((k.0, child.clone()));
                    }
                }
                let gone: Vec<(Uuid, String)> = w
                    .image_manifests
                    .iter()
                    .map(|m| (m.repository_id, m.digest.clone()))
                    .filter(|k| !keep.contains(k))
                    .collect();
                for (repo, digest) in &gone {
                    json_delete_manifest(w, *repo, digest);
                }
                w.image_repository_blobs.retain(|b| {
                    b.created_at > cutoff || keep.contains(&(b.repository_id, b.digest.clone()))
                });
                Ok(gone.len() as u64)
            }
        }
    }

    /// The blobs no repository uses, made before cutoff.
    pub async fn orphan_blobs(&mut self, cutoff: DateTime<Utc>) -> Result<Vec<String>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "SELECT b.digest FROM registry_blobs b WHERE b.created_at <= $1
                 AND NOT EXISTS (SELECT 1 FROM image_repository_blobs rb WHERE rb.digest = b.digest)",
            )
            .bind(cutoff)
            .fetch_all(&mut **tx)
            .await?),
            Backend::Json(tx) => {
                let w = &tx.work;
                Ok(w.registry_blobs
                    .iter()
                    .filter(|b| b.created_at <= cutoff && !w.image_repository_blobs.iter().any(|rb| rb.digest == b.digest))
                    .map(|b| b.digest.clone())
                    .collect())
            }
        }
    }

    /// Forgets a blob no repository uses, giving its size; None if one
    /// uses it again.
    pub async fn forget_blob(&mut self, digest: &str) -> Result<Option<i64>> {
        match &mut self.inner {
            Backend::Pg(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM registry_blobs b WHERE b.digest = $1
                 AND NOT EXISTS (SELECT 1 FROM image_repository_blobs rb WHERE rb.digest = b.digest) RETURNING b.size",
            )
            .bind(digest)
            .fetch_optional(&mut **tx)
            .await?),
            Backend::Json(tx) => {
                let w = &mut tx.work;
                if w.image_repository_blobs.iter().any(|rb| rb.digest == digest) {
                    return Ok(None);
                }
                let size = w.registry_blobs.iter().find(|b| b.digest == digest).map(|b| b.size);
                w.registry_blobs.retain(|b| b.digest != digest);
                Ok(size)
            }
        }
    }
}
