// Package audit records who did what: every change a person makes, and
// every decision Hangar makes on its own.
//
// Each event has an actor. A person acts through the web UI or an
// environment of theirs; Hangar's own decisions -- placing an environment,
// pruning what has expired -- are taken by system users, rows in the users
// table that nobody can sign in as, so an actor is always a user or a
// worker and every event can say which. A worker acts when it reports what
// its environments are doing. Someone not signed in, failing to sign in, is
// an actor with neither, and the name they gave.
//
// An event is recorded in the same transaction as the change it records,
// so there is no change without its event, and no event for a change that
// was rolled back.
//
// Every event carries subjects, tags naming what it concerns --
// environment:<id>, template:<id>, worker:<id>, image:<ref>, user:<id>,
// owner:<id> -- and each view of the log (an environment's, a template's,
// an image's, a person's) is the events tagged with it.
package audit

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/db"
)

// System users. Their rows are made by the schema, with these IDs.
const (
	// SystemHangar is the control plane itself, for what it does on its
	// own account: pruning, registering workers, cleaning up.
	SystemHangar = "00000000-0000-4000-8000-000000000001"
	// SystemPlacement decides which worker runs an environment.
	SystemPlacement = "00000000-0000-4000-8000-000000000002"
)

// Actor is who an event is by. One of UserID and WorkerID is set, or
// neither for someone not signed in.
type Actor struct {
	UserID   string
	WorkerID string
	// Name is how the actor was known at the time, kept with the event so
	// it still reads after the actor is gone.
	Name string
	// Via is what the actor acted through, when not the web UI: an
	// environment, for a change its owner's programs made there.
	Via string
	// IP is the address a request came from.
	IP string
}

// User is a person or system user acting.
func User(id, name string) Actor { return Actor{UserID: id, Name: name} }

// Worker is a worker acting.
func Worker(id, name string) Actor { return Actor{WorkerID: id, Name: name} }

// System is one of the system users acting.
func System(id string) Actor {
	name := "hangar"
	if id == SystemPlacement {
		name = "hangar:placement"
	}
	return Actor{UserID: id, Name: name}
}

type actorKey struct{}

// WithActor is ctx acting as a.
func WithActor(ctx context.Context, a Actor) context.Context {
	return context.WithValue(ctx, actorKey{}, a)
}

// ActorFrom is the actor ctx acts as. Without one, it is Hangar itself.
func ActorFrom(ctx context.Context) Actor {
	if a, ok := ctx.Value(actorKey{}).(Actor); ok {
		return a
	}
	return System(SystemHangar)
}

// Ref names something an event concerns.
type Ref struct {
	Type string
	ID   string
	Name string
}

func (r Ref) tag() string { return r.Type + ":" + r.ID }

// What a Ref names.
const (
	KindEnvironment = "environment"
	KindTemplate    = "template"
	KindWorker      = "worker"
	KindImage       = "image"
	KindUser        = "user"
	KindOwner       = "owner"
	KindSSHKey      = "ssh_key"
	KindFile        = "profile_file"
	KindPath        = "profile_path"
)

// Event is one thing that happened.
type Event struct {
	// Action is what happened, as noun.verb: environment.create.
	Action string
	// Target is what it happened to.
	Target Ref
	// Related are the other things it concerns: the owner of an
	// environment, the worker it was placed on, the image it runs.
	Related []Ref
	// Details are whatever else is worth keeping.
	Details map[string]any
}

// Record writes an event, by the actor ctx acts as, in tx.
func Record(ctx context.Context, tx db.Tx, e Event) error {
	a := ActorFrom(ctx)
	subjects := []string{e.Target.tag()}
	for _, r := range e.Related {
		if r.ID != "" {
			subjects = append(subjects, r.tag())
		}
	}
	if a.UserID != "" {
		subjects = append(subjects, "actor:"+a.UserID)
	}
	if a.WorkerID != "" {
		subjects = append(subjects, "worker:"+a.WorkerID)
	}
	details := e.Details
	if details == nil {
		details = map[string]any{}
	}
	if a.Via != "" {
		details["via"] = a.Via
	}
	b, err := json.Marshal(details)
	if err != nil {
		return err
	}
	_, err = tx.Exec(ctx, `INSERT INTO audit_events
			(actor_user, actor_worker, actor_name, source_ip, action, target_type, target_id, target_name, subjects, details)
		VALUES (nullif($1, '')::uuid, nullif($2, '')::uuid, $3, $4, $5, $6, $7, $8, $9, $10)`,
		a.UserID, a.WorkerID, a.Name, a.IP, e.Action, e.Target.Type, e.Target.ID, e.Target.Name, subjects, b)
	if err != nil {
		return fmt.Errorf("recording %s: %w", e.Action, err)
	}
	return nil
}

// Entry is a recorded event.
type Entry struct {
	ID          int64
	At          time.Time
	ActorUser   string
	ActorWorker string
	ActorKind   string // person, system, worker, or anonymous
	ActorName   string
	IP          string
	Action      string
	Target      Ref
	Subjects    []string
	Details     map[string]any
}

// Query selects entries.
type Query struct {
	// Subjects the entries must all be tagged with, as type:id.
	Subjects []string
	// AnyOf, if set, the entries must be tagged with at least one of.
	AnyOf []string
	// Before is an entry ID: only older entries, for paging.
	Before int64
	Limit  int
}

// Log reads the log.
type Log struct {
	db *db.DB
}

func NewLog(d *db.DB) *Log { return &Log{db: d} }

// List returns entries, newest first.
func (l *Log) List(ctx context.Context, q Query) ([]Entry, error) {
	if q.Limit <= 0 || q.Limit > 500 {
		q.Limit = 100
	}
	if q.Before <= 0 {
		q.Before = 1<<63 - 1
	}
	if q.Subjects == nil {
		q.Subjects = []string{}
	}
	var out []Entry
	err := l.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `
			SELECT e.id, e.at, coalesce(e.actor_user::text, ''), coalesce(e.actor_worker::text, ''),
				CASE WHEN e.actor_worker IS NOT NULL THEN 'worker'
				     WHEN e.actor_user IS NULL THEN 'anonymous'
				     ELSE coalesce(u.kind, 'person') END,
				e.actor_name, e.source_ip, e.action, e.target_type, e.target_id, e.target_name, e.subjects, e.details
			FROM audit_events e
			LEFT JOIN users u ON u.id = e.actor_user
			WHERE e.id < $1 AND e.subjects @> $2
				AND ($3::text[] IS NULL OR e.subjects && $3)
			ORDER BY e.id DESC
			LIMIT $4`, q.Before, q.Subjects, q.AnyOf, q.Limit)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (Entry, error) {
			var e Entry
			var details []byte
			err := r.Scan(&e.ID, &e.At, &e.ActorUser, &e.ActorWorker, &e.ActorKind, &e.ActorName, &e.IP,
				&e.Action, &e.Target.Type, &e.Target.ID, &e.Target.Name, &e.Subjects, &details)
			if err == nil {
				err = json.Unmarshal(details, &e.Details)
			}
			return e, err
		})
		return err
	})
	return out, err
}

// Record writes an event on its own, for an action that changes nothing
// else: opening a terminal, signing in to an editor.
func (l *Log) Record(ctx context.Context, e Event) error {
	return l.db.Transact(ctx, func(tx db.Tx) error { return Record(ctx, tx, e) })
}
