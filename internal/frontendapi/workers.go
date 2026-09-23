package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/workers"
)

func (h *handler) ListWorkers(ctx context.Context, _ ListWorkersRequestObject) (ListWorkersResponseObject, error) {
	list, err := h.workers.List(ctx)
	if err != nil {
		return nil, err
	}
	out := make(ListWorkers200JSONResponse, len(list))
	for i, w := range list {
		out[i] = worker(w)
	}
	return out, nil
}

func (h *handler) RevokeWorker(ctx context.Context, req RevokeWorkerRequestObject) (RevokeWorkerResponseObject, error) {
	err := h.workers.Revoke(ctx, req.ID)
	switch {
	case errors.Is(err, workers.ErrNotFound):
		return RevokeWorker404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return RevokeWorker204Response{}, nil
}

func (h *handler) DeleteWorker(ctx context.Context, req DeleteWorkerRequestObject) (DeleteWorkerResponseObject, error) {
	err := h.workers.Delete(ctx, req.ID)
	switch {
	case errors.Is(err, workers.ErrNotFound):
		return DeleteWorker404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, workers.ErrConflict):
		return DeleteWorker409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteWorker204Response{}, nil
}

func worker(w api.Worker) Worker {
	unknown := w.Unknown
	if unknown == nil {
		unknown = []string{}
	}
	return Worker{
		ID:         w.ID,
		Name:       w.Name,
		Labels:     w.Labels,
		Capacity:   Resources{CPUs: w.Capacity.CPUs, MemoryMiB: w.Capacity.MemoryMiB},
		Allocated:  Resources{CPUs: w.Allocated.CPUs, MemoryMiB: w.Allocated.MemoryMiB},
		Online:     w.Online,
		Revoked:    w.Revoked,
		Unknown:    unknown,
		LastSeenAt: w.LastSeenAt,
		CreatedAt:  w.CreatedAt,
	}
}
