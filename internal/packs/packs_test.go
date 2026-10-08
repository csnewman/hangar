package packs_test

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/blob"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/packs"
	"github.com/csnewman/hangar/internal/users"
)

type plane struct {
	d      *db.DB
	root   string
	store  *packs.Store
	worker string
}

func newPlane(t *testing.T) *plane {
	t.Helper()
	d := dbtest.Open(t)
	root := t.TempDir()
	store, err := packs.NewStore(d, root)
	if err != nil {
		t.Fatal(err)
	}
	p := &plane{d: d, root: root, store: store}
	ctx := context.Background()
	if err := d.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `INSERT INTO workers (name, credential_hash) VALUES ('w', '\x00') RETURNING id`).Scan(&p.worker)
	}); err != nil {
		t.Fatal(err)
	}
	return p
}

func (p *plane) user(t *testing.T, name string) string {
	t.Helper()
	ctx := context.Background()
	var id string
	if err := p.d.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `INSERT INTO users (username) VALUES ($1) RETURNING id`, name).Scan(&id)
	}); err != nil {
		t.Fatal(err)
	}
	return id
}

func (p *plane) team(t *testing.T, slug string, members map[string]string) string {
	t.Helper()
	ctx := context.Background()
	var id string
	err := p.d.Transact(ctx, func(tx db.Tx) error {
		if err := tx.QueryRow(ctx, `INSERT INTO teams (slug, name) VALUES ($1, $1) RETURNING id`, slug).Scan(&id); err != nil {
			return err
		}
		for u, role := range members {
			if _, err := tx.Exec(ctx, `INSERT INTO team_members (team_id, user_id, role) VALUES ($1, $2, $3)`,
				id, u, role); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return id
}

// env makes a running environment of owner's with spec, on the worker.
func (p *plane) env(t *testing.T, owner, name string, spec api.Spec) string {
	t.Helper()
	ctx := context.Background()
	raw, err := json.Marshal(spec)
	if err != nil {
		t.Fatal(err)
	}
	var id string
	if err := p.d.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `INSERT INTO environments (owner_id, name, template_name, spec, image, cpus,
				memory_mib, desired, worker_id, phase)
			VALUES ($1, $2, 't', $3, 'img', 1, 512, 'running', $4, 'running') RETURNING id`,
			owner, name, raw, p.worker).Scan(&id)
	}); err != nil {
		t.Fatal(err)
	}
	return id
}

func as(id string) users.Principal { return users.Principal{UserID: id} }

func admin(id string) users.Principal { return users.Principal{UserID: id, Admin: true} }

func paths(ps ...string) []packs.Path {
	var out []packs.Path
	for _, p := range ps {
		out = append(out, packs.Path{Path: p})
	}
	return out
}

func TestPaths(t *testing.T) {
	for _, ok := range []string{"~/.gitconfig", "~/.claude/", "!~/.claude/projects/", "/workspace/app/.env", "/srv/conf/"} {
		if err := packs.CheckPath(ok); err != nil {
			t.Errorf("%s refused: %v", ok, err)
		}
	}
	for _, bad := range []string{"", ".gitconfig", "~/", "~/../x", "/x/../y", "/", "/~/x", "/proc/x", "~//x", "/a//"} {
		if packs.CheckPath(bad) == nil {
			t.Errorf("%q taken", bad)
		}
	}
	ps := packs.Paths{"~/.claude/", "!~/.claude/projects/", "~/.claude/.credentials.json", "/w/.env"}
	for path, want := range map[string]bool{
		"~/.claude/settings.json":     true,
		"~/.claude/.credentials.json": true,
		"~/.claude/projects/x.jsonl":  false,
		"~/.claude/projects":          false,
		"/w/.env":                     true,
		"/w/other":                    false,
		"~/.gitconfig":                false,
	} {
		if got := ps.Shared(path); got != want {
			t.Errorf("%s shared: %v, want %v", path, got, want)
		}
	}
	if got := ps.Routed(); !slices.Equal(got, packs.Paths{"~/.claude/", "!~/.claude/projects/", "/w/.env"}) {
		t.Errorf("routed: %v", got)
	}
	sens := []packs.Path{{Path: "~/.ssh/", Sensitive: true}, {Path: "~/.npmrc", Sensitive: true}, {Path: "~/.gitconfig"}}
	for path, want := range map[string]bool{"~/.ssh/id": true, "~/.npmrc": true, "~/.gitconfig": false, "~/.sshx": false} {
		if got := packs.Sensitive(sens, path); got != want {
			t.Errorf("%s sensitive: %v, want %v", path, got, want)
		}
	}
}

