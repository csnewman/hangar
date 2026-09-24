package vm

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"syscall"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/ch"
)

// A suspended environment is its disks plus a snapshot of the machine: its
// memory and device state as Cloud Hypervisor wrote them, and beside them
// the state each backend wrote of what the guest was holding.
//
// suspendedMark is written last, once all of that is complete, so an
// environment with it is resumable and one without it boots afresh. It
// records what the guest had open that lives outside its disks -- the
// image's root filesystem and the editor disk -- because a guest resumed
// against a different one would find files it holds changed underneath it.
const (
	suspendedMark = "suspended.json"
	snapshotDir   = "snapshot"
)

// suspendRecord is what a suspended environment was running against.
type suspendRecord struct {
	Base   string `json:"base"`
	Editor string `json:"editor,omitempty"`
}

// fingerprint identifies a file or directory as the one it is: made again
// in the same place, even from the same source, it is a different one.
func fingerprint(path string) string {
	if path == "" {
		return ""
	}
	st, err := os.Stat(path)
	if err != nil {
		return ""
	}
	id := fmt.Sprintf("%s:%d:%d", path, st.Size(), st.ModTime().UnixNano())
	if sys, ok := st.Sys().(*syscall.Stat_t); ok {
		id += fmt.Sprintf(":%d:%d", sys.Dev, sys.Ino)
	}
	return id
}

func (m *machine) currentRecord(img Image) suspendRecord {
	return suspendRecord{Base: fingerprint(img.Base), Editor: fingerprint(m.rt.cfg.Editor)}
}

// suspendedRecord is the environment's suspend record, or nil if it is not
// suspended.
func (m *machine) suspendedRecord() *suspendRecord {
	b, err := os.ReadFile(filepath.Join(m.dir, suspendedMark))
	if err != nil {
		return nil
	}
	var r suspendRecord
	if json.Unmarshal(b, &r) != nil {
		return nil
	}
	return &r
}

// discardSuspend forgets a suspended environment's memory. Its disks are
// left as the guest had them, which is what a power cut leaves.
func (m *machine) discardSuspend() {
	os.Remove(filepath.Join(m.dir, suspendedMark))
	os.RemoveAll(filepath.Join(m.dir, snapshotDir))
	os.RemoveAll(filepath.Join(m.dir, snapshotDir+".new"))
}

// clearBackendState removes what backends saved of an earlier guest, which
// a fresh boot's backends would otherwise restore into a guest that never
// held it.
func (m *machine) clearBackendState() {
	for _, f := range []string{"fs.json", "fs.ready", "gpu.json", "gpu.ready"} {
		os.Remove(filepath.Join(m.dir, f))
	}
}

// suspend writes the running machine to disk and stops it. If it cannot,
// the guest carries on running as it was.
//
// The guest is paused first so nothing it holds changes while it is written
// down; each backend then records what the guest is holding, and only then
// is the snapshot taken, so they all describe the same instant.
func (m *machine) suspend() error {
	m.mu.Lock()
	inst := m.running
	m.mu.Unlock()
	if inst == nil {
		return errors.New("the machine is not running")
	}
	m.set(api.PhaseSuspending, "")
	start := time.Now()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	defer cancel()

	snap := filepath.Join(m.dir, snapshotDir)
	partial := snap + ".new"
	err := func() error {
		if err := inst.api.Pause(ctx); err != nil {
			return fmt.Errorf("pausing: %w", err)
		}
		if inst.fs != nil {
			if err := inst.fs.SaveState(30 * time.Second); err != nil {
				return fmt.Errorf("saving the file system backend's state: %w", err)
			}
		}
		if inst.gpu != nil {
			if err := inst.gpu.SaveState(30 * time.Second); err != nil {
				return fmt.Errorf("saving the GPU backend's state: %w", err)
			}
		}
		os.RemoveAll(partial)
		if err := os.MkdirAll(partial, 0o700); err != nil {
			return err
		}
		if err := inst.api.Snapshot(ctx, partial); err != nil {
			return fmt.Errorf("taking the snapshot: %w", err)
		}
		os.RemoveAll(snap)
		if err := os.Rename(partial, snap); err != nil {
			return err
		}
		b, err := json.Marshal(m.currentRecord(inst.image))
		if err != nil {
			return err
		}
		return writeFileAtomic(filepath.Join(m.dir, suspendedMark), b)
	}()
	if err != nil {
		m.discardSuspend()
		if rerr := inst.api.Resume(ctx); rerr != nil {
			m.log.Warn("could not resume the guest after a failed suspend", "err", rerr)
		}
		return err
	}

	// With the state on disk the monitor has nothing left to do.
	m.mu.Lock()
	m.running = nil
	m.mu.Unlock()
	if err := inst.api.Shutdown(ctx); err != nil {
		m.log.Warn("stopping the monitor after suspending", "err", err)
	}
	select {
	case <-inst.exited:
	case <-time.After(15 * time.Second):
	}
	inst.close()
	m.log.Info("suspended", "seconds", time.Since(start).Seconds())
	return nil
}

// restore starts the monitor from the environment's snapshot and resumes
// the guest once every backend has rebuilt what the guest holds of it.
//
// The monitor starts with nothing but its API socket: the machine, its
// devices and their sockets are all in the snapshot, at the paths they had,
// which the backends are listening on again.
func (m *machine) restore(ctx context.Context, monCtx context.Context, cfg *ch.Config, inst *instance) error {
	args := []string{"--api-socket", cfg.APISocket}
	if cfg.Seccomp != "" {
		args = append(args, "--seccomp", cfg.Seccomp)
	}
	if err := m.launchArgs(monCtx, args, inst); err != nil {
		return err
	}
	inst.api = ch.NewAPI(cfg.APISocket)
	if err := inst.api.WaitReady(ctx, 10*time.Second); err != nil {
		return err
	}
	if err := inst.api.Restore(ctx, filepath.Join(m.dir, snapshotDir)); err != nil {
		return fmt.Errorf("restoring the snapshot: %w", err)
	}
	if inst.fs != nil {
		if err := inst.fs.WaitRestored(ctx, 2*time.Minute); err != nil {
			return fmt.Errorf("restoring the file system backend: %w", err)
		}
	}
	if inst.gpu != nil {
		if err := inst.gpu.WaitRestored(ctx, 2*time.Minute); err != nil {
			return fmt.Errorf("restoring the GPU backend: %w", err)
		}
	}
	return inst.api.Resume(ctx)
}

func writeFileAtomic(path string, b []byte) error {
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}
