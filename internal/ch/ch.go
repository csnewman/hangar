// Package ch builds Cloud Hypervisor command lines and supervises the
// resulting process.
//
// The shape here follows docs/cloud-hypervisor-switch.md: direct kernel boot,
// virtio-blk for the read-only image and for writable storage, and guest
// memory as a shared memfd on transparent huge pages.
//
// Guest memory is always shared: the GPU's backend is vhost-user, which
// reads the guest's memory itself, and what a guest costs the host is read
// from the memfd's allocated blocks. Shared memory takes huge pages only when
// the host allows it, which is what CheckHugePages exists to verify.
package ch

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

// reclaimMinAge is how long a guest's page goes untouched before DAMON
// reclaims it, handing it back to the host. Read again, a page comes from
// the host's own page cache: the guest's disks are files there.
const reclaimMinAge = 5 * time.Minute

// maxDiskQueues bounds a disk's queues. Each is a thread in the monitor with
// an io_uring and its own descriptors, half a dozen of them: past a few
// queues the guest gains little, and every disk of a large machine having one
// per vCPU costs hundreds of threads and thousands of descriptors.
const maxDiskQueues = 8

// QUEUE_SIZE is the depth of each of the GPU's two virtqueues, and has to
// match what the backend offers.
const QUEUE_SIZE = 256

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

	// MemoryMaxMB, above MemoryMB, lets the guest's memory be resized while
	// it runs, from BootMemoryMB(MemoryMB) up to MemoryMaxMB. Zero gives it
	// MemoryMB for as long as it runs.
	MemoryMaxMB int

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

	// Pmem are files given to the guest as virtio-pmem, read-only: the
	// guest's writes are discarded, and the host's page cache pages are
	// mapped copy-on-write, so every guest on a file shares them.
	Pmem []string

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

	// APISocket, when set, is where the running monitor is driven from
	// (API): suspended, and its memory resized.
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
		"--memory", c.memory(),

		// free-page-reporting is the whole point of the balloon: the guest
		// hands back pages as it frees them and the host takes them without
		// anyone deciding a target size.
		"--balloon", "size=0,free_page_reporting=on",

		// The guest says when its kernel panics, before it reboots, so the
		// host can tell a panic from a reboot.
		"--pvpanic",
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
			// A queue per vCPU, so each submits its own I/O without a lock
			// shared with the others, up to maxDiskQueues.
			spec += ",num_queues=" + strconv.Itoa(min(c.CPUs, maxDiskQueues))
			if d.ReadOnly {
				spec += ",readonly=on"
			}
			disk = append(disk, spec)
		}
		args = append(args, disk...)
	}

	for _, f := range c.Pmem {
		args = append(args, "--pmem", "file="+f+",discard_writes=on")
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

	args = append(args, "--cmdline", c.Cmdline())
	return args, nil
}

// BootMemoryMB is how much of a resizable machine of mb MiB is there from
// boot: a quarter, in whole 128 MiB, and at least 1 GiB. It can never be
// resized below it. The rest is virtio-mem, which the guest onlines as
// movable memory while it is at most three times what it booted with, so
// that it can be unplugged again; the kernel's own allocations, which cannot
// move, are left the quarter.
func BootMemoryMB(mb int) int {
	return min(max(mb/4/virtioMemAlignMB*virtioMemAlignMB, 1024), mb)
}

// virtioMemAlignMB is what Cloud Hypervisor's virtio-mem region is sized in,
// and virtioMemBlockMB what is plugged and unplugged in.
const (
	virtioMemAlignMB = 128
	virtioMemBlockMB = 2
)

// memory is the --memory argument: mb MiB, all of it there from boot, or
// with MemoryMaxMB, a boot size and the rest of mb plugged into a region
// that reaches MemoryMaxMB.
func (c *Config) memory() string {
	if c.MemoryMaxMB <= c.MemoryMB {
		return fmt.Sprintf("size=%dM,shared=on", c.MemoryMB)
	}
	boot := BootMemoryMB(c.MemoryMB)
	region := (c.MemoryMaxMB - boot + virtioMemAlignMB - 1) / virtioMemAlignMB * virtioMemAlignMB
	arg := fmt.Sprintf("size=%dM,shared=on,hotplug_method=virtio-mem,hotplug_size=%dM", boot, region)
	if plugged := plugged(c.MemoryMB-boot, region); plugged > 0 {
		arg += fmt.Sprintf(",hotplugged_size=%dM", plugged)
	}
	return arg
}

// plugged is mb in whole virtio-mem blocks, rounded up, and at most region.
func plugged(mb, region int) int {
	mb = (mb + virtioMemBlockMB - 1) / virtioMemBlockMB * virtioMemBlockMB
	return min(max(mb, 0), region)
}

// Cmdline is the guest kernel's command line.
func (c *Config) Cmdline() string {
	// No root= : the initramfs mounts the base read-only, the upper layer
	// read-write, stacks overlayfs and switch_roots into the result.
	cl := "console=" + c.ConsoleTTY
	cl += " systemd.show_status=1"
	// The VM is the boundary between tenants, and the host protects itself;
	// inside a guest, one owner's processes need not pay to be kept from
	// each other.
	cl += " mitigations=off"
	// Memory plugged in is onlined movable while the guest has three times
	// as much movable as it booted with, the kernel's own allocations
	// keeping the rest, and in the kernel's zone beyond: a guest grown far
	// past its boot size still has room for page tables and caches.
	cl += " memory_hotplug.online_policy=auto-movable"
	// Memory the guest frees goes back to the host by the balloon's free
	// page reporting, but its file cache is not free and would be kept for
	// as long as it runs. DAMON reclaims pages untouched for minAge,
	// whatever memory is free, so the cache of what was read once goes
	// back too. Anonymous memory is left alone: the guest has no swap. The
	// agent widens what it watches to the memory virtio-mem plugs in.
	cl += fmt.Sprintf(" damon_reclaim.enabled=Y damon_reclaim.min_age=%d damon_reclaim.skip_anon=Y"+
		" damon_reclaim.wmarks_high=1000 damon_reclaim.wmarks_mid=999 damon_reclaim.wmarks_low=0",
		reclaimMinAge.Microseconds())
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
