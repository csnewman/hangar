package registry

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	"github.com/distribution/reference"
	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/users"
)

var (
	ErrNotFound = errors.New("repository not found")
	ErrInvalid  = errors.New("invalid")
	// ErrForbidden is returned for a change to a repository the caller can
	// see but may not make.
	ErrForbidden = errors.New("forbidden")
)

// Person is a user as others see them.
type Person struct {
	ID          string
	Username    string
	DisplayName string
}

// TeamRef names a team.
type TeamRef struct {
	ID   string
	Slug string
	Name string
}

// Repository is one repository, as a particular caller sees it.
type Repository struct {
	ID string
	// Path is the repository's name in the registry: <namespace>/<name>.
	Path        string
	Description string
	Visibility  string
	// Owner and Team: exactly one is set.
	Owner             *Person
	Team              *TeamRef
	Collaborators     []Person
	CollaboratorTeams []TeamRef
	TagCount          int
	// Tags is filled in by Get only.
	Tags      []Tag
	CreatedAt time.Time
	UpdatedAt time.Time
	// CanPush is whether the caller may push to it, and choose its
	// collaborators; CanManage whether they may also change its visibility
	// or delete it.
	CanPush   bool
	CanManage bool
}

// Tag is one of a repository's tags.
type Tag struct {
	Name   string
	Digest string
	// MediaType is the tagged manifest's: an image, or an index of them.
	MediaType string
	// Size is the image's config and layers, compressed; for an index, the
	// largest of its images.
	Size      int64
	UpdatedAt time.Time
}

const repoColumns = `r.id, r.path, r.description, r.visibility, r.created_at, r.updated_at,
	coalesce(u.id::text, ''), coalesce(u.username, ''), coalesce(u.display_name, ''),
	coalesce(tm.id::text, ''), coalesce(tm.slug, ''), coalesce(tm.name, ''),
	(SELECT count(*) FROM image_tags t WHERE t.repository_id = r.id),
	` + canPush + `, ` + canManage

const repoFrom = `image_repositories r LEFT JOIN users u ON u.id = r.owner_id LEFT JOIN teams tm ON tm.id = r.team_id`

func scanRepo(row pgx.Row) (Repository, error) {
	var x Repository
	var owner Person
	var team TeamRef
	err := row.Scan(&x.ID, &x.Path, &x.Description, &x.Visibility, &x.CreatedAt, &x.UpdatedAt,
		&owner.ID, &owner.Username, &owner.DisplayName, &team.ID, &team.Slug, &team.Name,
		&x.TagCount, &x.CanPush, &x.CanManage)
	if errors.Is(err, pgx.ErrNoRows) {
		return x, ErrNotFound
	}
	if err != nil {
		return x, err
	}
	if owner.ID != "" {
		x.Owner = &owner
	}
	if team.ID != "" {
		x.Team = &team
	}
	return x, nil
}

