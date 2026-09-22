package ch

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

// FsBackend is a running hangar-fs, exporting one directory to one guest.
type FsBackend struct {
	cmd    *exec.Cmd
	socket string
	dir    string
	state  string
}

// fsSearchPath is where the backend lives: a local build during development,
// and the path the published image installs to.
var fsSearchPath = []string{
	filepath.Join("fs", "target", "release", "hangar-fs"),
	"/usr/local/bin/hangar-fs",
}

// FindFsBackend locates the filesystem backend.
func FindFsBackend() (string, error) {
	if p, err := exec.LookPath("hangar-fs"); err == nil {
		return p, nil
	}
	for _, p := range fsSearchPath {
		if _, err := os.Stat(p); err == nil {
			return filepath.Abs(p)
		}
	}
	return "", fmt.Errorf("hangar-fs not found on PATH or in %v", fsSearchPath)
}

// DefaultDaxMinFileSize is the file size below which a file is read rather
// than mapped.
//
// Mapping trades boot latency for memory, and the trade is per mapping: a
// mapping costs a round trip to this process, a SHMEM_MAP to the monitor and
// two mmaps, plus window space, which is allocated per mapping whatever the
// file's size. Measured over a boot of the base, every threshold below 64 KiB
// costs more latency than 4 KiB and saves no more memory, so this is the
// floor worth using. docs/plan.md has the numbers.
const DefaultDaxMinFileSize = 64 * 1024

// StartFsBackend exports dir over a vhost-user socket for a single guest.
//
// One backend serves one VM: the vhost-user protocol is a point-to-point
// conversation about one guest's memory, so this is not a shared service.
//
// daxMinFileSize is the size from which a file is offered to the guest as a
// mapping rather than read. It only has an effect if the monitor gives the
// device a window to map into; without one the guest never asks.
//
// state is where the guest's session is kept so it can outlive this process.
// If it exists the backend starts by restoring it, standing in for one the
// guest was already talking to. Empty means the guest cannot be suspended.
func StartFsBackend(ctx context.Context, dir, socket, tag string, daxMinFileSize uint64, state string, verbose bool) (*FsBackend, error) {
	abs, err := filepath.Abs(dir)
	if err != nil {
		return nil, err
	}
	if st, err := os.Stat(abs); err != nil || !st.IsDir() {
		return nil, fmt.Errorf("%s is not a directory to export", abs)
	}
	bin, err := FindFsBackend()
	if err != nil {
		return nil, err
	}
	if tag == "" {
		tag = DefaultVirtiofsTag
	}

	_ = os.Remove(socket)
	if err := os.MkdirAll(filepath.Dir(socket), 0o755); err != nil {
		return nil, err
	}

	cmd := exec.CommandContext(ctx, bin,
		"--socket", socket,
		"--shared-dir", abs,
		"--tag", tag,
		"--dax-min-file-size", fmt.Sprint(daxMinFileSize),
	)
	if state != "" {
		cmd.Args = append(cmd.Args, "--state", state)
	}
	if verbose {
		cmd.Stdout = os.Stderr
		cmd.Stderr = os.Stderr
	}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("starting the fs backend: %w", err)
	}

	// The monitor connects to the socket at startup and fails if it is not
	// there yet, so wait for it rather than racing.
	deadline := time.Now().Add(10 * time.Second)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(socket); err == nil {
			return &FsBackend{cmd: cmd, socket: socket, dir: abs, state: state}, nil
		}
		if cmd.ProcessState != nil && cmd.ProcessState.Exited() {
			return nil, fmt.Errorf("fs backend exited before creating %s", socket)
		}
		time.Sleep(20 * time.Millisecond)
	}
	_ = cmd.Process.Kill()
	return nil, fmt.Errorf("fs backend did not create %s within 10s", socket)
}

// Socket is the path the monitor should connect to.
func (f *FsBackend) Socket() string { return f.socket }

// SaveState writes the guest's session out, for a snapshot to be taken with.
func (f *FsBackend) SaveState(timeout time.Duration) error {
	if f.state == "" {
		return fmt.Errorf("the fs backend was started without a state file")
	}
	return saveState(f.cmd.Process, f.state, timeout)
}

// WaitRestored waits until a restored session is whole again: every nodeid
// rebound and every mapping the guest holds made again. The guest must not
// run before then, because it is already holding addresses in its window.
func (f *FsBackend) WaitRestored(ctx context.Context, timeout time.Duration) error {
	return waitFile(ctx, readyPath(f.state), timeout)
}

// Close stops the backend and removes its socket.
func (f *FsBackend) Close() error {
	if f.cmd != nil && f.cmd.Process != nil {
		_ = f.cmd.Process.Kill()
		_, _ = f.cmd.Process.Wait()
	}
	return os.Remove(f.socket)
}
