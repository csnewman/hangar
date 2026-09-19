// Package host detects the capabilities of the machine Hangar is running on:
// which QEMU binary to use, which machine type, and which accelerator.
//
// This exists because the guest architecture follows the host. On an Apple
// Silicon Mac we run aarch64 guests under HVF; on a Linux x86-64 node we run
// x86-64 guests under KVM. The machine types differ too: QEMU's minimal
// "microvm" type is x86-only, so aarch64 uses "virt".
package host

import (
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"strings"
)

// Accel is a QEMU accelerator.
type Accel string

const (
	AccelKVM  Accel = "kvm"
	AccelHVF  Accel = "hvf"
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

	bin := "qemu-system-" + c.Arch
	path, err := exec.LookPath(bin)
	if err != nil {
		return nil, fmt.Errorf("%s not found in PATH: install QEMU (macOS: brew install qemu)", bin)
	}
	c.QEMUBin = path

	if out, err := exec.Command(path, "--version").Output(); err == nil {
		c.QEMUVer = strings.TrimSpace(strings.SplitN(string(out), "\n", 2)[0])
	}

	c.Accel = detectAccel()
	return c, nil
}

func detectAccel() Accel {
	switch runtime.GOOS {
	case "darwin":
		// Hypervisor.framework is present on every supported macOS. QEMU will
		// fail loudly at launch if the binary lacks the entitlement.
		return AccelHVF
	case "linux":
		if _, err := os.Stat("/dev/kvm"); err == nil {
			return AccelKVM
		}
		return AccelNone
	default:
		return AccelNone
	}
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
