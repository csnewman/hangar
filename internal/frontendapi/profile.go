package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/profile"
)

func (h *handler) GetProfile(ctx context.Context, _ GetProfileRequestObject) (GetProfileResponseObject, error) {
	uid := principal(ctx).UserID
	keys, err := h.profiles.Keys(ctx, uid)
	if err != nil {
		return nil, err
	}
	logins, err := h.profiles.LoginKeys(ctx, uid)
	if err != nil {
		return nil, err
	}
	out := Profile{Keys: []SSHKey{}, LoginKeys: []LoginKey{}, Secrets: h.profiles.KeepsSecrets()}
	for _, k := range logins {
		out.LoginKeys = append(out.LoginKeys, loginKey(k))
	}
	for _, k := range keys {
		out.Keys = append(out.Keys, sshKey(k))
	}
	return GetProfile200JSONResponse(out), nil
}

func (h *handler) AddSSHKey(ctx context.Context, req AddSSHKeyRequestObject) (AddSSHKeyResponseObject, error) {
	uid := principal(ctx).UserID
	var k profile.Key
	var err error
	if req.Body.PrivateKey != nil && *req.Body.PrivateKey != "" {
		k, err = h.profiles.ImportKey(ctx, uid, req.Body.Name, []byte(*req.Body.PrivateKey))
	} else {
		k, err = h.profiles.GenerateKey(ctx, uid, req.Body.Name)
	}
	switch {
	case errors.Is(err, profile.ErrInvalid), errors.Is(err, profile.ErrNoKey):
		return AddSSHKey400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return AddSSHKey201JSONResponse(sshKey(k)), nil
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

func (h *handler) AddLoginKey(ctx context.Context, req AddLoginKeyRequestObject) (AddLoginKeyResponseObject, error) {
	name := ""
	if req.Body.Name != nil {
		name = *req.Body.Name
	}
	k, err := h.profiles.AddLoginKey(ctx, principal(ctx).UserID, name, req.Body.PublicKey)
	switch {
	case errors.Is(err, profile.ErrInvalid):
		return AddLoginKey400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return AddLoginKey201JSONResponse(loginKey(k)), nil
}

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

func loginKey(k profile.LoginKey) LoginKey {
	return LoginKey{ID: k.ID, Name: k.Name, PublicKey: k.PublicKey, Fingerprint: k.Fingerprint, CreatedAt: k.CreatedAt}
}