// The profile is built in: everyone may use it, only an admin changes it,
// no one deletes it, and its credentials are sensitive.
func TestTheProfileIsBuiltIn(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	alice := p.user(t, "alice")
	list, err := p.store.Packs(ctx, as(alice))
	if err != nil || len(list) == 0 || !list[0].Builtin || list[0].Name != "Profile" {
		t.Fatalf("alice's packs: %+v (%v)", list, err)
	}
	profile := list[0]
	if profile.CanChange || profile.CanDelete || profile.Attach != packs.AttachEveryone {
		t.Errorf("the profile as alice sees it: %+v", profile)
	}
	if !packs.Sensitive(profile.Paths, "~/.claude/.credentials.json") || packs.Sensitive(profile.Paths, "~/.gitconfig") {
		t.Error("the profile's credentials are not sensitive, or its settings are")
	}
	if _, err := p.store.UpdatePack(ctx, as(alice), profile.ID, packs.PackInput{Name: "Profile",
		Attach: packs.AttachEveryone, Paths: paths("~/.gitconfig")}); !errors.Is(err, packs.ErrForbidden) {
		t.Errorf("alice changed the profile: %v", err)
	}
	in := packs.PackInput{Name: "Profile", Attach: packs.AttachEveryone,
		Paths: append(profile.Paths, packs.Path{Path: "~/.config/nvim/"})}
	if _, err := p.store.UpdatePack(ctx, admin(alice), profile.ID, in); err != nil {
		t.Errorf("an admin changing the profile: %v", err)
	}
	if err := p.store.DeletePack(ctx, admin(alice), profile.ID); !errors.Is(err, packs.ErrForbidden) {
		t.Errorf("an admin deleted the profile: %v", err)
	}
}

// Who sees, changes and deletes a pack, and what attaches it.
func TestPackAccess(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	alice, bob, carol := p.user(t, "alice"), p.user(t, "bob"), p.user(t, "carol")
	team := p.team(t, "web", map[string]string{alice: "admin", bob: "viewer"})

	for _, bad := range [][]string{{".env"}, {"/proc/x"}, {"/w/../x"}, {"!~/.x/"}, {"~/.a/", "!/b/"}} {
		if _, err := p.store.CreatePack(ctx, as(alice), packs.PackInput{Name: "bad", Paths: paths(bad...)}); !errors.Is(err, packs.ErrInvalid) {
			t.Errorf("a pack took %v: %v", bad, err)
		}
	}
	if _, err := p.store.CreatePack(ctx, as(alice), packs.PackInput{Name: "all", Attach: packs.AttachEveryone}); !errors.Is(err, packs.ErrForbidden) {
		t.Errorf("a user attached a pack to everyone: %v", err)
	}
	if _, err := p.store.CreatePack(ctx, as(alice), packs.PackInput{Name: "t", Attach: packs.AttachTeam}); !errors.Is(err, packs.ErrInvalid) {
		t.Errorf("a person's pack attached to a team: %v", err)
	}
	if _, err := p.store.CreatePack(ctx, as(bob), packs.PackInput{Name: "x", TeamID: team}); !errors.Is(err, packs.ErrForbidden) {
		t.Errorf("a viewer made a team's pack: %v", err)
	}
	shared, err := p.store.CreatePack(ctx, as(alice), packs.PackInput{Name: "web", TeamID: team, Attach: packs.AttachTeam,
		Paths: paths("/w/.env")})
	if err != nil {
		t.Fatal(err)
	}
	own, err := p.store.CreatePack(ctx, as(alice), packs.PackInput{Name: "mine", Paths: paths("~/.toolrc")})
	if err != nil {
		t.Fatal(err)
	}
	names := func(u string) string {
		ks, err := p.store.Packs(ctx, as(u))
		if err != nil {
			t.Fatal(err)
		}
		var out []string
		for _, k := range ks {
			out = append(out, k.Name)
		}
		return strings.Join(out, ",")
	}
	if got := names(bob); got != "Profile,web" {
		t.Errorf("bob sees %s", got)
	}
	if got := names(carol); got != "Profile" {
		t.Errorf("carol sees %s", got)
	}
	if _, err := p.store.UpdatePack(ctx, as(bob), shared.ID, packs.PackInput{Name: "web", Paths: paths("/w/x")}); !errors.Is(err, packs.ErrForbidden) {
		t.Errorf("a viewer changed a pack: %v", err)
	}
	if _, err := p.store.Pack(ctx, as(bob), own.ID); !errors.Is(err, packs.ErrNotFound) {
		t.Errorf("bob reached alice's own pack: %v", err)
	}
	if err := p.store.DeletePack(ctx, as(bob), shared.ID); !errors.Is(err, packs.ErrForbidden) {
		t.Errorf("a viewer deleted a pack: %v", err)
	}
	if err := p.store.DeletePack(ctx, as(alice), own.ID); err != nil {
		t.Errorf("deleting one's own pack: %v", err)
	}
}

