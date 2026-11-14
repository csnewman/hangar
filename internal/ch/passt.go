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
	"syscall"
	"time"

	"golang.org/x/sys/unix"
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
//
// Without vhostUser passt speaks its stream protocol on the socket instead,
// frames prefixed with their length, which is how libkrun reaches it.
func StartPasst(ctx context.Context, socket string, vhostUser bool) (*Passt, error) {
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
	passtProbe.Do(func() { probePasst(bin) })

	args := []string{
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
	if vhostUser {
		args = append(args, "--vhost-user")
	}
	if passtAsRoot.Load() {
		args = append(args, asRoot...)
	}
	cmd := exec.CommandContext(ctx, bin, args...)
	err = launch(cmd, "passt", socket, "", 10*time.Second)
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
		err = launch(cmd, "passt", socket, "", 10*time.Second)
	}
	if err != nil {
		return nil, err
	}
	return &Passt{cmd: cmd, socket: socket}, nil
}

// passtProbe decides, once, whether passt can run as nobody here.
var passtProbe sync.Once

// probePasst finds out whether passt can sandbox itself as nobody on this
// host, and if it cannot, has every passt run as root. A passt listening on
// a path binds it before sandboxing, then exits when that fails, leaving a
// socket nothing answers, and the monitor waits on it for a minute; one
// given a connected socket fails as quietly. So a passt is started on a
// socket of its own and watched until it has had time to sandbox itself.
func probePasst(bin string) {
	if os.Geteuid() != 0 {
		return
	}
	ours, theirs, err := socketPair()
	if err != nil {
		return
	}
	defer ours.Close()
	var stderr strings.Builder
	cmd := exec.Command(bin, "--fd", "3", "--foreground")
	cmd.ExtraFiles = []*os.File{theirs}
	cmd.Stderr = &stderr
	err = cmd.Start()
	theirs.Close()
	if err != nil {
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

// StartPasstConnected gives one libkrun machine a network through passt,
// over a connected pair of sockets: passt has one, speaking its stream
// protocol on it, and the other, returned, is the monitor's to inherit.
//
// passt accepts nothing on a connected socket. One listening on a path
// accepts its monitor after it has sandboxed itself, when the path is
// outside its mount namespace, and an AppArmor that mediates unix sockets by
// path refuses that (Ubuntu's 7.0 kernels, under passt's own profile): the
// monitor's frames then wait in the socket, and the guest never has a
// network.
//
// passt exits when the monitor's end closes.
func StartPasstConnected(ctx context.Context) (*Passt, *os.File, error) {
	bin, err := exec.LookPath("passt")
	if err != nil {
		return nil, nil, fmt.Errorf("passt not found (apt install passt): %w", err)
	}
	passtProbe.Do(func() { probePasst(bin) })
	ours, theirs, err := socketPair()
	if err != nil {
		return nil, nil, err
	}
	// Stay in the foreground so this process supervises it rather than
	// losing track of a daemon that forked away.
	args := []string{"--fd", "3", "--foreground", "--quiet"}
	if passtAsRoot.Load() {
		args = append(args, asRoot...)
	}
	cmd := exec.CommandContext(ctx, bin, args...)
	cmd.ExtraFiles = []*os.File{theirs}
	err = launch(cmd, "passt", "", "", 10*time.Second)
	theirs.Close()
	if err != nil {
		ours.Close()
		return nil, nil, err
	}
	return &Passt{cmd: cmd}, ours, nil
}

// socketPair makes a connected pair of stream sockets, closed on exec.
func socketPair() (*os.File, *os.File, error) {
	syscall.ForkLock.RLock()
	fds, err := unix.Socketpair(unix.AF_UNIX, unix.SOCK_STREAM, 0)
	if err == nil {
		unix.CloseOnExec(fds[0])
		unix.CloseOnExec(fds[1])
	}
	syscall.ForkLock.RUnlock()
	if err != nil {
		return nil, nil, fmt.Errorf("making passt's socket: %w", err)
	}
	return os.NewFile(uintptr(fds[0]), "passt"), os.NewFile(uintptr(fds[1]), "passt"), nil
}

// asRoot keeps passt as root rather than changing to nobody.
var asRoot = []string{"--runas", "0:0"}

// passtAsRoot is set once passt has been found unable to sandbox itself as
// nobody on this host, so every later one starts as root.
var passtAsRoot atomic.Bool

// Socket is the path the monitor should connect to, for a passt started
// on one.
func (p *Passt) Socket() string { return p.socket }

// Close stops passt and removes its socket.
func (p *Passt) Close() error {
	if p.cmd != nil && p.cmd.Process != nil {
		_ = p.cmd.Process.Kill()
		_, _ = p.cmd.Process.Wait()
	}
	if p.socket == "" {
		return nil
	}
	_ = os.Remove(p.socket + ".repair")
	return os.Remove(p.socket)
}

// Pid is the backend's process ID.
func (b *Passt) Pid() int { return pidOf(b.cmd) }
