package ch

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

// Virtiofsd is a running virtiofsd, exporting one directory to one guest.
type Virtiofsd struct {
	cmd    *exec.Cmd
	socket string
	dir    string
}

// StartVirtiofsd exports dir over a vhost-user socket for a single guest.
//
// One daemon serves one VM: the vhost-user protocol is a point-to-point
// conversation about one guest's memory, so this is not a shared service.
//
// It runs unsandboxed. virtiofsd's default sandbox unshares a mount namespace
// and pivots, which needs privileges a worker should not have; the directory
// being exported is a read-only image layer the host itself assembled, not
// something the guest chose.
func StartVirtiofsd(ctx context.Context, dir, socket string, verbose bool) (*Virtiofsd, error) {
	abs, err := filepath.Abs(dir)
	if err != nil {
		return nil, err
	}
	if st, err := os.Stat(abs); err != nil || !st.IsDir() {
		return nil, fmt.Errorf("%s is not a directory to export", abs)
	}
	bin, err := exec.LookPath("virtiofsd")
	if err != nil {
		// Debian and Ubuntu put it here rather than on PATH.
		for _, p := range []string{"/usr/lib/qemu/virtiofsd", "/usr/libexec/virtiofsd"} {
			if _, statErr := os.Stat(p); statErr == nil {
				bin, err = p, nil
				break
			}
		}
		if err != nil {
			return nil, fmt.Errorf("virtiofsd not found (apt install virtiofsd): %w", err)
		}
	}

	_ = os.Remove(socket)
	if err := os.MkdirAll(filepath.Dir(socket), 0o755); err != nil {
		return nil, err
	}

	cmd := exec.CommandContext(ctx, bin,
		"--socket-path="+socket,
		"--shared-dir="+abs,
		// The guest's page cache may hold entries indefinitely: the host is
		// the only writer of a base layer, so there is nothing to invalidate.
		"--cache=always",
		"--sandbox=none",
	)
	if verbose {
		cmd.Stdout = os.Stderr
		cmd.Stderr = os.Stderr
	}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("starting virtiofsd: %w", err)
	}

	// The monitor connects to the socket at startup and fails if it is not
	// there yet, so wait for it rather than racing.
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(socket); err == nil {
			return &Virtiofsd{cmd: cmd, socket: socket, dir: abs}, nil
		}
		if cmd.ProcessState != nil && cmd.ProcessState.Exited() {
			return nil, fmt.Errorf("virtiofsd exited before creating %s", socket)
		}
		time.Sleep(20 * time.Millisecond)
	}
	_ = cmd.Process.Kill()
	return nil, fmt.Errorf("virtiofsd did not create %s within 10s", socket)
}

// Socket is the path the monitor should connect to.
func (v *Virtiofsd) Socket() string { return v.socket }

// Close stops the daemon and removes its socket.
func (v *Virtiofsd) Close() error {
	if v.cmd != nil && v.cmd.Process != nil {
		_ = v.cmd.Process.Kill()
		_, _ = v.cmd.Process.Wait()
	}
	return os.Remove(v.socket)
}
