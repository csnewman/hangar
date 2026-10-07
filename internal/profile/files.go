package profile

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path"
	"sort"
	"strings"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/blob"
	"github.com/csnewman/hangar/internal/db"
)

// A file set's files are kept on the files root, in a directory named by
// the set's ID, as every worker serves them to its environments over NFS
// (internal/nfs). A profile's paths are kept as they are, relative to the
// home directory; a pack's absolute ones without their leading slash.

// stagingPrefix names files written beside one they replace: this store's,
// and hangarfs's for a save renamed over a routed name. They are not a
// set's files.
const stagingPrefix = ".hangarfs-"

// keyPath is where a set's file is under the root.
func keyPath(set, key string) string { return set + "/" + strings.TrimPrefix(key, "/") }

// setKey is the key of the file at p in set's directory: a pack's are
// absolute but for those in the home directory, under Home.
func setKey(set, p string, pack bool) string {
	key := strings.TrimPrefix(p, set+"/")
	if pack && !strings.HasPrefix(key, Home) {
		key = "/" + key
	}
	return key
}

// isPack reports whether a set is a pack's, whose paths are absolute.
func (s *Store) isPack(ctx context.Context, set string) (bool, error) {
	var pack bool
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		err := tx.QueryRow(ctx, `SELECT pack_id IS NOT NULL FROM file_sets WHERE id = $1`, set).Scan(&pack)
		if errors.Is(err, pgx.ErrNoRows) {
			return nil
		}
		return err
	})
	return pack, err
}

