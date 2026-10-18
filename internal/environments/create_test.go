package environments_test

import (
	"errors"
	"slices"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
)

// An environment may be made with a machine and repositories of its own in
// place of its template's, held to what a template may ask, and shows them
// as its differences from the template.
func TestCreateWithOwnMachineAndRepos(t *testing.T) {
	d := dbtest.Open(t)
	em, tm := environments.NewManager(d), templates.NewManager(d)
	u, err := users.NewManager(d).Create(ctx, users.NewUser{Username: "owner", Password: "password1"})
	if err != nil {
		t.Fatal(err)
	}
	p := users.Principal{UserID: u.ID, Username: u.Username}
	spec := api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 2, MemoryMiB: 2048, Display: api.DisplayNone,
		Repos:    []api.Repo{{URL: "https://example.com/a.git", Path: "/workspace/a", Branch: "try/{name}"}},
		Projects: []string{"/workspace/a"}}}
	tpl, err := tm.Create(ctx, p, templates.Input{Name: "t", Spec: spec})
	if err != nil {
		t.Fatal(err)
	}

	repos := []api.Repo{{URL: "https://example.com/b.git", Path: "/workspace/b", Branch: "work/{name}"}}
	env, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "e",
		Machine: &api.Machine{CPUs: 4, MemoryMiB: 4096, Display: api.DisplayDesktop, GPU: api.GPUNone},
		Repos:   &repos})
	if err != nil {
		t.Fatal(err)
	}
	if env.CPUs != 4 || env.MemoryMiB != 4096 || env.Spec.Display != api.DisplayDesktop {
		t.Errorf("machine: %d vCPUs, %d MiB, %s", env.CPUs, env.MemoryMiB, env.Spec.Display)
	}
	if len(env.Spec.Repos) != 1 || env.Spec.Repos[0].URL != repos[0].URL || env.Spec.Repos[0].Branch != "work/e" {
		t.Errorf("repos: %+v", env.Spec.Repos)
	}
	if !slices.Equal(env.Spec.Projects, []string{"/workspace/b"}) || env.Spec.EditorPath != "/workspace/b" {
		t.Errorf("projects %q, editor %q, want the repository's", env.Spec.Projects, env.Spec.EditorPath)
	}
	for _, want := range []string{environments.SettingCPUs, environments.SettingMemory, environments.SettingDisplay,
		environments.SettingRepos} {
		if !slices.Contains(env.TemplateChanges, want) {
			t.Errorf("changes %v lack %s", env.TemplateChanges, want)
		}
	}

	none := []api.Repo{}
	bare, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "bare", Repos: &none})
	if err != nil {
		t.Fatal(err)
	}
	if len(bare.Spec.Repos) != 0 || len(bare.Spec.Projects) != 0 || bare.Spec.EditorPath != "" || bare.CPUs != 2 {
		t.Errorf("no repositories: repos %+v, projects %q, %d vCPUs", bare.Spec.Repos, bare.Spec.Projects, bare.CPUs)
	}

	for name, m := range map[string]api.Machine{
		"too many vCPUs":       {CPUs: 999, MemoryMiB: 2048, Display: api.DisplayNone, GPU: api.GPUNone},
		"a GPU passed through": {CPUs: 2, MemoryMiB: 2048, Display: api.DisplayNone, GPU: api.GPUPassthrough},
	} {
		if _, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "x", Machine: &m}); !errors.Is(err, environments.ErrInvalid) {
			t.Errorf("%s: %v, want ErrInvalid", name, err)
		}
	}
	bad := []api.Repo{{URL: "https://example.com/c.git", Path: "relative"}}
	if _, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "y", Repos: &bad}); !errors.Is(err, environments.ErrInvalid) {
		t.Errorf("a relative path: %v, want ErrInvalid", err)
	}
}
