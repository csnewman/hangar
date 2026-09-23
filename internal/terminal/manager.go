package terminal

import (
	"bufio"
	"bytes"
	"crypto/rand"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"os/exec"
	"sort"
	"sync"
	"time"

	"github.com/creack/pty"
)

// HistoryBytes is how much of a session's recent output is kept to replay
// to a connection that attaches.
const HistoryBytes = 2 << 20

// Shell starts a session's shell. It is given the new session's request and
// returns the command to run on the pseudo-terminal, which the Manager
// starts in a session of its own with the terminal as its controlling one.
type Shell func(req Request) (*exec.Cmd, error)

// Manager holds a machine's sessions.
type Manager struct {
	shell Shell
	log   *slog.Logger

	mu       sync.Mutex
	sessions map[string]*session
}

func NewManager(shell Shell, log *slog.Logger) *Manager {
	if log == nil {
		log = slog.Default()
	}
	return &Manager{shell: shell, log: log, sessions: map[string]*session{}}
}

// Serve handles one connection: it reads the Request, answers it, and for an
// attach relays frames until either end goes away. The session outlives the
// connection.
func (m *Manager) Serve(conn io.ReadWriteCloser) {
	defer conn.Close()
	br := bufio.NewReader(conn)
	line, err := br.ReadBytes('\n')
	if err != nil {
		return
	}
	var req Request
	if err := json.Unmarshal(line, &req); err != nil {
		writeReply(conn, Reply{Err: "malformed request: " + err.Error()})
		return
	}
	switch req.Op {
	case OpList:
		writeReply(conn, Reply{Sessions: m.List()})
	case OpClose:
		if err := m.Close(req.Session); err != nil {
			writeReply(conn, Reply{Err: err.Error()})
			return
		}
		writeReply(conn, Reply{})
	case OpAttach:
		m.attach(conn, br, req)
	default:
		writeReply(conn, Reply{Err: fmt.Sprintf("unknown operation %q", req.Op)})
	}
}

func writeReply(w io.Writer, r Reply) error {
	b, err := json.Marshal(r)
	if err != nil {
		return err
	}
	_, err = w.Write(append(b, '\n'))
	return err
}

// List returns every session, oldest first.
func (m *Manager) List() []Session {
	m.mu.Lock()
	all := make([]*session, 0, len(m.sessions))
	for _, s := range m.sessions {
		all = append(all, s)
	}
	m.mu.Unlock()
	out := make([]Session, 0, len(all))
	for _, s := range all {
		out = append(out, s.describe())
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Created.Before(out[j].Created) })
	return out
}

var errNoSession = errors.New("no such session")

// Close ends a session: its shell is hung up on, as when a terminal window
// closes.
func (m *Manager) Close(id string) error {
	m.mu.Lock()
	s, ok := m.sessions[id]
	m.mu.Unlock()
	if !ok {
		return errNoSession
	}
	s.hangup()
	return nil
}

func (m *Manager) attach(conn io.ReadWriteCloser, br *bufio.Reader, req Request) {
	s, err := m.find(req)
	if err != nil {
		writeReply(conn, Reply{Err: err.Error()})
		return
	}

	c := &client{out: make(chan []byte, 256)}
	history := s.join(c)
	defer s.leave(c)

	d := s.describe()
	if err := writeReply(conn, Reply{Session: &d}); err != nil {
		return
	}

	// Output goes through its own goroutine so a slow reader never holds up
	// the session: it is fed from a buffered channel, and a client that lets
	// the buffer fill is dropped rather than stalling everyone else.
	var wmu sync.Mutex
	write := func(typ byte, p []byte) error {
		wmu.Lock()
		defer wmu.Unlock()
		return WriteFrame(conn, typ, p)
	}
	done := make(chan struct{})
	go func() {
		defer close(done)
		for len(history) > 0 {
			n := min(len(history), 64<<10)
			if write(FrameOutput, history[:n]) != nil {
				conn.Close()
				return
			}
			history = history[n:]
		}
		for p := range c.out {
			if p == nil {
				code := make([]byte, 4)
				binary.BigEndian.PutUint32(code, uint32(int32(s.exitCode())))
				write(FrameExit, code)
				conn.Close()
				return
			}
			if write(FrameOutput, p) != nil {
				conn.Close()
				return
			}
		}
		// Dropped for falling behind: closing the connection tells the
		// other end, which re-attaches.
		conn.Close()
	}()

	if req.Cols > 0 && req.Rows > 0 {
		s.resize(req.Cols, req.Rows, true)
	}
	for {
		typ, payload, err := ReadFrame(br)
		if err != nil {
			break
		}
		switch typ {
		case FrameInput:
			s.input(payload)
		case FrameResize:
			if cols, rows, err := ParseResize(payload); err == nil {
				s.resize(cols, rows, false)
			}
		}
	}
	s.leave(c)
	<-done
}

