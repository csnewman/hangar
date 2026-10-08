package packs

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/users"
)

// Copy is one set of a pack's files.
type Copy struct {
	ID     string
	PackID string
	// OwnerID and Owner are the person who owns it, if one does; TeamID and
	// Team (its slug) the team that does, if one does.
	OwnerID, Owner string
	TeamID, Team   string
	// Personal is a person's own copy, one each.
	Personal bool
	Name     string
	// What the caller may do: change its files, and delete it.
	CanWrite, CanDelete bool
	CreatedAt           time.Time
}

// CopyInput is a shared copy as it is made.
type CopyInput struct {
	Name string
	// TeamID makes a team the owner; without one, the caller is.
	TeamID string
}

// The conditions below take the caller as $1 (admin) and $2 (user ID), on
// a copy c. A person's own copy is theirs alone, an admin's included.
const (
	copyRole  = `(SELECT m.role FROM team_members m WHERE m.team_id = c.team_id AND m.user_id = $2)`
	copyUse   = `(CASE WHEN c.personal THEN c.owner_id = $2 ELSE ($1 OR c.owner_id = $2 OR ` + copyRole + ` IS NOT NULL) END)`
	copyWrite = `(CASE WHEN c.personal THEN c.owner_id = $2 ELSE ($1 OR c.owner_id = $2 OR ` + copyRole + ` IN ('member', 'admin')) END)`
	copyDrop  = `(CASE WHEN c.personal THEN c.owner_id = $2 ELSE ($1 OR c.owner_id = $2 OR ` + copyRole + ` = 'admin') END)`

	copyColumns = `c.id::text, c.pack_id::text, coalesce(c.owner_id::text, ''), coalesce(u.username, ''),
		coalesce(c.team_id::text, ''), coalesce(t.slug, ''), c.personal, c.name,
		coalesce(` + copyWrite + `, false), coalesce(` + copyDrop + `, false), c.created_at`
	copyFrom = `copies c LEFT JOIN users u ON u.id = c.owner_id LEFT JOIN teams t ON t.id = c.team_id`
)

func scanCopy(row pgx.Row) (Copy, error) {
	var c Copy
	err := row.Scan(&c.ID, &c.PackID, &c.OwnerID, &c.Owner, &c.TeamID, &c.Team, &c.Personal, &c.Name,
		&c.CanWrite, &c.CanDelete, &c.CreatedAt)
	if errors.Is(err, pgx.ErrNoRows) {
		return c, ErrNotFound
	}
	return c, err
}

func copyRef(c Copy) audit.Ref {
	name := c.Name
	if c.Personal {
		name = c.Owner + "'s"
	}
	return audit.Ref{Type: audit.KindCopy, ID: c.ID, Name: name}
}

func getCopy(ctx context.Context, tx db.Tx, p users.Principal, id string) (Copy, error) {
	return scanCopy(tx.QueryRow(ctx, `SELECT `+copyColumns+` FROM `+copyFrom+` WHERE `+copyUse+` AND c.id = $3`,
		p.Admin, p.UserID, id))
}

// Copy returns one copy p may use.
func (s *Store) Copy(ctx context.Context, p users.Principal, id string) (Copy, error) {
	if !db.ValidUUID(id) {
		return Copy{}, ErrNotFound
	}
	var c Copy
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		var err error
		c, err = getCopy(ctx, tx, p, id)
		return err
	})
	return c, err
}

// Copies returns the copies of a pack p may use: their own first, made if
// they have none yet, then the shared ones by name.
func (s *Store) Copies(ctx context.Context, p users.Principal, pack string) ([]Copy, error) {
	if !db.ValidUUID(pack) {
		return nil, ErrNotFound
	}
	var out []Copy
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := getPack(ctx, tx, p, pack); err != nil {
			return err
		}
		if _, err := personalCopy(ctx, tx, pack, p.UserID); err != nil {
			return err
		}
		rows, err := tx.Query(ctx, `SELECT `+copyColumns+` FROM `+copyFrom+` WHERE `+copyUse+` AND c.pack_id = $3
			ORDER BY NOT c.personal, lower(c.name), c.id`, p.Admin, p.UserID, pack)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (Copy, error) { return scanCopy(r) })
		return err
	})
	return out, err
}

