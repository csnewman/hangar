package profile

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/pem"
	"errors"
	"fmt"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/jackc/pgx/v5"
	"golang.org/x/crypto/ssh"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/blob"
	"github.com/csnewman/hangar/internal/db"
)

// Channel carries a user's ID when their profile, or who holds one of
// their locks, changes.
const Channel = "hangar_profile"

// lockLease is how long a lock is held for an environment without being
// renewed. Its agent renews it while the lock is there; one that stops, by
// dying or being suspended, lets it go when this runs out. It matches
// Claude's own staleness for its lock.
const lockLease = 60 * time.Second

var (
	ErrNotFound = errors.New("not found")
	ErrInvalid  = errors.New("invalid")
)

// File is one file of a profile.
type File struct {
	Path string
	// Data is the file's contents, where they were asked for.
	Data []byte
	Size int64
	Mode uint32
	// TrustedOnly keeps the file from environments not trusted with their
	// owner's credentials: they are told it is removed.
	TrustedOnly bool
	Version     int64
	Deleted     bool
	UpdatedAt   time.Time
}

// Key is one of a user's SSH keys, as anyone may see it.
type Key struct {
	ID        string
	Name      string
	PublicKey string
	// Fingerprint is the key's SHA256 fingerprint, as ssh-keygen -l shows.
	Fingerprint string
	CreatedAt   time.Time
}

// Store keeps profiles: each file's path, mode and version in the database,
// its contents in a versioned bucket as <user>/<path> (blobKey), the row
// naming the version that holds them.
type Store struct {
	db     *db.DB
	blobs  blob.Versioned
	sealer *Sealer
	// everyone are the paths the server shares for every user beside the
	// DefaultPaths.
	everyone []string
}

// ShareForEveryone shares paths for every user beside the DefaultPaths, as
// a server is set up to. Each is one a user could share for themselves.
func (s *Store) ShareForEveryone(paths []string) error {
	for _, p := range paths {
		if err := CheckUserPath(p); err != nil {
			return err
		}
		if !slices.Contains(DefaultPaths, p) && !slices.Contains(s.everyone, p) {
			s.everyone = append(s.everyone, p)
		}
	}
	return nil
}

// UserPaths is the set a user shares: the DefaultPaths, the server's, and
// own, the user's own.
func (s *Store) UserPaths(own []string) Paths {
	return UserPaths(append(slices.Clone(s.everyone), own...))
}

// NewStore returns a store keeping files' contents in blobs. A nil sealer
// keeps no SSH keys: they are refused.
func NewStore(d *db.DB, blobs blob.Versioned, sealer *Sealer) *Store {
	return &Store{db: d, blobs: blobs, sealer: sealer}
}

// KeepsSecrets reports whether the store has a key to keep SSH keys with.
func (s *Store) KeepsSecrets() bool { return s.sealer != nil }

// blobKey is where a set's file is kept: <set>/<path>, an absolute path
// keeping its own slash.
func blobKey(set, path string) string {
	if strings.HasPrefix(path, "/") {
		return set + path
	}
	return set + "/" + path
}

