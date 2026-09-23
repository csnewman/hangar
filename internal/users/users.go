// Package users manages who can use Hangar: accounts, how they sign in, and
// the sessions that carry a signed-in user from one request to the next.
//
// Signing in is the only part that depends on how a user proves who they
// are. A local password is one way; an external identity provider will be
// another, and both end in the same place, a session for a user row. What a
// user may do is decided from that row alone -- Principal -- so nothing past
// sign-in knows or cares which way they came in.
package users

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"fmt"
	"regexp"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/db"
)

var (
	ErrNotFound = errors.New("user not found")
	ErrConflict = errors.New("conflict")
	ErrInvalid  = errors.New("invalid")
	// ErrBadCredentials covers every way a sign-in can fail -- no such user,
	// wrong password, disabled -- so the answer reveals nothing about which.
	ErrBadCredentials = errors.New("incorrect username or password")
	// ErrNoSession means a session token is missing, unknown or expired.
	ErrNoSession = errors.New("not signed in")
)

// SessionLifetime is how long a session lasts without being used. Each use
// extends it, so an active user stays signed in.
const SessionLifetime = 14 * 24 * time.Hour

// sessionRefresh is how stale a session's expiry may be before a request
// extends it. Extending on every request would be a write per request.
const sessionRefresh = time.Hour

// A username is what a person types to sign in, and may be a local part of
// an email address.
var validUsername = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9._@-]{0,62}$`)

// Principal is the signed-in user a request acts as. Every permission check
// in Hangar is made against one.
type Principal struct {
	UserID string
	Admin  bool
}

// User is one account.
type User struct {
	ID          string
	Username    string
	DisplayName string
	Admin       bool
	Disabled    bool
	HasPassword bool
	CreatedAt   time.Time
	// Environments is how many environments the user owns.
	Environments int
}

type Manager struct {
	db *db.DB
}

func NewManager(d *db.DB) *Manager { return &Manager{db: d} }

const columns = `u.id, u.username, u.display_name, u.is_admin, u.disabled_at IS NOT NULL,
	u.password_hash IS NOT NULL, u.created_at,
	(SELECT count(*) FROM environments e WHERE e.owner_id = u.id)`

func scan(row pgx.Row) (User, error) {
	var u User
	err := row.Scan(&u.ID, &u.Username, &u.DisplayName, &u.Admin, &u.Disabled, &u.HasPassword, &u.CreatedAt,
		&u.Environments)
	if errors.Is(err, pgx.ErrNoRows) {
		return u, ErrNotFound
	}
	return u, err
}

// NewUser is an account to create.
type NewUser struct {
	Username    string
	DisplayName string
	Password    string
	Admin       bool
}

func validatePassword(p string) error {
	if len(p) < MinPasswordLength {
		return fmt.Errorf("%w: a password must be at least %d characters", ErrInvalid, MinPasswordLength)
	}
	return nil
}

// Create adds a user with a local password.
func (m *Manager) Create(ctx context.Context, nu NewUser) (User, error) {
	if !validUsername.MatchString(nu.Username) {
		return User{}, fmt.Errorf("%w: a username is letters, digits and . _ @ -, at most 63 characters", ErrInvalid)
	}
	if err := validatePassword(nu.Password); err != nil {
		return User{}, err
	}
	hash, err := hashPassword(nu.Password)
	if err != nil {
		return User{}, err
	}
	var u User
	err = m.db.Transact(ctx, func(tx db.Tx) error {
		var id string
		err := tx.QueryRow(ctx, `INSERT INTO users (username, display_name, password_hash, is_admin)
			VALUES ($1, $2, $3, $4) RETURNING id`,
			nu.Username, strings.TrimSpace(nu.DisplayName), hash, nu.Admin).Scan(&id)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: the username %s is taken", ErrConflict, nu.Username)
		}
		if err != nil {
			return err
		}
		u, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM users u WHERE u.id = $1`, id))
		return err
	})
	return u, err
}

// EnsureAdmin creates an administrator if there are no users at all, and
// reports whether it did. It is how a new installation gets its first user;
// once any user exists it does nothing, so leaving it configured is harmless.
func (m *Manager) EnsureAdmin(ctx context.Context, username, password string) (bool, error) {
	var n int
	if err := m.db.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `SELECT count(*) FROM users`).Scan(&n)
	}); err != nil {
		return false, err
	}
	if n > 0 {
		return false, nil
	}
	_, err := m.Create(ctx, NewUser{Username: username, Password: password, Admin: true})
	if errors.Is(err, ErrConflict) {
		// Another replica created it first.
		return false, nil
	}
	return err == nil, err
}

