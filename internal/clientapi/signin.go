package clientapi

import (
	"context"
	"errors"
	"net/url"
	"time"

	"github.com/csnewman/hangar/internal/users"
)

// signInInterval is how often an app asks whether its sign-in is approved.
const signInInterval = 2 * time.Second

func (h *handler) StartSignIn(ctx context.Context, req StartSignInRequestObject) (StartSignInResponseObject, error) {
	device, g, err := h.users.StartSignIn(ctx, req.Body.Name)
	switch {
	case errors.Is(err, users.ErrInvalid):
		return StartSignIn400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return StartSignIn201JSONResponse{
		DeviceCode:       device,
		UserCode:         g.Code,
		VerificationPath: "/connect/desktop?code=" + url.QueryEscape(g.Code),
		ExpiresAt:        g.ExpiresAt,
		Interval:         int(signInInterval / time.Second),
	}, nil
}

func (h *handler) FinishSignIn(ctx context.Context, req FinishSignInRequestObject) (FinishSignInResponseObject, error) {
	token, t, err := h.users.FinishSignIn(ctx, req.Body.DeviceCode)
	switch {
	case errors.Is(err, users.ErrPending):
		return FinishSignIn202JSONResponse{Error: err.Error()}, nil
	case errors.Is(err, users.ErrRefused):
		return FinishSignIn403JSONResponse{Error: err.Error()}, nil
	case errors.Is(err, users.ErrNotFound):
		return FinishSignIn404JSONResponse{NotFoundJSONResponse{Error: "no such sign-in: it expired, was collected, or never was"}}, nil
	case err != nil:
		return nil, err
	}
	return FinishSignIn200JSONResponse{Token: token, Name: t.Name}, nil
}
