package environments

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"unicode/utf8"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/users"
)

// Channel carries an environment's ID whenever something a client shows of
// it changes, sent by migration 0016's triggers: its row, its task, its
// agents' sessions. An ID that does not resolve is an environment gone.
const Channel = "environment_changed"

// taskColumns and agentColumns read an environment's task and its agents'
// sessions as JSON, beside its other columns.
const (
	taskColumns = `(SELECT jsonb_build_object('id', t.id, 'text', t.text, 'set_by', t.set_by, 'set_at', t.set_at)
		FROM environment_tasks t WHERE t.environment_id = e.id AND t.done_at IS NULL)`
	agentColumns = `(SELECT jsonb_agg(jsonb_build_object('id', a.id, 'agent', a.agent, 'terminal', a.terminal,
			'state', a.state, 'step', a.step, 'message', a.message, 'since', a.since,
			'seen', a.seen_at IS NOT NULL AND a.seen_at >= a.since) ORDER BY a.since)
		FROM agent_sessions a WHERE a.environment_id = e.id)`
)

// scanAgents fills in an environment's task and its agents' sessions from
// taskColumns and agentColumns. A finished session its owner has seen is
// idle.
func scanAgents(e *api.Environment, task, agents []byte) error {
	if task != nil {
		e.Task = &api.Task{}
		if err := json.Unmarshal(task, e.Task); err != nil {
			return err
		}
	}
	if agents != nil {
		if err := json.Unmarshal(agents, &e.Agents); err != nil {
			return err
		}
		for i, a := range e.Agents {
			if a.State == api.AgentFinished && a.Seen {
				e.Agents[i].State = api.AgentIdle
			}
		}
	}
	return nil
}

// CleanTask is a task as it is kept: on one line, spaces collapsed, and
// between 1 and api.MaxTaskLength characters.
func CleanTask(text string) (string, error) {
	text = strings.Join(strings.Fields(text), " ")
	switch {
	case text == "":
		return "", fmt.Errorf("%w: a task needs some text", ErrInvalid)
	case !utf8.ValidString(text):
		return "", fmt.Errorf("%w: a task must be UTF-8", ErrInvalid)
	case utf8.RuneCountInString(text) > api.MaxTaskLength:
		return "", fmt.Errorf("%w: a task has at most %d characters", ErrInvalid, api.MaxTaskLength)
	}
	return text, nil
}

// SetTask sets the task of an environment p may reach, as its owner would.
// The task before it is done.
func (m *Manager) SetTask(ctx context.Context, p users.Principal, id, text string) (api.Task, error) {
	var t api.Task
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		if err := reachable(ctx, tx, p, id); err != nil {
			return err
		}
		var err error
		t, err = setTask(ctx, tx, id, text, api.TaskByUser)
		return err
	})
	return t, err
}

// EndTask marks the task of an environment p may reach done, leaving it
// none.
func (m *Manager) EndTask(ctx context.Context, p users.Principal, id string) error {
	return m.db.Transact(ctx, func(tx db.Tx) error {
		if err := reachable(ctx, tx, p, id); err != nil {
			return err
		}
		return endTask(ctx, tx, id)
	})
}

// Tasks returns the tasks of an environment p may reach, newest first: its
// task, if it has one, then those done.
func (m *Manager) Tasks(ctx context.Context, p users.Principal, id string) ([]api.Task, error) {
	var out []api.Task
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		if err := reachable(ctx, tx, p, id); err != nil {
			return err
		}
		rows, err := tx.Query(ctx, `SELECT id::text, text, set_by, set_at, done_at FROM environment_tasks
			WHERE environment_id = $1 ORDER BY set_at DESC, done_at DESC NULLS FIRST`, id)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (api.Task, error) {
			var t api.Task
			return t, r.Scan(&t.ID, &t.Text, &t.SetBy, &t.SetAt, &t.DoneAt)
		})
		return err
	})
	if out == nil {
		out = []api.Task{}
	}
	return out, err
}

// AgentSetTask sets an environment's task as its coding agent does, from
// inside it. An empty text marks the task done.
func (m *Manager) AgentSetTask(ctx context.Context, id, text string) (*api.Task, error) {
	var t *api.Task
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		if text == "" {
			return endTask(ctx, tx, id)
		}
		set, err := setTask(ctx, tx, id, text, api.TaskByAgent)
		t = &set
		return err
	})
	return t, err
}