// setLock serializes a file set's writes until the transaction ends.
func setLock(ctx context.Context, tx db.Tx, set string) error {
	_, err := tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended($1, 0))`, "profile:"+set)
	return err
}

// Files returns a user's files, removed ones included, with their contents.
// Without trusted, a trusted-only file is given as removed.
func (s *Store) Files(ctx context.Context, set string, trusted bool) ([]File, error) {
	return s.files(ctx, set, trusted, 0, "", true)
}

// List returns, as Files does, a user's files, without their contents.
func (s *Store) List(ctx context.Context, set string, trusted bool) ([]File, error) {
	return s.files(ctx, set, trusted, 0, "", false)
}

// FilesUnder returns, as Files does, a user's files whose paths start with
// dir: those in a directory and below, for a dir ending in a slash.
func (s *Store) FilesUnder(ctx context.Context, set, dir string, trusted bool) ([]File, error) {
	return s.files(ctx, set, trusted, 0, dir, true)
}

// File returns one of a user's files, with its contents, or ErrNotFound if
// it is not there or was removed.
func (s *Store) File(ctx context.Context, set, path string, trusted bool) (File, error) {
	files, err := s.files(ctx, set, trusted, 0, path, true)
	if err != nil {
		return File{}, err
	}
	for _, f := range files {
		if f.Path == path && !f.Deleted {
			return f, nil
		}
	}
	return File{}, ErrNotFound
}

// Since returns the files changed after version.
func (s *Store) Since(ctx context.Context, set string, trusted bool, version int64) ([]File, error) {
	return s.files(ctx, set, trusted, version, "", true)
}

func (s *Store) files(ctx context.Context, set string, trusted bool, after int64, prefix string, contents bool) ([]File, error) {
	if !db.ValidUUID(set) {
		return nil, ErrNotFound
	}
	var out []File
	var versions []string
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT path, coalesce(version_id, ''), size, mode, version, deleted, trusted_only, updated_at
			FROM set_files
			WHERE set_id = $1 AND version > $2 AND starts_with(path, $3) ORDER BY version`, set, after, prefix)
		if err != nil {
			return err
		}
		out, versions = nil, nil
		for rows.Next() {
			var f File
			var mode int32
			var vid string
			if err := rows.Scan(&f.Path, &vid, &f.Size, &mode, &f.Version, &f.Deleted, &f.TrustedOnly, &f.UpdatedAt); err != nil {
				return err
			}
			f.Mode = uint32(mode)
			if f.TrustedOnly && !trusted {
				// Gone, as far as such an environment knows: one that was
				// given it before it was made trusted-only removes it.
				f.Deleted, f.Size, vid = true, 0, ""
			}
			out = append(out, f)
			versions = append(versions, vid)
		}
		return rows.Err()
	})
	if err != nil || !contents {
		return out, err
	}
	// Read side by side: a whole profile is hundreds of small objects.
	var wg sync.WaitGroup
	var mu sync.Mutex
	var errs []error
	sem := make(chan struct{}, 16)
	for i := range out {
		if out[i].Deleted || versions[i] == "" {
			continue
		}
		wg.Add(1)
		sem <- struct{}{}
		go func() {
			defer func() { <-sem; wg.Done() }()
			data, err := blob.ReadVersion(ctx, s.blobs, blobKey(set, out[i].Path), versions[i])
			if err != nil {
				mu.Lock()
				errs = append(errs, fmt.Errorf("%s: %w", out[i].Path, err))
				mu.Unlock()
				return
			}
			out[i].Data = data
		}()
	}
	wg.Wait()
	if len(errs) > 0 {
		return nil, errors.Join(errs...)
	}
	return out, nil
}

// Put writes a file, returning it as stored.
func (s *Store) Put(ctx context.Context, set, path string, data []byte, mode uint32) (File, error) {
	paths, err := s.SetPaths(ctx, set)
	if err != nil {
		return File{}, err
	}
	if !paths.Synced(path) {
		return File{}, fmt.Errorf("%w: %s is not shared", ErrInvalid, path)
	}
	if len(data) > MaxFileSize {
		return File{}, fmt.Errorf("%w: %s is larger than %d bytes", ErrInvalid, path, MaxFileSize)
	}
	mode &= 0o777
	if mode == 0 {
		mode = 0o644
		if TrustedOnlyByDefault(path) {
			mode = 0o600
		}
	}
	return s.write(ctx, set, path, data, mode, false)
}

// TrustedOnly reports whether a file is kept from untrusted environments: as
// its setting says, or, for one not made yet, as its path's default.
func (s *Store) TrustedOnly(ctx context.Context, set, path string) (bool, error) {
	if !db.ValidUUID(set) {
		return false, ErrNotFound
	}
	only := TrustedOnlyByDefault(path)
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		err := tx.QueryRow(ctx, `SELECT trusted_only FROM set_files WHERE set_id = $1 AND path = $2`,
			set, path).Scan(&only)
		if errors.Is(err, pgx.ErrNoRows) {
			return nil
		}
		return err
	})
	return only, err
}

