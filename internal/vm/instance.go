package vm

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"syscall"
	"time"

	"github.com/csnewman/hangar/internal/agent"
	"github.com/csnewman/hangar/internal/ch"
	"github.com/csnewman/hangar/internal/host"
)

// InstanceConfig is one virtual machine: what it boots, what it is given,
// and where it keeps what it keeps. The worker's environments and `hangar
// run` both boot through it.
type InstanceConfig struct {
	// ID names the machine's sockets. It must be unique on the host.
	ID string `json:"id"`
	// Name is the monitor's name for the machine.
	Name string `json:"name"`
	// Dir holds what outlives a boot: the backends' saved state, a
	// suspended machine's snapshot, and the monitor's log.
	Dir string `json:"dir"`

	Kernel string `json:"kernel"`
	// Agent is the guest agent, each boot's initramfs and then its init.
	Agent string `json:"agent"`
	// Base is the image's root filesystem, served over virtio-fs.
	Base string `json:"base"`
	// Disks follow: the writable layer first, which the agent mounts as
	// /dev/vda, then the Docker disk, then any read-only ones.
	Disks []ch.Disk `json:"disks"`

	MemoryMiB int `json:"memory_mib"`
	CPUs      int `json:"cpus"`
	// DaxMiB sizes the window the base's files are mapped through. Zero
	// reads every file through the backend instead.
	DaxMiB int `json:"dax_mib"`
	// Net gives the guest outbound networking, through passt.
	Net bool `json:"net"`

	GPU bool `json:"gpu"`
	// GPUVenus offers Vulkan, with GPU.
	GPUVenus bool `json:"gpu_venus,omitempty"`
	// GPUVenusRestore carries Vulkan state across a suspend.
	GPUVenusRestore bool `json:"gpu_venus_restore,omitempty"`
	GPUWindowMiB    int  `json:"gpu_window_mib,omitempty"`

	// ConsoleFile receives the guest's console; empty is the monitor's
	// stdio.
	ConsoleFile  string `json:"console_file,omitempty"`
	ExtraCmdline string `json:"extra_cmdline,omitempty"`
	Seccomp      string `json:"seccomp,omitempty"`

	// Monitor receives the monitor's own output, and Stdin feeds its
	// console when the console is stdio. A nil Monitor writes
	// <Dir>/monitor.log.
	Monitor io.Writer `json:"-"`
	Stdin   io.Reader `json:"-"`
	// Verbose has the backends log to stderr.
	Verbose bool `json:"-"`

	// AgentWait is how long the agent has to dial back.
	AgentWait time.Duration `json:"-"`
	// Progress, if set, is told each step of a boot.
	Progress func(step string) `json:"-"`
	Log      *slog.Logger      `json:"-"`
}

// Instance is a running machine: the monitor, its backends, and the
// agent's session with the guest.
type Instance struct {
	cfg      InstanceConfig
	cmd      *exec.Cmd
	api      *ch.API
	session  *agent.Session
	server   *agent.Server
	cancel   context.CancelFunc
	backends []interface{ Close() error }
	exited   chan struct{}
	// fs and gpu are the backends that keep guest state a suspend must
	// save; gpu is nil without a GPU.
	fs      *ch.FsBackend
	gpu     *ch.GpuBackend
	resumed bool

	mu  sync.Mutex
	err error
}

// Session is the agent's session with the guest.
func (i *Instance) Session() *agent.Session { return i.session }

// Resumed reports whether the machine came back from a suspend rather than
// booting.
func (i *Instance) Resumed() bool { return i.resumed }

// Exited is closed when the monitor exits.
func (i *Instance) Exited() <-chan struct{} { return i.exited }

// Pid is the monitor's process ID.
func (i *Instance) Pid() int {
	if i.cmd == nil || i.cmd.Process == nil {
		return 0
	}
	return i.cmd.Process.Pid
}

// Err is why the monitor exited, if not by being asked to.
func (i *Instance) Err() error {
	i.mu.Lock()
	defer i.mu.Unlock()
	return i.err
}

// runDir is where a machine's sockets are, readable only by its owner.
func (c InstanceConfig) runDir() string { return filepath.Join(c.Dir, "run") }

