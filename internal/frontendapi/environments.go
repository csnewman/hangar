package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/environments"
)

func (h *handler) ListEnvironments(ctx context.Context, _ ListEnvironmentsRequestObject) (ListEnvironmentsResponseObject, error) {
	list, err := h.envs.List(ctx, principal(ctx))
	if err != nil {
		return nil, err
	}
	out := make(ListEnvironments200JSONResponse, len(list))
	for i, e := range list {
		out[i] = environment(e)
	}
	return out, nil
}

func (h *handler) CreateEnvironment(ctx context.Context, req CreateEnvironmentRequestObject) (CreateEnvironmentResponseObject, error) {
	e, err := h.envs.Create(ctx, principal(ctx), api.CreateEnvironment{
		TemplateID: req.Body.TemplateID,
		Name:       req.Body.Name,
	})
	switch {
	case errors.Is(err, environments.ErrInvalid):
		return CreateEnvironment400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrConflict):
		return CreateEnvironment409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return CreateEnvironment201JSONResponse(environment(e)), nil
}

func (h *handler) GetEnvironment(ctx context.Context, req GetEnvironmentRequestObject) (GetEnvironmentResponseObject, error) {
	e, err := h.envs.Get(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return GetEnvironment404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return GetEnvironment200JSONResponse(environment(e)), nil
}

func (h *handler) StartEnvironment(ctx context.Context, req StartEnvironmentRequestObject) (StartEnvironmentResponseObject, error) {
	e, err := h.setDesired(ctx, req.ID, api.DesiredRunning)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return StartEnvironment404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrConflict):
		return StartEnvironment409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return StartEnvironment200JSONResponse(environment(*e)), nil
}

func (h *handler) StopEnvironment(ctx context.Context, req StopEnvironmentRequestObject) (StopEnvironmentResponseObject, error) {
	e, err := h.setDesired(ctx, req.ID, api.DesiredStopped)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return StopEnvironment404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrConflict):
		return StopEnvironment409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return StopEnvironment200JSONResponse(environment(*e)), nil
}

func (h *handler) SuspendEnvironment(ctx context.Context, req SuspendEnvironmentRequestObject) (SuspendEnvironmentResponseObject, error) {
	e, err := h.setDesired(ctx, req.ID, api.DesiredSuspended)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return SuspendEnvironment404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrConflict):
		return SuspendEnvironment409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return SuspendEnvironment200JSONResponse(environment(*e)), nil
}

func (h *handler) DeleteEnvironment(ctx context.Context, req DeleteEnvironmentRequestObject) (DeleteEnvironmentResponseObject, error) {
	e, err := h.setDesired(ctx, req.ID, api.DesiredDeleted)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return DeleteEnvironment404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	case e == nil:
		return DeleteEnvironment204Response{}, nil
	}
	return DeleteEnvironment200JSONResponse(environment(*e)), nil
}

// setDesired returns the environment as it stands afterwards, or nil if the
// change removed it outright.
func (h *handler) setDesired(ctx context.Context, id string, d api.DesiredState) (*api.Environment, error) {
	p := principal(ctx)
	exists, err := h.envs.SetDesired(ctx, p, id, d)
	if err != nil || !exists {
		return nil, err
	}
	e, err := h.envs.Get(ctx, p, id)
	if err != nil {
		return nil, err
	}
	return &e, nil
}

func environment(e api.Environment) Environment {
	return Environment{
		ID:         e.ID,
		OwnerID:    e.OwnerID,
		Owner:      e.Owner,
		Name:       e.Name,
		TemplateID: optional(e.TemplateID),
		Template:   e.Template,
		Spec:       toSpec(e.Spec),
		Image:      e.Image,
		CPUs:       e.CPUs,
		MemoryMiB:  e.MemoryMiB,
		Desired:    DesiredState(e.Desired),
		Phase:      Phase(e.Phase),
		Reason:     optional(e.Reason),
		WorkerID:   optional(e.WorkerID),
		Worker:     optional(e.Worker),
		Stats:      environmentStats(e.Stats),
		Progress:   startProgress(e.Progress),
		CreatedAt:  e.CreatedAt,
		UpdatedAt:  e.UpdatedAt,
	}
}

func startProgress(p *api.Progress) *StartProgress {
	if p == nil {
		return nil
	}
	out := &StartProgress{Step: StartProgressStep(p.Step)}
	if p.Total > 0 {
		unit := StartProgressUnit(p.Unit)
		out.Done, out.Total, out.Unit = &p.Done, &p.Total, &unit
		if p.Rate > 0 {
			rate := float32(p.Rate)
			out.Rate = &rate
		}
	}
	return out
}

func environmentStats(s *api.EnvironmentStats) *EnvironmentStats {
	if s == nil {
		return nil
	}
	return &EnvironmentStats{
		CPUPercent:     float32(s.CPUPercent),
		MemoryUsedMiB:  s.MemoryUsedMiB,
		MemoryTotalMiB: s.MemoryTotalMiB,
		DiskUsedBytes:  s.DiskUsedBytes,
		DiskReadBps:    float32(s.DiskReadBps),
		DiskWriteBps:   float32(s.DiskWriteBps),
		NetRxBps:       float32(s.NetRxBps),
		NetTxBps:       float32(s.NetTxBps),
	}
}
