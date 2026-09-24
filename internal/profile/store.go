package profile

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/pem"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"golang.org/x/crypto/ssh"

	"github.com/csnewman/hangar/internal/audit"
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
	Path      string
	Data      []byte
	Mode      uint32
	Version   int64
	Deleted   bool
	UpdatedAt time.Time
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

// Store keeps profiles.
type Store struct {
	db     *db.DB
	sealer *Sealer
}

// NewStore returns a store. A nil sealer keeps no secrets: credentials and
// SSH keys are refused.
func NewStore(d *db.DB, sealer *Sealer) *Store {
	return &Store{db: d, sealer: sealer}
}

// KeepsSecrets reports whether the store has a key to keep secrets with.
func (s *Store) KeepsSecrets() bool { return s.sealer != nil }

// Files returns a user's files, removed ones included, with secrets opened.
// Without secrets, secret files are left out.
func (s *Store) Files(ctx context.Context, userID string, secrets bool) ([]File, error) {
	return s.files(ctx, userID, secrets, 0)
}

// Since returns the files changed after version.
func (s *Store) Since(ctx context.Context, userID string, secrets bool, version int64) ([]File, error) {
	return s.files(ctx, userID, secrets, version)
}

func (s *Store) files(ctx context.Context, userID string, secrets bool, after int64) ([]File, error) {
	if !db.ValidUUID(userID) {
		return nil, ErrNotFound
	}
	var out []File
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT path, data, mode, version, deleted, updated_at FROM profile_files
			WHERE user_id = $1 AND version > $2 ORDER BY version`, userID, after)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (File, error) {
			var f File
			var mode int32
			err := r.Scan(&f.Path, &f.Data, &mode, &f.Version, &f.Deleted, &f.UpdatedAt)
			f.Mode = uint32(mode)
			return f, err
		})
		return err
	})
	if err != nil {
		return nil, err
	}
	kept := out[:0]
	for _, f := range out {
		if Secret(f.Path) {
			if !secrets {
				continue
			}
			if !f.Deleted {
				plain, err := s.sealer.open(f.Data, userID+"/"+f.Path)
				if err != nil {
					return nil, fmt.Errorf("%s: %w", f.Path, err)
				}
				f.Data = plain
			}
		}
		kept = append(kept, f)
	}
	return kept, nil
}

// Put writes a file, returning it as stored.
func (s *Store) Put(ctx context.Context, userID, path string, data []byte, mode uint32) (File, error) {
	paths, err := s.Paths(ctx, userID)
	if err != nil {
		return File{}, err
	}
	if !paths.Synced(path) {
		return File{}, fmt.Errorf("%w: %s is not shared", ErrInvalid, path)
	}
	if len(data) > MaxFileSize {
		return File{}, fmt.Errorf("%w: %s is larger than %d bytes", ErrInvalid, path, MaxFileSize)
	}
	stored := data
	if Secret(path) {
		if stored, err = s.sealer.seal(data, userID+"/"+path); err != nil {
			return File{}, err
		}
	}
	mode &= 0o777
	if mode == 0 {
		mode = 0o644
	}
	return s.write(ctx, userID, path, stored, mode, false, data)
}

// Delete removes a file, leaving a mark that it is gone.
func (s *Store) Delete(ctx context.Context, userID, path string) (File, error) {
	if !Valid(path) {
		return File{}, fmt.Errorf("%w: %q is not a path in the home directory", ErrInvalid, path)
	}
	return s.write(ctx, userID, path, []byte{}, 0o644, true, nil)
}

func (s *Store) write(ctx context.Context, userID, path string, stored []byte, mode uint32, deleted bool, plain []byte) (File, error) {
	if !db.ValidUUID(userID) {
		return File{}, ErrNotFound
	}
	f := File{Path: path, Data: plain, Mode: mode, Deleted: deleted}
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		// One sequence per user, so a session asks for everything after the
		// last version it sent. A user's writes take turns, so no two take
		// the same number.
		if _, err := tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtextextended($1, 0))`, "profile:"+userID); err != nil {
			return err
		}
		var total int64
		if err := tx.QueryRow(ctx, `SELECT coalesce(sum(length(data)), 0) FROM profile_files
			WHERE user_id = $1 AND path <> $2`, userID, path).Scan(&total); err != nil {
			return err
		}
		if total+int64(len(stored)) > MaxProfileSize {
			return fmt.Errorf("%w: the profile would hold more than %d MiB", ErrInvalid, MaxProfileSize>>20)
		}
		err := tx.QueryRow(ctx, `
			INSERT INTO profile_files (user_id, path, data, mode, deleted, version)
			VALUES ($1, $2, $3, $4, $5,
				(SELECT coalesce(max(version), 0) + 1 FROM profile_files WHERE user_id = $1))
			ON CONFLICT (user_id, path) DO UPDATE SET data = EXCLUDED.data, mode = EXCLUDED.mode,
				deleted = EXCLUDED.deleted, version = EXCLUDED.version, updated_at = now()
			RETURNING version, updated_at`, userID, path, stored, int32(mode), deleted).
			Scan(&f.Version, &f.UpdatedAt)
		if err != nil {
			return err
		}
		action := "profile.file_write"
		if deleted {
			action = "profile.file_delete"
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: action,
			Target:  audit.Ref{Type: audit.KindFile, ID: userID + ":" + path, Name: path},
			Related: []audit.Ref{{Type: audit.KindOwner, ID: userID}},
			Details: map[string]any{"size": len(plain), "secret": Secret(path)}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, userID)
	})
	return f, err
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
func (s *Store) Lock(ctx context.Context, userID, path, environment string) (bool, error) {
	var held bool
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		var holder string
		err := tx.QueryRow(ctx, `
			INSERT INTO profile_locks (user_id, path, environment, expires_at) VALUES ($1, $2, $3, now() + $4::interval)
			ON CONFLICT (user_id, path) DO UPDATE SET environment = EXCLUDED.environment, expires_at = EXCLUDED.expires_at
				WHERE profile_locks.environment = EXCLUDED.environment OR profile_locks.expires_at < now()
			RETURNING environment::text`, userID, path, environment, lockLease).Scan(&holder)
		if errors.Is(err, pgx.ErrNoRows) {
			held = false
			return nil
		}
		if err != nil {
			return err
		}
		held = true
		return db.Notify(ctx, tx, Channel, userID)
	})
	return held, err
}

