//go:build linux

package main

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"

	"golang.org/x/sys/unix"
)

// The agent is also the initramfs's init. The worker builds each boot's
// initramfs from the agent alone, so no image carries an initramfs, and the
// agent that runs is always the worker's.
//
// As PID 1 it assembles the root and hands over to the image's own init:
//
//	/base  the image, read-only, over virtiofs (tag hangar-base)
//	/rw    the environment's writable disk, the first virtio-blk device
//	/root  overlayfs of the two, which becomes /
//
// /var/lib/docker is not part of the overlay: overlay2 cannot stack on
// overlayfs, so the image's fstab mounts a disk of its own there by label.

const (
	baseTag     = "hangar-base"
	upperDisk   = "/dev/vda"
	newRoot     = "/root"
	agentInRoot = "/usr/local/bin/hangar-agent"
	// diskWait covers the kernel still probing the virtio-blk device when
	// init starts.
	diskWait = 10 * time.Second
)

// initRoot assembles the root and executes the image's init. It returns only
// if that fails, having said why on the console.
func initRoot() {
	if err := assembleRoot(); err != nil {
		fmt.Fprintf(os.Stderr, "INITRAMFS: %v\n", err)
		fmt.Fprintln(os.Stderr, "INITRAMFS: cannot start the environment; see above")
		// PID 1 exiting panics the kernel; waiting leaves the message on
		// the console for whoever reads it.
		for {
			time.Sleep(time.Hour)
		}
	}
}

func assembleRoot() error {
	for _, d := range []string{"/proc", "/sys", "/dev", "/base", "/rw", newRoot} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			return err
		}
	}
	mounts := []struct{ src, dst, fstype, data string }{
		{"proc", "/proc", "proc", ""},
		{"sysfs", "/sys", "sysfs", ""},
		{"devtmpfs", "/dev", "devtmpfs", ""},
	}
	for _, m := range mounts {
		if err := unix.Mount(m.src, m.dst, m.fstype, 0, m.data); err != nil {
			return fmt.Errorf("mounting %s: %w", m.dst, err)
		}
	}

	if err := unix.Mount(baseTag, "/base", "virtiofs", unix.MS_RDONLY, ""); err != nil {
		return fmt.Errorf("mounting the base over virtiofs (%s): %w", baseTag, err)
	}
	fmt.Fprintln(os.Stderr, "INITRAMFS: base over virtiofs")

	deadline := time.Now().Add(diskWait)
	for {
		if _, err := os.Stat(upperDisk); err == nil {
			break
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("no writable disk at %s", upperDisk)
		}
		time.Sleep(50 * time.Millisecond)
	}
	if err := unix.Mount(upperDisk, "/rw", "ext4", 0, ""); err != nil {
		return fmt.Errorf("mounting the writable layer (%s): %w", upperDisk, err)
	}
	for _, d := range []string{"/rw/upper", "/rw/work"} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			return err
		}
	}
	if err := unix.Mount("overlay", newRoot, "overlay", 0,
		"lowerdir=/base,upperdir=/rw/upper,workdir=/rw/work"); err != nil {
		return fmt.Errorf("stacking overlayfs: %w", err)
	}

	// The agent the worker supplied, installed where the image's
	// hangar-agent.service runs it.
	if err := copyFile("/init", filepath.Join(newRoot, agentInRoot), 0o755); err != nil {
		return fmt.Errorf("installing the agent: %w", err)
	}

	// The layers stay reachable in the new root rather than orphaned, and
	// the kernel's filesystems move with it.
	for _, m := range []struct{ from, to string }{
		{"/base", "/run/hangar/base"},
		{"/rw", "/run/hangar/rw"},
		{"/dev", "/dev"},
		{"/proc", "/proc"},
		{"/sys", "/sys"},
	} {
		to := filepath.Join(newRoot, m.to)
		if err := os.MkdirAll(to, 0o755); err != nil {
			return err
		}
		if err := unix.Mount(m.from, to, "", unix.MS_MOVE, ""); err != nil {
			return fmt.Errorf("moving %s into the new root: %w", m.from, err)
		}
	}

	// switch_root: the initramfs's own files are freed, the new root is
	// moved over /, and the image's init replaces this process.
	_ = os.Remove("/init")
	if err := unix.Chdir(newRoot); err != nil {
		return err
	}
	if err := unix.Mount(".", "/", "", unix.MS_MOVE, ""); err != nil {
		return fmt.Errorf("moving the new root to /: %w", err)
	}
	if err := unix.Chroot("."); err != nil {
		return err
	}
	if err := unix.Chdir("/"); err != nil {
		return err
	}
	for _, init := range []string{"/sbin/init", "/lib/systemd/systemd", "/usr/lib/systemd/systemd"} {
		if _, err := os.Stat(init); err == nil {
			err = unix.Exec(init, []string{init}, os.Environ())
			return fmt.Errorf("executing %s: %w", init, err)
		}
	}
	return errors.New("the image has no init (/sbin/init)")
}

func copyFile(from, to string, mode os.FileMode) error {
	in, err := os.Open(from)
	if err != nil {
		return err
	}
	defer in.Close()
	if err := os.MkdirAll(filepath.Dir(to), 0o755); err != nil {
		return err
	}
	tmp := to + ".new"
	out, err := os.OpenFile(tmp, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, mode)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		return err
	}
	if err := out.Close(); err != nil {
		return err
	}
	return os.Rename(tmp, to)
}
