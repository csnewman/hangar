package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/users"
)

func (h *handler) ListUsers(ctx context.Context, _ ListUsersRequestObject) (ListUsersResponseObject, error) {
	list, err := h.users.List(ctx)
	if err != nil {
		return nil, err
	}
	out := make(ListUsers200JSONResponse, len(list))
	for i, u := range list {
		out[i] = user(u)
	}
	return out, nil
}

func (h *handler) CreateUser(ctx context.Context, req CreateUserRequestObject) (CreateUserResponseObject, error) {
	nu := users.NewUser{Username: req.Body.Username, Password: req.Body.Password}
	if req.Body.DisplayName != nil {
		nu.DisplayName = *req.Body.DisplayName
	}
	if req.Body.Admin != nil {
		nu.Admin = *req.Body.Admin
	}
	u, err := h.users.Create(ctx, nu)
	switch {
	case errors.Is(err, users.ErrInvalid):
		return CreateUser400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, users.ErrConflict):
		return CreateUser409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return CreateUser201JSONResponse(user(u)), nil
}

func (h *handler) UpdateUser(ctx context.Context, req UpdateUserRequestObject) (UpdateUserResponseObject, error) {
	u, err := h.users.Update(ctx, req.ID, users.Update{
		DisplayName: req.Body.DisplayName,
		Admin:       req.Body.Admin,
		Disabled:    req.Body.Disabled,
		Password:    req.Body.Password,
	})
	switch {
	case errors.Is(err, users.ErrNotFound):
		return UpdateUser404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, users.ErrInvalid):
		return UpdateUser400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, users.ErrConflict):
		return UpdateUser409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return UpdateUser200JSONResponse(user(u)), nil
}

func (h *handler) DeleteUser(ctx context.Context, req DeleteUserRequestObject) (DeleteUserResponseObject, error) {
	err := h.users.Delete(ctx, req.ID)
	switch {
	case errors.Is(err, users.ErrNotFound):
		return DeleteUser404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, users.ErrConflict):
		return DeleteUser409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteUser204Response{}, nil
}

func user(u users.User) User {
	return User{
		ID:           u.ID,
		Username:     u.Username,
		DisplayName:  u.DisplayName,
		Admin:        u.Admin,
		Disabled:     u.Disabled,
		HasPassword:  u.HasPassword,
		Environments: u.Environments,
		CreatedAt:    u.CreatedAt,
	}
}
