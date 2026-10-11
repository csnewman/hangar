package terminal_test

import (
	"bufio"
	"bytes"
	"encoding/json"
	"io"
	"log/slog"
	"net"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/terminal"
)

// conn is one client connection to a Manager.
type conn struct {
	t   *testing.T
	c   net.Conn
	br  *bufio.Reader
	out bytes.Buffer
}

func dial(t *testing.T, m *terminal.Manager, req terminal.Request) (*conn, terminal.Reply) {
	t.Helper()
	a, b := net.Pipe()
	go m.Serve(b)
	c := &conn{t: t, c: a, br: bufio.NewReader(a)}
	line, _ := json.Marshal(req)
	a.SetDeadline(time.Now().Add(10 * time.Second))
	if _, err := a.Write(append(line, '\n')); err != nil {
		t.Fatal(err)
	}
	reply, err := c.br.ReadBytes('\n')
	if err != nil {
		t.Fatal(err)
	}
	var r terminal.Reply
	if err := json.Unmarshal(reply, &r); err != nil {
		t.Fatal(err)
	}
	return c, r
}

// until reads output frames until the text seen so far contains want, and
// reports whether the shell exited first.
func (c *conn) until(want string) bool {
	c.t.Helper()
	c.c.SetDeadline(time.Now().Add(10 * time.Second))
	for !strings.Contains(c.out.String(), want) {
		typ, p, err := terminal.ReadFrame(c.br)
		if err != nil {
			c.t.Fatalf("waiting for %q, got %q: %v", want, c.out.String(), err)
		}
		switch typ {
		case terminal.FrameOutput:
			c.out.Write(p)
		case terminal.FrameExit:
			return true
		}
	}
	return false
}

// size is what a FrameSize says.
type size struct {
	cols, rows uint16
	clients    int
	owner      bool
}

// size reads frames until a size, keeping any output, and returns it.
func (c *conn) size() size {
	c.t.Helper()
	c.c.SetDeadline(time.Now().Add(10 * time.Second))
	for {
		typ, p, err := terminal.ReadFrame(c.br)
		if err != nil {
			c.t.Fatalf("waiting for a size: %v", err)
		}
		switch typ {
		case terminal.FrameOutput:
			c.out.Write(p)
		case terminal.FrameSize:
			cols, rows, n, owner, err := terminal.ParseSize(p)
			if err != nil {
				c.t.Fatal(err)
			}
			return size{cols, rows, n, owner}
		}
	}
}

func (c *conn) sized(typ byte, cols, rows uint16) {
	c.t.Helper()
	if err := terminal.WriteFrame(c.c, typ, terminal.ResizePayload(cols, rows)); err != nil {
		c.t.Fatal(err)
	}
}

func (c *conn) send(s string) {
	c.t.Helper()
	if err := terminal.WriteFrame(c.c, terminal.FrameInput, []byte(s)); err != nil {
		c.t.Fatal(err)
	}
}

func newManager() *terminal.Manager {
	return terminal.NewManager(func(terminal.Request) (*exec.Cmd, error) {
		// A shell that says hello and then echoes what it is sent, so the
		// test does not depend on anyone's prompt.
		return exec.Command("/bin/sh", "-c", `echo hello; while read l; do echo "got:$l"; done`), nil
	}, slog.New(slog.NewTextHandler(io.Discard, nil)))
}

func TestSessionsOutliveConnections(t *testing.T) {
	m := newManager()

	first, r := dial(t, m, terminal.Request{Op: terminal.OpAttach, Cols: 100, Rows: 30})
	if r.Err != "" || r.Session == nil {
		t.Fatalf("attach: %+v", r)
	}
	id := r.Session.ID
	first.until("hello")
	first.send("one\n")
	first.until("got:one")

	// A second connection -- another tab -- gets the history, then the
	// same live output as the first.
	second, r := dial(t, m, terminal.Request{Op: terminal.OpAttach, Session: id})
	if r.Err != "" || r.Session.Clients != 2 {
		t.Fatalf("second attach: %+v", r)
	}
	// It is told how much of what follows is the replay: the history so far.
	if want := len("hello\r\none\r\ngot:one\r\n"); r.History != want {
		t.Fatalf("history: %d bytes, want %d", r.History, want)
	}
	second.until("got:one")
	second.send("two\n")
	first.until("got:two")
	second.until("got:two")

	// The first goes away -- a refresh -- and the session carries on.
	first.c.Close()
	time.Sleep(100 * time.Millisecond)
	list, r := dial(t, m, terminal.Request{Op: terminal.OpList})
	list.c.Close()
	if len(r.Sessions) != 1 || r.Sessions[0].ID != id || r.Sessions[0].Clients != 1 {
		t.Fatalf("after one left: %+v", r.Sessions)
	}

	third, r := dial(t, m, terminal.Request{Op: terminal.OpAttach, Session: id})
	if r.Err != "" {
		t.Fatalf("re-attach: %+v", r)
	}
	third.until("got:two")

	// Closing hangs up on the shell, and every client is told it ended.
	closer, r := dial(t, m, terminal.Request{Op: terminal.OpClose, Session: id})
	closer.c.Close()
	if r.Err != "" {
		t.Fatalf("close: %+v", r)
	}
	if !second.until("never printed") {
		t.Fatal("no exit frame after close")
	}

	_, r = dial(t, m, terminal.Request{Op: terminal.OpAttach, Session: id})
	if r.Err == "" {
		t.Fatal("attached to a session that ended")
	}
}

