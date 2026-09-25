// Package environments manages environments as their owners see them: what
// exists, and what each should be doing.
//
// Every operation acts as a users.Principal, and the check is part of the
// query rather than a step before it: a user reaches only environments they
// own, an administrator reaches all of them, and an environment the caller
// may not reach is reported as not found, so its existence is not revealed.
//
// Where an environment runs is placement's business, and what it is actually
// doing is its worker's report. This package changes only the desired state,
// and tells whichever of the two needs to act on the change.
package environments

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/placement"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
	"github.com/csnewman/hangar/internal/workers"
)

var (
	ErrNotFound = errors.New("environment not found")
	ErrConflict = errors.New("conflict")
	ErrInvalid  = errors.New("invalid")
)

type Manager struct {
	db *db.DB
}

func NewManager(d *db.DB) *Manager { return &Manager{db: d} }

const columns = `e.id, e.owner_id, u.username, e.name, coalesce(e.template_id::text, ''), e.template_name,
	e.spec, e.image, e.cpus, e.memory_mib, e.desired, e.phase, e.reason, coalesce(e.worker_id::text, ''),
	coalesce(w.name, ''), e.created_at, e.updated_at, e.stats`

const from = `environments e
	JOIN users u ON u.id = e.owner_id
	LEFT JOIN workers w ON w.id = e.worker_id`

// visible is the condition that limits a query to what a principal may
// reach. It takes the principal as $1 (admin) and $2 (user ID).
const visible = `($1 OR e.owner_id = $2)`

func scan(row pgx.Row) (api.Environment, error) {
	var e api.Environment
	var spec, stats []byte
	err := row.Scan(&e.ID, &e.OwnerID, &e.Owner, &e.Name, &e.TemplateID, &e.Template, &spec, &e.Image, &e.CPUs,
		&e.MemoryMiB, &e.Desired, &e.Phase, &e.Reason, &e.WorkerID, &e.Worker, &e.CreatedAt, &e.UpdatedAt, &stats)
	if errors.Is(err, pgx.ErrNoRows) {
		return e, ErrNotFound
	}
	if err != nil {
		return e, err
	}
	// Usage is only meaningful for an environment that is running; a
	// stopped one keeps the last figures it had, which would mislead.
	if stats != nil && e.Phase == api.PhaseRunning {
		e.Stats = &api.EnvironmentStats{}
		if err := json.Unmarshal(stats, e.Stats); err != nil {
			return e, err
		}
	}
	return e, json.Unmarshal(spec, &e.Spec)
}

// List returns every environment p may reach, oldest first.
func (m *Manager) List(ctx context.Context, p users.Principal) ([]api.Environment, error) {
	var out []api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT `+columns+` FROM `+from+` WHERE `+visible+` ORDER BY e.created_at`,
			p.Admin, p.UserID)
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

func (m *Manager) Get(ctx context.Context, p users.Principal, id string) (api.Environment, error) {
	if !db.ValidUUID(id) {
		return api.Environment{}, ErrNotFound
	}
	var e api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		e, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE `+visible+` AND e.id = $3`,
			p.Admin, p.UserID, id))
		return err
	})
	return e, err
}

// ByName finds an environment p may reach by its owner's username and its
// name.
func (m *Manager) ByName(ctx context.Context, p users.Principal, owner, name string) (api.Environment, error) {
	var e api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		e, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE `+visible+`
			AND lower(u.username) = lower($3) AND e.name = $4 AND e.desired <> 'deleted'`, p.Admin, p.UserID, owner, name))
		return err
	})
	return e, err
}

// Create makes an environment, owned by p, from a template p may see, and
// asks for it to run. Placement picks it up from there.
//
// The template's spec is resolved for the new name and copied into the
// environment, in the same transaction that reads it, so the environment is
// exactly what the template said at that moment.
func (m *Manager) Create(ctx context.Context, p users.Principal, req api.CreateEnvironment) (api.Environment, error) {
	var e api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		tname, tspec, err := templates.ForUse(ctx, tx, p, req.TemplateID)
		if errors.Is(err, templates.ErrNotFound) {
			return fmt.Errorf("%w: no such template", ErrInvalid)
		}
		if err != nil {
			return err
		}
		spec, err := templates.Resolve(tspec, req.Name)
		if errors.Is(err, templates.ErrInvalid) {
			return fmt.Errorf("%w%s", ErrInvalid, strings.TrimPrefix(err.Error(), templates.ErrInvalid.Error()))
		}
		if err != nil {
			return err
		}
		raw, err := json.Marshal(spec)
		if err != nil {
			return err
		}

		var id string
		err = tx.QueryRow(ctx, `INSERT INTO environments
				(owner_id, name, template_id, template_name, spec, image, cpus, memory_mib, desired)
			VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'running') RETURNING id`,
			p.UserID, req.Name, req.TemplateID, tname, raw, spec.Image, spec.CPUs, spec.MemoryMiB).Scan(&id)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: you already have an environment named %s", ErrConflict, req.Name)
		}
		if err != nil {
			return err
		}
		if err := db.Notify(ctx, tx, placement.Channel, ""); err != nil {
			return err
		}
		if e, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE e.id = $1`, id)); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "environment.create", Target: Ref(e.ID, e.Name),
			Related: []audit.Ref{{Type: audit.KindOwner, ID: e.OwnerID},
				{Type: audit.KindTemplate, ID: req.TemplateID, Name: tname}, {Type: audit.KindImage, ID: spec.Image}},
			Details: map[string]any{"cpus": spec.CPUs, "memory_mib": spec.MemoryMiB, "untrusted": spec.Untrusted}})
	})
	return e, err
}

