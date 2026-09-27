package environments

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/users"
	"github.com/csnewman/hangar/internal/workers"
)

// An environment is pinned to the copy of its image it was made over, and
// its worker says when it holds a newer one. Upgrading re-pins it at its
// next start, keeping its disks, and its worker keeps a copy of the
// writable disk from before to roll back to until the upgrade is kept.

// reach reads and locks an environment p may reach.
func reach(ctx context.Context, tx db.Tx, p users.Principal, id string) (api.Environment, error) {
	if !db.ValidUUID(id) {
		return api.Environment{}, ErrNotFound
	}
	if _, err := tx.Exec(ctx, `SELECT 1 FROM environments e WHERE `+visible+` AND e.id = $3 FOR UPDATE`,
		p.Admin, p.UserID, id); err != nil {
		return api.Environment{}, err
	}
	return scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE `+visible+` AND e.id = $3`,
		p.Admin, p.UserID, id))
}

// changeImage sets the copy an environment's next start pins it to, and
// whether its rollback is dropped, and tells its worker.
func (m *Manager) changeImage(ctx context.Context, p users.Principal, id string, stopped bool,
	decide func(cur api.Environment) (want *string, drop bool, details map[string]any, action string, err error),
) (api.Environment, error) {
	var e api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var cur api.Environment
		var err error
		if stopped {
			cur, err = changing(ctx, tx, p, id)
		} else {
			cur, err = reach(ctx, tx, p, id)
		}
		if err != nil {
			return err
		}
		want, drop, details, action, err := decide(cur)
		if err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `UPDATE environments SET image_pin_want = $2, image_drop_rollback = $3,
			updated_at = now() WHERE id = $1`, id, want, drop); err != nil {
			return err
		}
		if cur.WorkerID != "" {
			if err := workers.Bump(ctx, tx, cur.WorkerID); err != nil {
				return err
			}
		}
		if e, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE e.id = $1`, id)); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: action, Target: Ref(id, cur.Name),
			Related: []audit.Ref{{Type: audit.KindOwner, ID: cur.OwnerID}, {Type: audit.KindImage, ID: cur.Image}},
			Details: details})
	})
	if errors.Is(err, pgx.ErrNoRows) {
		err = ErrNotFound
	}
	return e, err
}

// UpgradeImage re-pins a stopped environment to the newer copy of its image
// its worker holds, at its next start. Its writable layer is kept over the
// newer copy, so a file it has changed that the newer copy also changes
// keeps its version; unless force, that refuses the upgrade, as does a
// comparison not yet made or that failed.
func (m *Manager) UpgradeImage(ctx context.Context, p users.Principal, id string, force bool) (api.Environment, error) {
	return m.changeImage(ctx, p, id, true, func(cur api.Environment) (*string, bool, map[string]any, string, error) {
		u := cur.ImageUpdate
		if u == nil {
			return nil, false, nil, "", fmt.Errorf("%w: its worker holds no newer copy of its image", ErrConflict)
		}
		if !force {
			if err := upgradeRisk(u); err != nil {
				return nil, false, nil, "", err
			}
		}
		digest := u.Digest
		return &digest, false, map[string]any{"from": cur.ImageDigest, "to": digest, "forced": force,
			"conflicts": len(u.Conflicts) + u.MoreConflicts, "packages": u.Packages}, "environment.image_upgrade", nil
	})
}

// upgradeRisk explains why upgrading is not known to be safe, or is nil.
func upgradeRisk(u *api.ImageUpdate) error {
	switch {
	case !u.Checked:
		return fmt.Errorf("%w: its writable disk is still being compared with the newer image; try again in a moment", ErrConflict)
	case u.Error != "":
		return fmt.Errorf("%w: its writable disk could not be compared with the newer image: %s", ErrConflict, u.Error)
	case u.Packages:
		return fmt.Errorf("%w: packages have been installed in it, and its package database would hide the newer image's", ErrConflict)
	case len(u.Conflicts) > 0:
		n := len(u.Conflicts) + u.MoreConflicts
		shown := strings.Join(u.Conflicts[:min(len(u.Conflicts), 3)], ", ")
		if n > 3 {
			shown += ", ..."
		}
		return fmt.Errorf("%w: %d file(s) it has changed would hide the newer image's: %s", ErrConflict, n, shown)
	}
	return nil
}

// RollbackImage puts a stopped environment back as it was before its last
// upgrade at its next start: the copy of the image it had and its writable
// disk then, losing what has been written to that disk since.
func (m *Manager) RollbackImage(ctx context.Context, p users.Principal, id string) (api.Environment, error) {
	return m.changeImage(ctx, p, id, true, func(cur api.Environment) (*string, bool, map[string]any, string, error) {
		rb := cur.ImageRollback
		if rb == nil {
			return nil, false, nil, "", fmt.Errorf("%w: there is no upgrade to roll back", ErrConflict)
		}
		digest := rb.Digest
		return &digest, false, map[string]any{"from": cur.ImageDigest, "to": digest}, "environment.image_rollback", nil
	})
}

// KeepImage keeps an environment's upgrade, discarding the copy of its
// writable disk kept to roll back to, which frees its space and lets its
// worker delete the old copy of the image.
func (m *Manager) KeepImage(ctx context.Context, p users.Principal, id string) (api.Environment, error) {
	return m.changeImage(ctx, p, id, false, func(cur api.Environment) (*string, bool, map[string]any, string, error) {
		if cur.ImageRollback == nil {
			return nil, false, nil, "", fmt.Errorf("%w: there is no upgrade to keep", ErrConflict)
		}
		return nil, true, map[string]any{"digest": cur.ImageDigest}, "environment.image_keep", nil
	})
}

// CancelImageChange withdraws an upgrade or rollback not yet made.
func (m *Manager) CancelImageChange(ctx context.Context, p users.Principal, id string) (api.Environment, error) {
	return m.changeImage(ctx, p, id, false, func(cur api.Environment) (*string, bool, map[string]any, string, error) {
		if cur.ImageChange == "" {
			return nil, false, nil, "", fmt.Errorf("%w: no change to its image is waiting", ErrConflict)
		}
		return nil, false, map[string]any{"change": cur.ImageChange}, "environment.image_change_cancel", nil
	})
}