// Boot starts a machine and waits for its agent: it resumes it if Dir holds
// a snapshot of it taken against the same base and read-only disks, and
// boots it from its disks otherwise. On error, everything it started has
// been stopped again.
func Boot(ctx context.Context, cfg InstanceConfig) (_ *Instance, err error) {
	if cfg.Log == nil {
		cfg.Log = slog.Default()
	}
	if cfg.AgentWait == 0 {
		cfg.AgentWait = 3 * time.Minute
	}
	progress := func(step string) {
		if cfg.Progress != nil {
			cfg.Progress(step)
		}
	}
	if err := os.MkdirAll(cfg.Dir, 0o755); err != nil {
		return nil, err
	}
	run := cfg.runDir()
	if err := os.MkdirAll(run, 0o700); err != nil {
		return nil, err
	}

	resuming := false
	if rec := cfg.suspended(); rec != nil {
		if rec.equal(cfg.record()) {
			resuming = true
		} else {
			// The guest would find files it holds open replaced. Its
			// memory is given up and it boots from its disks.
			cfg.Log.Warn("the base or a read-only disk changed while the machine was suspended; booting it afresh")
			cfg.discardSuspend()
		}
	}
	if !resuming {
		cfg.clearBackendState()
	}
	caps, err := host.Detect()
	if err != nil {
		return nil, err
	}

	// Everything below lives as long as the instance. The monitor has its
	// own context so it can be stopped before its backends.
	procCtx, cancelProcs := context.WithCancel(context.Background())
	inst := &Instance{cfg: cfg, exited: make(chan struct{}), resumed: resuming}
	monCtx, cancelMon := context.WithCancel(procCtx)
	inst.cancel = cancelMon
	defer func() {
		if err != nil {
			if inst.cmd == nil {
				close(inst.exited)
			}
			inst.Close()
			cancelProcs()
		}
	}()
	inst.backends = append(inst.backends, closer(cancelProcs))

	initrd := filepath.Join(run, "initrd.img")
	if err := WriteInitrd(cfg.Agent, initrd); err != nil {
		return nil, err
	}
	ccfg := &ch.Config{
		Name:         cfg.Name,
		Kernel:       cfg.Kernel,
		Initrd:       initrd,
		Disks:        cfg.Disks,
		MemoryMB:     cfg.MemoryMiB,
		CPUs:         cfg.CPUs,
		ConsoleFile:  cfg.ConsoleFile,
		ConsoleTTY:   caps.ConsoleTTY,
		ExtraCmdline: cfg.ExtraCmdline,
		Seccomp:      cfg.Seccomp,
		GuestCID:     guestCID,
		VsockSocket:  filepath.Join(run, "vsock.sock"),
		APISocket:    filepath.Join(run, "api.sock"),
	}

	// The base is exported read-only over virtio-fs. With a DAX window its
	// files are mapped from the host's page cache, which every machine on
	// the base shares.
	var minSize uint64
	if cfg.DaxMiB > 0 {
		minSize = ch.DefaultDaxMinFileSize
	}
	fs, err := ch.StartFsBackend(procCtx, cfg.Base, filepath.Join(run, "fs.sock"), ch.DefaultVirtiofsTag,
		minSize, filepath.Join(cfg.Dir, "fs.json"), cfg.Verbose)
	if err != nil {
		return nil, err
	}
	inst.backends = append(inst.backends, fs)
	inst.fs = fs
	ccfg.VirtiofsSocket = fs.Socket()
	ccfg.VirtiofsDaxMiB = cfg.DaxMiB

	if cfg.Net {
		pdir, err := passtDir(cfg.ID)
		if err != nil {
			return nil, err
		}
		net, err := ch.StartPasst(procCtx, filepath.Join(pdir, "net.sock"), cfg.Verbose)
		if err != nil {
			return nil, err
		}
		inst.backends = append(inst.backends, net)
		ccfg.NetSocket = net.Socket()
	}

	if cfg.GPU {
		gpu, err := ch.StartGpuBackend(procCtx, filepath.Join(run, "gpu.sock"), cfg.GPUVenus, cfg.GPUVenusRestore,
			filepath.Join(cfg.Dir, "gpu.json"), cfg.Verbose)
		if err != nil {
			return nil, err
		}
		inst.backends = append(inst.backends, gpu)
		inst.gpu = gpu
		ccfg.GpuSocket = gpu.Socket()
		ccfg.GpuShmMiB = cfg.GPUWindowMiB
		if ccfg.GpuShmMiB == 0 {
			ccfg.GpuShmMiB = 512
		}
	}

	// The agent dials early in the boot, so it is listened for before the
	// monitor starts. The monitor binds the vsock path itself, and one left
	// by a monitor that was killed would stop it starting.
	_ = os.Remove(ccfg.VsockSocket)
	_ = os.Remove(ccfg.APISocket)
	inst.server, err = agent.ListenHybrid(ccfg.VsockSocket, guestCID)
	if err != nil {
		return nil, fmt.Errorf("listening for the agent: %w", err)
	}

	if resuming {
		progress("resuming")
		if err := inst.restore(ctx, monCtx, ccfg); err != nil {
			return nil, err
		}
	} else {
		progress("booting")
		args, err := ccfg.Args()
		if err != nil {
			return nil, err
		}
		if err := inst.launch(monCtx, args); err != nil {
			return nil, err
		}
		inst.api = ch.NewAPI(ccfg.APISocket)
	}

	if inst.session, err = inst.awaitAgent(ctx); err != nil {
		return nil, err
	}
	cfg.Log.Info("agent connected", "hostname", inst.session.Hello.Hostname, "kernel", inst.session.Hello.Kernel,
		"up_seconds", float64(inst.session.Hello.BootMicros)/1e6, "resumed", resuming)

	// The guest's clock is wrong on arrival: a fresh boot starts from the
	// image's build time and a resume from the moment of the suspend.
	if err := inst.session.SetClock(time.Now(), 5*time.Second); err != nil {
		cfg.Log.Warn("could not set the guest clock", "err", err)
	}
	if resuming {
		// Running again, so the snapshot is spent: resuming from it a
		// second time would roll the guest back under its own disks.
		cfg.discardSuspend()
	}
	return inst, nil
}

