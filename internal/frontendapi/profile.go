package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/profile"
)

func (h *handler) GetProfile(ctx context.Context, _ GetProfileRequestObject) (GetProfileResponseObject, error) {
	uid := principal(ctx).UserID
	files, err := h.profiles.Files(ctx, uid, true)
	if err != nil {
		return nil, err
	}
	keys, err := h.profiles.Keys(ctx, uid)
	if err != nil {
		return nil, err
	}
	out := Profile{Files: []ProfileFile{}, Keys: []SSHKey{}, Paths: profile.Paths(), Secrets: h.profiles.KeepsSecrets()}
	for _, f := range files {
		if f.Deleted {
			continue
		}
		out.Files = append(out.Files, ProfileFile{Path: f.Path, Size: len(f.Data), Mode: int(f.Mode),
			Secret: profile.Secret(f.Path), UpdatedAt: f.UpdatedAt})
	}
	for _, k := range keys {
		out.Keys = append(out.Keys, sshKey(k))
	}
	return GetProfile200JSONResponse(out), nil
}

func (h *handler) GetProfileFile(ctx context.Context, req GetProfileFileRequestObject) (GetProfileFileResponseObject, error) {
	if profile.Secret(req.Params.Path) {
		return GetProfileFile403JSONResponse{ForbiddenJSONResponse{Error: "a credential's content is not shown"}}, nil
	}
	files, err := h.profiles.Files(ctx, principal(ctx).UserID, false)
	if err != nil {
		return nil, err
	}
	for _, f := range files {
		if f.Path == req.Params.Path && !f.Deleted {
			return GetProfileFile200JSONResponse{Path: f.Path, Content: string(f.Data), Mode: int(f.Mode),
				UpdatedAt: f.UpdatedAt}, nil
		}
	}
	return GetProfileFile404JSONResponse{NotFoundJSONResponse{Error: "no such file in the profile"}}, nil
}

func (h *handler) PutProfileFile(ctx context.Context, req PutProfileFileRequestObject) (PutProfileFileResponseObject, error) {
	uid := principal(ctx).UserID
	mode := uint32(0)
	if req.Body.Mode != nil {
		mode = uint32(*req.Body.Mode)
	} else if files, err := h.profiles.Files(ctx, uid, false); err == nil {
		for _, f := range files {
			if f.Path == req.Params.Path && !f.Deleted {
				mode = f.Mode
			}
		}
	}
	if mode == 0 && profile.Secret(req.Params.Path) {
		mode = 0o600
	}
	f, err := h.profiles.Put(ctx, uid, req.Params.Path, []byte(req.Body.Content), mode)
	switch {
	case errors.Is(err, profile.ErrInvalid), errors.Is(err, profile.ErrNoKey):
		return PutProfileFile400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return PutProfileFile200JSONResponse{Path: f.Path, Size: len(f.Data), Mode: int(f.Mode),
		Secret: profile.Secret(f.Path), UpdatedAt: f.UpdatedAt}, nil
}

func (h *handler) DeleteProfileFile(ctx context.Context, req DeleteProfileFileRequestObject) (DeleteProfileFileResponseObject, error) {
	_, err := h.profiles.Delete(ctx, principal(ctx).UserID, req.Params.Path)
	switch {
	case errors.Is(err, profile.ErrInvalid):
		return DeleteProfileFile400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteProfileFile204Response{}, nil
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
