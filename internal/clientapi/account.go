package clientapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/users"
)

func (h *handler) DeleteLoginKey(ctx context.Context, req DeleteLoginKeyRequestObject) (DeleteLoginKeyResponseObject, error) {
	err := h.profiles.DeleteLoginKey(ctx, principal(ctx).UserID, req.ID)
	switch {
	case errors.Is(err, profile.ErrNotFound):
		return DeleteLoginKey404JSONResponse{NotFoundJSONResponse{Error: "no such key"}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteLoginKey204Response{}, nil
}

func (h *handler) ListTokens(ctx context.Context, _ ListTokensRequestObject) (ListTokensResponseObject, error) {
	p := principal(ctx)
	ts, err := h.users.Tokens(ctx, p.UserID)
	if err != nil {
		return nil, err
	}
	out := make(ListTokens200JSONResponse, len(ts))
	for i, t := range ts {
		out[i] = AccessToken{ID: t.ID, Name: t.Name, CreatedAt: t.CreatedAt, LastUsedAt: t.LastUsedAt,
			ExpiresAt: t.ExpiresAt, Current: p.TokenID != "" && t.ID == p.TokenID}
	}
	return out, nil
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

func (h *handler) ListSSHKeys(ctx context.Context, _ ListSSHKeysRequestObject) (ListSSHKeysResponseObject, error) {
	keys, err := h.profiles.Keys(ctx, principal(ctx).UserID)
	if err != nil {
		return nil, err
	}
	out := make(ListSSHKeys200JSONResponse, len(keys))
	for i, k := range keys {
		out[i] = sshKey(k)
	}
	return out, nil
}

func (h *handler) GenerateSSHKey(ctx context.Context, req GenerateSSHKeyRequestObject) (GenerateSSHKeyResponseObject, error) {
	k, err := h.profiles.GenerateKey(ctx, principal(ctx).UserID, req.Body.Name)
	switch {
	case errors.Is(err, profile.ErrInvalid), errors.Is(err, profile.ErrNoKey):
		return GenerateSSHKey400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return GenerateSSHKey201JSONResponse(sshKey(k)), nil
}

func (h *handler) DeleteSSHKey(ctx context.Context, req DeleteSSHKeyRequestObject) (DeleteSSHKeyResponseObject, error) {
	err := h.profiles.DeleteKey(ctx, principal(ctx).UserID, req.ID)
	switch {
	case errors.Is(err, profile.ErrNotFound):
		return DeleteSSHKey404JSONResponse{NotFoundJSONResponse{Error: "no such key"}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteSSHKey204Response{}, nil
}

func sshKey(k profile.Key) SSHKey {
	return SSHKey{ID: k.ID, Name: k.Name, PublicKey: k.PublicKey, Fingerprint: k.Fingerprint, CreatedAt: k.CreatedAt}
}
