package ch

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
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

	cmd := exec.CommandContext(ctx, bin,
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
	)
	if err := launch(cmd, "passt", socket, "", 10*time.Second, verbose); err != nil {
		return nil, err
	}
	return &Passt{cmd: cmd, socket: socket}, nil
}

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
