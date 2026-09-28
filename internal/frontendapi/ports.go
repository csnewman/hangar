package frontendapi

import (
	"context"
	"errors"
	"net/url"

	"github.com/csnewman/hangar/internal/editor"
	"github.com/csnewman/hangar/internal/environments"
)

func (h *handler) SetEnvironmentPorts(ctx context.Context, req SetEnvironmentPortsRequestObject) (SetEnvironmentPortsResponseObject, error) {
	e, err := h.envs.SetPortsPublic(ctx, principal(ctx), req.ID, req.Body.Public)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return SetEnvironmentPorts404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return SetEnvironmentPorts200JSONResponse(environment(e)), nil
}

// SignInEnvironmentPorts is where a browser opening a private environment's
// web server is sent. It needs Hangar's session cookie, which a top-level
// navigation carries, so it is a page load rather than a call from the UI.
func (h *handler) SignInEnvironmentPorts(ctx context.Context, req SignInEnvironmentPortsRequestObject) (SignInEnvironmentPortsResponseObject, error) {
	if h.editors == nil {
		return SignInEnvironmentPorts503JSONResponse{UnavailableJSONResponse{Error: "this Hangar has no public URL set, which environments' hosts are named after"}}, nil
	}
	s := sessionFrom(ctx)
	if !s.ok || s.bearer {
		back := editor.PortsSignInPath(req.ID) + "?" + url.Values{"to": {req.Params.To}}.Encode()
		loc := "/login?" + url.Values{"next": {back}}.Encode()
		return SignInEnvironmentPorts303Response{Headers: SignInEnvironmentPorts303ResponseHeaders{Location: &loc}}, nil
	}
	env, err := h.envs.Get(ctx, s.principal, req.ID)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return SignInEnvironmentPorts404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	loc, err := h.editors.SignInPort(ctx, s.token, env.ID, env.ShortID, req.Params.To)
	if err != nil {
		return SignInEnvironmentPorts400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	}
	h.access(ctx, env, "environment.open_ports", map[string]any{"url": req.Params.To})
	return SignInEnvironmentPorts303Response{Headers: SignInEnvironmentPorts303ResponseHeaders{Location: &loc}}, nil
}
