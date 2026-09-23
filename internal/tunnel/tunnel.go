// Package tunnel carries streams from the control plane to a worker over a
// connection the worker opens.
//
// A worker dials out, never in: it may be behind NAT or in a pod with no
// ingress, and the control plane has no route to it. So the worker opens one
// WebSocket to the server and holds it; the server multiplexes streams back
// over it with yamux, each one the server's to open. Everything interactive
// about an environment, such as a terminal, travels this way, as a stream per
// use.
//
// Each stream starts with one JSON line, a Header, saying what it is for.
// What follows is that use's own protocol, which the tunnel does not read.
//
// A worker's tunnel ends at whichever server replica it reached. A replica
// can only open streams over the tunnels it holds; with several replicas a
// request for a worker whose tunnel is elsewhere is refused, not forwarded.
package tunnel

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/coder/websocket"
	"github.com/hashicorp/yamux"
)

// Kinds of stream.
const (
	KindTerminal = "terminal"
	// KindEditor carries one HTTP connection to the environment's editor.
	KindEditor = "editor"
	// KindDesktop carries one VNC (RFB) connection to the environment's
	// desktop.
	KindDesktop = "desktop"
)

// Header opens a stream.
type Header struct {
	Kind        string `json:"kind"`
	Environment string `json:"environment"`
}

// ErrNoTunnel is returned for a worker with no tunnel to this server.
var ErrNoTunnel = errors.New("the worker is not connected to this server")

func muxConfig() *yamux.Config {
	c := yamux.DefaultConfig()
	// Frequent enough to keep the connection through a proxy's idle
	// timeout, and to notice a dead peer within a minute.
	c.KeepAliveInterval = 20 * time.Second
	c.ConnectionWriteTimeout = 30 * time.Second
	c.LogOutput = io.Discard
	return c
}

// Registry holds the tunnels workers have opened to this server.
type Registry struct {
	log *slog.Logger

	mu      sync.Mutex
	tunnels map[string]*yamux.Session
}

func NewRegistry(log *slog.Logger) *Registry {
	return &Registry{log: log, tunnels: map[string]*yamux.Session{}}
}

// Accept takes a worker's tunnel on an HTTP request and holds it until it
// closes. A second tunnel from the same worker replaces the first.
func (r *Registry) Accept(w http.ResponseWriter, req *http.Request, workerID string) {
	// A worker is not a browser, so there is no Origin to check; its
	// credential has already been.
	c, err := websocket.Accept(w, req, &websocket.AcceptOptions{InsecureSkipVerify: true})
	if err != nil {
		return
	}
	c.SetReadLimit(-1)
	ctx := context.WithoutCancel(req.Context())
	sess, err := yamux.Client(websocket.NetConn(ctx, c, websocket.MessageBinary), muxConfig())
	if err != nil {
		c.Close(websocket.StatusInternalError, "")
		return
	}

	r.mu.Lock()
	if old, ok := r.tunnels[workerID]; ok {
		old.Close()
	}
	r.tunnels[workerID] = sess
	r.mu.Unlock()
	r.log.Info("worker tunnel opened", "worker", workerID)

	<-sess.CloseChan()
	r.mu.Lock()
	if r.tunnels[workerID] == sess {
		delete(r.tunnels, workerID)
	}
	r.mu.Unlock()
	r.log.Info("worker tunnel closed", "worker", workerID)
}

// Open starts a stream to a worker. The caller owns the stream and closes
// it.
func (r *Registry) Open(workerID string, h Header) (net.Conn, error) {
	r.mu.Lock()
	sess, ok := r.tunnels[workerID]
	r.mu.Unlock()
	if !ok || sess.IsClosed() {
		return nil, ErrNoTunnel
	}
	stream, err := sess.Open()
	if err != nil {
		return nil, fmt.Errorf("opening a stream to the worker: %w", err)
	}
	b, _ := json.Marshal(h)
	if _, err := stream.Write(append(b, '\n')); err != nil {
		stream.Close()
		return nil, err
	}
	return stream, nil
}

// Handler serves one stream on the worker. It owns the stream, and is given
// a reader positioned after the header.
type Handler func(h Header, r io.Reader, stream net.Conn)

// Serve holds a tunnel open to the server at base, redialling whenever it
// drops, until ctx ends or the server refuses the credential.
func Serve(ctx context.Context, base, credential string, handle Handler, log *slog.Logger) error {
	url := strings.Replace(strings.TrimRight(base, "/"), "http", "ws", 1) + "/api/worker/v1/tunnel"
	backoff := time.Second
	for {
		err := serveOnce(ctx, url, credential, handle)
		if ctx.Err() != nil {
			return nil
		}
		if websocket.CloseStatus(err) == websocket.StatusPolicyViolation || errors.Is(err, errRefused) {
			return err
		}
		log.Warn("worker tunnel dropped, redialling", "err", err, "after", backoff)
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(backoff):
		}
		backoff = min(backoff*2, 30*time.Second)
	}
}

var errRefused = errors.New("the server refused the worker's tunnel")

func serveOnce(ctx context.Context, url, credential string, handle Handler) error {
	c, resp, err := websocket.Dial(ctx, url, &websocket.DialOptions{
		HTTPHeader: http.Header{"Authorization": {"Bearer " + credential}},
	})
	if err != nil {
		if resp != nil && resp.StatusCode == http.StatusUnauthorized {
			return errRefused
		}
		return err
	}
	c.SetReadLimit(-1)
	sess, err := yamux.Server(websocket.NetConn(ctx, c, websocket.MessageBinary), muxConfig())
	if err != nil {
		c.Close(websocket.StatusInternalError, "")
		return err
	}
	defer sess.Close()
	for {
		stream, err := sess.Accept()
		if err != nil {
			return err
		}
		go func() {
			br := bufio.NewReader(stream)
			line, err := br.ReadBytes('\n')
			if err != nil {
				stream.Close()
				return
			}
			var h Header
			if err := json.Unmarshal(line, &h); err != nil {
				stream.Close()
				return
			}
			handle(h, br, stream)
		}()
	}
}
