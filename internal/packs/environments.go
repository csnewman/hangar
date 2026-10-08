package packs

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

// From is how the copy an environment uses of a pack was chosen.
type From string

const (
	FromEnvironment From = "environment" // chosen on the environment
	FromTemplate    From = "template"    // the template's pin
	FromKept        From = "kept"        // the one it was first given
	FromDefault     From = "default"     // its owner's default, or their own
)

// Attached is a pack an environment has and the copy it uses of it.
type Attached struct {
	Pack Pack
	Copy Copy
	From From
	// Listed is the template's listing it, rather than its attaching
	// itself.
	Listed bool
}

// environment is what of an environment the rules read.
type environment struct {
	id, owner string
	spec      api.Spec
}

func getEnvironment(ctx context.Context, tx db.Tx, id, worker string) (environment, error) {
	e := environment{id: id}
	var raw []byte
	q := `SELECT owner_id::text, spec FROM environments WHERE id = $1`
	args := []any{id}
	if worker != "" {
		q += ` AND worker_id = $2`
		args = append(args, worker)
	}
	err := tx.QueryRow(ctx, q, args...).Scan(&e.owner, &raw)
	if errors.Is(err, pgx.ErrNoRows) {
		return e, ErrNotFound
	}
	if err != nil {
		return e, err
	}
	return e, json.Unmarshal(raw, &e.spec)
}

// attached are the packs an environment has, in order, with the copy of
// each it uses: those its template lists, then -- unless it keeps them
// out -- those attaching themselves to it, its owner's, their teams',
// then everyone's. A pack its owner may not use is left out. The copy is
// the one chosen on the environment, else the template's pin, else the
// one it was first given, else its owner's default; the last is kept, so
// it does not change under the environment.
func attached(ctx context.Context, tx db.Tx, e environment) ([]Attached, error) {
	owner := users.Principal{UserID: e.owner}
	var out []Attached
	has := func(id string) bool {
		return slices.ContainsFunc(out, func(a Attached) bool { return a.Pack.ID == id })
	}
	pins := map[string]string{}
	for _, ref := range e.spec.Packs {
		if !db.ValidUUID(ref.Pack) || has(ref.Pack) {
			continue
		}
		k, err := getPack(ctx, tx, owner, ref.Pack)
		if errors.Is(err, ErrNotFound) {
			continue
		}
		if err != nil {
			return nil, err
		}
		pins[k.ID] = ref.Copy
		out = append(out, Attached{Pack: k, Listed: true})
	}
	if !e.spec.NoSelfAttached {
		rows, err := tx.Query(ctx, `SELECT `+packColumns+` FROM `+packFrom+` WHERE `+packUse+` AND (
				(k.attach = 'owner' AND k.owner_id = $2)
				OR (k.attach = 'team' AND `+packRole+` IS NOT NULL)
				OR k.attach = 'everyone')
			ORDER BY CASE k.attach WHEN 'owner' THEN 0 WHEN 'team' THEN 1 ELSE 2 END, k.builtin, lower(k.name), k.id`,
			false, e.owner)
		if err != nil {
			return nil, err
		}
		self, err := pgx.CollectRows(rows, func(r pgx.CollectableRow) (Pack, error) { return scanPack(r) })
		if err != nil {
			return nil, err
		}
		for _, k := range self {
			if !has(k.ID) {
				out = append(out, Attached{Pack: k})
			}
		}
	}

	type kept struct {
		copy   string
		chosen bool
	}
	keeps := map[string]kept{}
	rows, err := tx.Query(ctx, `SELECT pack_id::text, copy_id::text, chosen FROM environment_copies
		WHERE environment_id = $1`, e.id)
	if err != nil {
		return nil, err
	}
	for rows.Next() {
		var pack string
		var k kept
		if err := rows.Scan(&pack, &k.copy, &k.chosen); err != nil {
			rows.Close()
			return nil, err
		}
		keeps[pack] = k
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return nil, err
	}

	usable := func(pack, id string) (Copy, bool, error) {
		if !db.ValidUUID(id) {
			return Copy{}, false, nil
		}
		c, err := getCopy(ctx, tx, owner, id)
		if errors.Is(err, ErrNotFound) || err == nil && c.PackID != pack {
			return Copy{}, false, nil
		}
		return c, err == nil, err
	}
	for i := range out {
		a := &out[i]
		k, hasKept := keeps[a.Pack.ID]
		try := []struct {
			id   string
			from From
			ok   bool
		}{
			{k.copy, FromEnvironment, hasKept && k.chosen},
			{pins[a.Pack.ID], FromTemplate, true},
			{k.copy, FromKept, hasKept && !k.chosen},
		}
		found := false
		for _, t := range try {
			if !t.ok {
				continue
			}
			c, ok, err := usable(a.Pack.ID, t.id)
			if err != nil {
				return nil, err
			}
			if ok {
				a.Copy, a.From, found = c, t.from, true
				break
			}
		}
		if found {
			continue
		}
		id, err := defaultCopy(ctx, tx, a.Pack.ID, e.owner)
		if err != nil {
			return nil, err
		}
		if a.Copy, err = getCopy(ctx, tx, owner, id); err != nil {
			return nil, err
		}
		a.From = FromDefault
		if _, err := tx.Exec(ctx, `INSERT INTO environment_copies (environment_id, pack_id, copy_id)
			VALUES ($1, $2, $3) ON CONFLICT (environment_id, pack_id) DO UPDATE SET copy_id = EXCLUDED.copy_id, chosen = false`,
			e.id, a.Pack.ID, id); err != nil {
			return nil, err
		}
	}
	return out, nil
}

