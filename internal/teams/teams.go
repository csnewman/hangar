// Package teams manages teams: groups of users that own templates, as a user
// does, and give each member a role over what they own.
//
// A viewer sees and uses what the team owns, a member also changes it, and
// an admin also runs the team -- its name, description and members -- and
// deletes what it owns. Server administrators make and delete teams, and act
// as a team admin in every team.
//
// Every user may list the teams and their members, so a team can be picked
// as a template's owner or collaborator.
package teams

import (
	"context"
	"errors"
	"fmt"
	"regexp"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/users"
)

var (
	ErrNotFound = errors.New("team not found")
	ErrConflict = errors.New("conflict")
	ErrInvalid  = errors.New("invalid")
	// ErrForbidden is returned for a change the caller may not make.
	ErrForbidden = errors.New("forbidden")
)

// Role is what a member may do with what their team owns.
type Role string

const (
	Viewer Role = "viewer"
	Member Role = "member"
	Admin  Role = "admin"
)

func (r Role) valid() bool { return r == Viewer || r == Member || r == Admin }

// A slug names a team in paths, including image repositories, whose path
// components are lowercase letters and digits, separated by one of . _ -.
var validSlug = regexp.MustCompile(`^[a-z0-9]+(?:[._-][a-z0-9]+)*$`)

// Person is a user as others see them.
type Person struct {
	ID          string
	Username    string
	DisplayName string
}

// MemberOf is one member of a team.
type MemberOf struct {
	Person
	Role Role
}

// Team is one team, as a particular caller sees it.
type Team struct {
	ID          string
	Slug        string
	Name        string
	Description string
	CreatedAt   time.Time
	// Members is filled in by Get only; List gives MemberCount.
	Members     []MemberOf
	MemberCount int
	// Templates and Images are how many templates and image repositories
	// the team owns.
	Templates int
	Images    int
	// Role is the caller's role in the team, empty if they are not in it.
	Role Role
	// CanManage is whether the caller may change the team and its members:
	// a team admin, or a server administrator.
	CanManage bool
}

// Input is a team's editable content. The slug is set when the team is made
// and does not change: paths that name the team would break.
type Input struct {
	Slug        string
	Name        string
	Description string
}

type Manager struct {
	db *db.DB
}

func NewManager(d *db.DB) *Manager { return &Manager{db: d} }

// The conditions below take the caller as $1 (admin) and $2 (user ID).
const (
	myRole    = `(SELECT m.role FROM team_members m WHERE m.team_id = t.id AND m.user_id = $2)`
	canManage = `($1 OR ` + myRole + ` = 'admin')`
)

const columns = `t.id, t.slug, t.name, t.description, t.created_at,
	(SELECT count(*) FROM team_members m WHERE m.team_id = t.id),
	(SELECT count(*) FROM templates x WHERE x.team_id = t.id),
	(SELECT count(*) FROM image_repositories x WHERE x.team_id = t.id),
	coalesce(` + myRole + `, ''), coalesce(` + canManage + `, false)`

func scan(row pgx.Row) (Team, error) {
	var t Team
	err := row.Scan(&t.ID, &t.Slug, &t.Name, &t.Description, &t.CreatedAt, &t.MemberCount, &t.Templates,
		&t.Images, &t.Role, &t.CanManage)
	if errors.Is(err, pgx.ErrNoRows) {
		return t, ErrNotFound
	}
	return t, err
}

// List returns every team, by name.
func (m *Manager) List(ctx context.Context, p users.Principal) ([]Team, error) {
	var out []Team
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT `+columns+` FROM teams t ORDER BY lower(t.name), lower(t.slug)`,
			p.Admin, p.UserID)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (Team, error) { return scan(r) })
		return err
	})
	if out == nil {
		out = []Team{}
	}
	return out, err
}

// Get returns a team and its members.
func (m *Manager) Get(ctx context.Context, p users.Principal, id string) (Team, error) {
	if !db.ValidUUID(id) {
		return Team{}, ErrNotFound
	}
	var t Team
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		t, err = get(ctx, tx, p, id)
		return err
	})
	return t, err
}

func get(ctx context.Context, tx db.Tx, p users.Principal, id string) (Team, error) {
	t, err := scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM teams t WHERE t.id = $3`, p.Admin, p.UserID, id))
	if err != nil {
		return t, err
	}
	rows, err := tx.Query(ctx, `SELECT u.id, u.username, u.display_name, m.role
		FROM team_members m JOIN users u ON u.id = m.user_id
		WHERE m.team_id = $1
		ORDER BY CASE m.role WHEN 'admin' THEN 0 WHEN 'member' THEN 1 ELSE 2 END, lower(u.username)`, id)
	if err != nil {
		return t, err
	}
	t.Members, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (MemberOf, error) {
		var mo MemberOf
		err := r.Scan(&mo.ID, &mo.Username, &mo.DisplayName, &mo.Role)
		return mo, err
	})
	return t, err
}

