package ch

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

// saveState asks a backend to write what its guest is holding, and waits for
// it to have done so.
//
// The backends write on SIGUSR1, into a temporary file renamed over the real
// one, so the file appearing is the sign the write is complete. The old one is
// removed first, or it would look like success before the backend had begun.
func saveState(proc *os.Process, path string, timeout time.Duration) error {
	if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
		return err
	}
	if err := proc.Signal(syscall.SIGUSR1); err != nil {
		return fmt.Errorf("asking the backend to save its state: %w", err)
	}
	return waitFile(context.Background(), path, timeout)
}

// readyPath is where a backend says a restored session is whole again: the
// state file with its extension replaced, as the backend derives it.
func readyPath(state string) string {
	return strings.TrimSuffix(state, filepath.Ext(state)) + ".ready"
}

func waitFile(ctx context.Context, path string, timeout time.Duration) error {
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(path); err == nil {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(20 * time.Millisecond):
		}
	}
	return fmt.Errorf("%s did not appear within %s", path, timeout)
}

// logPathFor is where a backend's log goes: beside its state, so what it did
// to that state can be read alongside it.
func logPathFor(state string) string {
	if state == "" {
		return ""
	}
	return strings.TrimSuffix(state, filepath.Ext(state)) + ".log"
}
