// Package vm is the worker runtime that runs environments as Cloud
// Hypervisor virtual machines.
//
// Each environment has a controller goroutine of its own. The worker hands
// it the environment's spec whenever the desired set arrives; the controller
// compares what is asked for with what is running and does whatever closes
// the gap -- boots the machine, powers it off, or deletes its disks -- one
// step at a time, so two requests for the same environment never overlap.
//
// An environment is running only once its agent has dialled back and the
// template's provisioning has finished. Until then it is starting, with a
// reason saying which step it is on, so a slow clone and a wedged boot look
// different from the outside.
//
// Everything an environment keeps between runs is in its own directory under
// StateDir: its writable layer and Docker store, its console log, and a mark
// recording that provisioning is done. Stopping keeps the directory, so the
// next start boots the same disks without cloning again; deleting removes it.
package vm

import (
	"context"
	"errors"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/api"
)

// Image is how a worker finds the base an environment's image names.
//
// Base is a directory, exported to the guest over virtio-fs as the read-only
// lower layer, or an ext4 image attached as a read-only disk. Initrd is the
// initramfs that stacks the base with the environment's writable layer.
type Image struct {
	Base   string `yaml:"base"`
	Initrd string `yaml:"initrd"`
}

// Config is what the runtime needs from the worker's configuration.
type Config struct {
	// StateDir holds one directory per environment.
	StateDir string
	// ImagesDir is the worker's local store of the images its environments
	// boot from.
	ImagesDir string
	// Kernel is the guest kernel every environment boots.
	Kernel string
	// Images maps an image reference, as a template names it, to where the
	// worker fetches it from into its store. A reference not listed here
	// cannot be run on this worker.
	Images map[string]Image
	// UpperGiB and DockerGiB size each environment's writable layer and
	// Docker store. The files are sparse, so this is a ceiling rather than
	// space spent.
	UpperGiB  int
	DockerGiB int
	// DaxMiB sizes the window a virtio-fs base is mapped through. Zero
	// reads every file through the backend instead.
	DaxMiB int
	// BootTimeout is how long an agent has to dial back after the monitor
	// starts.
	BootTimeout time.Duration
	Log         *slog.Logger
}

func (c *Config) defaults() {
	if c.UpperGiB == 0 {
		c.UpperGiB = 16
	}
	if c.DockerGiB == 0 {
		c.DockerGiB = 24
	}
	if c.BootTimeout == 0 {
		c.BootTimeout = 3 * time.Minute
	}
	if c.Log == nil {
		c.Log = slog.Default()
	}
}

// Runtime runs environments as virtual machines. It implements the worker's
// Runtime interface.
type Runtime struct {
	cfg     Config
	store   *store
	ctx     context.Context
	cancel  context.CancelFunc
	mu      sync.Mutex
	envs    map[string]*machine
	changed chan struct{}
	wg      sync.WaitGroup
}

// New checks that this host can run environments and returns a runtime.
//
// Environments with a directory left in StateDir by an earlier run are
// reported as stopped: their machines went with the process that ran them,
// and their disks are still there for the next start.
func New(cfg Config) (*Runtime, error) {
	cfg.defaults()
	if err := preflight(&cfg); err != nil {
		return nil, err
	}
	if err := os.MkdirAll(cfg.StateDir, 0o755); err != nil {
		return nil, err
	}
	st, err := newStore(cfg.ImagesDir, cfg.Images)
	if err != nil {
		return nil, err
	}
	ctx, cancel := context.WithCancel(context.Background())
	r := &Runtime{cfg: cfg, store: st, ctx: ctx, cancel: cancel, envs: map[string]*machine{},
		changed: make(chan struct{}, 1)}

	entries, err := os.ReadDir(cfg.StateDir)
	if err != nil {
		return nil, err
	}
	for _, e := range entries {
		if e.IsDir() {
			r.adopt(e.Name())
		}
	}
	return r, nil
}

func (r *Runtime) Changed() <-chan struct{} { return r.changed }

func (r *Runtime) notify() {
	select {
	case r.changed <- struct{}{}:
	default:
	}
}

