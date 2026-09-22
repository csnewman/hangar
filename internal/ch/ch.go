// Package ch builds Cloud Hypervisor command lines and supervises the
// resulting process.
//
// The shape here follows docs/cloud-hypervisor-switch.md: direct kernel boot,
// the read-only base layer over virtio-fs, virtio-blk for writable storage,
// and guest memory as a shared memfd on transparent huge pages.
//
// Guest memory is always shared. Cloud Hypervisor's virtio-fs is vhost-user
// only and it refuses --fs without shared memory, so there is no configuration
// in which a Hangar guest has private memory, and no reason to offer one.
// Shared memory takes huge pages only when the host allows it, which is what
// CheckHugePages exists to verify.
package ch

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// QUEUE_SIZE is the depth of each of the GPU's two virtqueues, and has to
// match what the backend offers.
const QUEUE_SIZE = 256

// FS_QUEUE_SIZE is the depth of each of virtio-fs's virtqueues. Every read
// the guest does is a request through one, so it is deeper than the GPU's.
const FS_QUEUE_SIZE = 1024

// VirtiofsShmID is the identifier the guest looks the DAX window up by.
// virtio-fs defines 0 for its cache.
const VirtiofsShmID = 0

// DefaultVirtiofsTag is the mount tag the guest finds its root under.
const DefaultVirtiofsTag = "hangar-base"

// Disk is a block device attached to the guest.
type Disk struct {
	Path     string
	ReadOnly bool
}

// Config describes one environment VM.
type Config struct {
	Name     string
	Kernel   string // host path to the guest kernel
	Initrd   string // optional
	Disks    []Disk // first disk becomes /dev/vda
	MemoryMB int
	CPUs     int

	// ConsoleFile, when set, writes the guest serial console to this file
	// instead of attaching it to the terminal. Used for unattended runs.
	ConsoleFile string

	// ConsoleTTY is the serial device as the guest sees it, for the kernel
	// command line.
	ConsoleTTY string

	// ExtraCmdline is appended to the generated kernel command line.
	ExtraCmdline string

	// GuestCID gives the guest a vsock context ID. Zero leaves the agent
	// channel out.
	//
	// Cloud Hypervisor implements vsock in userspace rather than through the
	// host kernel, so VsockSocket is where the host end lives; see
	// vsock.HybridListener.
	GuestCID    uint32
	VsockSocket string

	// VirtiofsSocket is a hangar-fs vhost-user socket. The directory that
	// backend exports becomes the guest's root. Empty leaves it out.
	//
	// The mount tag is the backend's to set, since it is the backend that
	// answers the guest's config space.
	VirtiofsSocket string

	// VirtiofsDaxMiB sizes the DAX window the guest maps files into. Zero
	// leaves the guest reading every file through the backend instead.
	//
	// The window costs the guest nothing until it maps something into it: it
	// is address space, not memory. What it buys is the base being read
	// through the host's page cache rather than copied into each guest, which
	// is worth more the more guests share a base. docs/plan.md has the
	// numbers.
	VirtiofsDaxMiB int

	// NetSocket is a passt vhost-user socket. Empty leaves the guest with no
	// network.
	NetSocket string

	// GpuSocket is a hangar-gpu vhost-user socket. The guest gets a
	// virtio-gpu device rendered by that process. Empty leaves it out.
	GpuSocket string
	// GpuShmMiB sizes the window the guest maps blob resources into, and
	// GpuShmID is the identifier the guest looks it up by. virtio-gpu
	// defines 1 for its host-visible window.
	GpuShmMiB int
	GpuShmID  int

	// APISocket, when set, lets ch-remote drive the running VM, which is how
	// the balloon is resized.
	APISocket string

	// Seccomp selects the monitor's syscall filtering: "true", "false",
	// "log" or "errno". Empty leaves the monitor's default. "log" records
	// refusals in the host's audit log instead of killing the guest, which is
	// how the renderer's syscall set is extended.
	Seccomp string
}

func (c *Config) applyDefaults() {
	if c.MemoryMB == 0 {
		c.MemoryMB = 4096
	}
	if c.CPUs == 0 {
		c.CPUs = 2
	}
	if c.Name == "" {
		c.Name = "hangar-env"
	}
	if c.ConsoleTTY == "" {
		c.ConsoleTTY = "ttyAMA0"
	}
}