// launch starts the monitor and watches for it to exit.
func (i *Instance) launch(ctx context.Context, args []string) error {
	bin, err := ch.Find()
	if err != nil {
		return err
	}
	out := i.cfg.Monitor
	var logFile *os.File
	if out == nil {
		logFile, err = os.OpenFile(filepath.Join(i.cfg.Dir, "monitor.log"), os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o644)
		if err != nil {
			return err
		}
		out = logFile
	}
	cmd := exec.CommandContext(ctx, bin, args...)
	cmd.Stdout = out
	cmd.Stderr = out
	cmd.Stdin = i.cfg.Stdin
	cmd.SysProcAttr = sysProcAttr()
	cmd.Cancel = func() error { return cmd.Process.Signal(syscall.SIGTERM) }
	cmd.WaitDelay = 15 * time.Second
	if err := cmd.Start(); err != nil {
		if logFile != nil {
			logFile.Close()
		}
		return fmt.Errorf("starting cloud-hypervisor: %w", err)
	}
	i.cmd = cmd
	go func() {
		err := cmd.Wait()
		if logFile != nil {
			logFile.Close()
		}
		if err != nil && ctx.Err() == nil {
			i.mu.Lock()
			i.err = fmt.Errorf("cloud-hypervisor exited: %v%s", err, lastLine(filepath.Join(i.cfg.Dir, "monitor.log")))
			i.mu.Unlock()
		}
		close(i.exited)
	}()
	return nil
}

// restore starts the monitor from the machine's snapshot and resumes the
// guest once every backend has rebuilt what the guest holds of it.
//
// The monitor starts with nothing but its API socket: the machine, its
// devices and their sockets are all in the snapshot, at the paths they had,
// which the backends are listening on again.
func (i *Instance) restore(ctx, monCtx context.Context, ccfg *ch.Config) error {
	args := []string{"--api-socket", ccfg.APISocket}
	if ccfg.Seccomp != "" {
		args = append(args, "--seccomp", ccfg.Seccomp)
	}
	if err := i.launch(monCtx, args); err != nil {
		return err
	}
	i.api = ch.NewAPI(ccfg.APISocket)
	if err := i.api.WaitReady(ctx, 10*time.Second); err != nil {
		return err
	}
	if err := i.api.Restore(ctx, filepath.Join(i.cfg.Dir, snapshotDir)); err != nil {
		return fmt.Errorf("restoring the snapshot: %w", err)
	}
	if i.fs != nil {
		if err := i.fs.WaitRestored(ctx, 2*time.Minute); err != nil {
			return fmt.Errorf("restoring the file system backend: %w", err)
		}
	}
	if i.gpu != nil {
		if err := i.gpu.WaitRestored(ctx, 2*time.Minute); err != nil {
			return fmt.Errorf("restoring the GPU backend: %w", err)
		}
	}
	return i.api.Resume(ctx)
}

