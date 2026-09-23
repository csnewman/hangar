package worker

import (
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/api"
)

// Simulated is a Runtime that runs nothing. Each environment walks through
// the phases a real one would, with a delay at each step, so the control
// plane and the UI can be exercised end to end on a machine without KVM.
//
// An image whose reference contains "fail" fails to start, to exercise the
// failure path.
type Simulated struct {
	// Step is how long each transition takes.
	Step time.Duration

	mu      sync.Mutex
	envs    map[string]*simEnv
	changed chan struct{}
}

type simEnv struct {
	phase  api.Phase
	reason string
	// target is the desired state the pending transition is heading for. A
	// transition whose target is stale when it lands is dropped.
	target api.DesiredState
	gen    int
}

func NewSimulated(step time.Duration) *Simulated {
	return &Simulated{Step: step, envs: map[string]*simEnv{}, changed: make(chan struct{}, 1)}
}

func (s *Simulated) Changed() <-chan struct{} { return s.changed }

func (s *Simulated) Observe() []api.ObservedEnvironment {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]api.ObservedEnvironment, 0, len(s.envs))
	for id, e := range s.envs {
		out = append(out, api.ObservedEnvironment{ID: id, Phase: e.phase, Reason: e.reason})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out
}

func (s *Simulated) Apply(spec api.EnvironmentSpec) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e, held := s.envs[spec.ID]

	switch spec.Desired {
	case api.DesiredRunning:
		if !held {
			e = &simEnv{}
			s.envs[spec.ID] = e
		}
		// Already heading for running, or there and failed. A failed
		// environment stays failed until it is stopped and started again;
		// retrying on every desired set would hide the failure in a loop.
		if e.target == api.DesiredRunning {
			return
		}
		s.transition(spec.ID, e, api.DesiredRunning, api.PhaseStarting, func(e *simEnv) {
			if strings.Contains(spec.Spec.Image, "fail") {
				e.phase, e.reason = api.PhaseFailed, "simulated failure: image "+spec.Spec.Image
				return
			}
			e.phase, e.reason = api.PhaseRunning, ""
		})

	case api.DesiredStopped:
		if !held {
			// Never started here, so it is already stopped.
			s.envs[spec.ID] = &simEnv{phase: api.PhaseStopped, target: api.DesiredStopped}
			s.notify()
			return
		}
		if e.target == api.DesiredStopped {
			return
		}
		s.transition(spec.ID, e, api.DesiredStopped, api.PhaseStopping, func(e *simEnv) {
			e.phase, e.reason = api.PhaseStopped, ""
		})

	case api.DesiredDeleted:
		if !held || e.target == api.DesiredDeleted {
			return
		}
		s.transition(spec.ID, e, api.DesiredDeleted, api.PhaseDeleting, nil)
	}
}

// transition puts e into an intermediate phase at once and runs done after
// Step. A nil done removes the environment.
func (s *Simulated) transition(id string, e *simEnv, target api.DesiredState, during api.Phase, done func(*simEnv)) {
	e.gen++
	gen := e.gen
	e.target, e.phase, e.reason = target, during, ""
	s.notify()
	time.AfterFunc(s.Step, func() {
		s.mu.Lock()
		defer s.mu.Unlock()
		cur, ok := s.envs[id]
		if !ok || cur.gen != gen {
			return
		}
		if done == nil {
			delete(s.envs, id)
		} else {
			done(cur)
		}
		s.notify()
	})
}

func (s *Simulated) notify() {
	select {
	case s.changed <- struct{}{}:
	default:
	}
}