// SetSettings changes a file's mode and whether it is trusted-only, as a
// new version, which reaches every environment as any change does.
func (s *Store) SetSettings(ctx context.Context, set, path string, mode uint32, trustedOnly bool) (File, error) {
	if !db.ValidUUID(set) {
		return File{}, ErrNotFound
	}
	mode &= 0o777
	if mode == 0 {
		return File{}, fmt.Errorf("%w: a file needs a mode", ErrInvalid)
	}
	var f File
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		if err := setLock(ctx, tx, set); err != nil {
			return err
		}
		var version int64
		if err := tx.QueryRow(ctx, `INSERT INTO set_versions (set_id, version) VALUES ($1, 1)
			ON CONFLICT (set_id) DO UPDATE SET version = set_versions.version + 1
			RETURNING version`, set).Scan(&version); err != nil {
			return err
		}
		var m int32
		err := tx.QueryRow(ctx, `UPDATE set_files SET mode = $3, trusted_only = $4, version = $5, updated_at = now()
			WHERE set_id = $1 AND path = $2 AND NOT deleted
			RETURNING path, size, mode, version, trusted_only, updated_at`,
			set, path, int32(mode), trustedOnly, version).
			Scan(&f.Path, &f.Size, &m, &f.Version, &f.TrustedOnly, &f.UpdatedAt)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		f.Mode = uint32(m)
		if err := audit.Record(ctx, tx, audit.Event{Action: "profile.file_settings",
			Target:  audit.Ref{Type: audit.KindFile, ID: set + ":" + path, Name: path},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: set}},
			Details: map[string]any{"mode": fmt.Sprintf("%#o", mode), "trusted_only": trustedOnly}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, set)
	})
	return f, err
}

// Delete removes a file, leaving a mark that it is gone.
func (s *Store) Delete(ctx context.Context, set, path string) (File, error) {
	if !ValidKey(path) {
		return File{}, fmt.Errorf("%w: %q is not a file's path", ErrInvalid, path)
	}
	return s.write(ctx, set, path, nil, 0o644, true)
}

