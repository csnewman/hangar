package worker_test

import (
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/worker"
)

func phases(s *worker.Simulated) map[string]api.Phase {
	out := map[string]api.Phase{}
	for _, o := range s.Observe() {
		out[o.ID] = o.Phase
	}
	return out
}

func eventually(t *testing.T, s *worker.Simulated, id string, want api.Phase, gone bool) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		got, held := phases(s)[id]
		if gone && !held || !gone && got == want {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("%s: phases are %v, want %s (gone=%v)", id, phases(s), want, gone)
}

func TestSimulatedLifecycle(t *testing.T) {
	s := worker.NewSimulated(20 * time.Millisecond)
	spec := api.EnvironmentSpec{ID: "a", Spec: api.Spec{Image: "img"}, Desired: api.DesiredRunning}

	s.Apply(spec)
	if got := phases(s)["a"]; got != api.PhaseStarting {
		t.Fatalf("after Apply(running): %s, want starting", got)
	}
	// Applying the same spec again, as every desired set does, must not
	// restart the transition.
	s.Apply(spec)
	eventually(t, s, "a", api.PhaseRunning, false)

	spec.Desired = api.DesiredStopped
	s.Apply(spec)
	eventually(t, s, "a", api.PhaseStopped, false)

	spec.Desired = api.DesiredDeleted
	s.Apply(spec)
	eventually(t, s, "a", "", true)
}

func TestSimulatedFailureIsSticky(t *testing.T) {
	s := worker.NewSimulated(time.Millisecond)
	spec := api.EnvironmentSpec{ID: "f", Spec: api.Spec{Image: "will-fail"}, Desired: api.DesiredRunning}
	s.Apply(spec)
	eventually(t, s, "f", api.PhaseFailed, false)

	s.Apply(spec)
	time.Sleep(10 * time.Millisecond)
	if got := phases(s)["f"]; got != api.PhaseFailed {
		t.Fatalf("a repeated desired set retried a failed environment: %s", got)
	}
}

// An environment the worker never started is stopped already, and one it
// never held is already deleted.
func TestSimulatedNeverStarted(t *testing.T) {
	s := worker.NewSimulated(time.Hour)
	s.Apply(api.EnvironmentSpec{ID: "s", Desired: api.DesiredStopped})
	s.Apply(api.EnvironmentSpec{ID: "d", Desired: api.DesiredDeleted})
	got := phases(s)
	if got["s"] != api.PhaseStopped {
		t.Errorf("never-started stopped environment is %q", got["s"])
	}
	if _, held := got["d"]; held {
		t.Error("never-held deleted environment is reported")
	}
}

// A newer desired state overtakes a transition still in flight.
func TestSimulatedOvertake(t *testing.T) {
	s := worker.NewSimulated(30 * time.Millisecond)
	s.Apply(api.EnvironmentSpec{ID: "o", Spec: api.Spec{Image: "img"}, Desired: api.DesiredRunning})
	s.Apply(api.EnvironmentSpec{ID: "o", Spec: api.Spec{Image: "img"}, Desired: api.DesiredStopped})
	eventually(t, s, "o", api.PhaseStopped, false)
	time.Sleep(60 * time.Millisecond)
	if got := phases(s)["o"]; got != api.PhaseStopped {
		t.Fatalf("a stale transition landed: %s", got)
	}
}

func TestParseSize(t *testing.T) {
	for in, want := range map[string]worker.Size{
		"0":       0,
		"1024":    1024,
		"4GiB":    4 << 30,
		"512 MiB": 512 << 20,
		"1TiB":    1 << 40,
	} {
		got, err := worker.ParseSize(in)
		if err != nil || got != want {
			t.Errorf("ParseSize(%q) = %d, %v; want %d", in, got, err, want)
		}
	}
	for _, bad := range []string{"", "4GB", "-1", "lots"} {
		if _, err := worker.ParseSize(bad); err == nil {
			t.Errorf("ParseSize(%q) succeeded", bad)
		}
	}
}