// EnvironmentPacks returns the packs an environment has and the copy it
// uses of each.
func (s *Store) EnvironmentPacks(ctx context.Context, env string) ([]Attached, error) {
	if !db.ValidUUID(env) {
		return nil, ErrNotFound
	}
	var out []Attached
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		e, err := getEnvironment(ctx, tx, env, "")
		if err != nil {
			return err
		}
		out, err = attached(ctx, tx, e)
		return err
	})
	return out, err
}

// ChooseCopy has an environment use a copy of one of its packs, one its
// owner may use; an empty copy returns it to the rules. p must own the
// environment.
func (s *Store) ChooseCopy(ctx context.Context, p users.Principal, env, pack, copy string) error {
	if !db.ValidUUID(env) || !db.ValidUUID(pack) {
		return ErrNotFound
	}
	return s.db.Transact(ctx, func(tx db.Tx) error {
		e, err := getEnvironment(ctx, tx, env, "")
		if err != nil {
			return err
		}
		if e.owner != p.UserID {
			return fmt.Errorf("%w: only the environment's owner chooses its copies", ErrForbidden)
		}
		if copy == "" {
			if _, err := tx.Exec(ctx, `DELETE FROM environment_copies WHERE environment_id = $1 AND pack_id = $2`,
				env, pack); err != nil {
				return err
			}
		} else {
			c, err := getCopy(ctx, tx, users.Principal{UserID: e.owner}, copy)
			if errors.Is(err, ErrNotFound) || err == nil && c.PackID != pack {
				return fmt.Errorf("%w: not a copy of the pack its owner may use", ErrInvalid)
			}
			if err != nil {
				return err
			}
			if _, err := tx.Exec(ctx, `INSERT INTO environment_copies (environment_id, pack_id, copy_id, chosen)
				VALUES ($1, $2, $3, true) ON CONFLICT (environment_id, pack_id)
				DO UPDATE SET copy_id = EXCLUDED.copy_id, chosen = true`, env, pack, copy); err != nil {
				return err
			}
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "environment.choose_copy",
			Target:  audit.Ref{Type: audit.KindEnvironment, ID: env},
			Related: []audit.Ref{{Type: audit.KindPack, ID: pack}, {Type: audit.KindCopy, ID: copy}}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, "")
	})
}