// Unlock releases one of a user's locks if the environment holds it.
func (s *Store) Unlock(ctx context.Context, userID, path, environment string) error {
	return s.db.Transact(ctx, func(tx db.Tx) error {
		tag, err := tx.Exec(ctx, `DELETE FROM profile_locks WHERE user_id = $1 AND path = $2 AND environment = $3`,
			userID, path, environment)
		if err != nil || tag.RowsAffected() == 0 {
			return err
		}
		return db.Notify(ctx, tx, Channel, userID)
	})
}

// Paths returns the paths a user shares: everyone's, and their own.
func (s *Store) Paths(ctx context.Context, userID string) (Paths, error) {
	own, err := s.OwnPaths(ctx, userID)
	if err != nil {
		return nil, err
	}
	return UserPaths(own), nil
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
	if slices.Contains(DefaultPaths, path) || (!strings.HasSuffix(path, "/") && UserPaths(nil).Synced(path)) {
		return fmt.Errorf("%w: %s is already shared", ErrInvalid, path)
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
	return s.db.Transact(ctx, func(tx db.Tx) error {
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
		still := UserPaths(own)
		rows, err = tx.Query(ctx, `SELECT path FROM profile_files WHERE user_id = $1`, userID)
		if err != nil {
			return err
		}
		files, err := pgx.CollectRows(rows, pgx.RowTo[string])
		if err != nil {
			return err
		}
		for _, f := range files {
			if !still.Synced(f) {
				if _, err := tx.Exec(ctx, `DELETE FROM profile_files WHERE user_id = $1 AND path = $2`, userID, f); err != nil {
					return err
				}
			}
		}
		return db.Notify(ctx, tx, Channel, userID)
	})
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
