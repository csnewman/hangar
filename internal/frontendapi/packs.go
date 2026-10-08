package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/packs"
)

func pack(k packs.Pack) Pack {
	out := Pack{ID: k.ID, Name: k.Name, Description: k.Description, OwnerID: optional(k.OwnerID),
		Owner: optional(k.Owner), TeamID: optional(k.TeamID), Team: optional(k.Team), Builtin: k.Builtin,
		Attach: PackAttach(k.Attach), Paths: []PackPath{}, CanChange: k.CanChange, CanDelete: k.CanDelete,
		CreatedAt: k.CreatedAt, UpdatedAt: k.UpdatedAt}
	for _, p := range k.Paths {
		out.Paths = append(out.Paths, PackPath{Path: p.Path, Sensitive: p.Sensitive})
	}
	return out
}

func packInput(in PackInput) packs.PackInput {
	out := packs.PackInput{Name: in.Name, Description: deref(in.Description), TeamID: deref(in.TeamID)}
	if in.Attach != nil {
		out.Attach = packs.Attach(*in.Attach)
	}
	for _, p := range in.Paths {
		out.Paths = append(out.Paths, packs.Path{Path: p.Path, Sensitive: p.Sensitive})
	}
	return out
}

func copyOf(c packs.Copy) Copy {
	return Copy{ID: c.ID, PackID: c.PackID, OwnerID: optional(c.OwnerID), Owner: optional(c.Owner),
		TeamID: optional(c.TeamID), Team: optional(c.Team), Personal: c.Personal, Name: c.Name,
		CanWrite: c.CanWrite, CanDelete: c.CanDelete, CreatedAt: c.CreatedAt}
}

func packFile(f packs.File) PackFile {
	return PackFile{Path: f.Path, Size: int(f.Size), Mode: int(f.Mode), Sensitive: f.Sensitive, Shared: f.Shared,
		UpdatedAt: f.UpdatedAt}
}

// packError sorts a packs error: not found, invalid, forbidden, or
// another.
func packError(err error) (notFound, invalid, forbidden *Error) {
	e := &Error{}
	if err != nil {
		e.Error = err.Error()
	}
	switch {
	case errors.Is(err, packs.ErrNotFound):
		return e, nil, nil
	case errors.Is(err, packs.ErrInvalid):
		return nil, e, nil
	case errors.Is(err, packs.ErrForbidden):
		return nil, nil, e
	}
	return nil, nil, nil
}

func (h *handler) ListPacks(ctx context.Context, _ ListPacksRequestObject) (ListPacksResponseObject, error) {
	p := principal(ctx)
	ks, err := h.packs.Packs(ctx, p)
	if err != nil {
		return nil, err
	}
	defaults, err := h.packs.Defaults(ctx, p)
	if err != nil {
		return nil, err
	}
	out := make(ListPacks200JSONResponse, 0, len(ks))
	for _, k := range ks {
		x := pack(k)
		if c, ok := defaults[k.ID]; ok {
			x.DefaultCopyID = &c
		}
		out = append(out, x)
	}
	return out, nil
}

func (h *handler) CreatePack(ctx context.Context, req CreatePackRequestObject) (CreatePackResponseObject, error) {
	k, err := h.packs.CreatePack(ctx, principal(ctx), packInput(*req.Body))
	if _, bad, no := packError(err); bad != nil {
		return CreatePack400JSONResponse{InvalidJSONResponse(*bad)}, nil
	} else if no != nil {
		return CreatePack403JSONResponse{ForbiddenJSONResponse(*no)}, nil
	} else if err != nil {
		return nil, err
	}
	return CreatePack201JSONResponse(pack(k)), nil
}

func (h *handler) GetPack(ctx context.Context, req GetPackRequestObject) (GetPackResponseObject, error) {
	p := principal(ctx)
	k, err := h.packs.Pack(ctx, p, req.ID)
	if nf, _, _ := packError(err); nf != nil {
		return GetPack404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	} else if err != nil {
		return nil, err
	}
	defaults, err := h.packs.Defaults(ctx, p)
	if err != nil {
		return nil, err
	}
	x := pack(k)
	if c, ok := defaults[k.ID]; ok {
		x.DefaultCopyID = &c
	}
	return GetPack200JSONResponse(x), nil
}

