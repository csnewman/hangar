package worker

import (
	"context"
	"encoding/json"
	"fmt"
	"html"
	"net"
	"net/http"
	"sync"

	"github.com/csnewman/hangar/internal/api"
)

// DialEditor connects to a running environment's editor. There is no guest
// to run VS Code in, so the editor is a page saying so, which also shows what
// reached it: enough to see the path through the control plane and tunnel
// work. A request for /_simulated/request answers with that as JSON.
func (s *Simulated) DialEditor(_ context.Context, id string) (net.Conn, error) {
	s.mu.Lock()
	e, ok := s.envs[id]
	running := ok && e.phase == api.PhaseRunning
	s.mu.Unlock()
	if !running {
		return nil, fmt.Errorf("environment %s is not running here", id)
	}
	a, b := net.Pipe()
	go (&http.Server{Handler: simulatedEditor(id)}).Serve(&oneConn{conn: b, done: make(chan struct{})})
	return a, nil
}

// SimulatedRequest is what the simulated editor reports about a request.
type SimulatedRequest struct {
	Environment string      `json:"environment"`
	Host        string      `json:"host"`
	Path        string      `json:"path"`
	Header      http.Header `json:"header"`
}

func simulatedEditor(id string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seen := SimulatedRequest{Environment: id, Host: r.Host, Path: r.URL.RequestURI(), Header: r.Header}
		if r.URL.Path == "/_simulated/request" {
			w.Header().Set("Content-Type", "application/json")
			json.NewEncoder(w).Encode(seen)
			return
		}
		b, _ := json.MarshalIndent(seen, "", "  ")
		w.Header().Set("Content-Type", "text/html; charset=utf-8")
		fmt.Fprintf(w, `<!doctype html><title>Simulated editor</title>
<body style="font: 14px system-ui; padding: 24px">
<h2>Simulated environment</h2>
<p>This environment runs on a simulated worker, which has no VS Code to serve.
The request reached it through Hangar as:</p>
<pre>%s</pre>`, html.EscapeString(string(b)))
	})
}

// oneConn is a listener that yields one connection, and then nothing until
// that connection closes.
type oneConn struct {
	conn net.Conn
	once sync.Once
	done chan struct{}
}

func (l *oneConn) Accept() (net.Conn, error) {
	var c net.Conn
	l.once.Do(func() { c = &closeNotify{Conn: l.conn, done: l.done} })
	if c != nil {
		return c, nil
	}
	<-l.done
	return nil, net.ErrClosed
}

func (l *oneConn) Close() error   { return nil }
func (l *oneConn) Addr() net.Addr { return l.conn.LocalAddr() }

type closeNotify struct {
	net.Conn
	once sync.Once
	done chan struct{}
}

func (c *closeNotify) Close() error {
	c.once.Do(func() { close(c.done) })
	return c.Conn.Close()
}