// sensitivity is which of a set's files are sensitive where that differs
// from the default, and sensitive what a file is, given it.
func (s *Store) sensitivity(ctx context.Context, set string) (map[string]bool, error) {
	out := map[string]bool{}
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT path, sensitive FROM file_sensitivity WHERE set_id = $1`, set)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var p string
			var only bool
			if err := rows.Scan(&p, &only); err != nil {
				return err
			}
			out[p] = only
		}
		return rows.Err()
	})
	return out, err
}

func sensitive(marks map[string]bool, key string) bool {
	if v, ok := marks[key]; ok {
		return v
	}
	return SensitiveByDefault(key)
}

// List returns a set's files, without their contents. Without
// withSensitive, a sensitive file is left out.
func (s *Store) List(ctx context.Context, set string, withSensitive bool) ([]File, error) {
	if !db.ValidUUID(set) {
		return nil, ErrNotFound
	}
	pack, err := s.isPack(ctx, set)
	if err != nil {
		return nil, err
	}
	marks, err := s.sensitivity(ctx, set)
	if err != nil {
		return nil, err
	}
	out := []File{}
	err = fs.WalkDir(s.root.FS(), set, func(p string, e fs.DirEntry, err error) error {
		if err != nil {
			if errors.Is(err, fs.ErrNotExist) {
				return fs.SkipAll
			}
			return err
		}
		if !e.Type().IsRegular() || strings.HasPrefix(e.Name(), stagingPrefix) {
			return nil
		}
		key := setKey(set, p, pack)
		only := sensitive(marks, key)
		if only && !withSensitive {
			return nil
		}
		info, err := e.Info()
		if err != nil {
			return nil
		}
		out = append(out, File{Path: key, Size: info.Size(), Mode: uint32(info.Mode().Perm()),
			Sensitive: only, UpdatedAt: info.ModTime()})
		return nil
	})
	sort.Slice(out, func(i, j int) bool { return out[i].Path < out[j].Path })
	return out, err
}

// File returns one of a set's files with its contents, or ErrNotFound.
func (s *Store) File(ctx context.Context, set, key string, withSensitive bool) (File, error) {
	if !db.ValidUUID(set) || !ValidKey(key) {
		return File{}, ErrNotFound
	}
	info, err := s.root.Lstat(keyPath(set, key))
	if errors.Is(err, fs.ErrNotExist) || err == nil && !info.Mode().IsRegular() {
		return File{}, ErrNotFound
	}
	if err != nil {
		return File{}, err
	}
	marks, err := s.sensitivity(ctx, set)
	if err != nil {
		return File{}, err
	}
	only := sensitive(marks, key)
	if only && !withSensitive {
		return File{}, ErrNotFound
	}
	data, err := s.root.ReadFile(keyPath(set, key))
	if err != nil {
		return File{}, err
	}
	return File{Path: key, Data: data, Size: int64(len(data)), Mode: uint32(info.Mode().Perm()),
		Sensitive: only, UpdatedAt: info.ModTime()}, nil
}

// size is what a set's files take, but the one at except.
func (s *Store) size(set, except string) int64 {
	var total int64
	fs.WalkDir(s.root.FS(), set, func(p string, e fs.DirEntry, err error) error {
		if err == nil && e.Type().IsRegular() && p != except {
			if info, err := e.Info(); err == nil {
				total += info.Size()
			}
		}
		return nil
	})
	return total
}

// Put writes a file, returning it as stored. It is written beside its
// name and renamed over it, so an environment reading it sees it whole.
func (s *Store) Put(ctx context.Context, set, key string, data []byte, mode uint32) (File, error) {
	paths, err := s.SetPaths(ctx, set)
	if err != nil {
		return File{}, err
	}
	if !paths.Synced(key) {
		return File{}, fmt.Errorf("%w: %s is not shared", ErrInvalid, key)
	}
	if len(data) > MaxFileSize {
		return File{}, fmt.Errorf("%w: %s is larger than %d bytes", ErrInvalid, key, MaxFileSize)
	}
	target := keyPath(set, key)
	if s.size(set, target)+int64(len(data)) > MaxProfileSize {
		return File{}, fmt.Errorf("%w: the set would hold more than %d MiB", ErrInvalid, MaxProfileSize>>20)
	}
	mode &= 0o777
	if mode == 0 {
		mode = 0o644
		if SensitiveByDefault(key) {
			mode = 0o600
		}
	}
	if err := s.ensureSet(ctx, set); err != nil {
		return File{}, err
	}
	if err := s.root.MkdirAll(path.Dir(target), 0o755); err != nil {
		return File{}, err
	}
	if err := s.replace(target, data, os.FileMode(mode)); err != nil {
		return File{}, err
	}
	err = s.db.Transact(ctx, func(tx db.Tx) error {
		return audit.Record(ctx, tx, audit.Event{Action: "profile.file_write",
			Target:  audit.Ref{Type: audit.KindFile, ID: set + ":" + key, Name: key},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: set}},
			Details: map[string]any{"size": len(data)}})
	})
	if err != nil {
		return File{}, err
	}
	return s.File(ctx, set, key, true)
}

// replace writes data beside name and renames it over name.
func (s *Store) replace(name string, data []byte, mode os.FileMode) error {
	tmp := path.Join(path.Dir(name), stagingPrefix+randomName())
	f, err := s.root.OpenFile(tmp, os.O_WRONLY|os.O_CREATE|os.O_EXCL, mode)
	if err != nil {
		return err
	}
	_, err = f.Write(data)
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err == nil {
		// Set apart from the umask the file was made under.
		err = s.root.Chmod(tmp, mode)
	}
	if err == nil {
		err = s.root.Rename(tmp, name)
	}
	if err != nil {
		s.root.Remove(tmp)
	}
	return err
}

func randomName() string {
	b := make([]byte, 6)
	rand.Read(b)
	return hex.EncodeToString(b)
}

// ensureSet makes a user's profile's set, as their first file does, and
// the set's directory.
func (s *Store) ensureSet(ctx context.Context, set string) error {
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `INSERT INTO file_sets (id, user_id) SELECT id, id FROM users WHERE id = $1
			ON CONFLICT DO NOTHING`, set)
		return err
	})
	if err != nil {
		return err
	}
	return s.root.MkdirAll(set, 0o755)
}

// SetSettings changes a file's mode and whether it is sensitive.
func (s *Store) SetSettings(ctx context.Context, set, key string, mode uint32, sensitive bool) (File, error) {
	if !db.ValidUUID(set) || !ValidKey(key) {
		return File{}, ErrNotFound
	}
	mode &= 0o777
	if mode == 0 {
		return File{}, fmt.Errorf("%w: a file needs a mode", ErrInvalid)
	}
	info, err := s.root.Lstat(keyPath(set, key))
	if errors.Is(err, fs.ErrNotExist) || err == nil && !info.Mode().IsRegular() {
		return File{}, ErrNotFound
	}
	if err != nil {
		return File{}, err
	}
	if err := s.root.Chmod(keyPath(set, key), os.FileMode(mode)); err != nil {
		return File{}, err
	}
	err = s.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := tx.Exec(ctx, `INSERT INTO file_sensitivity (set_id, path, sensitive) VALUES ($1, $2, $3)
			ON CONFLICT (set_id, path) DO UPDATE SET sensitive = EXCLUDED.sensitive`,
			set, key, sensitive); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "profile.file_settings",
			Target:  audit.Ref{Type: audit.KindFile, ID: set + ":" + key, Name: key},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: set}},
			Details: map[string]any{"mode": fmt.Sprintf("%#o", mode), "sensitive": sensitive}})
	})
	if err != nil {
		return File{}, err
	}
	return s.File(ctx, set, key, true)
}

// Delete removes a file, from the set and every environment given it.
func (s *Store) Delete(ctx context.Context, set, key string) error {
	if !db.ValidUUID(set) || !ValidKey(key) {
		return fmt.Errorf("%w: %q is not a file's path", ErrInvalid, key)
	}
	err := s.root.Remove(keyPath(set, key))
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return err
	}
	return s.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := tx.Exec(ctx, `DELETE FROM file_sensitivity WHERE set_id = $1 AND path = $2`, set, key); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "profile.file_delete",
			Target:  audit.Ref{Type: audit.KindFile, ID: set + ":" + key, Name: key},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: set}}})
	})
}

// DeleteSets deletes every file of file sets, once they are gone.
func (s *Store) DeleteSets(ctx context.Context, sets ...string) error {
	var errs []error
	for _, set := range sets {
		if db.ValidUUID(set) {
			errs = append(errs, s.root.RemoveAll(set))
		}
	}
	return errors.Join(errs...)
}

// dropUnshared removes a set's files that its paths no longer share. An
// environment given them sees them go.
func (s *Store) dropUnshared(set string, still Paths, pack bool) {
	var drop []string
	fs.WalkDir(s.root.FS(), set, func(p string, e fs.DirEntry, err error) error {
		if err != nil || e.IsDir() {
			return nil
		}
		if !still.Synced(setKey(set, p, pack)) {
			drop = append(drop, p)
		}
		return nil
	})
	for _, p := range drop {
		s.root.Remove(p)
	}
}

// hidden are the paths in a set, as the set's directory has them, kept
// from an environment given none of its owner's sensitive files.
func (s *Store) hidden(ctx context.Context, set string, pack bool) ([]string, error) {
	marks, err := s.sensitivity(ctx, set)
	if err != nil {
		return nil, err
	}
	var out []string
	for key, only := range marks {
		if only {
			out = append(out, strings.TrimPrefix(key, "/"))
		}
	}
	for _, key := range secretPaths {
		// A pack's are in its home directory.
		if pack {
			key = Home + key
		}
		if _, set := marks[key]; !set {
			out = append(out, key)
		}
	}
	return out, nil
}

// MigrateFrom copies the files the database names in the blob store --
// where they were kept before they were kept on the files root -- that the
// root does not have yet, and reports how many it copied.
func (s *Store) MigrateFrom(ctx context.Context, blobs blob.Versioned) (int, error) {
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
		target := keyPath(x.set, x.key)
		if _, err := s.root.Lstat(target); err == nil {
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
