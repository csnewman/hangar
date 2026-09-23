package workers_test

import (
	"context"
	"errors"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/placement"
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
	env, err := em.Create(ctx, p, api.CreateEnvironment{Name: "e", Image: "img", CPUs: 1, MemoryMiB: 1024})
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
