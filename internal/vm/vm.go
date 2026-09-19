// Package vm builds QEMU command lines and supervises the resulting process.
//
// The shape here deliberately matches docs/qemu-configuration.md: direct
// kernel boot, virtio-blk for writable storage, a serial console on stdio.
// Features that document defers (virtio-gpu, virtio-mem, passt, vsock) are
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

// Config describes one environment VM.
type Config struct {
	Name     string
	Kernel   string // host path to the guest kernel
	Initrd   string // optional
	Disks    []Disk // first disk becomes /dev/vda
	MemoryMB int
	CPUs     int
	// Network enables QEMU's built-in user-mode networking, which gives the
	// guest outbound NAT with no host privileges and no TAP device. This is a
	// stand-in for passt (docs/qemu-configuration.md section 5), which is the
	// same shape -- an unprivileged userspace network backend -- but faster
	// and production-grade.
	Network bool

	// ConsoleFile, when set, writes the guest serial console to this file
	// instead of attaching it to stdio. Used for unattended runs.
	ConsoleFile string

	// ExtraCmdline is appended to the generated kernel command line.
	ExtraCmdline string
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

	if c.Network {
		args = append(args,
			"-netdev", "user,id=net0",
			"-device", "virtio-net-pci,netdev=net0",
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