// Set is a copy an environment routes to and the paths it routes there,
// as Paths.Routed has them.
type Set struct {
	ID    string
	Paths Paths
}

// EnvironmentSets are the copies an environment routes to, in order, and
// each one's paths. A path two of them name is the first's.
func (s *Store) EnvironmentSets(ctx context.Context, env string) ([]Set, error) {
	as, err := s.EnvironmentPacks(ctx, env)
	if err != nil {
		return nil, err
	}
	out := make([]Set, 0, len(as))
	for _, a := range as {
		out = append(out, Set{ID: a.Copy.ID, Paths: PathsOf(a.Pack.Paths).Routed()})
	}
	return out, nil
}

// Routed records the copies an environment's agent routes to. Its worker
// goes on serving those beside the ones it has, so the agent can copy a
// copy's files down as it stops routing to it.
func (s *Store) Routed(ctx context.Context, env string, copies []string) error {
	if !db.ValidUUID(env) {
		return ErrNotFound
	}
	var ids []string
	for _, c := range copies {
		if db.ValidUUID(c) && !slices.Contains(ids, c) {
			ids = append(ids, c)
		}
	}
	return s.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := tx.Exec(ctx, `DELETE FROM environment_routed WHERE environment_id = $1`, env); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `INSERT INTO environment_routed (environment_id, copy_id)
			SELECT $1, c.id FROM copies c WHERE c.id = ANY($2::uuid[]) ON CONFLICT DO NOTHING`, env, ids)
		return err
	})
}

// EnvironmentFiles is what of the files root an environment placed on a
// worker may reach: the copies it has and those its agent still routes
// to, and in each, for an environment given no sensitive files, those
// under the pack's sensitive paths, hidden. An environment placed on
// another worker, or none, is not found.
func (s *Store) EnvironmentFiles(ctx context.Context, worker, env string) (api.EnvironmentFiles, error) {
	out := api.EnvironmentFiles{Sets: []string{}}
	if !db.ValidUUID(env) || !db.ValidUUID(worker) {
		return out, ErrNotFound
	}
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		e, err := getEnvironment(ctx, tx, env, worker)
		if err != nil {
			return err
		}
		as, err := attached(ctx, tx, e)
		if err != nil {
			return err
		}
		packOf := map[string][]Path{}
		for _, a := range as {
			if !slices.Contains(out.Sets, a.Copy.ID) {
				out.Sets = append(out.Sets, a.Copy.ID)
				packOf[a.Copy.ID] = a.Pack.Paths
			}
		}
		rows, err := tx.Query(ctx, `SELECT c.id::text, coalesce(array_agg(pp.path) FILTER (WHERE pp.sensitive), '{}')
			FROM environment_routed r JOIN copies c ON c.id = r.copy_id
			LEFT JOIN pack_paths pp ON pp.pack_id = c.pack_id
			WHERE r.environment_id = $1 GROUP BY c.id`, env)
		if err != nil {
			return err
		}
		for rows.Next() {
			var id string
			var sensitive []string
			if err := rows.Scan(&id, &sensitive); err != nil {
				rows.Close()
				return err
			}
			if !slices.Contains(out.Sets, id) {
				out.Sets = append(out.Sets, id)
				for _, p := range sensitive {
					packOf[id] = append(packOf[id], Path{Path: p, Sensitive: true})
				}
			}
		}
		rows.Close()
		if err := rows.Err(); err != nil {
			return err
		}
		if !e.spec.NoSensitiveFiles {
			return nil
		}
		out.Hidden = map[string][]string{}
		for id, paths := range packOf {
			for _, p := range paths {
				if p.Sensitive && !strings.HasPrefix(p.Path, Exclude) {
					out.Hidden[id] = append(out.Hidden[id], strings.TrimPrefix(p.Path, "/"))
				}
			}
		}
		return nil
	})
	return out, err
}

