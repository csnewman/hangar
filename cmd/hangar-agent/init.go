//go:build linux

package main

import (
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"syscall"
	"time"

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
//	       hangar.image_dax=1, or else over virtiofs (tag hangar-base)
//	/rw    the environment's writable disk, the first virtio-blk device
//	/root  overlayfs of the two, which becomes /; with hangar.root=hangarfs
//	       on the command line, hangarfs over that overlayfs (mounted at
//	       /lower); or with hangar.root=router, hangar-router merging them,
//	       run from the initramfs as the root filesystem's daemon
//
// /var/lib/docker is not part of the root: overlay2 cannot stack on
// overlayfs (or on hangar-router). It is /rw/docker, a directory on the same
// writable disk, mounted there beside the root; or, for an environment made
// with a disk of its own for Docker, that disk (hangar.docker=).

const (
	baseTag   = "hangar-base"
	upperDisk = "/dev/vda"
	newRoot   = "/root"
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
		if err := unix.Mount(baseTag, "/base", "virtiofs", unix.MS_RDONLY, ""); err != nil {
			return fmt.Errorf("mounting the base over virtiofs (%s): %w", baseTag, err)
		}
		fmt.Fprintln(os.Stderr, "INITRAMFS: base over virtiofs")
		return nil
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

// cmdlineValue is the value of a key=value parameter on the kernel command
// line, or "" without it.
func cmdlineValue(key string) string {
	b, err := os.ReadFile("/proc/cmdline")
	if err != nil {
		return ""
	}
	for _, f := range strings.Fields(string(b)) {
		if v, ok := strings.CutPrefix(f, key+"="); ok {
			return v
		}
	}
	return ""
}

// mountDocker gives the new root its Docker store at /var/lib/docker.
func mountDocker() error {
	target := filepath.Join(newRoot, "var", "lib", "docker")
	if err := os.MkdirAll(target, 0o710); err != nil {
		return err
	}
	if dev := cmdlineValue("hangar.docker"); dev != "" {
		if err := waitFor(dev); err != nil {
			return err
		}
		return unix.Mount(dev, target, "ext4", 0, "discard")
	}
	src := "/rw/docker"
	if err := os.MkdirAll(src, 0o710); err != nil {
		return err
	}
	return unix.Mount(src, target, "", unix.MS_BIND, "")
}

// cmdlineHas is whether the kernel command line has a parameter.
func cmdlineHas(param string) bool {
	b, err := os.ReadFile("/proc/cmdline")
	return err == nil && slices.Contains(strings.Fields(string(b)), param)
}

// routerBin is where the worker puts hangar-router in the initramfs.
const routerBin = "/hangar-router"

// routerRingEntries is how many requests each CPU's io_uring ring holds:
// one being answered while the next waits.
const routerRingEntries = "2"

// fuseMagic is the type statfs reports for a FUSE filesystem.
const fuseMagic = 0x65735546

// startRouter runs hangar-router over the writable layer and the base, as
// the new root, and waits for it to be mounted.
//
// It is the root filesystem's daemon: started from the initramfs, it
// outlives the hand-over to the image's init, and its name starts with @,
// which tells systemd to leave it running when it stops everything else at
// shutdown, since the root it is unmounting is the router's.
func startRouter() error {
	r, w, err := os.Pipe()
	if err != nil {
		return err
	}
	defer r.Close()
	attr := &os.ProcAttr{Files: []*os.File{nil, os.Stderr, os.Stderr, w},
		Sys: &syscall.SysProcAttr{Setsid: true}}
	p, err := os.StartProcess(routerBin, []string{"@hangar-router",
		"--upper", "/rw/upper", "--lower", "/base", "--mountpoint", newRoot,
		"--uring", routerRingEntries}, attr)
	w.Close()
	if err != nil {
		return fmt.Errorf("starting the router: %w", err)
	}
	// The pipe's write end is only the router's: it closes when the router
	// exits, which is how a router that failed to start is noticed.
	exited := make(chan struct{})
	go func() {
		io.Copy(io.Discard, r)
		close(exited)
	}()
	deadline := time.Now().Add(diskWait)
	for {
		var st unix.Statfs_t
		if unix.Statfs(newRoot, &st) == nil && uint32(st.Type) == fuseMagic {
			break
		}
		select {
		case <-exited:
			return fmt.Errorf("the router exited before mounting the root (pid %d)", p.Pid)
		default:
		}
		if time.Now().After(deadline) {
			return errors.New("the router did not mount the root")
		}
		time.Sleep(20 * time.Millisecond)
	}
	// Its binary goes with the rest of the initramfs; the running router
	// keeps its own copy.
	_ = os.Remove(routerBin)
	fmt.Fprintln(os.Stderr, "INITRAMFS: root by hangar-router")
	return nil
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
	routerRoot := cmdlineHas("hangar.root=router")
	hangarfsRoot := cmdlineHas("hangar.root=hangarfs")
	if routerRoot {
		if err := startRouter(); err != nil {
			return err
		}
	} else {
		// Under hangarfs, the overlay is its lower directory, left in the
		// initramfs: hangarfs holds it for as long as it is mounted.
		at := newRoot
		if hangarfsRoot {
			at = "/lower"
			if err := os.MkdirAll(at, 0o755); err != nil {
				return err
			}
		}
		if err := unix.Mount("overlay", at, "overlay", 0,
			"lowerdir=/base,upperdir=/rw/upper,workdir=/rw/work"); err != nil {
			return fmt.Errorf("stacking overlayfs: %w", err)
		}
		if hangarfsRoot {
			// transparent: hangarfs takes no stacking depth of its own,
			// so an overlay can still be mounted on the root.
			err := unix.Mount(at, newRoot, "hangarfs", 0, "transparent")
			switch {
			case errors.Is(err, unix.ENODEV):
				// A kernel built without hangarfs: the overlay is the
				// root, and locks on shared files are this guest's own.
				if err := unix.Mount(at, newRoot, "", unix.MS_MOVE, ""); err != nil {
					return fmt.Errorf("moving the overlay to the root: %w", err)
				}
				fmt.Fprintln(os.Stderr, "INITRAMFS: this kernel has no hangarfs; root by overlayfs alone")
			case err != nil:
				return fmt.Errorf("mounting hangarfs over the overlay: %w", err)
			default:
				fmt.Fprintln(os.Stderr, "INITRAMFS: root by hangarfs over overlayfs")
			}
		}
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
	if routerRoot {
		// The router stays in the initramfs, and reopens files through
		// /proc/self/fd: it is given a /proc of its own there, for the one
		// it had has moved into the new root.
		if err := unix.Mount("proc", "/proc", "proc", 0, ""); err != nil {
			return fmt.Errorf("mounting the router's /proc: %w", err)
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
