package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/users"
)

func (h *handler) GetSignIn(ctx context.Context, req GetSignInRequestObject) (GetSignInResponseObject, error) {
	g, err := h.users.SignInGrant(ctx, req.Code)
	switch {
	case errors.Is(err, users.ErrNotFound):
		return GetSignIn404JSONResponse{NotFoundJSONResponse{Error: "no app is waiting under that code"}}, nil
	case err != nil:
		return nil, err
	}
	return GetSignIn200JSONResponse{Code: g.Code, Name: g.Name, CreatedAt: g.CreatedAt, ExpiresAt: g.ExpiresAt}, nil
}

func (h *handler) ApproveSignIn(ctx context.Context, req ApproveSignInRequestObject) (ApproveSignInResponseObject, error) {
	err := h.users.ApproveSignIn(ctx, req.Code, principal(ctx).UserID)
	switch {
	case errors.Is(err, users.ErrNotFound):
		return ApproveSignIn404JSONResponse{NotFoundJSONResponse{Error: "no app is waiting under that code"}}, nil
	case err != nil:
		return nil, err
	}
	return ApproveSignIn204Response{}, nil
}

func (h *handler) RefuseSignIn(ctx context.Context, req RefuseSignInRequestObject) (RefuseSignInResponseObject, error) {
	err := h.users.RefuseSignIn(ctx, req.Code, principal(ctx).UserID)
	switch {
	case errors.Is(err, users.ErrNotFound):
		return RefuseSignIn404JSONResponse{NotFoundJSONResponse{Error: "no app is waiting under that code"}}, nil
	case err != nil:
		return nil, err
	}
	return RefuseSignIn204Response{}, nil
}