// A person's own copy is theirs alone; a team's shared copy is its
// members', written by its members and admins. Files go where the pack's
// paths say, with sensitive ones made private.
func TestCopies(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	alice, bob := p.user(t, "alice"), p.user(t, "bob")
	team := p.team(t, "web", map[string]string{alice: "member", bob: "viewer"})
	k, err := p.store.CreatePack(ctx, as(alice), packs.PackInput{Name: "web", TeamID: team,
		Paths: []packs.Path{{Path: "/w/.env"}, {Path: "~/.npmrc", Sensitive: true}, {Path: "~/.config/tool/"},
			{Path: "!~/.config/tool/cache/"}}})
	if err != nil {
		t.Fatal(err)
	}
	mine, err := p.store.PersonalCopy(ctx, as(alice), k.ID)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.Copy(ctx, as(bob), mine.ID); !errors.Is(err, packs.ErrNotFound) {
		t.Errorf("bob reached alice's own copy: %v", err)
	}
	if _, err := p.store.Copy(ctx, admin(bob), mine.ID); !errors.Is(err, packs.ErrNotFound) {
		t.Errorf("an admin reached alice's own copy: %v", err)
	}
	staging, err := p.store.CreateCopy(ctx, as(alice), k.ID, packs.CopyInput{Name: "staging", TeamID: team})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.Put(ctx, as(bob), staging.ID, "/w/.env", []byte("x"), 0); !errors.Is(err, packs.ErrForbidden) {
		t.Errorf("a viewer wrote a shared copy: %v", err)
	}
	cs, err := p.store.Copies(ctx, as(bob), k.ID)
	if err != nil || len(cs) != 2 || !cs[0].Personal || cs[1].ID != staging.ID {
		t.Errorf("bob's copies: %+v (%v)", cs, err)
	}

	for key, want := range map[string]uint32{"/w/.env": 0o644, "~/.npmrc": 0o600, "~/.config/tool/conf": 0o644} {
		f, err := p.store.Put(ctx, as(alice), staging.ID, key, []byte("v"), 0)
		if err != nil {
			t.Fatalf("writing %s: %v", key, err)
		}
		if f.Mode != want || f.Sensitive != (key == "~/.npmrc") || !f.Shared {
			t.Errorf("%s stored as %+v", key, f)
		}
	}
	for _, key := range []string{"~/.config/tool/cache/x", "/w/other", "~/.gitconfig"} {
		if _, err := p.store.Put(ctx, as(alice), staging.ID, key, []byte("v"), 0); !errors.Is(err, packs.ErrInvalid) {
			t.Errorf("wrote %s, which the pack does not share: %v", key, err)
		}
	}
	if _, err := os.Stat(filepath.Join(p.root, staging.ID, "~/.npmrc")); err != nil {
		t.Errorf("a home file is not under ~/ in its copy: %v", err)
	}
	if _, err := os.Stat(filepath.Join(p.root, staging.ID, "w/.env")); err != nil {
		t.Errorf("an absolute file is not at its path in its copy: %v", err)
	}

	// A path taken out of the pack: its files stay, unshared.
	if _, err := p.store.UpdatePack(ctx, as(alice), k.ID, packs.PackInput{Name: "web",
		Paths: []packs.Path{{Path: "~/.npmrc", Sensitive: true}, {Path: "~/.config/tool/"}}}); err != nil {
		t.Fatal(err)
	}
	files, err := p.store.Files(ctx, as(alice), staging.ID)
	if err != nil {
		t.Fatal(err)
	}
	i := slices.IndexFunc(files, func(f packs.File) bool { return f.Path == "/w/.env" })
	if i < 0 || files[i].Shared {
		t.Errorf("a file the pack stopped sharing: %+v", files)
	}
	if err := p.store.Delete(ctx, as(alice), staging.ID, "/w/.env"); err != nil {
		t.Errorf("deleting it: %v", err)
	}
}