func (s *Store) write(ctx context.Context, set, path string, data []byte, mode uint32, deleted bool) (File, error) {
	if !db.ValidUUID(set) {
		return File{}, ErrNotFound
	}
	key := blobKey(set, path)
	f := File{Path: path, Data: data, Size: int64(len(data)), Mode: mode, Deleted: deleted}
	// Every version put, one per attempt the transaction makes: all but
	// the one it commits are let go of after.
	var put []string
	var old, kept string
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		old, kept = "", ""
		// A profile's set is made with its first file.
		if _, err := tx.Exec(ctx, `INSERT INTO file_sets (id, user_id) SELECT id, id FROM users WHERE id = $1
			ON CONFLICT DO NOTHING`, set); err != nil {
			return err
		}
		// One sequence per set (set_versions), so a session asks for
		// everything after the last version it sent. A set's writes take
		// turns, so no two take the same number.
		if err := setLock(ctx, tx, set); err != nil {
			return err
		}
		var total int64
		if err := tx.QueryRow(ctx, `SELECT coalesce(sum(size), 0) FROM set_files
			WHERE set_id = $1 AND path <> $2`, set, path).Scan(&total); err != nil {
			return err
		}
		if total+f.Size > MaxProfileSize {
			return fmt.Errorf("%w: the profile would hold more than %d MiB", ErrInvalid, MaxProfileSize>>20)
		}
		err := tx.QueryRow(ctx, `SELECT coalesce(version_id, '') FROM set_files WHERE set_id = $1 AND path = $2`,
			set, path).Scan(&old)
		if err != nil && !errors.Is(err, pgx.ErrNoRows) {
			return err
		}
		if !deleted {
			vid, err := s.blobs.PutVersion(ctx, key, bytes.NewReader(data), f.Size)
			if err != nil {
				return fmt.Errorf("keeping %s: %w", path, err)
			}
			put = append(put, vid)
			kept = vid
		}
		var version int64
		if err := tx.QueryRow(ctx, `INSERT INTO set_versions (set_id, version) VALUES ($1, 1)
			ON CONFLICT (set_id) DO UPDATE SET version = set_versions.version + 1
			RETURNING version`, set).Scan(&version); err != nil {
			return err
		}
		err = tx.QueryRow(ctx, `
			INSERT INTO set_files (set_id, path, version_id, size, mode, deleted, version, trusted_only)
			VALUES ($1, $2, nullif($3, ''), $4, $5, $6, $7, $8)
			ON CONFLICT (set_id, path) DO UPDATE SET version_id = EXCLUDED.version_id, size = EXCLUDED.size,
				mode = EXCLUDED.mode, deleted = EXCLUDED.deleted, version = EXCLUDED.version,
				updated_at = now()
			RETURNING version, updated_at, trusted_only`, set, path, kept, f.Size, int32(mode), deleted, version,
			TrustedOnlyByDefault(path)).
			Scan(&f.Version, &f.UpdatedAt, &f.TrustedOnly)
		if err != nil {
			return err
		}
		action := "profile.file_write"
		if deleted {
			action = "profile.file_delete"
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: action,
			Target:  audit.Ref{Type: audit.KindFile, ID: set + ":" + path, Name: path},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: set}},
			Details: map[string]any{"size": f.Size}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, set)
	})
	// What the row no longer names: the version it named before, if the
	// write committed, and every version an attempt put and did not keep.
	// One a failure here leaves is deleted by the bucket's lifecycle once
	// another version replaces it.
	var drop []string
	if err == nil && old != "" {
		drop = append(drop, old)
	}
	for _, v := range put {
		if err != nil || v != kept {
			drop = append(drop, v)
		}
	}
	for _, v := range drop {
		s.blobs.DeleteVersion(context.WithoutCancel(ctx), key, v)
	}
	return f, err
}

// DeleteUser deletes every file kept for a user, once they are gone.
func (s *Store) DeleteSets(ctx context.Context, sets ...string) error {
	var errs []error
	for _, set := range sets {
		if db.ValidUUID(set) {
			errs = append(errs, s.blobs.DeleteAll(ctx, set+"/"))
		}
	}
	return errors.Join(errs...)
}

// SetsOf are the file sets of a user's own: their profile's, and their
// copies of personal packs.
func (s *Store) SetsOf(ctx context.Context, userID string) ([]string, error) {
	if !db.ValidUUID(userID) {
		return nil, ErrNotFound
	}
	out := []string{userID}
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT id::text FROM file_sets WHERE user_id = $1 AND pack_id IS NOT NULL`, userID)
		if err != nil {
			return err
		}
		more, err := pgx.CollectRows(rows, pgx.RowTo[string])
		out = append(out, more...)
		return err
	})
	return out, err
}

// SetPaths are the paths a file set keeps: a profile's, relative to the
// home directory, or a pack's, absolute. A set not made yet is a user's
// profile, as their first file makes it.
func (s *Store) SetPaths(ctx context.Context, set string) (Paths, error) {
	if !db.ValidUUID(set) {
		return nil, ErrNotFound
	}
	var user, pack *string
	var paths []string
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		err := tx.QueryRow(ctx, `SELECT user_id::text, pack_id::text FROM file_sets WHERE id = $1`, set).Scan(&user, &pack)
		if errors.Is(err, pgx.ErrNoRows) {
			user = &set
			return nil
		}
		if err != nil || pack == nil {
			return err
		}
		rows, err := tx.Query(ctx, `SELECT path FROM file_pack_paths WHERE pack_id = $1 ORDER BY path`, *pack)
		if err != nil {
			return err
		}
		paths, err = pgx.CollectRows(rows, pgx.RowTo[string])
		return err
	})
	if err != nil {
		return nil, err
	}
	if pack != nil {
		return Paths(paths), nil
	}
	return s.Paths(ctx, *user)
}

// Keys returns a user's SSH keys.
func (s *Store) Keys(ctx context.Context, userID string) ([]Key, error) {
	if !db.ValidUUID(userID) {
		return nil, ErrNotFound
	}
	var out []Key
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT id, name, public_key, created_at FROM ssh_keys
			WHERE user_id = $1 ORDER BY created_at`, userID)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (Key, error) {
			var k Key
			err := r.Scan(&k.ID, &k.Name, &k.PublicKey, &k.CreatedAt)
			if pub, _, _, _, perr := ssh.ParseAuthorizedKey([]byte(k.PublicKey)); perr == nil {
				k.Fingerprint = ssh.FingerprintSHA256(pub)
			}
			return k, err
		})
		return err
	})
	return out, err
}

