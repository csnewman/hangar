package worker

import (
	"context"
	"fmt"
	"math/rand/v2"
	"net"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/procs"
	"github.com/csnewman/hangar/internal/terminal"
)

// Simulated is a Runtime that runs nothing. Each environment walks through
// the phases a real one would, with a delay at each step, so the control
// plane and the UI can be exercised end to end on a machine without KVM.
//
// An image whose reference contains "fail" fails to start, to exercise the
// failure path.
//
// It also pretends to measure what running environments use, and keeps a
// store of the images they were started from, so every view of a worker has
// something moving in it.
type Simulated struct {
	// Step is how long each transition takes.
	Step time.Duration

	mu      sync.Mutex
	envs    map[string]*simEnv
	images  map[string]int64
	changed chan struct{}
	// terminals are the environments' sessions, run on this machine by the
	// same manager the guest agent uses, so a terminal behaves the same in
	// development as against a real environment.
	terminals map[string]*terminal.Manager
	// procs lists this machine's processes for every environment.
	procs *procs.Server
}

type simEnv struct {
	phase  api.Phase
	reason string
	// target is the desired state the pending transition is heading for. A
	// transition whose target is stale when it lands is dropped.
	target api.DesiredState
	gen    int
	// since is when the pending transition began.
	since time.Time
	spec  api.Spec
	// usage is the pretend measurement, wandering from one Observe to the
	// next.
	usage api.EnvironmentStats
}

func NewSimulated(step time.Duration) *Simulated {
	return &Simulated{Step: step, envs: map[string]*simEnv{}, images: map[string]int64{},
		changed: make(chan struct{}, 1), terminals: map[string]*terminal.Manager{}}
}

func (s *Simulated) Changed() <-chan struct{} { return s.changed }

func (s *Simulated) Observe() []api.ObservedEnvironment {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]api.ObservedEnvironment, 0, len(s.envs))
	for id, e := range s.envs {
		o := api.ObservedEnvironment{ID: id, Phase: e.phase, Reason: e.reason}
		if e.phase == api.PhaseStarting {
			// A start pretends to download its image over the Step it
			// takes.
			total := s.images[e.spec.Image]
			done := total
			if s.Step > 0 {
				done = min(total, int64(float64(total)*float64(time.Since(e.since))/float64(s.Step)))
			}
			o.Reason = "downloading the image"
			o.Progress = &api.Progress{Step: api.StepDownload, Done: done, Total: total, Unit: "bytes"}
			if s.Step > 0 {
				o.Progress.Rate = float64(total) / s.Step.Seconds()
			}
		}
		if e.phase == api.PhaseRunning {
			e.wander()
			u := e.usage
			o.Stats = &u
		}
		out = append(out, o)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out
}

func (s *Simulated) Apply(spec api.EnvironmentSpec) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e, held := s.envs[spec.ID]
	if held {
		e.spec = spec.Spec
	}

	switch spec.Desired {
	case api.DesiredRunning:
		if !held {
			e = &simEnv{spec: spec.Spec}
			s.envs[spec.ID] = e
		}
		if _, ok := s.images[spec.Spec.Image]; !ok && spec.Spec.Image != "" {
			// The same image always reports the same size.
			s.images[spec.Spec.Image] = int64(900+len(spec.Spec.Image)*37) << 20
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

	case api.DesiredSuspended:
		if !held {
			// Nothing running here, so there is nothing to keep.
			s.envs[spec.ID] = &simEnv{phase: api.PhaseStopped, target: api.DesiredStopped}
			s.notify()
			return
		}
		if e.target == api.DesiredSuspended || e.phase != api.PhaseRunning {
			return
		}
		s.transition(spec.ID, e, api.DesiredSuspended, api.PhaseSuspending, func(e *simEnv) {
			e.phase, e.reason = api.PhaseSuspended, ""
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
	e.target, e.phase, e.reason, e.since = target, during, "", time.Now()
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

// wander moves the pretend measurement a little, within what the
// environment's spec allows.
func (e *simEnv) wander() {
	u := &e.usage
	u.MemoryTotalMiB = e.spec.MemoryMiB
	if u.MemoryUsedMiB == 0 {
		u.MemoryUsedMiB = e.spec.MemoryMiB / 4
		u.DiskUsedBytes = 600 << 20
	}
	step := func(v, by, lo, hi float64) float64 { return min(max(v+(rand.Float64()*2-1)*by, lo), hi) }
	u.CPUPercent = step(u.CPUPercent, 12, 1, 95)
	u.MemoryUsedMiB = int(step(float64(u.MemoryUsedMiB), float64(e.spec.MemoryMiB)/20, 128,
		float64(e.spec.MemoryMiB)*0.9))
	u.DiskUsedBytes += int64(rand.IntN(4 << 20))
	burst := func(v float64) float64 {
		if rand.IntN(4) == 0 {
			return rand.Float64() * 40e6
		}
		return v * 0.4
	}
	u.DiskReadBps = burst(u.DiskReadBps)
	u.DiskWriteBps = burst(u.DiskWriteBps)
	u.NetRxBps = burst(u.NetRxBps)
	u.NetTxBps = burst(u.NetTxBps) / 3
}

// Images reports the pretend store: every image an environment was started
// from, until it is removed.
func (s *Simulated) Images() []api.LocalImage {
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]api.LocalImage, 0, len(s.images))
	for ref, size := range s.images {
		img := api.LocalImage{Ref: ref, SizeBytes: size, State: "ready", Environments: []string{}}
		for id, e := range s.envs {
			if e.spec.Image == ref {
				img.Environments = append(img.Environments, id)
			}
		}
		sort.Strings(img.Environments)
		out = append(out, img)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Ref < out[j].Ref })
	return out
}

// RemoveImage forgets an image no held environment uses.
func (s *Simulated) RemoveImage(ref string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, e := range s.envs {
		if e.spec.Image == ref {
			return
		}
	}
	if _, ok := s.images[ref]; ok {
		delete(s.images, ref)
		s.notify()
	}
}

// DialTerminal connects to a running environment's terminal sessions. The
// shells run on this machine, as whoever runs the worker: there is no guest
// to run them in.
func (s *Simulated) DialTerminal(_ context.Context, id string) (net.Conn, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e, ok := s.envs[id]
	if !ok || e.phase != api.PhaseRunning {
		return nil, fmt.Errorf("environment %s is not running here", id)
	}
	m, ok := s.terminals[id]
	if !ok {
		m = terminal.NewManager(terminal.LoginShell("dev"), nil)
		s.terminals[id] = m
	}
	a, b := net.Pipe()
	go m.Serve(b)
	return a, nil
}
