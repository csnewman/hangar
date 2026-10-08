package packs

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/users"
)

// Errors the store gives.
var (
	ErrNotFound  = errors.New("not found")
	ErrInvalid   = errors.New("invalid")
	ErrForbidden = errors.New("forbidden")
)

// Channel is notified, with a copy's ID or nothing, when what environments
// are given changes: a pack's paths, a copy's choice, a pack attached.
const Channel = "packs"

// Attach is which environments a pack reaches without a template listing
// it.
type Attach string

const (
	AttachListed   Attach = "listed"   // none: only templates that list it
	AttachOwner    Attach = "owner"    // its owner's
	AttachTeam     Attach = "team"     // its team's members'
	AttachEveryone Attach = "everyone" // everyone's; an admin's to set
)

// Pack is a named list of paths.
type Pack struct {
	ID          string
	Name        string
	Description string
	// OwnerID and Owner are the person who owns it, if one does; TeamID
	// and Team (its slug) the team that does, if one does. Builtin is the
	// server's, which neither owns.
	OwnerID, Owner string
	TeamID, Team   string
	Builtin        bool
	Attach         Attach
	Paths          []Path
	// What the caller may do: change it, and delete it.
	CanChange, CanDelete bool
	CreatedAt, UpdatedAt time.Time
}

// PackInput is a pack as it is made or changed. Its owner is set when it is
// made.
type PackInput struct {
	Name, Description string
	// TeamID makes a team the owner; without one, the caller is.
	TeamID string
	Attach Attach
	Paths  []Path
}

// Store keeps packs, copies and their files. root is the files root.
type Store struct {
	db   *db.DB
	root *os.Root
}

// NewStore keeps packs in d and copies' files under root, made if it is
// not there.
func NewStore(d *db.DB, root string) (*Store, error) {
	if err := os.MkdirAll(root, 0o755); err != nil {
		return nil, err
	}
	r, err := os.OpenRoot(root)
	if err != nil {
		return nil, err
	}
	return &Store{db: d, root: r}, nil
}

// The conditions below take the caller as $1 (admin) and $2 (user ID), on
// a pack k.
const (
	packRole = `(SELECT m.role FROM team_members m WHERE m.team_id = k.team_id AND m.user_id = $2)`
	packUse  = `($1 OR k.builtin OR k.attach = 'everyone' OR k.owner_id = $2 OR ` + packRole + ` IS NOT NULL)`
	packEdit = `($1 OR (NOT k.builtin AND (k.owner_id = $2 OR ` + packRole + ` IN ('member', 'admin'))))`
	packDrop = `(NOT k.builtin AND ($1 OR k.owner_id = $2 OR ` + packRole + ` = 'admin'))`

	packColumns = `k.id::text, k.name, k.description, coalesce(k.owner_id::text, ''), coalesce(u.username, ''),
		coalesce(k.team_id::text, ''), coalesce(t.slug, ''), k.builtin, k.attach,
		coalesce((SELECT array_agg(path ORDER BY ltrim(path, '!')) FROM pack_paths WHERE pack_id = k.id), '{}'),
		coalesce((SELECT array_agg(sensitive ORDER BY ltrim(path, '!')) FROM pack_paths WHERE pack_id = k.id), '{}'),
		coalesce(` + packEdit + `, false), coalesce(` + packDrop + `, false), k.created_at, k.updated_at`
	packFrom = `packs k LEFT JOIN users u ON u.id = k.owner_id LEFT JOIN teams t ON t.id = k.team_id`
)

func scanPack(row pgx.Row) (Pack, error) {
	var k Pack
	var paths []string
	var sensitive []bool
	err := row.Scan(&k.ID, &k.Name, &k.Description, &k.OwnerID, &k.Owner, &k.TeamID, &k.Team, &k.Builtin,
		&k.Attach, &paths, &sensitive, &k.CanChange, &k.CanDelete, &k.CreatedAt, &k.UpdatedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return k, ErrNotFound
	}
	for i, p := range paths {
		k.Paths = append(k.Paths, Path{Path: p, Sensitive: i < len(sensitive) && sensitive[i]})
	}
	return k, err
}

func packRef(k Pack) audit.Ref { return audit.Ref{Type: audit.KindPack, ID: k.ID, Name: k.Name} }

