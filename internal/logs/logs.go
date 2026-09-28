// Package logs reads a worker's logs for the control plane: the files an
// environment's machine writes -- its console, its monitor's, its backends'
// -- and the worker's own log, kept in memory, whole and per environment.
//
// Every log reads as a stream of bytes that only grows, so a reader asks
// for what follows the offset it has read to. The oldest of a long log may
// be gone, from a file's start or a ring's; Start in a reply says where
// what it holds begins.
package logs

import (
	"errors"
	"io"
	"os"
)

// The logs an environment has.
const (
	// Console is the guest's serial console: the kernel's messages, the
	// agent as init assembling the root, and systemd.
	Console = "console"
	// Monitor is Cloud Hypervisor's own log.
	Monitor = "monitor"
	// Files is the backend serving the image over virtio-fs.
	Files = "fs"
	// GPU is the virtual GPU's backend.
	GPU = "gpu"
	// Worker is what the worker logged about the environment, or, for the
	// worker as a whole, everything it logged.
	Worker = "worker"
)

// Names are an environment's logs, in the order they are offered.
var Names = []string{Console, Worker, Monitor, Files, GPU}

// MaxRead is the most a reply holds.
const MaxRead = 256 << 10

// Request asks for one log. An environment of "" asks for the worker's
// whole log. A negative Offset asks for the end: the last Max bytes.
type Request struct {
	Environment string `json:"environment"`
	Log         string `json:"log"`
	Offset      int64  `json:"offset"`
	Max         int    `json:"max"`
}

// Reply is part of a log: Data, from Start, of a log Size bytes long so far.
// A reader continues from Start+len(Data).
type Reply struct {
	Start int64  `json:"start"`
	Size  int64  `json:"size"`
	Data  []byte `json:"data"`
	// Missing is that there is no such log yet: the machine has not been
	// started here, or has no GPU.
	Missing bool   `json:"missing,omitempty"`
	Error   string `json:"error,omitempty"`
}

// clamp is the window a request reads of a log size bytes long, of which
// the oldest kept begins at first.
func clamp(req Request, first, size int64) (start, n int64) {
	limit := int64(req.Max)
	if limit <= 0 || limit > MaxRead {
		limit = MaxRead
	}
	start = req.Offset
	if start < 0 || start > size {
		start = size - limit
	}
	start = min(max(start, first), size)
	return start, min(limit, size-start)
}

// ReadFile reads part of a log file.
func ReadFile(path string, req Request) (Reply, error) {
	f, err := os.Open(path)
	if errors.Is(err, os.ErrNotExist) {
		return Reply{Missing: true}, nil
	}
	if err != nil {
		return Reply{}, err
	}
	defer f.Close()
	st, err := f.Stat()
	if err != nil {
		return Reply{}, err
	}
	start, n := clamp(req, 0, st.Size())
	data := make([]byte, n)
	got, err := f.ReadAt(data, start)
	if err != nil && !errors.Is(err, io.EOF) {
		return Reply{}, err
	}
	return Reply{Start: start, Size: st.Size(), Data: data[:got]}, nil
}
