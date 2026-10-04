package ch

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// Passt is a running passt, giving one guest outbound networking.
type Passt struct {
	cmd    *exec.Cmd
	socket string
}

// StartPasst gives one guest a network through passt in vhost-user mode.
//
// passt translates the guest's traffic into ordinary host sockets, so the VM
// rides the host's normal networking with no TAP device and no NET_ADMIN.
// Cloud Hypervisor has no socket-based network backend, only tap, a tap file
// descriptor, or vhost-user -- so vhost-user is how the two meet.
//
// passt is the backend and owns the socket, so it must be listening before
// the monitor starts: Cloud Hypervisor connects as the client, which is its
// default vhost_mode.
//
// One passt serves one VM. vhost-user is a point-to-point conversation about
// one guest's memory, so this is not a shared service.
func StartPasst(ctx context.Context, socket string, verbose bool) (*Passt, error) {
	bin, err := exec.LookPath("passt")
	if err != nil {
		return nil, fmt.Errorf("passt not found (apt install passt): %w", err)
	}

	// passt also binds a second socket beside the one it is given, for moving
	// TCP connections during a migration. Both have to go: one left by a passt
	// that was killed makes the next refuse to start.
	_ = os.Remove(socket)
	_ = os.Remove(socket + ".repair")
	if err := os.MkdirAll(filepath.Dir(socket), 0o755); err != nil {
		return nil, err
	}
	passtProbe.Do(func() { probePasst(bin, filepath.Dir(socket)) })

	args := []string{
		"--vhost-user",
		"--socket-path", socket,
		// Stay in the foreground so this process supervises it rather than
		// losing track of a daemon that forked away.
		"--foreground",
		// Exit when the monitor disconnects. passt clears the parent-death
		// signal as it drops its privileges, so this is what ends it when
		// the process that started it dies uncleanly: the monitor dies with
		// that process, and passt follows. Every boot and resume starts a
		// passt of its own, so one monitor is all it ever serves.
		"--one-off",
		"--quiet",
	}
	if passtAsRoot.Load() {
		args = append(args, asRoot...)
	}
	cmd := exec.CommandContext(ctx, bin, args...)
	err = launch(cmd, "passt", socket, "", 10*time.Second, verbose)
	if err != nil && !passtAsRoot.Load() && os.Geteuid() == 0 && strings.Contains(err.Error(), "Failed to sandbox") {
		// Started as root, passt changes to nobody and then sandboxes
		// itself in new namespaces, a user namespace among them. Ubuntu
		// refuses an unprivileged process one unless its AppArmor profile
		// allows it (kernel.apparmor_restrict_unprivileged_userns), which
		// passt's own profile does, but not in a container with AppArmor
		// turned off, as the worker's is. Kept as root, the process that
		// makes the namespaces has the capabilities to, and passt is still
		// sandboxed in them.
		passtAsRoot.Store(true)
		_ = os.Remove(socket)
		_ = os.Remove(socket + ".repair")
		cmd = exec.CommandContext(ctx, bin, append(args, asRoot...)...)
		err = launch(cmd, "passt", socket, "", 10*time.Second, verbose)
	}
	if err != nil {
		return nil, err
	}
	return &Passt{cmd: cmd, socket: socket}, nil
}

// passtProbe decides, once, whether passt can run as nobody here.
var passtProbe sync.Once

// probePasst finds out whether passt can sandbox itself as nobody on this
// host, and if it cannot, has every passt run as root. Its socket is no sign:
// passt binds it before sandboxing, then exits when that fails, leaving a
// socket nothing answers, and the monitor waits on it for a minute. So a
// passt is started on a socket of its own and watched until it has had time
// to sandbox itself.
func probePasst(bin, dir string) {
	if os.Geteuid() != 0 {
		return
	}
	socket := filepath.Join(dir, "probe.sock")
	defer os.Remove(socket)
	defer os.Remove(socket + ".repair")
	_ = os.Remove(socket)
	_ = os.Remove(socket + ".repair")
	var stderr strings.Builder
	cmd := exec.Command(bin, "--vhost-user", "--socket-path", socket, "--foreground", "--one-off")
	cmd.Stderr = &stderr
	if err := cmd.Start(); err != nil {
		return
	}
	exited := make(chan struct{})
	go func() { cmd.Wait(); close(exited) }()
	select {
	case <-exited:
		if strings.Contains(stderr.String(), "Failed to sandbox") {
			passtAsRoot.Store(true)
		}
	case <-time.After(time.Second):
		_ = cmd.Process.Kill()
		<-exited
	}
}

// asRoot keeps passt as root rather than changing to nobody.
var asRoot = []string{"--runas", "0:0"}

// passtAsRoot is set once passt has been found unable to sandbox itself as
// nobody on this host, so every later one starts as root.
var passtAsRoot atomic.Bool

// Socket is the path the monitor should connect to.
func (p *Passt) Socket() string { return p.socket }

// Close stops passt and removes its socket.
func (p *Passt) Close() error {
	if p.cmd != nil && p.cmd.Process != nil {
		_ = p.cmd.Process.Kill()
		_, _ = p.cmd.Process.Wait()
	}
	_ = os.Remove(p.socket + ".repair")
	return os.Remove(p.socket)
}

// Pid is the backend's process ID.
func (b *Passt) Pid() int { return pidOf(b.cmd) }
