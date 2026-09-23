// Package editor serves each environment's editor -- VS Code's server,
// running in the guest -- to the browser, on an origin of the environment's
// own.
//
// The editor runs code the environment controls: extensions, and a
// workbench that renders the repository. Served from Hangar's origin it
// could call Hangar's API as the signed-in user. On its own origin, a sibling
// of Hangar's (e-<id>.<hangar host>), it cannot read Hangar's cookie or its
// responses, and Hangar's API refuses its state-changing requests as
// cross-origin.
//
// That origin cannot see the Hangar session, so it has sessions of its own:
// Hangar hands the browser a one-time ticket, the editor origin redeems it
// for a cookie of its own, and that cookie lasts only as long as the Hangar
// session it came from.
package editor

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
)

// ErrNoSession means an editor ticket or cookie is missing, unknown, expired,
// or for another environment.
var ErrNoSession = errors.New("not signed in to this editor")

const (
	// ticketLifetime is how long the browser has to redeem a ticket. It is
	// redeemed by the iframe Hangar opens straight after asking for it.
	ticketLifetime = time.Minute
	// cookieLifetime is how long an editor cookie lasts unused. Each use
	// extends it, and it never outlives the Hangar session behind it.
	cookieLifetime = 24 * time.Hour
	// cookieRefresh is how stale an expiry may be before a request extends
	// it, so that not every request is a write.
	cookieRefresh = time.Hour
)

// Target is the running environment an editor request goes to.
type Target struct {
	WorkerID string
	Running  bool
}

type Manager struct {
	db *db.DB
}

func NewManager(d *db.DB) *Manager { return &Manager{db: d} }

func newToken() (string, []byte, error) {
	raw := make([]byte, 32)
	if _, err := rand.Read(raw); err != nil {
		return "", nil, err
	}
	token := base64.RawURLEncoding.EncodeToString(raw)
	sum := sha256.Sum256([]byte(token))
	return token, sum[:], nil
}

// Ticket issues a ticket to one environment's editor for the Hangar session
// whose token is given. Whether the session's user may reach the environment
// is the caller's to have checked.
func (m *Manager) Ticket(ctx context.Context, sessionToken, environmentID string) (string, error) {
	ticket, hash, err := newToken()
	if err != nil {
		return "", err
	}
	session := sha256.Sum256([]byte(sessionToken))
	err = m.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `INSERT INTO editor_sessions (token_hash, session_hash, environment_id, ticket, expires_at)
			VALUES ($1, $2, $3, true, $4)`, hash, session[:], environmentID, time.Now().Add(ticketLifetime))
		return err
	})
	return ticket, err
}

// Redeem exchanges a ticket for the editor origin's cookie. A ticket works
// once. The cookie replaces any earlier one the same Hangar session had for
// the environment.
func (m *Manager) Redeem(ctx context.Context, ticket, environmentID string) (string, error) {
	if ticket == "" || !db.ValidUUID(environmentID) {
		return "", ErrNoSession
	}
	cookie, hash, err := newToken()
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256([]byte(ticket))
	err = m.db.Transact(ctx, func(tx db.Tx) error {
		var session []byte
		err := tx.QueryRow(ctx, `DELETE FROM editor_sessions
			WHERE token_hash = $1 AND ticket AND environment_id = $2 AND expires_at > now()
			RETURNING session_hash`, sum[:], environmentID).Scan(&session)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNoSession
		}
		if err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `DELETE FROM editor_sessions
			WHERE session_hash = $1 AND environment_id = $2 AND NOT ticket`, session, environmentID); err != nil {
			return err
		}
		_, err = tx.Exec(ctx, `INSERT INTO editor_sessions (token_hash, session_hash, environment_id, ticket, expires_at)
			VALUES ($1, $2, $3, false, $4)`, hash, session, environmentID, time.Now().Add(cookieLifetime))
		return err
	})
	if err != nil {
		return "", err
	}
	return cookie, nil
}

// Authenticate resolves an editor cookie to the environment it may reach.
// It is refused once it or its Hangar session has expired, or the user has
// been disabled.
func (m *Manager) Authenticate(ctx context.Context, cookie, environmentID string) (Target, error) {
	if cookie == "" || !db.ValidUUID(environmentID) {
		return Target{}, ErrNoSession
	}
	sum := sha256.Sum256([]byte(cookie))
	var t Target
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var worker *string
		var phase string
		var expires time.Time
		err := tx.QueryRow(ctx, `SELECT e.worker_id, e.phase, es.expires_at
			FROM editor_sessions es
			JOIN sessions s ON s.token_hash = es.session_hash
			JOIN users u ON u.id = s.user_id
			JOIN environments e ON e.id = es.environment_id
			WHERE es.token_hash = $1 AND NOT es.ticket AND es.environment_id = $2
				AND es.expires_at > now() AND s.expires_at > now() AND u.disabled_at IS NULL`,
			sum[:], environmentID).Scan(&worker, &phase, &expires)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNoSession
		}
		if err != nil {
			return err
		}
		if worker != nil {
			t.WorkerID = *worker
		}
		t.Running = phase == string(api.PhaseRunning) && t.WorkerID != ""
		if time.Until(expires) < cookieLifetime-cookieRefresh {
			_, err = tx.Exec(ctx, `UPDATE editor_sessions SET expires_at = $2 WHERE token_hash = $1`,
				sum[:], time.Now().Add(cookieLifetime))
		}
		return err
	})
	return t, err
}

// Prune deletes expired tickets and cookies and returns how many it removed.
func (m *Manager) Prune(ctx context.Context) (int64, error) {
	var n int64
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		tag, err := tx.Exec(ctx, `DELETE FROM editor_sessions WHERE expires_at <= now()`)
		n = tag.RowsAffected()
		return err
	})
	return n, err
}
