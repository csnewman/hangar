package profile_test

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/users"
)

// user makes another user.
func (p *plane) user(t *testing.T, name string) string {
	t.Helper()
	ctx := context.Background()
	var id string
	err := p.d.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `INSERT INTO users (username) VALUES ($1) RETURNING id`, name).Scan(&id)
	})
	if err != nil {
		t.Fatal(err)
	}
	return id
}

// team makes a team with members, by user ID, in the roles given.
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

func as(id string) users.Principal { return users.Principal{UserID: id} }

// A pack's files go to their absolute paths in an environment whose spec
// lists it, once the directory the pack's path is in exists, and what
// changes there goes back to the pack.
func TestPackFilesFollowTheirPaths(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	ws := t.TempDir()
	proj := filepath.Join(ws, "proj")

	pack, err := p.store.CreatePack(ctx, as(p.owner), profile.PackInput{Name: "proj",
		Paths: []string{proj + "/.env", proj + "/secrets/"}})
	if err != nil {
		t.Fatal(err)
	}
	set, err := p.store.PackSet(ctx, as(p.owner), pack.ID, true)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.Put(ctx, set, proj+"/.env", []byte("A=1\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.Put(ctx, set, proj+"/other", []byte("x"), 0); !errors.Is(err, profile.ErrInvalid) {
		t.Errorf("a file outside the pack's paths was taken: %v", err)
	}
	if _, err := p.store.Put(ctx, p.owner, ".gitconfig", []byte("[user]\n"), 0); err != nil {
		t.Fatal(err)
	}

	_, home, g := p.envSpec(t, "a", fmt.Sprintf(`{"file_packs": [%q]}`, pack.ID), api.PhaseRunning)
	p.kernel(g).report(t, ws)
	eventually(t, "the profile to arrive", func() bool { return read(home, ".gitconfig") == "[user]\n" })

	// Nothing is made where the workspace's clone is yet to go.
	time.Sleep(500 * time.Millisecond)
	if _, err := os.Stat(proj); err == nil {
		t.Fatal("the pack made the directory a clone would go to")
	}
	if err := os.Mkdir(proj, 0o755); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the pack's file once its directory exists", func() bool { return read(proj, ".env") == "A=1\n" })
	if st, err := os.Stat(filepath.Join(proj, ".env")); err != nil || st.Mode().Perm() != 0o600 {
		t.Errorf("the pack's file has mode %v, %v", st.Mode().Perm(), err)
	}

	// Changed there, it goes back to the pack; a file the pack does not
	// name stays.
	if err := os.WriteFile(filepath.Join(proj, ".env"), []byte("A=2\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	os.MkdirAll(filepath.Join(proj, "secrets"), 0o755)
	if err := os.WriteFile(filepath.Join(proj, "secrets", "key"), []byte("k"), 0o600); err != nil {
		t.Fatal(err)
	}
	os.WriteFile(filepath.Join(proj, "README"), []byte("r"), 0o644)
	eventually(t, "changes to reach the pack", func() bool {
		env, err1 := p.store.File(ctx, set, proj+"/.env", true)
		key, err2 := p.store.File(ctx, set, proj+"/secrets/key", true)
		return err1 == nil && err2 == nil && string(env.Data) == "A=2\n" && string(key.Data) == "k"
	})
	files, _ := p.store.Files(ctx, set, true)
	for _, f := range files {
		if strings.HasSuffix(f.Path, "README") {
			t.Error("a file outside the pack was stored")
		}
	}
	profileFiles, _ := p.store.Files(ctx, p.owner, true)
	for _, f := range profileFiles {
		if strings.HasPrefix(f.Path, "/") {
			t.Errorf("a pack's file %s was stored in the profile", f.Path)
		}
	}

	// Written from the web UI, it reaches the environment.
	if _, err := p.store.Put(ctx, set, proj+"/secrets/key", []byte("k2"), 0o600); err != nil {
		t.Fatal(err)
	}
	eventually(t, "a change to the pack to arrive", func() bool { return read(proj, "secrets/key") == "k2" })

	// A path the pack no longer names is dropped from it; the
	// environment keeps its file.
	if _, err := p.store.UpdatePack(ctx, as(p.owner), pack.ID, profile.PackInput{Name: "proj",
		Paths: []string{proj + "/.env"}}); err != nil {
		t.Fatal(err)
	}
	if _, err := p.store.File(ctx, set, proj+"/secrets/key", true); !errors.Is(err, profile.ErrNotFound) {
		t.Errorf("a file under a path the pack dropped is still in it: %v", err)
	}
	time.Sleep(500 * time.Millisecond)
	if read(proj, "secrets/key") != "k2" {
		t.Error("the environment lost a file the pack stopped naming")
	}
}

// Who may use, change and delete a pack, and whose copy of its files they
// have.
func TestPackAccess(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	alice := p.owner
	bob := p.user(t, "bob")
	carol := p.user(t, "carol")
	team := p.team(t, "web", map[string]string{alice: "admin", bob: "viewer"})

	if _, err := p.store.CreatePack(ctx, as(bob), profile.PackInput{Name: "x", TeamID: team,
		Paths: []string{"/w/x"}}); !errors.Is(err, profile.ErrForbidden) {
		t.Errorf("a viewer made a team's pack: %v", err)
	}
	for _, bad := range []string{"w/.env", "/proc/x", "/w/../etc/x", "/"} {
		if _, err := p.store.CreatePack(ctx, as(alice), profile.PackInput{Name: "bad",
			Paths: []string{bad}}); !errors.Is(err, profile.ErrInvalid) {
			t.Errorf("a pack took the path %q: %v", bad, err)
		}
	}
	shared, err := p.store.CreatePack(ctx, as(alice), profile.PackInput{Name: "shared", TeamID: team,
		Paths: []string{"/w/.env"}})
	if err != nil {
		t.Fatal(err)
	}
	personal, err := p.store.CreatePack(ctx, as(alice), profile.PackInput{Name: "personal", TeamID: team,
		Personal: true, Paths: []string{"/w/.token"}})
	if err != nil {
		t.Fatal(err)
	}
	own, err := p.store.CreatePack(ctx, as(alice), profile.PackInput{Name: "mine", Paths: []string{"/w/mine"}})
	if err != nil {
		t.Fatal(err)
	}

	// Bob, a viewer, sees and uses the team's, not Alice's own; Carol,
	// in no team, none.
	names := func(u string) string {
		packs, err := p.store.Packs(ctx, as(u))
		if err != nil {
			t.Fatal(err)
		}
		var out []string
		for _, k := range packs {
			out = append(out, k.Name)
		}
		return strings.Join(out, ",")
	}
	if got := names(bob); got != "personal,shared" {
		t.Errorf("bob sees %s", got)
	}
	if got := names(carol); got != "" {
		t.Errorf("carol sees %s", got)
	}
	if _, err := p.store.Pack(ctx, as(carol), shared.ID); !errors.Is(err, profile.ErrNotFound) {
		t.Errorf("carol reached the team's pack: %v", err)
	}
	sets := func(u string) int {
		s, err := p.store.EnvironmentSets(ctx, u, []string{shared.ID, personal.ID, own.ID})
		if err != nil {
			t.Fatal(err)
		}
		return len(s)
	}
	if got := sets(bob); got != 3 {
		t.Errorf("bob's environment keeps %d sets, not his profile and the team's two", got)
	}
	if got := sets(carol); got != 1 {
		t.Errorf("carol's environment keeps %d sets, not only her profile", got)
	}

	// A viewer changes their own copy of a personal pack's files, not a
	// shared pack's, nor the pack.
	if _, err := p.store.PackSet(ctx, as(bob), shared.ID, true); !errors.Is(err, profile.ErrForbidden) {
		t.Errorf("a viewer may write a shared pack's files: %v", err)
	}
	bobs, err := p.store.PackSet(ctx, as(bob), personal.ID, true)
	if err != nil {
		t.Fatal(err)
	}
	alices, err := p.store.PackSet(ctx, as(alice), personal.ID, true)
	if err != nil {
		t.Fatal(err)
	}
	if bobs == alices {
		t.Error("a personal pack has one copy for everyone")
	}
	if _, err := p.store.Put(ctx, bobs, "/w/.token", []byte("bob"), 0o600); err != nil {
		t.Fatal(err)
	}
	if f, err := p.store.File(ctx, alices, "/w/.token", true); !errors.Is(err, profile.ErrNotFound) {
		t.Errorf("alice has bob's copy: %q, %v", f.Data, err)
	}
	if _, err := p.store.UpdatePack(ctx, as(bob), shared.ID, profile.PackInput{Name: "s",
		Paths: []string{"/w/.env"}}); !errors.Is(err, profile.ErrForbidden) {
		t.Errorf("a viewer changed a pack: %v", err)
	}
	if err := p.store.DeletePack(ctx, as(bob), shared.ID); !errors.Is(err, profile.ErrForbidden) {
		t.Errorf("a viewer deleted a pack: %v", err)
	}

	// Deleted, its files go with it.
	if err := p.store.DeletePack(ctx, as(alice), personal.ID); err != nil {
		t.Fatal(err)
	}
	for k := range p.blobs.Versions() {
		if strings.HasPrefix(k, bobs+"/") {
			t.Errorf("%s outlived its pack", k)
		}
	}
	if got := sets(bob); got != 2 {
		t.Errorf("bob's environment keeps %d sets after a pack was deleted", got)
	}
}
