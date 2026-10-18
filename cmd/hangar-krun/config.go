// Command hangar-krun runs one environment's machine under libkrun, the
// monitor on macOS, where Cloud Hypervisor cannot run. The worker starts
// it with a JSON description of the machine, as it starts Cloud Hypervisor
// on Linux; libkrun then takes the process over, and it exits when the
// guest does.
//
// libkrun is built from source with dev/krun-build.sh, which also builds
// this command against it and signs it with the hypervisor entitlement
// macOS requires.
package main

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

type Disk struct {
	ID       string `json:"id"`
	Path     string `json:"path"`
	ReadOnly bool   `json:"read_only,omitempty"`
}

type Net struct {
	Socket string `json:"socket"`
	MAC    string `json:"mac"`
}
