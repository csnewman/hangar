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
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"sync"
	"time"

	"github.com/coder/websocket"
	"github.com/hashicorp/yamux"
)

// Kinds of stream.
const (
	KindTerminal = "terminal"
	// KindDesktop carries one VNC (RFB) connection to the environment's
	// desktop.
	KindDesktop = "desktop"
	// KindCode carries one connection to the environment's Code tab
	// service.
	KindCode = "code"
	// KindProfile carries the environment's profile session, which the
	// server holds open while the environment runs.
	KindProfile = "profile"
	// KindProcesses carries requests about the processes running in the
	// environment.
	KindProcesses = "processes"
	// KindSSH carries one SSH connection from the gateway to the
	// environment's SSH server.
	KindSSH = "ssh"
	// KindPorts carries one connection to the environment's port forwarder
	// (package guestport), which the control plane asks for a port: one of
	// the environment's own web servers.
	KindPorts = "ports"
	// KindLogs carries one request for part of a log the worker holds
	// (package logs), and its reply: the worker answers it itself.
	KindLogs = "logs"
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

// Connected lists the workers whose tunnels this server holds.
func (r *Registry) Connected() []string {
	r.mu.Lock()
	defer r.mu.Unlock()
	out := make([]string, 0, len(r.tunnels))
	for id, sess := range r.tunnels {
		if !sess.IsClosed() {
			out = append(out, id)
		}
	}
	return out
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
