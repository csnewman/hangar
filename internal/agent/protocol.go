// Package agent defines the control channel between the host and an
// environment, and the host end of it.
//
// What the channel proves, and what it does not:
//
// The context ID reported on accept is written by the host kernel's vhost-vsock
// driver from the guest-cid QEMU was given, so a guest can neither choose nor
// forge it, and vsock offers no guest-to-guest path -- a guest can reach only
// the host. One environment therefore cannot impersonate another, as long as
// the host resolves identity through its own CID-to-environment records and
// treats everything in Hello as advisory.
//
// It does not prove which process is talking. Connecting on AF_VSOCK needs no
// privilege, so any code in the environment can open this port and speak the
// protocol; the channel authenticates the VM, not the agent. A secret handed
// to the guest would not change that, because the workload can read whatever
// the agent can. The VM is the boundary, so this channel must never carry
// authority that the environment's own workload should not already have.
//
// The guest dials out. When hangar-agent starts it connects to the host over
// vsock and announces itself; the host never dials in. That direction is the
// point: the host needs no address for a guest, nothing waits for the guest's
// network, and an environment that is wedged, still booting or deliberately
// cut off from the network is still reachable.
//
// vsock rather than a serial port, because an environment needs more than one
// stream. A control channel, a code-server session and a forwarded port are
// separate connections on separate ports, which vsock provides directly; a
// single-stream transport would mean multiplexing them by hand.
//
// The protocol is newline-delimited JSON in both directions. Every request
// carries an ID and every response repeats it, so replies may be returned out
// of order.
package agent

// Port is the vsock port the host listens on and the agent dials.
//
// vsock ports are their own space, shared with nothing, so this only has to
// avoid other Hangar services.
const Port uint32 = 8100

// ProtocolVersion is sent in Hello and checked by the host. The agent is
// installed into the image while the host binary comes from the node, so the
// two can be built at different times and must be able to say so.
const ProtocolVersion = 1

// Message kinds.
const (
	KindHello = "hello"
	KindPing  = "ping"
	KindExec  = "exec"
	KindInfo  = "info"
)

// Hello is the first line the agent sends. It is unsolicited and has no ID,
// because nothing requested it.
//
// Every field here is ADVISORY. It is whatever the guest chose to say about
// itself, and a compromised environment can put anything in it. Identity comes
// from the context ID the kernel reports on accept, which the guest cannot
// forge; nothing may key an environment record off these fields.
type Hello struct {
	Kind     string `json:"kind"`
	Version  int    `json:"version"`
	Hostname string `json:"hostname"`
	Kernel   string `json:"kernel"`
	// BootMicros is how long the guest took to reach the agent, measured from
	// the kernel's own clock, so it excludes QEMU and host process startup.
	BootMicros int64 `json:"boot_micros"`
}

// Request is a host-to-agent message.
type Request struct {
	ID   uint64   `json:"id"`
	Kind string   `json:"kind"`
	Cmd  []string `json:"cmd,omitempty"`
}

// Response is an agent-to-host reply. Err is set when the request could not be
// carried out at all; a command that ran and failed reports its status in Code
// and leaves Err empty.
type Response struct {
	ID     uint64 `json:"id"`
	Err    string `json:"err,omitempty"`
	Stdout string `json:"stdout,omitempty"`
	Stderr string `json:"stderr,omitempty"`
	Code   int    `json:"code"`
	Info   string `json:"info,omitempty"`
}
