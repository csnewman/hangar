package vm

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/csnewman/hangar/internal/agent"
	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/ch"
	"github.com/csnewman/hangar/internal/host"
)

// guestCID is the context ID every guest is given. Cloud Hypervisor carries
// each guest's vsock on a unix socket of its own, so the number is local to
// that socket and never has to be unique across the host.
const guestCID = 3

// provisionedMark records that an environment's template has been applied,
// so a restart boots its disks as they are rather than cloning again.
const provisionedMark = "provisioned"

// preflight refuses to start on a host that cannot run environments at all,
// rather than failing each one as it is asked for.
func preflight(cfg *Config) error {
	if _, err := host.Detect(); err != nil {
		return err
	}
	if err := ch.CheckHugePages(); err != nil {
		return err
	}
	if _, err := ch.Find(); err != nil {
		return err
	}
	if _, err := exec.LookPath("passt"); err != nil {
		return fmt.Errorf("passt not found (apt install passt): %w", err)
	}
	if _, err := exec.LookPath("mkfs.ext4"); err != nil {
		return fmt.Errorf("mkfs.ext4 not found (apt install e2fsprogs): %w", err)
	}
	if _, err := os.Stat(cfg.Kernel); err != nil {
		return fmt.Errorf("guest kernel: %w", err)
	}
	for ref, img := range cfg.Images {
		if _, err := os.Stat(img.Base); err != nil {
			return fmt.Errorf("image %s: base: %w", ref, err)
		}
		if _, err := os.Stat(img.Initrd); err != nil {
			return fmt.Errorf("image %s: initrd: %w", ref, err)
		}
	}
	return nil
}

// instance is a booted machine: the monitor, its backends, and the agent's
// session.
type instance struct {
	cmd      *exec.Cmd
	api      *ch.API
	session  *agent.Session
	server   *agent.Server
	cancel   context.CancelFunc
	backends []interface{ Close() error }
	exited   chan struct{}

	mu  sync.Mutex
	err error
}

func (i *instance) exitErr() error {
	i.mu.Lock()
	defer i.mu.Unlock()
	return i.err
}

