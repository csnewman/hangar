package profile

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/pem"
	"errors"
	"fmt"
	"os"
	"slices"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"golang.org/x/crypto/ssh"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
)

// Channel carries a file set's ID when its files, or the paths it shares,
// change; an empty one when any may have.
const Channel = "hangar_profile"

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
	// owner's credentials.
	TrustedOnly bool
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

// Store keeps file sets -- profiles and packs -- and users' keys. A set's
// files are on the files root (files.go); what is known of them beside
// their contents and modes, in the database.
type Store struct {
	db     *db.DB
	root   *os.Root
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

// NewStore returns a store keeping file sets under the directory root. A
// nil sealer keeps no SSH keys: they are refused.
func NewStore(d *db.DB, root string, sealer *Sealer) (*Store, error) {
	if err := os.MkdirAll(root, 0o755); err != nil {
		return nil, err
	}
	r, err := os.OpenRoot(root)
	if err != nil {
		return nil, err
	}
	return &Store{db: d, root: r, sealer: sealer}, nil
}

// KeepsSecrets reports whether the store has a key to keep SSH keys with.
func (s *Store) KeepsSecrets() bool { return s.sealer != nil }

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
	paths := s.UserPaths(own)
	if x, ok := strings.CutPrefix(path, Exclude); ok {
		// Left out of a shared directory, and not already.
		if by, ok := paths.Covering(x); !ok || !strings.HasSuffix(by, "/") {
			return fmt.Errorf("%w: %s is not in a shared directory", ErrInvalid, x)
		}
		if !paths.Synced(strings.TrimSuffix(x, "/")) {
			return fmt.Errorf("%w: %s is already left out", ErrInvalid, x)
		}
	} else if by, ok := paths.Covering(path); ok {
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
	var still Paths
	err := s.db.Transact(ctx, func(tx db.Tx) error {
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
		still = s.UserPaths(own)
		return db.Notify(ctx, tx, Channel, userID)
	})
	if err == nil {
		s.dropUnshared(userID, still, false)
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