// List returns every repository p may pull, by path.
func (reg *Registry) List(ctx context.Context, p users.Principal) ([]Repository, error) {
	var out []Repository
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT `+repoColumns+` FROM `+repoFrom+` WHERE `+canPull+` ORDER BY r.path`,
			p.Admin, p.UserID)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (Repository, error) { return scanRepo(r) })
		if err != nil {
			return err
		}
		return repoCollaborators(ctx, tx, out)
	})
	if out == nil {
		out = []Repository{}
	}
	return out, err
}

// Get returns a repository p may pull, with its tags.
func (reg *Registry) Get(ctx context.Context, p users.Principal, id string) (Repository, error) {
	if !db.ValidUUID(id) {
		return Repository{}, ErrNotFound
	}
	var x Repository
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		x, err = getRepo(ctx, tx, p, id)
		return err
	})
	return x, err
}

func getRepo(ctx context.Context, tx db.Tx, p users.Principal, id string) (Repository, error) {
	x, err := scanRepo(tx.QueryRow(ctx, `SELECT `+repoColumns+` FROM `+repoFrom+` WHERE `+canPull+` AND r.id = $3`,
		p.Admin, p.UserID, id))
	if err != nil {
		return x, err
	}
	xs := []Repository{x}
	if err := repoCollaborators(ctx, tx, xs); err != nil {
		return x, err
	}
	x = xs[0]
	x.Tags, err = repoTags(ctx, tx, id)
	return x, err
}

// repoTags lists a repository's tags, newest first, with each image's size.
func repoTags(ctx context.Context, tx db.Tx, id string) ([]Tag, error) {
	rows, err := tx.Query(ctx, `SELECT t.tag, t.digest, m.media_type, m.content, t.updated_at
		FROM image_tags t JOIN image_manifests m ON m.repository_id = t.repository_id AND m.digest = t.digest
		WHERE t.repository_id = $1 ORDER BY t.updated_at DESC, t.tag`, id)
	if err != nil {
		return nil, err
	}
	type raw struct {
		tag     Tag
		content []byte
	}
	var list []raw
	for rows.Next() {
		var x raw
		if err := rows.Scan(&x.tag.Name, &x.tag.Digest, &x.tag.MediaType, &x.content, &x.tag.UpdatedAt); err != nil {
			rows.Close()
			return nil, err
		}
		list = append(list, x)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return nil, err
	}
	tags := make([]Tag, len(list))
	for i, x := range list {
		tags[i] = x.tag
		size, err := imageSize(ctx, tx, id, x.content)
		if err != nil {
			return nil, err
		}
		tags[i].Size = size
	}
	return tags, nil
}

// imageSize is what a manifest's image takes: its config and layers, or for
// an index the largest of the images it lists.
func imageSize(ctx context.Context, tx db.Tx, repo string, content []byte) (int64, error) {
	var m manifest
	if err := json.Unmarshal(content, &m); err != nil {
		return 0, nil
	}
	if m.Config != nil {
		size := m.Config.Size
		for _, l := range m.Layers {
			size += l.Size
		}
		return size, nil
	}
	var largest int64
	for _, x := range m.Manifests {
		var child []byte
		err := tx.QueryRow(ctx, `SELECT content FROM image_manifests WHERE repository_id = $1 AND digest = $2`,
			repo, x.Digest).Scan(&child)
		if errors.Is(err, pgx.ErrNoRows) {
			continue
		}
		if err != nil {
			return 0, err
		}
		var cm manifest
		if json.Unmarshal(child, &cm) != nil || cm.Config == nil {
			continue
		}
		size := cm.Config.Size
		for _, l := range cm.Layers {
			size += l.Size
		}
		largest = max(largest, size)
	}
	return largest, nil
}

func repoCollaborators(ctx context.Context, tx db.Tx, xs []Repository) error {
	if len(xs) == 0 {
		return nil
	}
	ids := make([]string, len(xs))
	byID := map[string]*Repository{}
	for i := range xs {
		ids[i] = xs[i].ID
		xs[i].Collaborators = []Person{}
		xs[i].CollaboratorTeams = []TeamRef{}
		byID[xs[i].ID] = &xs[i]
	}
	rows, err := tx.Query(ctx, `SELECT c.repository_id, u.id, u.username, u.display_name
		FROM image_repository_collaborators c JOIN users u ON u.id = c.user_id
		WHERE c.repository_id = ANY($1::uuid[]) ORDER BY lower(u.username)`, ids)
	if err != nil {
		return err
	}
	for rows.Next() {
		var rid string
		var p Person
		if err := rows.Scan(&rid, &p.ID, &p.Username, &p.DisplayName); err != nil {
			rows.Close()
			return err
		}
		byID[rid].Collaborators = append(byID[rid].Collaborators, p)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return err
	}
	rows, err = tx.Query(ctx, `SELECT c.repository_id, tm.id, tm.slug, tm.name
		FROM image_repository_team_collaborators c JOIN teams tm ON tm.id = c.team_id
		WHERE c.repository_id = ANY($1::uuid[]) ORDER BY lower(tm.name)`, ids)
	if err != nil {
		return err
	}
	defer rows.Close()
	for rows.Next() {
		var rid string
		var r TeamRef
		if err := rows.Scan(&rid, &r.ID, &r.Slug, &r.Name); err != nil {
			return err
		}
		byID[rid].CollaboratorTeams = append(byID[rid].CollaboratorTeams, r)
	}
	return rows.Err()
}

func repoRef(x Repository) audit.Ref {
	return audit.Ref{Type: audit.KindImage, ID: x.Path, Name: x.Path}
}

// lockRepo locks a repository p may pull and returns it with p's rights.
func lockRepo(ctx context.Context, tx db.Tx, p users.Principal, id string) (Repository, error) {
	if !db.ValidUUID(id) {
		return Repository{}, ErrNotFound
	}
	if _, err := tx.Exec(ctx, `SELECT 1 FROM image_repositories WHERE id = $1 FOR UPDATE`, id); err != nil {
		return Repository{}, err
	}
	return getRepo(ctx, tx, p, id)
}

// Update changes a repository's visibility and description. Its owner may:
// the user, or the team's admins.
func (reg *Registry) Update(ctx context.Context, p users.Principal, id, visibility, description string) (Repository, error) {
	if visibility != "private" && visibility != "shared" {
		return Repository{}, fmt.Errorf("%w: visibility must be private or shared", ErrInvalid)
	}
	description = strings.TrimSpace(description)
	if len(description) > 2000 {
		return Repository{}, fmt.Errorf("%w: a description is at most 2000 characters", ErrInvalid)
	}
	var x Repository
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockRepo(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanManage {
			return fmt.Errorf("%w: only the repository's owner may change it", ErrForbidden)
		}
		if _, err := tx.Exec(ctx, `UPDATE image_repositories SET visibility = $2, description = $3, updated_at = now()
			WHERE id = $1`, id, visibility, description); err != nil {
			return err
		}
		if x, err = getRepo(ctx, tx, p, id); err != nil {
			return err
		}
		details := map[string]any{}
		if cur.Visibility != visibility {
			details["visibility"] = map[string]string{"from": cur.Visibility, "to": visibility}
		}
		return audit.Record(ctx, tx, audit.Event{Action: "image_repository.update", Target: repoRef(x), Details: details})
	})
	return x, err
}

// SetCollaborators replaces the users and teams who, besides the owner, may
// work with a repository: users push to it, a team's viewers pull from it
// and its members and admins push. Anyone who may push may choose them.
func (reg *Registry) SetCollaborators(ctx context.Context, p users.Principal, id string, userIDs, teamIDs []string) (Repository, error) {
	for _, x := range append(slices.Clone(userIDs), teamIDs...) {
		if !db.ValidUUID(x) {
			return Repository{}, fmt.Errorf("%w: unknown user or team %q", ErrInvalid, x)
		}
	}
	var x Repository
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockRepo(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanPush {
			return fmt.Errorf("%w: only those who may push to the repository choose its collaborators", ErrForbidden)
		}
		wantUsers := slices.DeleteFunc(slices.Clone(userIDs), func(u string) bool { return cur.Owner != nil && u == cur.Owner.ID })
		slices.Sort(wantUsers)
		wantUsers = slices.Compact(wantUsers)
		wantTeams := slices.DeleteFunc(slices.Clone(teamIDs), func(t string) bool { return cur.Team != nil && t == cur.Team.ID })
		slices.Sort(wantTeams)
		wantTeams = slices.Compact(wantTeams)
		var known int
		if err := tx.QueryRow(ctx, `SELECT count(*) FROM users WHERE id = ANY($1::uuid[]) AND kind = 'person'`,
			wantUsers).Scan(&known); err != nil {
			return err
		}
		if known != len(wantUsers) {
			return fmt.Errorf("%w: unknown user among collaborators", ErrInvalid)
		}
		if err := tx.QueryRow(ctx, `SELECT count(*) FROM teams WHERE id = ANY($1::uuid[])`, wantTeams).Scan(&known); err != nil {
			return err
		}
		if known != len(wantTeams) {
			return fmt.Errorf("%w: unknown team among collaborators", ErrInvalid)
		}
		for _, q := range []string{
			`DELETE FROM image_repository_collaborators WHERE repository_id = $1`,
			`DELETE FROM image_repository_team_collaborators WHERE repository_id = $1`,
		} {
			if _, err := tx.Exec(ctx, q, id); err != nil {
				return err
			}
		}
		if _, err := tx.Exec(ctx, `INSERT INTO image_repository_collaborators (repository_id, user_id)
			SELECT $1, unnest($2::uuid[])`, id, wantUsers); err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `INSERT INTO image_repository_team_collaborators (repository_id, team_id)
			SELECT $1, unnest($2::uuid[])`, id, wantTeams); err != nil {
			return err
		}
		x, err = getRepo(ctx, tx, p, id)
		if errors.Is(err, ErrNotFound) {
			// A collaborator who takes themselves off a private repository
			// cannot see it afterwards. The change stands.
			x, err = getRepo(ctx, tx, users.Principal{Admin: true}, id)
			x.CanPush, x.CanManage = false, false
		}
		if err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "image_repository.set_collaborators", Target: repoRef(x),
			Details: map[string]any{"collaborators": wantUsers, "teams": wantTeams}})
	})
	return x, err
}

// DeleteTag removes a tag. Anyone who may push may.
func (reg *Registry) DeleteTag(ctx context.Context, p users.Principal, id, tag string) (Repository, error) {
	var x Repository
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockRepo(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanPush {
			return fmt.Errorf("%w: only those who may push to the repository delete its tags", ErrForbidden)
		}
		res, err := tx.Exec(ctx, `DELETE FROM image_tags WHERE repository_id = $1 AND tag = $2`, id, tag)
		if err != nil {
			return err
		}
		if res.RowsAffected() == 0 {
			return fmt.Errorf("%w: no tag %s", ErrInvalid, tag)
		}
		if x, err = getRepo(ctx, tx, p, id); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "image.delete", Target: repoRef(x),
			Details: map[string]any{"tag": tag}})
	})
	return x, err
}

// CheckPull fails with ErrNotFound unless p may pull the repository an
// image reference names, when it names one in this registry. A reference to
// any other registry is not this registry's to judge.
func (reg *Registry) CheckPull(ctx context.Context, p users.Principal, ref string) error {
	named, err := reference.ParseNormalizedNamed(ref)
	if err != nil || !strings.EqualFold(reference.Domain(named), reg.host) {
		return nil
	}
	path := reference.Path(named)
	return reg.db.Transact(ctx, func(tx db.Tx) error {
		_, err := lookup(ctx, tx, caller{Principal: p}, path)
		if errors.Is(err, errNameUnknown) {
			return fmt.Errorf("%w: no repository %s you may pull", ErrNotFound, path)
		}
		return err
	})
}

// Delete removes a repository, its tags and manifests. Its owner may.
func (reg *Registry) Delete(ctx context.Context, p users.Principal, id string) error {
	return reg.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockRepo(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanManage {
			return fmt.Errorf("%w: only the repository's owner may delete it", ErrForbidden)
		}
		if _, err := tx.Exec(ctx, `DELETE FROM image_repositories WHERE id = $1`, id); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "image_repository.delete", Target: repoRef(cur)})
	})
}
