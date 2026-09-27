package registry

import (
	"context"
	"errors"
	"os"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/opencontainers/go-digest"

	"github.com/csnewman/hangar/internal/db"
)

// Collected is what a collection deleted.
type Collected struct {
	Manifests int64
	Blobs     int
	Bytes     int64
}

// keep is every manifest and blob a repository keeps, as (repository_id,
// digest): its tagged manifests, any manifest an environment is pinned to,
// any made since $1, and everything they lead to -- an index's manifests,
// an image's config and layers, and the manifests that name one of them as
// their subject.
const keep = `
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
	)`

// Collect deletes what nothing needs, leaving anything made within grace, so
// a push in progress -- blobs first, then manifests, the tag last -- is not
// caught halfway. Manifests nothing keeps go first, then each repository's
// links to blobs nothing keeps, then the blobs no repository links to, file
// and all.
func (reg *Registry) Collect(ctx context.Context, grace time.Duration) (Collected, error) {
	var out Collected
	cutoff := time.Now().Add(-grace)
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		tag, err := tx.Exec(ctx, keep+`
			DELETE FROM image_manifests m
			WHERE NOT EXISTS (SELECT 1 FROM keep k WHERE k.repository_id = m.repository_id AND k.digest = m.digest)`,
			cutoff)
		if err != nil {
			return err
		}
		out.Manifests = tag.RowsAffected()
		_, err = tx.Exec(ctx, keep+`
			DELETE FROM image_repository_blobs rb
			WHERE rb.created_at <= $1
				AND NOT EXISTS (SELECT 1 FROM keep k WHERE k.repository_id = rb.repository_id AND k.digest = rb.digest)`,
			cutoff)
		return err
	})
	if err != nil {
		return out, err
	}

	var orphans []string
	err = reg.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT b.digest FROM registry_blobs b
			WHERE b.created_at <= $1 AND NOT EXISTS (SELECT 1 FROM image_repository_blobs rb WHERE rb.digest = b.digest)`,
			cutoff)
		if err != nil {
			return err
		}
		orphans, err = pgx.CollectRows(rows, pgx.RowTo[string])
		return err
	})
	if err != nil {
		return out, err
	}
	var errs []error
	for _, s := range orphans {
		d, err := digest.Parse(s)
		if err != nil {
			continue
		}
		var size int64
		deleted := false
		err = reg.db.Transact(ctx, func(tx db.Tx) error {
			deleted = false
			// The same lock an upload takes to put the blob in place.
			if err := lockBlob(ctx, tx, d); err != nil {
				return err
			}
			// Linked again since it was found, or deleted by another
			// replica: left as it is.
			err := tx.QueryRow(ctx, `DELETE FROM registry_blobs b WHERE b.digest = $1
				AND NOT EXISTS (SELECT 1 FROM image_repository_blobs rb WHERE rb.digest = b.digest)
				RETURNING b.size`, s).Scan(&size)
			if errors.Is(err, pgx.ErrNoRows) {
				return nil
			}
			if err != nil {
				return err
			}
			if err := os.Remove(reg.blobPath(d)); err != nil && !errors.Is(err, os.ErrNotExist) {
				return err
			}
			deleted = true
			return nil
		})
		if err != nil {
			errs = append(errs, err)
			continue
		}
		if deleted {
			out.Blobs++
			out.Bytes += size
		}
	}
	return out, errors.Join(errs...)
}
