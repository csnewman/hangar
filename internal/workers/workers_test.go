package workers_test

import (
	"context"
	"errors"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/placement"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
	"github.com/csnewman/hangar/internal/workers"
)

var ctx = context.Background()

func TestCredentials(t *testing.T) {
	m := workers.NewManager(dbtest.Open(t))

	cred, err := m.Register(ctx, api.RegisterWorker{Name: "w1"})
	if err != nil {
		t.Fatal(err)
	}
	id, err := m.Authenticate(ctx, cred.Credential)
	if err != nil || id != cred.ID {
		t.Fatalf("Authenticate(issued) = %q, %v; want %q", id, err, cred.ID)
	}

	for _, bad := range []string{
		"",
		cred.ID,
		cred.ID + ".",
		cred.ID + ".AAAA",
		"not-a-uuid." + cred.Credential[len(cred.ID)+1:],
	} {
		if _, err := m.Authenticate(ctx, bad); !errors.Is(err, workers.ErrUnauthorized) {
			t.Errorf("Authenticate(%q) = %v, want ErrUnauthorized", bad, err)
		}
	}

	if _, err := m.Register(ctx, api.RegisterWorker{Name: "w1"}); !errors.Is(err, workers.ErrConflict) {
		t.Fatalf("registering a taken name: %v, want ErrConflict", err)
	}
	if _, err := m.Register(ctx, api.RegisterWorker{Name: "-bad name"}); !errors.Is(err, workers.ErrInvalid) {
		t.Fatalf("registering an invalid name: %v, want ErrInvalid", err)
	}

	if err := m.Delete(ctx, cred.ID); !errors.Is(err, workers.ErrConflict) {
		t.Fatalf("deleting an unrevoked worker: %v, want ErrConflict", err)
	}
	if err := m.Revoke(ctx, cred.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := m.Authenticate(ctx, cred.Credential); !errors.Is(err, workers.ErrUnauthorized) {
		t.Fatalf("a revoked credential authenticated: %v", err)
	}
	if err := m.Delete(ctx, cred.ID); err != nil {
		t.Fatal(err)
	}
	if _, err := m.Register(ctx, api.RegisterWorker{Name: "w1"}); err != nil {
		t.Fatalf("a removed worker's name could not register again: %v", err)
	}
}

// A worker speaks only for its own environments: naming another worker's
// changes nothing and is recorded as unknown.
func TestReportCannotTouchAnotherWorkersEnvironment(t *testing.T) {
	d := dbtest.Open(t)
	wm := workers.NewManager(d)
	em := environments.NewManager(d)

	a, _ := wm.Register(ctx, api.RegisterWorker{Name: "a"})
	b, _ := wm.Register(ctx, api.RegisterWorker{Name: "b"})
	report(t, wm, a.ID, 4, 8192)

	owner, err := users.NewManager(d).Create(ctx, users.NewUser{Username: "owner", Password: "password1"})
	if err != nil {
		t.Fatal(err)
	}
	p := users.Principal{UserID: owner.ID}
	tmpl, err := templates.NewManager(d).Create(ctx, p, templates.Input{
		Name: "t", Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024}},
	})
	if err != nil {
		t.Fatal(err)
	}
	env, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tmpl.ID, Name: "e"})
	if err != nil {
		t.Fatal(err)
	}
	if n, err := placement.NewManager(d).Place(ctx); err != nil || n != 1 {
		t.Fatalf("Place = %d, %v", n, err)
	}

	report(t, wm, b.ID, 4, 8192, api.ObservedEnvironment{ID: env.ID, Phase: api.PhaseFailed, Reason: "forged"})

	got, _ := em.Get(ctx, p, env.ID)
	if got.Phase != api.PhasePending {
		t.Fatalf("worker b changed worker a's environment to %s", got.Phase)
	}
	list, _ := wm.List(ctx)
	for _, w := range list {
		if w.Name == "b" && (len(w.Unknown) != 1 || w.Unknown[0] != env.ID) {
			t.Fatalf("worker b's unknown list is %v, want [%s]", w.Unknown, env.ID)
		}
	}
}

func TestReportRejectsUnknownPhase(t *testing.T) {
	wm := workers.NewManager(dbtest.Open(t))
	w, _ := wm.Register(ctx, api.RegisterWorker{Name: "w"})
	err := wm.ReportStatus(ctx, w.ID, api.WorkerStatus{
		Environments: []api.ObservedEnvironment{{ID: "x", Phase: "exploded"}},
	})
	if !errors.Is(err, workers.ErrInvalid) {
		t.Fatalf("ReportStatus with a bad phase: %v, want ErrInvalid", err)
	}
}

func report(t *testing.T, wm *workers.Manager, id string, cpus, mem int, envs ...api.ObservedEnvironment) {
	t.Helper()
	err := wm.ReportStatus(ctx, id, api.WorkerStatus{
		Capacity:     api.Resources{CPUs: cpus, MemoryMiB: mem},
		Environments: envs,
	})
	if err != nil {
		t.Fatalf("ReportStatus: %v", err)
	}
}

