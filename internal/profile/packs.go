package profile

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

// ErrForbidden is a change the caller may see but not make.
var ErrForbidden = errors.New("forbidden")

// MaxPackPaths is the most paths one pack names.
const MaxPackPaths = 32

// Pack is a named set of files at absolute paths, which templates give the
// environments made from them: the .env files of a project's checkouts,
// say. A person or a team owns it. A shared pack is one copy of its files
// for everyone who uses it; a personal one is each person's own copy.
//
// Whoever may use a pack -- its owner, or any member of its team -- has it
// in the environments they own whose template lists it, and may change
// their own copy of a personal pack's files. Changing the pack itself, or a
// shared pack's files, is for its owner and its team's members and admins;
// deleting it, for its owner and its team's admins.
type Pack struct {
	ID          string
	Name        string
	Description string
	// OwnerID and Owner are the person who owns it, if one does; TeamID and
	// Team (its slug) the team that does, if one does.
	OwnerID, Owner string
	TeamID, Team   string
	Personal       bool
	// Paths are absolute; a directory's ends in a slash.
	Paths []string
	// What the caller may do: change the pack, its files, and delete it.
	CanChange, CanWrite, CanDelete bool
	CreatedAt, UpdatedAt           time.Time
}

// PackInput is a pack as it is made or changed. Its owner and whether it is
// personal are set when it is made: changing either would leave its files'
// copies belonging to the wrong people.
type PackInput struct {
	Name, Description string
	// TeamID makes a team the owner; without one, the caller is.
	TeamID   string
	Personal bool
	Paths    []string
}

// The conditions below take the caller as $1 (admin) and $2 (user ID), on a
// pack k.
const (
	packRole = `(SELECT m.role FROM team_members m WHERE m.team_id = k.team_id AND m.user_id = $2)`
	packUse  = `($1 OR k.owner_id = $2 OR ` + packRole + ` IS NOT NULL)`
	packEdit = `($1 OR k.owner_id = $2 OR ` + packRole + ` IN ('member', 'admin'))`
	packDrop = `($1 OR k.owner_id = $2 OR ` + packRole + ` = 'admin')`

	packColumns = `k.id::text, k.name, k.description, coalesce(k.owner_id::text, ''), coalesce(u.username, ''),
		coalesce(k.team_id::text, ''), coalesce(t.slug, ''), k.personal,
		coalesce((SELECT array_agg(path ORDER BY path) FROM file_pack_paths WHERE pack_id = k.id), '{}'),
		coalesce(` + packEdit + `, false), coalesce(` + packDrop + `, false), k.created_at, k.updated_at`
	packFrom = `file_packs k LEFT JOIN users u ON u.id = k.owner_id LEFT JOIN teams t ON t.id = k.team_id`
)

func scanPack(row pgx.Row) (Pack, error) {
	var k Pack
	err := row.Scan(&k.ID, &k.Name, &k.Description, &k.OwnerID, &k.Owner, &k.TeamID, &k.Team, &k.Personal,
		&k.Paths, &k.CanChange, &k.CanDelete, &k.CreatedAt, &k.UpdatedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return k, ErrNotFound
	}
	k.CanWrite = k.CanChange || k.Personal
	return k, err
}

func packRef(k Pack) audit.Ref { return audit.Ref{Type: audit.KindPack, ID: k.ID, Name: k.Name} }

