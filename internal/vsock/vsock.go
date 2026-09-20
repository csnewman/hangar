// Package vsock speaks AF_VSOCK, the virtio transport between a host and the
// guests running on it.
//
// vsock is the agent's channel because it is not a network: a guest reaches
// the host without an address, a route or a NIC, so an environment whose
// networking is broken, still booting, or deliberately locked down is still
// reachable. Nothing on the host's network can dial into a guest either,
// because the only path is the one QEMU created.
//
// It also has the right shape. A vsock endpoint is a socket, so an
// environment can carry many independent connections at once -- a control
// channel, a code-server session, a forwarded port -- each as its own stream
// on its own port. A single-stream transport would mean multiplexing those by
// hand.
//
// The kernel calls are made directly. Go's standard library knows the address
// family but has no SockaddrVM and no net.Conn for it, and a single 16-byte
// struct is a smaller thing to own than a dependency.
package vsock

// Well-known context IDs from linux/vm_sockets.h.
const (
	// CIDHypervisor addresses the hypervisor itself. Unused here; listed so
	// the numbering below is not mysterious.
	CIDHypervisor uint32 = 0

	// CIDLocal is loopback within one system.
	CIDLocal uint32 = 1

	// CIDHost is what a guest dials to reach its host. Every guest uses this
	// same number: the guest does not need to know where it is running.
	CIDHost uint32 = 2

	// CIDAny binds a listener to every context. A host listens on this rather
	// than on a guest's CID, so one listener serves every environment.
	CIDAny uint32 = 0xFFFFFFFF

	// FirstGuestCID is the lowest CID assignable to a guest; 0-2 are reserved.
	FirstGuestCID uint32 = 3
)
