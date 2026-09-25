// Package procs is an environment's task manager: what runs inside the
// guest, what each process costs, and stopping one.
//
// The guest agent serves it on Port. A connection carries requests and
// replies as JSON lines, one reply per request. CPU use is measured between
// one listing and the next, so the first listing a Server gives reports
// none; a listing refreshed every few seconds reports the share each
// process had since the one before.
package procs

import (
	"bufio"
	"encoding/json"
	"io"
	"sync"
	"time"
)

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

// Server answers requests about the processes of the machine it runs on.
type Server struct {
	mu   sync.Mutex
	prev map[int]sample
	at   time.Time
}

type sample struct {
	ticks uint64
	start uint64
}

func NewServer() *Server { return &Server{prev: map[int]sample{}} }

// Serve answers requests on conn until it closes.
func (s *Server) Serve(conn io.ReadWriteCloser) {
	defer conn.Close()
	r := bufio.NewReader(conn)
	for {
		line, err := r.ReadBytes('\n')
		if err != nil {
			return
		}
		var req Request
		var reply Reply
		if err := json.Unmarshal(line, &req); err != nil {
			reply.Error = "a malformed request"
		} else {
			reply = s.handle(req)
		}
		b, _ := json.Marshal(reply)
		if _, err := conn.Write(append(b, '\n')); err != nil {
			return
		}
	}
}

func (s *Server) handle(req Request) Reply {
	switch req.Op {
	case "list":
		ps, cpus, mem, err := s.list()
		if err != nil {
			return Reply{Error: err.Error()}
		}
		return Reply{Processes: ps, CPUs: cpus, MemoryBytes: mem}
	case "kill":
		if err := kill(req.PID, req.Signal); err != nil {
			return Reply{Error: err.Error()}
		}
		return Reply{}
	}
	return Reply{Error: "unknown request " + req.Op}
}
