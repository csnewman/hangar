package environments

import (
	"context"
	"errors"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/users"
)

// SetPortsPublic says whether anyone may reach an environment's own web
// servers without signing in to Hangar, or only those who may reach the
// environment. It applies to the next request, running or not.
func (m *Manager) SetPortsPublic(ctx context.Context, p users.Principal, id string, public bool) (api.Environment, error) {
	var e api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := reach(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `UPDATE environments SET ports_public = $2, updated_at = now() WHERE id = $1`,
			id, public); err != nil {
			return err
		}
		if e, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE e.id = $1`, id)); err != nil {
			return err
		}
		if cur.PortsPublic == public {
			return nil
		}
		return audit.Record(ctx, tx, audit.Event{Action: "environment.ports_public", Target: Ref(id, cur.Name),
			Related: []audit.Ref{{Type: audit.KindOwner, ID: cur.OwnerID}},
			Details: map[string]any{"public": public}})
	})
	if errors.Is(err, pgx.ErrNoRows) {
		err = ErrNotFound
	}
	return e, err
}
