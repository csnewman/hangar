package clientapi

import (
	"context"
	"errors"
	"time"

	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/logs"
	"github.com/csnewman/hangar/internal/tunnel"
)

var errNotPlaced = errors.New("the environment has not been placed on a worker yet, so it has no logs")

// GetEnvironmentLog reads part of one of an environment's logs from its
// worker, which keeps them whatever the environment is doing.
func (h *handler) GetEnvironmentLog(ctx context.Context, req GetEnvironmentLogRequestObject) (GetEnvironmentLogResponseObject, error) {
	env, err := h.envs.Get(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return GetEnvironmentLog404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	if env.WorkerID == "" {
		return GetEnvironmentLog409JSONResponse{ConflictJSONResponse{Error: errNotPlaced.Error()}}, nil
	}
	if h.tunnels == nil {
		return GetEnvironmentLog503JSONResponse{UnavailableJSONResponse{Error: errUnreachable.Error()}}, nil
	}
	stream, err := h.tunnels.Open(env.WorkerID, tunnel.Header{Kind: tunnel.KindLogs, Environment: env.ID})
	if err != nil {
		h.log.Warn("opening a logs stream", "environment", env.ID, "err", err)
		return GetEnvironmentLog503JSONResponse{UnavailableJSONResponse{Error: errUnreachable.Error()}}, nil
	}
	defer stream.Close()
	stream.SetDeadline(time.Now().Add(15 * time.Second))
	offset := int64(-1)
	if req.Params.Offset != nil {
		offset = *req.Params.Offset
	}
	reply, err := logs.Ask(stream, logs.Request{Environment: env.ID, Log: string(req.Log), Offset: offset})
	switch {
	case errors.Is(err, logs.ErrNoReply):
		return GetEnvironmentLog503JSONResponse{UnavailableJSONResponse{Error: errUnreachable.Error()}}, nil
	case err != nil:
		return GetEnvironmentLog503JSONResponse{UnavailableJSONResponse{Error: err.Error()}}, nil
	}
	part := LogPart{Start: reply.Start, Next: reply.Start + int64(len(reply.Data)), Size: reply.Size, Text: string(reply.Data)}
	if reply.Missing {
		part.Missing = &reply.Missing
	}
	return GetEnvironmentLog200JSONResponse(part), nil
}
