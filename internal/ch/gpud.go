package ch

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

// GpuBackend is a running hangar-gpu, rendering for one guest.
type GpuBackend struct {
	cmd    *exec.Cmd
	socket string
	state  string
}

// gpuSearchPath is where the backend lives: a local build during development,
// and the path the published image installs to.
var gpuSearchPath = []string{
	filepath.Join("gpu", "target", "release", "hangar-gpu"),
	"/usr/local/bin/hangar-gpu",
}

// FindGpuBackend locates the renderer.
func FindGpuBackend() (string, error) {
	if p, err := exec.LookPath("hangar-gpu"); err == nil {
		return p, nil
	}
	for _, p := range gpuSearchPath {
		if _, err := os.Stat(p); err == nil {
			return filepath.Abs(p)
		}
	}
	return "", fmt.Errorf("hangar-gpu not found on PATH or in %v", gpuSearchPath)
}

// StartGpuBackend renders for one guest from a process of its own.
//
// The renderer links native graphics libraries whose syscall surface is not
// ours to enumerate, and loads whichever driver the host has at run time.
// Keeping it out of the monitor means that surface, and any crash in it,
// belongs to a process holding no guest memory it was not handed.
//
// It owns the socket and must be listening before the monitor starts, which
// connects to it as a client.
//
// state is where the objects the guest holds are recorded; see SaveState.
func StartGpuBackend(ctx context.Context, socket string, venus bool, state string, verbose bool) (*GpuBackend, error) {
	bin, err := FindGpuBackend()
	if err != nil {
		return nil, err
	}

	_ = os.Remove(socket)
	if err := os.MkdirAll(filepath.Dir(socket), 0o755); err != nil {
		return nil, err
	}

	args := []string{"--socket", socket, "--virgl", "true"}
	if venus {
		args = append(args, "--venus", "true")
	}
	if state != "" {
		args = append(args, "--state", state)
	}
	cmd := exec.CommandContext(ctx, bin, args...)
	// A host with no /dev/dri has no GBM device, and the renderer needs
	// telling to use the surfaceless platform rather than probing for one.
	cmd.Env = append(os.Environ(), "EGL_PLATFORM=surfaceless")
	if err := launch(cmd, "the gpu backend", socket, 15*time.Second, verbose); err != nil {
		return nil, err
	}
	return &GpuBackend{cmd: cmd, socket: socket, state: state}, nil
}

// Socket is the path the monitor should connect to.
func (g *GpuBackend) Socket() string { return g.socket }

// SaveState records the objects the guest holds.
//
// Nothing behind them can be carried across -- a renderer's state is not
// readable -- so this is what lets a restored backend tell the guest precisely
// which of its objects are gone, rather than answer as if it had invented
// them.
func (g *GpuBackend) SaveState(timeout time.Duration) error {
	if g.state == "" {
		return fmt.Errorf("the gpu backend was started without a state file")
	}
	return saveState(g.cmd.Process, g.state, timeout)
}

// Close stops the backend and removes its socket.
func (g *GpuBackend) Close() error {
	if g.cmd != nil && g.cmd.Process != nil {
		_ = g.cmd.Process.Kill()
		_, _ = g.cmd.Process.Wait()
	}
	return os.Remove(g.socket)
}

// WaitRestored waits until a restored session has been built again in the
// renderer: every resource with its contents, and every context's objects and
// bindings. The guest must not run before then, since the first thing a
// program mid-render does is use them.
func (g *GpuBackend) WaitRestored(ctx context.Context, timeout time.Duration) error {
	return waitFile(ctx, readyPath(g.state), timeout)
}