// GenerateKey makes a new ed25519 key for a user.
func (s *Store) GenerateKey(ctx context.Context, userID, name string) (Key, error) {
	_, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return Key{}, err
	}
	block, err := ssh.MarshalPrivateKey(priv, name)
	if err != nil {
		return Key{}, err
	}
	return s.addKey(ctx, userID, name, pem.EncodeToMemory(block))
}

// ImportKey adds a user's existing private key, in OpenSSH or PEM form. A
// key with a passphrase is refused: the server signs without anyone there
// to type it.
func (s *Store) ImportKey(ctx context.Context, userID, name string, private []byte) (Key, error) {
	return s.addKey(ctx, userID, name, private)
}

func (s *Store) addKey(ctx context.Context, userID, name string, private []byte) (Key, error) {
	if !db.ValidUUID(userID) {
		return Key{}, ErrNotFound
	}
	name = strings.TrimSpace(name)
	if name == "" || len(name) > 100 {
		return Key{}, fmt.Errorf("%w: a key needs a name of up to 100 characters", ErrInvalid)
	}
	signer, err := ssh.ParsePrivateKey(private)
	if err != nil {
		var missing *ssh.PassphraseMissingError
		if errors.As(err, &missing) {
			return Key{}, fmt.Errorf("%w: the key has a passphrase; remove it first (ssh-keygen -p)", ErrInvalid)
		}
		return Key{}, fmt.Errorf("%w: not a private key: %v", ErrInvalid, err)
	}
	pub := strings.TrimSpace(string(ssh.MarshalAuthorizedKey(signer.PublicKey()))) + " " + name
	k := Key{Name: name, PublicKey: pub, Fingerprint: ssh.FingerprintSHA256(signer.PublicKey())}
	err = s.db.Transact(ctx, func(tx db.Tx) error {
		// Sealed inside, so a retry seals afresh rather than reusing a nonce
		// bound to nothing.
		sealed, err := s.sealer.seal(private, userID+"/ssh/"+k.Fingerprint)
		if err != nil {
			return err
		}
		if err := tx.QueryRow(ctx, `INSERT INTO ssh_keys (user_id, name, public_key, private_key)
			VALUES ($1, $2, $3, $4) RETURNING id, created_at`, userID, name, pub, sealed).Scan(&k.ID, &k.CreatedAt); err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "ssh_key.add",
			Target:  audit.Ref{Type: audit.KindSSHKey, ID: k.ID, Name: name},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: userID}},
			Details: map[string]any{"fingerprint": k.Fingerprint}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, userID)
	})
	return k, err
}

// DeleteKey removes one of a user's keys.
func (s *Store) DeleteKey(ctx context.Context, userID, id string) error {
	if !db.ValidUUID(userID) || !db.ValidUUID(id) {
		return ErrNotFound
	}
	return s.db.Transact(ctx, func(tx db.Tx) error {
		var name string
		err := tx.QueryRow(ctx, `DELETE FROM ssh_keys WHERE user_id = $1 AND id = $2 RETURNING name`, userID, id).Scan(&name)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "ssh_key.delete",
			Target:  audit.Ref{Type: audit.KindSSHKey, ID: id, Name: name},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: userID}}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, userID)
	})
}