func teamRef(t Team) audit.Ref {
	return audit.Ref{Type: audit.KindTeam, ID: t.ID, Name: t.Slug}
}

// Create makes a team. Only server administrators may; the team starts with
// no members.
func (m *Manager) Create(ctx context.Context, p users.Principal, in Input) (Team, error) {
	if !p.Admin {
		return Team{}, fmt.Errorf("%w: only administrators make teams", ErrForbidden)
	}
	in.Slug = strings.TrimSpace(in.Slug)
	if len(in.Slug) > 63 || !validSlug.MatchString(in.Slug) {
		return Team{}, fmt.Errorf("%w: a team's slug is lowercase letters and digits, separated by single . _ or -, at most 63 characters", ErrInvalid)
	}
	in, err := normalise(in)
	if err != nil {
		return Team{}, err
	}
	var t Team
	err = m.db.Transact(ctx, func(tx db.Tx) error {
		if err := users.LockNames(ctx, tx); err != nil {
			return err
		}
		var taken bool
		if err := tx.QueryRow(ctx, `SELECT EXISTS (SELECT 1 FROM users WHERE lower(username) = lower($1))`,
			in.Slug).Scan(&taken); err != nil {
			return err
		}
		if taken {
			return fmt.Errorf("%w: %s is a user's username", ErrConflict, in.Slug)
		}
		var id string
		err := tx.QueryRow(ctx, `INSERT INTO teams (slug, name, description) VALUES ($1, $2, $3) RETURNING id`,
			in.Slug, in.Name, in.Description).Scan(&id)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: there is already a team %s", ErrConflict, in.Slug)
		}
		if err != nil {
			return err
		}
		if t, err = get(ctx, tx, p, id); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "team.create", Target: teamRef(t),
			Details: map[string]any{"name": t.Name}})
	})
	return t, err
}

// Update changes a team's name and description. The slug stays.
func (m *Manager) Update(ctx context.Context, p users.Principal, id string, in Input) (Team, error) {
	if !db.ValidUUID(id) {
		return Team{}, ErrNotFound
	}
	in, err := normalise(in)
	if err != nil {
		return Team{}, err
	}
	var t Team
	err = m.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockForChange(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if in.Slug != "" && in.Slug != cur.Slug {
			return fmt.Errorf("%w: a team's slug does not change", ErrInvalid)
		}
		if _, err := tx.Exec(ctx, `UPDATE teams SET name = $2, description = $3 WHERE id = $1`,
			id, in.Name, in.Description); err != nil {
			return err
		}
		if t, err = get(ctx, tx, p, id); err != nil {
			return err
		}
		details := map[string]any{}
		if cur.Name != t.Name {
			details["name"] = map[string]string{"from": cur.Name, "to": t.Name}
		}
		return audit.Record(ctx, tx, audit.Event{Action: "team.update", Target: teamRef(t), Details: details})
	})
	return t, err
}

// Delete removes a team that owns nothing: no templates, image repositories
// or file packs. Only server administrators may.
func (m *Manager) Delete(ctx context.Context, p users.Principal, id string) error {
	if !db.ValidUUID(id) {
		return ErrNotFound
	}
	if !p.Admin {
		return fmt.Errorf("%w: only administrators delete teams", ErrForbidden)
	}
	return m.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockForChange(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if cur.Templates > 0 {
			return fmt.Errorf("%w: the team owns %d templates; move or delete them first", ErrConflict, cur.Templates)
		}
		if cur.Images > 0 {
			return fmt.Errorf("%w: the team owns %d image repositories; delete them first", ErrConflict, cur.Images)
		}
		var packs, copies int
		if err := tx.QueryRow(ctx, `SELECT (SELECT count(*) FROM packs WHERE team_id = $1),
			(SELECT count(*) FROM copies WHERE team_id = $1)`, id).Scan(&packs, &copies); err != nil {
			return err
		}
		// Their files are kept apart from the database, and go when a pack
		// or copy is deleted.
		if packs > 0 {
			return fmt.Errorf("%w: the team owns %d packs; delete them first", ErrConflict, packs)
		}
		if copies > 0 {
			return fmt.Errorf("%w: the team owns %d shared copies; delete them first", ErrConflict, copies)
		}
		if _, err := tx.Exec(ctx, `DELETE FROM teams WHERE id = $1`, id); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "team.delete", Target: teamRef(cur)})
	})
}

