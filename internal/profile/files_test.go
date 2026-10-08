package profile_test

import (
	"context"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/packs"
	"github.com/csnewman/hangar/internal/profile"
)

// The routes copies give an environment: in the home directory under its
// home, an exclusion as the guest's own, an absolute path where it is; a
// path two copies name, the first's; a relative one, as a server before
// packs sent, under home.
func TestRoutesFor(t *testing.T) {
	routes := profile.RoutesFor("/home/dev", []profile.SetPaths{
		{ID: "me", Paths: []string{"~/.gitconfig", "~/.claude/", "!~/.claude/projects/", ".bashrc"}},
		{ID: "pack1", Paths: []string{"/workspace/app/.env", "/home/dev/.gitconfig", "/srv/conf/"}},
		{ID: "pack2", Paths: []string{"/workspace/app/.env", "../bad", "/x/../y", "~/../x"}},
	})
	want := []profile.Route{
		{Path: "/home/dev/.gitconfig", Target: "me/~/.gitconfig"},
		{Path: "/home/dev/.claude", Target: "me/~/.claude", Dir: true},
		{Path: "/home/dev/.claude/projects", Exclude: true},
		{Path: "/home/dev/.bashrc", Target: "me/.bashrc"},
		{Path: "/workspace/app/.env", Target: "pack1/workspace/app/.env"},
		{Path: "/srv/conf", Target: "pack1/srv/conf", Dir: true},
	}
	if !slices.Equal(routes, want) {
		t.Errorf("routes:\n got %+v\nwant %+v", routes, want)
	}
}

// An environment routes to its owner's copy of the profile, and to the
// copies of the packs its spec lists; a copy chosen on it, or a pack's
// paths changed, reach it as it runs.
func TestEnvironmentsAreRoutedToTheirCopies(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	app, err := p.packs.CreatePack(ctx, as(p.owner), packs.PackInput{Name: "app",
		Paths: []packs.Path{{Path: "/workspace/app/.env"}}})
	if err != nil {
		t.Fatal(err)
	}
	id, home, _ := p.envIn(t, "a", api.Spec{Packs: []api.PackRef{{Pack: app.ID}}}, api.PhaseRunning, t.TempDir())

	var profileCopy, appCopy string
	eventually(t, "the profile and the pack to be routed", func() bool {
		as, err := p.packs.EnvironmentPacks(ctx, id)
		if err != nil || len(as) < 2 {
			return false
		}
		appCopy, profileCopy = as[0].Copy.ID, as[1].Copy.ID
		return p.routed(id, profile.Route{Path: filepath.Join(home, ".claude"), Target: profileCopy + "/~/.claude", Dir: true}) &&
			p.routed(id, profile.Route{Path: filepath.Join(home, ".claude/projects"), Exclude: true}) &&
			p.routed(id, profile.Route{Path: "/workspace/app/.env", Target: appCopy + "/workspace/app/.env"})
	})

	// Another copy, chosen on the environment.
	shared, err := p.packs.CreateCopy(ctx, as(p.owner), app.ID, packs.CopyInput{Name: "staging"})
	if err != nil {
		t.Fatal(err)
	}
	if err := p.packs.ChooseCopy(ctx, as(p.owner), id, app.ID, shared.ID); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the chosen copy to be routed", func() bool {
		return p.routed(id, profile.Route{Path: "/workspace/app/.env", Target: shared.ID + "/workspace/app/.env"})
	})

	// A path the pack gains.
	if _, err := p.packs.UpdatePack(ctx, as(p.owner), app.ID, packs.PackInput{Name: "app",
		Paths: []packs.Path{{Path: "/workspace/app/.env"}, {Path: "/workspace/app/config/"}}}); err != nil {
		t.Fatal(err)
	}
	eventually(t, "a pack's new path to be routed", func() bool {
		return p.routed(id, profile.Route{Path: "/workspace/app/config", Target: shared.ID + "/workspace/app/config", Dir: true})
	})
}

// As paths become shared, a file only the environment has is copied up
// into the copy, and one differing from the copy's is kept aside as a
// conflict, which resolving with the environment's writes over the copy's;
// a path no longer shared is copied down, the environment keeping it.
func TestFilesMoveAsSharingChanges(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	dots, err := p.packs.CreatePack(ctx, as(p.owner), packs.PackInput{Name: "dots", Attach: packs.AttachOwner,
		Paths: []packs.Path{{Path: "~/.toolrc"}, {Path: "~/.tool/"}}})
	if err != nil {
		t.Fatal(err)
	}
	mine, err := p.packs.PersonalCopy(ctx, as(p.owner), dots.ID)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := p.packs.Put(ctx, as(p.owner), mine.ID, "~/.tool/a", []byte("shared"), 0); err != nil {
		t.Fatal(err)
	}
	home := t.TempDir()
	os.WriteFile(filepath.Join(home, ".toolrc"), []byte("local"), 0o644)
	os.MkdirAll(filepath.Join(home, ".tool"), 0o755)
	os.WriteFile(filepath.Join(home, ".tool/a"), []byte("mine"), 0o644)
	id, _, _ := p.envIn(t, "a", api.Spec{}, api.PhaseRunning, home)

	copied := filepath.Join(p.root, mine.ID, "~/.toolrc")
	eventually(t, "the environment's own file to be copied up", func() bool {
		b, err := os.ReadFile(copied)
		return err == nil && string(b) == "local"
	})
	var conflicts []packs.Conflict
	eventually(t, "the differing file to be a conflict", func() bool {
		conflicts, _ = p.packs.Conflicts(ctx, id)
		return len(conflicts) == 1
	})
	if c := conflicts[0]; c.Copy != mine.ID || c.Path != "~/.tool/a" {
		t.Fatalf("the conflict: %+v", c)
	}
	if b, _ := os.ReadFile(filepath.Join(p.root, mine.ID, "~/.tool/a")); string(b) != "shared" {
		t.Errorf("the copy's file was changed by a conflict: %q", b)
	}
	if err := p.packs.ResolveConflict(ctx, as(p.owner), id, mine.ID, "~/.tool/a", packs.ResolveEnvironment); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the environment's file to replace the copy's", func() bool {
		b, _ := os.ReadFile(filepath.Join(p.root, mine.ID, "~/.tool/a"))
		cs, _ := p.packs.Conflicts(ctx, id)
		return string(b) == "mine" && len(cs) == 0
	})

	// Edited in the copy, then no longer shared: the environment keeps the
	// copy's.
	if _, err := p.packs.Put(ctx, as(p.owner), mine.ID, "~/.toolrc", []byte("edited"), 0); err != nil {
		t.Fatal(err)
	}
	if _, err := p.packs.UpdatePack(ctx, as(p.owner), dots.ID, packs.PackInput{Name: "dots", Attach: packs.AttachOwner,
		Paths: []packs.Path{{Path: "~/.tool/"}}}); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the file no longer shared to be copied down", func() bool {
		b, _ := os.ReadFile(filepath.Join(home, ".toolrc"))
		return string(b) == "edited"
	})
}
