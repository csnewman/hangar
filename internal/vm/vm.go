// Package vm builds QEMU command lines and supervises the resulting process.
//
// The shape here deliberately matches docs/qemu-configuration.md: direct
// kernel boot, virtio-blk for writable storage, a serial console on stdio.
// Features that document defers (virtio-gpu, virtio-mem) are
// absent rather than half-wired.
package vm

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"os/signal"
	"strconv"
	"syscall"

	"github.com/csnewman/hangar/internal/host"
)

// Disk is a block device attached to the guest.
type Disk struct {
	Path     string
	ReadOnly bool
}

const (
	// sharedMemID names the shared memory object vhost-user backends map.
	sharedMemID = "hangar-mem"
	// virtiofsChardevID names the socket to virtiofsd.
	virtiofsChardevID = "hangar-virtiofs"
)

// Config describes one environment VM.
type Config struct {
	Name     string
	Kernel   string // host path to the guest kernel
	Initrd   string // optional
	Disks    []Disk // first disk becomes /dev/vda
	MemoryMB int
	CPUs     int
	// Network gives the guest outbound connectivity through passt, which
	// needs no host privileges and no TAP device. See
	// docs/qemu-configuration.md section 5.
	Network bool

	// ConsoleFile, when set, writes the guest serial console to this file
	// instead of attaching it to stdio. Used for unattended runs.
	ConsoleFile string

	// ExtraCmdline is appended to the generated kernel command line.
	ExtraCmdline string

	// GuestCID gives the guest a vsock context ID, which is how hangar-agent
	// reaches the host. Zero leaves the agent channel out.
	//
	// It must be unique among the guests running on this host.
	GuestCID uint32

	// VirtiofsSocket is a virtiofsd vhost-user socket. The directory it
	// exports appears in the guest as a virtiofs filesystem tagged
	// VirtiofsTag. Empty leaves it out.
	//
	// This is how the read-only base layer is meant to reach a guest: the host
	// already holds it as an unpacked directory, so nothing is converted to a
	// disk image, and the page cache backing it is shared by every environment
	// running the same base.
	VirtiofsSocket string
	// VirtiofsTag is the mount tag the guest uses. Defaults to "hangar-base".
	VirtiofsTag string
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
}