func (m *Manager) List(ctx context.Context) ([]User, error) {
	var out []User
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT `+columns+` FROM users u ORDER BY lower(u.username)`)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (User, error) { return scan(r) })
		return err
	})
	if out == nil {
		out = []User{}
	}
	return out, err
}

// Directory returns every enabled user, by username: who there is to name as
// a collaborator.
func (m *Manager) Directory(ctx context.Context) ([]User, error) {
	var out []User
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT `+columns+` FROM users u WHERE u.disabled_at IS NULL
			ORDER BY lower(u.username)`)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (User, error) { return scan(r) })
		return err
	})
	if out == nil {
		out = []User{}
	}
	return out, err
}

func (m *Manager) Get(ctx context.Context, id string) (User, error) {
	if !db.ValidUUID(id) {
		return User{}, ErrNotFound
	}
	var u User
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		u, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM users u WHERE u.id = $1`, id))
		return err
	})
	return u, err
}

// Update is a change to a user. A nil field is left as it is.
type Update struct {
	DisplayName *string
	Admin       *bool
	Disabled    *bool
	// Password replaces the user's password, and signs them out everywhere.
	Password *string
}

// Update changes a user. It refuses any change that would leave no enabled
// administrator, since nobody could then manage the installation.
func (m *Manager) Update(ctx context.Context, id string, up Update) (User, error) {
	if !db.ValidUUID(id) {
		return User{}, ErrNotFound
	}
	var hash *string
	if up.Password != nil {
		if err := validatePassword(*up.Password); err != nil {
			return User{}, err
		}
		h, err := hashPassword(*up.Password)
		if err != nil {
			return User{}, err
		}
		hash = &h
	}
	var u User
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		// Every enabled administrator is locked, so two requests demoting
		// the last two cannot each see the other still standing.
		if _, err := tx.Exec(ctx, `SELECT 1 FROM users WHERE is_admin AND disabled_at IS NULL
			ORDER BY id FOR UPDATE`); err != nil {
			return err
		}
		tag, err := tx.Exec(ctx, `UPDATE users SET
				display_name = coalesce($2, display_name),
				is_admin = coalesce($3, is_admin),
				disabled_at = CASE WHEN $4::boolean IS NULL THEN disabled_at
				                   WHEN $4 THEN coalesce(disabled_at, now())
				                   ELSE NULL END,
				password_hash = coalesce($5, password_hash)
			WHERE id = $1`,
			id, trimmed(up.DisplayName), up.Admin, up.Disabled, hash)
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return ErrNotFound
		}
		if err := requireAdmin(ctx, tx); err != nil {
			return err
		}
		if hash != nil || (up.Disabled != nil && *up.Disabled) {
			if _, err := tx.Exec(ctx, `DELETE FROM sessions WHERE user_id = $1`, id); err != nil {
				return err
			}
		}
		u, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM users u WHERE u.id = $1`, id))
		return err
	})
	return u, err
}

// requireAdmin fails the transaction if it has left no enabled administrator.
func requireAdmin(ctx context.Context, tx db.Tx) error {
	var admins int
	if err := tx.QueryRow(ctx, `SELECT count(*) FROM users WHERE is_admin AND disabled_at IS NULL`).
		Scan(&admins); err != nil {
		return err
	}
	if admins == 0 {
		return fmt.Errorf("%w: that would leave no administrator", ErrConflict)
	}
	return nil
}

// Delete removes a user who owns no environments.
func (m *Manager) Delete(ctx context.Context, id string) error {
	if !db.ValidUUID(id) {
		return ErrNotFound
	}
	return m.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := tx.Exec(ctx, `SELECT 1 FROM users WHERE is_admin AND disabled_at IS NULL
			ORDER BY id FOR UPDATE`); err != nil {
			return err
		}
		var owned int
		err := tx.QueryRow(ctx, `SELECT count(*) FROM environments WHERE owner_id = $1`, id).Scan(&owned)
		if err != nil {
			return err
		}
		if owned > 0 {
			return fmt.Errorf("%w: the user owns %d environments; delete them or disable the user instead",
				ErrConflict, owned)
		}
		tag, err := tx.Exec(ctx, `DELETE FROM users WHERE id = $1`, id)
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return ErrNotFound
		}
		return requireAdmin(ctx, tx)
	})
}