// Signers returns a user's keys, able to sign.
func (s *Store) Signers(ctx context.Context, userID string) ([]ssh.Signer, error) {
	if !db.ValidUUID(userID) {
		return nil, ErrNotFound
	}
	type row struct {
		pub     string
		private []byte
	}
	var rows []row
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		r, err := tx.Query(ctx, `SELECT public_key, private_key FROM ssh_keys WHERE user_id = $1 ORDER BY created_at`, userID)
		if err != nil {
			return err
		}
		rows, err = pgx.CollectRows(r, func(r pgx.CollectableRow) (row, error) {
			var x row
			return x, r.Scan(&x.pub, &x.private)
		})
		return err
	})
	if err != nil {
		return nil, err
	}
	var out []ssh.Signer
	for _, r := range rows {
		pub, _, _, _, err := ssh.ParseAuthorizedKey([]byte(r.pub))
		if err != nil {
			continue
		}
		private, err := s.sealer.open(r.private, userID+"/ssh/"+ssh.FingerprintSHA256(pub))
		if err != nil {
			return nil, err
		}
		signer, err := ssh.ParsePrivateKey(private)
		if err != nil {
			return nil, err
		}
		out = append(out, signer)
	}
	return out, nil
}

// Lock takes or renews one of a user's locks on behalf of one environment,
// reporting whether that environment holds it.
func (s *Store) Lock(ctx context.Context, set, path, environment string) (bool, error) {
	var held bool
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		var holder string
		err := tx.QueryRow(ctx, `
			INSERT INTO set_locks (set_id, path, environment, expires_at) VALUES ($1, $2, $3, now() + $4::interval)
			ON CONFLICT (set_id, path) DO UPDATE SET environment = EXCLUDED.environment, expires_at = EXCLUDED.expires_at
				WHERE set_locks.environment = EXCLUDED.environment OR set_locks.expires_at < now()
			RETURNING environment::text`, set, path, environment, lockLease).Scan(&holder)
		if errors.Is(err, pgx.ErrNoRows) {
			held = false
			return nil
		}
		if err != nil {
			return err
		}
		held = true
		return db.Notify(ctx, tx, Channel, set)
	})
	return held, err
}

// Unlock releases one of a user's locks if the environment holds it.
func (s *Store) Unlock(ctx context.Context, set, path, environment string) error {
	return s.db.Transact(ctx, func(tx db.Tx) error {
		tag, err := tx.Exec(ctx, `DELETE FROM set_locks WHERE set_id = $1 AND path = $2 AND environment = $3`,
			set, path, environment)
		if err != nil || tag.RowsAffected() == 0 {
			return err
		}
		return db.Notify(ctx, tx, Channel, set)
	})
}

// Paths returns the paths a user shares: everyone's, and their own.
func (s *Store) Paths(ctx context.Context, userID string) (Paths, error) {
	own, err := s.OwnPaths(ctx, userID)
	if err != nil {
		return nil, err
	}
	return s.UserPaths(own), nil
}