// PersonalCopy returns p's own copy of a pack they may use, made if they
// have none yet.
func (s *Store) PersonalCopy(ctx context.Context, p users.Principal, pack string) (Copy, error) {
	if !db.ValidUUID(pack) {
		return Copy{}, ErrNotFound
	}
	var c Copy
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := getPack(ctx, tx, p, pack); err != nil {
			return err
		}
		id, err := personalCopy(ctx, tx, pack, p.UserID)
		if err != nil {
			return err
		}
		c, err = getCopy(ctx, tx, p, id)
		return err
	})
	return c, err
}

// personalCopy is user's own copy of a pack, made if it is not yet.
func personalCopy(ctx context.Context, tx db.Tx, pack, user string) (string, error) {
	var id string
	for range 2 {
		err := tx.QueryRow(ctx, `SELECT id::text FROM copies WHERE pack_id = $1 AND owner_id = $2 AND personal`,
			pack, user).Scan(&id)
		if !errors.Is(err, pgx.ErrNoRows) {
			return id, err
		}
		if _, err := tx.Exec(ctx, `INSERT INTO copies (pack_id, owner_id, personal) VALUES ($1, $2, true)
			ON CONFLICT DO NOTHING`, pack, user); err != nil {
			return "", err
		}
	}
	return "", errors.New("a personal copy could not be made")
}

// CreateCopy makes a shared copy of a pack p may use, owned by the team it
// names, which p must be a member or admin of, or else by p.
func (s *Store) CreateCopy(ctx context.Context, p users.Principal, pack string, in CopyInput) (Copy, error) {
	if !db.ValidUUID(pack) {
		return Copy{}, ErrNotFound
	}
	in.Name = strings.TrimSpace(in.Name)
	if in.Name == "" || len(in.Name) > 100 {
		return Copy{}, fmt.Errorf("%w: a shared copy needs a name of at most 100 characters", ErrInvalid)
	}
	var c Copy
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		k, err := getPack(ctx, tx, p, pack)
		if err != nil {
			return err
		}
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
		if err := tx.QueryRow(ctx, `INSERT INTO copies (pack_id, owner_id, team_id, name) VALUES ($1, $2, $3, $4)
			RETURNING id::text`, pack, owner, team, in.Name).Scan(&id); err != nil {
			return err
		}
		if c, err = getCopy(ctx, tx, p, id); err != nil {
			return err
		}
		return audit.Record(ctx, tx, audit.Event{Action: "pack_copy.create", Target: copyRef(c),
			Related: []audit.Ref{packRef(k)}})
	})
	return c, err
}

// RenameCopy renames a shared copy p may change.
func (s *Store) RenameCopy(ctx context.Context, p users.Principal, id, name string) (Copy, error) {
	if !db.ValidUUID(id) {
		return Copy{}, ErrNotFound
	}
	name = strings.TrimSpace(name)
	if name == "" || len(name) > 100 {
		return Copy{}, fmt.Errorf("%w: a shared copy needs a name of at most 100 characters", ErrInvalid)
	}
	var c Copy
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := getCopy(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if cur.Personal || !cur.CanWrite {
			return fmt.Errorf("%w: only a shared copy's owner, or its team's members, rename it", ErrForbidden)
		}
		if _, err := tx.Exec(ctx, `UPDATE copies SET name = $2, updated_at = now() WHERE id = $1`, id, name); err != nil {
			return err
		}
		c, err = getCopy(ctx, tx, p, id)
		return err
	})
	return c, err
}

