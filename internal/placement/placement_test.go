package placement_test

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"testing"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/placement"
	"github.com/csnewman/hangar/internal/workers"
)

var ctx = context.Background()

type plane struct {
	envs    *environments.Manager
	workers *workers.Manager
	place   *placement.Manager
}

func newPlane(t *testing.T) (*plane, *db.DB) {
	d := dbtest.Open(t)
	return &plane{environments.NewManager(d), workers.NewManager(d), placement.NewManager(d)}, d
}

func (p *plane) worker(t *testing.T, name string, cpus, mem int) string {
	t.Helper()
	cred, err := p.workers.Register(ctx, api.RegisterWorker{Name: name})
	if err != nil {
		t.Fatal(err)
	}
	p.report(t, cred.ID, cpus, mem)
	return cred.ID
}

func (p *plane) report(t *testing.T, id string, cpus, mem int, envs ...api.ObservedEnvironment) {
	t.Helper()
	if err := p.workers.ReportStatus(ctx, id, api.WorkerStatus{
		Capacity:     api.Resources{CPUs: cpus, MemoryMiB: mem},
		Environments: envs,
	}); err != nil {
		t.Fatal(err)
	}
}

func (p *plane) env(t *testing.T, name string, cpus, mem int) api.Environment {
	t.Helper()
	e, err := p.envs.Create(ctx, api.CreateEnvironment{Name: name, Image: "img", CPUs: cpus, MemoryMiB: mem})
	if err != nil {
		t.Fatal(err)
	}
	return e
}

func (p *plane) get(t *testing.T, id string) api.Environment {
	t.Helper()
	e, err := p.envs.Get(ctx, id)
	if err != nil {
		t.Fatal(err)
	}
	return e
}

// The whole life of an environment, as the server sees it.
func TestLifecycle(t *testing.T) {
	p, _ := newPlane(t)
	w := p.worker(t, "w", 4, 8192)
	v0, _ := p.workers.DesiredVersion(ctx, w)

	e := p.env(t, "e", 2, 2048)
	if e.Phase != api.PhasePending || e.WorkerID != "" {
		t.Fatalf("new environment is %s on %q", e.Phase, e.WorkerID)
	}
	if n, err := p.place.Place(ctx); err != nil || n != 1 {
		t.Fatalf("Place = %d, %v", n, err)
	}

	set, err := p.workers.DesiredSet(ctx, w)
	if err != nil {
		t.Fatal(err)
	}
	if set.Version <= v0 {
		t.Fatalf("placing did not advance the desired version (%d -> %d)", v0, set.Version)
	}
	if len(set.Environments) != 1 || set.Environments[0].ID != e.ID || set.Environments[0].Desired != api.DesiredRunning {
		t.Fatalf("desired set is %+v", set.Environments)
	}

	p.report(t, w, 4, 8192, api.ObservedEnvironment{ID: e.ID, Phase: api.PhaseRunning})
	if got := p.get(t, e.ID); got.Phase != api.PhaseRunning || got.Worker != "w" {
		t.Fatalf("after report: %s on %q", got.Phase, got.Worker)
	}

	if _, err := p.envs.SetDesired(ctx, e.ID, api.DesiredDeleted); err != nil {
		t.Fatal(err)
	}
	if _, err := p.envs.SetDesired(ctx, e.ID, api.DesiredRunning); !errors.Is(err, environments.ErrConflict) {
		t.Fatalf("restarting a deleting environment: %v, want ErrConflict", err)
	}
	set, _ = p.workers.DesiredSet(ctx, w)
	if len(set.Environments) != 1 || set.Environments[0].Desired != api.DesiredDeleted {
		t.Fatalf("a deleting environment must stay in the desired set until it is gone: %+v", set.Environments)
	}

	// Still held: the row stays.
	p.report(t, w, 4, 8192, api.ObservedEnvironment{ID: e.ID, Phase: api.PhaseDeleting})
	p.get(t, e.ID)

	// No longer held: the row goes, and the worker's set shrinks.
	before := set.Version
	p.report(t, w, 4, 8192)
	if _, err := p.envs.Get(ctx, e.ID); !errors.Is(err, environments.ErrNotFound) {
		t.Fatalf("environment survived its worker reporting it gone: %v", err)
	}
	set, _ = p.workers.DesiredSet(ctx, w)
	if len(set.Environments) != 0 || set.Version <= before {
		t.Fatalf("desired set after removal: version %d -> %d, %+v", before, set.Version, set.Environments)
	}
}