// OwnPaths returns the paths a user has added.
func (s *Store) OwnPaths(ctx context.Context, userID string) ([]string, error) {
	if !db.ValidUUID(userID) {
		return nil, ErrNotFound
	}
	var out []string
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT path FROM profile_paths WHERE user_id = $1 ORDER BY path`, userID)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, pgx.RowTo[string])
		return err
	})
	return out, err
}

// AddPath shares another path for a user: a file, or a directory, ending
// in a slash, and everything under it.
func (s *Store) AddPath(ctx context.Context, userID, path string) error {
	if !db.ValidUUID(userID) {
		return ErrNotFound
	}
	if err := CheckUserPath(path); err != nil {
		return fmt.Errorf("%w: %v", ErrInvalid, err)
	}
	if slices.Contains(DefaultPaths, path) || slices.Contains(s.everyone, path) {
		return fmt.Errorf("%w: %s is already shared", ErrInvalid, path)
	}
	own, err := s.OwnPaths(ctx, userID)
	if err != nil {
		return err
	}
	if slices.Contains(own, path) {
		return nil
	}
	if by, ok := s.UserPaths(own).Covering(path); ok {
		return fmt.Errorf("%w: %s is already shared, by %s", ErrInvalid, path, by)
	}
	return s.db.Transact(ctx, func(tx db.Tx) error {
		var n int
		if err := tx.QueryRow(ctx, `SELECT count(*) FROM profile_paths WHERE user_id = $1`, userID).Scan(&n); err != nil {
			return err
		}
		if n >= MaxUserPaths {
			return fmt.Errorf("%w: a profile shares at most %d paths of its own", ErrInvalid, MaxUserPaths)
		}
		if _, err := tx.Exec(ctx, `INSERT INTO profile_paths (user_id, path) VALUES ($1, $2)
			ON CONFLICT DO NOTHING`, userID, path); err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "profile.share",
			Target:  audit.Ref{Type: audit.KindPath, ID: userID + ":" + path, Name: path},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: userID}}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, userID)
	})
}

// RemovePath stops sharing a path the user added. The profile's copies of
// what only it shared are dropped; environments keep theirs, as files of
// their own.
func (s *Store) RemovePath(ctx context.Context, userID, path string) error {
	if !db.ValidUUID(userID) {
		return ErrNotFound
	}
	type file struct{ path, version string }
	var dropped []file
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		dropped = nil
		tag, err := tx.Exec(ctx, `DELETE FROM profile_paths WHERE user_id = $1 AND path = $2`, userID, path)
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return ErrNotFound
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "profile.unshare",
			Target:  audit.Ref{Type: audit.KindPath, ID: userID + ":" + path, Name: path},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: userID}}}); err != nil {
			return err
		}
		rows, err := tx.Query(ctx, `SELECT path FROM profile_paths WHERE user_id = $1`, userID)
		if err != nil {
			return err
		}
		own, err := pgx.CollectRows(rows, pgx.RowTo[string])
		if err != nil {
			return err
		}
		still := s.UserPaths(own)
		rows, err = tx.Query(ctx, `SELECT path, coalesce(version_id, '') FROM set_files WHERE set_id = $1`, userID)
		if err != nil {
			return err
		}
		files, err := pgx.CollectRows(rows, func(r pgx.CollectableRow) (file, error) {
			var f file
			return f, r.Scan(&f.path, &f.version)
		})
		if err != nil {
			return err
		}
		for _, f := range files {
			if !still.Synced(f.path) {
				if _, err := tx.Exec(ctx, `DELETE FROM set_files WHERE set_id = $1 AND path = $2`, userID, f.path); err != nil {
					return err
				}
				if f.version != "" {
					dropped = append(dropped, f)
				}
			}
		}
		return db.Notify(ctx, tx, Channel, userID)
	})
	if err == nil {
		for _, f := range dropped {
			s.blobs.DeleteVersion(context.WithoutCancel(ctx), blobKey(userID, f.path), f.version)
		}
	}
	return err
}

// RecordSignature records that a user's key signed something on their
// behalf: every signature is a use of the key.
func (s *Store) RecordSignature(ctx context.Context, userID, fingerprint string, err error) {
	s.db.Transact(ctx, func(tx db.Tx) error {
		details := map[string]any{"fingerprint": fingerprint}
		if err != nil {
			details["error"] = err.Error()
		}
		return audit.Record(ctx, tx, audit.Event{Action: "ssh_key.sign",
			Target:  audit.Ref{Type: audit.KindSSHKey, ID: fingerprint, Name: fingerprint},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: userID}}, Details: details})
	})
}

// LoginKey is a public key its owner signs in to their environments with.
type LoginKey struct {
	ID          string
	Name        string
	PublicKey   string
	Fingerprint string
	CreatedAt   time.Time
}

// AddLoginKey lets a user sign in to their environments with a public key,
// in authorized_keys form.
func (s *Store) AddLoginKey(ctx context.Context, userID, name, public string) (LoginKey, error) {
	if !db.ValidUUID(userID) {
		return LoginKey{}, ErrNotFound
	}
	pub, comment, _, _, err := ssh.ParseAuthorizedKey([]byte(strings.TrimSpace(public)))
	if err != nil {
		return LoginKey{}, fmt.Errorf("%w: not a public key in authorized_keys form (ssh-ed25519 AAAA...): %v", ErrInvalid, err)
	}
	name = strings.TrimSpace(name)
	if name == "" {
		name = comment
	}
	if name == "" || len(name) > 100 {
		return LoginKey{}, fmt.Errorf("%w: a key needs a name of up to 100 characters", ErrInvalid)
	}
	k := LoginKey{Name: name, PublicKey: strings.TrimSpace(string(ssh.MarshalAuthorizedKey(pub))), Fingerprint: ssh.FingerprintSHA256(pub)}
	err = s.db.Transact(ctx, func(tx db.Tx) error {
		err := tx.QueryRow(ctx, `INSERT INTO ssh_login_keys (user_id, name, public_key, fingerprint) VALUES ($1, $2, $3, $4)
			RETURNING id, created_at`, userID, name, k.PublicKey, k.Fingerprint).Scan(&k.ID, &k.CreatedAt)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: that key is already added", ErrInvalid)
		}
		if err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "login_key.add",
			Target:  audit.Ref{Type: audit.KindSSHKey, ID: k.ID, Name: name},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: userID}},
			Details: map[string]any{"fingerprint": k.Fingerprint}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, userID)
	})
	return k, err
}

// LoginKeys returns the keys a user signs in with.
func (s *Store) LoginKeys(ctx context.Context, userID string) ([]LoginKey, error) {
	if !db.ValidUUID(userID) {
		return nil, ErrNotFound
	}
	var out []LoginKey
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT id, name, public_key, fingerprint, created_at FROM ssh_login_keys
			WHERE user_id = $1 ORDER BY created_at`, userID)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (LoginKey, error) {
			var k LoginKey
			return k, r.Scan(&k.ID, &k.Name, &k.PublicKey, &k.Fingerprint, &k.CreatedAt)
		})
		return err
	})
	if out == nil {
		out = []LoginKey{}
	}
	return out, err
}

