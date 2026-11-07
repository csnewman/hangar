package frontendapi

import (
	"encoding/json"
	"errors"
	"net/http"

	"github.com/coder/websocket"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/code"
	"github.com/csnewman/hangar/internal/codews"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/tunnel"
)

// codeSocket carries the Code tab's connection to the environment's Code
// service over a WebSocket: GET /api/frontend/environments/{id}/code.
//
// Each binary message is one of the service's frames, whose length the
// stream carries as a four-byte prefix and the WebSocket carries as the
// message's own; the server reads nothing else of them.
//
// A WebSocket is not covered by the spec, so this checks for itself what the
// spec would: that the caller is signed in and may reach the environment.
func (h *handler) codeSocket(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	if !sessionFrom(ctx).ok {
		writeError(w, http.StatusUnauthorized, "not signed in")
		return
	}
	env, err := h.envs.Get(ctx, principal(ctx), r.PathValue("id"))
	switch {
	case errors.Is(err, environments.ErrNotFound):
		writeError(w, http.StatusNotFound, err.Error())
		return
	case err != nil:
		h.log.Error("opening the code service", "err", err)
		writeError(w, http.StatusInternalServerError, "internal error")
		return
	}
	if env.Phase != api.PhaseRunning || env.WorkerID == "" {
		writeError(w, http.StatusConflict, errNotRunning.Error())
		return
	}
	if h.tunnels == nil {
		writeError(w, http.StatusServiceUnavailable, errUnreachable.Error())
		return
	}
	h.access(ctx, env, "environment.open_code", nil)
	stream, err := h.tunnels.Open(env.WorkerID, tunnel.Header{Kind: tunnel.KindCode, Environment: env.ID})
	if err != nil {
		h.log.Warn("opening a code stream", "environment", env.ID, "err", err)
		writeError(w, http.StatusServiceUnavailable, errUnreachable.Error())
		return
	}
	defer stream.Close()
	req, _ := json.Marshal(code.Request{Root: env.Spec.EditorPath})
	if _, err := stream.Write(append(req, '\n')); err != nil {
		writeError(w, http.StatusServiceUnavailable, errUnreachable.Error())
		return
	}

	// Accept refuses a request whose Origin is not this host.
	c, err := websocket.Accept(w, r, nil)
	if err != nil {
		return
	}
	defer c.CloseNow()
	codews.Bridge(ctx, c, stream, h.socketPing)
}
