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
	"context"
	"encoding/json"
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
	// GPU, if set, gives the guest a virtio-gpu: Vulkan through Venus,
	// rendered by the Mac's GPU, and no OpenGL of its own.
	GPU *GPU `json:"gpu,omitempty"`
}

// GPU is the guest's virtual GPU.
type GPU struct {
	// WindowMiB sizes the window the guest maps GPU memory through.
	WindowMiB int `json:"window_mib"`
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

// Features are what hangar-krun's libkrun was built with, and what it
// renders with.
type Features struct {
	GPU bool `json:"gpu"`
	// Vulkan is the Vulkan driver a guest's GPU renders with on the Mac:
	// VulkanKosmicKrisp or VulkanMoltenVK; empty without a GPU.
	Vulkan string `json:"vulkan,omitempty"`
	// Zink is whether a guest's OpenGL can be drawn on the GPU, by Mesa's
	// OpenGL on Vulkan (zink): the driver can share images between the
	// guest's programs, which dev/krun-build.sh's KosmicKrisp can.
	Zink bool `json:"zink,omitempty"`
}

// The Vulkan drivers hangar-krun renders a guest's GPU with.
const (
	// VulkanKosmicKrisp is LunarG's driver in Mesa, on Metal 4 (macOS 26
	// and later): conformant Vulkan 1.4, robustness2 among it.
	VulkanKosmicKrisp = "kosmickrisp"
	// VulkanMoltenVK is Khronos's, on any Metal.
	VulkanMoltenVK = "moltenvk"
)

// ReadFeatures asks hangar-krun what its libkrun was built with.
func ReadFeatures(ctx context.Context) (Features, error) {
	var f Features
	bin, err := Find()
	if err != nil {
		return f, err
	}
	out, err := exec.CommandContext(ctx, bin, "-features").Output()
	if err != nil {
		return f, fmt.Errorf("asking hangar-krun what it offers: %w", err)
	}
	return f, json.Unmarshal(out, &f)
}

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
