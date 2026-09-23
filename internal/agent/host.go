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
	ln vsock.Acceptor
}

// Listen starts accepting agent connections on Port over AF_VSOCK.
//
// The listener binds CIDAny, so one server serves every environment on the
// node rather than one per guest. This is the right form for a monitor whose
// vsock is the host kernel's vhost-vsock.
func Listen() (*Server, error) {
	ln, err := vsock.Listen(vsock.CIDAny, Port)
	if err != nil {
		return nil, err
	}
	return &Server{ln: ln}, nil
}

// ListenHybrid starts accepting agent connections from a monitor that carries
// vsock over a unix socket instead of the host kernel, which is what Cloud
// Hypervisor does.
//
// base is the monitor's vsock socket path and cid is the context ID the host
// allocated for that guest. Unlike Listen, one server serves one environment,
// because the socket belongs to one monitor.
func ListenHybrid(base string, cid uint32) (*Server, error) {
	ln, err := vsock.ListenHybrid(base, Port, cid)
	if err != nil {
		return nil, err
	}
	return &Server{ln: ln}, nil
}

// Close stops the server. A blocked Accept returns an error once it is closed.
func (s *Server) Close() error { return s.ln.Close() }

// Accept waits for an agent to connect and reads its Hello.
//
// The CID does not come from the agent, so it identifies which environment is
// calling even if the agent lies about its hostname. On AF_VSOCK the kernel
// stamps it; over a unix socket it is the host's own allocation for the guest
// that socket belongs to.
func (s *Server) Accept(timeout time.Duration) (*Session, error) {
	f, cid, err := s.ln.Accept(timeout)
	if err != nil {
		return nil, err
	}
	sess := &Session{
		CID:     cid,
		f:       f,
		br:      bufio.NewReader(f),
		next:    1,
		pending: map[uint64]chan *Response{},
	}
	if err := sess.readHello(timeout); err != nil {
		f.Close()
		return nil, err
	}
	go sess.readReplies()
	return sess, nil
}

// Session is a live connection to one environment's agent.
//
// Requests are independent: several may be outstanding at once, and each
// reply is matched to its request by ID. A long-running command therefore
// holds up nothing but its own caller, and a request that times out leaves
// nothing behind to be mistaken for the reply to the next.
type Session struct {
	// CID is the guest's context ID, established by the host rather than by
	// the peer. This is the only trustworthy identifier on the connection,
	// and the only thing a caller may resolve to an environment.
	//
	// A CID identifies whichever VM holds it now. They are recycled when
	// environments are destroyed, so a control plane must pair this with the
	// allocation it made and drop a session whose CID is no longer live.
	CID uint32
	// Hello is what the agent said about itself. Advisory; see its doc.
	Hello Hello

	f  *os.File
	br *bufio.Reader

	// wmu keeps each request's line whole on the wire.
	wmu sync.Mutex

	mu      sync.Mutex
	next    uint64
	pending map[uint64]chan *Response
	// broken is why the connection stopped carrying replies, once it has.
	broken error
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

// readReplies hands each reply to the request waiting for it, until the
// connection ends. A reply nobody is waiting for belongs to a request that
// timed out, and is dropped.
func (s *Session) readReplies() {
	for {
		line, err := s.br.ReadBytes('\n')
		if err != nil {
			s.mu.Lock()
			s.broken = err
			for id, ch := range s.pending {
				close(ch)
				delete(s.pending, id)
			}
			s.mu.Unlock()
			return
		}
		var resp Response
		if json.Unmarshal(line, &resp) != nil {
			continue
		}
		s.mu.Lock()
		ch, ok := s.pending[resp.ID]
		delete(s.pending, resp.ID)
		s.mu.Unlock()
		if ok {
			ch <- &resp
		}
	}
}

// Do sends a request and waits for its reply.
func (s *Session) Do(req Request, timeout time.Duration) (*Response, error) {
	ch := make(chan *Response, 1)
	s.mu.Lock()
	if s.broken != nil {
		err := s.broken
		s.mu.Unlock()
		return nil, fmt.Errorf("sending %s: the agent connection ended: %w", req.Kind, err)
	}
	req.ID = s.next
	s.next++
	s.pending[req.ID] = ch
	s.mu.Unlock()
	forget := func() {
		s.mu.Lock()
		delete(s.pending, req.ID)
		s.mu.Unlock()
	}

	b, err := json.Marshal(req)
	if err != nil {
		forget()
		return nil, err
	}
	s.wmu.Lock()
	err = s.f.SetWriteDeadline(time.Now().Add(timeout))
	if err == nil {
		_, err = s.f.Write(append(b, '\n'))
		_ = s.f.SetWriteDeadline(time.Time{})
	}
	s.wmu.Unlock()
	if err != nil {
		forget()
		return nil, fmt.Errorf("sending %s: %w", req.Kind, err)
	}

	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case resp, ok := <-ch:
		if !ok {
			s.mu.Lock()
			err := s.broken
			s.mu.Unlock()
			return nil, fmt.Errorf("awaiting reply to %s: the agent connection ended: %w", req.Kind, err)
		}
		return resp, nil
	case <-timer.C:
		forget()
		return nil, fmt.Errorf("awaiting reply to %s: no answer in %v", req.Kind, timeout)
	}
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

// SetClock sets the guest's wall clock to t.
//
// A guest has no clock of its own worth trusting: it boots at whatever time
// its image was built with, and a restored guest carries on from the moment
// it was suspended, however long ago that was. The host's clock is the
// reference, so the host sets the guest's every time the agent connects,
// which is both after boot and after every resume.
func (s *Session) SetClock(t time.Time, timeout time.Duration) error {
	resp, err := s.Do(Request{Kind: KindSetClock, UnixNanos: t.UnixNano()}, timeout)
	if err != nil {
		return err
	}
	if resp.Err != "" {
		return fmt.Errorf("setting the clock: %s", resp.Err)
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
