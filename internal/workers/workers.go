// Package workers manages the machines that run environments: their
// identity, what they report, and the desired set each is sent.
//
// A worker's desired set is every environment placed on it. Anything that
// changes that set -- placing an environment, changing what one should be
// doing, removing one -- calls Bump in the same transaction, which raises the
// worker's version and wakes whichever replica is holding its request.
//
// A transaction that locks both environment and worker rows locks the
// environments first. Every one follows that order, so none can deadlock
// another.
package workers

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"regexp"
	"slices"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
)

var (
	ErrNotFound     = errors.New("worker not found")
	ErrConflict     = errors.New("conflict")
	ErrUnauthorized = errors.New("unauthorized")
	ErrInvalid      = errors.New("invalid")
)

// Channel carries a worker's ID when its desired set changes.
const Channel = "hangar_worker"

// CapacityChannel fires when a worker's capacity changes, which may make
// room for an environment that did not fit before.
const CapacityChannel = "hangar_capacity"

// OnlineWindow is how recently a worker must have reported to count as
// online. Workers report at a third of this, so one lost report does not
// take a worker offline.
const OnlineWindow = 45 * time.Second

// A name is usually a hostname, and may be a pod or instance name.
var validName = regexp.MustCompile(`^[A-Za-z0-9]([A-Za-z0-9._-]{0,126}[A-Za-z0-9])?$`)

type Manager struct {
	db *db.DB
}

func NewManager(d *db.DB) *Manager { return &Manager{db: d} }

// Bump marks a worker's desired set as changed. It is for the managers that
// change what a worker holds, inside their own transactions.
func Bump(ctx context.Context, tx db.Tx, workerID string) error {
	if _, err := tx.Exec(ctx, `UPDATE workers SET desired_version = desired_version + 1 WHERE id = $1`, workerID); err != nil {
		return err
	}
	return db.Notify(ctx, tx, Channel, workerID)
}

// Register creates a worker and issues its credential, which is returned
// once and stored only as a hash.
//
// A name already in use is refused, even to a valid bootstrap token. The
// bootstrap token is shared, and accepting it for an existing name would let
// any holder take over that worker's environments. A worker keeps its
// credential on disk; one that has lost it must be revoked and removed by an
// operator before its name can register again.
func (m *Manager) Register(ctx context.Context, req api.RegisterWorker) (api.WorkerCredential, error) {
	if !validName.MatchString(req.Name) {
		return api.WorkerCredential{}, fmt.Errorf("%w: worker name %q", ErrInvalid, req.Name)
	}
	secret := make([]byte, 32)
	if _, err := rand.Read(secret); err != nil {
		return api.WorkerCredential{}, err
	}
	sum := sha256.Sum256(secret)
	labels, err := json.Marshal(nonNil(req.Labels))
	if err != nil {
		return api.WorkerCredential{}, err
	}
	var id string
	err = m.db.Transact(ctx, func(tx db.Tx) error {
		err := tx.QueryRow(ctx, `INSERT INTO workers (name, credential_hash, labels) VALUES ($1, $2, $3) RETURNING id`,
			req.Name, sum[:], labels).Scan(&id)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: a worker named %s is already registered", ErrConflict, req.Name)
		}
		if err != nil {
			return err
		}
		// Admitted on the bootstrap token: Hangar's own decision.
		return audit.Record(audit.WithActor(ctx, audit.System(audit.SystemHangar)), tx, audit.Event{
			Action: "worker.register", Target: workerRef(id, req.Name), Details: map[string]any{"labels": req.Labels}})
	})
	if err != nil {
		return api.WorkerCredential{}, err
	}
	return api.WorkerCredential{
		ID:         id,
		Credential: id + "." + base64.RawURLEncoding.EncodeToString(secret),
	}, nil
}

// Authenticate checks a credential issued by Register and returns the worker
// it belongs to. A revoked worker's credential never succeeds.
func (m *Manager) Authenticate(ctx context.Context, credential string) (string, error) {
	id, enc, ok := strings.Cut(credential, ".")
	if !ok || !db.ValidUUID(id) {
		return "", ErrUnauthorized
	}
	secret, err := base64.RawURLEncoding.DecodeString(enc)
	if err != nil {
		return "", ErrUnauthorized
	}
	var hash []byte
	err = m.db.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `SELECT credential_hash FROM workers WHERE id = $1 AND revoked_at IS NULL`, id).Scan(&hash)
	})
	if errors.Is(err, pgx.ErrNoRows) {
		return "", ErrUnauthorized
	}
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(secret)
	if subtle.ConstantTimeCompare(sum[:], hash) != 1 {
		return "", ErrUnauthorized
	}
	return id, nil
}

