package clientapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/environments"
)

// SetEnvironmentPorts says who may reach an environment's own web servers.
func (h *handler) SetEnvironmentPorts(ctx context.Context, req SetEnvironmentPortsRequestObject) (SetEnvironmentPortsResponseObject, error) {
	e, err := h.envs.SetPortsPublic(ctx, principal(ctx), req.ID, req.Body.Public)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return SetEnvironmentPorts404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return SetEnvironmentPorts200JSONResponse{EnvironmentJSONResponse(environment(e))}, nil
}
