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

// GrantLifetime is how long an app has to be approved and to collect its
// token.
const GrantLifetime = 10 * time.Minute

var (
	// ErrPending is a grant not yet approved or refused.
	ErrPending = errors.New("waiting for the sign-in to be approved")
	// ErrRefused is a grant the person refused.
	ErrRefused = errors.New("the sign-in was refused")
)

// Grant is an app's request to sign in through the browser, as the person
// asked to approve it sees it.
type Grant struct {
	// Code is what the app shows, for the person to compare: XXXX-XXXX.
	Code      string
	Name      string
	CreatedAt time.Time
	ExpiresAt time.Time
}

// The letters a code is made of: no vowels, so no words, and nothing read
// as another (no 0, O, 1, I).
const codeAlphabet = "BCDFGHJKLMNPQRSTVWXZ23456789"

// StartSignIn makes a grant for an app that calls itself name, returning
// the device code the app collects its token with -- kept only as a hash
// -- and the grant.
func (m *Manager) StartSignIn(ctx context.Context, name string) (string, Grant, error) {
	name = strings.TrimSpace(name)
	if name == "" || len(name) > 100 {
		return "", Grant{}, fmt.Errorf("%w: an app signing in needs a name of up to 100 characters", ErrInvalid)
	}
	raw := make([]byte, 32)
	if _, err := rand.Read(raw); err != nil {
		return "", Grant{}, err
	}
	device := base64.RawURLEncoding.EncodeToString(raw)
	sum := sha256.Sum256([]byte(device))
	g := Grant{Name: name}
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := tx.Exec(ctx, `DELETE FROM sign_in_grants WHERE expires_at < now()`); err != nil {
			return err
		}
		// A code already in use, unlikely as it is, is drawn again.
		for range 5 {
			code, err := newCode()
			if err != nil {
				return err
			}
			tag, err := tx.Exec(ctx, `INSERT INTO sign_in_grants (device_hash, user_code, name, expires_at)
				VALUES ($1, $2, $3, now() + make_interval(secs => $4)) ON CONFLICT (user_code) DO NOTHING`,
				sum[:], code, name, GrantLifetime.Seconds())
			if err != nil {
				return err
			}
			if tag.RowsAffected() == 1 {
				return tx.QueryRow(ctx, `SELECT user_code, created_at, expires_at FROM sign_in_grants WHERE device_hash = $1`,
					sum[:]).Scan(&g.Code, &g.CreatedAt, &g.ExpiresAt)
			}
		}
		return errors.New("no free sign-in code")
	})
	return device, g, err
}

func newCode() (string, error) {
	b := make([]byte, 8)
	if _, err := rand.Read(b); err != nil {
		return "", err
	}
	out := make([]byte, 0, 9)
	for i, c := range b {
		if i == 4 {
			out = append(out, '-')
		}
		// 256 is not a multiple of the alphabet's length; the bias it leaves
		// (under 4%) costs a fraction of a bit of a code that lives ten
		// minutes and needs a signed-in person to use.
		out = append(out, codeAlphabet[int(c)%len(codeAlphabet)])
	}
	return string(out), nil
}

// NormaliseCode is a code as a person may type it -- lower case, without
// the hyphen, with spaces -- in the form codes are kept in.
func NormaliseCode(s string) string {
	var b strings.Builder
	for _, r := range strings.ToUpper(s) {
		if strings.ContainsRune(codeAlphabet, r) {
			b.WriteRune(r)
		}
	}
	c := b.String()
	if len(c) != 8 {
		return c
	}
	return c[:4] + "-" + c[4:]
}

// SignInGrant is the grant waiting under code: one not yet approved,
// refused or expired.
func (m *Manager) SignInGrant(ctx context.Context, code string) (Grant, error) {
	var g Grant
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		err := tx.QueryRow(ctx, `SELECT user_code, name, created_at, expires_at FROM sign_in_grants
			WHERE user_code = $1 AND expires_at > now() AND approved_by IS NULL AND NOT denied`, NormaliseCode(code)).
			Scan(&g.Code, &g.Name, &g.CreatedAt, &g.ExpiresAt)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		return err
	})
	return g, err
}

// ApproveSignIn lets the app waiting under code sign in as userID.
func (m *Manager) ApproveSignIn(ctx context.Context, code, userID string) error {
	return m.decide(ctx, code, userID, false)
}

// RefuseSignIn turns away the app waiting under code.
func (m *Manager) RefuseSignIn(ctx context.Context, code, userID string) error {
	return m.decide(ctx, code, userID, true)
}

func (m *Manager) decide(ctx context.Context, code, userID string, refuse bool) error {
	return m.db.Transact(ctx, func(tx db.Tx) error {
		var id, name string
		err := tx.QueryRow(ctx, `UPDATE sign_in_grants SET approved_by = CASE WHEN $3 THEN NULL ELSE $2::uuid END, denied = $3
			WHERE user_code = $1 AND expires_at > now() AND approved_by IS NULL AND NOT denied RETURNING id, name`,
			NormaliseCode(code), userID, refuse).Scan(&id, &name)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		action := "sign_in.approve"
		if refuse {
			action = "sign_in.refuse"
		}
		return audit.Record(ctx, tx, audit.Event{Action: action,
			Target:  audit.Ref{Type: "sign_in", ID: id, Name: name},
			Related: []audit.Ref{{Type: audit.KindUser, ID: userID}, {Type: audit.KindOwner, ID: userID}}})
	})
}

// FinishSignIn is the app collecting its token with its device code: an
// access token named as the app, for the person who approved it, once.
// ErrPending says to ask again shortly; ErrRefused and ErrNotFound (an
// unknown, expired or collected grant) say to stop.
func (m *Manager) FinishSignIn(ctx context.Context, device string) (string, Token, error) {
	sum := sha256.Sum256([]byte(device))
	var token string
	var t Token
	refused := false
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var id, name string
		var approvedBy *string
		var denied bool
		err := tx.QueryRow(ctx, `SELECT id, name, approved_by, denied FROM sign_in_grants
			WHERE device_hash = $1 AND expires_at > now() FOR UPDATE`, sum[:]).Scan(&id, &name, &approvedBy, &denied)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		if !denied && approvedBy == nil {
			return ErrPending
		}
		if _, err := tx.Exec(ctx, `DELETE FROM sign_in_grants WHERE id = $1`, id); err != nil {
			return err
		}
		if denied {
			refused = true
			return nil
		}
		token, t, err = createToken(ctx, tx, *approvedBy, name, 0)
		return err
	})
	if err == nil && refused {
		err = ErrRefused
	}
	return token, t, err
}
