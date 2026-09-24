// Package placement decides which worker runs each environment.
//
// Placement is permanent. An environment's disks are local to the worker it
// lands on, so once placed it stays there for life; nothing here moves an
// environment, and an environment whose worker disappears waits for it.
package placement

import (
	"context"
	"encoding/json"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/workers"
)

// Channel fires when an environment may be waiting to be placed.
const Channel = "hangar_placement"

// The reasons given to an environment that cannot be placed yet.
const (
	// Unplaceable is for an environment no suitable worker has room for.
	Unplaceable = "waiting for a worker with room for it"
	// NoMatch is for an environment whose template's placement rules no
	// online worker satisfies, however much room it has.
	NoMatch = "waiting for a worker that matches its placement rules"
)

// batch bounds how much of the queue one transaction claims, so a long queue
// is shared between replicas rather than taken whole by one.
const batch = 100

type Manager struct {
	db *db.DB
}

func NewManager(d *db.DB) *Manager { return &Manager{db: d} }

type env struct {
	id        string
	name      string
	owner     string
	cpus      int
	mem       int
	reason    string
	placement map[string]string
}

type worker struct {
	id        string
	cpus, mem int
	labels    map[string]string
}

// matches reports whether a worker's labels satisfy a placement selector:
// every key present, with the same value.
func (w *worker) matches(selector map[string]string) bool {
	for k, v := range selector {
		if got, ok := w.labels[k]; !ok || got != v {
			return false
		}
	}
	return true
}

// Place assigns environments that want to run to online workers with room
// for them, and returns how many it placed.
//
// Several replicas may place at once. The queue is claimed with SKIP LOCKED,
// so they take different environments, and the candidate workers are locked
// for the length of the transaction, so two replicas cannot both spend the
// same worker's last free memory.
//
// Every environment placed on a worker spends its capacity, stopped or not.
// Placement is permanent, so a stopped environment must still fit when it is
// started again.
func (m *Manager) Place(ctx context.Context) (int, error) {
	var placed int
	// Placement is Hangar's decision, whoever's change prompted it.
	actor := audit.WithActor(ctx, audit.System(audit.SystemPlacement))
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		placed = 0
		rows, err := tx.Query(ctx, `SELECT id, name, owner_id::text, cpus, memory_mib, reason, coalesce(spec->'placement', '{}')
			FROM environments
			WHERE worker_id IS NULL AND desired = 'running'
			ORDER BY created_at LIMIT $1 FOR UPDATE SKIP LOCKED`, batch)
		if err != nil {
			return err
		}
		queue, err := pgx.CollectRows(rows, func(r pgx.CollectableRow) (env, error) {
			var e env
			var sel []byte
			if err := r.Scan(&e.id, &e.name, &e.owner, &e.cpus, &e.mem, &e.reason, &sel); err != nil {
				return e, err
			}
			return e, json.Unmarshal(sel, &e.placement)
		})
		if err != nil || len(queue) == 0 {
			return err
		}

		// Only the workers locked here are candidates. Their allocation is
		// summed by a later statement, which at READ COMMITTED sees
		// everything committed by the time the locks were granted -- including
		// what another replica placed while this one waited for them.
		rows, err = tx.Query(ctx, `SELECT id FROM workers
			WHERE revoked_at IS NULL AND last_seen_at > now() - $1::interval
			ORDER BY id FOR UPDATE`, workers.OnlineWindow)
		if err != nil {
			return err
		}
		locked, err := pgx.CollectRows(rows, pgx.RowTo[string])
		if err != nil {
			return err
		}
		rows, err = tx.Query(ctx, `
			SELECT w.id, w.cpus - coalesce(sum(e.cpus), 0), w.memory_mib - coalesce(sum(e.memory_mib), 0), w.labels
			FROM workers w
			LEFT JOIN environments e ON e.worker_id = w.id AND e.desired <> 'deleted'
			WHERE w.id = ANY($1::uuid[])
			GROUP BY w.id`, locked)
		if err != nil {
			return err
		}
		free, err := pgx.CollectRows(rows, func(r pgx.CollectableRow) (*worker, error) {
			w := &worker{}
			var labels []byte
			if err := r.Scan(&w.id, &w.cpus, &w.mem, &labels); err != nil {
				return w, err
			}
			return w, json.Unmarshal(labels, &w.labels)
		})
		if err != nil {
			return err
		}

		bumped := map[string]bool{}
		for _, e := range queue {
			best, matched := choose(free, e)
			if best == nil {
				reason := Unplaceable
				if !matched {
					reason = NoMatch
				}
				if e.reason != reason {
					if _, err := tx.Exec(ctx, `UPDATE environments SET reason = $2, updated_at = now() WHERE id = $1`,
						e.id, reason); err != nil {
						return err
					}
					if err := audit.Record(actor, tx, audit.Event{Action: "environment.unplaceable",
						Target:  audit.Ref{Type: audit.KindEnvironment, ID: e.id, Name: e.name},
						Related: []audit.Ref{{Type: audit.KindOwner, ID: e.owner}},
						Details: map[string]any{"reason": reason}}); err != nil {
						return err
					}
				}
				continue
			}
			best.cpus -= e.cpus
			best.mem -= e.mem
			if _, err := tx.Exec(ctx, `UPDATE environments SET worker_id = $2, phase = 'pending', reason = '',
				updated_at = now() WHERE id = $1`, e.id, best.id); err != nil {
				return err
			}
			if err := audit.Record(actor, tx, audit.Event{Action: "environment.place",
				Target:  audit.Ref{Type: audit.KindEnvironment, ID: e.id, Name: e.name},
				Related: []audit.Ref{{Type: audit.KindOwner, ID: e.owner}, {Type: audit.KindWorker, ID: best.id}},
				Details: map[string]any{"cpus": e.cpus, "memory_mib": e.mem}}); err != nil {
				return err
			}
			bumped[best.id] = true
			placed++
		}
		for id := range bumped {
			if err := workers.Bump(ctx, tx, id); err != nil {
				return err
			}
		}
		return nil
	})
	return placed, err
}

// choose picks a worker for an environment from those its placement rules
// allow, and reports whether any were allowed at all. It spreads rather than
// packs: the allowed worker with the most free memory that fits takes the
// environment. Memory is what runs out first.
func choose(free []*worker, e env) (*worker, bool) {
	var best *worker
	matched := false
	for _, w := range free {
		if !w.matches(e.placement) {
			continue
		}
		matched = true
		if w.cpus >= e.cpus && w.mem >= e.mem && (best == nil || w.mem > best.mem) {
			best = w
		}
	}
	return best, matched
}
