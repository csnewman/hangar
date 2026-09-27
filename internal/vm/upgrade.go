package vm

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"time"

	"github.com/csnewman/hangar/internal/api"
)

// An environment is pinned to the copy of its image its writable layer was
// made over. Upgrading re-pins it to a newer copy, keeping its disks: the
// files it has changed stay as they are, over the newer image. The writable
// disk is copied first, into rollbackDir, and going back restores that copy
// and the old pin. The Docker disk is the environment's own, whatever the
// image, and is left alone either way.

// rollbackDir, in an environment's directory, holds the copy of its writable
// disk from before its last upgrade and the pin it had then.
const rollbackDir = "rollback"

// maxConflicts is how many of the files an upgrade would leave hiding the
// image's are listed; the rest are counted.
const maxConflicts = 50

// rollback is the record kept in rollbackDir.
type rollback struct {
	api.ImageCopy
	At        time.Time `json:"at"`
	SizeBytes int64     `json:"size_bytes"`
}

func readRollback(dir string) *rollback {
	b, err := os.ReadFile(filepath.Join(dir, rollbackDir, imagePinFile))
	if err != nil {
		return nil
	}
	var r rollback
	if json.Unmarshal(b, &r) != nil || r.Ref == "" {
		return nil
	}
	if _, err := os.Stat(filepath.Join(dir, rollbackDir, "upper.ext4")); err != nil {
		return nil
	}
	return &r
}

// imageState is what the environment reports about its image: the pin, a
// newer copy to upgrade to and what upgrading would hide, and the rollback
// kept from its last upgrade. Checking what an upgrade would hide is started
// here, for a stopped environment, since it mounts the writable disk.
func (m *machine) imageState(o *api.ObservedEnvironment) {
	m.mu.Lock()
	pin, rb, update, from := m.image, m.rollback, m.update, m.updateFrom
	phase, checking := m.phase, m.checking
	m.mu.Unlock()
	if pin == nil {
		return
	}
	o.ImagePinned = true
	if rb != nil {
		o.ImageRollback = &api.ImageRollback{Digest: rb.Digest, At: rb.At, SizeBytes: rb.SizeBytes}
	}
	latest, ok := m.rt.store.Latest(pin.Ref)
	if !ok || latest == pin.Digest || !m.hasDisks() {
		return
	}
	if update != nil && update.Digest == latest && from == pin.Digest {
		u := *update
		o.ImageUpdate = &u
		return
	}
	o.ImageUpdate = &api.ImageUpdate{Digest: latest}
	if (phase == api.PhaseStopped || phase == api.PhaseFailed) && !checking {
		m.mu.Lock()
		m.checking = true
		m.mu.Unlock()
		go m.checkUpdate(*pin, api.ImageCopy{Ref: pin.Ref, Digest: latest})
	}
}

// checkUpdate compares the environment's writable layer with the copy of
// its image it is pinned to and a newer one, while it is stopped. It stands
// down if the environment starts first.
func (m *machine) checkUpdate(from, to api.ImageCopy) {
	defer func() {
		m.mu.Lock()
		m.checking = false
		m.mu.Unlock()
	}()
	if !m.disk.TryLock() {
		return
	}
	defer m.disk.Unlock()
	if phase, _ := m.status(); phase != api.PhaseStopped && phase != api.PhaseFailed {
		return
	}
	u := &api.ImageUpdate{Digest: to.Digest, Checked: true}
	if err := m.compareImages(m.rt.ctx, from, to, u); err != nil {
		m.log.Warn("comparing the writable layer with a newer copy of the image", "image", to.Ref, "err", err)
		u = &api.ImageUpdate{Digest: to.Digest, Checked: true, Error: err.Error()}
	}
	m.mu.Lock()
	m.update, m.updateFrom = u, from.Digest
	m.mu.Unlock()
	m.rt.notify()
}

func (m *machine) compareImages(ctx context.Context, from, to api.ImageCopy, u *api.ImageUpdate) error {
	old, err := m.rt.store.Get(ctx, from, nil)
	if err != nil {
		return fmt.Errorf("the copy it is pinned to: %w", err)
	}
	newer, err := m.rt.store.Get(ctx, to, nil)
	if err != nil {
		return fmt.Errorf("the newer copy: %w", err)
	}
	mnt, err := os.MkdirTemp("", "hangar-upper-")
	if err != nil {
		return err
	}
	defer os.Remove(mnt)
	unmount, err := mountReadOnly(filepath.Join(m.dir, "upper.ext4"), mnt)
	if err != nil {
		return fmt.Errorf("mounting its writable disk: %w", err)
	}
	defer unmount()
	conflicts, packages, err := compareLayer(filepath.Join(mnt, "upper"), old.Base, newer.Base)
	if err != nil {
		return err
	}
	if len(conflicts) > maxConflicts {
		u.MoreConflicts = len(conflicts) - maxConflicts
		conflicts = conflicts[:maxConflicts]
	}
	u.Conflicts, u.Packages = conflicts, packages
	return nil
}

