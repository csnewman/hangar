package worker

import (
	"context"
	"fmt"
	"log/slog"
	"net"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/sshd"
)

// DialSSH serves SSH for a running environment. There is no guest, so the
// shells run on this machine, as its dev user if it has one.
func (s *Simulated) DialSSH(_ context.Context, id string) (net.Conn, error) {
	s.mu.Lock()
	e, ok := s.envs[id]
	running := ok && e.phase == api.PhaseRunning
	s.mu.Unlock()
	if !running {
		return nil, fmt.Errorf("environment %s is not running here", id)
	}
	srv, err := sshd.NewServer("dev", slog.Default())
	if err != nil {
		return nil, err
	}
	a, b, err := sshd.Pipe()
	if err != nil {
		return nil, err
	}
	go srv.Serve(b)
	return a, nil
}
