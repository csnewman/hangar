// Package host detects the capabilities of the machine Hangar is running on:
// which QEMU binary to use, which machine type, and which accelerator.
//
// This exists because the guest architecture follows the host: an arm64 node
// runs aarch64 guests and an x86-64 node runs
// x86-64 guests under KVM. The machine types differ too: QEMU's minimal
// "microvm" type is x86-only, so aarch64 uses "virt".
package host

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
)

// Accel is a QEMU accelerator.
type Accel string

const (
	AccelKVM  Accel = "kvm"
	AccelNone Accel = "tcg" // emulation; correct but far too slow for real use
)

// Caps describes what this host can do.
type Caps struct {
	OS         string // runtime.GOOS
	Arch       string // guest architecture, e.g. "aarch64", "x86_64"
	QEMUBin    string // absolute path to the qemu-system-* binary
	QEMUVer    string
	Accel      Accel
	Machine    string // "virt" or "microvm"/"q35"
	ConsoleTTY string // serial console device as the guest sees it

}

// Detect inspects the host and returns its capabilities. A non-nil error means
// Hangar cannot run VMs here at all; a Caps with Accel == AccelNone means it
// can, but only under emulation.
func Detect() (*Caps, error) {
	// Hangar hosts environments on Linux only.
	//
	// The agent channel is vsock, which is backed by /dev/vhost-vsock -- a
	// Linux kernel interface. QEMU built for any other host has no vsock
	// device at all, not even the vhost-user one, because vhost-user is itself
	// Linux-only. An environment without an agent is not an environment, so
	// there is nothing to gain by half-supporting another host.
	//
	// On macOS, run Hangar inside a Linux VM: see docs/dev.md.
	if runtime.GOOS != "linux" {
		return nil, fmt.Errorf("Hangar requires a Linux host; this is %s. "+
			"On macOS, run it inside a Linux VM with nested virtualisation", runtime.GOOS)
	}

	c := &Caps{OS: runtime.GOOS}

	switch runtime.GOARCH {
	case "arm64":
		c.Arch = "aarch64"
		// The "virt" machine is the only sensible aarch64 target. There is no
		// aarch64 equivalent of x86's microvm.
		c.Machine = "virt"
		c.ConsoleTTY = "ttyAMA0"
	case "amd64":
		c.Arch = "x86_64"
		// q35 rather than microvm: virtio-gpu needs PCI. See
		// docs/qemu-configuration.md section 1.
		c.Machine = "q35"
		c.ConsoleTTY = "ttyS0"
	default:
		return nil, fmt.Errorf("unsupported host architecture %q", runtime.GOARCH)
	}

	// Only Hangar's own QEMU is supported. A distribution build is not a
	// degraded version of it, it is a different thing: not linked against
	// virglrenderer, so no virtio-gpu-gl and no accelerated display at all,
	// and carrying several hundred device models a guest can never address.
	// Accepting one would produce environments that boot and are quietly
	// wrong, which is worse than refusing.
	bin, err := findQEMU(c.Arch)
	if err != nil {
		return nil, err
	}
	c.QEMUBin = bin

	if out, err := exec.Command(bin, "--version").Output(); err == nil {
		c.QEMUVer = strings.TrimSpace(strings.SplitN(string(out), "\n", 2)[0])
	}

	c.Accel = detectAccel()
	return c, nil
}

// qemuSearchPath is where Hangar's own QEMU lives: the build output during
// development, and the path the published OCI image installs to.
var qemuSearchPath = []string{
	filepath.Join("out", "qemu"),
	"/usr/local/bin",
}

// findQEMU locates Hangar's QEMU and refuses anything else.
//
// Being in the right directory is not the test -- the binary is checked for
// virtio-gpu-gl-pci, which only exists when QEMU was linked against
// virglrenderer. That is the property Hangar's build exists to provide, so it
// is also the cheapest way to tell the two apart.
func findQEMU(arch string) (string, error) {
	name := "qemu-system-" + arch
	var tried []string
	for _, dir := range qemuSearchPath {
		p := filepath.Join(dir, name)
		if _, err := os.Stat(p); err != nil {
			tried = append(tried, p)
			continue
		}
		abs, err := filepath.Abs(p)
		if err != nil {
			return "", err
		}
		out, err := exec.Command(abs, "-device", "help").CombinedOutput()
		if err != nil {
			return "", fmt.Errorf("%s would not list its devices: %w", abs, err)
		}
		if !strings.Contains(string(out), "virtio-gpu-gl-pci") {
			return "", fmt.Errorf("%s has no virtio-gpu-gl-pci, so it was not built by "+
				"\"hangar qemu\" -- a distribution QEMU cannot accelerate a guest's "+
				"display and is not supported", abs)
		}
		return abs, nil
	}
	return "", fmt.Errorf("%s not found in %s: build it with \"hangar qemu\"",
		name, strings.Join(tried, " or "))
}

func detectAccel() Accel {
	if _, err := os.Stat("/dev/kvm"); err == nil {
		return AccelKVM
	}
	return AccelNone
}

// Summary renders the capabilities for `hangar doctor`.
func (c *Caps) Summary() string {
	var b strings.Builder
	fmt.Fprintf(&b, "host        %s/%s\n", c.OS, runtime.GOARCH)
	fmt.Fprintf(&b, "guest arch  %s\n", c.Arch)
	fmt.Fprintf(&b, "qemu        %s\n", c.QEMUBin)
	if c.QEMUVer != "" {
		fmt.Fprintf(&b, "            %s\n", c.QEMUVer)
	}
	fmt.Fprintf(&b, "machine     %s\n", c.Machine)
	fmt.Fprintf(&b, "accel       %s", c.Accel)
	if c.Accel == AccelNone {
		b.WriteString("   (emulation only — expect it to be very slow)")
	}
	b.WriteString("\n")
	return b.String()
}
