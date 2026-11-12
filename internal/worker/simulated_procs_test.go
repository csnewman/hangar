package worker_test

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/procs"
)

// DialProcesses lists a running environment's processes. There is no guest,
// so the list is the worker's own process, which cannot be stopped.
func (s *Simulated) DialProcesses(_ context.Context, id string) (net.Conn, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e, ok := s.envs[id]
	if !ok || e.phase != api.PhaseRunning {
		return nil, fmt.Errorf("environment %s is not running here", id)
	}
	a, b := net.Pipe()
	go func() {
		defer b.Close()
		r := bufio.NewReader(b)
		for {
			line, err := r.ReadBytes('\n')
			if err != nil {
				return
			}
			var req procs.Request
			reply := procs.Reply{Error: "that process keeps the environment running, and cannot be stopped from here"}
			if json.Unmarshal(line, &req) == nil && req.Op == "list" {
				reply = procs.Reply{CPUs: 1, MemoryBytes: 1 << 30, Processes: []procs.Process{{
					PID: os.Getpid(), Name: "worker", Command: "worker", State: "S",
					Started: time.Now(), Protected: true,
				}}}
			}
			out, _ := json.Marshal(reply)
			if _, err := b.Write(append(out, '\n')); err != nil {
				return
			}
		}
	}()
	return a, nil
}
