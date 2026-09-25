package vm

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/agent"
	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/ch"
	"github.com/csnewman/hangar/internal/gitout"
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
	if cfg.Agent == "" {
		return errors.New("no agent: every environment boots with the worker's (vm.agent)")
	}
	if _, err := os.Stat(cfg.Agent); err != nil {
		return fmt.Errorf("agent: %w", err)
	}
	if _, err := ch.FindFsBackend(); err != nil {
		return err
	}
	for ref, img := range cfg.Images {
		st, err := os.Stat(img.Base)
		if err != nil {
			return fmt.Errorf("image %s: base: %w", ref, err)
		}
		if !st.IsDir() {
			return fmt.Errorf("image %s: base %s is not a directory: an image is its root filesystem, served over virtio-fs", ref, img.Base)
		}
	}
	return nil
}

// start boots the environment and provisions it, or resumes it if it is
// suspended. On error, everything it started has been stopped again.
func (m *machine) start(ctx context.Context, spec api.EnvironmentSpec) (_ *Instance, err error) {
	s := spec.Spec
	if s.GPU == api.GPUPassthrough {
		return nil, fmt.Errorf("GPU passthrough is %w", errUnsupported)
	}
	img, err := m.resolve(ctx, spec)
	if err != nil {
		return nil, err
	}
	m.step(api.StepDisks, "preparing its disks")
	if err := os.MkdirAll(m.dir, 0o755); err != nil {
		return nil, err
	}
	upper := filepath.Join(m.dir, "upper.ext4")
	docker := filepath.Join(m.dir, "docker.ext4")
	if err := EnsureDisk(ctx, upper, "hangar-upper", m.rt.cfg.UpperGiB); err != nil {
		return nil, err
	}
	if err := EnsureDisk(ctx, docker, "hangar-docker", m.rt.cfg.DockerGiB); err != nil {
		return nil, err
	}
	// The writable layer is the first disk: the agent, as init, mounts
	// /dev/vda. The Docker disk is mounted by label, and so is the editor
	// disk, shared read-only by every environment.
	disks := []ch.Disk{{Path: upper}, {Path: docker}}
	if m.rt.cfg.Editor != "" {
		disks = append(disks, ch.Disk{Path: m.rt.cfg.Editor, ReadOnly: true})
	}
	cfg := InstanceConfig{
		ID:          m.id,
		Name:        spec.Name,
		Dir:         m.dir,
		Kernel:      m.rt.cfg.Kernel,
		Agent:       m.rt.cfg.Agent,
		Base:        img.Base,
		Disks:       disks,
		MemoryMiB:   s.MemoryMiB,
		CPUs:        s.CPUs,
		DaxMiB:      m.rt.cfg.DaxMiB,
		Net:         true,
		GPU:         s.GPU == api.GPUVirtual,
		ConsoleFile: filepath.Join(m.dir, "console.log"),
		AgentWait:   m.rt.cfg.BootTimeout,
		Progress:    func(step string) { m.step(api.StepBoot, step) },
		Log:         m.log,
	}
	if s.Display == api.DisplayNone {
		// The desktop is a unit in the image; a headless environment
		// simply never starts it.
		cfg.ExtraCmdline = "systemd.mask=hangar-desktop.service"
	}
	inst, err := Boot(ctx, cfg)
	if err != nil {
		return nil, err
	}
	defer func() {
		if err != nil {
			inst.Shutdown(m.log)
		}
	}()
	if err := writeEditorTrust(inst.Session(), s); err != nil {
		m.log.Warn("could not tell the editor which folders to trust", "err", err)
	}
	if err := m.provision(ctx, inst.Session(), spec); err != nil {
		return nil, err
	}
	return inst, nil
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
	m.step(api.StepServices, "waiting for the guest to finish booting")
	if ctx.Err() != nil {
		return ctx.Err()
	}
	if out, err := sess.Exec(3*time.Minute, "systemctl", "is-system-running", "--wait"); err != nil {
		return fmt.Errorf("waiting for the guest to finish booting: %w", err)
	} else if state := strings.TrimSpace(out.Stdout); state != "running" && state != "degraded" {
		return fmt.Errorf("the guest did not finish booting: it is %s", firstLine(state, out.Stderr))
	}

	m.step(api.StepWorkspace, "setting up the workspace")
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
		m.step(api.StepWorkspace, "cloning "+r.URL)
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
			if err := m.clone(ctx, sess, r.URL, git("clone", "--progress", "--", r.URL, partial)); err != nil {
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

// cloneLog is where a clone's progress and errors are written in the guest,
// for the host to read while it runs.
const cloneLog = "/run/hangar-clone.log"

// clone runs a clone, reporting git's progress as it goes. git writes its
// progress to the log rather than to the reply, which comes only when the
// command exits; the log is read once a second meanwhile. A failure is
// explained by the line that names what went wrong, not by git's closing
// advice.
func (m *machine) clone(ctx context.Context, sess *agent.Session, url string, cmd []string) error {
	what := "cloning " + url
	stop := make(chan struct{})
	watched := make(chan struct{})
	go func() {
		defer close(watched)
		t := time.NewTicker(time.Second)
		defer t.Stop()
		for {
			select {
			case <-stop:
				return
			case <-t.C:
			}
			out, err := sess.Exec(5*time.Second, "tail", "-c", "512", cloneLog)
			if err != nil || out.Code != 0 {
				continue
			}
			if stage, done, total, ok := gitout.Progress(out.Stdout); ok {
				m.step(api.StepWorkspace, what+": "+strings.ToLower(stage))
				m.measure(done, total, "objects")
			}
		}
	}()
	defer func() {
		close(stop)
		<-watched
	}()

	if ctx.Err() != nil {
		return ctx.Err()
	}
	out, err := sess.Exec(15*time.Minute, append([]string{"sh", "-c", `log=$1; shift; "$@" 2>"$log"`, "sh", cloneLog}, cmd...)...)
	if err != nil {
		return fmt.Errorf("%s: %w", what, err)
	}
	if out.Code != 0 {
		log, _ := sess.Exec(5*time.Second, "cat", cloneLog)
		why := "no output"
		if log != nil {
			why = gitout.Failure(log.Stdout)
		}
		return fmt.Errorf("%s: %s", what, why)
	}
	return nil
}

// passtDir is where passt's socket goes. Distributions confine passt with an
// AppArmor profile that lets it create files under /tmp and nowhere Hangar
// keeps state, so its socket cannot sit with the others.
func passtDir(id string) (string, error) {
	dir := filepath.Join(os.TempDir(), "hangar-"+id)
	return dir, os.MkdirAll(dir, 0o700)
}

// EnsureDisk creates an empty ext4 filesystem of the given size at path,
// unless one is already there. The file is sparse: it takes space only as
// the guest writes.
func EnsureDisk(ctx context.Context, path, label string, gib int) error {
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

// writeEditorTrust writes the folders the editor trusts without asking,
// which VS Code's server reads on every page it serves. It is written at
// each boot, into /run, so it is always the environment's own and never
// outlives it.
func writeEditorTrust(sess *agent.Session, spec api.Spec) error {
	b, err := json.Marshal(spec.EditorTrust())
	if err != nil {
		return err
	}
	out, err := sess.Exec(10*time.Second, "sh", "-c",
		`mkdir -p /run/hangar && printf '%s' "$1" > /run/hangar/trusted-folders.json.new && `+
			`chmod 644 /run/hangar/trusted-folders.json.new && mv /run/hangar/trusted-folders.json.new /run/hangar/trusted-folders.json`,
		"sh", string(b))
	if err != nil {
		return err
	}
	if out.Code != 0 {
		return fmt.Errorf("exit %d: %s", out.Code, firstLine(out.Stderr, out.Stdout))
	}
	return nil
}
