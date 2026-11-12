// Package terminaltest stands in for the guest's terminal service
// (crates/agent/src/terminal.rs) in tests of what carries its connections:
// the worker's tunnel and the server's sockets. A session runs a command on
// this machine, on pipes rather than a terminal; what it writes is kept and
// replayed to each connection that attaches, before the live output, and
// each is sent the exit when it ends. Sizes are taken and not acted on.
package terminaltest

import (
	"bufio"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"sort"
	"sync"
	"time"

	"github.com/csnewman/hangar/internal/terminal"
)

// Guest holds sessions, each running a command Command gives.
type Guest struct {
	command func() *exec.Cmd

	mu       sync.Mutex
	sessions map[string]*session
	next     int
}

type session struct {
	id      string
	created time.Time
	cols    uint16
	rows    uint16
	cmd     *exec.Cmd
	stdin   io.WriteCloser
	// history, clients, ended and code are the Guest's to guard.
	history []byte
	clients map[chan []byte]struct{}
	ended   bool
	code    int
}

func New(command func() *exec.Cmd) *Guest {
	return &Guest{command: command, sessions: map[string]*session{}}
}

// Serve answers one connection.
func (g *Guest) Serve(conn io.ReadWriteCloser) {
	defer conn.Close()
	br := bufio.NewReader(conn)
	line, err := br.ReadBytes('\n')
	if err != nil {
		return
	}
	var req terminal.Request
	if err := json.Unmarshal(line, &req); err != nil {
		return
	}
	switch req.Op {
	case terminal.OpList:
		reply(conn, terminal.Reply{Sessions: g.list()})
	case terminal.OpClose:
		g.mu.Lock()
		s := g.sessions[req.Session]
		g.mu.Unlock()
		if s == nil {
			reply(conn, terminal.Reply{Err: "no such session"})
			return
		}
		_ = s.cmd.Process.Kill()
		reply(conn, terminal.Reply{})
	case terminal.OpAttach:
		g.attach(conn, br, req)
	default:
		reply(conn, terminal.Reply{Err: "unknown operation " + req.Op})
	}
}

func (g *Guest) attach(conn io.Writer, br *bufio.Reader, req terminal.Request) {
	g.mu.Lock()
	s := g.sessions[req.Session]
	if req.Session == "" {
		var err error
		if s, err = g.start(req); err != nil {
			g.mu.Unlock()
			reply(conn, terminal.Reply{Err: err.Error()})
			return
		}
	}
	if s == nil {
		g.mu.Unlock()
		reply(conn, terminal.Reply{Err: "no such session"})
		return
	}
	out := make(chan []byte, 256)
	history := append([]byte(nil), s.history...)
	if s.ended {
		close(out)
	} else {
		s.clients[out] = struct{}{}
	}
	d := s.describe()
	g.mu.Unlock()

	reply(conn, terminal.Reply{Session: &d, History: len(history), Owner: true})
	go func() {
		for {
			typ, p, err := terminal.ReadFrame(br)
			if err != nil {
				break
			}
			if typ == terminal.FrameInput {
				_, _ = s.stdin.Write(p)
			}
		}
		g.mu.Lock()
		if _, ok := s.clients[out]; ok {
			delete(s.clients, out)
			close(out)
		}
		g.mu.Unlock()
	}()
	if len(history) > 0 && terminal.WriteFrame(conn, terminal.FrameOutput, history) != nil {
		return
	}
	for p := range out {
		if terminal.WriteFrame(conn, terminal.FrameOutput, p) != nil {
			return
		}
	}
	g.mu.Lock()
	ended, code := s.ended, s.code
	g.mu.Unlock()
	if ended {
		b := make([]byte, 4)
		binary.BigEndian.PutUint32(b, uint32(int32(code)))
		_ = terminal.WriteFrame(conn, terminal.FrameExit, b)
	}
}

// start runs a new session's command. g.mu is held.
func (g *Guest) start(req terminal.Request) (*session, error) {
	cmd := g.command()
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, err
	}
	r, w, err := os.Pipe()
	if err != nil {
		return nil, err
	}
	cmd.Stdout, cmd.Stderr = w, w
	if err := cmd.Start(); err != nil {
		r.Close()
		w.Close()
		return nil, err
	}
	w.Close()
	g.next++
	s := &session{id: fmt.Sprintf("%012x", g.next), created: time.Now(), cols: req.Cols, rows: req.Rows,
		cmd: cmd, stdin: stdin, clients: map[chan []byte]struct{}{}}
	g.sessions[s.id] = s
	go func() {
		buf := make([]byte, 32<<10)
		for {
			n, err := r.Read(buf)
			if n > 0 {
				p := append([]byte(nil), buf[:n]...)
				g.mu.Lock()
				s.history = append(s.history, p...)
				for c := range s.clients {
					select {
					case c <- p:
					default:
					}
				}
				g.mu.Unlock()
			}
			if err != nil {
				break
			}
		}
		r.Close()
		code := 0
		if err := cmd.Wait(); err != nil {
			code = -1
			if ee, ok := err.(*exec.ExitError); ok {
				code = ee.ExitCode()
			}
		}
		g.mu.Lock()
		delete(g.sessions, s.id)
		s.ended, s.code = true, code
		for c := range s.clients {
			close(c)
		}
		s.clients = map[chan []byte]struct{}{}
		g.mu.Unlock()
	}()
	return s, nil
}

func (g *Guest) list() []terminal.Session {
	g.mu.Lock()
	defer g.mu.Unlock()
	out := []terminal.Session{}
	for _, s := range g.sessions {
		out = append(out, s.describe())
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Created.Before(out[j].Created) })
	return out
}

// describe is g.mu's to call.
func (s *session) describe() terminal.Session {
	return terminal.Session{ID: s.id, Created: s.created, Clients: len(s.clients), Cols: s.cols, Rows: s.rows}
}

func reply(w io.Writer, r terminal.Reply) {
	b, _ := json.Marshal(r)
	_, _ = w.Write(append(b, '\n'))
}
