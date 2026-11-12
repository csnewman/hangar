package clientapi

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/desktop"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/tunnel"
	"github.com/csnewman/hangar/internal/users"
)

// desktopTarget is an environment the caller may reach whose desktop can be
// served, or the status and words saying why not.
func (h *handler) desktopTarget(ctx context.Context, id string) (api.Environment, int, string) {
	env, err := h.envs.Get(ctx, principal(ctx), id)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return env, http.StatusNotFound, err.Error()
	case err != nil:
		h.log.Error("reaching a desktop", "err", err)
		return env, http.StatusInternalServerError, "internal error"
	case env.Phase != api.PhaseRunning || env.WorkerID == "":
		return env, http.StatusConflict, errNotRunning.Error()
	case env.Spec.Display == api.DisplayNone:
		return env, http.StatusConflict, "the environment is headless: it has no desktop"
	case h.tunnels == nil:
		return env, http.StatusServiceUnavailable, errUnreachable.Error()
	}
	return env, 0, ""
}

// desktopSocket carries a VNC viewer's connection to an environment's
// desktop: GET /api/v1/environments/{id}/desktop. Every binary message is
// RFB bytes, both ways; the server reads none of them.
func (h *handler) desktopSocket(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	if _, ok := ctx.Value(principalKey{}).(users.Principal); !ok {
		writeError(w, http.StatusUnauthorized, errUnauthorized.Error())
		return
	}
	env, code, msg := h.desktopTarget(ctx, r.PathValue("id"))
	if code != 0 {
		writeError(w, code, msg)
		return
	}
	h.access(ctx, env, "environment.open_desktop", nil)
	stream, err := h.tunnels.Open(env.WorkerID, tunnel.Header{Kind: tunnel.KindDesktop, Environment: env.ID})
	if err != nil {
		h.log.Warn("opening a desktop stream", "environment", env.ID, "err", err)
		writeError(w, http.StatusServiceUnavailable, errUnreachable.Error())
		return
	}
	defer stream.Close()
	req, _ := json.Marshal(desktop.Request{Op: desktop.OpVNC})
	if _, err := stream.Write(append(req, '\n')); err != nil {
		writeError(w, http.StatusServiceUnavailable, errUnreachable.Error())
		return
	}
	c, err := websocket.Accept(w, r, nil)
	if err != nil {
		return
	}
	defer c.CloseNow()
	c.SetReadLimit(-1)
	ctx, cancel := context.WithCancel(context.WithoutCancel(ctx))
	defer cancel()
	conn := websocket.NetConn(ctx, c, websocket.MessageBinary)
	var sent atomic.Int64
	done := make(chan struct{}, 2)
	go func() {
		n, _ := io.Copy(conn, stream)
		sent.Store(n)
		done <- struct{}{}
	}()
	go func() {
		defer func() { done <- struct{}{} }()
		for {
			kind, msg, err := c.Read(ctx)
			if err != nil {
				return
			}
			if kind != websocket.MessageBinary {
				continue
			}
			if _, err := stream.Write(msg); err != nil {
				return
			}
		}
	}()
	<-done
	if sent.Load() == 0 {
		c.Close(websocket.StatusTryAgainLater, "the environment's desktop is not running")
		return
	}
	c.Close(websocket.StatusNormalClosure, "")
}

func (h *handler) ResizeDesktop(ctx context.Context, req ResizeDesktopRequestObject) (ResizeDesktopResponseObject, error) {
	env, code, msg := h.desktopTarget(ctx, req.ID)
	switch code {
	case 0:
	case http.StatusNotFound:
		return ResizeDesktop404JSONResponse{NotFoundJSONResponse{Error: msg}}, nil
	case http.StatusConflict:
		return ResizeDesktop409JSONResponse{ConflictJSONResponse{Error: msg}}, nil
	case http.StatusServiceUnavailable:
		return ResizeDesktop503JSONResponse{UnavailableJSONResponse{Error: msg}}, nil
	default:
		return nil, errors.New(msg)
	}
	stream, err := h.tunnels.Open(env.WorkerID, tunnel.Header{Kind: tunnel.KindDesktop, Environment: env.ID})
	if err != nil {
		return ResizeDesktop503JSONResponse{UnavailableJSONResponse{Error: errUnreachable.Error()}}, nil
	}
	defer stream.Close()
	stream.SetDeadline(time.Now().Add(30 * time.Second))
	b, _ := json.Marshal(desktop.Request{Op: desktop.OpResize, Width: req.Body.Width, Height: req.Body.Height})
	if _, err := stream.Write(append(b, '\n')); err != nil {
		return ResizeDesktop503JSONResponse{UnavailableJSONResponse{Error: errUnreachable.Error()}}, nil
	}
	line, err := bufio.NewReader(stream).ReadBytes('\n')
	if err != nil {
		return ResizeDesktop503JSONResponse{UnavailableJSONResponse{Error: errUnreachable.Error()}}, nil
	}
	var reply desktop.Reply
	if err := json.Unmarshal(line, &reply); err != nil {
		return nil, err
	}
	if reply.Err != "" {
		return ResizeDesktop409JSONResponse{ConflictJSONResponse{Error: reply.Err}}, nil
	}
	return ResizeDesktop204Response{}, nil
}