func (r *Runtime) Observe() []api.ObservedEnvironment {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make([]api.ObservedEnvironment, 0, len(r.envs))
	for id, m := range r.envs {
		phase, reason := m.status()
		o := api.ObservedEnvironment{ID: id, Phase: phase, Reason: reason}
		m.mu.Lock()
		if m.stats != nil && phase == api.PhaseRunning {
			s := *m.stats
			o.Stats = &s
		}
		m.mu.Unlock()
		out = append(out, o)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].ID < out[j].ID })
	return out
}

// Apply hands an environment's spec to its controller, starting one if this
// is the first the runtime has heard of it.
func (r *Runtime) Apply(spec api.EnvironmentSpec) {
	r.mu.Lock()
	m, ok := r.envs[spec.ID]
	if !ok {
		if spec.Desired == api.DesiredDeleted {
			// Never held here, so there is nothing to delete.
			r.mu.Unlock()
			return
		}
		m = r.newMachine(spec.ID, api.PhaseStopped)
	}
	r.mu.Unlock()
	m.want(spec)
}

// adopt takes on an environment found on disk.
func (r *Runtime) adopt(id string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.newMachine(id, api.PhaseStopped)
}

func (r *Runtime) newMachine(id string, phase api.Phase) *machine {
	m := &machine{
		rt:    r,
		id:    id,
		dir:   filepath.Join(r.cfg.StateDir, id),
		phase: phase,
		nudge: make(chan struct{}, 1),
		log:   r.cfg.Log.With("environment", id),
	}
	r.envs[id] = m
	r.wg.Add(1)
	go func() {
		defer r.wg.Done()
		m.control(r.ctx)
	}()
	return m
}

func (r *Runtime) forget(id string) {
	r.mu.Lock()
	delete(r.envs, id)
	r.mu.Unlock()
	r.notify()
}