// find returns the session a request names, starting a new one if it names
// none.
func (m *Manager) find(req Request) (*session, error) {
	if req.Session != "" {
		m.mu.Lock()
		s, ok := m.sessions[req.Session]
		m.mu.Unlock()
		if !ok {
			return nil, errNoSession
		}
		return s, nil
	}

	cmd, err := m.shell(req)
	if err != nil {
		return nil, err
	}
	cols, rows := req.Cols, req.Rows
	if cols == 0 || rows == 0 {
		cols, rows = 80, 24
	}
	ptmx, err := pty.StartWithSize(cmd, &pty.Winsize{Cols: cols, Rows: rows})
	if err != nil {
		return nil, fmt.Errorf("starting the shell: %w", err)
	}
	id := make([]byte, 6)
	_, _ = rand.Read(id)
	s := &session{
		id:      hex.EncodeToString(id),
		created: time.Now(),
		cmd:     cmd,
		ptmx:    ptmx,
		cols:    cols,
		rows:    rows,
		clients: map[*client]struct{}{},
	}
	m.mu.Lock()
	m.sessions[s.id] = s
	m.mu.Unlock()
	go s.pump(func() {
		m.mu.Lock()
		delete(m.sessions, s.id)
		m.mu.Unlock()
		m.log.Info("terminal session ended", "session", s.id, "code", s.exitCode())
	})
	m.log.Info("terminal session started", "session", s.id, "cmd", cmd.Path)
	return s, nil
}

// client is one attached connection.
type client struct {
	// out carries output to the connection; nil means the shell exited.
	out    chan []byte
	closed bool
}

type session struct {
	id      string
	created time.Time
	cmd     *exec.Cmd
	ptmx    *os.File

	// wmu keeps writes to the terminal whole, whichever client sends them.
	wmu sync.Mutex

	mu      sync.Mutex
	cols    uint16
	rows    uint16
	title   string
	alt     bool
	history []byte
	// carry is the end of the previous read, so a sequence split across two
	// reads is still recognised.
	carry   []byte
	clients map[*client]struct{}
	ended   bool
	code    int
}

func (s *session) describe() Session {
	s.mu.Lock()
	defer s.mu.Unlock()
	return Session{ID: s.id, Title: s.title, Created: s.created, Clients: len(s.clients), Cols: s.cols, Rows: s.rows}
}

// join attaches a client and returns the history to replay to it. Both
// happen under the session's lock, so no output falls between the replay and
// the live stream, and none is sent twice.
func (s *session) join(c *client) []byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.ended {
		c.out <- nil
	} else {
		s.clients[c] = struct{}{}
	}
	return bytes.Clone(s.history)
}

func (s *session) leave(c *client) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.clients[c]; ok {
		delete(s.clients, c)
		if !c.closed {
			c.closed = true
			close(c.out)
		}
	}
}

func (s *session) input(p []byte) {
	s.wmu.Lock()
	defer s.wmu.Unlock()
	_, _ = s.ptmx.Write(p)
}