// Packs returns the packs p may use: the built-in ones first, then by
// name.
func (s *Store) Packs(ctx context.Context, p users.Principal) ([]Pack, error) {
	out := []Pack{}
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT `+packColumns+` FROM `+packFrom+` WHERE `+packUse+`
			ORDER BY NOT k.builtin, lower(k.name), k.id`, p.Admin, p.UserID)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (Pack, error) { return scanPack(r) })
		return err
	})
	return out, err
}

// Pack returns one pack p may use.
func (s *Store) Pack(ctx context.Context, p users.Principal, id string) (Pack, error) {
	if !db.ValidUUID(id) {
		return Pack{}, ErrNotFound
	}
	var k Pack
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		k, err = getPack(ctx, tx, p, id)
		return err
	})
	return k, err
}

// anyPack returns a pack, whoever may use it.
func anyPack(ctx context.Context, tx db.Tx, id string) (Pack, error) {
	return getPack(ctx, tx, users.Principal{Admin: true, UserID: nobody}, id)
}

// nobody is a user ID no user has, for the conditions that take one.
const nobody = "00000000-0000-0000-0000-000000000000"

func getPack(ctx context.Context, tx db.Tx, p users.Principal, id string) (Pack, error) {
	return scanPack(tx.QueryRow(ctx, `SELECT `+packColumns+` FROM `+packFrom+` WHERE `+packUse+` AND k.id = $3`,
		p.Admin, p.UserID, id))
}

// checkPack checks a pack's name, attach rule and paths, returning them as
// kept. team and builtin are what owns it.
func checkPack(p users.Principal, in PackInput, team, builtin bool) (PackInput, error) {
	in.Name = strings.TrimSpace(in.Name)
	in.Description = strings.TrimSpace(in.Description)
	if in.Name == "" || len(in.Name) > 100 {
		return in, fmt.Errorf("%w: a pack needs a name of at most 100 characters", ErrInvalid)
	}
	if in.Attach == "" {
		in.Attach = AttachListed
	}
	switch in.Attach {
	case AttachListed:
	case AttachOwner:
		if team || builtin {
			return in, fmt.Errorf("%w: only a person's pack attaches to its owner's environments", ErrInvalid)
		}
	case AttachTeam:
		if !team {
			return in, fmt.Errorf("%w: only a team's pack attaches to its members' environments", ErrInvalid)
		}
	case AttachEveryone:
		if !p.Admin {
			return in, fmt.Errorf("%w: only an admin attaches a pack to everyone's environments", ErrForbidden)
		}
	default:
		return in, fmt.Errorf("%w: no such attach rule %q", ErrInvalid, in.Attach)
	}
	paths, err := checkPaths(in.Paths)
	if err != nil {
		return in, err
	}
	in.Paths = paths
	return in, nil
}

// CreatePack makes a pack, owned by the team it names, which the caller
// must be a member or admin of, or else by the caller.
func (s *Store) CreatePack(ctx context.Context, p users.Principal, in PackInput) (Pack, error) {
	in, err := checkPack(p, in, in.TeamID != "", false)
	if err != nil {
		return Pack{}, err
	}
	var k Pack
	err = s.db.Transact(ctx, func(tx db.Tx) error {
		var owner, team *string
		if in.TeamID != "" {
			if err := canGive(ctx, tx, p, in.TeamID); err != nil {
				return err
			}
			team = &in.TeamID
		} else {
			owner = &p.UserID
		}
		var id string
		err := tx.QueryRow(ctx, `INSERT INTO packs (name, description, owner_id, team_id, attach)
			VALUES ($1, $2, $3, $4, $5) RETURNING id::text`, in.Name, in.Description, owner, team, in.Attach).Scan(&id)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: its owner already has a pack named %s", ErrInvalid, in.Name)
		}
		if err != nil {
			return err
		}
		if err := setPaths(ctx, tx, id, in.Paths); err != nil {
			return err
		}
		if k, err = getPack(ctx, tx, p, id); err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "file_pack.create", Target: packRef(k),
			Details: map[string]any{"paths": PathsOf(k.Paths), "attach": k.Attach}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, "")
	})
	return k, err
}

// canGive checks p may give a team a pack or a copy: a member or admin of
// it, or an admin.
func canGive(ctx context.Context, tx db.Tx, p users.Principal, team string) error {
	if !db.ValidUUID(team) {
		return fmt.Errorf("%w: no such team", ErrInvalid)
	}
	var role string
	err := tx.QueryRow(ctx, `SELECT coalesce((SELECT role FROM team_members WHERE team_id = t.id AND user_id = $2), '')
		FROM teams t WHERE t.id = $1`, team, p.UserID).Scan(&role)
	if errors.Is(err, pgx.ErrNoRows) {
		return fmt.Errorf("%w: no such team", ErrInvalid)
	}
	if err != nil {
		return err
	}
	if !p.Admin && role != "member" && role != "admin" {
		return fmt.Errorf("%w: only the team's members and admins give it packs and copies", ErrForbidden)
	}
	return nil
}

func setPaths(ctx context.Context, tx db.Tx, id string, paths []Path) error {
	if _, err := tx.Exec(ctx, `DELETE FROM pack_paths WHERE pack_id = $1`, id); err != nil {
		return err
	}
	ps := make([]string, len(paths))
	sens := make([]bool, len(paths))
	for i, p := range paths {
		ps[i], sens[i] = p.Path, p.Sensitive
	}
	_, err := tx.Exec(ctx, `INSERT INTO pack_paths (pack_id, path, sensitive)
		SELECT $1, p, s FROM unnest($2::text[], $3::boolean[]) AS x (p, s)`, id, ps, sens)
	return err
}

// UpdatePack changes a pack's name, description, attach rule and paths.
// Copies keep their files under a path taken out, unshared, until someone
// deletes them; environments copy them down as their own.
func (s *Store) UpdatePack(ctx context.Context, p users.Principal, id string, in PackInput) (Pack, error) {
	if !db.ValidUUID(id) {
		return Pack{}, ErrNotFound
	}
	var k Pack
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := tx.Exec(ctx, `SELECT 1 FROM packs WHERE id = $1 FOR UPDATE`, id); err != nil {
			return err
		}
		cur, err := getPack(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanChange {
			return fmt.Errorf("%w: only the pack's owner, its team's members, or an admin for a built-in one, change it",
				ErrForbidden)
		}
		if in, err = checkPack(p, in, cur.TeamID != "", cur.Builtin); err != nil {
			return err
		}
		if cur.Builtin && in.Attach != AttachEveryone && in.Attach != AttachListed {
			return fmt.Errorf("%w: a built-in pack attaches to everyone's environments or none", ErrInvalid)
		}
		if cur.Attach == AttachEveryone && in.Attach != AttachEveryone && !p.Admin {
			return fmt.Errorf("%w: only an admin stops a pack attaching to everyone's environments", ErrForbidden)
		}
		_, err = tx.Exec(ctx, `UPDATE packs SET name = $2, description = $3, attach = $4, updated_at = now() WHERE id = $1`,
			id, in.Name, in.Description, in.Attach)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: its owner already has a pack named %s", ErrInvalid, in.Name)
		}
		if err != nil {
			return err
		}
		if err := setPaths(ctx, tx, id, in.Paths); err != nil {
			return err
		}
		if k, err = getPack(ctx, tx, p, id); err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "file_pack.update", Target: packRef(k),
			Details: map[string]any{"paths": PathsOf(k.Paths), "attach": k.Attach}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, "")
	})
	return k, err
}

// DeletePack deletes a pack and every copy of its files. A built-in pack
// is not deleted; it is attached to nothing instead.
func (s *Store) DeletePack(ctx context.Context, p users.Principal, id string) error {
	if !db.ValidUUID(id) {
		return ErrNotFound
	}
	var copies []string
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := tx.Exec(ctx, `SELECT 1 FROM packs WHERE id = $1 FOR UPDATE`, id); err != nil {
			return err
		}
		cur, err := getPack(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanDelete {
			return fmt.Errorf("%w: only the pack's owner, or its team's admins, delete it; a built-in one is not deleted",
				ErrForbidden)
		}
		rows, err := tx.Query(ctx, `SELECT id::text FROM copies WHERE pack_id = $1`, id)
		if err != nil {
			return err
		}
		if copies, err = pgx.CollectRows(rows, pgx.RowTo[string]); err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `DELETE FROM packs WHERE id = $1`, id); err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "file_pack.delete", Target: packRef(cur)}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, "")
	})
	if err != nil {
		return err
	}
	return s.RemoveCopies(copies...)
}
