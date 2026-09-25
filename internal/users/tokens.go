package users

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
)

// TokenPrefix begins every access token, so one is recognised for what it
// is wherever it turns up.
const TokenPrefix = "hgr_"

// Token is an access token, as its owner sees it after it is made.
type Token struct {
	ID         string
	Name       string
	CreatedAt  time.Time
	LastUsedAt *time.Time
	ExpiresAt  *time.Time
}

// CreateToken makes an access token for a user, returning it -- shown this
// once, and kept only as a hash -- and what the user sees of it after.
func (m *Manager) CreateToken(ctx context.Context, userID, name string, lifetime time.Duration) (string, Token, error) {
	name = strings.TrimSpace(name)
	if name == "" || len(name) > 100 {
		return "", Token{}, fmt.Errorf("%w: a token needs a name of up to 100 characters", ErrInvalid)
	}
	raw := make([]byte, 32)
	if _, err := rand.Read(raw); err != nil {
		return "", Token{}, err
	}
	token := TokenPrefix + base64.RawURLEncoding.EncodeToString(raw)
	sum := sha256.Sum256([]byte(token))
	var expires *time.Time
	if lifetime > 0 {
		e := time.Now().Add(lifetime)
		expires = &e
	}
	t := Token{Name: name, ExpiresAt: expires}
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		if err := tx.QueryRow(ctx, `INSERT INTO access_tokens (user_id, name, token_hash, expires_at)
			VALUES ($1, $2, $3, $4) RETURNING id, created_at`, userID, name, sum[:], expires).Scan(&t.ID, &t.CreatedAt); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "token.create",
			Target:  audit.Ref{Type: "token", ID: t.ID, Name: name},
			Related: []audit.Ref{{Type: audit.KindUser, ID: userID}, {Type: audit.KindOwner, ID: userID}}})
	})
	return token, t, err
}

// Tokens lists a user's access tokens.
func (m *Manager) Tokens(ctx context.Context, userID string) ([]Token, error) {
	var out []Token
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT id, name, created_at, last_used_at, expires_at FROM access_tokens
			WHERE user_id = $1 ORDER BY created_at`, userID)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (Token, error) {
			var t Token
			return t, r.Scan(&t.ID, &t.Name, &t.CreatedAt, &t.LastUsedAt, &t.ExpiresAt)
		})
		return err
	})
	if out == nil {
		out = []Token{}
	}
	return out, err
}

// RevokeToken ends one of a user's access tokens.
func (m *Manager) RevokeToken(ctx context.Context, userID, id string) error {
	if !db.ValidUUID(id) {
		return ErrNotFound
	}
	return m.db.Transact(ctx, func(tx db.Tx) error {
		var name string
		err := tx.QueryRow(ctx, `DELETE FROM access_tokens WHERE id = $1 AND user_id = $2 RETURNING name`, id, userID).Scan(&name)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "token.revoke",
			Target:  audit.Ref{Type: "token", ID: id, Name: name},
			Related: []audit.Ref{{Type: audit.KindUser, ID: userID}, {Type: audit.KindOwner, ID: userID}}})
	})
}

// AuthenticateToken resolves an access token to its user, and the token's
// name. A token is refused once it has expired or its user is disabled.
func (m *Manager) AuthenticateToken(ctx context.Context, token string) (Principal, string, error) {
	if !strings.HasPrefix(token, TokenPrefix) {
		return Principal{}, "", ErrNoSession
	}
	sum := sha256.Sum256([]byte(token))
	var p Principal
	var name string
	var lastUsed *time.Time
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		err := tx.QueryRow(ctx, `SELECT u.id, u.username, u.is_admin, t.name, t.last_used_at
			FROM access_tokens t JOIN users u ON u.id = t.user_id
			WHERE t.token_hash = $1 AND (t.expires_at IS NULL OR t.expires_at > now())
				AND u.disabled_at IS NULL AND u.kind = 'person'`, sum[:]).
			Scan(&p.UserID, &p.Username, &p.Admin, &name, &lastUsed)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNoSession
		}
		if err != nil {
			return err
		}
		if lastUsed == nil || time.Since(*lastUsed) > sessionRefresh {
			_, err = tx.Exec(ctx, `UPDATE access_tokens SET last_used_at = now() WHERE token_hash = $1`, sum[:])
		}
		return err
	})
	return p, name, err
}
