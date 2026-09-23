// Package placement decides which worker runs each environment.
//
// Placement is permanent. An environment's disks are local to the worker it
// lands on, so once placed it stays there for life; nothing here moves an
// environment, and an environment whose worker disappears waits for it.
package placement

import (
	"context"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/workers"
)

// Channel fires when an environment may be waiting to be placed.
const Channel = "hangar_placement"

// Unplaceable is the reason given to an environment no worker has room for.
const Unplaceable = "waiting for a worker with room for it"

// batch bounds how much of the queue one transaction claims, so a long queue
// is shared between replicas rather than taken whole by one.
const batch = 100

type Manager struct {
	db *db.DB
}

func NewManager(d *db.DB) *Manager { return &Manager{db: d} }

type env struct {
	id     string
	cpus   int
	mem    int
	reason string
}

type worker struct {
	id        string
	cpus, mem int
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
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		placed = 0
		rows, err := tx.Query(ctx, `SELECT id, cpus, memory_mib, reason FROM environments
			WHERE worker_id IS NULL AND desired = 'running'
			ORDER BY created_at LIMIT $1 FOR UPDATE SKIP LOCKED`, batch)
		if err != nil {
			return err
		}
		queue, err := pgx.CollectRows(rows, func(r pgx.CollectableRow) (env, error) {
			var e env
			return e, r.Scan(&e.id, &e.cpus, &e.mem, &e.reason)
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
			SELECT w.id, w.cpus - coalesce(sum(e.cpus), 0), w.memory_mib - coalesce(sum(e.memory_mib), 0)
			FROM workers w
			LEFT JOIN environments e ON e.worker_id = w.id AND e.desired <> 'deleted'
			WHERE w.id = ANY($1::uuid[])
			GROUP BY w.id`, locked)
		if err != nil {
			return err
		}
		free, err := pgx.CollectRows(rows, func(r pgx.CollectableRow) (*worker, error) {
			w := &worker{}
			return w, r.Scan(&w.id, &w.cpus, &w.mem)
		})
		if err != nil {
			return err
		}

		bumped := map[string]bool{}
		for _, e := range queue {
			best := choose(free, e)
			if best == nil {
				if e.reason != Unplaceable {
					if _, err := tx.Exec(ctx, `UPDATE environments SET reason = $2, updated_at = now() WHERE id = $1`,
						e.id, Unplaceable); err != nil {
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

// choose spreads rather than packs: the worker with the most free memory
// that fits takes the environment. Memory is what runs out first.
func choose(free []*worker, e env) *worker {
	var best *worker
	for _, w := range free {
		if w.cpus >= e.cpus && w.mem >= e.mem && (best == nil || w.mem > best.mem) {
			best = w
		}
	}
	return best
}