// An image an environment on the worker uses cannot be removed. Once
// nothing uses it, a removal is asked for in the desired set and stands
// until the worker stops reporting the image.
func TestImageRemoval(t *testing.T) {
	d := dbtest.Open(t)
	wm := workers.NewManager(d)
	em := environments.NewManager(d)
	w, _ := wm.Register(ctx, api.RegisterWorker{Name: "w"})
	owner, _ := users.NewManager(d).Create(ctx, users.NewUser{Username: "owner", Password: "password1"})
	p := users.Principal{UserID: owner.ID}

	img := api.LocalImage{Ref: "img", SizeBytes: 1 << 30, State: "ready"}
	status := func(images ...api.LocalImage) {
		t.Helper()
		if err := wm.ReportStatus(ctx, w.ID, api.WorkerStatus{
			Capacity: api.Resources{CPUs: 4, MemoryMiB: 8192}, Images: images,
		}); err != nil {
			t.Fatal(err)
		}
	}
	status(img)

	tmpl, _ := templates.NewManager(d).Create(ctx, p, templates.Input{
		Name: "t", Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024}},
	})
	env, err := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tmpl.ID, Name: "e"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := placement.NewManager(d).Place(ctx); err != nil {
		t.Fatal(err)
	}

	if err := wm.RemoveImage(ctx, w.ID, "img"); !errors.Is(err, workers.ErrInUse) {
		t.Fatalf("removing an image in use: %v, want ErrInUse", err)
	}
	if err := wm.RemoveImage(ctx, w.ID, "other"); !errors.Is(err, workers.ErrNoImage) {
		t.Fatalf("removing an image the worker lacks: %v, want ErrNoImage", err)
	}

	// Deleted and gone from the worker: nothing uses it.
	if _, err := em.SetDesired(ctx, p, env.ID, api.DesiredDeleted); err != nil {
		t.Fatal(err)
	}
	status(img)
	before, _ := wm.DesiredVersion(ctx, w.ID)
	if err := wm.RemoveImage(ctx, w.ID, "img"); err != nil {
		t.Fatalf("removing an unused image: %v", err)
	}
	set, _ := wm.DesiredSet(ctx, w.ID)
	if set.Version <= before || len(set.RemoveImages) != 1 || set.RemoveImages[0] != "img" {
		t.Fatalf("desired set after asking: version %d -> %d, removals %v", before, set.Version, set.RemoveImages)
	}
	got, _ := wm.Get(ctx, w.ID)
	if len(got.PendingRemovals) != 1 {
		t.Fatalf("pending removals %v", got.PendingRemovals)
	}

	// Still held: still asked for. Gone: done.
	status(img)
	if set, _ := wm.DesiredSet(ctx, w.ID); len(set.RemoveImages) != 1 {
		t.Fatal("the removal was dropped while the worker still held the image")
	}
	status()
	if set, _ := wm.DesiredSet(ctx, w.ID); len(set.RemoveImages) != 0 {
		t.Fatalf("the removal stands after the image went: %v", set.RemoveImages)
	}
}

// What a worker measures reaches the environment, and only while it runs.
func TestStats(t *testing.T) {
	d := dbtest.Open(t)
	wm := workers.NewManager(d)
	em := environments.NewManager(d)
	w, _ := wm.Register(ctx, api.RegisterWorker{Name: "w"})
	owner, _ := users.NewManager(d).Create(ctx, users.NewUser{Username: "owner", Password: "password1"})
	p := users.Principal{UserID: owner.ID}
	if err := wm.ReportStatus(ctx, w.ID, api.WorkerStatus{Capacity: api.Resources{CPUs: 4, MemoryMiB: 8192}}); err != nil {
		t.Fatal(err)
	}
	tmpl, _ := templates.NewManager(d).Create(ctx, p, templates.Input{
		Name: "t", Spec: api.TemplateSpec{Spec: api.Spec{Image: "img", CPUs: 1, MemoryMiB: 1024}},
	})
	env, _ := em.Create(ctx, p, api.CreateEnvironment{TemplateID: tmpl.ID, Name: "e"})
	placement.NewManager(d).Place(ctx)

	report := func(phase api.Phase) {
		t.Helper()
		if err := wm.ReportStatus(ctx, w.ID, api.WorkerStatus{
			Capacity: api.Resources{CPUs: 4, MemoryMiB: 8192},
			Stats:    &api.WorkerStats{CPUPercent: 42, MemoryTotalMiB: 8192, MemoryUsedMiB: 1000},
			Environments: []api.ObservedEnvironment{{ID: env.ID, Phase: phase,
				Stats: &api.EnvironmentStats{CPUPercent: 12.5, MemoryUsedMiB: 300, MemoryTotalMiB: 1024}}},
		}); err != nil {
			t.Fatal(err)
		}
	}
	report(api.PhaseRunning)
	got, _ := em.Get(ctx, p, env.ID)
	if got.Stats == nil || got.Stats.CPUPercent != 12.5 || got.Stats.MemoryUsedMiB != 300 {
		t.Fatalf("running environment's stats: %+v", got.Stats)
	}
	stamped := got.UpdatedAt
	report(api.PhaseRunning)
	if again, _ := em.Get(ctx, p, env.ID); !again.UpdatedAt.Equal(stamped) {
		t.Fatal("a measurement alone moved updated_at, which marks changes of phase")
	}
	if w, _ := wm.Get(ctx, w.ID); w.Stats == nil || w.Stats.CPUPercent != 42 {
		t.Fatalf("worker stats: %+v", w.Stats)
	}

	report(api.PhaseStopped)
	if got, _ := em.Get(ctx, p, env.ID); got.Stats != nil {
		t.Fatalf("a stopped environment reports usage: %+v", got.Stats)
	}
}
