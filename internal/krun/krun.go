// Package krun describes and finds hangar-krun, which runs one
// environment's machine under libkrun: the monitor on macOS, where Cloud
// Hypervisor cannot run. The worker starts it with a Config as JSON, as it
// starts Cloud Hypervisor on Linux; libkrun then takes the process over,
// and it exits when the guest does.
//
// libkrun is built from source with dev/krun-build.sh, which also builds
// hangar-krun against it and signs it with the hypervisor entitlement macOS
// requires.
package krun

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
)

// Config is the machine hangar-krun runs.
type Config struct {
	CPUs      int    `json:"cpus"`
	MemoryMiB int    `json:"memory_mib"`
	Kernel    string `json:"kernel"`
	Initrd    string `json:"initrd"`
	Cmdline   string `json:"cmdline"`
	// Console is the file the guest's console is written to.
	Console string `json:"console"`
	// Disks are attached in order, as /dev/vda, /dev/vdb and on.
	Disks []Disk `json:"disks"`
	// HostPorts are the vsock ports the guest connects out on, each to the
	// unix socket the host listens on.
	HostPorts map[uint32]string `json:"host_ports,omitempty"`
	// GuestPorts are the vsock ports the guest listens on, each reached by
	// connecting to the unix socket libkrun listens on.
	GuestPorts map[uint32]string `json:"guest_ports,omitempty"`
	// Net, if set, is a unix datagram socket carrying the guest's network
	// (gvproxy in vfkit mode), with the guest's MAC address.
	Net *Net `json:"net,omitempty"`
}

// Disk is a virtio-blk device.
type Disk struct {
	ID       string `json:"id"`
	Path     string `json:"path"`
	ReadOnly bool   `json:"read_only,omitempty"`
}

// Net is the guest's network device.
type Net struct {
	Socket string `json:"socket"`
	MAC    string `json:"mac"`
}

// searchPath is where hangar-krun is found beside PATH: what
// dev/krun-build.sh builds, from the repository's root.
var searchPath = []string{filepath.Join("out", "krun", "bin", "hangar-krun")}

// Tool locates a program built beside hangar-krun for a worker on macOS,
// such as mkfs.ext4.
func Tool(name string) (string, error) {
	bin, err := Find()
	if err != nil {
		return "", err
	}
	p := filepath.Join(filepath.Dir(bin), name)
	if _, err := os.Stat(p); err != nil {
		return "", fmt.Errorf("%s is not beside hangar-krun: build it with dev/krun-build.sh", name)
	}
	return p, nil
}

// Find locates hangar-krun.
func Find() (string, error) {
	if p, err := exec.LookPath("hangar-krun"); err == nil {
		return p, nil
	}
	for _, p := range searchPath {
		if _, err := os.Stat(p); err == nil {
			return filepath.Abs(p)
		}
	}
	return "", fmt.Errorf("hangar-krun not found on PATH or in %v: build it with dev/krun-build.sh", searchPath)
}
