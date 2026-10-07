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
	"strconv"
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

// columns are an environment's, its worker's being online among them: seen
// within workers.OnlineWindow and not revoked.
var columns = `e.id, e.short_id, e.owner_id, u.username, e.name, coalesce(e.template_id::text, ''), e.template_name,
	e.spec, e.image, e.image_digest, e.cpus, e.memory_mib, e.desired, e.phase, e.reason, coalesce(e.worker_id::text, ''),
	coalesce(w.name, ''), e.created_at, e.updated_at, e.stats, e.progress, w.gpu,
	tm.spec, coalesce(tm.revision, 0), e.template_revision, e.image_pin_want, e.image_update, e.image_rollback, e.ports_public,
	coalesce(w.last_seen_at > now() - interval '` + strconv.Itoa(int(workers.OnlineWindow.Seconds())) + ` seconds'
		AND w.revoked_at IS NULL, false)`

const from = `environments e
	JOIN users u ON u.id = e.owner_id
	LEFT JOIN workers w ON w.id = e.worker_id
	LEFT JOIN templates tm ON tm.id = e.template_id`

// visible is the condition that limits a query to what a principal may
// reach. It takes the principal as $1 (admin) and $2 (user ID).
const visible = `($1 OR e.owner_id = $2)`

func scan(row pgx.Row) (api.Environment, error) {
	var e api.Environment
	var spec, stats, progress, gpu, tspec, update, rollback []byte
	var trev, erev int64
	var pinWant *string
	err := row.Scan(&e.ID, &e.ShortID, &e.OwnerID, &e.Owner, &e.Name, &e.TemplateID, &e.Template, &spec, &e.Image, &e.ImageDigest, &e.CPUs,
		&e.MemoryMiB, &e.Desired, &e.Phase, &e.Reason, &e.WorkerID, &e.Worker, &e.CreatedAt, &e.UpdatedAt, &stats,
		&progress, &gpu, &tspec, &trev, &erev, &pinWant, &update, &rollback, &e.PortsPublic, &e.WorkerOnline)
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
	if progress != nil && e.Phase == api.PhaseStarting {
		e.Progress = &api.Progress{}
		if err := json.Unmarshal(progress, e.Progress); err != nil {
			return e, err
		}
	}
	if err := json.Unmarshal(spec, &e.Spec); err != nil {
		return e, err
	}
	if tspec != nil {
		var t api.TemplateSpec
		if err := json.Unmarshal(tspec, &t); err != nil {
			return e, err
		}
		e.TemplateChanges = templateChanges(e, t)
		e.TemplateUpdated = trev > erev
	}
	if update != nil {
		e.ImageUpdate = &api.ImageUpdate{}
		if err := json.Unmarshal(update, e.ImageUpdate); err != nil {
			return e, err
		}
	}
	if rollback != nil {
		e.ImageRollback = &api.ImageRollback{}
		if err := json.Unmarshal(rollback, e.ImageRollback); err != nil {
			return e, err
		}
	}
	if pinWant != nil && *pinWant != e.ImageDigest {
		e.ImageChange = api.ImageChangeUpgrade
		if e.ImageRollback != nil && e.ImageRollback.Digest == *pinWant {
			e.ImageChange = api.ImageChangeRollback
		}
	}
	// What its worker's virtual GPUs render with, when it has one.
	if gpu != nil && e.Spec.GPU == api.GPUVirtual {
		var g api.GPUInfo
		if err := json.Unmarshal(gpu, &g); err != nil {
			return e, err
		}
		if g.GL != nil {
			e.GPURenderer, e.GPUSoftware = g.GL.Renderer, g.GL.Software
		}
	}
	return e, nil
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
			AND lower(u.username) = lower($3) AND lower(e.name) = lower($4) AND e.desired <> 'deleted'`, p.Admin, p.UserID, owner, name))
		return err
	})
	return e, err
}

// ByShortID returns the environment with this short ID, if p may reach it.
func (m *Manager) ByShortID(ctx context.Context, p users.Principal, short string) (api.Environment, error) {
	var e api.Environment
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		e, err = scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE `+visible+`
			AND e.short_id = $3 AND e.desired <> 'deleted'`, p.Admin, p.UserID, short))
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
		tname, tspec, trev, err := templates.ForUse(ctx, tx, p, req.TemplateID)
		if errors.Is(err, templates.ErrNotFound) {
			return fmt.Errorf("%w: no such template", ErrInvalid)
		}
		if err != nil {
			return err
		}
		var owner string
		if err := tx.QueryRow(ctx, `SELECT username FROM users WHERE id = $1`, p.UserID).Scan(&owner); err != nil {
			return err
		}

		// The short ID is drawn at random before the spec is made, since the
		// spec may use it; one already taken draws again.
		var id string
		var spec api.Spec
		for tries := 0; id == ""; tries++ {
			if tries == 10 {
				return errors.New("no free short ID for the environment after 10 tries")
			}
			var short string
			if err := tx.QueryRow(ctx, `SELECT new_short_id()`).Scan(&short); err != nil {
				return err
			}
			spec, err = templates.Resolve(tspec, templates.Vars{Name: req.Name, Owner: owner, ShortID: short, Template: tname})
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
			err = tx.QueryRow(ctx, `INSERT INTO environments
					(owner_id, name, short_id, template_id, template_name, template_revision, spec, image, cpus, memory_mib, desired)
				VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'running')
				ON CONFLICT (short_id) DO NOTHING RETURNING id`,
				p.UserID, req.Name, short, req.TemplateID, tname, trev, raw, spec.Image, spec.CPUs, spec.MemoryMiB).Scan(&id)
			if db.IsUniqueViolation(err) {
				return fmt.Errorf("%w: you already have an environment named %s", ErrConflict, req.Name)
			}
			if err != nil && !errors.Is(err, pgx.ErrNoRows) {
				return err
			}
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
		var cpus, mem int
		err := tx.QueryRow(ctx, `SELECT e.worker_id::text, e.desired, e.name, e.owner_id::text, e.image, e.cpus, e.memory_mib
			FROM environments e WHERE `+visible+` AND e.id = $3 FOR UPDATE`, p.Admin, p.UserID, id).
			Scan(&workerID, &current, &name, &owner, &image, &cpus, &mem)
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

		// A stopped or suspended environment's disks are on its worker, so it
		// can run only there, and only when that worker has room for it: its
		// CPUs and memory were given back when it stopped.
		if workerID != nil && desired == api.DesiredRunning {
			room, err := workers.RoomFor(ctx, tx, *workerID, id)
			if err != nil {
				return err
			}
			if cpus > room.FreeCPUs || mem > room.FreeMemMiB {
				return fmt.Errorf("%w: its worker, %s, has no room for it: it needs %d vCPUs and %d MiB, and %d vCPUs and %d MiB are free; stop another environment there first",
					ErrConflict, room.Name, cpus, mem, max(room.FreeCPUs, 0), max(room.FreeMemMiB, 0))
			}
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