func (h *handler) UpdatePack(ctx context.Context, req UpdatePackRequestObject) (UpdatePackResponseObject, error) {
	k, err := h.packs.UpdatePack(ctx, principal(ctx), req.ID, packInput(*req.Body))
	nf, bad, no := packError(err)
	switch {
	case nf != nil:
		return UpdatePack404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	case bad != nil:
		return UpdatePack400JSONResponse{InvalidJSONResponse(*bad)}, nil
	case no != nil:
		return UpdatePack403JSONResponse{ForbiddenJSONResponse(*no)}, nil
	case err != nil:
		return nil, err
	}
	return UpdatePack200JSONResponse(pack(k)), nil
}

func (h *handler) DeletePack(ctx context.Context, req DeletePackRequestObject) (DeletePackResponseObject, error) {
	err := h.packs.DeletePack(ctx, principal(ctx), req.ID)
	nf, _, no := packError(err)
	switch {
	case nf != nil:
		return DeletePack404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	case no != nil:
		return DeletePack403JSONResponse{ForbiddenJSONResponse(*no)}, nil
	case err != nil:
		return nil, err
	}
	return DeletePack204Response{}, nil
}

func (h *handler) ListCopies(ctx context.Context, req ListCopiesRequestObject) (ListCopiesResponseObject, error) {
	cs, err := h.packs.Copies(ctx, principal(ctx), req.ID)
	if nf, _, _ := packError(err); nf != nil {
		return ListCopies404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	} else if err != nil {
		return nil, err
	}
	out := make(ListCopies200JSONResponse, 0, len(cs))
	for _, c := range cs {
		out = append(out, copyOf(c))
	}
	return out, nil
}

func (h *handler) CreateCopy(ctx context.Context, req CreateCopyRequestObject) (CreateCopyResponseObject, error) {
	c, err := h.packs.CreateCopy(ctx, principal(ctx), req.ID, packs.CopyInput{Name: req.Body.Name,
		TeamID: deref(req.Body.TeamID)})
	nf, bad, no := packError(err)
	switch {
	case nf != nil:
		return CreateCopy404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	case bad != nil:
		return CreateCopy400JSONResponse{InvalidJSONResponse(*bad)}, nil
	case no != nil:
		return CreateCopy403JSONResponse{ForbiddenJSONResponse(*no)}, nil
	case err != nil:
		return nil, err
	}
	return CreateCopy201JSONResponse(copyOf(c)), nil
}

func (h *handler) SetDefaultCopy(ctx context.Context, req SetDefaultCopyRequestObject) (SetDefaultCopyResponseObject, error) {
	err := h.packs.SetDefault(ctx, principal(ctx), req.ID, deref(req.Body.CopyID))
	nf, bad, _ := packError(err)
	switch {
	case nf != nil:
		return SetDefaultCopy404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	case bad != nil:
		return SetDefaultCopy400JSONResponse{InvalidJSONResponse(*bad)}, nil
	case err != nil:
		return nil, err
	}
	return SetDefaultCopy204Response{}, nil
}

func (h *handler) GetCopy(ctx context.Context, req GetCopyRequestObject) (GetCopyResponseObject, error) {
	p := principal(ctx)
	c, err := h.packs.Copy(ctx, p, req.ID)
	if nf, _, _ := packError(err); nf != nil {
		return GetCopy404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	} else if err != nil {
		return nil, err
	}
	k, err := h.packs.Pack(ctx, p, c.PackID)
	if nf, _, _ := packError(err); nf != nil {
		return GetCopy404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	} else if err != nil {
		return nil, err
	}
	files, err := h.packs.Files(ctx, p, c.ID)
	if err != nil {
		return nil, err
	}
	out := CopyFiles{Copy: copyOf(c), Pack: pack(k), Files: []PackFile{}}
	for _, f := range files {
		out.Files = append(out.Files, packFile(f))
	}
	return GetCopy200JSONResponse(out), nil
}

