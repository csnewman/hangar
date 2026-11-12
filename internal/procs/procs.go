// Package procs is an environment's task manager: what runs inside the
// guest, what each process costs, and stopping one.
//
// The guest agent's Rust half serves it on Port (crates/agent/src/procs.rs). A connection carries requests and
// replies as JSON lines, one reply per request. CPU use is measured between
// one listing and the next, so the first listing a Server gives reports
// none; a listing refreshed every few seconds reports the share each
// process had since the one before.
package procs

import "time"

// Port is the vsock port the guest agent serves the process list on.
const Port uint32 = 8106

// Request is one request.
type Request struct {
	// Op is "list", or "kill" with PID and Signal.
	Op  string `json:"op"`
	PID int    `json:"pid,omitempty"`
	// Signal is TERM, the default, or KILL.
	Signal string `json:"signal,omitempty"`
}

// Process is one process.
type Process struct {
	PID     int    `json:"pid"`
	PPID    int    `json:"ppid"`
	User    string `json:"user"`
	Name    string `json:"name"`
	Command string `json:"command"`
	State   string `json:"state"`
	// CPUPercent is its share of one CPU since the last listing: a
	// process busy on two CPUs is at 200.
	CPUPercent float64   `json:"cpu_percent"`
	RSSBytes   int64     `json:"rss_bytes"`
	Threads    int       `json:"threads"`
	Started    time.Time `json:"started"`
	// Protected is a process that cannot be stopped from here: the guest's
	// init, and the agent answering.
	Protected bool `json:"protected,omitempty"`
}

// Reply answers one request.
type Reply struct {
	Processes []Process `json:"processes,omitempty"`
	// CPUs is how many CPUs the guest has, and MemoryBytes its memory.
	CPUs        int    `json:"cpus,omitempty"`
	MemoryBytes int64  `json:"memory_bytes,omitempty"`
	Error       string `json:"error,omitempty"`
}