// Args builds the Cloud Hypervisor argument list.
func (c *Config) Args() ([]string, error) {
	c.applyDefaults()

	if c.Kernel == "" {
		return nil, fmt.Errorf("kernel path is required")
	}
	if len(c.Disks) == 0 {
		return nil, fmt.Errorf("at least one disk is required")
	}
	if c.GuestCID != 0 && c.VsockSocket == "" {
		return nil, fmt.Errorf("a guest CID needs a vsock socket path")
	}

	args := []string{
		"--kernel", c.Kernel,
		"--cpus", "boot=" + strconv.Itoa(c.CPUs),

		// shared=on is not optional: vhost-user backends read and write the
		// guest's memory directly. Huge pages come from the host's shmem
		// policy rather than from a flag here -- Cloud Hypervisor already
		// asks for them with madvise.
		"--memory", fmt.Sprintf("size=%dM,shared=on", c.MemoryMB),

		// free-page-reporting is the whole point of the balloon: the guest
		// hands back pages as it frees them and the host takes them without
		// anyone deciding a target size.
		"--balloon", "size=0,free_page_reporting=on",
	}

	if c.Initrd != "" {
		args = append(args, "--initramfs", c.Initrd)
	}

	// The guest console is a serial port, so the emulated console device is
	// turned off rather than left to compete for the same output.
	args = append(args, "--console", "off")
	if c.ConsoleFile != "" {
		args = append(args, "--serial", "file="+c.ConsoleFile)
	} else {
		args = append(args, "--serial", "tty")
	}

	if len(c.Disks) > 0 {
		disk := []string{"--disk"}
		for _, d := range c.Disks {
			// image_type=raw is stated rather than detected: these are raw
			// ext4 images, and letting the monitor guess invites it to read a
			// guest-writable file as a QCOW header.
			spec := "path=" + d.Path + ",image_type=raw"
			if d.ReadOnly {
				spec += ",readonly=on"
			}
			disk = append(disk, spec)
		}
		args = append(args, disk...)
	}

	if c.VirtiofsSocket != "" {
		// The generic device rather than --fs, because only it can be given
		// a shared memory window; the tag comes from the backend either way.
		spec := fmt.Sprintf(
			"socket=%s,device_type=fs,queue_sizes=[%d,%d]",
			c.VirtiofsSocket, FS_QUEUE_SIZE, FS_QUEUE_SIZE,
		)
		if c.VirtiofsDaxMiB > 0 {
			spec += fmt.Sprintf(",shm_size=%dM,shm_id=%d", c.VirtiofsDaxMiB, VirtiofsShmID)
		}
		args = append(args, "--generic-vhost-user", spec)
	}

	if c.GuestCID != 0 {
		args = append(args, "--vsock", fmt.Sprintf("cid=%d,socket=%s", c.GuestCID, c.VsockSocket))
	}

	if c.NetSocket != "" {
		// passt in vhost-user mode is the backend, so it owns the socket and
		// listens; Cloud Hypervisor connects, which is vhost_mode=client and
		// is its default.
		args = append(args, "--net", "vhost_user=on,socket="+c.NetSocket)
	}

	if c.GpuSocket != "" {
		// The device type is named rather than numbered, and the window is
		// published by the monitor for the backend to map blobs into.
		spec := fmt.Sprintf(
			"socket=%s,device_type=gpu,queue_sizes=[%d,%d]",
			c.GpuSocket, QUEUE_SIZE, QUEUE_SIZE,
		)
		if c.GpuShmMiB > 0 {
			id := c.GpuShmID
			if id == 0 {
				id = 1
			}
			spec += fmt.Sprintf(",shm_size=%dM,shm_id=%d", c.GpuShmMiB, id)
		}
		args = append(args, "--generic-vhost-user", spec)
	}

	if c.Seccomp != "" {
		args = append(args, "--seccomp", c.Seccomp)
	}

	if c.APISocket != "" {
		args = append(args, "--api-socket", c.APISocket)
	}

	args = append(args, "--cmdline", c.cmdline())
	return args, nil
}

