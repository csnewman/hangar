package profile_test

import (
	"context"
	"errors"
	"fmt"
	"path/filepath"
	"slices"
	"strings"
	"testing"

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

// The routes an environment's sets give it: the profile's paths under the
// home directory, with the Locks; a pack's where they are; a path two sets
// name, the first's.
func TestRoutesFor(t *testing.T) {
	routes := profile.RoutesFor("/home/dev", []profile.SetPaths{
		{ID: "me", Paths: []string{".gitconfig", ".claude/skills/"}},
		{ID: "pack1", Paths: []string{"/workspace/app/.env", "/home/dev/.gitconfig", "/srv/conf/"}},
		{ID: "pack2", Paths: []string{"/workspace/app/.env", "../bad", "/x/../y"}},
	})
	want := []profile.Route{
		{Path: "/home/dev/.gitconfig", Target: "me/.gitconfig"},
		{Path: "/home/dev/.claude/skills", Target: "me/.claude/skills", Dir: true},
		{Path: "/home/dev/.claude/.oauth_refresh.lock", Target: "me/.claude/.oauth_refresh.lock", Lock: true},
		{Path: "/workspace/app/.env", Target: "pack1/workspace/app/.env"},
		{Path: "/srv/conf", Target: "pack1/srv/conf", Dir: true},
	}
	if !slices.Equal(routes, want) {
		t.Errorf("routes:\n got %+v\nwant %+v", routes, want)
	}
}

// An environment's guest is routed to its owner's profile and the packs its
// template lists that its owner may use, and routed again as they change.
func TestEnvironmentsAreRoutedToTheirSets(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	carol := p.user(t, "carol")
	team := p.team(t, "web", map[string]string{carol: "admin"})
	own, err := p.store.CreatePack(ctx, as(p.owner), profile.PackInput{Name: "app",
		Paths: []string{"/workspace/app/.env"}})
	if err != nil {
		t.Fatal(err)
	}
	theirs, err := p.store.CreatePack(ctx, as(carol), profile.PackInput{Name: "secret", TeamID: team,
		Paths: []string{"/workspace/app/secret"}})
	if err != nil {
		t.Fatal(err)
	}
	set, err := p.store.PackSet(ctx, as(p.owner), own.ID, false)
	if err != nil {
		t.Fatal(err)
	}

	id, home, _ := p.envSpec(t, "a", fmt.Sprintf(`{"file_packs": [%q, %q]}`, own.ID, theirs.ID), api.PhaseRunning)
	has := func(guestPath, target string) bool {
		return slices.ContainsFunc(p.routesOf(id), func(r profile.Route) bool {
			return r.Path == guestPath && r.Target == target
		})
	}
	eventually(t, "the profile and the pack to be routed", func() bool {
		return has(filepath.Join(home, ".gitconfig"), p.owner+"/.gitconfig") &&
			has("/workspace/app/.env", set+"/workspace/app/.env")
	})
	if slices.ContainsFunc(p.routesOf(id), func(r profile.Route) bool { return r.Path == "/workspace/app/secret" }) {
		t.Error("routed to a pack its owner may not use")
	}

	// A path the pack gains is routed too.
	if _, err := p.store.UpdatePack(ctx, as(p.owner), own.ID, profile.PackInput{Name: "app",
		Paths: []string{"/workspace/app/.env", "/workspace/app/config/"}}); err != nil {
		t.Fatal(err)
	}
	eventually(t, "a pack's new path to be routed", func() bool {
		return slices.Contains(p.routesOf(id), profile.Route{Path: "/workspace/app/config",
			Target: set + "/workspace/app/config", Dir: true})
	})
}