// Conflict is a path an environment had a file of its own at, differing
// from its copy's, when the path became shared.
type Conflict struct {
	Copy    string
	Path    string
	FoundAt time.Time
	// Resolution is what someone chose, until the environment's agent has
	// done it: ResolveShared or ResolveEnvironment.
	Resolution string
}

// What a conflict is resolved with.
const (
	ResolveShared      = "shared"      // the copy's file stays; the environment's is dropped
	ResolveEnvironment = "environment" // the environment's file replaces the copy's
)

// AddConflict records a conflict an environment's agent found.
func (s *Store) AddConflict(ctx context.Context, env, copy, path string) error {
	if !db.ValidUUID(env) || !db.ValidUUID(copy) || !ValidKey(path) {
		return ErrInvalid
	}
	return s.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `INSERT INTO environment_conflicts (environment_id, copy_id, path)
			SELECT $1, id, $3 FROM copies WHERE id = $2 ON CONFLICT DO NOTHING`, env, copy, path)
		return err
	})
}

// Conflicts returns an environment's conflicts, by path.
func (s *Store) Conflicts(ctx context.Context, env string) ([]Conflict, error) {
	if !db.ValidUUID(env) {
		return nil, ErrNotFound
	}
	out := []Conflict{}
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT copy_id::text, path, found_at, coalesce(resolution, '')
			FROM environment_conflicts WHERE environment_id = $1 ORDER BY path, copy_id`, env)
		if err != nil {
			return err
		}
		out, err = pgx.CollectRows(rows, func(r pgx.CollectableRow) (Conflict, error) {
			var c Conflict
			err := r.Scan(&c.Copy, &c.Path, &c.FoundAt, &c.Resolution)
			return c, err
		})
		return err
	})
	return out, err
}

// ResolveConflict records how p, the environment's owner, resolves one of
// its conflicts; its agent does it when it next hears from the server. To
// use the environment's file, p must be able to change the copy's.
func (s *Store) ResolveConflict(ctx context.Context, p users.Principal, env, copy, path, resolution string) error {
	if resolution != ResolveShared && resolution != ResolveEnvironment {
		return fmt.Errorf("%w: resolve with %q or %q", ErrInvalid, ResolveShared, ResolveEnvironment)
	}
	if !db.ValidUUID(env) || !db.ValidUUID(copy) {
		return ErrNotFound
	}
	return s.db.Transact(ctx, func(tx db.Tx) error {
		e, err := getEnvironment(ctx, tx, env, "")
		if err != nil {
			return err
		}
		if e.owner != p.UserID {
			return fmt.Errorf("%w: only the environment's owner resolves its conflicts", ErrForbidden)
		}
		if resolution == ResolveEnvironment {
			c, err := getCopy(ctx, tx, users.Principal{UserID: e.owner}, copy)
			if err != nil {
				return err
			}
			if !c.CanWrite {
				return fmt.Errorf("%w: you may not change the copy's files", ErrForbidden)
			}
		}
		tag, err := tx.Exec(ctx, `UPDATE environment_conflicts SET resolution = $4
			WHERE environment_id = $1 AND copy_id = $2 AND path = $3`, env, copy, path, resolution)
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return ErrNotFound
		}
		if err := audit.Record(ctx, tx, audit.Event{Action: "environment.resolve_conflict",
			Target:  audit.Ref{Type: audit.KindEnvironment, ID: env},
			Related: []audit.Ref{{Type: audit.KindCopy, ID: copy}},
			Details: map[string]any{"path": path, "resolution": resolution}}); err != nil {
			return err
		}
		return db.Notify(ctx, tx, Channel, "")
	})
}

// ConflictDone forgets a conflict the environment's agent has resolved.
func (s *Store) ConflictDone(ctx context.Context, env, copy, path string) error {
	if !db.ValidUUID(env) || !db.ValidUUID(copy) {
		return ErrNotFound
	}
	return s.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `DELETE FROM environment_conflicts WHERE environment_id = $1 AND copy_id = $2 AND path = $3`,
			env, copy, path)
		return err
	})
}