// Args builds the QEMU argument list for the given host.
func (c *Config) Args(h *host.Caps) ([]string, error) {
	c.applyDefaults()

	if c.Kernel == "" {
		return nil, fmt.Errorf("kernel path is required")
	}
	if len(c.Disks) == 0 {
		return nil, fmt.Errorf("at least one disk is required")
	}

	machine := h.Machine
	if h.Accel != host.AccelNone {
		machine += ",accel=" + string(h.Accel)
	}
	// vhost-user backends read and write the guest's memory directly, which
	// they can only do if it is shared rather than private to QEMU. The
	// backend has to be named on -machine itself, so it is appended here
	// rather than passed as a separate option -- a second -machine would
	// replace this one instead of adding to it.
	if c.VirtiofsSocket != "" {
		machine += ",memory-backend=" + sharedMemID
	}

	cpu := "host"
	if h.Accel == host.AccelNone {
		// "host" is meaningless without an accelerator.
		cpu = "max"
	}

	args := []string{
		"-name", c.Name,
		"-machine", machine,
		"-cpu", cpu,
		"-smp", strconv.Itoa(c.CPUs),
		"-m", strconv.Itoa(c.MemoryMB),

		// No default devices: we want to know exactly what the guest sees.
		"-nodefaults",
		"-no-user-config",

		// -nographic would also disable the monitor in ways that complicate
		// later work, so be explicit.
		"-display", "none",

		// Entropy, so the guest does not block on first boot.
		"-device", "virtio-rng-pci",

		// Paravirtual clock source and a shutdown path, so `poweroff` in the
		// guest actually stops the process.
		"-device", "virtio-balloon-pci",

		"-kernel", c.Kernel,
	}

	if c.ConsoleFile != "" {
		args = append(args, "-serial", "file:"+c.ConsoleFile)
	} else {
		args = append(args, "-serial", "mon:stdio")
	}

	if c.Initrd != "" {
		args = append(args, "-initrd", c.Initrd)
	}

	for i, d := range c.Disks {
		node := fmt.Sprintf("disk%d", i)
		blockdev := fmt.Sprintf(
			"driver=raw,node-name=%s,file.driver=file,file.filename=%s,cache.direct=on,cache.no-flush=off",
			node, d.Path,
		)
		if d.ReadOnly {
			blockdev += ",read-only=on"
		}
		args = append(args,
			"-blockdev", blockdev,
			"-device", fmt.Sprintf("virtio-blk-pci,drive=%s,serial=%s", node, node),
		)
	}

	if c.VirtiofsSocket != "" {
		tag := c.VirtiofsTag
		if tag == "" {
			tag = "hangar-base"
		}
		args = append(args,
			"-object", fmt.Sprintf("memory-backend-memfd,id=%s,size=%dM,share=on",
				sharedMemID, c.MemoryMB),
			"-chardev", fmt.Sprintf("socket,id=%s,path=%s", virtiofsChardevID, c.VirtiofsSocket),
			"-device", fmt.Sprintf("vhost-user-fs-pci,chardev=%s,tag=%s", virtiofsChardevID, tag),
		)
	}

	if c.GuestCID != 0 {
		// The guest dials the host; nothing dials in. The device only has to
		// exist and carry a CID unique among the guests on this host.
		args = append(args,
			"-device", fmt.Sprintf("vhost-vsock-pci,guest-cid=%d", c.GuestCID),
		)
	}

	if c.Network {
		// passt rather than QEMU's built-in user-mode networking: both are
		// unprivileged userspace backends needing no TAP and no NET_ADMIN, but
		// passt translates to ordinary host sockets rather than carrying its
		// own TCP stack, and is the one this design settled on.
		//
		// QEMU starts passt itself and connects to it, so there is no socket
		// path to manage here.
		//
		// romfile= disables the iPXE option ROM. A guest is direct-booted from
		// -kernel and never boots over the network, so the ROM is dead weight,
		// and it ships in a separate package (ipxe-qemu) that QEMU refuses to
		// start without once the device asks for it.
		args = append(args,
			"-netdev", "passt,id=net0",
			"-device", "virtio-net-pci,netdev=net0,romfile=",
		)
	}

	args = append(args, "-append", c.cmdline(h))
	return args, nil
}

func (c *Config) cmdline(h *host.Caps) string {
	// No root= : the initramfs mounts /dev/vda read-only, /dev/vdb read-write,
	// stacks overlayfs and switch_roots into it.
	cl := fmt.Sprintf("console=%s", h.ConsoleTTY)
	cl += " systemd.show_status=1"
	if c.ExtraCmdline != "" {
		cl += " " + c.ExtraCmdline
	}
	return cl
}

// Run launches the VM and blocks until it exits. Stdio is connected to the
// guest serial console, so the caller's terminal becomes the guest console.
func Run(ctx context.Context, h *host.Caps, cfg *Config) error {
	args, err := cfg.Args(h)
	if err != nil {
		return err
	}

	// Ctrl-C should shut the VM down rather than kill us and orphan it.
	ctx, stop := signal.NotifyContext(ctx, os.Interrupt, syscall.SIGTERM)
	defer stop()

	cmd := exec.CommandContext(ctx, h.QEMUBin, args...)
	cmd.Stdin = os.Stdin
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Cancel = func() error { return cmd.Process.Signal(syscall.SIGTERM) }

	if err := cmd.Run(); err != nil {
		if ctx.Err() != nil {
			return nil // we asked it to stop
		}
		return fmt.Errorf("qemu exited: %w", err)
	}
	return nil
}

// PrintCommand renders the command line for debugging, one argument per line
// so it can be copied and edited by hand.
func PrintCommand(h *host.Caps, cfg *Config) (string, error) {
	args, err := cfg.Args(h)
	if err != nil {
		return "", err
	}
	out := h.QEMUBin
	for i := 0; i < len(args); i++ {
		if len(args[i]) > 0 && args[i][0] == '-' && i+1 < len(args) && len(args[i+1]) > 0 && args[i+1][0] != '-' {
			out += fmt.Sprintf(" \\\n    %s %s", args[i], args[i+1])
			i++
			continue
		}
		out += fmt.Sprintf(" \\\n    %s", args[i])
	}
	return out, nil
}
