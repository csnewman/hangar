// Package profiletest is the guest's half of profile sessions, for tests of
// the server's. It keeps what the server sends and asks what the guest
// agent asks, but routes nothing and moves no files: answering the copies
// with what it routes to and each resolution with its having been done is
// all it does with them. The guest agent's own half is in
// crates/agent/src/profile.
package profiletest

import (
	"bufio"
	"encoding/json"
	"errors"
	"io"
	"strings"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/profile"
)

// Guest is one environment's guest, across its sessions.
type Guest struct {
	mu      sync.Mutex
	current *session
	changed chan struct{} // closed and replaced as anything changes

	keys        []string
	keysSet     bool
	sets        []profile.SetPaths
	setsSet     bool
	registry    string
	task        *api.Task
	resolutions []profile.Message
}

// New is a guest with no session yet.
func New() *Guest { return &Guest{changed: make(chan struct{})} }

type session struct {
	conn io.ReadWriteCloser
	// out is written by a goroutine of its own, so reading never waits on
	// writing: the server may be writing to the guest as the guest writes
	// to it.
	out  chan []byte
	done chan struct{}

	pmu     sync.Mutex
	pending map[int64]chan profile.Message
	nextID  int64
}

// wait is how long the guest waits for a session or an answer.
const wait = 10 * time.Second

func (s *session) send(m profile.Message) error {
	b, err := json.Marshal(m)
	if err != nil {
		return err
	}
	select {
	case s.out <- append(b, '\n'):
		return nil
	case <-s.done:
		return errors.New("the profile session ended")
	}
}

func (s *session) write() {
	for {
		select {
		case b := <-s.out:
			if _, err := s.conn.Write(b); err != nil {
				s.conn.Close()
				return
			}
		case <-s.done:
			return
		}
	}
}

// Serve holds one session, replacing the one before it, until conn ends.
func (g *Guest) Serve(conn io.ReadWriteCloser) {
	defer conn.Close()
	s := &session{conn: conn, out: make(chan []byte, 256), done: make(chan struct{}),
		pending: map[int64]chan profile.Message{}}
	defer close(s.done)
	go s.write()
	g.mu.Lock()
	if g.current != nil {
		g.current.conn.Close()
	}
	g.current = s
	g.notify()
	g.mu.Unlock()
	defer func() {
		g.mu.Lock()
		if g.current == s {
			g.current = nil
			g.notify()
		}
		g.mu.Unlock()
		s.pmu.Lock()
		for id, ch := range s.pending {
			ch <- profile.Message{ID: id, Error: "the profile session ended"}
			delete(s.pending, id)
		}
		s.pmu.Unlock()
	}()

	r := bufio.NewReader(conn)
	for {
		line, err := r.ReadBytes('\n')
		if err != nil {
			return
		}
		var m profile.Message
		if json.Unmarshal(line, &m) != nil {
			return
		}
		switch m.Type {
		case profile.TypeSigned, profile.TypeCredential, profile.TypeTaskSet:
			s.pmu.Lock()
			ch := s.pending[m.ID]
			delete(s.pending, m.ID)
			s.pmu.Unlock()
			if ch != nil {
				ch <- m
			}
		case profile.TypePaths:
			g.mu.Lock()
			g.sets, g.setsSet = m.Sets, true
			g.notify()
			g.mu.Unlock()
			s.send(profile.Message{Type: profile.TypeRouted, Routed: routed(m.Sets)})
		case profile.TypeKeys:
			g.mu.Lock()
			g.keys, g.keysSet = m.Keys, true
			g.notify()
			g.mu.Unlock()
		case profile.TypeRegistry:
			g.mu.Lock()
			g.registry = m.Host
			g.notify()
			g.mu.Unlock()
		case profile.TypeTask:
			g.mu.Lock()
			g.task = m.Task
			g.notify()
			g.mu.Unlock()
		case profile.TypeResolve:
			g.mu.Lock()
			g.resolutions = append(g.resolutions, m)
			g.notify()
			g.mu.Unlock()
			s.send(profile.Message{Type: profile.TypeResolved, Copy: m.Copy, Path: m.Path})
		}
	}
}

// routed are the copies sets route to: those sharing any path.
func routed(sets []profile.SetPaths) []string {
	var out []string
	for _, s := range sets {
		for _, p := range s.Paths {
			if !strings.HasPrefix(p, "!") {
				out = append(out, s.ID)
				break
			}
		}
	}
	return out
}

func (g *Guest) notify() {
	close(g.changed)
	g.changed = make(chan struct{})
}

// connected is the session once there is one, waiting a while.
func (g *Guest) connected() (*session, error) {
	deadline := time.After(wait)
	for {
		g.mu.Lock()
		s, changed := g.current, g.changed
		g.mu.Unlock()
		if s != nil {
			return s, nil
		}
		select {
		case <-changed:
		case <-deadline:
			return nil, errors.New("no session")
		}
	}
}

// ask sends a request and waits for its answer.
func (g *Guest) ask(m profile.Message) (profile.Message, error) {
	s, err := g.connected()
	if err != nil {
		return profile.Message{}, err
	}
	ch := make(chan profile.Message, 1)
	s.pmu.Lock()
	s.nextID++
	m.ID = s.nextID
	s.pending[m.ID] = ch
	s.pmu.Unlock()
	if err := s.send(m); err != nil {
		return profile.Message{}, err
	}
	select {
	case r := <-ch:
		if r.Error != "" {
			return r, errors.New(r.Error)
		}
		return r, nil
	case <-time.After(wait):
		return profile.Message{}, errors.New("the server did not answer")
	}
}

// Keys are the authorized-keys lines the server last sent, and whether it
// has sent any.
func (g *Guest) Keys() ([]string, bool) {
	g.mu.Lock()
	defer g.mu.Unlock()
	return g.keys, g.keysSet
}

// Sets are the copies and their paths the server last sent, and whether
// it has sent them.
func (g *Guest) Sets() ([]profile.SetPaths, bool) {
	g.mu.Lock()
	defer g.mu.Unlock()
	return g.sets, g.setsSet
}

// Registry is the registry's host, as the server said.
func (g *Guest) Registry() string {
	g.mu.Lock()
	defer g.mu.Unlock()
	return g.registry
}

// Task is the environment's task, as the server last said.
func (g *Guest) Task() *api.Task {
	g.mu.Lock()
	defer g.mu.Unlock()
	return g.task
}

// Resolutions are the resolve messages the server has sent.
func (g *Guest) Resolutions() []profile.Message {
	g.mu.Lock()
	defer g.mu.Unlock()
	return append([]profile.Message(nil), g.resolutions...)
}

// Sign has the server sign data with key, given in SSH wire form, and
// returns the signature in wire form.
func (g *Guest) Sign(key, data []byte) ([]byte, error) {
	r, err := g.ask(profile.Message{Type: profile.TypeSign, Key: key, Data: data})
	return r.Signature, err
}

// Credential asks the server for a credential for its registry.
func (g *Guest) Credential() (profile.Message, error) {
	return g.ask(profile.Message{Type: profile.TypeGetCredential})
}

// SetTask sets the environment's task as its coding agent, as it is after.
func (g *Guest) SetTask(text string) (*api.Task, error) {
	r, err := g.ask(profile.Message{Type: profile.TypeSetTask, Text: text})
	return r.Task, err
}

// Send sends the server a message of the guest's own: a conflict, the
// coding agents' sessions.
func (g *Guest) Send(m profile.Message) error {
	s, err := g.connected()
	if err != nil {
		return err
	}
	return s.send(m)
}
