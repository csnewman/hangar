package worker

import (
	"context"
	"fmt"
	"net"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/procs"
)

// DialProcesses lists a running environment's processes. There is no guest,
// so they are this machine's own -- the worker's container's, in the
// development stack.
func (s *Simulated) DialProcesses(_ context.Context, id string) (net.Conn, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	e, ok := s.envs[id]
	if !ok || e.phase != api.PhaseRunning {
		return nil, fmt.Errorf("environment %s is not running here", id)
	}
	if s.procs == nil {
		s.procs = procs.NewServer()
	}
	a, b := net.Pipe()
	go s.procs.Serve(b)
	return a, nil
}
