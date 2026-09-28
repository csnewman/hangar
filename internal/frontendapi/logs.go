package frontendapi

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"time"

	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/logs"
	"github.com/csnewman/hangar/internal/tunnel"
	"github.com/csnewman/hangar/internal/workers"
)

var errNotPlaced = errors.New("the environment has not been placed on a worker yet, so it has no logs")

// readLog asks a worker for part of one of its logs.
func (h *handler) readLog(workerID string, req logs.Request) (LogPart, error) {
	if h.tunnels == nil {
		return LogPart{}, errUnreachable
	}
	stream, err := h.tunnels.Open(workerID, tunnel.Header{Kind: tunnel.KindLogs, Environment: req.Environment})
	if err != nil {
		h.log.Warn("opening a logs stream", "worker", workerID, "err", err)
		return LogPart{}, errUnreachable
	}
	defer stream.Close()
	stream.SetDeadline(time.Now().Add(15 * time.Second))
	b, _ := json.Marshal(req)
	if _, err := stream.Write(append(b, '\n')); err != nil {
		return LogPart{}, errUnreachable
	}
	line, err := bufio.NewReader(stream).ReadBytes('\n')
	if err != nil {
		// A worker that serves no logs closes the stream unanswered.
		return LogPart{}, errUnreachable
	}
	var reply logs.Reply
	if err := json.Unmarshal(line, &reply); err != nil {
		return LogPart{}, err
	}
	if reply.Error != "" {
		return LogPart{}, errors.New(reply.Error)
	}
	return LogPart{Start: reply.Start, Next: reply.Start + int64(len(reply.Data)), Size: reply.Size,
		Text: string(reply.Data), Missing: optionalBool(reply.Missing)}, nil
}

func offsetOf(p *int64) int64 {
	if p == nil {
		return -1
	}
	return *p
}

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
	part, err := h.readLog(env.WorkerID, logs.Request{Environment: env.ID, Log: string(req.Log), Offset: offsetOf(req.Params.Offset)})
	if err != nil {
		return GetEnvironmentLog503JSONResponse{UnavailableJSONResponse{Error: err.Error()}}, nil
	}
	return GetEnvironmentLog200JSONResponse(part), nil
}

func (h *handler) GetWorkerLog(ctx context.Context, req GetWorkerLogRequestObject) (GetWorkerLogResponseObject, error) {
	if _, err := h.workers.Get(ctx, req.ID); errors.Is(err, workers.ErrNotFound) {
		return GetWorkerLog404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	} else if err != nil {
		return nil, err
	}
	part, err := h.readLog(req.ID, logs.Request{Log: logs.Worker, Offset: offsetOf(req.Params.Offset)})
	if err != nil {
		return GetWorkerLog503JSONResponse{UnavailableJSONResponse{Error: err.Error()}}, nil
	}
	return GetWorkerLog200JSONResponse(part), nil
}
