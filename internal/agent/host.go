package agent

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/vsock"
)

// Server listens for agents dialling out and hands each connection to the
// caller as a Session.
type Server struct {
	ln *vsock.Listener
}

// Listen starts accepting agent connections on Port.
//
// The listener binds CIDAny, so one server serves every environment on the
// node rather than one per guest.
func Listen() (*Server, error) {
	ln, err := vsock.Listen(vsock.CIDAny, Port)
	if err != nil {
		return nil, err
	}
	return &Server{ln: ln}, nil
}

// Close stops the server. A blocked Accept returns an error once it is closed.
func (s *Server) Close() error { return s.ln.Close() }

// Accept waits for an agent to connect and reads its Hello.
//
// The CID comes from the kernel, not from the agent, so it identifies which
// environment is calling even if the agent lies about its hostname.
func (s *Server) Accept(timeout time.Duration) (*Session, error) {
	f, cid, err := s.ln.Accept(timeout)
	if err != nil {
		return nil, err
	}
	sess := &Session{CID: cid, f: f, br: bufio.NewReader(f), next: 1}
	if err := sess.readHello(timeout); err != nil {
		f.Close()
		return nil, err
	}
	return sess, nil
}

// Session is a live connection to one environment's agent.
type Session struct {
	// CID is the guest's context ID, as reported by the kernel. This is the
	// only trustworthy identifier on the connection, and the only thing a
	// caller may resolve to an environment.
	//
	// A CID identifies whichever VM holds it now. They are recycled when
	// environments are destroyed, so a control plane must pair this with the
	// allocation it made and drop a session whose CID is no longer live.
	CID uint32
	// Hello is what the agent said about itself. Advisory; see its doc.
	Hello Hello

	f  *os.File
	br *bufio.Reader

	mu   sync.Mutex
	next uint64
}

func (s *Session) readHello(timeout time.Duration) error {
	if err := s.f.SetDeadline(time.Now().Add(timeout)); err != nil {
		return err
	}
	defer s.f.SetDeadline(time.Time{})

	line, err := s.br.ReadBytes('\n')
	if err != nil {
		return fmt.Errorf("reading hello: %w", err)
	}
	if err := json.Unmarshal(line, &s.Hello); err != nil {
		return fmt.Errorf("malformed hello: %w", err)
	}
	if s.Hello.Kind != KindHello {
		return fmt.Errorf("first message was %q, want %q", s.Hello.Kind, KindHello)
	}
	if s.Hello.Version != ProtocolVersion {
		return fmt.Errorf("agent speaks protocol %d, host speaks %d", s.Hello.Version, ProtocolVersion)
	}
	return nil
}

// Do sends a request and waits for its reply.
//
// Requests are serialised: the port carries one exchange at a time, which is
// enough for a control plane and avoids a correlation table. The IDs are on
// the wire so a later version can relax this without a protocol change.
func (s *Session) Do(req Request, timeout time.Duration) (*Response, error) {
	s.mu.Lock()
	defer s.mu.Unlock()

	req.ID = s.next
	s.next++

	if err := s.f.SetDeadline(time.Now().Add(timeout)); err != nil {
		return nil, err
	}
	defer s.f.SetDeadline(time.Time{})

	b, err := json.Marshal(req)
	if err != nil {
		return nil, err
	}
	if _, err := s.f.Write(append(b, '\n')); err != nil {
		return nil, fmt.Errorf("sending %s: %w", req.Kind, err)
	}

	line, err := s.br.ReadBytes('\n')
	if err != nil {
		return nil, fmt.Errorf("awaiting reply to %s: %w", req.Kind, err)
	}
	var resp Response
	if err := json.Unmarshal(line, &resp); err != nil {
		return nil, fmt.Errorf("malformed reply to %s: %w", req.Kind, err)
	}
	if resp.ID != req.ID {
		return nil, fmt.Errorf("reply id %d does not match request %d", resp.ID, req.ID)
	}
	return &resp, nil
}

// Ping checks the agent is answering.
func (s *Session) Ping(timeout time.Duration) error {
	resp, err := s.Do(Request{Kind: KindPing}, timeout)
	if err != nil {
		return err
	}
	if resp.Err != "" {
		return fmt.Errorf("ping: %s", resp.Err)
	}
	return nil
}

// Exec runs a command in the environment and returns its output.
//
// A command that runs and exits non-zero is not an error here: that is
// reported in Response.Code. An error means the command could not be run.
func (s *Session) Exec(timeout time.Duration, cmd ...string) (*Response, error) {
	resp, err := s.Do(Request{Kind: KindExec, Cmd: cmd}, timeout)
	if err != nil {
		return nil, err
	}
	if resp.Err != "" {
		return nil, fmt.Errorf("exec %v: %s", cmd, resp.Err)
	}
	return resp, nil
}

// Close ends the session.
func (s *Session) Close() error { return s.f.Close() }