// resize sets the terminal's size. An attach passes redraw, which also tells
// a full-screen program to draw itself again even if the size is unchanged:
// the replayed output is a record of bytes, and a program that has been
// redrawing one corner of the screen for an hour cannot be reconstructed
// from the last two megabytes of it.
func (s *session) resize(cols, rows uint16, redraw bool) {
	s.mu.Lock()
	same := s.cols == cols && s.rows == rows
	s.cols, s.rows = cols, rows
	alt := s.alt
	s.mu.Unlock()
	if !same {
		_ = pty.Setsize(s.ptmx, &pty.Winsize{Cols: cols, Rows: rows})
		return
	}
	if redraw && alt {
		signalForeground(s.ptmx)
	}
}

func (s *session) hangup() {
	if s.cmd.Process != nil {
		hangupGroup(s.cmd.Process.Pid)
	}
}

func (s *session) exitCode() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.code
}

// pump copies the terminal's output to the history and every client until
// the shell exits.
func (s *session) pump(ended func()) {
	buf := make([]byte, 32<<10)
	for {
		n, err := s.ptmx.Read(buf)
		if n > 0 {
			s.output(bytes.Clone(buf[:n]))
		}
		if err != nil {
			break
		}
	}
	code := 0
	if err := s.cmd.Wait(); err != nil {
		var ee *exec.ExitError
		if errors.As(err, &ee) {
			code = ee.ExitCode()
		} else {
			code = -1
		}
	}
	s.ptmx.Close()

	s.mu.Lock()
	s.ended = true
	s.code = code
	for c := range s.clients {
		if !c.closed {
			select {
			case c.out <- nil:
			default:
			}
		}
	}
	s.mu.Unlock()
	ended()
}

func (s *session) output(p []byte) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.track(p)
	s.history = append(s.history, p...)
	if over := len(s.history) - HistoryBytes; over > 0 {
		// Cut at a line boundary, so the replay does not start halfway
		// through an escape sequence and print its tail as text.
		cut := over
		if i := bytes.IndexByte(s.history[over:], '\n'); i >= 0 {
			cut = over + i + 1
		}
		s.history = append([]byte(nil), s.history[cut:]...)
	}
	for c := range s.clients {
		select {
		case c.out <- p:
		default:
			// Too far behind to catch up without holding up everyone
			// else; its connection is closed, and a refresh re-attaches.
			delete(s.clients, c)
			c.closed = true
			close(c.out)
		}
	}
}

var (
	altOn  = [][]byte{[]byte("\x1b[?1049h"), []byte("\x1b[?1047h"), []byte("\x1b[?47h")}
	altOff = [][]byte{[]byte("\x1b[?1049l"), []byte("\x1b[?1047l"), []byte("\x1b[?47l")}
)

// track follows the window title and whether a full-screen program has the
// alternate screen, from the output as it passes.
func (s *session) track(p []byte) {
	buf := append(s.carry, p...)
	on, off := lastIndex(buf, altOn), lastIndex(buf, altOff)
	if on > off {
		s.alt = true
	} else if off > on {
		s.alt = false
	}
	// OSC 0 or 2: ESC ] 0 ; title BEL or ESC ] 2 ; title ST.
	for _, start := range [][]byte{[]byte("\x1b]0;"), []byte("\x1b]2;")} {
		if i := bytes.LastIndex(buf, start); i >= 0 {
			rest := buf[i+len(start):]
			end := bytes.IndexAny(rest, "\x07\x1b")
			if end >= 0 && end <= 256 {
				s.title = string(rest[:end])
			}
		}
	}
	if len(buf) > 300 {
		buf = buf[len(buf)-300:]
	}
	s.carry = append(s.carry[:0], buf...)
}

func lastIndex(b []byte, seps [][]byte) int {
	best := -1
	for _, sep := range seps {
		if i := bytes.LastIndex(b, sep); i > best {
			best = i
		}
	}
	return best
}
