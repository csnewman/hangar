package frontendapi

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"strconv"
	"time"

	"github.com/coder/websocket"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/terminal"
	"github.com/csnewman/hangar/internal/tunnel"
)

// Tunnels opens streams to workers.
type Tunnels interface {
	Open(workerID string, h tunnel.Header) (net.Conn, error)
}

var (
	errNotRunning  = errors.New("the environment is not running")
	errUnreachable = errors.New("the environment's worker cannot be reached from this server just now")
)

// terminalStream opens a stream to the sessions of an environment the caller
// may reach, and sends it req. The reply is read and returned; for an attach
// the stream then carries frames.
func (h *handler) terminalStream(ctx context.Context, id string, req terminal.Request) (net.Conn, *bufio.Reader, terminal.Reply, error) {
	env, err := h.envs.Get(ctx, principal(ctx), id)
	if err != nil {
		return nil, nil, terminal.Reply{}, err
	}
	if env.Phase != api.PhaseRunning || env.WorkerID == "" {
		return nil, nil, terminal.Reply{}, errNotRunning
	}
	if h.tunnels == nil {
		return nil, nil, terminal.Reply{}, errUnreachable
	}
	stream, err := h.tunnels.Open(env.WorkerID, tunnel.Header{Kind: tunnel.KindTerminal, Environment: env.ID})
	if err != nil {
		h.log.Warn("opening a terminal stream", "environment", env.ID, "err", err)
		return nil, nil, terminal.Reply{}, errUnreachable
	}
	if req.Op == terminal.OpAttach && req.Session == "" && req.Dir == "" {
		req.Dir = env.Spec.EditorPath
	}
	b, _ := json.Marshal(req)
	stream.SetDeadline(time.Now().Add(30 * time.Second))
	if _, err := stream.Write(append(b, '\n')); err != nil {
		stream.Close()
		return nil, nil, terminal.Reply{}, errUnreachable
	}
	br := bufio.NewReader(stream)
	line, err := br.ReadBytes('\n')
	if err != nil {
		stream.Close()
		return nil, nil, terminal.Reply{}, errUnreachable
	}
	stream.SetDeadline(time.Time{})
	var reply terminal.Reply
	if err := json.Unmarshal(line, &reply); err != nil {
		stream.Close()
		return nil, nil, terminal.Reply{}, fmt.Errorf("reading the terminal's reply: %w", err)
	}
	return stream, br, reply, nil
}

func (h *handler) ListTerminals(ctx context.Context, req ListTerminalsRequestObject) (ListTerminalsResponseObject, error) {
	stream, _, reply, err := h.terminalStream(ctx, req.ID, terminal.Request{Op: terminal.OpList})
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return ListTerminals404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, errNotRunning):
		return ListTerminals409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, errUnreachable):
		return ListTerminals503JSONResponse{UnavailableJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	stream.Close()
	out := make(ListTerminals200JSONResponse, len(reply.Sessions))
	for i, s := range reply.Sessions {
		out[i] = TerminalSession{ID: s.ID, Title: s.Title, CreatedAt: s.Created, Clients: s.Clients,
			Cols: int(s.Cols), Rows: int(s.Rows)}
	}
	return out, nil
}

func (h *handler) CloseTerminal(ctx context.Context, req CloseTerminalRequestObject) (CloseTerminalResponseObject, error) {
	stream, _, reply, err := h.terminalStream(ctx, req.ID, terminal.Request{Op: terminal.OpClose, Session: req.Session})
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return CloseTerminal404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, errNotRunning):
		return CloseTerminal409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, errUnreachable):
		return CloseTerminal503JSONResponse{UnavailableJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	stream.Close()
	if reply.Err != "" {
		return CloseTerminal404JSONResponse{NotFoundJSONResponse{Error: reply.Err}}, nil
	}
	return CloseTerminal204Response{}, nil
}

// terminalSocket attaches a browser to a terminal session over a WebSocket:
// GET /api/frontend/environments/{id}/terminal?session=&cols=&rows=
//
// The first message to the browser is the attach's JSON reply, as text.
// After it every message is binary, one frame each: the frame type byte
// followed by its payload, in both directions. The length the stream's
// frames carry is the message's own, so the browser never sees it.
//
// A WebSocket is not covered by the spec, so this checks for itself what the
// spec would: that the caller is signed in and may reach the environment.
func (h *handler) terminalSocket(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	if !sessionFrom(ctx).ok {
		writeError(w, http.StatusUnauthorized, "not signed in")
		return
	}
	q := r.URL.Query()
	cols, _ := strconv.Atoi(q.Get("cols"))
	rows, _ := strconv.Atoi(q.Get("rows"))
	req := terminal.Request{
		Op:      terminal.OpAttach,
		Session: q.Get("session"),
		Cols:    uint16(min(max(cols, 0), 1000)),
		Rows:    uint16(min(max(rows, 0), 1000)),
	}
	stream, br, reply, err := h.terminalStream(ctx, r.PathValue("id"), req)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		writeError(w, http.StatusNotFound, err.Error())
		return
	case errors.Is(err, errNotRunning):
		writeError(w, http.StatusConflict, err.Error())
		return
	case errors.Is(err, errUnreachable):
		writeError(w, http.StatusServiceUnavailable, err.Error())
		return
	case err != nil:
		h.log.Error("opening a terminal", "err", err)
		writeError(w, http.StatusInternalServerError, "internal error")
		return
	}
	defer stream.Close()

	// Accept refuses a request whose Origin is not this host, which is what
	// keeps another site from opening a terminal with the user's cookie.
	c, err := websocket.Accept(w, r, nil)
	if err != nil {
		return
	}
	defer c.CloseNow()
	c.SetReadLimit(terminal.MaxFrame + 1)

	first, _ := json.Marshal(reply)
	if err := c.Write(ctx, websocket.MessageText, first); err != nil || reply.Err != "" {
		c.Close(websocket.StatusNormalClosure, "")
		return
	}

	ctx, cancel := context.WithCancel(context.WithoutCancel(ctx))
	defer cancel()
	go func() {
		defer cancel()
		for {
			typ, payload, err := terminal.ReadFrame(br)
			if err != nil {
				return
			}
			msg := append([]byte{typ}, payload...)
			if err := c.Write(ctx, websocket.MessageBinary, msg); err != nil {
				return
			}
			if typ == terminal.FrameExit {
				c.Close(websocket.StatusNormalClosure, "the shell exited")
				return
			}
		}
	}()
	for {
		kind, msg, err := c.Read(ctx)
		if err != nil {
			return
		}
		if kind != websocket.MessageBinary || len(msg) == 0 {
			continue
		}
		switch msg[0] {
		case terminal.FrameInput, terminal.FrameResize:
			if terminal.WriteFrame(stream, msg[0], msg[1:]) != nil {
				return
			}
		}
	}
}