// awaitAgent waits for the guest's agent to dial back, and gives up early if
// the monitor exits or ctx ends.
func (i *Instance) awaitAgent(ctx context.Context) (*agent.Session, error) {
	type result struct {
		s   *agent.Session
		err error
	}
	got := make(chan result, 1)
	go func() {
		s, err := i.server.Accept(i.cfg.AgentWait)
		got <- result{s, err}
	}()
	select {
	case r := <-got:
		if r.err != nil {
			return nil, fmt.Errorf("the agent did not connect within %s: %v", i.cfg.AgentWait, r.err)
		}
		return r.s, nil
	case <-i.exited:
		if err := i.Err(); err != nil {
			return nil, err
		}
		return nil, errors.New("the machine stopped before its agent connected")
	case <-ctx.Done():
		i.server.Close()
		return nil, ctx.Err()
	}
}

// Close releases everything the instance holds. The monitor must already
// have exited, or be about to: backends are closed under it otherwise.
func (i *Instance) Close() {
	if i.session != nil {
		i.session.Close()
	}
	if i.server != nil {
		i.server.Close()
	}
	// The monitor goes before its backends: tearing a vhost-user backend
	// out from under a live monitor makes it report a broken device.
	i.cancel()
	<-i.exited
	for j := len(i.backends) - 1; j >= 0; j-- {
		i.backends[j].Close()
	}
}

// Shutdown powers the guest off so it flushes its disks, forces the matter
// if it does not go in time, and releases the instance.
func (i *Instance) Shutdown(log *slog.Logger) {
	if i.session != nil {
		// poweroff does not return once it has worked: the connection
		// goes with the guest. Its answer is not waited for.
		go i.session.Exec(time.Minute, "systemctl", "poweroff")
	}
	select {
	case <-i.exited:
	case <-time.After(90 * time.Second):
		log.Warn("the guest did not power off; stopping the monitor")
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		if err := i.api.Shutdown(ctx); err != nil {
			i.cmd.Process.Signal(syscall.SIGTERM)
		}
		cancel()
		select {
		case <-i.exited:
		case <-time.After(10 * time.Second):
			i.cmd.Process.Kill()
		}
	}
	i.Close()
}

type closer func()

func (c closer) Close() error { c(); return nil }

// Save writes cfg beside the machine's state, so a later Boot of a
// suspended machine is given the same backends it was suspended with.
func (c InstanceConfig) Save() error {
	b, err := json.MarshalIndent(c, "", "  ")
	if err != nil {
		return err
	}
	return writeFileAtomic(filepath.Join(c.Dir, "machine.json"), b)
}

// LoadInstanceConfig reads what Save wrote.
func LoadInstanceConfig(dir string) (InstanceConfig, error) {
	var c InstanceConfig
	b, err := os.ReadFile(filepath.Join(dir, "machine.json"))
	if err != nil {
		return c, err
	}
	err = json.Unmarshal(b, &c)
	return c, err
}

// Command is the monitor's command line for a fresh boot of cfg, one
// argument per line, for reading and editing by hand.
func (c InstanceConfig) Command() (string, error) {
	run := c.runDir()
	ccfg := &ch.Config{
		Name: c.Name, Kernel: c.Kernel, Initrd: filepath.Join(run, "initrd.img"), Disks: c.Disks,
		MemoryMB: c.MemoryMiB, CPUs: c.CPUs, ConsoleFile: c.ConsoleFile, ExtraCmdline: c.ExtraCmdline,
		Seccomp: c.Seccomp, GuestCID: guestCID, VsockSocket: filepath.Join(run, "vsock.sock"),
		APISocket: filepath.Join(run, "api.sock"), VirtiofsSocket: filepath.Join(run, "fs.sock"),
		VirtiofsDaxMiB: c.DaxMiB,
	}
	if c.Net {
		ccfg.NetSocket = filepath.Join(os.TempDir(), "hangar-"+c.ID, "net.sock")
	}
	if c.GPU {
		ccfg.GpuSocket = filepath.Join(run, "gpu.sock")
		ccfg.GpuShmMiB = c.GPUWindowMiB
	}
	return ch.PrintCommand(ccfg)
}
