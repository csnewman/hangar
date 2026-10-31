// Package host detects the capabilities of the machine Hangar is running on.
//
// This exists because the guest architecture follows the host: an arm64 node
// runs aarch64 guests and an x86-64 node runs x86-64 guests. The monitor and
// the serial console the guest sees follow from the host too.
package host

import (
	"fmt"
	"os"
	"runtime"
	"strings"

	"github.com/csnewman/hangar/internal/api"
)

// The monitors environments run under.
const (
	// MonitorCloudHypervisor runs guests on Linux's KVM.
	MonitorCloudHypervisor = api.MonitorCloudHypervisor
	// MonitorKrun runs guests through libkrun (hangar-krun): on macOS's
	// Hypervisor.framework, and on Linux's KVM beside Cloud Hypervisor.
	MonitorKrun = api.MonitorKrun
)

// Caps describes what this host can do.
type Caps struct {
	OS         string // runtime.GOOS
	Arch       string // guest architecture, e.g. "aarch64", "x86_64"
	ConsoleTTY string // serial console device as the guest sees it
	Monitor    string // MonitorCloudHypervisor or MonitorKrun
}

// Detect inspects the host and returns its capabilities under monitor, or
// the host's own when monitor is empty: Cloud Hypervisor on Linux, libkrun
// on macOS. A non-nil error means Hangar cannot run VMs here under it.
func Detect(monitor string) (*Caps, error) {
	c := &Caps{OS: runtime.GOOS}
	switch runtime.GOARCH {
	case "arm64":
		c.Arch = "aarch64"
	case "amd64":
		c.Arch = "x86_64"
	default:
		return nil, fmt.Errorf("unsupported host architecture %q", runtime.GOARCH)
	}

	switch runtime.GOOS {
	case "linux":
		switch monitor {
		case "", MonitorCloudHypervisor:
			c.Monitor = MonitorCloudHypervisor
			c.ConsoleTTY = "ttyAMA0"
			if c.Arch == "x86_64" {
				c.ConsoleTTY = "ttyS0"
			}
		case MonitorKrun:
			// libkrun's console is a virtio one.
			c.Monitor = MonitorKrun
			c.ConsoleTTY = "hvc0"
		default:
			return nil, fmt.Errorf("unknown monitor %q", monitor)
		}
		// Neither monitor has an emulation mode: without KVM there is no
		// way to run a guest at all, fast or slow. So this is a refusal
		// rather than a warning about performance.
		if _, err := os.Stat("/dev/kvm"); err != nil {
			return nil, fmt.Errorf("no /dev/kvm: %s runs guests on KVM only, "+
				"and there is no emulation fallback (%w)", c.Monitor, err)
		}
	case "darwin":
		// libkrun runs guests on Apple silicon alone, its console a virtio
		// one.
		if c.Arch != "aarch64" {
			return nil, fmt.Errorf("environments run on Apple silicon Macs, not %s", runtime.GOARCH)
		}
		if monitor != "" && monitor != MonitorKrun {
			return nil, fmt.Errorf("a Mac runs environments under %s, not %s", MonitorKrun, monitor)
		}
		c.Monitor = MonitorKrun
		c.ConsoleTTY = "hvc0"
		if !hypervisorFramework() {
			return nil, fmt.Errorf("this Mac does not offer Hypervisor.framework (kern.hv_support)")
		}
	default:
		return nil, fmt.Errorf("Hangar runs environments on Linux and macOS, not %s", runtime.GOOS)
	}
	return c, nil
}

// Summary renders the capabilities for `hangar doctor`.
func (c *Caps) Summary() string {
	var b strings.Builder
	fmt.Fprintf(&b, "host        %s/%s\n", c.OS, runtime.GOARCH)
	fmt.Fprintf(&b, "guest arch  %s\n", c.Arch)
	fmt.Fprintf(&b, "console     %s\n", c.ConsoleTTY)
	fmt.Fprintf(&b, "monitor     %s\n", c.Monitor)
	return b.String()
}