// changeImage re-pins a stopped environment as its spec asks, before it
// boots: back to the copy its rollback was taken from, restoring that
// writable disk, or on to another copy, keeping a copy of the writable disk
// to roll back to.
func (m *machine) changeImage(ctx context.Context, spec api.EnvironmentSpec) error {
	m.mu.Lock()
	pin, rb := m.image, m.rollback
	m.update = nil
	m.mu.Unlock()
	if spec.PinDigest == nil || pin == nil || *spec.PinDigest == pin.Digest {
		return nil
	}
	if _, err := os.Stat(filepath.Join(m.dir, suspendedMark)); err == nil {
		// A suspended machine's memory refers to the files it had.
		return nil
	}
	want := api.ImageCopy{Ref: pin.Ref, Digest: *spec.PinDigest}
	if rb != nil && rb.ImageCopy == want {
		return m.restoreRollback(rb)
	}
	if _, err := m.rt.store.Get(ctx, want, m.fetchProgress); err != nil {
		return err
	}
	if err := m.snapshot(ctx, *pin); err != nil {
		return fmt.Errorf("copying its writable disk to roll back to: %w", err)
	}
	if err := m.writePin(want); err != nil {
		return err
	}
	m.log.Info("upgraded the image", "image", want.Ref, "from", pin.Digest, "to", want.Digest)
	return nil
}

// snapshot copies the writable disk into rollbackDir, with the pin it was
// made over, replacing any rollback from an earlier upgrade.
func (m *machine) snapshot(ctx context.Context, pin api.ImageCopy) error {
	dst := filepath.Join(m.dir, rollbackDir)
	tmp := dst + ".tmp"
	if err := os.RemoveAll(tmp); err != nil {
		return err
	}
	// The rollback being replaced is one upgrade further back, and would
	// double the space needed.
	if err := os.RemoveAll(dst); err != nil {
		return err
	}
	m.mu.Lock()
	m.rollback = nil
	m.mu.Unlock()
	if err := os.MkdirAll(tmp, 0o755); err != nil {
		return err
	}
	m.step(api.StepSnapshot, "copying its writable disk to roll back to")
	size, err := copySparse(ctx, filepath.Join(m.dir, "upper.ext4"), filepath.Join(tmp, "upper.ext4"),
		func(done, total int64) { m.measure(done, total, "bytes") })
	if err != nil {
		os.RemoveAll(tmp)
		return err
	}
	rb := &rollback{ImageCopy: pin, At: time.Now().UTC(), SizeBytes: size}
	b, err := json.Marshal(rb)
	if err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(tmp, imagePinFile), b, 0o644); err != nil {
		os.RemoveAll(tmp)
		return err
	}
	if err := os.Rename(tmp, dst); err != nil {
		os.RemoveAll(tmp)
		return err
	}
	m.mu.Lock()
	m.rollback = rb
	m.mu.Unlock()
	return nil
}

// restoreRollback puts back the writable disk and pin kept from before the
// last upgrade, dropping what was written since.
func (m *machine) restoreRollback(rb *rollback) error {
	m.step(api.StepDisks, "restoring the writable disk from before the upgrade")
	if err := os.Rename(filepath.Join(m.dir, rollbackDir, "upper.ext4"), filepath.Join(m.dir, "upper.ext4")); err != nil {
		return fmt.Errorf("restoring its writable disk: %w", err)
	}
	if err := m.writePin(rb.ImageCopy); err != nil {
		return err
	}
	m.mu.Lock()
	m.rollback = nil
	m.mu.Unlock()
	if err := os.RemoveAll(filepath.Join(m.dir, rollbackDir)); err != nil {
		m.log.Warn("removing the rollback", "err", err)
	}
	m.log.Info("rolled the image back", "image", rb.Ref, "to", rb.Digest)
	m.rt.prune()
	m.rt.notify()
	return nil
}

// writePin pins the environment to c.
func (m *machine) writePin(c api.ImageCopy) error {
	b, err := json.Marshal(c)
	if err != nil {
		return err
	}
	tmp := filepath.Join(m.dir, imagePinFile+".tmp")
	if err := os.WriteFile(tmp, b, 0o644); err != nil {
		return err
	}
	if err := os.Rename(tmp, filepath.Join(m.dir, imagePinFile)); err != nil {
		return err
	}
	m.mu.Lock()
	m.image = &c
	m.mu.Unlock()
	m.rt.notify()
	return nil
}

// dropRollback discards the rollback when the spec asks, keeping the
// upgrade, so the copy of the image it was taken from can go.
func (m *machine) dropRollback(spec api.EnvironmentSpec) {
	m.mu.Lock()
	rb := m.rollback
	m.mu.Unlock()
	if !spec.DropRollback || rb == nil {
		return
	}
	m.disk.Lock()
	err := os.RemoveAll(filepath.Join(m.dir, rollbackDir))
	m.disk.Unlock()
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		m.log.Warn("discarding the rollback", "err", err)
		return
	}
	m.mu.Lock()
	m.rollback = nil
	m.mu.Unlock()
	m.log.Info("kept the upgrade and discarded its rollback", "image", rb.Ref)
	m.rt.prune()
	m.rt.notify()
}

// fetchProgress reports fetching a copy of the image as the start's
// download and unpack steps.
func (m *machine) fetchProgress(p FetchProgress) {
	switch p.Stage {
	case StageCopy:
		m.step(api.StepDownload, "copying the image")
	case StageDownload:
		m.step(api.StepDownload, "downloading the image")
		m.measure(p.Done, p.Total, "bytes")
	case StageUnpack:
		reason := "unpacking the image"
		if p.Layers > 1 {
			reason += fmt.Sprintf(": layer %d of %d", p.Layer, p.Layers)
		}
		m.step(api.StepUnpack, reason)
		m.measure(p.Done, p.Total, "bytes")
	}
}
