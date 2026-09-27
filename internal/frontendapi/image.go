package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/environments"
)

func imageUpdate(u *api.ImageUpdate) *ImageUpdate {
	if u == nil {
		return nil
	}
	out := &ImageUpdate{Digest: u.Digest, Checked: u.Checked, Error: optional(u.Error),
		Packages: optionalBool(u.Packages)}
	if len(u.Conflicts) > 0 {
		c := u.Conflicts
		out.Conflicts = &c
	}
	if u.MoreConflicts > 0 {
		n := u.MoreConflicts
		out.MoreConflicts = &n
	}
	return out
}

func imageRollback(r *api.ImageRollback) *ImageRollback {
	if r == nil {
		return nil
	}
	return &ImageRollback{Digest: r.Digest, At: r.At, SizeBytes: r.SizeBytes}
}

func imageChange(c string) *EnvironmentImageChange {
	if c == "" {
		return nil
	}
	out := EnvironmentImageChange(c)
	return &out
}

// imageResult sorts a change to an environment's image into the responses
// every such operation shares.
func imageResult(e api.Environment, err error) (Environment, int, string, error) {
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return Environment{}, 404, err.Error(), nil
	case errors.Is(err, environments.ErrConflict):
		return Environment{}, 409, err.Error(), nil
	case err != nil:
		return Environment{}, 0, "", err
	}
	return environment(e), 200, "", nil
}

func (h *handler) UpgradeEnvironmentImage(ctx context.Context, req UpgradeEnvironmentImageRequestObject) (UpgradeEnvironmentImageResponseObject, error) {
	force := req.Body != nil && req.Body.Force != nil && *req.Body.Force
	e, code, msg, err := imageResult(h.envs.UpgradeImage(ctx, principal(ctx), req.ID, force))
	switch code {
	case 404:
		return UpgradeEnvironmentImage404JSONResponse{NotFoundJSONResponse{Error: msg}}, nil
	case 409:
		return UpgradeEnvironmentImage409JSONResponse{ConflictJSONResponse{Error: msg}}, nil
	}
	if err != nil {
		return nil, err
	}
	return UpgradeEnvironmentImage200JSONResponse(e), nil
}

func (h *handler) RollbackEnvironmentImage(ctx context.Context, req RollbackEnvironmentImageRequestObject) (RollbackEnvironmentImageResponseObject, error) {
	e, code, msg, err := imageResult(h.envs.RollbackImage(ctx, principal(ctx), req.ID))
	switch code {
	case 404:
		return RollbackEnvironmentImage404JSONResponse{NotFoundJSONResponse{Error: msg}}, nil
	case 409:
		return RollbackEnvironmentImage409JSONResponse{ConflictJSONResponse{Error: msg}}, nil
	}
	if err != nil {
		return nil, err
	}
	return RollbackEnvironmentImage200JSONResponse(e), nil
}

func (h *handler) KeepEnvironmentImage(ctx context.Context, req KeepEnvironmentImageRequestObject) (KeepEnvironmentImageResponseObject, error) {
	e, code, msg, err := imageResult(h.envs.KeepImage(ctx, principal(ctx), req.ID))
	switch code {
	case 404:
		return KeepEnvironmentImage404JSONResponse{NotFoundJSONResponse{Error: msg}}, nil
	case 409:
		return KeepEnvironmentImage409JSONResponse{ConflictJSONResponse{Error: msg}}, nil
	}
	if err != nil {
		return nil, err
	}
	return KeepEnvironmentImage200JSONResponse(e), nil
}

func (h *handler) CancelEnvironmentImageChange(ctx context.Context, req CancelEnvironmentImageChangeRequestObject) (CancelEnvironmentImageChangeResponseObject, error) {
	e, code, msg, err := imageResult(h.envs.CancelImageChange(ctx, principal(ctx), req.ID))
	switch code {
	case 404:
		return CancelEnvironmentImageChange404JSONResponse{NotFoundJSONResponse{Error: msg}}, nil
	case 409:
		return CancelEnvironmentImageChange409JSONResponse{ConflictJSONResponse{Error: msg}}, nil
	}
	if err != nil {
		return nil, err
	}
	return CancelEnvironmentImageChange200JSONResponse(e), nil
}