// The size is the window of whoever is using the terminal: the first to
// attach, then whoever types or asks for it. Another window opening, or
// changing, takes nothing from them; it is told the size to draw at.
func TestTheSizeIsWhoeverIsUsingIt(t *testing.T) {
	m := newManager()

	a, r := dial(t, m, terminal.Request{Op: terminal.OpAttach, Cols: 100, Rows: 30})
	if r.Err != "" || !r.Owner || r.Session.Cols != 100 || r.Session.Rows != 30 {
		t.Fatalf("first attach: %+v", r)
	}
	id := r.Session.ID
	if got := a.size(); got != (size{100, 30, 1, true}) {
		t.Fatalf("first size: %+v", got)
	}

	b, r := dial(t, m, terminal.Request{Op: terminal.OpAttach, Session: id, Cols: 60, Rows: 20})
	if r.Owner || r.Session.Cols != 100 || r.Session.Rows != 30 {
		t.Fatalf("a second window took the size: %+v", r)
	}
	if got := b.size(); got != (size{100, 30, 2, false}) {
		t.Fatalf("second window told: %+v", got)
	}
	if got := a.size(); got != (size{100, 30, 2, true}) {
		t.Fatalf("first window told: %+v", got)
	}

	// The other window changing changes nothing; the holder's does.
	b.sized(terminal.FramePrefer, 70, 25)
	a.sized(terminal.FramePrefer, 110, 35)
	if got := a.size(); got != (size{110, 35, 2, true}) {
		t.Fatalf("after the holder's window changed: %+v", got)
	}
	if got := b.size(); got != (size{110, 35, 2, false}) {
		t.Fatalf("the other told: %+v", got)
	}

	// Typing takes the size, at the typist's window's.
	b.send("x\n")
	if got := b.size(); got != (size{70, 25, 2, true}) {
		t.Fatalf("after typing: %+v", got)
	}
	if got := a.size(); got != (size{70, 25, 2, false}) {
		t.Fatalf("the first told: %+v", got)
	}

	// Asking for it takes it back.
	a.sized(terminal.FrameResize, 120, 40)
	if got := a.size(); got != (size{120, 40, 2, true}) {
		t.Fatalf("after asking: %+v", got)
	}
	if got := b.size(); got != (size{120, 40, 2, false}) {
		t.Fatalf("the second told: %+v", got)
	}

	// The holder leaving leaves the size as it was, held by nobody.
	a.c.Close()
	if got := b.size(); got != (size{120, 40, 1, false}) {
		t.Fatalf("after the holder left: %+v", got)
	}
	list, r := dial(t, m, terminal.Request{Op: terminal.OpList})
	list.c.Close()
	if len(r.Sessions) != 1 || r.Sessions[0].Cols != 120 || r.Sessions[0].Rows != 40 {
		t.Fatalf("the terminal's size: %+v", r.Sessions)
	}

	closer, _ := dial(t, m, terminal.Request{Op: terminal.OpClose, Session: id})
	closer.c.Close()
}

func TestFrames(t *testing.T) {
	var b bytes.Buffer
	if err := terminal.WriteFrame(&b, terminal.FrameResize, terminal.ResizePayload(132, 43)); err != nil {
		t.Fatal(err)
	}
	typ, p, err := terminal.ReadFrame(&b)
	if err != nil || typ != terminal.FrameResize {
		t.Fatalf("read: %c %v", typ, err)
	}
	cols, rows, err := terminal.ParseResize(p)
	if err != nil || cols != 132 || rows != 43 {
		t.Fatalf("resize %dx%d %v", cols, rows, err)
	}
	gotCols, gotRows, n, owner, err := terminal.ParseSize(terminal.SizePayload(80, 24, 3, true))
	if err != nil || gotCols != 80 || gotRows != 24 || n != 3 || !owner {
		t.Fatalf("size %dx%d %d %v %v", gotCols, gotRows, n, owner, err)
	}
	if err := terminal.WriteFrame(&b, terminal.FrameInput, make([]byte, terminal.MaxFrame+1)); err == nil {
		t.Fatal("wrote an oversized frame")
	}
}
