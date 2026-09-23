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

func (h *handler) GetWorker(ctx context.Context, req GetWorkerRequestObject) (GetWorkerResponseObject, error) {
	w, err := h.workers.Get(ctx, req.ID)
	switch {
	case errors.Is(err, workers.ErrNotFound):
		return GetWorker404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return GetWorker200JSONResponse(worker(w)), nil
}

func (h *handler) RemoveWorkerImage(ctx context.Context, req RemoveWorkerImageRequestObject) (RemoveWorkerImageResponseObject, error) {
	err := h.workers.RemoveImage(ctx, req.ID, req.Body.Ref)
	switch {
	case errors.Is(err, workers.ErrNotFound), errors.Is(err, workers.ErrNoImage):
		return RemoveWorkerImage404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, workers.ErrInUse):
		return RemoveWorkerImage409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return RemoveWorkerImage202Response{}, nil
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
		ID:              w.ID,
		Name:            w.Name,
		Labels:          w.Labels,
		Capacity:        Resources{CPUs: w.Capacity.CPUs, MemoryMiB: w.Capacity.MemoryMiB},
		Allocated:       Resources{CPUs: w.Allocated.CPUs, MemoryMiB: w.Allocated.MemoryMiB},
		Online:          w.Online,
		Revoked:         w.Revoked,
		Unknown:         unknown,
		LastSeenAt:      w.LastSeenAt,
		CreatedAt:       w.CreatedAt,
		Stats:           workerStats(w.Stats),
		Images:          localImages(w.Images),
		PendingRemovals: nonNilStrings(w.PendingRemovals),
	}
}

func workerStats(s *api.WorkerStats) *WorkerStats {
	if s == nil {
		return nil
	}
	return &WorkerStats{
		CPUPercent:     float32(s.CPUPercent),
		Load1:          float32(s.Load1),
		MemoryUsedMiB:  s.MemoryUsedMiB,
		MemoryTotalMiB: s.MemoryTotalMiB,
		DiskUsedBytes:  s.DiskUsedBytes,
		DiskTotalBytes: s.DiskTotalBytes,
	}
}

func localImages(in []api.LocalImage) []LocalImage {
	out := make([]LocalImage, len(in))
	for i, img := range in {
		out[i] = LocalImage{
			Ref:          img.Ref,
			SizeBytes:    img.SizeBytes,
			State:        LocalImageState(img.State),
			Environments: nonNilStrings(img.Environments),
		}
	}
	return out
}

func nonNilStrings(s []string) []string {
	if s == nil {
		return []string{}
	}
	return s
}
