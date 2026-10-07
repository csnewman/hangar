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
	"github.com/csnewman/hangar/internal/sysuser"
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
	switch cfg.ImageDevice {
	case ImageDisk, ImagePmem:
		if _, err := exec.LookPath("mkfs.erofs"); err != nil {
			return fmt.Errorf("mkfs.erofs not found (apt install erofs-utils): %w", err)
		}
	case ImageVirtiofs:
		if _, err := ch.FindFsBackend(); err != nil {
			return err
		}
	default:
		return fmt.Errorf("vm.image_device %q is none of %s, %s or %s", cfg.ImageDevice, ImageDisk, ImagePmem, ImageVirtiofs)
	}
	for ref, img := range cfg.Images {
		st, err := os.Stat(img.Base)
		if err != nil {
			return fmt.Errorf("image %s: base: %w", ref, err)
		}
		if !st.IsDir() {
			return fmt.Errorf("image %s: base %s is not a directory: an image is its root filesystem", ref, img.Base)
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
	if s.GPU == api.GPUVirtual {
		switch {
		case m.rt.gpu == nil:
			return nil, errors.New("this worker has no GPU backend, and the template asks for a virtual GPU")
		case m.rt.gpu.Unavailable != "":
			return nil, fmt.Errorf("this worker offers no virtual GPU: %s", m.rt.gpu.Unavailable)
		}
	}
	// A check of what an upgrade would hide stands down once the phase is
	// starting, and one under way is waited for: both use the writable disk.
	m.disk.Lock()
	m.set(api.PhaseStarting, "starting")
	err = m.changeImage(ctx, spec)
	m.disk.Unlock()
	if err != nil {
		return nil, err
	}
	img, err := m.resolve(ctx, spec)
	if err != nil {
		return nil, err
	}
	if s.Display == api.DisplayDesktop {
		// An image that says it has no desktop would boot to one that never
		// appears; one that does not say is taken at its word.
		info, known, err := ReadImageInfo(img.Base)
		if err != nil {
			return nil, fmt.Errorf("image %s: %w", s.Image, err)
		}
		if known && !info.Desktop {
			return nil, fmt.Errorf("image %s has no desktop, and the template asks for one: use an image with a desktop, or set the display to none", s.Image)
		}
	}
	m.step(api.StepDisks, "preparing its disks")
	if err := os.MkdirAll(m.dir, 0o755); err != nil {
		return nil, err
	}
	// One writable disk holds both the root's layer and Docker's store,
	// which the agent mounts at /var/lib/docker beside the root rather than
	// in it: overlay2 cannot stack on the overlay root.
	upper := filepath.Join(m.dir, "upper.ext4")
	if err := EnsureDisk(ctx, upper, "hangar-upper", m.rt.cfg.UpperGiB+m.rt.cfg.DockerGiB); err != nil {
		return nil, err
	}
	var baseFile, baseDevice string
	if dev := m.rt.cfg.ImageDevice; dev == ImageDisk || dev == ImagePmem {
		// Built once per copy of the image, and shared by every machine on
		// it.
		m.step(api.StepDisks, "preparing the image")
		f, err := m.rt.store.EROFS(ctx, img)
		if err != nil {
			return nil, fmt.Errorf("image %s: %w", s.Image, err)
		}
		baseFile, baseDevice = f, dev
		m.step(api.StepDisks, "preparing its disks")
	}
	m.makeRoomForHugePages(s.MemoryMiB)
	// The writable layer is the first disk: the agent, as init, mounts
	// /dev/vda. The editor disk, shared read-only by every environment, is
	// mounted by label.
	disks := []ch.Disk{{Path: upper}}
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
		BaseFile:    baseFile,
		BaseDevice:  baseDevice,
		BaseDAX:     baseDevice == ImagePmem && s.DAX,
		Disks:       disks,
		MemoryMiB:   s.MemoryMiB,
		CPUs:        s.CPUs,
		DaxMiB:      daxMiB(m.rt.cfg, s),
		Net:         true,
		GPU:         s.GPU == api.GPUVirtual,
		ConsoleFile: filepath.Join(m.dir, "console.log"),
		AgentWait:   m.rt.cfg.BootTimeout,
		Progress:    func(step string) { m.step(api.StepBoot, step) },
		Log:         m.log,
	}
	if cfg.GPU {
		cfg.GPUVenus = m.rt.cfg.GPUVenus
		cfg.GPUVenusRestore = m.rt.cfg.GPUVenus && m.rt.cfg.GPUVenusRestore
		cfg.GPUWindowMiB = m.rt.cfg.GPUWindowMiB
		cfg.GPUEnv = m.rt.gpuEnv
	}
	// An image may have a desktop; the template decides whether it runs. A
	// headless environment boots to multi-user.target, which leaves out
	// everything graphical whatever the image holds, and masks the desktop
	// so nothing can pull it in.
	if s.Display == api.DisplayNone {
		cfg.ExtraCmdline = "systemd.unit=multi-user.target systemd.mask=hangar-desktop.service"
	} else {
		cfg.ExtraCmdline = "systemd.unit=graphical.target"
	}
	inst, err := Boot(ctx, cfg)
	if err != nil {
		return nil, err
	}
	// The machine takes streams while it is set up: a clone over SSH signs
	// with the owner's keys, which the server sends over the profile
	// session it opens as soon as the machine will have one.
	m.mu.Lock()
	m.starting = inst
	m.mu.Unlock()
	defer func() {
		m.mu.Lock()
		m.starting = nil
		m.mu.Unlock()
		if err != nil {
			inst.Shutdown(m.log)
		}
	}()
	if err := writeEditorTrust(inst.Session(), s); err != nil {
		m.log.Warn("could not tell the editor which folders to trust", "err", err)
	}
	if err := m.waitNetwork(ctx, inst.Session()); err != nil {
		return nil, err
	}
	m.waitProfile(ctx, inst.Session())
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

	m.step(api.StepWorkspace, "setting up the workspace")
	// Written directly rather than through hostnamectl: the agent is up
	// early in the boot, before the bus hostnamectl talks to. The name is a
	// DNS label, in lower case as host names are written, and reaches the
	// shell as an argument, never as script.
	if err := run(30*time.Second, "setting the hostname", "sh", "-c",
		`printf '%s\n' "$1" > /etc/hostname && hostname "$1"`, "sh", strings.ToLower(spec.Name)); err != nil {
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
	// runuser leaves the SSH agent out of git's environment, and a clone
	// over SSH signs in with the owner's keys through it.
	git := func(args ...string) []string {
		cmd := append(append([]string{}, as...), "env", "SSH_AUTH_SOCK="+sysuser.SSHAuthSock, "git")
		return append(cmd, args...)
	}
	done := func(cmd ...string) bool {
		out, err := sess.Exec(30*time.Second, cmd...)
		return err == nil && out.Code == 0
	}
	// The keys are listed before the first clone over SSH, for the worker's
	// log, and named again if the git host refuses them.
	var keys *agentKeys
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
			if keys == nil && overSSH(r.URL) {
				keys = listAgentKeys(sess, as)
				m.log.Info("the owner's SSH agent in the guest", "keys", keys.String())
			}
			if err := m.clone(ctx, sess, r.URL, git("clone", "--progress", "--", r.URL, partial)); err != nil {
				if keys != nil && strings.Contains(err.Error(), "(publickey") {
					return fmt.Errorf("%s: %s", strings.TrimRight(err.Error(), "."), keys.refused())
				}
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

// waitNetwork waits for the guest's network and name resolver, on every
// boot: an environment is running only once it can reach the outside, and
// its workspace's clones need both. The agent answers early in the boot,
// before either is up.
//
// It waits for those targets alone, not for the whole machine: the rest of
// the boot -- Docker, the desktop -- carries on starting in the guest
// without holding the environment back.
func (m *machine) waitNetwork(ctx context.Context, sess *agent.Session) error {
	m.step(api.StepNetwork, "waiting for the network")
	if ctx.Err() != nil {
		return ctx.Err()
	}
	// Starting a unit waits until it is up, and systemctl reaches systemd
	// without the system bus, which is not up this early. nss-lookup.target
	// cannot be started by hand, so the resolver is waited for as itself,
	// on the bases that have it.
	out, err := sess.Exec(3*time.Minute, "sh", "-c", `systemctl start network-online.target &&
		{ ! systemctl cat systemd-resolved.service >/dev/null 2>&1 || systemctl start systemd-resolved.service; }`)
	if err != nil {
		return fmt.Errorf("waiting for the network: %w", err)
	}
	if out.Code != 0 {
		return fmt.Errorf("the network did not come up: %s", firstLine(out.Stderr, out.Stdout))
	}
	return nil
}

// profileWait is how long a boot waits for the owner's profile.
const profileWait = 2 * time.Minute

// waitProfile waits for the owner's profile to reach the guest, so that the
// workspace is set up, and the environment is running, with their settings
// and the keys a clone over SSH signs in with. A profile that does not come
// holds nothing else back: the environment starts without it, and a clone
// that needs a key says so.
func (m *machine) waitProfile(ctx context.Context, sess *agent.Session) {
	m.step(api.StepProfile, "syncing the profile")
	start := time.Now()
	for time.Since(start) < profileWait {
		if ctx.Err() != nil {
			return
		}
		out, err := sess.Exec(10*time.Second, "test", "-e", sysuser.ProfileSynced)
		if err == nil && out.Code == 0 {
			m.log.Info("the profile reached the guest", "after", time.Since(start).Round(time.Millisecond))
			return
		}
		select {
		case <-ctx.Done():
			return
		case <-time.After(250 * time.Millisecond):
		}
	}
	m.log.Warn("the profile did not reach the guest; starting without it", "waited", profileWait)
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

// agentKeys are the keys the owner's SSH agent in the guest offers, as a
// clone over SSH sees them, to tell a key the git host does not know from no
// key at all.
type agentKeys struct {
	keys []string // fingerprint and type: "SHA256:... (ED25519)"
	why  string   // why there are none
}

func listAgentKeys(sess *agent.Session, as []string) *agentKeys {
	cmd := append(append([]string{}, as...), "env", "SSH_AUTH_SOCK="+sysuser.SSHAuthSock, "ssh-add", "-l")
	out, err := sess.Exec(45*time.Second, cmd...)
	switch {
	case err != nil || out.Code == 2:
		return &agentKeys{why: "the SSH agent could not be reached"}
	case out.Code == 1:
		return &agentKeys{why: "the SSH agent offered no keys"}
	}
	k := &agentKeys{}
	for _, line := range strings.Split(strings.TrimSpace(out.Stdout), "\n") {
		// "256 SHA256:... comment (ED25519)"
		f := strings.Fields(line)
		if len(f) >= 2 {
			k.keys = append(k.keys, f[1]+" "+f[len(f)-1])
		}
	}
	return k
}

func (k *agentKeys) String() string {
	if len(k.keys) == 0 {
		return k.why
	}
	return strings.Join(k.keys, ", ")
}

// refused explains a git host's refusal of the keys.
func (k *agentKeys) refused() string {
	switch len(k.keys) {
	case 0:
		return k.why + "; add one under Profile, then SSH keys (an environment of an untrusted template is given none)."
	case 1:
		return "the git host refused " + k.String() + ", the key the SSH agent offered; add it to your account there."
	}
	return "the git host refused " + k.String() + ", the keys the SSH agent offered; add one to your account there."
}

// overSSH is whether git fetches url over SSH: an ssh:// URL, or the
// scp-like user@host:path.
func overSSH(url string) bool {
	if scheme, _, ok := strings.Cut(url, "://"); ok {
		return strings.HasPrefix(scheme, "ssh") || strings.HasSuffix(scheme, "ssh")
	}
	host, _, ok := strings.Cut(url, ":")
	return ok && !strings.Contains(host, "/")
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

// daxMiB is the virtio-fs DAX window an environment's machine is given: the
// worker's, if its base is served over virtio-fs and it asks to map its
// image's files, and none otherwise.
func daxMiB(worker Config, s api.Spec) int {
	if !s.DAX || worker.ImageDevice != ImageVirtiofs {
		return 0
	}
	return worker.DaxMiB
}

// makeRoomForHugePages compacts the host's memory when it has too few free
// huge-page-sized blocks for a guest of memMiB. A guest's memory that finds
// none runs on 4 KiB pages, which is slower on any host and, under nested
// virtualisation, hundreds of times slower to touch; a page cache that has
// grown through the host's free memory is the usual reason, and compaction
// moves it aside in about a second.
func (m *machine) makeRoomForHugePages(memMiB int) {
	free, err := ch.FreeHugeBlocksMiB()
	if err != nil || free >= memMiB {
		return
	}
	start := time.Now()
	if err := ch.CompactMemory(); err != nil {
		m.log.Warn("compacting the host's memory for the guest's huge pages", "err", err)
		return
	}
	after, _ := ch.FreeHugeBlocksMiB()
	m.log.Info("compacted the host's memory for the guest's huge pages", "free_huge_mib_before", free,
		"free_huge_mib_after", after, "guest_mib", memMiB, "took", time.Since(start).Round(time.Millisecond))
}