// Ref names an environment in the audit log.
func Ref(id, name string) audit.Ref {
	return audit.Ref{Type: audit.KindEnvironment, ID: id, Name: name}
}

// SetDesired changes what an environment should be doing, and reports
// whether the environment still exists afterwards. An environment already
// being deleted cannot be brought back.
//
// An environment no worker holds has nothing to wait for, so deleting one
// removes it at once and stopping one reports it stopped.
func (m *Manager) SetDesired(ctx context.Context, p users.Principal, id string, desired api.DesiredState) (bool, error) {
	if !db.ValidUUID(id) {
		return false, ErrNotFound
	}
	var exists bool
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		exists = true
		var workerID *string
		var current api.DesiredState
		var name, owner, image string
		err := tx.QueryRow(ctx, `SELECT e.worker_id::text, e.desired, e.name, e.owner_id::text, e.image FROM environments e
			WHERE `+visible+` AND e.id = $3 FOR UPDATE`, p.Admin, p.UserID, id).
			Scan(&workerID, &current, &name, &owner, &image)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		if current == desired {
			return nil
		}
		related := []audit.Ref{{Type: audit.KindOwner, ID: owner}, {Type: audit.KindImage, ID: image}}
		if workerID != nil {
			related = append(related, audit.Ref{Type: audit.KindWorker, ID: *workerID})
		}
		record := func() error {
			return audit.Record(ctx, tx, audit.Event{Action: "environment." + verb(desired), Target: Ref(id, name),
				Related: related, Details: map[string]any{"from": current}})
		}
		if current == api.DesiredDeleted {
			return fmt.Errorf("%w: the environment is being deleted", ErrConflict)
		}
		// Only a machine that is running has memory to keep. A stopped
		// environment stays stopped; one no worker holds has nothing to
		// suspend.
		if desired == api.DesiredSuspended && (current != api.DesiredRunning || workerID == nil) {
			return fmt.Errorf("%w: only a running environment can be suspended", ErrConflict)
		}

		if workerID != nil {
			if _, err := tx.Exec(ctx, `UPDATE environments SET desired = $2, updated_at = now() WHERE id = $1`,
				id, desired); err != nil {
				return err
			}
			if err := workers.Bump(ctx, tx, *workerID); err != nil {
				return err
			}
			return record()
		}

		switch desired {
		case api.DesiredDeleted:
			exists = false
			if _, err = tx.Exec(ctx, `DELETE FROM environments WHERE id = $1`, id); err != nil {
				return err
			}
		case api.DesiredStopped:
			if _, err = tx.Exec(ctx, `UPDATE environments SET desired = $2, phase = 'stopped', reason = '',
				updated_at = now() WHERE id = $1`, id, desired); err != nil {
				return err
			}
		default:
			_, err = tx.Exec(ctx, `UPDATE environments SET desired = $2, phase = 'pending', reason = '',
				updated_at = now() WHERE id = $1`, id, desired)
			if err != nil {
				return err
			}
			if err := db.Notify(ctx, tx, placement.Channel, ""); err != nil {
				return err
			}
		}
		return record()
	})
	return exists, err
}

// verb is the audit action for asking an environment to be in a state.
func verb(d api.DesiredState) string {
	switch d {
	case api.DesiredRunning:
		return "start"
	case api.DesiredStopped:
		return "stop"
	case api.DesiredSuspended:
		return "suspend"
	case api.DesiredDeleted:
		return "delete"
	}
	return string(d)
}
