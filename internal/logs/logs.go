// Package logs is how the control plane asks a worker for its logs: the
// files an environment's machine writes -- its console, its monitor's, its
// backends' -- the worker's own log, which it keeps in memory, whole and
// per environment, and each environment's combined log. The worker's half
// is crates/worker's logs.
//
// Every log reads as a stream of bytes that only grows, so a reader asks
// for what follows the offset it has read to. The oldest of a long log may
// be gone, from a file's start or a ring's; Start in a reply says where
// what it holds begins.
package logs

import (
	"bufio"
	"encoding/json"
	"errors"
	"io"
)

// The logs an environment has.
const (
	// Combined is all of an environment's other logs as one: each line as
	// the worker sees it written, marked with the log it is from.
	Combined = "combined"
	// Console is the guest's serial console: the kernel's messages, the
	// agent as init assembling the root, and systemd.
	Console = "console"
	// Monitor is Cloud Hypervisor's own log.
	Monitor = "monitor"
	// GPU is the virtual GPU's backend.
	GPU = "gpu"
	// Worker is what the worker logged about the environment, or, for the
	// worker as a whole, everything it logged.
	Worker = "worker"
)

// Names are an environment's logs, in the order they are offered.
var Names = []string{Combined, Console, Worker, Monitor, GPU}

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

// ErrNoReply is a stream closed without a reply, as by a worker that
// serves no logs.
var ErrNoReply = errors.New("the worker did not answer")

// Ask sends a request down a stream to a worker and reads its reply: one
// line of JSON each way. A reply saying why it could not be read is an
// error.
func Ask(stream io.ReadWriter, req Request) (Reply, error) {
	b, err := json.Marshal(req)
	if err != nil {
		return Reply{}, err
	}
	if _, err := stream.Write(append(b, '\n')); err != nil {
		return Reply{}, err
	}
	line, err := bufio.NewReader(stream).ReadBytes('\n')
	if err != nil {
		return Reply{}, ErrNoReply
	}
	var reply Reply
	if err := json.Unmarshal(line, &reply); err != nil {
		return Reply{}, err
	}
	if reply.Error != "" {
		return Reply{}, errors.New(reply.Error)
	}
	return reply, nil
}
