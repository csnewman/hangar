package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/environments"
)

// Editors signs users in to environments' editors.
type Editors interface {
	// SignIn returns the URL that signs the holder of a Hangar session in to
	// an environment's editor and opens it on folder.
	SignIn(ctx context.Context, sessionToken, environmentID, folder string) (string, error)
}

func (h *handler) OpenEditor(ctx context.Context, req OpenEditorRequestObject) (OpenEditorResponseObject, error) {
	env, err := h.envs.Get(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return OpenEditor404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	if h.editors == nil {
		return OpenEditor503JSONResponse{UnavailableJSONResponse{Error: "this Hangar has no public URL set, which editors are served under"}}, nil
	}
	if env.Phase != api.PhaseRunning || env.WorkerID == "" {
		return OpenEditor409JSONResponse{ConflictJSONResponse{Error: errNotRunning.Error()}}, nil
	}
	url, err := h.editors.SignIn(ctx, sessionFrom(ctx).token, env.ID, env.Spec.EditorPath)
	if err != nil {
		return nil, err
	}
	h.access(ctx, env, "environment.open_editor", nil)
	return OpenEditor200JSONResponse{URL: url}, nil
}
