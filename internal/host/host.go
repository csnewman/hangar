// Package host detects the capabilities of the machine Hangar is running on.
//
// This exists because the guest architecture follows the host: an arm64 node
// runs aarch64 guests and an x86-64 node runs x86-64 guests. The serial
// console the guest sees follows from that too.
package host

import (
	"fmt"
	"os"
	"runtime"
	"strings"
)

// Caps describes what this host can do.
type Caps struct {
	OS         string // runtime.GOOS
	Arch       string // guest architecture, e.g. "aarch64", "x86_64"
	ConsoleTTY string // serial console device as the guest sees it
}

// Detect inspects the host and returns its capabilities. A non-nil error means
// Hangar cannot run VMs here.
func Detect() (*Caps, error) {
	// Hangar hosts environments on Linux only.
	//
	// The monitor is Cloud Hypervisor, which runs on KVM and nothing else,
	// and the filesystem and network reach the guest over vhost-user, which
	// is a Linux interface. An environment without those is not an
	// environment, so there is nothing to gain by half-supporting another
	// host.
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
		c.ConsoleTTY = "ttyAMA0"
	case "amd64":
		c.Arch = "x86_64"
		c.ConsoleTTY = "ttyS0"
	default:
		return nil, fmt.Errorf("unsupported host architecture %q", runtime.GOARCH)
	}

	// Cloud Hypervisor has no emulation mode: without KVM there is no way to
	// run a guest at all, fast or slow. So this is a refusal rather than a
	// warning about performance.
	if _, err := os.Stat("/dev/kvm"); err != nil {
		return nil, fmt.Errorf("no /dev/kvm: Cloud Hypervisor runs guests on KVM only, "+
			"and there is no emulation fallback (%w)", err)
	}

	return c, nil
}

// Summary renders the capabilities for `hangar doctor`.
func (c *Caps) Summary() string {
	var b strings.Builder
	fmt.Fprintf(&b, "host        %s/%s\n", c.OS, runtime.GOARCH)
	fmt.Fprintf(&b, "guest arch  %s\n", c.Arch)
	fmt.Fprintf(&b, "console     %s\n", c.ConsoleTTY)
	fmt.Fprintf(&b, "kvm         yes\n")
	return b.String()
}