// DeleteCopy deletes a copy p may delete, and its files. Environments that
// used it are given another by the rules that would have chosen it; a
// person who deletes their own is given an empty one when next they need
// it.
func (s *Store) DeleteCopy(ctx context.Context, p users.Principal, id string) error {
	if !db.ValidUUID(id) {
		return ErrNotFound
	}
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		cur, err := getCopy(ctx, tx, p, id)
		if err != nil {
			return err
		}
		if !cur.CanDelete {
			return fmt.Errorf("%w: only a copy's owner, or its team's admins, delete it", ErrForbidden)
		}
		if _, err := tx.Exec(ctx, `DELETE FROM copies WHERE id = $1`, id); err != nil {
			return err
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "pack_copy.delete", Target: copyRef(cur)}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, "")
	})
	if err != nil {
		return err
	}
	return s.RemoveCopies(id)
}

// CopiesOf returns the IDs of the copies a person owns, for removing their
// files once the person is deleted.
func (s *Store) CopiesOf(ctx context.Context, user string) ([]string, error) {
	if !db.ValidUUID(user) {
		return nil, ErrNotFound
	}
	var out []string
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT id::text FROM copies WHERE owner_id = $1`, user)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, pgx.RowTo[string])
		return err
	})
	return out, err
}

// RemoveCopies removes copies' files, once the copies are gone.
func (s *Store) RemoveCopies(ids ...string) error {
	var errs []error
	for _, id := range ids {
		if db.ValidUUID(id) {
			errs = append(errs, s.root.RemoveAll(id))
		}
	}
	return errors.Join(errs...)
}

// SetDefault makes a copy the one p uses of its pack unless an environment
// or its template says otherwise; an empty copy returns them to their own.
func (s *Store) SetDefault(ctx context.Context, p users.Principal, pack, copy string) error {
	if !db.ValidUUID(pack) {
		return ErrNotFound
	}
	return s.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := getPack(ctx, tx, p, pack); err != nil {
			return err
		}
		if copy == "" {
			_, err := tx.Exec(ctx, `DELETE FROM pack_defaults WHERE user_id = $1 AND pack_id = $2`, p.UserID, pack)
			return err
		}
		c, err := getCopy(ctx, tx, users.Principal{UserID: p.UserID}, copy)
		if errors.Is(err, ErrNotFound) || err == nil && c.PackID != pack {
			return fmt.Errorf("%w: not a copy of the pack you may use", ErrInvalid)
		}
		if err != nil {
			return err
		}
		_, err = tx.Exec(ctx, `INSERT INTO pack_defaults (user_id, pack_id, copy_id) VALUES ($1, $2, $3)
			ON CONFLICT (user_id, pack_id) DO UPDATE SET copy_id = EXCLUDED.copy_id`, p.UserID, pack, copy)
		return err
	})
}

// defaultCopy is the copy user uses of a pack by default: one they chose
// that they may still use, or their own.
func defaultCopy(ctx context.Context, tx db.Tx, pack, user string) (string, error) {
	var id string
	err := tx.QueryRow(ctx, `SELECT d.copy_id::text FROM pack_defaults d JOIN copies c ON c.id = d.copy_id
		WHERE d.user_id = $2 AND d.pack_id = $3 AND `+copyUse, false, user, pack).Scan(&id)
	if err == nil {
		return id, nil
	}
	if !errors.Is(err, pgx.ErrNoRows) {
		return "", err
	}
	return personalCopy(ctx, tx, pack, user)
}

// Defaults returns the copies p chose to use of packs by default, by pack,
// among those they may still use.
func (s *Store) Defaults(ctx context.Context, p users.Principal) (map[string]string, error) {
	out := map[string]string{}
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT d.pack_id::text, d.copy_id::text FROM pack_defaults d
			JOIN copies c ON c.id = d.copy_id WHERE d.user_id = $2 AND `+copyUse, false, p.UserID)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var pack, copy string
			if err := rows.Scan(&pack, &copy); err != nil {
				return err
			}
			out[pack] = copy
		}
		return rows.Err()
	})
	return out, err
}
