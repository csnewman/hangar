//go:build linux

package main

import (
	"crypto/rand"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"
	"unsafe"

	"golang.org/x/sys/unix"
)

// The agent is also the initramfs's init. The worker builds each boot's
// initramfs from the agent alone, so no image carries an initramfs, and the
// agent that runs is always the worker's.
//
// As PID 1 it assembles the root and hands over to the image's own init:
//
//	/base  the image, read-only: an EROFS image on the device the kernel
//	       command line names (hangar.image=), mounted with DAX if
//	       hangar.image_dax=1
//	/rw    the environment's writable disk, the first virtio-blk device
//	/lower overlayfs of the two
//	/root  hangarfs over /lower, which becomes /
//
// and swaps to the disk hangar.swap= names, if it names one.
//
// /var/lib/docker is not part of the root: overlay2 cannot stack on
// overlayfs. It is /rw/docker, a directory on the same writable disk,
// mounted there beside the root.

const (
	upperDisk = "/dev/vda"
	newRoot   = "/root"
	// lowerDir is where the overlay hangarfs serves is mounted.
	lowerDir = "/lower"
	// diskWait covers the kernel still probing the virtio-blk device when
	// init starts.
	diskWait = 10 * time.Second
)

// mountBase mounts the image at /base.
func mountBase() error {
	b, err := os.ReadFile("/proc/cmdline")
	if err != nil {
		return err
	}
	var device string
	var dax bool
	for _, f := range strings.Fields(string(b)) {
		if v, ok := strings.CutPrefix(f, "hangar.image="); ok {
			device = v
		}
		if f == "hangar.image_dax=1" {
			dax = true
		}
	}
	if device == "" {
		return errors.New("the kernel command line names no image (hangar.image=)")
	}
	if err := waitFor(device); err != nil {
		return err
	}
	opts := ""
	if dax {
		opts = "dax=always"
	}
	if err := unix.Mount(device, "/base", "erofs", unix.MS_RDONLY, opts); err != nil {
		return fmt.Errorf("mounting the base from %s: %w", device, err)
	}
	how := "erofs"
	if dax {
		how += ", dax"
	}
	fmt.Fprintf(os.Stderr, "INITRAMFS: base from %s (%s)\n", device, how)
	return nil
}

// swapFlagDiscard is linux/swap.h's SWAP_FLAG_DISCARD: the device is
// discarded whole when swapping to it starts, and each run of pages as it
// is freed.
const swapFlagDiscard = 0x10000

// swapOn swaps to the disk the kernel command line names (hangar.swap=),
// if it names one. The worker gives each boot an empty disk, so the swap
// header is written here, for this kernel's page size. Discarding is what
// hands the host back the space of what is swapped in again: the disk is
// a sparse file there.
func swapOn() error {
	b, err := os.ReadFile("/proc/cmdline")
	if err != nil {
		return err
	}
	var device string
	for _, f := range strings.Fields(string(b)) {
		if v, ok := strings.CutPrefix(f, "hangar.swap="); ok {
			device = v
		}
	}
	if device == "" {
		return nil
	}
	if err := waitFor(device); err != nil {
		return err
	}
	f, err := os.OpenFile(device, os.O_RDWR, 0)
	if err != nil {
		return err
	}
	defer f.Close()
	size, err := f.Seek(0, io.SeekEnd)
	if err != nil {
		return err
	}
	page := os.Getpagesize()
	pages := size / int64(page)
	if pages < 2 || pages-1 > 1<<32-1 {
		return fmt.Errorf("%s is %d bytes, which is no size for swap", device, size)
	}
	// The first page: boot sector room, then struct swap_header's info at
	// 1024 -- version, last page, bad pages, UUID, label -- and the magic
	// ending the page.
	hdr := make([]byte, page)
	binary.NativeEndian.PutUint32(hdr[1024:], 1)
	binary.NativeEndian.PutUint32(hdr[1028:], uint32(pages-1))
	if _, err := rand.Read(hdr[1036:1052]); err != nil {
		return err
	}
	copy(hdr[1052:1068], "hangar-swap")
	copy(hdr[page-10:], "SWAPSPACE2")
	if _, err := f.WriteAt(hdr, 0); err != nil {
		return err
	}
	if err := f.Sync(); err != nil {
		return err
	}
	path, err := unix.BytePtrFromString(device)
	if err != nil {
		return err
	}
	if _, _, errno := unix.Syscall(unix.SYS_SWAPON, uintptr(unsafe.Pointer(path)), swapFlagDiscard, 0); errno != 0 {
		return fmt.Errorf("swapon %s: %w", device, errno)
	}
	fmt.Fprintf(os.Stderr, "INITRAMFS: swap on %s (%d MiB)\n", device, size>>20)
	return nil
}

