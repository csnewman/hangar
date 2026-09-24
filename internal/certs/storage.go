// Package certs obtains and renews hangar-server's TLS certificates from an
// ACME CA: one for Hangar's host, and a wildcard one for every name under
// it, which is what the editors' e-<id> hosts need.
//
// A wildcard certificate is issued only against the DNS-01 challenge, a TXT
// record the CA looks up in the zone. Hangar is its zone's authoritative
// server (internal/zone), so it answers that challenge itself: nothing is
// configured at a DNS provider beyond delegating the zone once.
//
// Certificates, keys and the ACME account live in Postgres, and so do the
// locks certmagic takes while obtaining or renewing, so every replica
// serves the same certificates and only one renews each.
package certs

import (
	"context"
	"errors"
	"io/fs"
	"strings"
	"time"

	"github.com/caddyserver/certmagic"
	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/db"
)

// lockLease is how long a lock is held without being renewed. certmagic
// renews it while it works; a replica that dies holding one leaves it to
// expire.
const lockLease = 2 * time.Minute

// Storage is certmagic's storage, in Postgres.
type Storage struct {
	db *db.DB
}

var (
	_ certmagic.Storage          = (*Storage)(nil)
	_ certmagic.LockLeaseRenewer = (*Storage)(nil)
)

func NewStorage(d *db.DB) *Storage { return &Storage{db: d} }

func (s *Storage) Store(ctx context.Context, key string, value []byte) error {
	return s.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `INSERT INTO tls_storage (key, value, modified) VALUES ($1, $2, now())
			ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, modified = now()`, key, value)
		return err
	})
}

func (s *Storage) Load(ctx context.Context, key string) ([]byte, error) {
	var v []byte
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `SELECT value FROM tls_storage WHERE key = $1`, key).Scan(&v)
	})
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, fs.ErrNotExist
	}
	return v, err
}

// Delete deletes a key, or every key under it: certmagic's keys are paths.
func (s *Storage) Delete(ctx context.Context, key string) error {
	var n int64
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		tag, err := tx.Exec(ctx, `DELETE FROM tls_storage WHERE key = $1 OR key LIKE $2`, key, likePrefix(key))
		n = tag.RowsAffected()
		return err
	})
	if err == nil && n == 0 {
		return fs.ErrNotExist
	}
	return err
}

func (s *Storage) Exists(ctx context.Context, key string) bool {
	_, err := s.Stat(ctx, key)
	return err == nil
}

// List lists the keys under a path: with recursive, all of them; without,
// the path's immediate children, a child with keys under it standing for
// them as a directory would.
func (s *Storage) List(ctx context.Context, path string, recursive bool) ([]string, error) {
	var keys []string
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		keys = nil
		rows, err := tx.Query(ctx, `SELECT key FROM tls_storage WHERE key LIKE $1 ORDER BY key`, likePrefix(path))
		if err != nil {
			return err
		}
		defer rows.Close()
		seen := map[string]bool{}
		prefix := strings.TrimSuffix(path, "/") + "/"
		for rows.Next() {
			var k string
			if err := rows.Scan(&k); err != nil {
				return err
			}
			if !recursive {
				rest := strings.TrimPrefix(k, prefix)
				if i := strings.Index(rest, "/"); i >= 0 {
					k = prefix + rest[:i]
				}
			}
			if !seen[k] {
				seen[k] = true
				keys = append(keys, k)
			}
		}
		return rows.Err()
	})
	if err == nil && len(keys) == 0 {
		return nil, fs.ErrNotExist
	}
	return keys, err
}

func (s *Storage) Stat(ctx context.Context, key string) (certmagic.KeyInfo, error) {
	info := certmagic.KeyInfo{Key: key}
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		var size int64
		err := tx.QueryRow(ctx, `SELECT modified, length(value) FROM tls_storage WHERE key = $1`, key).
			Scan(&info.Modified, &size)
		if err == nil {
			info.Size = size
			info.IsTerminal = true
			return nil
		}
		if !errors.Is(err, pgx.ErrNoRows) {
			return err
		}
		// A key with keys under it is a directory.
		return tx.QueryRow(ctx, `SELECT max(modified) FROM tls_storage WHERE key LIKE $1 HAVING count(*) > 0`,
			likePrefix(key)).Scan(&info.Modified)
	})
	if errors.Is(err, pgx.ErrNoRows) {
		return info, fs.ErrNotExist
	}
	return info, err
}

// Lock takes a lock, waiting for it if another replica holds it, until ctx
// ends.
func (s *Storage) Lock(ctx context.Context, name string) error {
	for {
		var took bool
		err := s.db.Transact(ctx, func(tx db.Tx) error {
			tag, err := tx.Exec(ctx, `INSERT INTO tls_locks (name, expires_at) VALUES ($1, $2)
				ON CONFLICT (name) DO UPDATE SET expires_at = EXCLUDED.expires_at
				WHERE tls_locks.expires_at < now()`, name, time.Now().Add(lockLease))
			took = tag.RowsAffected() == 1
			return err
		})
		if err != nil {
			return err
		}
		if took {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(time.Second):
		}
	}
}

func (s *Storage) Unlock(ctx context.Context, name string) error {
	return s.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `DELETE FROM tls_locks WHERE name = $1`, name)
		return err
	})
}

func (s *Storage) RenewLockLease(ctx context.Context, name string, lease time.Duration) error {
	return s.db.Transact(ctx, func(tx db.Tx) error {
		tag, err := tx.Exec(ctx, `UPDATE tls_locks SET expires_at = $2 WHERE name = $1`, name, time.Now().Add(lease))
		if err == nil && tag.RowsAffected() == 0 {
			return errors.New("the lock has expired or been released")
		}
		return err
	})
}

// likePrefix matches every key under path.
func likePrefix(path string) string {
	esc := strings.NewReplacer(`\`, `\\`, `%`, `\%`, `_`, `\_`).Replace(strings.TrimSuffix(path, "/"))
	return esc + "/%"
}