// close releases everything the instance holds. The monitor must already
// have exited, or be about to: backends are closed under it otherwise.
func (i *instance) close() {
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

// shutdown powers the guest off so it flushes its disks, and forces the
// matter if it does not go in time.
func (i *instance) shutdown(log *slog.Logger) {
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
	i.close()
}

// start boots the environment and provisions it. On error, everything it
// started has been stopped again.
func (m *machine) start(ctx context.Context, spec api.EnvironmentSpec) (_ *instance, err error) {
	m.set(api.PhaseStarting, "preparing its disks")

	s := spec.Spec
	if s.GPU == api.GPUPassthrough {
		return nil, fmt.Errorf("GPU passthrough is %w", errUnsupported)
	}
	img, err := m.resolve(spec)
	if err != nil {
		return nil, err
	}
	caps, err := host.Detect()
	if err != nil {
		return nil, err
	}
	if err := os.MkdirAll(m.dir, 0o755); err != nil {
		return nil, err
	}
	run, err := socketDir(m.dir)
	if err != nil {
		return nil, err
	}
	upper := filepath.Join(m.dir, "upper.ext4")
	docker := filepath.Join(m.dir, "docker.ext4")
	if err := ensureDisk(ctx, upper, "hangar-upper", m.rt.cfg.UpperGiB); err != nil {
		return nil, err
	}
	if err := ensureDisk(ctx, docker, "hangar-docker", m.rt.cfg.DockerGiB); err != nil {
		return nil, err
	}

	// Everything below lives as long as the instance. The monitor has its
	// own context so it can be stopped before its backends.
	procCtx, cancelProcs := context.WithCancel(context.Background())
	inst := &instance{exited: make(chan struct{})}
	monCtx, cancelMon := context.WithCancel(procCtx)
	inst.cancel = cancelMon
	defer func() {
		if err != nil {
			if inst.cmd == nil {
				close(inst.exited)
			}
			inst.close()
			cancelProcs()
		}
	}()
	inst.backends = append(inst.backends, closer(cancelProcs))

	cfg := &ch.Config{
		Name:        spec.Name,
		Kernel:      m.rt.cfg.Kernel,
		Initrd:      img.Initrd,
		MemoryMB:    s.MemoryMiB,
		CPUs:        s.CPUs,
		ConsoleFile: filepath.Join(m.dir, "console.log"),
		ConsoleTTY:  caps.ConsoleTTY,
		GuestCID:    guestCID,
		VsockSocket: filepath.Join(run, "vsock.sock"),
		APISocket:   filepath.Join(run, "api.sock"),
	}
	if s.Display == api.DisplayNone {
		// The desktop is a unit in the image; a headless environment
		// simply never starts it.
		cfg.ExtraCmdline = "systemd.mask=hangar-desktop.service"
	}
	disks := []ch.Disk{{Path: upper}, {Path: docker}}

	st, err := os.Stat(img.Base)
	if err != nil {
		return nil, err
	}
	if st.IsDir() {
		var minSize uint64
		if m.rt.cfg.DaxMiB > 0 {
			minSize = ch.DefaultDaxMinFileSize
		}
		fs, err := ch.StartFsBackend(procCtx, img.Base, filepath.Join(run, "fs.sock"), ch.DefaultVirtiofsTag,
			minSize, filepath.Join(m.dir, "fs.json"), false)
		if err != nil {
			return nil, err
		}
		inst.backends = append(inst.backends, fs)
		cfg.VirtiofsSocket = fs.Socket()
		cfg.VirtiofsDaxMiB = m.rt.cfg.DaxMiB
	} else {
		// A base on a block device is shared read-only between every
		// environment using it, and comes first so the initramfs finds it
		// at /dev/vda.
		disks = append([]ch.Disk{{Path: img.Base, ReadOnly: true}}, disks...)
	}
	cfg.Disks = disks

	pdir, err := passtDir(m.id)
	if err != nil {
		return nil, err
	}
	net, err := ch.StartPasst(procCtx, filepath.Join(pdir, "net.sock"), false)
	if err != nil {
		return nil, err
	}
	inst.backends = append(inst.backends, net)
	cfg.NetSocket = net.Socket()

	if s.GPU == api.GPUVirtual {
		gpu, err := ch.StartGpuBackend(procCtx, filepath.Join(run, "gpu.sock"), false, false,
			filepath.Join(m.dir, "gpu.json"), false)
		if err != nil {
			return nil, err
		}
		inst.backends = append(inst.backends, gpu)
		cfg.GpuSocket = gpu.Socket()
		cfg.GpuShmMiB = 512
	}

	// The agent dials early in the boot, so it is listened for before the
	// monitor starts. The monitor binds the vsock path itself, and one left
	// by a monitor that was killed would stop it starting.
	_ = os.Remove(cfg.VsockSocket)
	_ = os.Remove(cfg.APISocket)
	inst.server, err = agent.ListenHybrid(cfg.VsockSocket, guestCID)
	if err != nil {
		return nil, fmt.Errorf("listening for the agent: %w", err)
	}

	m.set(api.PhaseStarting, "booting")
	if err := m.launch(monCtx, cfg, inst); err != nil {
		return nil, err
	}
	inst.api = ch.NewAPI(cfg.APISocket)

	sess, err := m.awaitAgent(ctx, inst)
	if err != nil {
		return nil, err
	}
	inst.session = sess
	m.log.Info("agent connected", "hostname", sess.Hello.Hostname, "kernel", sess.Hello.Kernel,
		"boot_seconds", float64(sess.Hello.BootMicros)/1e6)

	if err := sess.SetClock(time.Now(), 5*time.Second); err != nil {
		m.log.Warn("could not set the guest clock", "err", err)
	}
	if err := m.provision(ctx, sess, spec); err != nil {
		return nil, err
	}
	return inst, nil
}

type closer func()

func (c closer) Close() error { c(); return nil }

// launch starts the monitor, with its output in the environment's
// directory, and watches for it to exit.
func (m *machine) launch(ctx context.Context, cfg *ch.Config, inst *instance) error {
	bin, err := ch.Find()
	if err != nil {
		return err
	}
	args, err := cfg.Args()
	if err != nil {
		return err
	}
	logFile, err := os.OpenFile(filepath.Join(m.dir, "monitor.log"), os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o644)
	if err != nil {
		return err
	}
	cmd := exec.CommandContext(ctx, bin, args...)
	cmd.Stdout = logFile
	cmd.Stderr = logFile
	cmd.SysProcAttr = sysProcAttr()
	cmd.Cancel = func() error { return cmd.Process.Signal(syscall.SIGTERM) }
	cmd.WaitDelay = 15 * time.Second
	if err := cmd.Start(); err != nil {
		logFile.Close()
		return fmt.Errorf("starting cloud-hypervisor: %w", err)
	}
	inst.cmd = cmd
	go func() {
		err := cmd.Wait()
		logFile.Close()
		if err != nil && ctx.Err() == nil {
			inst.mu.Lock()
			inst.err = fmt.Errorf("cloud-hypervisor exited: %v%s", err, lastLine(filepath.Join(m.dir, "monitor.log")))
			inst.mu.Unlock()
		}
		close(inst.exited)
	}()
	return nil
}

// awaitAgent waits for the guest's agent to dial back, and gives up early if
// the monitor exits or the boot stops being wanted.
func (m *machine) awaitAgent(ctx context.Context, inst *instance) (*agent.Session, error) {
	type result struct {
		s   *agent.Session
		err error
	}
	got := make(chan result, 1)
	go func() {
		s, err := inst.server.Accept(m.rt.cfg.BootTimeout)
		got <- result{s, err}
	}()
	select {
	case r := <-got:
		if r.err != nil {
			return nil, fmt.Errorf("the agent did not connect within %s: %v", m.rt.cfg.BootTimeout, r.err)
		}
		return r.s, nil
	case <-inst.exited:
		if err := inst.exitErr(); err != nil {
			return nil, err
		}
		return nil, errors.New("the machine stopped before its agent connected")
	case <-ctx.Done():
		inst.server.Close()
		return nil, ctx.Err()
	}
}

// provision applies an environment's template the first time it boots: its
// hostname, and the repositories it clones. Later boots find the mark and
// leave the disks as they are.
func (m *machine) provision(ctx context.Context, sess *agent.Session, spec api.EnvironmentSpec) error {
	mark := filepath.Join(m.dir, provisionedMark)
	if _, err := os.Stat(mark); err == nil {
		return nil
	}

	run := func(timeout time.Duration, what string, cmd ...string) error {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		out, err := sess.Exec(timeout, cmd...)
		if err != nil {
			return fmt.Errorf("%s: %w", what, err)
		}
		if out.Code != 0 {
			return fmt.Errorf("%s: exit %d: %s", what, out.Code, firstLine(out.Stderr, out.Stdout))
		}
		return nil
	}

	// The agent answers early in the boot, before the network and the name
	// resolver are up. Provisioning needs both, so it waits for systemd to
	// finish starting the machine. "degraded" is finished too -- some unit
	// failed -- and exits non-zero, so the answer is read, not the status.
	m.set(api.PhaseStarting, "waiting for the guest to finish booting")
	if ctx.Err() != nil {
		return ctx.Err()
	}
	if out, err := sess.Exec(3*time.Minute, "systemctl", "is-system-running", "--wait"); err != nil {
		return fmt.Errorf("waiting for the guest to finish booting: %w", err)
	} else if state := strings.TrimSpace(out.Stdout); state != "running" && state != "degraded" {
		return fmt.Errorf("the guest did not finish booting: it is %s", firstLine(state, out.Stderr))
	}

	m.set(api.PhaseStarting, "setting up the workspace")
	// Written directly rather than through hostnamectl: the agent is up
	// early in the boot, before the bus hostnamectl talks to. The name is a
	// DNS label, and reaches the shell as an argument, never as script.
	if err := run(30*time.Second, "setting the hostname", "sh", "-c",
		`printf '%s\n' "$1" > /etc/hostname && hostname "$1"`, "sh", spec.Name); err != nil {
		return err
	}

	// The workspace belongs to the person using it, so repositories are
	// cloned as dev where the image has that user. An image without one is
	// set up as root.
	as := []string{}
	owner := "root"
	if out, err := sess.Exec(10*time.Second, "id", "-u", "dev"); err == nil && out.Code == 0 {
		as = []string{"runuser", "-u", "dev", "--"}
		owner = "dev"
	}
	// Provisioning is interrupted by a stop, a failure or the worker
	// restarting, and runs again from the top on the next boot -- the disks
	// keep what it had done. So each step is skipped if it is already done,
	// rather than failing on finding its own earlier work.
	git := func(args ...string) []string {
		return append(append([]string{}, as...), append([]string{"git"}, args...)...)
	}
	done := func(cmd ...string) bool {
		out, err := sess.Exec(30*time.Second, cmd...)
		return err == nil && out.Code == 0
	}
	for _, r := range spec.Spec.Repos {
		m.set(api.PhaseStarting, "cloning "+r.URL)
		if err := run(time.Minute, "creating "+filepath.Dir(r.Path),
			"install", "-d", "-o", owner, "-g", owner, filepath.Dir(r.Path)); err != nil {
			return err
		}
		if !done(git("-C", r.Path, "rev-parse", "--verify", "HEAD")...) {
			// Cloned beside its destination and moved into place whole, so
			// an interrupted clone leaves nothing at the path for the next
			// attempt to trip over. mv -T refuses to replace a directory
			// with anything in it, so it never clobbers one it did not make.
			partial := r.Path + ".cloning"
			if err := run(time.Minute, "clearing an earlier attempt", "rm", "-rf", "--", partial); err != nil {
				return err
			}
			if err := run(15*time.Minute, "cloning "+r.URL, git("clone", "--", r.URL, partial)...); err != nil {
				return err
			}
			if r.Ref != "" {
				if err := run(time.Minute, "checking out "+r.Ref, git("-C", partial, "checkout", r.Ref)...); err != nil {
					return err
				}
			}
			if err := run(time.Minute, "moving the clone to "+r.Path, "mv", "-T", "--", partial, r.Path); err != nil {
				return err
			}
		}
		if r.Branch != "" && !done(git("-C", r.Path, "rev-parse", "--verify", "refs/heads/"+r.Branch)...) {
			if err := run(time.Minute, "creating the branch "+r.Branch,
				git("-C", r.Path, "switch", "-c", r.Branch)...); err != nil {
				return err
			}
		}
	}
	return os.WriteFile(mark, nil, 0o644)
}

// socketDir makes the directory an environment's sockets live in, readable
// only by the worker.
func socketDir(envDir string) (string, error) {
	dir := filepath.Join(envDir, "run")
	return dir, os.MkdirAll(dir, 0o700)
}

// passtDir is where passt's socket goes. Distributions confine passt with an
// AppArmor profile that lets it create files under /tmp and nowhere Hangar
// keeps state, so its socket cannot sit with the others.
func passtDir(id string) (string, error) {
	dir := filepath.Join(os.TempDir(), "hangar-"+id)
	return dir, os.MkdirAll(dir, 0o700)
}

// ensureDisk creates an empty ext4 filesystem of the given size at path,
// unless one is already there. The file is sparse: it takes space only as
// the guest writes.
func ensureDisk(ctx context.Context, path, label string, gib int) error {
	if _, err := os.Stat(path); err == nil {
		return nil
	}
	tmp := path + ".new"
	f, err := os.Create(tmp)
	if err != nil {
		return err
	}
	err = f.Truncate(int64(gib) << 30)
	f.Close()
	if err != nil {
		os.Remove(tmp)
		return err
	}
	out, err := exec.CommandContext(ctx, "mkfs.ext4", "-q", "-F", "-L", label, tmp).CombinedOutput()
	if err != nil {
		os.Remove(tmp)
		return fmt.Errorf("making the %s filesystem: %v: %s", label, err, strings.TrimSpace(string(out)))
	}
	// Renamed into place only once complete, so a disk interrupted half
	// made is made again rather than booted.
	return os.Rename(tmp, path)
}

func firstLine(texts ...string) string {
	for _, t := range texts {
		t = strings.TrimSpace(t)
		if t == "" {
			continue
		}
		lines := strings.Split(t, "\n")
		return strings.TrimSpace(lines[len(lines)-1])
	}
	return "no output"
}

// lastLine is the monitor's last word, for explaining why it exited.
func lastLine(path string) string {
	b, err := os.ReadFile(path)
	if err != nil {
		return ""
	}
	line := firstLine(string(b))
	if line == "no output" {
		return ""
	}
	return ": " + line
}