// mountDocker gives the new root its Docker store at /var/lib/docker.
func mountDocker() error {
	target := filepath.Join(newRoot, "var", "lib", "docker")
	if err := os.MkdirAll(target, 0o710); err != nil {
		return err
	}
	src := "/rw/docker"
	if err := os.MkdirAll(src, 0o710); err != nil {
		return err
	}
	return unix.Mount(src, target, "", unix.MS_BIND, "")
}

// waitFor waits for a device node, which the kernel may still be probing
// when init starts.
func waitFor(device string) error {
	deadline := time.Now().Add(diskWait)
	for {
		if _, err := os.Stat(device); err == nil {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("no device at %s", device)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

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

	if err := mountBase(); err != nil {
		return err
	}

	if err := waitFor(upperDisk); err != nil {
		return fmt.Errorf("the writable disk: %w", err)
	}
	// discard hands freed blocks back to the host as files are deleted: the
	// disk is a sparse file there, and without it everything an environment
	// ever wrote stays allocated. Nothing in the guest can trim it later,
	// for systemd's /run hides where it is mounted.
	if err := unix.Mount(upperDisk, "/rw", "ext4", 0, "discard"); err != nil {
		return fmt.Errorf("mounting the writable layer (%s): %w", upperDisk, err)
	}
	for _, d := range []string{"/rw/upper", "/rw/work"} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			return err
		}
	}
	// The overlay is hangarfs's lower directory, left in the initramfs:
	// hangarfs holds it for as long as it is mounted. transparent: hangarfs
	// takes no stacking depth of its own, so an overlay can still be
	// mounted on the root.
	if err := os.MkdirAll(lowerDir, 0o755); err != nil {
		return err
	}
	if err := unix.Mount("overlay", lowerDir, "overlay", 0,
		"lowerdir=/base,upperdir=/rw/upper,workdir=/rw/work"); err != nil {
		return fmt.Errorf("stacking overlayfs: %w", err)
	}
	if err := unix.Mount(lowerDir, newRoot, "hangarfs", 0, "transparent"); err != nil {
		return fmt.Errorf("mounting hangarfs over the overlay: %w", err)
	}

	// Without it the environment runs as it would with none.
	if err := swapOn(); err != nil {
		fmt.Fprintf(os.Stderr, "INITRAMFS: swap: %v\n", err)
	}

	// Without it Docker cannot start, but the environment is still usable.
	if err := mountDocker(); err != nil {
		fmt.Fprintf(os.Stderr, "INITRAMFS: mounting Docker's store: %v\n", err)
	}

	// The agent the worker supplied, installed where the image's
	// hangar-agent.service runs it.
	if err := copyFile("/init", filepath.Join(newRoot, agentInRoot), 0o755); err != nil {
		return fmt.Errorf("installing the agent: %w", err)
	}

	if err := installOOMDropIn(newRoot); err != nil {
		return fmt.Errorf("installing the agent's OOM settings: %w", err)
	}

	if err := ensureMachineID(newRoot); err != nil {
		return fmt.Errorf("giving the environment a machine ID: %w", err)
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

// ensureMachineID gives the environment its own machine ID on its first
// boot, where the image has none, before systemd starts. With one, systemd
// does not treat the boot as a machine's first, whose work -- applying the
// presets -- the image did when it was built, and which on some
// distributions resets the default target the image set. The ID is kept on
// the writable layer, so the environment keeps it.
func ensureMachineID(root string) error {
	path := filepath.Join(root, "etc", "machine-id")
	if b, err := os.ReadFile(path); err == nil {
		id := strings.TrimSpace(string(b))
		if len(id) == 32 && strings.Trim(id, "0123456789abcdef") == "" {
			return nil
		}
	}
	var raw [16]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return err
	}
	// Version 4, as systemd makes them.
	raw[6] = raw[6]&0x0f | 0x40
	raw[8] = raw[8]&0x3f | 0x80
	return os.WriteFile(path, []byte(hex.EncodeToString(raw[:])+"\n"), 0o444)
}