// Which packs an environment has and which copy of each: the listed ones
// first, then those attaching themselves, the owner's before their teams'
// before everyone's; the copy chosen on it, else the pin, else the one it
// was first given, else the owner's default.
func TestEnvironmentPacks(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	alice := p.user(t, "alice")
	team := p.team(t, "web", map[string]string{alice: "member"})
	listed, err := p.store.CreatePack(ctx, as(alice), packs.PackInput{Name: "listed", Paths: paths("/w/.env")})
	if err != nil {
		t.Fatal(err)
	}
	teams, err := p.store.CreatePack(ctx, as(alice), packs.PackInput{Name: "teams", TeamID: team,
		Attach: packs.AttachTeam, Paths: paths("~/.teamrc")})
	if err != nil {
		t.Fatal(err)
	}
	own, err := p.store.CreatePack(ctx, as(alice), packs.PackInput{Name: "own", Attach: packs.AttachOwner,
		Paths: paths("~/.ownrc")})
	if err != nil {
		t.Fatal(err)
	}
	pin, err := p.store.CreateCopy(ctx, as(alice), listed.ID, packs.CopyInput{Name: "pinned"})
	if err != nil {
		t.Fatal(err)
	}
	env := p.env(t, alice, "a", api.Spec{Packs: []api.PackRef{{Pack: listed.ID, Copy: pin.ID}}})
	as1, err := p.store.EnvironmentPacks(ctx, env)
	if err != nil {
		t.Fatal(err)
	}
	var got []string
	for _, a := range as1 {
		got = append(got, a.Pack.Name+":"+string(a.From))
	}
	if want := []string{"listed:template", "own:default", "teams:default", "Profile:default"}; !slices.Equal(got, want) {
		t.Errorf("the environment's packs: %v, want %v", got, want)
	}
	if as1[0].Copy.ID != pin.ID || !as1[0].Listed || as1[1].Listed {
		t.Errorf("the pinned pack: %+v", as1[0])
	}

	// A default chosen later does not move it: it keeps the copy it was
	// given. A copy chosen on it does.
	other, err := p.store.CreateCopy(ctx, as(alice), own.ID, packs.CopyInput{Name: "other"})
	if err != nil {
		t.Fatal(err)
	}
	if err := p.store.SetDefault(ctx, as(alice), own.ID, other.ID); err != nil {
		t.Fatal(err)
	}
	as2, _ := p.store.EnvironmentPacks(ctx, env)
	if as2[1].Copy.ID == other.ID || as2[1].From != packs.FromKept {
		t.Errorf("a kept copy moved with the default: %+v", as2[1])
	}
	if env2 := p.env(t, alice, "b", api.Spec{}); true {
		as3, _ := p.store.EnvironmentPacks(ctx, env2)
		if i := slices.IndexFunc(as3, func(a packs.Attached) bool { return a.Pack.ID == own.ID }); i < 0 || as3[i].Copy.ID != other.ID {
			t.Errorf("a new environment is not given the default: %+v", as3)
		}
	}
	if err := p.store.ChooseCopy(ctx, as(alice), env, own.ID, other.ID); err != nil {
		t.Fatal(err)
	}
	as4, _ := p.store.EnvironmentPacks(ctx, env)
	if as4[1].Copy.ID != other.ID || as4[1].From != packs.FromEnvironment {
		t.Errorf("the chosen copy: %+v", as4[1])
	}
	if err := p.store.ChooseCopy(ctx, as(p.user(t, "bob")), env, own.ID, ""); !errors.Is(err, packs.ErrForbidden) {
		t.Errorf("another chose an environment's copy: %v", err)
	}

	// Kept from self-attaching packs, it has only the listed one.
	only := p.env(t, alice, "c", api.Spec{Packs: []api.PackRef{{Pack: listed.ID}},
		Access: api.Access{NoSelfAttached: true}})
	as5, _ := p.store.EnvironmentPacks(ctx, only)
	if len(as5) != 1 || as5[0].Pack.ID != listed.ID || as5[0].Copy.ID == pin.ID {
		t.Errorf("an environment kept from self-attaching packs: %+v", as5)
	}
	_ = teams
}

