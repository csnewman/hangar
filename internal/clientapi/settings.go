package clientapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/packs"
)

// UpdateEnvironmentSettings changes an environment's machine: a stopped
// one's at its next start, a running one's memory as it runs.
func (h *handler) UpdateEnvironmentSettings(ctx context.Context, req UpdateEnvironmentSettingsRequestObject) (UpdateEnvironmentSettingsResponseObject, error) {
	b := req.Body
	e, err := h.envs.UpdateSettings(ctx, principal(ctx), req.ID, environments.Settings{
		CPUs: b.CPUs, MemoryMiB: b.MemoryMiB,
		Display: api.Display(b.Display), GPU: api.GPU(b.Gpu),
		DAX: b.Dax != nil && *b.Dax, SwapMiB: b.SwapMiB})
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return UpdateEnvironmentSettings404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrInvalid):
		return UpdateEnvironmentSettings400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrConflict):
		return UpdateEnvironmentSettings409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return UpdateEnvironmentSettings200JSONResponse{EnvironmentJSONResponse(environment(e))}, nil
}

// ListWorkerSizes is how big an environment may be: each online worker's
// capacity, not saying which is which.
func (h *handler) ListWorkerSizes(ctx context.Context, _ ListWorkerSizesRequestObject) (ListWorkerSizesResponseObject, error) {
	if h.workers == nil {
		return ListWorkerSizes200JSONResponse{}, nil
	}
	sizes, err := h.workers.Sizes(ctx)
	if err != nil {
		return nil, err
	}
	out := make(ListWorkerSizes200JSONResponse, len(sizes))
	for i, s := range sizes {
		out[i] = Resources{CPUs: s.CPUs, MemoryMiB: s.MemoryMiB}
	}
	return out, nil
}

// ListEnvironmentPacks is an environment's packs, each with the copy it
// uses and the copies the user could choose instead.
func (h *handler) ListEnvironmentPacks(ctx context.Context, req ListEnvironmentPacksRequestObject) (ListEnvironmentPacksResponseObject, error) {
	p := principal(ctx)
	if _, err := h.envs.Get(ctx, p, req.ID); errors.Is(err, environments.ErrNotFound) {
		return ListEnvironmentPacks404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	} else if err != nil {
		return nil, err
	}
	if h.packs == nil {
		return ListEnvironmentPacks200JSONResponse{}, nil
	}
	as, err := h.packs.EnvironmentPacks(ctx, req.ID)
	if errors.Is(err, packs.ErrNotFound) {
		return ListEnvironmentPacks404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	} else if err != nil {
		return nil, err
	}
	out := make(ListEnvironmentPacks200JSONResponse, 0, len(as))
	for _, a := range as {
		cs, err := h.packs.Copies(ctx, p, a.Pack.ID)
		if err != nil && !errors.Is(err, packs.ErrNotFound) {
			return nil, err
		}
		copies := make([]PackCopy, 0, len(cs))
		for _, c := range cs {
			copies = append(copies, packCopy(c))
		}
		out = append(out, EnvironmentPack{
			ID: a.Pack.ID, Name: a.Pack.Name, Description: a.Pack.Description,
			Owner: optional(a.Pack.Owner), Team: optional(a.Pack.Team), Builtin: a.Pack.Builtin,
			Copy: packCopy(a.Copy), From: string(a.From), Listed: a.Listed, Copies: copies,
		})
	}
	return out, nil
}

func packCopy(c packs.Copy) PackCopy {
	return PackCopy{ID: c.ID, Name: c.Name, Owner: optional(c.Owner), Team: optional(c.Team), Personal: c.Personal}
}

// ChooseEnvironmentCopy chooses which copy of a pack an environment uses,
// or undoes the choice.
func (h *handler) ChooseEnvironmentCopy(ctx context.Context, req ChooseEnvironmentCopyRequestObject) (ChooseEnvironmentCopyResponseObject, error) {
	p := principal(ctx)
	if _, err := h.envs.Get(ctx, p, req.ID); errors.Is(err, environments.ErrNotFound) {
		return ChooseEnvironmentCopy404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	} else if err != nil {
		return nil, err
	}
	if h.packs == nil {
		return ChooseEnvironmentCopy404JSONResponse{NotFoundJSONResponse{Error: "no such pack"}}, nil
	}
	copy := ""
	if req.Body != nil && req.Body.CopyID != nil {
		copy = *req.Body.CopyID
	}
	err := h.packs.ChooseCopy(ctx, p, req.ID, req.Pack, copy)
	switch {
	case errors.Is(err, packs.ErrNotFound):
		return ChooseEnvironmentCopy404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, packs.ErrInvalid):
		return ChooseEnvironmentCopy400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, packs.ErrForbidden):
		return ChooseEnvironmentCopy403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return ChooseEnvironmentCopy204Response{}, nil
}

// ChangeEnvironmentImage upgrades an environment's image, rolls it back,
// keeps the upgrade, or cancels a change not yet made.
func (h *handler) ChangeEnvironmentImage(ctx context.Context, req ChangeEnvironmentImageRequestObject) (ChangeEnvironmentImageResponseObject, error) {
	p := principal(ctx)
	var e api.Environment
	var err error
	switch req.Change {
	case "upgrade":
		force := req.Body != nil && req.Body.Force != nil && *req.Body.Force
		e, err = h.envs.UpgradeImage(ctx, p, req.ID, force)
	case "rollback":
		e, err = h.envs.RollbackImage(ctx, p, req.ID)
	case "keep":
		e, err = h.envs.KeepImage(ctx, p, req.ID)
	case "cancel":
		e, err = h.envs.CancelImageChange(ctx, p, req.ID)
	default:
		return ChangeEnvironmentImage400JSONResponse{InvalidJSONResponse{
			Error: "change is upgrade, rollback, keep or cancel"}}, nil
	}
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return ChangeEnvironmentImage404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrInvalid):
		return ChangeEnvironmentImage400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrConflict):
		return ChangeEnvironmentImage409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return ChangeEnvironmentImage200JSONResponse{EnvironmentJSONResponse(environment(e))}, nil
}