func (c *Config) cmdline() string {
	// No root= : the initramfs mounts the base read-only, the upper layer
	// read-write, stacks overlayfs and switch_roots into the result.
	cl := "console=" + c.ConsoleTTY
	cl += " systemd.show_status=1"
	if c.ExtraCmdline != "" {
		cl += " " + c.ExtraCmdline
	}
	return cl
}

// searchPath is where Cloud Hypervisor lives: a local build during
// development, and the path the published image installs to.
var searchPath = []string{
	filepath.Join("out", "cloud-hypervisor"),
	"/usr/local/bin/cloud-hypervisor",
	"/usr/bin/cloud-hypervisor",
}

// Find locates the monitor.
func Find() (string, error) {
	if p, err := exec.LookPath("cloud-hypervisor"); err == nil {
		return p, nil
	}
	for _, p := range searchPath {
		if _, err := os.Stat(p); err == nil {
			return filepath.Abs(p)
		}
	}
	return "", fmt.Errorf("cloud-hypervisor not found on PATH or in %s",
		strings.Join(searchPath, " or "))
}

// Run launches the VM and blocks until it exits.
func Run(ctx context.Context, cfg *Config) error {
	args, err := cfg.Args()
	if err != nil {
		return err
	}
	return monitor(ctx, args, nil)
}

// Restore brings a suspended guest back from the snapshot in dir.
//
// The monitor is started with nothing but its API socket, since everything
// else -- the machine, its devices, their sockets -- is in the snapshot. The
// backends those devices connect to must already be listening, as for Run.
//
// The guest comes back paused, and ready is called before it is resumed. That
// gap is the only moment a backend can rebuild state the guest is already
// relying on without the guest racing it, so ready should return only once
// every backend has.
func Restore(ctx context.Context, apiSocket, dir, seccomp string, ready func(context.Context) error) error {
	_ = os.Remove(apiSocket)
	args := []string{"--api-socket", apiSocket}
	if seccomp != "" {
		args = append(args, "--seccomp", seccomp)
	}
	return monitor(ctx, args, func(ctx context.Context) error {
		api := NewAPI(apiSocket)
		if err := api.WaitReady(ctx, 10*time.Second); err != nil {
			return err
		}
		if err := api.Restore(ctx, dir); err != nil {
			return err
		}
		if ready != nil {
			if err := ready(ctx); err != nil {
				return fmt.Errorf("waiting for the backends to restore: %w", err)
			}
		}
		return api.Resume(ctx)
	})
}

// monitor runs the monitor in the foreground until it exits.
//
// started, if given, runs once the process is up; an error from it stops the
// monitor and is returned.
func monitor(ctx context.Context, args []string, started func(context.Context) error) error {
	bin, err := Find()
	if err != nil {
		return err
	}

	// Ctrl-C should shut the VM down rather than kill us and orphan it.
	ctx, stop := signal.NotifyContext(ctx, os.Interrupt, syscall.SIGTERM)
	defer stop()

	cmd := exec.CommandContext(ctx, bin, args...)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Cancel = func() error { return cmd.Process.Signal(syscall.SIGTERM) }

	if err := cmd.Start(); err != nil {
		return fmt.Errorf("starting cloud-hypervisor: %w", err)
	}
	if started != nil {
		if err := started(ctx); err != nil {
			_ = cmd.Process.Signal(syscall.SIGTERM)
			_ = cmd.Wait()
			return err
		}
	}
	if err := cmd.Wait(); err != nil {
		if ctx.Err() != nil {
			return nil // we asked it to stop
		}
		return fmt.Errorf("cloud-hypervisor exited: %w", err)
	}
	return nil
}

// PrintCommand renders the command line for debugging, one argument per line
// so it can be copied and edited by hand.
func PrintCommand(cfg *Config) (string, error) {
	args, err := cfg.Args()
	if err != nil {
		return "", err
	}
	bin, err := Find()
	if err != nil {
		bin = "cloud-hypervisor"
	}
	out := bin
	for i := 0; i < len(args); i++ {
		line := args[i]
		// --disk takes one value per disk, so a flag's values run until the
		// next flag rather than stopping at the first.
		for i+1 < len(args) && !strings.HasPrefix(args[i+1], "--") {
			line += " " + args[i+1]
			i++
		}
		out += " \\\n    " + line
	}
	return out, nil
}