// DeleteLoginKey stops a key signing its owner in.
func (s *Store) DeleteLoginKey(ctx context.Context, userID, id string) error {
	if !db.ValidUUID(userID) || !db.ValidUUID(id) {
		return ErrNotFound
	}
	return s.db.Transact(ctx, func(tx db.Tx) error {
		var name string
		err := tx.QueryRow(ctx, `DELETE FROM ssh_login_keys WHERE user_id = $1 AND id = $2 RETURNING name`, userID, id).Scan(&name)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "login_key.delete",
			Target:  audit.Ref{Type: audit.KindSSHKey, ID: id, Name: name},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: userID}}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, userID)
	})
}

// KeyOwner is a person a sign-in key belongs to.
type KeyOwner struct {
	UserID   string
	Username string
	Admin    bool
}

// LoginKeyOwners are the enabled people who sign in with the key of this
// fingerprint: nearly always one, though nothing stops two people adding
// the same public key.
func (s *Store) LoginKeyOwners(ctx context.Context, fingerprint string) ([]KeyOwner, error) {
	var out []KeyOwner
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT u.id::text, u.username, u.is_admin FROM ssh_login_keys k
			JOIN users u ON u.id = k.user_id
			WHERE k.fingerprint = $1 AND u.disabled_at IS NULL AND u.kind = 'person'
			ORDER BY k.created_at`, fingerprint)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (KeyOwner, error) {
			var o KeyOwner
			return o, r.Scan(&o.UserID, &o.Username, &o.Admin)
		})
		return err
	})
	return out, err
}
