// Package environments manages environments as their owners see them: what
// exists, and what each should be doing.
//
// Where an environment runs is placement's business, and what it is actually
// doing is its worker's report. This package changes only the desired state,
// and tells whichever of the two needs to act on the change.
package environments

import (
	"context"
	"errors"
	"fmt"
	"regexp"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/placement"
	"github.com/csnewman/hangar/internal/workers"
)

var (
	ErrNotFound = errors.New("environment not found")
	ErrConflict = errors.New("conflict")
	ErrInvalid  = errors.New("invalid")
)

// A name becomes a hostname and part of URLs, so it is held to a DNS label.
var validName = regexp.MustCompile(`^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$`)

const (
	MaxCPUs      = 64
	MinMemoryMiB = 512
	MaxMemoryMiB = 256 * 1024
)

type Manager struct {
	db *db.DB
}

func NewManager(d *db.DB) *Manager { return &Manager{db: d} }

const columns = `e.id, e.name, e.image, e.cpus, e.memory_mib, e.desired, e.phase, e.reason,
	coalesce(e.worker_id::text, ''), coalesce(w.name, ''), e.created_at, e.updated_at`

const from = `environments e LEFT JOIN workers w ON w.id = e.worker_id`

func scan(row pgx.Row) (api.Environment, error) {
	var e api.Environment
	err := row.Scan(&e.ID, &e.Name, &e.Image, &e.CPUs, &e.MemoryMiB, &e.Desired, &e.Phase,
		&e.Reason, &e.WorkerID, &e.Worker, &e.CreatedAt, &e.UpdatedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return e, ErrNotFound
	}
	return e, err
}

func (m *Manager) List(ctx context.Context) ([]api.Environment, error) {
	var out []api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT `+columns+` FROM `+from+` ORDER BY e.created_at`)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (api.Environment, error) { return scan(r) })
		return err
	})
	if out == nil {
		out = []api.Environment{}
	}
	return out, err
}

func (m *Manager) Get(ctx context.Context, id string) (api.Environment, error) {
	if !db.ValidUUID(id) {
		return api.Environment{}, ErrNotFound
	}
	var e api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		e, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE e.id = $1`, id))
		return err
	})
	return e, err
}

// Validate checks a request to create an environment.
func Validate(req api.CreateEnvironment) error {
	switch {
	case !validName.MatchString(req.Name):
		return fmt.Errorf("%w: name must be lowercase letters, digits and hyphens, at most 63 characters", ErrInvalid)
	case req.Image == "":
		return fmt.Errorf("%w: image is required", ErrInvalid)
	case req.CPUs < 1 || req.CPUs > MaxCPUs:
		return fmt.Errorf("%w: cpus must be between 1 and %d", ErrInvalid, MaxCPUs)
	case req.MemoryMiB < MinMemoryMiB || req.MemoryMiB > MaxMemoryMiB:
		return fmt.Errorf("%w: memory_mib must be between %d and %d", ErrInvalid, MinMemoryMiB, MaxMemoryMiB)
	}
	return nil
}

// Create records a new environment that wants to run. Placement picks it up
// from there.
func (m *Manager) Create(ctx context.Context, req api.CreateEnvironment) (api.Environment, error) {
	if err := Validate(req); err != nil {
		return api.Environment{}, err
	}
	var e api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var id string
		err := tx.QueryRow(ctx, `INSERT INTO environments (name, image, cpus, memory_mib, desired)
			VALUES ($1, $2, $3, $4, 'running') RETURNING id`,
			req.Name, req.Image, req.CPUs, req.MemoryMiB).Scan(&id)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: an environment named %s already exists", ErrConflict, req.Name)
		}
		if err != nil {
			return err
		}
		if err := db.Notify(ctx, tx, placement.Channel, ""); err != nil {
			return err
		}
		e, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE e.id = $1`, id))
		return err
	})
	return e, err
}

// SetDesired changes what an environment should be doing, and reports
// whether the environment still exists afterwards. An environment already
// being deleted cannot be brought back.
//
// An environment no worker holds has nothing to wait for, so deleting one
// removes it at once and stopping one reports it stopped.
func (m *Manager) SetDesired(ctx context.Context, id string, desired api.DesiredState) (bool, error) {
	if !db.ValidUUID(id) {
		return false, ErrNotFound
	}
	var exists bool
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		exists = true
		var workerID *string
		var current api.DesiredState
		err := tx.QueryRow(ctx, `SELECT worker_id::text, desired FROM environments WHERE id = $1 FOR UPDATE`, id).
			Scan(&workerID, &current)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		if current == desired {
			return nil
		}
		if current == api.DesiredDeleted {
			return fmt.Errorf("%w: the environment is being deleted", ErrConflict)
		}

		if workerID != nil {
			if _, err := tx.Exec(ctx, `UPDATE environments SET desired = $2, updated_at = now() WHERE id = $1`,
				id, desired); err != nil {
				return err
			}
			return workers.Bump(ctx, tx, *workerID)
		}

		switch desired {
		case api.DesiredDeleted:
			exists = false
			_, err = tx.Exec(ctx, `DELETE FROM environments WHERE id = $1`, id)
			return err
		case api.DesiredStopped:
			_, err = tx.Exec(ctx, `UPDATE environments SET desired = $2, phase = 'stopped', reason = '',
				updated_at = now() WHERE id = $1`, id, desired)
			return err
		default:
			_, err = tx.Exec(ctx, `UPDATE environments SET desired = $2, phase = 'pending', reason = '',
				updated_at = now() WHERE id = $1`, id, desired)
			if err != nil {
				return err
			}
			return db.Notify(ctx, tx, placement.Channel, "")
		}
	})
	return exists, err
}
