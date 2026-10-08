package packs

import (
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path"
	"strings"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/blob"
	"github.com/csnewman/hangar/internal/db"
)

// Migrate brings the files root up to what the database says, once: a
// copy laid out relative to the home directory is moved under ~/ (as
// copy_relayout lists), the files the blob store held are copied in (when
// there is one), and paths are moved from one copy to another (as
// copy_moves lists). It reports how many files and directories it moved or
// copied.
func (s *Store) Migrate(ctx context.Context, blobs blob.Versioned) (int, error) {
	n, err := s.relayout(ctx)
	if err != nil {
		return n, err
	}
	if blobs != nil {
		c, err := s.migrateFrom(ctx, blobs)
		n += c
		if err != nil {
			return n, err
		}
	}
	m, err := s.moves(ctx)
	return n + m, err
}

// migrateFrom copies the files the database names in the blob store --
// where they were kept before they were kept on the files root -- that the
// root does not have yet, and forgets each once the root has it. A
// profile's, kept relative to the home directory, go under ~/.
func (s *Store) migrateFrom(ctx context.Context, blobs blob.Versioned) (int, error) {
	type row struct {
		set, key, version string
		mode              uint32
	}
	var rows []row
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		var exists bool
		if err := tx.QueryRow(ctx, `SELECT to_regclass('set_files') IS NOT NULL`).Scan(&exists); err != nil || !exists {
			return err
		}
		r, err := tx.Query(ctx, `SELECT set_id::text, path, version_id, mode FROM set_files
			WHERE NOT deleted AND version_id IS NOT NULL`)
		if err != nil {
			return err
		}
		var err2 error
		rows, err2 = pgx.CollectRows(r, func(r pgx.CollectableRow) (row, error) {
			var x row
			var mode int32
			err := r.Scan(&x.set, &x.key, &x.version, &mode)
			x.mode = uint32(mode)
			return x, err
		})
		return err2
	})
	if err != nil {
		return 0, err
	}
	copied := 0
	for _, x := range rows {
		forget := func() error {
			return s.db.Transact(ctx, func(tx db.Tx) error {
				_, err := tx.Exec(ctx, `DELETE FROM set_files WHERE set_id = $1 AND path = $2`, x.set, x.key)
				return err
			})
		}
		key := x.key
		if !strings.HasPrefix(key, "/") && !strings.HasPrefix(key, Home) {
			key = Home + key
		}
		if !ValidKey(key) || !db.ValidUUID(x.set) {
			continue
		}
		target := keyPath(x.set, key)
		if _, err := s.root.Lstat(target); err == nil {
			if err := forget(); err != nil {
				return copied, err
			}
			continue
		}
		rc, err := blobs.GetVersion(ctx, blobKey(x.set, x.key), x.version)
		if err != nil {
			return copied, fmt.Errorf("reading %s of %s: %w", x.key, x.set, err)
		}
		data, err := io.ReadAll(rc)
		rc.Close()
		if err != nil {
			return copied, err
		}
		if err := s.root.MkdirAll(path.Dir(target), 0o755); err != nil {
			return copied, err
		}
		if err := s.replace(target, data, os.FileMode(x.mode&0o777)); err != nil {
			return copied, err
		}
		if err := forget(); err != nil {
			return copied, err
		}
		copied++
	}
	return copied, nil
}

// blobKey is where the blob store kept a set's file: <set>/<path>, an
// absolute path keeping its own slash.
func blobKey(set, key string) string {
	if strings.HasPrefix(key, "/") {
		return set + key
	}
	return set + "/" + key
}

// relayout moves the files of each copy copy_relayout lists under ~/.
func (s *Store) relayout(ctx context.Context) (int, error) {
	var ids []string
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT copy_id::text FROM copy_relayout`)
		if err != nil {
			return err
		}
		ids, err = pgx.CollectRows(rows, pgx.RowTo[string])
		return err
	})
	if err != nil {
		return 0, err
	}
	made := 0
	for _, id := range ids {
		n, err := s.underHome(id)
		made += n
		if err != nil {
			return made, fmt.Errorf("moving %s's files under ~/: %w", id, err)
		}
		if err := s.db.Transact(ctx, func(tx db.Tx) error {
			_, err := tx.Exec(ctx, `DELETE FROM copy_relayout WHERE copy_id = $1`, id)
			return err
		}); err != nil {
			return made, err
		}
	}
	return made, nil
}

// underHome moves everything in a copy's directory but ~ under ~/.
func (s *Store) underHome(copy string) (int, error) {
	if !db.ValidUUID(copy) {
		return 0, nil
	}
	entries, err := fs.ReadDir(s.root.FS(), copy)
	if errors.Is(err, fs.ErrNotExist) {
		return 0, nil
	}
	if err != nil {
		return 0, err
	}
	home := copy + "/" + strings.TrimSuffix(Home, "/")
	if err := s.root.MkdirAll(home, 0o755); err != nil {
		return 0, err
	}
	n := 0
	for _, e := range entries {
		if e.Name()+"/" == Home {
			continue
		}
		if err := s.root.Rename(copy+"/"+e.Name(), home+"/"+e.Name()); err != nil {
			return n, err
		}
		n++
	}
	return n, nil
}

// moves moves each path copy_moves lists from one copy to the other, when
// the one has it and the other does not.
func (s *Store) moves(ctx context.Context) (int, error) {
	type move struct{ from, to, path string }
	var moves []move
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT from_copy::text, to_copy::text, path FROM copy_moves`)
		if err != nil {
			return err
		}
		moves, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (move, error) {
			var m move
			err := r.Scan(&m.from, &m.to, &m.path)
			return m, err
		})
		return err
	})
	if err != nil {
		return 0, err
	}
	made := 0
	for _, m := range moves {
		if db.ValidUUID(m.from) && db.ValidUUID(m.to) && ValidKey(m.path) {
			from, to := keyPath(m.from, m.path), keyPath(m.to, m.path)
			_, ferr := s.root.Lstat(from)
			_, terr := s.root.Lstat(to)
			if ferr == nil && errors.Is(terr, fs.ErrNotExist) {
				if err := s.root.MkdirAll(path.Dir(to), 0o755); err != nil {
					return made, err
				}
				if err := s.root.Rename(from, to); err != nil {
					return made, fmt.Errorf("moving %s from %s to %s: %w", m.path, m.from, m.to, err)
				}
				made++
			}
		}
		if err := s.db.Transact(ctx, func(tx db.Tx) error {
			_, err := tx.Exec(ctx, `DELETE FROM copy_moves WHERE from_copy = $1 AND to_copy = $2 AND path = $3`,
				m.from, m.to, m.path)
			return err
		}); err != nil {
			return made, err
		}
	}
	return made, nil
}
