// Package templates manages templates: recipes for environments, so nobody
// starts from an empty machine.
//
// Anyone may make a template. It is private -- seen by its owner and its
// collaborators -- or shared with everyone. Collaborators work on it as the
// owner does: they edit the recipe and add or remove collaborators, which is
// how a team keeps one template between them. Two things stay with the owner
// (and administrators): whether everyone may see it, and deleting it.
//
// An environment copies its template's spec when it is created, resolved for
// that environment (see Resolve). Editing or deleting a template afterwards
// changes nothing already made from it.
package templates

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/users"
)

var (
	ErrNotFound = errors.New("template not found")
	ErrConflict = errors.New("conflict")
	ErrInvalid  = errors.New("invalid")
	// ErrForbidden is returned for a template the caller can see but may
	// not change. One they cannot see is ErrNotFound.
	ErrForbidden = errors.New("forbidden")
)

// Visibility is who may see and use a template.
type Visibility string

const (
	Private Visibility = "private"
	Shared  Visibility = "shared"
)

// Person is a user as others see them.
type Person struct {
	ID          string
	Username    string
	DisplayName string
}

// Template is one template, as a particular caller sees it.
type Template struct {
	ID            string
	Name          string
	Description   string
	Visibility    Visibility
	Owner         Person
	Collaborators []Person
	Spec          api.TemplateSpec
	// Environments is how many environments were made from it and still
	// exist.
	Environments int
	CreatedAt    time.Time
	UpdatedAt    time.Time
	// CanEdit is whether the caller may change the recipe and its
	// collaborators; CanManage whether they may also change its visibility or
	// delete it.
	CanEdit   bool
	CanManage bool
}

// Input is a template's editable content.
type Input struct {
	Name        string
	Description string
	Visibility  Visibility
	Spec        api.TemplateSpec
}

type Manager struct {
	db *db.DB
}

func NewManager(d *db.DB) *Manager { return &Manager{db: d} }

// The conditions below take the caller as $1 (admin) and $2 (user ID).
const (
	isCollaborator = `EXISTS (SELECT 1 FROM template_collaborators c WHERE c.template_id = t.id AND c.user_id = $2)`
	canSee         = `(t.visibility = 'shared' OR t.owner_id = $2 OR $1 OR ` + isCollaborator + `)`
	canEdit        = `(t.owner_id = $2 OR $1 OR ` + isCollaborator + `)`
	canManage      = `(t.owner_id = $2 OR $1)`
)

const columns = `t.id, t.name, t.description, t.visibility, t.spec, t.created_at, t.updated_at,
	u.id, u.username, u.display_name,
	(SELECT count(*) FROM environments e WHERE e.template_id = t.id),
	` + canEdit + `, ` + canManage

const from = `templates t JOIN users u ON u.id = t.owner_id`

func scan(row pgx.Row) (Template, error) {
	var t Template
	var spec []byte
	err := row.Scan(&t.ID, &t.Name, &t.Description, &t.Visibility, &spec, &t.CreatedAt, &t.UpdatedAt,
		&t.Owner.ID, &t.Owner.Username, &t.Owner.DisplayName, &t.Environments, &t.CanEdit, &t.CanManage)
	if errors.Is(err, pgx.ErrNoRows) {
		return t, ErrNotFound
	}
	if err != nil {
		return t, err
	}
	return t, json.Unmarshal(spec, &t.Spec)
}

// collaborators fills in each template's collaborators.
func collaborators(ctx context.Context, tx db.Tx, ts []Template) error {
	if len(ts) == 0 {
		return nil
	}
	ids := make([]string, len(ts))
	byID := map[string]*Template{}
	for i := range ts {
		ids[i] = ts[i].ID
		ts[i].Collaborators = []Person{}
		byID[ts[i].ID] = &ts[i]
	}
	rows, err := tx.Query(ctx, `SELECT c.template_id, u.id, u.username, u.display_name
		FROM template_collaborators c JOIN users u ON u.id = c.user_id
		WHERE c.template_id = ANY($1::uuid[]) ORDER BY lower(u.username)`, ids)
	if err != nil {
		return err
	}
	defer rows.Close()
	for rows.Next() {
		var tid string
		var p Person
		if err := rows.Scan(&tid, &p.ID, &p.Username, &p.DisplayName); err != nil {
			return err
		}
		byID[tid].Collaborators = append(byID[tid].Collaborators, p)
	}
	return rows.Err()
}