// An environment nothing holds has nobody to wait for.
func TestUnplacedEnvironmentsChangeAtOnce(t *testing.T) {
	p, _ := newPlane(t)
	e := p.env(t, "e", 1, 1024)

	if _, err := p.envs.SetDesired(ctx, e.ID, api.DesiredStopped); err != nil {
		t.Fatal(err)
	}
	if got := p.get(t, e.ID); got.Phase != api.PhaseStopped {
		t.Fatalf("stopped unplaced environment is %s", got.Phase)
	}

	// A stopped environment is not placed.
	p.worker(t, "w", 4, 8192)
	if n, _ := p.place.Place(ctx); n != 0 {
		t.Fatalf("placed %d stopped environments", n)
	}

	exists, err := p.envs.SetDesired(ctx, e.ID, api.DesiredDeleted)
	if err != nil || exists {
		t.Fatalf("deleting an unplaced environment: exists=%v err=%v", exists, err)
	}
}

func TestPlacementRespectsCapacity(t *testing.T) {
	p, _ := newPlane(t)
	p.worker(t, "small", 2, 2048)
	p.worker(t, "large", 8, 16384)

	big := p.env(t, "big", 4, 8192)
	tiny := p.env(t, "tiny", 1, 1024)
	tooBig := p.env(t, "too-big", 16, 1024)
	if n, err := p.place.Place(ctx); err != nil || n != 2 {
		t.Fatalf("Place = %d, %v; want 2", n, err)
	}

	if got := p.get(t, big.ID).Worker; got != "large" {
		t.Errorf("big landed on %q, want large", got)
	}
	// Spread: large still has more free memory than small.
	if got := p.get(t, tiny.ID).Worker; got != "large" {
		t.Errorf("tiny landed on %q, want large (most free memory)", got)
	}
	got := p.get(t, tooBig.ID)
	if got.WorkerID != "" || got.Reason != placement.Unplaceable {
		t.Errorf("too-big: worker %q, reason %q", got.Worker, got.Reason)
	}
}

// A stopped environment keeps its place, and so keeps its share of capacity.
func TestStoppedEnvironmentsHoldCapacity(t *testing.T) {
	p, _ := newPlane(t)
	p.worker(t, "w", 2, 4096)
	first := p.env(t, "first", 2, 4096)
	p.place.Place(ctx)
	p.envs.SetDesired(ctx, first.ID, api.DesiredStopped)

	second := p.env(t, "second", 1, 1024)
	if n, _ := p.place.Place(ctx); n != 0 {
		t.Fatal("placed into capacity a stopped environment still holds")
	}
	if got := p.get(t, second.ID); got.WorkerID != "" {
		t.Fatalf("second placed on %q", got.Worker)
	}
}

func TestOfflineAndRevokedWorkersGetNothing(t *testing.T) {
	p, _ := newPlane(t)
	w := p.worker(t, "w", 4, 8192)
	if err := p.workers.Revoke(ctx, w); err != nil {
		t.Fatal(err)
	}
	// Registered but never reported, so never seen online.
	if _, err := p.workers.Register(ctx, api.RegisterWorker{Name: "silent"}); err != nil {
		t.Fatal(err)
	}

	p.env(t, "e", 1, 1024)
	if n, _ := p.place.Place(ctx); n != 0 {
		t.Fatalf("placed %d environments with no usable worker", n)
	}
}

// Replicas place concurrently. However the queue is split between them, no
// worker may be given more than it has.
func TestConcurrentPlacementNeverOvercommits(t *testing.T) {
	p, d := newPlane(t)
	for i := range 3 {
		p.worker(t, fmt.Sprintf("w%d", i), 4, 4096)
	}
	for i := range 40 {
		p.env(t, fmt.Sprintf("e%d", i), 1, 512)
	}

	var wg sync.WaitGroup
	for range 6 {
		wg.Go(func() {
			// Each goroutine is a replica with its own manager.
			m := placement.NewManager(d)
			for {
				n, err := m.Place(ctx)
				if err != nil {
					t.Error(err)
					return
				}
				if n == 0 {
					return
				}
			}
		})
	}
	wg.Wait()

	list, err := p.workers.List(ctx)
	if err != nil {
		t.Fatal(err)
	}
	total := 0
	for _, w := range list {
		if w.Allocated.CPUs > w.Capacity.CPUs || w.Allocated.MemoryMiB > w.Capacity.MemoryMiB {
			t.Errorf("%s overcommitted: %+v of %+v", w.Name, w.Allocated, w.Capacity)
		}
		total += w.Allocated.CPUs
	}
	if total != 12 {
		t.Errorf("placed %d vCPUs across three 4-vCPU workers, want 12", total)
	}
}