// Revoke stops a worker's credential working. Its environments stay
// assigned to it: their disks are on that machine, and moving them elsewhere
// is a decision to lose them, not something to do by default.
func (m *Manager) Revoke(ctx context.Context, id string) error {
	if !db.ValidUUID(id) {
		return ErrNotFound
	}
	return m.db.Transact(ctx, func(tx db.Tx) error {
		var name string
		err := tx.QueryRow(ctx, `UPDATE workers SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL
			RETURNING name`, id).Scan(&name)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "worker.revoke", Target: workerRef(id, name)}); err != nil {
			return err
		}
		// Wakes the worker's held request, so its next one is refused at
		// once rather than when the hold expires.
		return Bump(ctx, tx, id)
	})
}

// Delete forgets a revoked worker, which frees its name to register again.
// A worker still holding environments cannot be forgotten: their disks are on
// it, and the environments must be deleted first.
func (m *Manager) Delete(ctx context.Context, id string) error {
	if !db.ValidUUID(id) {
		return ErrNotFound
	}
	return m.db.Transact(ctx, func(tx db.Tx) error {
		var revoked bool
		err := tx.QueryRow(ctx, `SELECT revoked_at IS NOT NULL FROM workers WHERE id = $1 FOR UPDATE`, id).Scan(&revoked)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		if !revoked {
			return fmt.Errorf("%w: revoke the worker before removing it", ErrConflict)
		}
		var held int
		if err := tx.QueryRow(ctx, `SELECT count(*) FROM environments WHERE worker_id = $1`, id).Scan(&held); err != nil {
			return err
		}
		if held > 0 {
			return fmt.Errorf("%w: the worker still holds %d environments", ErrConflict, held)
		}
		var name string
		if err := tx.QueryRow(ctx, `DELETE FROM workers WHERE id = $1 RETURNING name`, id).Scan(&name); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "worker.delete", Target: workerRef(id, name)})
	})
}

func workerRef(id, name string) audit.Ref {
	return audit.Ref{Type: audit.KindWorker, ID: id, Name: name}
}

func (m *Manager) List(ctx context.Context) ([]api.Worker, error) {
	var out []api.Worker
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		out, err = list(ctx, tx, "")
		return err
	})
	if out == nil {
		out = []api.Worker{}
	}
	return out, err
}

// Get returns one worker.
func (m *Manager) Get(ctx context.Context, id string) (api.Worker, error) {
	if !db.ValidUUID(id) {
		return api.Worker{}, ErrNotFound
	}
	var out []api.Worker
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		out, err = list(ctx, tx, id)
		return err
	})
	if err != nil {
		return api.Worker{}, err
	}
	if len(out) == 0 {
		return api.Worker{}, ErrNotFound
	}
	return out[0], nil
}

// list reads every worker, or only the one with the given ID.
func list(ctx context.Context, tx db.Tx, id string) ([]api.Worker, error) {
	var only *string
	if id != "" {
		only = &id
	}
	rows, err := tx.Query(ctx, `
		SELECT w.id, w.name, w.labels, w.cpus, w.memory_mib, w.unknown, w.last_seen_at,
		       w.revoked_at IS NOT NULL, w.created_at,
		       coalesce(sum(e.cpus), 0), coalesce(sum(e.memory_mib), 0),
		       coalesce(w.last_seen_at > now() - $1::interval, false),
		       w.stats, w.images,
		       coalesce((SELECT jsonb_agg(r.ref ORDER BY r.ref) FROM worker_image_removals r
		                 WHERE r.worker_id = w.id), '[]')
		FROM workers w
		LEFT JOIN environments e ON e.worker_id = w.id AND e.desired <> 'deleted'
		WHERE $2::uuid IS NULL OR w.id = $2
		GROUP BY w.id
		ORDER BY w.name`, OnlineWindow, only)
	if err != nil {
		return nil, err
	}
	return pgx.CollectRows(rows, func(r pgx.CollectableRow) (api.Worker, error) {
		var w api.Worker
		var labels, unknown, stats, images, removals []byte
		if err := r.Scan(&w.ID, &w.Name, &labels, &w.Capacity.CPUs, &w.Capacity.MemoryMiB, &unknown,
			&w.LastSeenAt, &w.Revoked, &w.CreatedAt, &w.Allocated.CPUs, &w.Allocated.MemoryMiB, &w.Online,
			&stats, &images, &removals); err != nil {
			return w, err
		}
		for _, f := range []struct {
			raw []byte
			to  any
		}{{labels, &w.Labels}, {unknown, &w.Unknown}, {images, &w.Images}, {removals, &w.PendingRemovals}} {
			if err := json.Unmarshal(f.raw, f.to); err != nil {
				return w, err
			}
		}
		if stats != nil {
			w.Stats = &api.WorkerStats{}
			if err := json.Unmarshal(stats, w.Stats); err != nil {
				return w, err
			}
		}
		w.Online = w.Online && !w.Revoked
		return w, nil
	})
}