// Packs returns the packs p may use, by name.
func (s *Store) Packs(ctx context.Context, p users.Principal) ([]Pack, error) {
	out := []Pack{}
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT `+packColumns+` FROM `+packFrom+` WHERE `+packUse+`
			ORDER BY lower(k.name), k.id`, p.Admin, p.UserID)
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

func getPack(ctx context.Context, tx db.Tx, p users.Principal, id string) (Pack, error) {
	return scanPack(tx.QueryRow(ctx, `SELECT `+packColumns+` FROM `+packFrom+` WHERE `+packUse+` AND k.id = $3`,
		p.Admin, p.UserID, id))
}

// checkPack checks a pack's name and paths, returning them as kept.
func checkPack(in PackInput) (PackInput, error) {
	in.Name = strings.TrimSpace(in.Name)
	in.Description = strings.TrimSpace(in.Description)
	if in.Name == "" || len(in.Name) > 100 {
		return in, fmt.Errorf("%w: a pack needs a name of at most 100 characters", ErrInvalid)
	}
	if len(in.Paths) > MaxPackPaths {
		return in, fmt.Errorf("%w: a pack names at most %d paths", ErrInvalid, MaxPackPaths)
	}
	paths := Paths{}
	for _, p := range in.Paths {
		p = strings.TrimSpace(p)
		if err := CheckPackPath(p); err != nil {
			return in, fmt.Errorf("%w: %v", ErrInvalid, err)
		}
		if slices.Contains(paths, p) {
			continue
		}
		paths = append(paths, p)
	}
	for _, p := range paths {
		for _, by := range paths {
			if by != p && strings.HasSuffix(by, "/") && strings.HasPrefix(p, by) {
				return in, fmt.Errorf("%w: %s is already in the pack, by %s", ErrInvalid, p, by)
			}
		}
	}
	slices.Sort(paths)
	in.Paths = paths
	return in, nil
}

// CreatePack makes a pack, owned by the team it names, which the caller
// must be a member or admin of, or else by the caller.
func (s *Store) CreatePack(ctx context.Context, p users.Principal, in PackInput) (Pack, error) {
	in, err := checkPack(in)
	if err != nil {
		return Pack{}, err
	}
	var k Pack
	err = s.db.Transact(ctx, func(tx db.Tx) error {
		var owner, team *string
		if in.TeamID != "" {
			if !db.ValidUUID(in.TeamID) {
				return fmt.Errorf("%w: no such team", ErrInvalid)
			}
			var role string
			err := tx.QueryRow(ctx, `SELECT coalesce((SELECT role FROM team_members WHERE team_id = t.id AND user_id = $2), '')
				FROM teams t WHERE t.id = $1`, in.TeamID, p.UserID).Scan(&role)
			if errors.Is(err, pgx.ErrNoRows) {
				return fmt.Errorf("%w: no such team", ErrInvalid)
			}
			if err != nil {
				return err
			}
			if !p.Admin && role != "member" && role != "admin" {
				return fmt.Errorf("%w: only the team's members and admins make its packs", ErrForbidden)
			}
			team = &in.TeamID
		} else {
			owner = &p.UserID
		}
		var id string
		err := tx.QueryRow(ctx, `INSERT INTO file_packs (name, description, owner_id, team_id, personal)
			VALUES ($1, $2, $3, $4, $5) RETURNING id::text`, in.Name, in.Description, owner, team, in.Personal).Scan(&id)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: its owner already has a pack named %s", ErrInvalid, in.Name)
		}
		if err != nil {
			return err
		}
		if err := setPackPaths(ctx, tx, id, in.Paths); err != nil {
			return err
		}
		if k, err = getPack(ctx, tx, p, id); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "file_pack.create", Target: packRef(k),
			Details: map[string]any{"paths": k.Paths, "personal": k.Personal}})
	})
	return k, err
}

func setPackPaths(ctx context.Context, tx db.Tx, id string, paths []string) error {
	if _, err := tx.Exec(ctx, `DELETE FROM file_pack_paths WHERE pack_id = $1`, id); err != nil {
		return err
	}
	_, err := tx.Exec(ctx, `INSERT INTO file_pack_paths (pack_id, path) SELECT $1, unnest($2::text[])`, id, paths)
	return err
}

// UpdatePack changes a pack's name, description and paths. The copies of
// files under a path it no longer names are dropped; environments keep
// theirs, as files of their own.
func (s *Store) UpdatePack(ctx context.Context, p users.Principal, id string, in PackInput) (Pack, error) {
	if !db.ValidUUID(id) {
		return Pack{}, ErrNotFound
	}
	in, err := checkPack(in)
	if err != nil {
		return Pack{}, err
	}
	var sets []string
	var k Pack
	err = s.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockPack(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanChange {
			return fmt.Errorf("%w: only the pack's owner, or its team's members, change it", ErrForbidden)
		}
		_, err = tx.Exec(ctx, `UPDATE file_packs SET name = $2, description = $3, updated_at = now() WHERE id = $1`,
			id, in.Name, in.Description)
		if db.IsUniqueViolation(err) {
			return fmt.Errorf("%w: its owner already has a pack named %s", ErrInvalid, in.Name)
		}
		if err != nil {
			return err
		}
		if err := setPackPaths(ctx, tx, id, in.Paths); err != nil {
			return err
		}
		rows, err := tx.Query(ctx, `SELECT id::text FROM file_sets WHERE pack_id = $1`, id)
		if err != nil {
			return err
		}
		if sets, err = pgx.CollectRows(rows, pgx.RowTo[string]); err != nil {
			return err
		}
		if k, err = getPack(ctx, tx, p, id); err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "file_pack.update", Target: packRef(k),
			Details: map[string]any{"paths": k.Paths}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, "")
	})
	if err == nil {
		for _, set := range sets {
			s.dropUnshared(set, Paths(in.Paths), true)
		}
	}
	return k, err
}

// lockPack locks a pack p may use and returns it.
func lockPack(ctx context.Context, tx db.Tx, p users.Principal, id string) (Pack, error) {
	if _, err := tx.Exec(ctx, `SELECT 1 FROM file_packs WHERE id = $1 FOR UPDATE`, id); err != nil {
		return Pack{}, err
	}
	return getPack(ctx, tx, p, id)
}

// DeletePack deletes a pack and every copy of its files.
func (s *Store) DeletePack(ctx context.Context, p users.Principal, id string) error {
	if !db.ValidUUID(id) {
		return ErrNotFound
	}
	var sets []string
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := lockPack(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanDelete {
			return fmt.Errorf("%w: only the pack's owner, or its team's admins, delete it", ErrForbidden)
		}
		rows, err := tx.Query(ctx, `SELECT id::text FROM file_sets WHERE pack_id = $1`, id)
		if err != nil {
			return err
		}
		if sets, err = pgx.CollectRows(rows, pgx.RowTo[string]); err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `DELETE FROM file_packs WHERE id = $1`, id); err != nil {
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
	return s.DeleteSets(context.WithoutCancel(ctx), sets...)
}

// PackSet returns the file set holding the copy of a pack's files p works
// with: the pack's one copy, or, for a personal pack, p's own, made if p
// has none yet. For write, p must be allowed to change those files.
func (s *Store) PackSet(ctx context.Context, p users.Principal, id string, write bool) (string, error) {
	if !db.ValidUUID(id) {
		return "", ErrNotFound
	}
	var set string
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		k, err := getPack(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if write && !k.CanWrite {
			return fmt.Errorf("%w: only the pack's owner, or its team's members, change its files", ErrForbidden)
		}
		set, err = packSet(ctx, tx, k.ID, k.Personal, p.UserID)
		return err
	})
	return set, err
}

// packSet is the set of a pack's files for user, made if it is not yet.
func packSet(ctx context.Context, tx db.Tx, pack string, personal bool, user string) (string, error) {
	var owner *string
	if personal {
		owner = &user
	}
	var set string
	for range 2 {
		err := tx.QueryRow(ctx, `SELECT id::text FROM file_sets WHERE pack_id = $1 AND user_id IS NOT DISTINCT FROM $2`,
			pack, owner).Scan(&set)
		if !errors.Is(err, pgx.ErrNoRows) {
			return set, err
		}
		if _, err := tx.Exec(ctx, `INSERT INTO file_sets (id, user_id, pack_id) VALUES (gen_random_uuid(), $1, $2)
			ON CONFLICT DO NOTHING`, owner, pack); err != nil {
			return "", err
		}
	}
	return "", errors.New("a pack's file set could not be made")
}

// FileSet is a set of files an environment is kept in step with, and the
// paths it keeps.
type FileSet struct {
	ID    string
	Paths Paths
}

// EnvironmentSets are the file sets an environment of user's, whose
// template lists packs, is kept in step with: user's profile, then each
// pack user may use, in the order listed. A pack user may not use, or
// that is gone, is left out.
func (s *Store) EnvironmentSets(ctx context.Context, user string, packs []string) ([]FileSet, error) {
	profile, err := s.Paths(ctx, user)
	if err != nil {
		return nil, err
	}
	out := []FileSet{{ID: user, Paths: profile}}
	var ids []string
	for _, id := range packs {
		if db.ValidUUID(id) && !slices.Contains(ids, id) {
			ids = append(ids, id)
		}
	}
	if len(ids) == 0 {
		return out, nil
	}
	err = s.db.Transact(ctx, func(tx db.Tx) error {
		out = out[:1]
		p := users.Principal{UserID: user}
		rows, err := tx.Query(ctx, `SELECT `+packColumns+` FROM `+packFrom+` WHERE `+packUse+` AND k.id = ANY($3::uuid[])`,
			p.Admin, p.UserID, ids)
		if err != nil {
			return err
		}
		found, err := pgx.CollectRows(rows, func(r pgx.CollectableRow) (Pack, error) { return scanPack(r) })
		if err != nil {
			return err
		}
		for _, id := range ids {
			i := slices.IndexFunc(found, func(k Pack) bool { return k.ID == id })
			if i < 0 {
				continue
			}
			set, err := packSet(ctx, tx, id, found[i].Personal, user)
			if err != nil {
				return err
			}
			out = append(out, FileSet{ID: set, Paths: Paths(found[i].Paths)})
		}
		return nil
	})
	return out, err
}

// EnvironmentFiles is what of the shared files an environment placed on a
// worker may reach: the sets its owner is given (EnvironmentSets), and for
// an environment not trusted with its owner's credentials, the
// trusted-only files in them, hidden. An environment placed on another
// worker, or none, is not found.
func (s *Store) EnvironmentFiles(ctx context.Context, workerID, envID string) (api.EnvironmentFiles, error) {
	out := api.EnvironmentFiles{Sets: []string{}}
	if !db.ValidUUID(envID) || !db.ValidUUID(workerID) {
		return out, ErrNotFound
	}
	var owner string
	var untrusted bool
	var packs []string
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		var raw []byte
		err := tx.QueryRow(ctx, `SELECT owner_id::text, coalesce((spec->>'untrusted')::boolean, false), spec->'file_packs'
			FROM environments WHERE id = $1 AND worker_id = $2`, envID, workerID).Scan(&owner, &untrusted, &raw)
		if errors.Is(err, pgx.ErrNoRows) {
			return ErrNotFound
		}
		if err != nil {
			return err
		}
		if len(raw) > 0 {
			json.Unmarshal(raw, &packs)
		}
		return nil
	})
	if err != nil {
		return out, err
	}
	sets, err := s.EnvironmentSets(ctx, owner, packs)
	if err != nil {
		return out, err
	}
	for _, set := range sets {
		out.Sets = append(out.Sets, set.ID)
	}
	if !untrusted {
		return out, nil
	}
	out.Hidden = map[string][]string{}
	for i, set := range out.Sets {
		// The first set is the owner's profile; the rest are packs'.
		hidden, err := s.hidden(ctx, set, i > 0)
		if err != nil {
			return out, err
		}
		if len(hidden) > 0 {
			out.Hidden[set] = hidden
		}
	}
	return out, nil
}
