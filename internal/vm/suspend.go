package vm

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"syscall"
	"time"
)

// A suspended machine is its disks plus a snapshot of it: its memory and
// device state as Cloud Hypervisor wrote them, and beside them the state
// each backend wrote of what the guest was holding.
//
// suspendedMark is written last, once all of that is complete, so a
// machine with it is resumable and one without it boots afresh. It records
// what the guest had open that lives outside its writable disks -- the
// base, and any read-only disk such as the editor's -- because a guest
// resumed against a different one would find files it holds changed
// underneath it.
const (
	suspendedMark = "suspended.json"
	snapshotDir   = "snapshot"
)

// suspendRecord is what a suspended machine was running against.
type suspendRecord struct {
	Base     string   `json:"base"`
	ReadOnly []string `json:"read_only,omitempty"`
}

func (r suspendRecord) equal(o suspendRecord) bool {
	return r.Base == o.Base && slices.Equal(r.ReadOnly, o.ReadOnly)
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

// record is what the machine runs against.
func (c InstanceConfig) record() suspendRecord {
	r := suspendRecord{Base: fingerprint(c.Base)}
	for _, d := range c.Disks {
		if d.ReadOnly {
			r.ReadOnly = append(r.ReadOnly, fingerprint(d.Path))
		}
	}
	return r
}

// suspended is the machine's suspend record, or nil if it is not
// suspended.
func (c InstanceConfig) suspended() *suspendRecord {
	b, err := os.ReadFile(filepath.Join(c.Dir, suspendedMark))
	if err != nil {
		return nil
	}
	var r suspendRecord
	if json.Unmarshal(b, &r) != nil {
		return nil
	}
	return &r
}

// Suspended reports whether dir holds a suspended machine.
func Suspended(dir string) bool {
	return InstanceConfig{Dir: dir}.suspended() != nil
}

// DiscardSuspend forgets a suspended machine's memory. Its disks are left
// as the guest had them, which is what a power cut leaves.
func DiscardSuspend(dir string) { InstanceConfig{Dir: dir}.discardSuspend() }

func (c InstanceConfig) discardSuspend() {
	os.Remove(filepath.Join(c.Dir, suspendedMark))
	os.RemoveAll(filepath.Join(c.Dir, snapshotDir))
	os.RemoveAll(filepath.Join(c.Dir, snapshotDir+".new"))
}

// clearBackendState removes what backends saved of an earlier guest, which
// a fresh boot's backends would otherwise restore into a guest that never
// held it.
func (c InstanceConfig) clearBackendState() {
	for _, f := range []string{"fs.json", "fs.ready", "gpu.json", "gpu.ready"} {
		os.Remove(filepath.Join(c.Dir, f))
	}
}

// Suspend writes the machine to its directory and stops it; Boot resumes
// it. If it cannot, the guest carries on running as it was, and the
// instance with it.
//
// The guest is paused first so nothing it holds changes while it is written
// down; each backend then records what the guest is holding, and only then
// is the snapshot taken, so they all describe the same instant.
func (i *Instance) Suspend(ctx context.Context) error {
	c := i.cfg
	snap := filepath.Join(c.Dir, snapshotDir)
	partial := snap + ".new"
	err := func() error {
		if err := i.api.Pause(ctx); err != nil {
			return fmt.Errorf("pausing: %w", err)
		}
		if i.fs != nil {
			if err := i.fs.SaveState(30 * time.Second); err != nil {
				return fmt.Errorf("saving the file system backend's state: %w", err)
			}
		}
		if i.gpu != nil {
			if err := i.gpu.SaveState(30 * time.Second); err != nil {
				return fmt.Errorf("saving the GPU backend's state: %w", err)
			}
		}
		os.RemoveAll(partial)
		if err := os.MkdirAll(partial, 0o700); err != nil {
			return err
		}
		if err := i.api.Snapshot(ctx, partial); err != nil {
			return fmt.Errorf("taking the snapshot: %w", err)
		}
		os.RemoveAll(snap)
		if err := os.Rename(partial, snap); err != nil {
			return err
		}
		b, err := json.Marshal(c.record())
		if err != nil {
			return err
		}
		return writeFileAtomic(filepath.Join(c.Dir, suspendedMark), b)
	}()
	if err != nil {
		c.discardSuspend()
		if rerr := i.api.Resume(ctx); rerr != nil {
			c.Log.Warn("could not resume the guest after a failed suspend", "err", rerr)
		}
		return err
	}

	// With the state on disk the monitor has nothing left to do.
	if err := i.api.Shutdown(ctx); err != nil && !errors.Is(err, context.Canceled) {
		c.Log.Warn("stopping the monitor after suspending", "err", err)
	}
	select {
	case <-i.exited:
	case <-time.After(15 * time.Second):
	}
	i.Close()
	return nil
}

func writeFileAtomic(path string, b []byte) error {
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}