// What a worker serves an environment: its copies, those its agent still
// routes to, and for one given no sensitive files, those hidden.
func TestEnvironmentFiles(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	alice := p.user(t, "alice")
	k, err := p.store.CreatePack(ctx, as(alice), packs.PackInput{Name: "k",
		Paths: []packs.Path{{Path: "~/.ssh/", Sensitive: true}, {Path: "/w/.env", Sensitive: true}, {Path: "~/.toolrc"}}})
	if err != nil {
		t.Fatal(err)
	}
	trusted := p.env(t, alice, "a", api.Spec{Packs: []api.PackRef{{Pack: k.ID}}})
	untrusted := p.env(t, alice, "b", api.Spec{Packs: []api.PackRef{{Pack: k.ID}}, Access: api.Untrusted})
	files, err := p.store.EnvironmentFiles(ctx, p.worker, trusted)
	if err != nil || len(files.Sets) != 2 || len(files.Hidden) != 0 {
		t.Errorf("a trusted environment's files: %+v (%v)", files, err)
	}
	files, err = p.store.EnvironmentFiles(ctx, p.worker, untrusted)
	if err != nil {
		t.Fatal(err)
	}
	mine := files.Sets[0]
	hidden := files.Hidden[mine]
	slices.Sort(hidden)
	if !slices.Equal(hidden, []string{"w/.env", "~/.ssh/"}) {
		t.Errorf("hidden from an untrusted environment: %v", files.Hidden)
	}
	if !slices.Contains(files.Hidden[files.Sets[1]], "~/.claude/.credentials.json") {
		t.Errorf("Claude's sign-in is not hidden: %v", files.Hidden)
	}

	// A copy the agent still routes to is served until it says it does not.
	other, err := p.store.CreateCopy(ctx, as(alice), k.ID, packs.CopyInput{Name: "other"})
	if err != nil {
		t.Fatal(err)
	}
	if err := p.store.Routed(ctx, trusted, []string{other.ID}); err != nil {
		t.Fatal(err)
	}
	files, _ = p.store.EnvironmentFiles(ctx, p.worker, trusted)
	if !slices.Contains(files.Sets, other.ID) {
		t.Errorf("a copy still routed to is not served: %v", files.Sets)
	}
	if err := p.store.Routed(ctx, trusted, nil); err != nil {
		t.Fatal(err)
	}
	files, _ = p.store.EnvironmentFiles(ctx, p.worker, trusted)
	if slices.Contains(files.Sets, other.ID) {
		t.Errorf("a copy the agent stopped routing to is served: %v", files.Sets)
	}
	if _, err := p.store.EnvironmentFiles(ctx, "00000000-0000-0000-0000-000000000000", trusted); !errors.Is(err, packs.ErrNotFound) {
		t.Errorf("another worker's environment: %v", err)
	}
}

