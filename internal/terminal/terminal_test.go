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
	if err := terminal.WriteFrame(&b, terminal.FrameInput, make([]byte, terminal.MaxFrame+1)); err == nil {
		t.Fatal("wrote an oversized frame")
	}
}