func (h *handler) RenameCopy(ctx context.Context, req RenameCopyRequestObject) (RenameCopyResponseObject, error) {
	c, err := h.packs.RenameCopy(ctx, principal(ctx), req.ID, req.Body.Name)
	nf, bad, no := packError(err)
	switch {
	case nf != nil:
		return RenameCopy404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	case bad != nil:
		return RenameCopy400JSONResponse{InvalidJSONResponse(*bad)}, nil
	case no != nil:
		return RenameCopy403JSONResponse{ForbiddenJSONResponse(*no)}, nil
	case err != nil:
		return nil, err
	}
	return RenameCopy200JSONResponse(copyOf(c)), nil
}

func (h *handler) DeleteCopy(ctx context.Context, req DeleteCopyRequestObject) (DeleteCopyResponseObject, error) {
	err := h.packs.DeleteCopy(ctx, principal(ctx), req.ID)
	nf, _, no := packError(err)
	switch {
	case nf != nil:
		return DeleteCopy404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	case no != nil:
		return DeleteCopy403JSONResponse{ForbiddenJSONResponse(*no)}, nil
	case err != nil:
		return nil, err
	}
	return DeleteCopy204Response{}, nil
}

func (h *handler) GetCopyFile(ctx context.Context, req GetCopyFileRequestObject) (GetCopyFileResponseObject, error) {
	f, err := h.packs.File(ctx, principal(ctx), req.ID, req.Params.Path)
	if nf, _, _ := packError(err); nf != nil {
		return GetCopyFile404JSONResponse{NotFoundJSONResponse{Error: "no such file in the copy"}}, nil
	} else if err != nil {
		return nil, err
	}
	return GetCopyFile200JSONResponse{Path: f.Path, Content: string(f.Data), Mode: int(f.Mode),
		Sensitive: f.Sensitive, Shared: f.Shared, UpdatedAt: f.UpdatedAt}, nil
}

func (h *handler) PutCopyFile(ctx context.Context, req PutCopyFileRequestObject) (PutCopyFileResponseObject, error) {
	mode := uint32(0)
	if req.Body.Mode != nil {
		mode = uint32(*req.Body.Mode)
	}
	f, err := h.packs.Put(ctx, principal(ctx), req.ID, req.Params.Path, []byte(req.Body.Content), mode)
	nf, bad, no := packError(err)
	switch {
	case nf != nil:
		return PutCopyFile404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	case bad != nil:
		return PutCopyFile400JSONResponse{InvalidJSONResponse(*bad)}, nil
	case no != nil:
		return PutCopyFile403JSONResponse{ForbiddenJSONResponse(*no)}, nil
	case err != nil:
		return nil, err
	}
	return PutCopyFile200JSONResponse(packFile(f)), nil
}

func (h *handler) SetCopyFileMode(ctx context.Context, req SetCopyFileModeRequestObject) (SetCopyFileModeResponseObject, error) {
	f, err := h.packs.Chmod(ctx, principal(ctx), req.ID, req.Params.Path, uint32(req.Body.Mode))
	nf, bad, no := packError(err)
	switch {
	case nf != nil:
		return SetCopyFileMode404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	case bad != nil:
		return SetCopyFileMode400JSONResponse{InvalidJSONResponse(*bad)}, nil
	case no != nil:
		return SetCopyFileMode403JSONResponse{ForbiddenJSONResponse(*no)}, nil
	case err != nil:
		return nil, err
	}
	return SetCopyFileMode200JSONResponse(packFile(f)), nil
}

func (h *handler) DeleteCopyFile(ctx context.Context, req DeleteCopyFileRequestObject) (DeleteCopyFileResponseObject, error) {
	err := h.packs.Delete(ctx, principal(ctx), req.ID, req.Params.Path)
	nf, bad, no := packError(err)
	switch {
	case nf != nil:
		return DeleteCopyFile404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	case bad != nil:
		return DeleteCopyFile400JSONResponse{InvalidJSONResponse(*bad)}, nil
	case no != nil:
		return DeleteCopyFile403JSONResponse{ForbiddenJSONResponse(*no)}, nil
	case err != nil:
		return nil, err
	}
	return DeleteCopyFile204Response{}, nil
}

// visibleEnvironment reports whether the caller may see an environment.
func (h *handler) visibleEnvironment(ctx context.Context, id string) (bool, error) {
	_, err := h.envs.Get(ctx, principal(ctx), id)
	if errors.Is(err, environments.ErrNotFound) {
		return false, nil
	}
	return err == nil, err
}

