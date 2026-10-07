package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/profile"
)

func filePack(k profile.Pack) FilePack {
	return FilePack{ID: k.ID, Name: k.Name, Description: k.Description, OwnerID: optional(k.OwnerID),
		Owner: optional(k.Owner), TeamID: optional(k.TeamID), Team: optional(k.Team), Personal: k.Personal,
		Paths: k.Paths, CanChange: k.CanChange, CanWrite: k.CanWrite, CanDelete: k.CanDelete,
		CreatedAt: k.CreatedAt, UpdatedAt: k.UpdatedAt}
}

func packInput(in FilePackInput) profile.PackInput {
	return profile.PackInput{Name: in.Name, Description: deref(in.Description), TeamID: deref(in.TeamID),
		Personal: in.Personal != nil && *in.Personal, Paths: in.Paths}
}

func profileFile(f profile.File) ProfileFile {
	return ProfileFile{Path: f.Path, Size: int(f.Size), Mode: int(f.Mode), Sensitive: f.Sensitive,
		UpdatedAt: f.UpdatedAt}
}

func (h *handler) ListFilePacks(ctx context.Context, _ ListFilePacksRequestObject) (ListFilePacksResponseObject, error) {
	packs, err := h.profiles.Packs(ctx, principal(ctx))
	if err != nil {
		return nil, err
	}
	out := make(ListFilePacks200JSONResponse, 0, len(packs))
	for _, k := range packs {
		out = append(out, filePack(k))
	}
	return out, nil
}

func (h *handler) CreateFilePack(ctx context.Context, req CreateFilePackRequestObject) (CreateFilePackResponseObject, error) {
	k, err := h.profiles.CreatePack(ctx, principal(ctx), packInput(*req.Body))
	switch {
	case errors.Is(err, profile.ErrInvalid):
		return CreateFilePack400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, profile.ErrForbidden):
		return CreateFilePack403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return CreateFilePack201JSONResponse(filePack(k)), nil
}

func (h *handler) GetFilePack(ctx context.Context, req GetFilePackRequestObject) (GetFilePackResponseObject, error) {
	p := principal(ctx)
	k, err := h.profiles.Pack(ctx, p, req.ID)
	if errors.Is(err, profile.ErrNotFound) {
		return GetFilePack404JSONResponse{NotFoundJSONResponse{Error: "no such pack"}}, nil
	}
	if err != nil {
		return nil, err
	}
	set, err := h.profiles.PackSet(ctx, p, req.ID, false)
	if err != nil {
		return nil, err
	}
	files, err := h.profiles.List(ctx, set, true)
	if err != nil {
		return nil, err
	}
	out := FilePackFiles{Pack: filePack(k), Files: []ProfileFile{}}
	for _, f := range files {
		out.Files = append(out.Files, profileFile(f))
	}
	return GetFilePack200JSONResponse(out), nil
}

func (h *handler) UpdateFilePack(ctx context.Context, req UpdateFilePackRequestObject) (UpdateFilePackResponseObject, error) {
	k, err := h.profiles.UpdatePack(ctx, principal(ctx), req.ID, packInput(*req.Body))
	switch {
	case errors.Is(err, profile.ErrNotFound):
		return UpdateFilePack404JSONResponse{NotFoundJSONResponse{Error: "no such pack"}}, nil
	case errors.Is(err, profile.ErrInvalid):
		return UpdateFilePack400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, profile.ErrForbidden):
		return UpdateFilePack403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return UpdateFilePack200JSONResponse(filePack(k)), nil
}

func (h *handler) DeleteFilePack(ctx context.Context, req DeleteFilePackRequestObject) (DeleteFilePackResponseObject, error) {
	err := h.profiles.DeletePack(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, profile.ErrNotFound):
		return DeleteFilePack404JSONResponse{NotFoundJSONResponse{Error: "no such pack"}}, nil
	case errors.Is(err, profile.ErrForbidden):
		return DeleteFilePack403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteFilePack204Response{}, nil
}

func (h *handler) GetFilePackFile(ctx context.Context, req GetFilePackFileRequestObject) (GetFilePackFileResponseObject, error) {
	set, err := h.profiles.PackSet(ctx, principal(ctx), req.ID, false)
	if err == nil {
		var f profile.File
		f, err = h.profiles.File(ctx, set, req.Params.Path, true)
		if err == nil {
			return GetFilePackFile200JSONResponse{Path: f.Path, Content: string(f.Data), Mode: int(f.Mode),
				UpdatedAt: f.UpdatedAt}, nil
		}
	}
	if errors.Is(err, profile.ErrNotFound) {
		return GetFilePackFile404JSONResponse{NotFoundJSONResponse{Error: "no such file in the pack"}}, nil
	}
	return nil, err
}

func (h *handler) PutFilePackFile(ctx context.Context, req PutFilePackFileRequestObject) (PutFilePackFileResponseObject, error) {
	set, err := h.profiles.PackSet(ctx, principal(ctx), req.ID, true)
	var f profile.File
	if err == nil {
		mode := uint32(0)
		if req.Body.Mode != nil {
			mode = uint32(*req.Body.Mode)
		} else if cur, err := h.profiles.File(ctx, set, req.Params.Path, true); err == nil {
			mode = cur.Mode
		}
		f, err = h.profiles.Put(ctx, set, req.Params.Path, []byte(req.Body.Content), mode)
	}
	switch {
	case errors.Is(err, profile.ErrNotFound):
		return PutFilePackFile404JSONResponse{NotFoundJSONResponse{Error: "no such pack"}}, nil
	case errors.Is(err, profile.ErrInvalid):
		return PutFilePackFile400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, profile.ErrForbidden):
		return PutFilePackFile403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return PutFilePackFile200JSONResponse(profileFile(f)), nil
}

func (h *handler) UpdateFilePackFileSettings(ctx context.Context, req UpdateFilePackFileSettingsRequestObject) (UpdateFilePackFileSettingsResponseObject, error) {
	set, err := h.profiles.PackSet(ctx, principal(ctx), req.ID, true)
	var f profile.File
	if err == nil {
		f, err = h.profiles.SetSettings(ctx, set, req.Params.Path, uint32(req.Body.Mode), req.Body.Sensitive)
	}
	switch {
	case errors.Is(err, profile.ErrNotFound):
		return UpdateFilePackFileSettings404JSONResponse{NotFoundJSONResponse{Error: "no such file in the pack"}}, nil
	case errors.Is(err, profile.ErrInvalid):
		return UpdateFilePackFileSettings400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, profile.ErrForbidden):
		return UpdateFilePackFileSettings403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return UpdateFilePackFileSettings200JSONResponse(profileFile(f)), nil
}

func (h *handler) DeleteFilePackFile(ctx context.Context, req DeleteFilePackFileRequestObject) (DeleteFilePackFileResponseObject, error) {
	set, err := h.profiles.PackSet(ctx, principal(ctx), req.ID, true)
	if err == nil {
		err = h.profiles.Delete(ctx, set, req.Params.Path)
	}
	switch {
	case errors.Is(err, profile.ErrNotFound):
		return DeleteFilePackFile404JSONResponse{NotFoundJSONResponse{Error: "no such pack"}}, nil
	case errors.Is(err, profile.ErrInvalid):
		return DeleteFilePackFile400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, profile.ErrForbidden):
		return DeleteFilePackFile403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteFilePackFile204Response{}, nil
}