// DesiredVersion is the version of a worker's desired set, for deciding
// whether the worker has seen it without reading the whole set.
func (m *Manager) DesiredVersion(ctx context.Context, workerID string) (int64, error) {
	var v int64
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `SELECT desired_version FROM workers WHERE id = $1`, workerID).Scan(&v)
	})
	if errors.Is(err, pgx.ErrNoRows) {
		return 0, ErrNotFound
	}
	return v, err
}

// DesiredSet reads a worker's version and environments in one statement, so
// the version returned describes exactly the set returned with it.
func (m *Manager) DesiredSet(ctx context.Context, workerID string) (api.DesiredSet, error) {
	var set api.DesiredSet
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		set = api.DesiredSet{Environments: []api.EnvironmentSpec{}, RemoveImages: []string{}, RemoveEnvironments: []string{}}
		// Every row repeats the version. A worker with no environments
		// still has its row, with the environment columns NULL.
		rows, err := tx.Query(ctx, `
			SELECT w.desired_version, e.id, e.name, e.desired, e.spec
			FROM workers w
			LEFT JOIN environments e ON e.worker_id = w.id
			WHERE w.id = $1
			ORDER BY e.created_at`, workerID)
		if err != nil {
			return err
		}
		defer rows.Close()
		found := false
		for rows.Next() {
			found = true
			var id, name, desired *string
			var spec []byte
			if err := rows.Scan(&set.Version, &id, &name, &desired, &spec); err != nil {
				return err
			}
			if id == nil {
				continue
			}
			e := api.EnvironmentSpec{ID: *id, Name: *name, Desired: api.DesiredState(*desired)}
			if err := json.Unmarshal(spec, &e.Spec); err != nil {
				return fmt.Errorf("environment %s: %w", *id, err)
			}
			set.Environments = append(set.Environments, e)
		}
		if err := rows.Err(); err != nil {
			return err
		}
		if !found {
			return ErrNotFound
		}
		rows.Close()
		// Read after the version: a removal asked for in between raises
		// the version again, so the worker comes straight back for it.
		rows, err = tx.Query(ctx, `SELECT ref FROM worker_image_removals WHERE worker_id = $1 ORDER BY ref`,
			workerID)
		if err != nil {
			return err
		}
		if set.RemoveImages, err = pgx.CollectRows(rows, pgx.RowTo[string]); err != nil {
			return err
		}
		rows, err = tx.Query(ctx, `SELECT environment FROM worker_environment_removals WHERE worker_id = $1
			ORDER BY environment`, workerID)
		if err != nil {
			return err
		}
		set.RemoveEnvironments, err = pgx.CollectRows(rows, pgx.RowTo[string])
		return err
	})
	return set, err
}

var validPhases = map[api.Phase]bool{
	api.PhaseStarting:   true,
	api.PhaseRunning:    true,
	api.PhaseStopping:   true,
	api.PhaseStopped:    true,
	api.PhaseSuspending: true,
	api.PhaseSuspended:  true,
	api.PhaseFailed:     true,
	api.PhaseDeleting:   true,
}

// ValidateStatus checks a report before anything is written, so a malformed
// one is refused whole.
func ValidateStatus(st api.WorkerStatus) error {
	if st.Capacity.CPUs < 0 || st.Capacity.MemoryMiB < 0 {
		return fmt.Errorf("%w: capacity cannot be negative", ErrInvalid)
	}
	for _, o := range st.Environments {
		if !validPhases[o.Phase] {
			return fmt.Errorf("%w: environment %s: unknown phase %q", ErrInvalid, o.ID, o.Phase)
		}
	}
	return nil
}

