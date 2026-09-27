package environments_test

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
)

var ctx = context.Background()

// An environment follows its template only as it was when made: editing
// either shows as a difference, which resetting to the template clears,
// keeping the environment.
func TestSettingsAndTemplate(t *testing.T) {
	d := dbtest.Open(t)
	em, tm := environments.NewManager(d), templates.NewManager(d)
	u, err := users.NewManager(d).Create(ctx, users.NewUser{Username: "owner", Password: "password1"})
	if err != nil {
		t.Fatal(err)
	}
	p := users.Principal{UserID: u.ID, Username: u.Username}
	spec := api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 2, MemoryMiB: 2048, Display: api.DisplayNone}}
	tpl, err := tm.Create(ctx, p, templates.Input{Name: "t", Spec: spec})
	if err != nil {
		t.Fatal(err)
	}
	env, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "e"})
	if err != nil {
		t.Fatal(err)
	}
	if len(env.TemplateChanges) != 0 || env.TemplateUpdated {
		t.Fatalf("a new environment: changes %v, updated %v", env.TemplateChanges, env.TemplateUpdated)
	}

	// Not yet placed, it has no machine, and its settings can change.
	got, err := em.UpdateSettings(ctx, p, env.ID, environments.Settings{CPUs: 4, MemoryMiB: 2048, Display: api.DisplayNone,
		GPU: api.GPUNone})
	if err != nil {
		t.Fatal(err)
	}
	if got.CPUs != 4 || got.Spec.CPUs != 4 || !slices.Equal(got.TemplateChanges, []string{environments.SettingCPUs}) || got.TemplateUpdated {
		t.Fatalf("after its own change: cpus %d/%d, changes %v, updated %v", got.CPUs, got.Spec.CPUs, got.TemplateChanges, got.TemplateUpdated)
	}
	if _, err := em.UpdateSettings(ctx, p, env.ID, environments.Settings{CPUs: 999, MemoryMiB: 2048}); !errors.Is(err, environments.ErrInvalid) {
		t.Errorf("999 vCPUs: %v, want ErrInvalid", err)
	}
	if _, err := em.UpdateSettings(ctx, p, env.ID, environments.Settings{CPUs: 2, MemoryMiB: 2048,
		GPU: api.GPUPassthrough}); !errors.Is(err, environments.ErrInvalid) {
		t.Errorf("a GPU passed through: %v, want ErrInvalid", err)
	}

	// The template moves on.
	spec.MemoryMiB = 4096
	if _, err := tm.Update(ctx, p, tpl.ID, templates.Input{Name: "t", Visibility: templates.Private, Spec: spec}); err != nil {
		t.Fatal(err)
	}
	got, err = em.Get(ctx, p, env.ID)
	if err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(got.TemplateChanges, []string{environments.SettingCPUs, environments.SettingMemory}) || !got.TemplateUpdated {
		t.Fatalf("after the template's change: changes %v, updated %v", got.TemplateChanges, got.TemplateUpdated)
	}

	got, err = em.ResetToTemplate(ctx, p, env.ID)
	if err != nil {
		t.Fatal(err)
	}
	if got.ID != env.ID || got.CPUs != 2 || got.MemoryMiB != 4096 || len(got.TemplateChanges) != 0 || got.TemplateUpdated {
		t.Fatalf("after resetting: %d vCPUs, %d MiB, changes %v, updated %v", got.CPUs, got.MemoryMiB, got.TemplateChanges, got.TemplateUpdated)
	}

	// Another image is another environment.
	spec.Image = "other"
	if _, err := tm.Update(ctx, p, tpl.ID, templates.Input{Name: "t", Visibility: templates.Private, Spec: spec}); err != nil {
		t.Fatal(err)
	}
	if _, err := em.ResetToTemplate(ctx, p, env.ID); !errors.Is(err, environments.ErrConflict) {
		t.Errorf("resetting to another image: %v, want ErrConflict", err)
	}
}

// A machine that may be running keeps its settings until it is stopped.
func TestSettingsNeedStopped(t *testing.T) {
	d := dbtest.Open(t)
	em, tm := environments.NewManager(d), templates.NewManager(d)
	u, _ := users.NewManager(d).Create(ctx, users.NewUser{Username: "owner", Password: "password1"})
	p := users.Principal{UserID: u.ID, Username: u.Username}
	tpl, err := tm.Create(ctx, p, templates.Input{Name: "t",
		Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 2, MemoryMiB: 2048}}})
	if err != nil {
		t.Fatal(err)
	}
	env, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "e"})
	if err != nil {
		t.Fatal(err)
	}
	// As its worker reports it once it has booted.
	if err := d.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `UPDATE environments SET phase = 'running' WHERE id = $1`, env.ID)
		return err
	}); err != nil {
		t.Fatal(err)
	}
	s := environments.Settings{CPUs: 4, MemoryMiB: 2048, Display: api.DisplayDesktop, GPU: api.GPUNone}
	if _, err := em.UpdateSettings(ctx, p, env.ID, s); !errors.Is(err, environments.ErrConflict) {
		t.Fatalf("changing a running environment: %v, want ErrConflict", err)
	}
	if _, err := em.SetDesired(ctx, p, env.ID, api.DesiredStopped); err != nil {
		t.Fatal(err)
	}
	if _, err := em.UpdateSettings(ctx, p, env.ID, s); err != nil {
		t.Fatalf("changing it once stopped: %v", err)
	}
}

// Whether an environment maps its image's files is its template's to start
// with, its own to change, and a difference from the template once changed.
func TestDAXSetting(t *testing.T) {
	d := dbtest.Open(t)
	em, tm := environments.NewManager(d), templates.NewManager(d)
	u, _ := users.NewManager(d).Create(ctx, users.NewUser{Username: "owner", Password: "password1"})
	p := users.Principal{UserID: u.ID, Username: u.Username}
	tpl, err := tm.Create(ctx, p, templates.Input{Name: "t",
		Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 2, MemoryMiB: 2048, Display: api.DisplayNone, DAX: true}}})
	if err != nil {
		t.Fatal(err)
	}
	env, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tpl.ID, Name: "e"})
	if err != nil {
		t.Fatal(err)
	}
	if !env.Spec.DAX {
		t.Fatal("an environment from a template asking for DAX does not ask for it")
	}

	got, err := em.UpdateSettings(ctx, p, env.ID, environments.Settings{CPUs: 2, MemoryMiB: 2048,
		Display: api.DisplayNone, GPU: api.GPUNone})
	if err != nil {
		t.Fatal(err)
	}
	if got.Spec.DAX || !slices.Equal(got.TemplateChanges, []string{environments.SettingDAX}) {
		t.Fatalf("after turning DAX off: dax %v, changes %v", got.Spec.DAX, got.TemplateChanges)
	}

	got, err = em.ResetToTemplate(ctx, p, env.ID)
	if err != nil {
		t.Fatal(err)
	}
	if !got.Spec.DAX || len(got.TemplateChanges) != 0 {
		t.Fatalf("after resetting: dax %v, changes %v", got.Spec.DAX, got.TemplateChanges)
	}
}
