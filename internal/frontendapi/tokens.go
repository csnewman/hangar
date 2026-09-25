package frontendapi

import (
	"context"
	"errors"
	"time"

	"github.com/csnewman/hangar/internal/users"
)

func (h *handler) ListTokens(ctx context.Context, _ ListTokensRequestObject) (ListTokensResponseObject, error) {
	ts, err := h.users.Tokens(ctx, principal(ctx).UserID)
	if err != nil {
		return nil, err
	}
	out := make(ListTokens200JSONResponse, len(ts))
	for i, t := range ts {
		out[i] = accessToken(t)
	}
	return out, nil
}

func (h *handler) CreateToken(ctx context.Context, req CreateTokenRequestObject) (CreateTokenResponseObject, error) {
	var lifetime time.Duration
	if req.Body.ExpiresInDays != nil {
		lifetime = time.Duration(*req.Body.ExpiresInDays) * 24 * time.Hour
	}
	token, t, err := h.users.CreateToken(ctx, principal(ctx).UserID, req.Body.Name, lifetime)
	switch {
	case errors.Is(err, users.ErrInvalid):
		return CreateToken400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return CreateToken201JSONResponse{Token: token, Info: accessToken(t)}, nil
}

func (h *handler) RevokeToken(ctx context.Context, req RevokeTokenRequestObject) (RevokeTokenResponseObject, error) {
	err := h.users.RevokeToken(ctx, principal(ctx).UserID, req.ID)
	switch {
	case errors.Is(err, users.ErrNotFound):
		return RevokeToken404JSONResponse{NotFoundJSONResponse{Error: "no such token"}}, nil
	case err != nil:
		return nil, err
	}
	return RevokeToken204Response{}, nil
}

func accessToken(t users.Token) AccessToken {
	return AccessToken{ID: t.ID, Name: t.Name, CreatedAt: t.CreatedAt, LastUsedAt: t.LastUsedAt, ExpiresAt: t.ExpiresAt}
}
