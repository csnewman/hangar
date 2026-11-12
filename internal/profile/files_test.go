package profile_test

import (
	"context"
	"slices"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/packs"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/profile/profiletest"
)

// hasSet reports whether the guest was last sent the copy with paths
// including want.
func hasSet(g *profiletest.Guest, copy string, want ...string) bool {
	sets, _ := g.Sets()
	for _, s := range sets {
		if s.ID == copy && !slices.ContainsFunc(want, func(p string) bool { return !slices.Contains(s.Paths, p) }) {
			return true
		}
	}
	return false
}

// An environment is sent its owner's copy of the profile, and the copies of
// the packs its spec lists, first; a copy chosen on it, or a pack's paths
// changed, reach it as it runs.
func TestEnvironmentsAreSentTheirCopies(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	app, err := p.packs.CreatePack(ctx, as(p.owner), packs.PackInput{Name: "app",
		Paths: []packs.Path{{Path: "/workspace/app/.env"}}})
	if err != nil {
		t.Fatal(err)
	}
	id, g := p.envIn(t, "a", api.Spec{Packs: []api.PackRef{{Pack: app.ID}}}, api.PhaseRunning)

	var profileCopy, appCopy string
	eventually(t, "the profile's and the pack's copies", func() bool {
		as, err := p.packs.EnvironmentPacks(ctx, id)
		if err != nil || len(as) < 2 {
			return false
		}
		appCopy, profileCopy = as[0].Copy.ID, as[1].Copy.ID
		sets, _ := g.Sets()
		return len(sets) == 2 && sets[0].ID == appCopy &&
			hasSet(g, profileCopy, "~/.claude/", "!~/.claude/projects/") &&
			hasSet(g, appCopy, "/workspace/app/.env")
	})

	// Another copy, chosen on the environment.
	shared, err := p.packs.CreateCopy(ctx, as(p.owner), app.ID, packs.CopyInput{Name: "staging"})
	if err != nil {
		t.Fatal(err)
	}
	if err := p.packs.ChooseCopy(ctx, as(p.owner), id, app.ID, shared.ID); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the chosen copy", func() bool { return hasSet(g, shared.ID, "/workspace/app/.env") })

	// A path the pack gains.
	if _, err := p.packs.UpdatePack(ctx, as(p.owner), app.ID, packs.PackInput{Name: "app",
		Paths: []packs.Path{{Path: "/workspace/app/.env"}, {Path: "/workspace/app/config/"}}}); err != nil {
		t.Fatal(err)
	}
	eventually(t, "a pack's new path", func() bool {
		return hasSet(g, shared.ID, "/workspace/app/.env", "/workspace/app/config/")
	})
}

// A conflict the agent finds is listed until it is resolved, the agent
// being told how, and done once the agent says it has.
func TestConflictsAreResolvedThroughTheAgent(t *testing.T) {
	ctx := context.Background()
	p := newPlane(t)
	dots, err := p.packs.CreatePack(ctx, as(p.owner), packs.PackInput{Name: "dots", Attach: packs.AttachOwner,
		Paths: []packs.Path{{Path: "~/.tool/"}}})
	if err != nil {
		t.Fatal(err)
	}
	mine, err := p.packs.PersonalCopy(ctx, as(p.owner), dots.ID)
	if err != nil {
		t.Fatal(err)
	}
	id, g := p.env(t, "a", api.Access{})
	eventually(t, "the copies", func() bool { return hasSet(g, mine.ID, "~/.tool/") })

	if err := g.Send(profile.Message{Type: profile.TypeConflict, Copy: mine.ID, Path: "~/.tool/a"}); err != nil {
		t.Fatal(err)
	}
	var conflicts []packs.Conflict
	eventually(t, "the conflict to be listed", func() bool {
		conflicts, _ = p.packs.Conflicts(ctx, id)
		return len(conflicts) == 1
	})
	if c := conflicts[0]; c.Copy != mine.ID || c.Path != "~/.tool/a" {
		t.Fatalf("the conflict: %+v", c)
	}
	if err := p.packs.ResolveConflict(ctx, as(p.owner), id, mine.ID, "~/.tool/a", packs.ResolveEnvironment); err != nil {
		t.Fatal(err)
	}
	eventually(t, "the agent to be told, and the conflict done", func() bool {
		rs := g.Resolutions()
		cs, _ := p.packs.Conflicts(ctx, id)
		return len(rs) == 1 && rs[0].Path == "~/.tool/a" && rs[0].Resolution == profile.ResolveEnvironment &&
			len(cs) == 0
	})
}