// A conflict is recorded, resolved by the environment's owner -- with the
// environment's file only by one who may write the copy -- and forgotten.
func TestConflicts(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	alice := p.user(t, "alice")
	k, err := p.store.CreatePack(ctx, as(alice), packs.PackInput{Name: "k", Paths: paths("~/.toolrc")})
	if err != nil {
		t.Fatal(err)
	}
	c, err := p.store.PersonalCopy(ctx, as(alice), k.ID)
	if err != nil {
		t.Fatal(err)
	}
	env := p.env(t, alice, "a", api.Spec{})
	if err := p.store.AddConflict(ctx, env, c.ID, "~/.toolrc"); err != nil {
		t.Fatal(err)
	}
	cs, err := p.store.Conflicts(ctx, env)
	if err != nil || len(cs) != 1 || cs[0].Resolution != "" {
		t.Fatalf("conflicts: %+v (%v)", cs, err)
	}
	if err := p.store.ResolveConflict(ctx, as(alice), env, c.ID, "~/.toolrc", "maybe"); !errors.Is(err, packs.ErrInvalid) {
		t.Errorf("resolved with nonsense: %v", err)
	}
	if err := p.store.ResolveConflict(ctx, as(p.user(t, "bob")), env, c.ID, "~/.toolrc", packs.ResolveShared); !errors.Is(err, packs.ErrForbidden) {
		t.Errorf("another resolved it: %v", err)
	}
	if err := p.store.ResolveConflict(ctx, as(alice), env, c.ID, "~/.toolrc", packs.ResolveShared); err != nil {
		t.Fatal(err)
	}
	if cs, _ := p.store.Conflicts(ctx, env); len(cs) != 1 || cs[0].Resolution != packs.ResolveShared {
		t.Errorf("after resolving: %+v", cs)
	}
	if err := p.store.ConflictDone(ctx, env, c.ID, "~/.toolrc"); err != nil {
		t.Fatal(err)
	}
	if cs, _ := p.store.Conflicts(ctx, env); len(cs) != 0 {
		t.Errorf("a done conflict remains: %+v", cs)
	}
}

// The files root is brought up to date once: a profile laid out relative
// to the home directory moves under ~/, the blob store's files are copied
// in under ~/ and forgotten, and paths move from one copy to another.
func TestMigrate(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	alice := p.user(t, "alice")
	mineCopy := "11111111-1111-1111-1111-111111111111"
	os.MkdirAll(filepath.Join(p.root, alice, ".claude"), 0o755)
	os.WriteFile(filepath.Join(p.root, alice, ".claude/settings.json"), []byte("{}"), 0o644)
	os.MkdirAll(filepath.Join(p.root, alice, ".config/nvim"), 0o755)
	os.WriteFile(filepath.Join(p.root, alice, ".config/nvim/init.lua"), []byte("--"), 0o644)
	blobs := blob.NewMemory()
	version, err := blobs.PutVersion(ctx, alice+"/.gitconfig", strings.NewReader("[user]\n"), 7)
	if err != nil {
		t.Fatal(err)
	}
	err = p.d.Transact(ctx, func(tx db.Tx) error {
		for _, q := range []string{
			`INSERT INTO copy_relayout (copy_id) VALUES ($1)`,
			`INSERT INTO file_sets (id, user_id) VALUES ($1, $1)`,
			`INSERT INTO set_files (set_id, path, version_id, size, mode, deleted, version)
				VALUES ($1, '.gitconfig', '` + version + `', 7, 384, false, 1)`,
			`INSERT INTO copy_moves (from_copy, to_copy, path) VALUES ($1, '` + mineCopy + `', '~/.config/nvim')`,
		} {
			if _, err := tx.Exec(ctx, q, alice); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if n, err := p.store.Migrate(ctx, blobs); err != nil || n != 4 {
		t.Fatalf("migrated %d (%v), want 4: two moved under ~/, one copied, one moved", n, err)
	}
	for path, want := range map[string]string{
		alice + "/~/.claude/settings.json":    "{}",
		alice + "/~/.gitconfig":               "[user]\n",
		mineCopy + "/~/.config/nvim/init.lua": "--",
	} {
		if b, err := os.ReadFile(filepath.Join(p.root, path)); err != nil || string(b) != want {
			t.Errorf("%s: %q (%v), want %q", path, b, err, want)
		}
	}
	if st, err := os.Stat(filepath.Join(p.root, alice, "~/.gitconfig")); err != nil || st.Mode().Perm() != 0o600 {
		t.Errorf("copied with mode %v (%v)", st.Mode().Perm(), err)
	}
	if n, err := p.store.Migrate(ctx, blobs); err != nil || n != 0 {
		t.Errorf("migrated %d again (%v)", n, err)
	}
}