// Login checks a username and password and opens a session, returning the
// token that identifies it. The token is shown once; only its hash is kept.
func (m *Manager) Login(ctx context.Context, username, password string) (string, User, error) {
	var id string
	var hash *string
	var disabled bool
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `SELECT id, password_hash, disabled_at IS NOT NULL FROM users
			WHERE lower(username) = lower($1)`, username).Scan(&id, &hash, &disabled)
	})
	if err != nil && !errors.Is(err, pgx.ErrNoRows) {
		return "", User{}, err
	}

	check := dummyHash
	if hash != nil {
		check = *hash
	}
	ok, err := checkPassword(check, password)
	if err != nil {
		return "", User{}, err
	}
	if !ok || hash == nil || disabled {
		return "", User{}, ErrBadCredentials
	}

	token, err := m.openSession(ctx, id)
	if err != nil {
		return "", User{}, err
	}
	u, err := m.Get(ctx, id)
	return token, u, err
}

// SignInAs opens a session for an enabled user without asking for any
// credential. It is for development only, where the server is configured to
// sign every visitor in as one user.
func (m *Manager) SignInAs(ctx context.Context, username string) (string, Principal, error) {
	var p Principal
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `SELECT id, is_admin FROM users
			WHERE lower(username) = lower($1) AND disabled_at IS NULL`, username).Scan(&p.UserID, &p.Admin)
	})
	if errors.Is(err, pgx.ErrNoRows) {
		return "", Principal{}, ErrNotFound
	}
	if err != nil {
		return "", Principal{}, err
	}
	token, err := m.openSession(ctx, p.UserID)
	return token, p, err
}

func (m *Manager) openSession(ctx context.Context, userID string) (string, error) {
	raw := make([]byte, 32)
	if _, err := rand.Read(raw); err != nil {
		return "", err
	}
	token := base64.RawURLEncoding.EncodeToString(raw)
	sum := sha256.Sum256([]byte(token))
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, $3)`,
			sum[:], userID, time.Now().Add(SessionLifetime))
		return err
	})
	return token, err
}

// Authenticate resolves a session token to the user it belongs to. A
// session is refused once it has expired or its user has been disabled.
func (m *Manager) Authenticate(ctx context.Context, token string) (Principal, error) {
	if token == "" {
		return Principal{}, ErrNoSession
	}
	sum := sha256.Sum256([]byte(token))
	var p Principal
	var lastSeen time.Time
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		err := tx.QueryRow(ctx, `SELECT u.id, u.is_admin, s.last_seen_at
			FROM sessions s JOIN users u ON u.id = s.user_id
			WHERE s.token_hash = $1 AND s.expires_at > now() AND u.disabled_at IS NULL`, sum[:]).
			Scan(&p.UserID, &p.Admin, &lastSeen)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNoSession
		}
		if err != nil {
			return err
		}
		if time.Since(lastSeen) > sessionRefresh {
			_, err = tx.Exec(ctx, `UPDATE sessions SET last_seen_at = now(), expires_at = $2 WHERE token_hash = $1`,
				sum[:], time.Now().Add(SessionLifetime))
		}
		return err
	})
	return p, err
}

// Logout ends the session a token identifies.
func (m *Manager) Logout(ctx context.Context, token string) error {
	sum := sha256.Sum256([]byte(token))
	return m.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `DELETE FROM sessions WHERE token_hash = $1`, sum[:])
		return err
	})
}

// ChangePassword replaces a user's own password, given the current one. It
// ends every other session the user has, and keeps the one making the
// change.
func (m *Manager) ChangePassword(ctx context.Context, userID, currentToken, current, next string) error {
	if err := validatePassword(next); err != nil {
		return err
	}
	var hash *string
	if err := m.db.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `SELECT password_hash FROM users WHERE id = $1`, userID).Scan(&hash)
	}); err != nil {
		return err
	}
	if hash == nil {
		return fmt.Errorf("%w: this account has no password to change", ErrInvalid)
	}
	ok, err := checkPassword(*hash, current)
	if err != nil {
		return err
	}
	if !ok {
		return fmt.Errorf("%w: the current password is incorrect", ErrInvalid)
	}
	newHash, err := hashPassword(next)
	if err != nil {
		return err
	}
	keep := sha256.Sum256([]byte(currentToken))
	return m.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := tx.Exec(ctx, `UPDATE users SET password_hash = $2 WHERE id = $1`, userID, newHash); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `DELETE FROM sessions WHERE user_id = $1 AND token_hash <> $2`, userID, keep[:])
		return err
	})
}

// PruneSessions deletes expired sessions and returns how many it removed.
func (m *Manager) PruneSessions(ctx context.Context) (int64, error) {
	var n int64
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		tag, err := tx.Exec(ctx, `DELETE FROM sessions WHERE expires_at <= now()`)
		n = tag.RowsAffected()
		return err
	})
	return n, err
}

func trimmed(s *string) *string {
	if s == nil {
		return nil
	}
	t := strings.TrimSpace(*s)
	return &t
}