// ReportStatus records what a worker says it holds, and marks it seen.
//
// A worker speaks only for environments placed on it. A report about any
// other ID is recorded as unknown to the server and never applied, so a
// worker cannot alter another's environments by naming them. An environment
// being deleted that the worker leaves out of its report is gone, and its
// row goes with it.
func (m *Manager) ReportStatus(ctx context.Context, workerID string, st api.WorkerStatus) error {
	if err := ValidateStatus(st); err != nil {
		return err
	}
	labels, err := json.Marshal(nonNil(st.Labels))
	if err != nil {
		return err
	}
	return m.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT id, desired, name, owner_id::text, phase FROM environments WHERE worker_id = $1
			ORDER BY id FOR UPDATE`, workerID)
		if err != nil {
			return err
		}
		assigned, err := pgx.CollectRows(rows, func(r pgx.CollectableRow) (placed, error) {
			var p placed
			return p, r.Scan(&p.id, &p.desired, &p.name, &p.owner, &p.phase)
		})
		if err != nil {
			return err
		}

		var oldCPUs, oldMem int
		var workerName string
		err = tx.QueryRow(ctx, `SELECT cpus, memory_mib, name FROM workers WHERE id = $1 FOR UPDATE`, workerID).
			Scan(&oldCPUs, &oldMem, &workerName)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		desired := map[string]api.DesiredState{}
		byID := map[string]placed{}
		for _, p := range assigned {
			desired[p.id] = p.desired
			byID[p.id] = p
		}
		// What the worker reports its environments doing is its own
		// account of them.
		wctx := audit.WithActor(ctx, audit.Worker(workerID, workerName))

		unknown := []string{}
		reported := map[string]bool{}
		for _, o := range st.Environments {
			if _, ok := desired[o.ID]; !ok {
				unknown = append(unknown, o.ID)
				continue
			}
			reported[o.ID] = true
			if p := byID[o.ID]; p.phase != o.Phase {
				if err := audit.Record(wctx, tx, audit.Event{Action: "environment.phase",
					Target:  audit.Ref{Type: audit.KindEnvironment, ID: p.id, Name: p.name},
					Related: []audit.Ref{{Type: audit.KindOwner, ID: p.owner}},
					Details: map[string]any{"from": p.phase, "to": o.Phase, "reason": o.Reason}}); err != nil {
					return err
				}
			}
			var stats []byte
			if o.Stats != nil {
				if stats, err = json.Marshal(o.Stats); err != nil {
					return err
				}
			}
			// updated_at marks a change of phase, not every measurement.
			if _, err := tx.Exec(ctx, `UPDATE environments SET phase = $2, reason = $3, stats = $4,
				updated_at = CASE WHEN (phase, reason) IS DISTINCT FROM ($2, $3) THEN now() ELSE updated_at END
				WHERE id = $1`, o.ID, o.Phase, o.Reason, stats); err != nil {
				return err
			}
		}

		removed := false
		for id, d := range desired {
			if d == api.DesiredDeleted && !reported[id] {
				if _, err := tx.Exec(ctx, `DELETE FROM environments WHERE id = $1`, id); err != nil {
					return err
				}
				p := byID[id]
				if err := audit.Record(wctx, tx, audit.Event{Action: "environment.deleted",
					Target:  audit.Ref{Type: audit.KindEnvironment, ID: id, Name: p.name},
					Related: []audit.Ref{{Type: audit.KindOwner, ID: p.owner}}}); err != nil {
					return err
				}
				removed = true
			}
		}
		if removed {
			if err := Bump(ctx, tx, workerID); err != nil {
				return err
			}
		}

		unknownJSON, err := json.Marshal(unknown)
		if err != nil {
			return err
		}
		var stats []byte
		if st.Stats != nil {
			if stats, err = json.Marshal(st.Stats); err != nil {
				return err
			}
		}
		images := st.Images
		if images == nil {
			images = []api.LocalImage{}
		}
		imagesJSON, err := json.Marshal(images)
		if err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `UPDATE workers SET cpus = $2, memory_mib = $3, labels = $4, unknown = $5,
			stats = $6, images = $7, last_seen_at = now() WHERE id = $1`,
			workerID, st.Capacity.CPUs, st.Capacity.MemoryMiB, labels, unknownJSON, stats, imagesJSON); err != nil {
			return err
		}

		// A removal is done once the worker stops reporting the image.
		held := make([]string, len(images))
		for i, img := range images {
			held[i] = img.Ref
		}
		if _, err := tx.Exec(ctx, `DELETE FROM worker_image_removals
			WHERE worker_id = $1 AND NOT (ref = ANY($2::text[]))`, workerID, held); err != nil {
			return err
		}
		// And a deletion once it stops reporting the environment.
		if _, err := tx.Exec(ctx, `DELETE FROM worker_environment_removals
			WHERE worker_id = $1 AND NOT (environment = ANY($2::text[]))`, workerID, unknown); err != nil {
			return err
		}
		if st.Capacity.CPUs != oldCPUs || st.Capacity.MemoryMiB != oldMem {
			return db.Notify(ctx, tx, CapacityChannel, workerID)
		}
		return nil
	})
}

var (
	// ErrInUse is returned for an image an environment on the worker still
	// uses.
	ErrInUse = errors.New("in use")
	// ErrNoImage is returned for an image the worker does not hold.
	ErrNoImage = errors.New("image not found")
)

// RemoveImage asks a worker to delete an image from its local store. The
// request is refused while an environment placed on the worker uses the
// image; otherwise it stands until the worker reports the image gone.
func (m *Manager) RemoveImage(ctx context.Context, workerID, ref string) error {
	if !db.ValidUUID(workerID) {
		return ErrNotFound
	}
	return m.db.Transact(ctx, func(tx db.Tx) error {
		// Environments before workers, as everywhere.
		var users []string
		rows, err := tx.Query(ctx, `SELECT name FROM environments
			WHERE worker_id = $1 AND desired <> 'deleted' AND image = $2 ORDER BY name FOR UPDATE`, workerID, ref)
		if err != nil {
			return err
		}
		if users, err = pgx.CollectRows(rows, pgx.RowTo[string]); err != nil {
			return err
		}
		var held bool
		err = tx.QueryRow(ctx, `SELECT images @> jsonb_build_array(jsonb_build_object('ref', $2::text))
			FROM workers WHERE id = $1 FOR UPDATE`, workerID, ref).Scan(&held)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		if !held {
			return fmt.Errorf("%w: the worker does not hold %s", ErrNoImage, ref)
		}
		if len(users) > 0 {
			return fmt.Errorf("%w: used by %s", ErrInUse, strings.Join(users, ", "))
		}
		if _, err := tx.Exec(ctx, `INSERT INTO worker_image_removals (worker_id, ref) VALUES ($1, $2)
			ON CONFLICT DO NOTHING`, workerID, ref); err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "image.remove",
			Target:  audit.Ref{Type: audit.KindImage, ID: ref, Name: ref},
			Related: []audit.Ref{{Type: audit.KindWorker, ID: workerID}}}); err != nil {
			return err
		}
		return Bump(ctx, tx, workerID)
	})
}

// RemoveUnknown asks a worker to delete environments it runs that the
// server has no record of. Each must be one the worker reports as such; an
// environment the server knows is deleted through the environment instead.
func (m *Manager) RemoveUnknown(ctx context.Context, workerID string, ids []string) error {
	if !db.ValidUUID(workerID) {
		return ErrNotFound
	}
	return m.db.Transact(ctx, func(tx db.Tx) error {
		var unknown []string
		var raw []byte
		err := tx.QueryRow(ctx, `SELECT unknown FROM workers WHERE id = $1 FOR UPDATE`, workerID).Scan(&raw)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		if err := json.Unmarshal(raw, &unknown); err != nil {
			return err
		}
		for _, id := range ids {
			if !slices.Contains(unknown, id) {
				return fmt.Errorf("%w: the worker does not report %s as unknown to the server", ErrInvalid, id)
			}
			var known bool
			if db.ValidUUID(id) {
				if err := tx.QueryRow(ctx, `SELECT EXISTS (SELECT 1 FROM environments WHERE id = $1)`, id).Scan(&known); err != nil {
					return err
				}
			}
			if known {
				return fmt.Errorf("%w: %s is an environment the server knows", ErrInvalid, id)
			}
			if _, err := tx.Exec(ctx, `INSERT INTO worker_environment_removals (worker_id, environment)
				VALUES ($1, $2) ON CONFLICT DO NOTHING`, workerID, id); err != nil {
				return err
			}
			if err := audit.Record(ctx, tx, audit.Event{Action: "environment.remove_unknown",
				Target:  audit.Ref{Type: audit.KindEnvironment, ID: id},
				Related: []audit.Ref{{Type: audit.KindWorker, ID: workerID}}}); err != nil {
				return err
			}
		}
		return Bump(ctx, tx, workerID)
	})
}

type placed struct {
	id      string
	desired api.DesiredState
	name    string
	owner   string
	phase   api.Phase
}

// Touch records that a worker is alive without a full report.
func (m *Manager) Touch(ctx context.Context, workerID string) error {
	return m.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `UPDATE workers SET last_seen_at = now() WHERE id = $1`, workerID)
		return err
	})
}

func nonNil(l map[string]string) map[string]string {
	if l == nil {
		return map[string]string{}
	}
	return l
}