// Shutdown powers every machine off and waits for them, or for ctx.
//
// A worker that is stopping takes its machines with it: nothing would be
// left to supervise them, answer their agents, or notice them exit. Their
// disks are kept, so they start again where they left off.
func (r *Runtime) Shutdown(ctx context.Context) error {
	r.cancel()
	done := make(chan struct{})
	go func() {
		r.wg.Wait()
		close(done)
	}()
	select {
	case <-done:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

// machine is one environment and the goroutine that looks after it.
type machine struct {
	rt  *Runtime
	id  string
	dir string
	log *slog.Logger

	mu     sync.Mutex
	spec   *api.EnvironmentSpec
	phase  api.Phase
	reason string
	// running is the booted machine, while there is one.
	running *instance
	// cancelBoot interrupts a boot in progress when what is wanted changes.
	cancelBoot context.CancelFunc
	// stats is the latest measurement of the running machine.
	stats *api.EnvironmentStats

	nudge chan struct{}
}

func (m *machine) status() (api.Phase, string) {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.phase, m.reason
}

func (m *machine) set(phase api.Phase, reason string) {
	m.mu.Lock()
	changed := m.phase != phase || m.reason != reason
	m.phase, m.reason = phase, reason
	m.mu.Unlock()
	if changed {
		m.log.Info("environment", "phase", phase, "reason", reason)
		m.rt.notify()
	}
}

// want records what the environment should be doing and wakes its
// controller. A boot in progress is interrupted if it stops being wanted.
func (m *machine) want(spec api.EnvironmentSpec) {
	m.mu.Lock()
	m.spec = &spec
	if spec.Desired != api.DesiredRunning && m.cancelBoot != nil {
		m.cancelBoot()
	}
	m.mu.Unlock()
	select {
	case m.nudge <- struct{}{}:
	default:
	}
}

// control is the environment's controller. It acts on the latest spec after
// every nudge, and on the machine exiting.
func (m *machine) control(ctx context.Context) {
	for {
		var exited <-chan struct{}
		m.mu.Lock()
		if m.running != nil {
			exited = m.running.exited
		}
		m.mu.Unlock()

		select {
		case <-ctx.Done():
			m.powerOff()
			return
		case <-m.nudge:
		case <-exited:
			m.lost()
			continue
		}

		m.mu.Lock()
		spec := m.spec
		running := m.running != nil
		failed := m.phase == api.PhaseFailed
		m.mu.Unlock()
		if spec == nil {
			continue
		}

		switch spec.Desired {
		case api.DesiredRunning:
			// A failed environment stays failed until it is stopped and
			// started again: retrying on every desired set would hide the
			// failure in a loop.
			if !running && !failed {
				m.boot(ctx, *spec)
			}
		case api.DesiredStopped:
			if running {
				m.set(api.PhaseStopping, "")
				m.powerOff()
			}
			m.set(api.PhaseStopped, "")
		case api.DesiredDeleted:
			m.set(api.PhaseDeleting, "")
			m.powerOff()
			if pdir, err := passtDir(m.id); err == nil {
				os.RemoveAll(pdir)
			}
			if err := os.RemoveAll(m.dir); err != nil {
				m.set(api.PhaseFailed, "removing its disks: "+err.Error())
				continue
			}
			m.rt.forget(m.id)
			return
		}
	}
}

// boot starts the machine and provisions it, reporting each step.
func (m *machine) boot(ctx context.Context, spec api.EnvironmentSpec) {
	bootCtx, cancel := context.WithCancel(ctx)
	m.mu.Lock()
	m.cancelBoot = cancel
	m.mu.Unlock()
	defer func() {
		m.mu.Lock()
		m.cancelBoot = nil
		m.mu.Unlock()
		cancel()
	}()

	inst, err := m.start(bootCtx, spec)
	if err != nil {
		if bootCtx.Err() != nil && ctx.Err() == nil {
			// Interrupted because what is wanted changed; the controller
			// deals with what is wanted instead.
			m.set(api.PhaseStopped, "")
		} else {
			m.set(api.PhaseFailed, err.Error())
		}
		select {
		case m.nudge <- struct{}{}:
		default:
		}
		return
	}
	m.mu.Lock()
	m.running = inst
	m.mu.Unlock()
	m.set(api.PhaseRunning, "")
	go m.sample(inst, spec.Spec.CPUs)
}

// lost handles a machine that exited without being asked to.
func (m *machine) lost() {
	m.mu.Lock()
	inst := m.running
	m.running = nil
	m.mu.Unlock()
	if inst == nil {
		return
	}
	reason := "the machine stopped on its own"
	if err := inst.exitErr(); err != nil {
		reason = err.Error()
	}
	inst.close()
	m.set(api.PhaseFailed, reason)
}

// powerOff stops the machine if one is running, cleanly if it can.
func (m *machine) powerOff() {
	m.mu.Lock()
	inst := m.running
	m.running = nil
	m.mu.Unlock()
	if inst != nil {
		inst.shutdown(m.log)
	}
}

var errUnsupported = errors.New("not supported by this worker")

// resolve returns the local copy of an environment's image, fetching it
// into the store first if need be.
func (m *machine) resolve(ctx context.Context, spec api.EnvironmentSpec) (Image, error) {
	if _, err := os.Stat(m.rt.store.path(spec.Spec.Image)); err != nil {
		m.set(api.PhaseStarting, "fetching the image")
	}
	return m.rt.store.get(ctx, spec.Spec.Image)
}

// Images reports the local store, with the environments using each image.
func (r *Runtime) Images() []api.LocalImage {
	imgs := r.store.list()
	users := r.imageUsers()
	for i := range imgs {
		if u := users[imgs[i].Ref]; u != nil {
			imgs[i].Environments = u
		}
	}
	return imgs
}

// RemoveImage deletes an image's local copy, unless an environment on this
// worker still uses it -- stopped or not, since starting it again boots
// from that copy.
func (r *Runtime) RemoveImage(ref string) {
	if len(r.imageUsers()[ref]) > 0 {
		return
	}
	if err := r.store.remove(ref); err != nil {
		r.cfg.Log.Warn("removing an image", "image", ref, "err", err)
		return
	}
	r.notify()
}

// imageUsers maps each image to the environments here that use it.
func (r *Runtime) imageUsers() map[string][]string {
	r.mu.Lock()
	defer r.mu.Unlock()
	users := map[string][]string{}
	for id, m := range r.envs {
		m.mu.Lock()
		if m.spec != nil && m.spec.Desired != api.DesiredDeleted {
			users[m.spec.Spec.Image] = append(users[m.spec.Spec.Image], id)
		}
		m.mu.Unlock()
	}
	for _, u := range users {
		sort.Strings(u)
	}
	return users
}
