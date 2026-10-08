package profile

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/pem"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"golang.org/x/crypto/ssh"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
)

// Channel carries a user's ID when their keys change.
const Channel = "hangar_profile"

var (
	ErrNotFound = errors.New("not found")
	ErrInvalid  = errors.New("invalid")
)

// Key is one of a user's SSH keys, as anyone may see it.
type Key struct {
	ID        string
	Name      string
	PublicKey string
	// Fingerprint is the key's SHA256 fingerprint, as ssh-keygen -l shows.
	Fingerprint string
	CreatedAt   time.Time
}

// Store keeps users' SSH keys and sign-in keys.
type Store struct {
	db     *db.DB
	sealer *Sealer
}

// NewStore returns a store keeping keys in d. A nil sealer keeps no SSH
// keys: they are refused.
func NewStore(d *db.DB, sealer *Sealer) *Store {
	return &Store{db: d, sealer: sealer}
}

// KeepsSecrets reports whether the store has a key to keep SSH keys with.
func (s *Store) KeepsSecrets() bool { return s.sealer != nil }

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
