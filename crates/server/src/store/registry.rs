//! The registry's repositories, blobs, manifests, tags and uploads, for
//! both backends.

use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

use super::rows::{ImageRepository, ImageTag, Manifest, RegistryUpload, RepositoryCollaborator};
use super::{Backend, Result, Tx};

const REPO_COLUMNS: &str =
    "id, path, owner_id, team_id, visibility, description, created_at, updated_at";
const MANIFEST_COLUMNS: &str =
    "repository_id, digest, media_type, artifact_type, subject, annotations, content, created_at";

fn repo(r: &impl super::Get) -> Result<ImageRepository> {
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

fn manifest(r: &impl super::Get) -> Result<Manifest> {
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
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {REPO_COLUMNS} FROM image_repositories ORDER BY path"
            ))
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(repo)
            .collect(),
        }
    }

    /// Makes a repository; false when one has its path already.
    pub async fn insert_image_repository(&mut self, r: &ImageRepository) -> Result<bool> {
        self.signal(super::signals::Signal::Records);
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
            Backend::Lite(tx) => Ok(sqlx::query(
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
                == 1)
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
        self.signal(super::signals::Signal::Records);
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
            Backend::Lite(tx) => {
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
        }
        Ok(())
    }

    pub async fn delete_image_repository(&mut self, id: Uuid) -> Result<()> {
        self.signal(super::signals::Signal::Records);
        match &mut self.inner {
            Backend::Pg(tx) => {
                sqlx::query("DELETE FROM image_repositories WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
            Backend::Lite(tx) => {
                sqlx::query("DELETE FROM image_repositories WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
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
            Backend::Lite(tx) => {
                let rows = |r: &sqlx::sqlite::SqliteRow| -> Result<RepositoryCollaborator> {
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
            Backend::Lite(tx) => {
                for q in [
                    "DELETE FROM image_repository_collaborators WHERE repository_id = $1",
                    "DELETE FROM image_repository_team_collaborators WHERE repository_id = $1",
                ] {
                    sqlx::query(q).bind(id).execute(&mut **tx).await?;
                }
                sqlx::query(
                    "INSERT INTO image_repository_collaborators (repository_id, user_id)
                     SELECT $1, unhex(replace(value, '-', '')) FROM json_each($2)",
                )
                .bind(id)
                .bind(sqlx::types::Json(users))
                .execute(&mut **tx)
                .await?;
                sqlx::query(
                    "INSERT INTO image_repository_team_collaborators (repository_id, team_id)
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
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "SELECT b.size FROM image_repository_blobs rb JOIN registry_blobs b ON b.digest = rb.digest
                 WHERE rb.repository_id = $1 AND rb.digest = $2",
            )
            .bind(repo)
            .bind(digest)
            .fetch_optional(&mut **tx)
            .await?)
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
            Backend::Lite(tx) => {
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
            Backend::Lite(tx) => {
                sqlx::query_scalar::<_, i64>("SELECT size FROM registry_blobs WHERE digest = $1")
                    .bind(digest)
                    .fetch_optional(&mut **tx)
                    .await?
            }
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
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM image_manifest_refs WHERE repository_id = $1 AND ref = $2)",
            )
            .bind(repo)
            .bind(digest)
            .fetch_one(&mut **tx)
            .await?),
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
            Backend::Lite(tx) => Ok(sqlx::query(
                "DELETE FROM image_repository_blobs WHERE repository_id = $1 AND digest = $2",
            )
            .bind(repo)
            .bind(digest)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                > 0),
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
            Backend::Lite(tx) => {
                let blob: Option<String> = sqlx::query_scalar(
                    "SELECT d.value FROM json_each($2) d WHERE NOT EXISTS
                        (SELECT 1 FROM image_repository_blobs WHERE repository_id = $1 AND digest = d.value)
                     LIMIT 1",
                )
                .bind(repo)
                .bind(sqlx::types::Json(blobs))
                .fetch_optional(&mut **tx)
                .await?;
                let manifest: Option<String> = sqlx::query_scalar(
                    "SELECT d.value FROM json_each($2) d WHERE NOT EXISTS
                        (SELECT 1 FROM image_manifests WHERE repository_id = $1 AND digest = d.value)
                     LIMIT 1",
                )
                .bind(repo)
                .bind(sqlx::types::Json(manifests))
                .fetch_optional(&mut **tx)
                .await?;
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
            Backend::Lite(tx) => sqlx::query(&format!(
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
            .transpose()
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
            Backend::Lite(tx) => {
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
                         SELECT $1, $2, value FROM json_each($3) WHERE true ON CONFLICT DO NOTHING",
                    )
                    .bind(m.repository_id)
                    .bind(&m.digest)
                    .bind(sqlx::types::Json(refs))
                    .execute(&mut **tx)
                    .await?;
                }
                n > 0
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
        self.signal(super::signals::Signal::Records);
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
            Backend::Lite(tx) => {
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
            Backend::Lite(tx) => Ok(sqlx::query(
                "DELETE FROM image_manifests WHERE repository_id = $1 AND digest = $2",
            )
            .bind(repo)
            .bind(digest)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                > 0),
        }
    }

    pub async fn delete_tag(&mut self, repo: Uuid, tag: &str) -> Result<bool> {
        self.signal(super::signals::Signal::Records);
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
            Backend::Lite(tx) => Ok(sqlx::query(
                "DELETE FROM image_tags WHERE repository_id = $1 AND tag = $2",
            )
            .bind(repo)
            .bind(tag)
            .execute(&mut **tx)
            .await?
            .rows_affected()
                > 0),
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
            Backend::Lite(tx) => sqlx::query(
                "SELECT repository_id, tag, digest, updated_at FROM image_tags WHERE $1 IS NULL OR repository_id = $1",
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
            Backend::Lite(tx) => sqlx::query(&format!(
                "SELECT {MANIFEST_COLUMNS} FROM image_manifests WHERE repository_id = $1 AND subject = $2 ORDER BY digest"
            ))
            .bind(repo)
            .bind(subject)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(manifest)
            .collect()
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
            Backend::Lite(tx) => {
                sqlx::query("UPDATE image_repositories SET updated_at = $2 WHERE id = $1")
                    .bind(id)
                    .bind(now)
                    .execute(&mut **tx)
                    .await?;
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
            Backend::Lite(tx) => {
                sqlx::query("INSERT INTO registry_uploads (id, repository_id, created_at) VALUES ($1, $2, $3)")
                    .bind(u.id)
                    .bind(u.repository_id)
                    .bind(u.created_at)
                    .execute(&mut **tx)
                    .await?;
            }
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
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM registry_uploads WHERE id = $1 AND repository_id = $2)",
            )
            .bind(id)
            .bind(repo)
            .fetch_one(&mut **tx)
            .await?),
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
            Backend::Lite(tx) => {
                sqlx::query("DELETE FROM registry_uploads WHERE id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await?;
            }
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
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM registry_uploads WHERE created_at < $1 RETURNING id",
            )
            .bind(before)
            .fetch_all(&mut **tx)
            .await?),
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
            Backend::Lite(tx) => {
                let n = sqlx::query(&format!(
                    "{KEEP} DELETE FROM image_manifests AS m
                     WHERE NOT EXISTS (SELECT 1 FROM keep k WHERE k.repository_id = m.repository_id AND k.digest = m.digest)"
                ))
                .bind(cutoff)
                .execute(&mut **tx)
                .await?
                .rows_affected();
                sqlx::query(&format!(
                    "{KEEP} DELETE FROM image_repository_blobs AS rb WHERE rb.created_at <= $1
                     AND NOT EXISTS (SELECT 1 FROM keep k WHERE k.repository_id = rb.repository_id AND k.digest = rb.digest)"
                ))
                .bind(cutoff)
                .execute(&mut **tx)
                .await?;
                Ok(n)
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
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "SELECT b.digest FROM registry_blobs b WHERE b.created_at <= $1
                 AND NOT EXISTS (SELECT 1 FROM image_repository_blobs rb WHERE rb.digest = b.digest)",
            )
            .bind(cutoff)
            .fetch_all(&mut **tx)
            .await?)
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
            Backend::Lite(tx) => Ok(sqlx::query_scalar(
                "DELETE FROM registry_blobs AS b WHERE b.digest = $1
                 AND NOT EXISTS (SELECT 1 FROM image_repository_blobs rb WHERE rb.digest = b.digest)
                 RETURNING size",
            )
            .bind(digest)
            .fetch_optional(&mut **tx)
            .await?)
        }
    }
}