// Task returns an environment's task, nil when it has none.
func (m *Manager) Task(ctx context.Context, id string) (*api.Task, error) {
	var t *api.Task
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var task api.Task
		err := tx.QueryRow(ctx, `SELECT id::text, text, set_by, set_at FROM environment_tasks
			WHERE environment_id = $1 AND done_at IS NULL`, id).Scan(&task.ID, &task.Text, &task.SetBy, &task.SetAt)
		switch {
		case errors.Is(err, pgx.ErrNoRows):
			return nil
		case err != nil:
			return err
		}
		t = &task
		return nil
	})
	return t, err
}

// reachable locks an environment p may reach, or is ErrNotFound.
func reachable(ctx context.Context, tx db.Tx, p users.Principal, id string) error {
	if !db.ValidUUID(id) {
		return ErrNotFound
	}
	err := tx.QueryRow(ctx, `SELECT 1 FROM environments e WHERE `+visible+` AND e.id = $3 FOR UPDATE`,
		p.Admin, p.UserID, id).Scan(new(int))
	if errors.Is(err, pgx.ErrNoRows) {
		return ErrNotFound
	}
	return err
}

func setTask(ctx context.Context, tx db.Tx, id, text, by string) (api.Task, error) {
	text, err := CleanTask(text)
	if err != nil {
		return api.Task{}, err
	}
	// The same text again is the same task.
	var t api.Task
	err = tx.QueryRow(ctx, `SELECT id::text, text, set_by, set_at FROM environment_tasks
		WHERE environment_id = $1 AND done_at IS NULL FOR UPDATE`, id).Scan(&t.ID, &t.Text, &t.SetBy, &t.SetAt)
	switch {
	case err == nil && t.Text == text:
		return t, nil
	case err == nil:
		if err := endTask(ctx, tx, id); err != nil {
			return api.Task{}, err
		}
	case !errors.Is(err, pgx.ErrNoRows):
		return api.Task{}, err
	}
	t = api.Task{Text: text, SetBy: by}
	err = tx.QueryRow(ctx, `INSERT INTO environment_tasks (environment_id, text, set_by) VALUES ($1, $2, $3)
		RETURNING id::text, set_at`, id, text, by).Scan(&t.ID, &t.SetAt)
	return t, err
}

func endTask(ctx context.Context, tx db.Tx, id string) error {
	_, err := tx.Exec(ctx, `UPDATE environment_tasks SET done_at = now()
		WHERE environment_id = $1 AND done_at IS NULL`, id)
	return err
}

// ReportAgents records the coding agents' sessions an environment's guest
// agent reports, all of them: a session it leaves out is gone. Whether its
// owner has seen each is kept.
func (m *Manager) ReportAgents(ctx context.Context, id string, sessions []api.AgentSession) error {
	ids := make([]string, 0, len(sessions))
	for _, s := range sessions {
		ids = append(ids, s.ID)
	}
	return m.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := tx.Exec(ctx, `DELETE FROM agent_sessions WHERE environment_id = $1 AND NOT (id = ANY($2))`,
			id, ids); err != nil {
			return err
		}
		for _, s := range sessions {
			// An unchanged session is left as it is, so it sends no change.
			if _, err := tx.Exec(ctx, `INSERT INTO agent_sessions AS a
					(environment_id, id, agent, terminal, state, step, message, since)
				VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
				ON CONFLICT (environment_id, id) DO UPDATE SET agent = $3, terminal = $4, state = $5, step = $6,
					message = $7, since = $8
				WHERE (a.agent, a.terminal, a.state, a.step, a.message, a.since) IS DISTINCT FROM ($3, $4, $5, $6, $7, $8)`,
				id, s.ID, s.Agent, s.Terminal, s.State, s.Step, s.Message, s.Since); err != nil {
				return err
			}
		}
		return nil
	})
}

// SeenAgent records that p has looked at an agent's session in an
// environment they may reach: one that asked or finished before then does
// not need them.
func (m *Manager) SeenAgent(ctx context.Context, p users.Principal, id, session string) error {
	return m.db.Transact(ctx, func(tx db.Tx) error {
		if err := reachable(ctx, tx, p, id); err != nil {
			return err
		}
		tag, err := tx.Exec(ctx, `UPDATE agent_sessions SET seen_at = greatest(now(), since)
			WHERE environment_id = $1 AND id = $2`, id, session)
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return ErrNotFound
		}
		return nil
	})
}