// List returns every template p may see, by name.
func (m *Manager) List(ctx context.Context, p users.Principal) ([]Template, error) {
	var out []Template
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT `+columns+` FROM `+from+` WHERE `+canSee+`
			ORDER BY lower(t.name), u.username`, p.Admin, p.UserID)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (Template, error) { return scan(r) })
		if err != nil {
			return err
		}
		return collaborators(ctx, tx, out)
	})
	if out == nil {
		out = []Template{}
	}
	return out, err
}

func (m *Manager) Get(ctx context.Context, p users.Principal, id string) (Template, error) {
	if !db.ValidUUID(id) {
		return Template{}, ErrNotFound
	}
	var t Template
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		t, err = get(ctx, tx, p, id)
		return err
	})
	return t, err
}

func get(ctx context.Context, tx db.Tx, p users.Principal, id string) (Template, error) {
	t, err := scan(tx.QueryRow(ctx, `SELECT `+columns+` FROM `+from+` WHERE `+canSee+` AND t.id = $3`,
		p.Admin, p.UserID, id))
	if err != nil {
		return t, err
	}
	ts := []Template{t}
	if err := collaborators(ctx, tx, ts); err != nil {
		return t, err
	}
	return ts[0], nil
}

// Create makes a template owned by p.
func (m *Manager) Create(ctx context.Context, p users.Principal, in Input) (Template, error) {
	in, err := normalise(in)
	if err != nil {
		return Template{}, err
	}
	spec, err := json.Marshal(in.Spec)
	if err != nil {
		return Template{}, err
	}
	var t Template
	err = m.db.Transact(ctx, func(tx db.Tx) error {
		var id string
		err := tx.QueryRow(ctx, `INSERT INTO templates (owner_id, name, description, visibility, spec)
			VALUES ($1, $2, $3, $4, $5) RETURNING id`, p.UserID, in.Name, in.Description, in.Visibility, spec).Scan(&id)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: you already have a template named %s", ErrConflict, in.Name)
		}
		if err != nil {
			return err
		}
		if t, err = get(ctx, tx, p, id); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "template.create", Target: templateRef(t),
			Related: []audit.Ref{{Type: audit.KindOwner, ID: t.Owner.ID}, {Type: audit.KindImage, ID: t.Spec.Image}},
			Details: map[string]any{"visibility": t.Visibility}})
	})
	return t, err
}

func templateRef(t Template) audit.Ref {
	return audit.Ref{Type: audit.KindTemplate, ID: t.ID, Name: t.Name}
}

// Update replaces a template's content. A collaborator may change the recipe
// but not its visibility, which is the owner's to decide.
func (m *Manager) Update(ctx context.Context, p users.Principal, id string, in Input) (Template, error) {
	if !db.ValidUUID(id) {
		return Template{}, ErrNotFound
	}
	in, err := normalise(in)
	if err != nil {
		return Template{}, err
	}
	spec, err := json.Marshal(in.Spec)
	if err != nil {
		return Template{}, err
	}
	var t Template
	err = m.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockForChange(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanEdit {
			return fmt.Errorf("%w: only the owner and collaborators may edit this template", ErrForbidden)
		}
		if in.Visibility != cur.Visibility && !cur.CanManage {
			return fmt.Errorf("%w: only the owner may change who can see this template", ErrForbidden)
		}
		_, err = tx.Exec(ctx, `UPDATE templates SET name = $2, description = $3, visibility = $4, spec = $5,
			updated_at = now() WHERE id = $1`, id, in.Name, in.Description, in.Visibility, spec)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: the owner already has a template named %s", ErrConflict, in.Name)
		}
		if err != nil {
			return err
		}
		if t, err = get(ctx, tx, p, id); err != nil {
			return err
		}
		details := map[string]any{}
		if cur.Name != t.Name {
			details["name"] = map[string]string{"from": cur.Name, "to": t.Name}
		}
		if cur.Visibility != t.Visibility {
			details["visibility"] = map[string]string{"from": string(cur.Visibility), "to": string(t.Visibility)}
		}
		if cur.Spec.Image != t.Spec.Image {
			details["image"] = map[string]string{"from": cur.Spec.Image, "to": t.Spec.Image}
		}
		if cur.Spec.Untrusted != t.Spec.Untrusted {
			details["untrusted"] = t.Spec.Untrusted
		}
		return audit.Record(ctx, tx, audit.Event{Action: "template.update", Target: templateRef(t),
			Related: []audit.Ref{{Type: audit.KindOwner, ID: t.Owner.ID}, {Type: audit.KindImage, ID: t.Spec.Image}},
			Details: details})
	})
	return t, err
}

// lockForChange locks a template the caller can see and returns it with the
// caller's rights over it.
func lockForChange(ctx context.Context, tx db.Tx, p users.Principal, id string) (Template, error) {
	if _, err := tx.Exec(ctx, `SELECT 1 FROM templates WHERE id = $1 FOR UPDATE`, id); err != nil {
		return Template{}, err
	}
	return get(ctx, tx, p, id)
}

// Delete removes a template. Environments made from it keep their copy of
// its spec and its name.
func (m *Manager) Delete(ctx context.Context, p users.Principal, id string) error {
	if !db.ValidUUID(id) {
		return ErrNotFound
	}
	return m.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockForChange(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanManage {
			return fmt.Errorf("%w: only the owner may delete this template", ErrForbidden)
		}
		if _, err = tx.Exec(ctx, `DELETE FROM templates WHERE id = $1`, id); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "template.delete", Target: templateRef(cur),
			Related: []audit.Ref{{Type: audit.KindOwner, ID: cur.Owner.ID}}})
	})
}

// SetCollaborators replaces who, besides the owner, may edit a template. Any
// editor may do it, so a team can bring people in without its owner.
func (m *Manager) SetCollaborators(ctx context.Context, p users.Principal, id string, userIDs []string) (Template, error) {
	if !db.ValidUUID(id) {
		return Template{}, ErrNotFound
	}
	for _, u := range userIDs {
		if !db.ValidUUID(u) {
			return Template{}, fmt.Errorf("%w: unknown user %q", ErrInvalid, u)
		}
	}
	var t Template
	err := m.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockForChange(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanEdit {
			return fmt.Errorf("%w: only the owner and collaborators may choose collaborators", ErrForbidden)
		}
		// The owner is never a collaborator: they have every right already.
		want := slices.DeleteFunc(slices.Clone(userIDs), func(u string) bool { return u == cur.Owner.ID })
		slices.Sort(want)
		want = slices.Compact(want)
		var known int
		if err := tx.QueryRow(ctx, `SELECT count(*) FROM users WHERE id = ANY($1::uuid[]) AND kind = 'person'`, want).Scan(&known); err != nil {
			return err
		}
		if known != len(want) {
			return fmt.Errorf("%w: unknown user among collaborators", ErrInvalid)
		}
		if _, err := tx.Exec(ctx, `DELETE FROM template_collaborators WHERE template_id = $1`, id); err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `INSERT INTO template_collaborators (template_id, user_id)
			SELECT $1, unnest($2::uuid[])`, id, want); err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `UPDATE templates SET updated_at = now() WHERE id = $1`, id); err != nil {
			return err
		}
		t, err = get(ctx, tx, p, id)
		if errors.Is(err, ErrNotFound) {
			// A collaborator who took themselves off a private template can
			// no longer see it. The change stands; they are shown what it
			// left, with no rights over it.
			t, err = get(ctx, tx, users.Principal{UserID: cur.Owner.ID, Admin: true}, id)
			t.CanEdit, t.CanManage = false, false
		}
		if err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "template.set_collaborators", Target: templateRef(t),
			Related: []audit.Ref{{Type: audit.KindOwner, ID: t.Owner.ID}},
			Details: map[string]any{"collaborators": want}})
	})
	return t, err
}

// ForUse reads the template p wants to make an environment from, inside the
// caller's transaction, and returns its name and spec.
func ForUse(ctx context.Context, tx db.Tx, p users.Principal, id string) (string, api.TemplateSpec, error) {
	if !db.ValidUUID(id) {
		return "", api.TemplateSpec{}, ErrNotFound
	}
	var name string
	var raw []byte
	err := tx.QueryRow(ctx, `SELECT t.name, t.spec FROM templates t WHERE `+canSee+` AND t.id = $3`,
		p.Admin, p.UserID, id).Scan(&name, &raw)
	if errors.Is(err, pgx.ErrNoRows) {
		return "", api.TemplateSpec{}, ErrNotFound
	}
	if err != nil {
		return "", api.TemplateSpec{}, err
	}
	var spec api.TemplateSpec
	if err := json.Unmarshal(raw, &spec); err != nil {
		return "", api.TemplateSpec{}, err
	}
	return name, spec, nil
}

// normalise checks a template's content and fills in defaults.
func normalise(in Input) (Input, error) {
	in.Name = strings.TrimSpace(in.Name)
	in.Description = strings.TrimSpace(in.Description)
	if in.Name == "" || len(in.Name) > 100 {
		return in, fmt.Errorf("%w: a template needs a name of at most 100 characters", ErrInvalid)
	}
	switch in.Visibility {
	case Private, Shared:
	case "":
		in.Visibility = Private
	default:
		return in, fmt.Errorf("%w: visibility must be private or shared", ErrInvalid)
	}
	spec, err := Validate(in.Spec)
	in.Spec = spec
	return in, err
}
