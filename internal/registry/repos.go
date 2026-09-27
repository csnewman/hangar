package registry

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
)

var (
	errNameInvalid = errors.New("invalid repository name")
	errNameUnknown = errors.New("repository not found")
	errDenied      = errors.New("denied")
)

// The conditions below take the caller as $1 (admin) and $2 (user ID, NULL
// for a worker), with the repository as r.
const (
	rOwns     = `(r.owner_id = $2)`
	rTeamRole = `(SELECT m.role FROM team_members m WHERE m.team_id = r.team_id AND m.user_id = $2)`
	rCollab   = `EXISTS (SELECT 1 FROM image_repository_collaborators c WHERE c.repository_id = r.id AND c.user_id = $2)`
	rViaTeam  = `EXISTS (SELECT 1 FROM image_repository_team_collaborators c JOIN team_members m ON m.team_id = c.team_id
		WHERE c.repository_id = r.id AND m.user_id = $2)`
	rViaTeamAs = `EXISTS (SELECT 1 FROM image_repository_team_collaborators c JOIN team_members m ON m.team_id = c.team_id
		WHERE c.repository_id = r.id AND m.user_id = $2 AND m.role IN ('member', 'admin'))`

	canPull = `coalesce((r.visibility = 'shared' OR $1 OR ` + rOwns + ` OR ` + rTeamRole + ` IS NOT NULL OR ` +
		rCollab + ` OR ` + rViaTeam + `), false)`
	canPush = `coalesce(($1 OR ` + rOwns + ` OR ` + rTeamRole + ` IN ('member', 'admin') OR ` + rCollab + ` OR ` +
		rViaTeamAs + `), false)`
	canManage = `coalesce(($1 OR ` + rOwns + ` OR ` + rTeamRole + ` = 'admin'), false)`
)

// access is a repository and what the caller may do with it.
type access struct {
	id                 string
	path               string
	pull, push, manage bool
}

// userArg is the caller's user ID as a query argument: NULL for a worker.
func userArg(c caller) any {
	if c.UserID == "" {
		return nil
	}
	return c.UserID
}

// open finds a repository and the caller's rights over it. A worker pulls
// every repository. With create, a repository that does not exist is made
// if the caller may push to its namespace.
func (reg *Registry) open(ctx context.Context, c caller, name string, create bool) (access, error) {
	if !validName.MatchString(name) || len(name) > 255 {
		return access{}, errNameInvalid
	}
	var a access
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		a, err = lookup(ctx, tx, c, name)
		if !errors.Is(err, errNameUnknown) || !create {
			return err
		}
		return reg.createRepo(ctx, tx, c, name)
	})
	if err != nil {
		return access{}, err
	}
	if a.id == "" {
		// Made by this call: read it back with the caller's rights.
		err = reg.db.Transact(ctx, func(tx db.Tx) error {
			a, err = lookup(ctx, tx, c, name)
			return err
		})
	}
	return a, err
}

func lookup(ctx context.Context, tx db.Tx, c caller, name string) (access, error) {
	a := access{path: name}
	err := tx.QueryRow(ctx, `SELECT r.id, `+canPull+`, `+canPush+`, `+canManage+`
		FROM image_repositories r WHERE r.path = $3`, c.Admin, userArg(c), name).Scan(&a.id, &a.pull, &a.push, &a.manage)
	if errors.Is(err, pgx.ErrNoRows) {
		return access{}, errNameUnknown
	}
	if err != nil {
		return access{}, err
	}
	if c.worker != "" {
		a.pull = true
	}
	if !a.pull {
		// A repository the caller may not pull is one they do not know of.
		return access{}, errNameUnknown
	}
	return a, nil
}

// createRepo makes a private repository in a namespace the caller may push
// to: their own, a team's they are a member or admin of, or any as an
// administrator.
func (reg *Registry) createRepo(ctx context.Context, tx db.Tx, c caller, name string) error {
	if c.UserID == "" {
		return errDenied
	}
	ns, _, _ := strings.Cut(name, "/")
	var kind, id string
	var role *string
	err := tx.QueryRow(ctx, `SELECT 'user', id::text, NULL FROM users WHERE lower(username) = $1 AND kind = 'person'
		UNION ALL
		SELECT 'team', t.id::text, (SELECT role FROM team_members WHERE team_id = t.id AND user_id = $2)
		FROM teams t WHERE lower(t.slug) = $1`, ns, c.UserID).Scan(&kind, &id, &role)
	if errors.Is(err, pgx.ErrNoRows) {
		return fmt.Errorf("%w: no user or team is named %s", errDenied, ns)
	}
	if err != nil {
		return err
	}
	var owner, team any
	switch {
	case kind == "user" && (id == c.UserID || c.Admin):
		owner = id
	case kind == "team" && (c.Admin || role != nil && (*role == "member" || *role == "admin")):
		team = id
	default:
		return fmt.Errorf("%w: you may not push to %s", errDenied, ns)
	}
	tag, err := tx.Exec(ctx, `INSERT INTO image_repositories (path, owner_id, team_id) VALUES ($1, $2, $3)
		ON CONFLICT (path) DO NOTHING`, name, owner, team)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return nil
	}
	return audit.Record(ctx, tx, audit.Event{Action: "image_repository.create",
		Target: audit.Ref{Type: audit.KindImage, ID: name, Name: name}})
}

// actor is the audit actor for a caller.
func actor(ctx context.Context, c caller) context.Context {
	if c.worker != "" {
		return audit.WithActor(ctx, audit.Worker(c.worker, ""))
	}
	return audit.WithActor(ctx, audit.User(c.UserID, c.Username))
}