func (h *handler) ListEnvironmentPacks(ctx context.Context, req ListEnvironmentPacksRequestObject) (ListEnvironmentPacksResponseObject, error) {
	if ok, err := h.visibleEnvironment(ctx, req.ID); err != nil || !ok {
		if err == nil {
			return ListEnvironmentPacks404JSONResponse{NotFoundJSONResponse{Error: "no such environment"}}, nil
		}
		return nil, err
	}
	as, err := h.packs.EnvironmentPacks(ctx, req.ID)
	if nf, _, _ := packError(err); nf != nil {
		return ListEnvironmentPacks404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	} else if err != nil {
		return nil, err
	}
	out := make(ListEnvironmentPacks200JSONResponse, 0, len(as))
	for _, a := range as {
		out = append(out, AttachedPack{Pack: pack(a.Pack), Copy: copyOf(a.Copy), From: AttachedPackFrom(a.From),
			Listed: a.Listed})
	}
	return out, nil
}

func (h *handler) ChooseEnvironmentCopy(ctx context.Context, req ChooseEnvironmentCopyRequestObject) (ChooseEnvironmentCopyResponseObject, error) {
	if ok, err := h.visibleEnvironment(ctx, req.ID); err != nil || !ok {
		if err == nil {
			return ChooseEnvironmentCopy404JSONResponse{NotFoundJSONResponse{Error: "no such environment"}}, nil
		}
		return nil, err
	}
	err := h.packs.ChooseCopy(ctx, principal(ctx), req.ID, req.Pack, deref(req.Body.CopyID))
	nf, bad, no := packError(err)
	switch {
	case nf != nil:
		return ChooseEnvironmentCopy404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	case bad != nil:
		return ChooseEnvironmentCopy400JSONResponse{InvalidJSONResponse(*bad)}, nil
	case no != nil:
		return ChooseEnvironmentCopy403JSONResponse{ForbiddenJSONResponse(*no)}, nil
	case err != nil:
		return nil, err
	}
	return ChooseEnvironmentCopy204Response{}, nil
}

func (h *handler) ListEnvironmentConflicts(ctx context.Context, req ListEnvironmentConflictsRequestObject) (ListEnvironmentConflictsResponseObject, error) {
	if ok, err := h.visibleEnvironment(ctx, req.ID); err != nil || !ok {
		if err == nil {
			return ListEnvironmentConflicts404JSONResponse{NotFoundJSONResponse{Error: "no such environment"}}, nil
		}
		return nil, err
	}
	cs, err := h.packs.Conflicts(ctx, req.ID)
	if err != nil {
		return nil, err
	}
	out := make(ListEnvironmentConflicts200JSONResponse, 0, len(cs))
	for _, c := range cs {
		x := FileConflict{CopyID: c.Copy, Path: c.Path, FoundAt: c.FoundAt}
		if c.Resolution != "" {
			r := FileConflictResolution(c.Resolution)
			x.Resolution = &r
		}
		out = append(out, x)
	}
	return out, nil
}

func (h *handler) ResolveEnvironmentConflict(ctx context.Context, req ResolveEnvironmentConflictRequestObject) (ResolveEnvironmentConflictResponseObject, error) {
	if ok, err := h.visibleEnvironment(ctx, req.ID); err != nil || !ok {
		if err == nil {
			return ResolveEnvironmentConflict404JSONResponse{NotFoundJSONResponse{Error: "no such environment"}}, nil
		}
		return nil, err
	}
	err := h.packs.ResolveConflict(ctx, principal(ctx), req.ID, req.Body.CopyID, req.Body.Path,
		string(req.Body.Resolution))
	nf, bad, no := packError(err)
	switch {
	case nf != nil:
		return ResolveEnvironmentConflict404JSONResponse{NotFoundJSONResponse(*nf)}, nil
	case bad != nil:
		return ResolveEnvironmentConflict400JSONResponse{InvalidJSONResponse(*bad)}, nil
	case no != nil:
		return ResolveEnvironmentConflict403JSONResponse{ForbiddenJSONResponse(*no)}, nil
	case err != nil:
		return nil, err
	}
	return ResolveEnvironmentConflict204Response{}, nil
}