// SetMember adds a user to a team with a role, or changes their role.
func (m *Manager) SetMember(ctx context.Context, p users.Principal, id, userID string, role Role) (Team, error) {
	if !db.ValidUUID(id) {
		return Team{}, ErrNotFound
	}
	if !db.ValidUUID(userID) {
		return Team{}, fmt.Errorf("%w: unknown user", ErrInvalid)
	}
	if !role.valid() {
		return Team{}, fmt.Errorf("%w: a role is viewer, member or admin", ErrInvalid)
	}
	var t Team
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockForChange(ctx, tx, p, id)
		if err != nil {
			return err
		}
		var name string
		err = tx.QueryRow(ctx, `SELECT username FROM users WHERE id = $1 AND kind = 'person'`, userID).Scan(&name)
		if errors.Is(err, pgx.ErrNoRows) {
			return fmt.Errorf("%w: unknown user", ErrInvalid)
		}
		if err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `INSERT INTO team_members (team_id, user_id, role) VALUES ($1, $2, $3)
			ON CONFLICT (team_id, user_id) DO UPDATE SET role = excluded.role`, id, userID, role); err != nil {
			return err
		}
		if t, err = get(ctx, tx, p, id); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "team.set_member", Target: teamRef(cur),
			Related: []audit.Ref{{Type: audit.KindUser, ID: userID, Name: name}},
			Details: map[string]any{"role": role}})
	})
	return t, err
}

// RemoveMember takes a user out of a team.
func (m *Manager) RemoveMember(ctx context.Context, p users.Principal, id, userID string) (Team, error) {
	if !db.ValidUUID(id) {
		return Team{}, ErrNotFound
	}
	if !db.ValidUUID(userID) {
		return Team{}, fmt.Errorf("%w: unknown user", ErrInvalid)
	}
	var t Team
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockForChange(ctx, tx, p, id)
		if err != nil {
			return err
		}
		var name string
		err = tx.QueryRow(ctx, `DELETE FROM team_members m USING users u
			WHERE m.team_id = $1 AND m.user_id = $2 AND u.id = m.user_id RETURNING u.username`, id, userID).Scan(&name)
		if errors.Is(err, pgx.ErrNoRows) {
			return fmt.Errorf("%w: not a member of the team", ErrInvalid)
		}
		if err != nil {
			return err
		}
		if t, err = get(ctx, tx, p, id); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "team.remove_member", Target: teamRef(cur),
			Related: []audit.Ref{{Type: audit.KindUser, ID: userID, Name: name}}})
	})
	return t, err
}

// lockForChange locks a team and returns it, failing unless the caller may
// run it.
func lockForChange(ctx context.Context, tx db.Tx, p users.Principal, id string) (Team, error) {
	if _, err := tx.Exec(ctx, `SELECT 1 FROM teams WHERE id = $1 FOR UPDATE`, id); err != nil {
		return Team{}, err
	}
	t, err := get(ctx, tx, p, id)
	if err != nil {
		return t, err
	}
	if !t.CanManage {
		return t, fmt.Errorf("%w: only the team's admins run it", ErrForbidden)
	}
	return t, nil
}

func normalise(in Input) (Input, error) {
	in.Name = strings.TrimSpace(in.Name)
	in.Description = strings.TrimSpace(in.Description)
	if in.Name == "" || len(in.Name) > 100 {
		return in, fmt.Errorf("%w: a team needs a name of at most 100 characters", ErrInvalid)
	}
	if len(in.Description) > 1000 {
		return in, fmt.Errorf("%w: a description is at most 1000 characters", ErrInvalid)
	}
	return in, nil
}
