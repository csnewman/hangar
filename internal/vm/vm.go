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
// StateDir: its writable layer and Docker store, its console log, a mark
// recording that provisioning is done, and which copy of its image it was
// made over. Stopping keeps the directory, so the
// next start boots the same disks without cloning again; deleting removes it.
package vm

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/ch"
	"github.com/csnewman/hangar/internal/code"
	"github.com/csnewman/hangar/internal/desktop"
	"github.com/csnewman/hangar/internal/procs"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/sshd"
	"github.com/csnewman/hangar/internal/terminal"
	"github.com/csnewman/hangar/internal/vscode"
)

// Image is how a worker finds the base an environment's image names.
//
// Base is the image's root filesystem, a directory, exported to the guest
// over virtio-fs as the read-only lower layer; every environment on the
// worker using the image shares it, and its page cache. An image carries
// nothing else: the kernel and the initramfs are the node's.
type Image struct {
	Base string `yaml:"base"`
	// ID names what Base holds when the directory itself cannot: a stack of
	// layers is mounted afresh after a restart, and is the same image. Empty
	// for a directory that is the image.
	ID string `yaml:"-"`
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
	// Agent is the hangar-agent every environment runs, built for the
	// guest's architecture. It is the node's, not the image's: each boot's
	// initramfs is the agent, as init, so it always matches the worker that
	// talks to it.
	Agent string
	// Editor is the editor disk, attached read-only to every environment;
	// the agent mounts it and runs VS Code's server from it. Empty leaves
	// environments without an editor.
	Editor string
	// Images maps an image reference, as a template names it, to a local
	// source the worker copies it from into its store. Any other reference
	// is pulled from its registry.
	Images map[string]Image
	// Registries are the credentials pulls sign in to registries with, by
	// registry host.
	Registries map[string]RegistryAuth
	// UpperGiB and DockerGiB size each environment's writable layer and
	// Docker store. The files are sparse, so this is a ceiling rather than
	// space spent.
	UpperGiB  int
	DockerGiB int
	// DaxMiB sizes the window a virtio-fs base is mapped through. Zero
	// reads every file through the backend instead.
	DaxMiB int
	// GPUVenus offers an environment with a virtual GPU Vulkan as well as
	// OpenGL, and GPUVenusRestore carries its Vulkan state across a suspend.
	GPUVenus, GPUVenusRestore bool
	// GPUWindowMiB sizes the window such an environment maps GPU resources
	// into.
	GPUWindowMiB int
	// GPURenderer and GPUDevice choose what renders for such environments
	// (ch.GPUEnv).
	GPURenderer, GPUDevice string
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
	if c.GPUWindowMiB == 0 {
		c.GPUWindowMiB = 512
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
	cfg    Config
	store  *ImageStore
	ctx    context.Context
	cancel context.CancelFunc
	mu     sync.Mutex
	envs   map[string]*machine
	// applied is set by the first spec the server hands over. The worker
	// applies a desired set whole, so from then on an environment with no
	// spec is one the server has no record of.
	applied bool
	changed chan struct{}
	wg      sync.WaitGroup
	// gpu is what virtual GPUs render with here, and gpuEnv the GPU
	// backend's environment for it; gpu is nil without a GPU backend.
	gpu    *api.GPUInfo
	gpuEnv []string
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
	st, err := NewImageStore(cfg.ImagesDir, cfg.Images, cfg.Registries)
	if err != nil {
		return nil, err
	}
	gpuEnv, err := ch.GPUEnv(cfg.GPURenderer, cfg.GPUDevice)
	if err != nil {
		return nil, fmt.Errorf("vm.gpu: %w", err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	r := &Runtime{cfg: cfg, store: st, ctx: ctx, cancel: cancel, envs: map[string]*machine{},
		changed: make(chan struct{}, 1), gpu: probeGPU(ctx, cfg, gpuEnv), gpuEnv: gpuEnv}
	go r.watchHugePages()

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

// probeGPU asks the GPU backend what it renders with, as the worker's
// configuration has it render, and decides whether a virtual GPU is on
// offer: not when it asks for hardware and there is only the CPU, nor when
// the backend cannot render at all. It is nil without a GPU backend.
func probeGPU(ctx context.Context, cfg Config, env []string) *api.GPUInfo {
	if _, err := ch.FindGpuBackend(); err != nil {
		return nil
	}
	info := &api.GPUInfo{Renderer: cfg.GPURenderer, Device: cfg.GPUDevice}
	if info.Renderer == "" {
		info.Renderer = ch.RendererAuto
	}
	p, err := ch.ProbeGPU(ctx, env)
	if err != nil {
		info.Unavailable = err.Error()
		cfg.Log.Warn("probing the GPU backend: environments are offered no virtual GPU", "err", err)
		return info
	}
	if p.GL != nil {
		info.GL = &api.GPURenderer{Vendor: p.GL.Vendor, Renderer: p.GL.Renderer, Version: p.GL.Version,
			Software: p.GL.Software}
	}
	info.GLError, info.VulkanError = p.GLError, p.VulkanError
	for _, d := range p.Vulkan {
		info.Vulkan = append(info.Vulkan, api.GPUDevice{Name: d.Name, Software: d.Software})
	}
	switch {
	case info.GL == nil:
		info.Unavailable = "the GPU backend has no OpenGL: " + info.GLError
	case info.Renderer == ch.RendererHardware && info.GL.Software:
		info.Unavailable = "the configuration asks for a GPU to render on, and this host renders on its CPU (" +
			info.GL.Renderer + ")"
	}
	if info.Unavailable != "" {
		cfg.Log.Warn("environments are offered no virtual GPU", "why", info.Unavailable)
	} else {
		cfg.Log.Info("virtual GPUs render with", "gl", info.GL.Renderer, "software", info.GL.Software)
	}
	return info
}

// GPU is what virtual GPUs render with here, nil without a GPU backend.
func (r *Runtime) GPU() *api.GPUInfo { return r.gpu }

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
		if m.image != nil {
			o.ImageDigest = m.image.Digest
		}
		if m.progress != nil && phase == api.PhaseStarting {
			p := *m.progress
			o.Progress = &p
		}
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
	r.applied = true
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

// adopt takes on an environment found on disk: suspended if it was
// suspended, and otherwise stopped.
func (r *Runtime) adopt(id string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	phase := api.PhaseStopped
	if _, err := os.Stat(filepath.Join(r.cfg.StateDir, id, suspendedMark)); err == nil {
		phase = api.PhaseSuspended
	}
	r.newMachine(id, phase)
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
	m.image = readImagePin(m.dir)
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
	r.prune()
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
	// progress is the step a start is on, and how much of it is done.
	progress *api.Progress
	// measured is when a measurement of progress was last announced, and
	// measuredDone how much was done then, for the rate.
	measured     time.Time
	measuredDone int64
	// running is the booted machine, while there is one.
	running *Instance
	// cancelBoot interrupts a boot in progress when what is wanted changes.
	cancelBoot context.CancelFunc
	// stats is the latest measurement of the running machine.
	stats *api.EnvironmentStats
	// suspendFailed is set when suspending failed, so it is not tried
	// again until something else is asked for.
	suspendFailed bool
	// image is the copy of its image the environment was made over, once
	// it has first started.
	image *api.ImageCopy

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
	if phase != api.PhaseStarting {
		m.progress = nil
	}
	m.mu.Unlock()
	if changed {
		m.log.Info("environment", "phase", phase, "reason", reason)
		m.rt.notify()
	}
}

// step records the step a start is on, with the reason to show for it. A
// new step, or a new part of one -- a clone going from receiving objects to
// resolving deltas -- is measured afresh.
func (m *machine) step(step api.Step, reason string) {
	m.mu.Lock()
	if m.progress == nil || m.progress.Step != step || m.reason != reason {
		m.progress = &api.Progress{Step: step}
		m.measured, m.measuredDone = time.Time{}, 0
	}
	m.mu.Unlock()
	m.set(api.PhaseStarting, reason)
}

// measureEvery is how often a step's measurement is announced: each one
// is a report to the server.
const measureEvery = time.Second

// measure records how much of the current step is done. It is announced
// at most every measureEvery, and when the step completes.
func (m *machine) measure(done, total int64, unit string) {
	m.mu.Lock()
	if m.progress == nil || m.phase != api.PhaseStarting {
		m.mu.Unlock()
		return
	}
	p := m.progress
	// The first measurement of a step is where its rate is measured from.
	fresh := m.measured.IsZero()
	p.Done, p.Total, p.Unit = done, total, unit
	now := time.Now()
	since := now.Sub(m.measured)
	announce := since >= measureEvery || (total > 0 && done >= total)
	if announce {
		// The rate is smoothed over the last few announcements, so it
		// follows a download that speeds up or stalls without jumping.
		if !fresh && since > 0 && done >= m.measuredDone {
			latest := float64(done-m.measuredDone) / since.Seconds()
			if p.Rate == 0 {
				p.Rate = latest
			} else {
				p.Rate = 0.7*p.Rate + 0.3*latest
			}
		}
		m.measured, m.measuredDone = now, done
	}
	m.mu.Unlock()
	if announce {
		m.rt.notify()
	}
}

// want records what the environment should be doing and wakes its
// controller. A boot in progress is interrupted if it stops being wanted.
func (m *machine) want(spec api.EnvironmentSpec) {
	m.mu.Lock()
	if m.spec == nil || m.spec.Desired != spec.Desired {
		m.suspendFailed = false
	}
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
		case api.DesiredSuspended:
			// Only a running machine has memory to keep. One that is not
			// stays as it is: stopped, failed, or already suspended.
			if running && !m.suspendFailed {
				if err := m.suspend(ctx); err != nil {
					// Tried once per request: the guest runs on, saying
					// why, until it is asked for something else.
					m.log.Warn("suspending", "err", err)
					m.suspendFailed = true
					m.set(api.PhaseRunning, "could not suspend: "+err.Error())
				} else {
					m.set(api.PhaseSuspended, "")
				}
			}
		case api.DesiredStopped:
			if running {
				m.set(api.PhaseStopping, "")
				m.powerOff()
			}
			DiscardSuspend(m.dir)
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
	if err := inst.Err(); err != nil {
		reason = err.Error()
	}
	inst.Close()
	m.set(api.PhaseFailed, reason)
}

// powerOff stops the machine if one is running, cleanly if it can.
func (m *machine) powerOff() {
	m.mu.Lock()
	inst := m.running
	m.running = nil
	m.mu.Unlock()
	if inst != nil {
		inst.Shutdown(m.log)
	}
}

var errUnsupported = errors.New("not supported by this worker")

// imagePinFile, in an environment's directory, names the copy of its image
// it was made over.
const imagePinFile = "image.json"

func readImagePin(dir string) *api.ImageCopy {
	b, err := os.ReadFile(filepath.Join(dir, imagePinFile))
	if err != nil {
		return nil
	}
	var c api.ImageCopy
	if json.Unmarshal(b, &c) != nil || c.Ref == "" {
		return nil
	}
	return &c
}

// resolve returns the copy of its image the environment boots from,
// fetching it into the store first if need be.
//
// On its first start that is whatever the image's reference names then,
// and the environment is pinned to it: every later start boots the same
// copy, whatever the reference has moved on to, because the environment's
// writable layer was made over it.
func (m *machine) resolve(ctx context.Context, spec api.EnvironmentSpec) (Image, error) {
	ref := spec.Spec.Image
	m.mu.Lock()
	pin := m.image
	m.mu.Unlock()
	fresh := pin == nil || pin.Ref != ref
	if fresh {
		c, err := m.choose(ctx, ref)
		if err != nil {
			return Image{}, err
		}
		pin = &c
	}
	img, err := m.rt.store.Get(ctx, *pin, func(p FetchProgress) {
		switch p.Stage {
		case StageCopy:
			m.step(api.StepDownload, "copying the image")
		case StageDownload:
			m.step(api.StepDownload, "downloading the image")
			m.measure(p.Done, p.Total, "bytes")
		case StageUnpack:
			reason := "unpacking the image"
			if p.Layers > 1 {
				reason += fmt.Sprintf(": layer %d of %d", p.Layer, p.Layers)
			}
			m.step(api.StepUnpack, reason)
			m.measure(p.Done, p.Total, "bytes")
		}
	})
	if err != nil {
		return Image{}, err
	}
	if fresh {
		if err := os.MkdirAll(m.dir, 0o755); err != nil {
			return Image{}, err
		}
		b, err := json.Marshal(pin)
		if err != nil {
			return Image{}, err
		}
		if err := os.WriteFile(filepath.Join(m.dir, imagePinFile), b, 0o644); err != nil {
			return Image{}, err
		}
		m.mu.Lock()
		m.image = pin
		m.mu.Unlock()
		m.rt.prune()
		m.rt.notify()
	}
	return img, nil
}

// choose picks the copy of ref an environment not yet pinned to one is
// made over: what ref names at its first start, or, when that cannot be
// looked up, the newest copy held. An environment with disks but no pin
// was made over a copy nothing recorded, and is given the oldest held, the
// one taken first.
func (m *machine) choose(ctx context.Context, ref string) (api.ImageCopy, error) {
	if m.hasDisks() {
		if d, ok := m.rt.store.Oldest(ref); ok {
			return api.ImageCopy{Ref: ref, Digest: d}, nil
		}
	}
	d, err := m.rt.store.Current(ctx, ref)
	if err == nil {
		return api.ImageCopy{Ref: ref, Digest: d}, nil
	}
	if held, ok := m.rt.store.Newest(ref); ok {
		m.log.Warn("looking up the image; using the newest copy held", "image", ref, "err", err)
		return api.ImageCopy{Ref: ref, Digest: held}, nil
	}
	return api.ImageCopy{}, err
}

func (m *machine) hasDisks() bool {
	_, err := os.Stat(filepath.Join(m.dir, "upper.ext4"))
	return err == nil
}

// Images reports the local store, with the environments using each copy.
func (r *Runtime) Images() []api.LocalImage {
	imgs := r.store.List()
	for i := range imgs {
		if u := r.imageUsers(api.ImageCopy{Ref: imgs[i].Ref, Digest: imgs[i].Digest}); u != nil {
			imgs[i].Environments = u
		}
	}
	return imgs
}

// UseRegistry says where Hangar's own registry is (see ImageStore.UseRegistry).
func (r *Runtime) UseRegistry(host, url, credential string) {
	r.store.UseRegistry(host, url, credential)
}

// RemoveImage deletes a copy of an image, unless an environment on this
// worker still uses it -- stopped or not, since starting it again boots
// from that copy.
func (r *Runtime) RemoveImage(c api.ImageCopy) {
	if len(r.imageUsers(c)) > 0 {
		return
	}
	if err := r.store.Remove(c); err != nil {
		r.cfg.Log.Warn("removing an image", "image", c.Ref, "digest", c.Digest, "err", err)
		return
	}
	r.notify()
}

// prune deletes the copies of images their references have moved on from
// that no environment here uses.
func (r *Runtime) prune() {
	if r.unaccounted() {
		return
	}
	removed, err := r.store.Prune(func(c api.ImageCopy) bool { return len(r.imageUsers(c)) > 0 })
	for _, c := range removed {
		r.cfg.Log.Info("removed a superseded copy of an image", "image", c.Ref, "digest", c.Digest)
	}
	if err != nil {
		r.cfg.Log.Warn("removing superseded copies of images", "err", err)
	}
}

// unaccounted is whether an environment here has disks but no pin, and no
// spec yet to say what its image is: it could be using any copy. Once the
// server has handed over its specs, one still without is unknown to it and
// cannot be started, so it holds on to nothing.
func (r *Runtime) unaccounted() bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.applied {
		return false
	}
	for _, m := range r.envs {
		m.mu.Lock()
		unknown := m.spec == nil && m.image == nil
		m.mu.Unlock()
		if unknown && m.hasDisks() {
			return true
		}
	}
	return false
}

// imageUsers lists the environments here that use a copy of an image: the
// ones pinned to it, and any with disks but no pin whose image is the
// same reference, which could have been made over any copy of it.
func (r *Runtime) imageUsers(c api.ImageCopy) []string {
	r.mu.Lock()
	defer r.mu.Unlock()
	var users []string
	for id, m := range r.envs {
		m.mu.Lock()
		spec, pin := m.spec, m.image
		m.mu.Unlock()
		if spec != nil && spec.Desired == api.DesiredDeleted {
			continue
		}
		if pin != nil && *pin == c || pin == nil && spec != nil && spec.Spec.Image == c.Ref && m.hasDisks() {
			users = append(users, id)
		}
	}
	sort.Strings(users)
	return users
}

// DialTerminal connects to a running environment's terminal sessions, which
// its agent holds in the guest.
func (r *Runtime) DialTerminal(ctx context.Context, id string) (net.Conn, error) {
	m, err := r.runningMachine(id)
	if err != nil {
		return nil, err
	}
	return dialGuest(ctx, filepath.Join(m.dir, "run", "vsock.sock"), terminal.Port)
}

// DialEditor opens one connection to a running environment's editor, which
// its agent runs in the guest.
func (r *Runtime) DialEditor(ctx context.Context, id string) (net.Conn, error) {
	m, err := r.runningMachine(id)
	if err != nil {
		return nil, err
	}
	return dialGuest(ctx, filepath.Join(m.dir, "run", "vsock.sock"), vscode.Port)
}

// DialCode opens one connection to a running environment's Code tab
// service, which its agent runs in the guest.
func (r *Runtime) DialSSH(ctx context.Context, id string) (net.Conn, error) {
	m, err := r.runningMachine(id)
	if err != nil {
		return nil, err
	}
	return dialGuest(ctx, filepath.Join(m.dir, "run", "vsock.sock"), sshd.Port)
}

func (r *Runtime) DialProcesses(ctx context.Context, id string) (net.Conn, error) {
	m, err := r.runningMachine(id)
	if err != nil {
		return nil, err
	}
	return dialGuest(ctx, filepath.Join(m.dir, "run", "vsock.sock"), procs.Port)
}

func (r *Runtime) DialProfile(ctx context.Context, id string) (net.Conn, error) {
	m, err := r.runningMachine(id)
	if err != nil {
		return nil, err
	}
	return dialGuest(ctx, filepath.Join(m.dir, "run", "vsock.sock"), profile.Port)
}

func (r *Runtime) DialCode(ctx context.Context, id string) (net.Conn, error) {
	m, err := r.runningMachine(id)
	if err != nil {
		return nil, err
	}
	return dialGuest(ctx, filepath.Join(m.dir, "run", "vsock.sock"), code.Port)
}

// DialDesktop opens one VNC connection to a running environment's desktop,
// which its agent serves in the guest.
func (r *Runtime) DialDesktop(ctx context.Context, id string) (net.Conn, error) {
	m, err := r.runningMachine(id)
	if err != nil {
		return nil, err
	}
	return dialGuest(ctx, filepath.Join(m.dir, "run", "vsock.sock"), desktop.Port)
}

// runningMachine is the environment here with this ID, if it is running.
func (r *Runtime) runningMachine(id string) (*machine, error) {
	r.mu.Lock()
	m, ok := r.envs[id]
	r.mu.Unlock()
	if !ok {
		return nil, fmt.Errorf("environment %s is not on this worker", id)
	}
	m.mu.Lock()
	running := m.running != nil
	m.mu.Unlock()
	if !running {
		return nil, fmt.Errorf("environment %s is not running", id)
	}
	return m, nil
}

// dialGuest opens a connection to a port in the guest through the monitor's
// vsock socket: Cloud Hypervisor carries each guest's vsock on a unix socket,
// and a host process reaches a guest port by connecting and asking for it.
func dialGuest(ctx context.Context, socket string, port uint32) (net.Conn, error) {
	var d net.Dialer
	conn, err := d.DialContext(ctx, "unix", socket)
	if err != nil {
		return nil, err
	}
	if deadline, ok := ctx.Deadline(); ok {
		conn.SetDeadline(deadline)
	}
	if _, err := fmt.Fprintf(conn, "CONNECT %d\n", port); err != nil {
		conn.Close()
		return nil, err
	}
	br := bufio.NewReader(conn)
	line, err := br.ReadString('\n')
	if err != nil {
		conn.Close()
		return nil, fmt.Errorf("connecting to guest port %d: %w", port, err)
	}
	if !strings.HasPrefix(line, "OK ") {
		conn.Close()
		return nil, fmt.Errorf("connecting to guest port %d: %s", port, strings.TrimSpace(line))
	}
	conn.SetDeadline(time.Time{})
	return &bufferedConn{Conn: conn, r: br}, nil
}

// bufferedConn is a connection whose first bytes were read into a buffer
// while its handshake was.
type bufferedConn struct {
	net.Conn
	r *bufio.Reader
}

func (c *bufferedConn) Read(p []byte) (int, error) { return c.r.Read(p) }

// suspend writes the running machine to disk and stops it. If it cannot,
// the guest carries on running as it was.
func (m *machine) suspend(ctx context.Context) error {
	m.mu.Lock()
	inst := m.running
	m.mu.Unlock()
	if inst == nil {
		return errors.New("the machine is not running")
	}
	m.set(api.PhaseSuspending, "")
	start := time.Now()
	sctx, cancel := context.WithTimeout(context.WithoutCancel(ctx), 10*time.Minute)
	defer cancel()
	// Taken from the controller before it goes, so its exit is not seen
	// as the machine stopping on its own.
	m.mu.Lock()
	m.running = nil
	m.mu.Unlock()
	if err := inst.Suspend(sctx); err != nil {
		m.mu.Lock()
		m.running = inst
		m.mu.Unlock()
		return err
	}
	m.log.Info("suspended", "seconds", time.Since(start).Seconds())
	return nil
}
