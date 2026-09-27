package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/registry"
)

func (h *handler) ListImageRepositories(ctx context.Context, _ ListImageRepositoriesRequestObject) (ListImageRepositoriesResponseObject, error) {
	if h.registry == nil {
		return ListImageRepositories404JSONResponse{NotFoundJSONResponse{Error: "this server runs no registry"}}, nil
	}
	list, err := h.registry.List(ctx, principal(ctx))
	if err != nil {
		return nil, err
	}
	out := make(ListImageRepositories200JSONResponse, len(list))
	for i, x := range list {
		out[i] = h.imageRepository(x)
	}
	return out, nil
}

func (h *handler) GetImageRepository(ctx context.Context, req GetImageRepositoryRequestObject) (GetImageRepositoryResponseObject, error) {
	if h.registry == nil {
		return GetImageRepository404JSONResponse{NotFoundJSONResponse{Error: "this server runs no registry"}}, nil
	}
	x, err := h.registry.Get(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, registry.ErrNotFound):
		return GetImageRepository404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return GetImageRepository200JSONResponse(h.imageRepository(x)), nil
}

func (h *handler) UpdateImageRepository(ctx context.Context, req UpdateImageRepositoryRequestObject) (UpdateImageRepositoryResponseObject, error) {
	if h.registry == nil {
		return UpdateImageRepository404JSONResponse{NotFoundJSONResponse{Error: "this server runs no registry"}}, nil
	}
	description := ""
	if req.Body.Description != nil {
		description = *req.Body.Description
	}
	x, err := h.registry.Update(ctx, principal(ctx), req.ID, string(req.Body.Visibility), description)
	switch {
	case errors.Is(err, registry.ErrNotFound):
		return UpdateImageRepository404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, registry.ErrForbidden):
		return UpdateImageRepository403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, registry.ErrInvalid):
		return UpdateImageRepository400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return UpdateImageRepository200JSONResponse(h.imageRepository(x)), nil
}

func (h *handler) DeleteImageRepository(ctx context.Context, req DeleteImageRepositoryRequestObject) (DeleteImageRepositoryResponseObject, error) {
	if h.registry == nil {
		return DeleteImageRepository404JSONResponse{NotFoundJSONResponse{Error: "this server runs no registry"}}, nil
	}
	err := h.registry.Delete(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, registry.ErrNotFound):
		return DeleteImageRepository404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, registry.ErrForbidden):
		return DeleteImageRepository403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteImageRepository204Response{}, nil
}

func (h *handler) SetImageRepositoryCollaborators(ctx context.Context, req SetImageRepositoryCollaboratorsRequestObject) (SetImageRepositoryCollaboratorsResponseObject, error) {
	if h.registry == nil {
		return SetImageRepositoryCollaborators404JSONResponse{NotFoundJSONResponse{Error: "this server runs no registry"}}, nil
	}
	var teamIDs []string
	if req.Body.TeamIds != nil {
		teamIDs = *req.Body.TeamIds
	}
	x, err := h.registry.SetCollaborators(ctx, principal(ctx), req.ID, req.Body.UserIds, teamIDs)
	switch {
	case errors.Is(err, registry.ErrNotFound):
		return SetImageRepositoryCollaborators404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, registry.ErrForbidden):
		return SetImageRepositoryCollaborators403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, registry.ErrInvalid):
		return SetImageRepositoryCollaborators400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return SetImageRepositoryCollaborators200JSONResponse(h.imageRepository(x)), nil
}

func (h *handler) DeleteImageTag(ctx context.Context, req DeleteImageTagRequestObject) (DeleteImageTagResponseObject, error) {
	if h.registry == nil {
		return DeleteImageTag404JSONResponse{NotFoundJSONResponse{Error: "this server runs no registry"}}, nil
	}
	x, err := h.registry.DeleteTag(ctx, principal(ctx), req.ID, req.Tag)
	switch {
	case errors.Is(err, registry.ErrNotFound):
		return DeleteImageTag404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, registry.ErrForbidden):
		return DeleteImageTag403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, registry.ErrInvalid):
		return DeleteImageTag400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteImageTag200JSONResponse(h.imageRepository(x)), nil
}

func (h *handler) imageRepository(x registry.Repository) ImageRepository {
	out := ImageRepository{
		ID:                x.ID,
		Path:              x.Path,
		Name:              h.registryHost + "/" + x.Path,
		Description:       x.Description,
		Visibility:        Visibility(x.Visibility),
		Collaborators:     make([]Person, len(x.Collaborators)),
		CollaboratorTeams: make([]TeamRef, len(x.CollaboratorTeams)),
		TagCount:          x.TagCount,
		CanPush:           x.CanPush,
		CanManage:         x.CanManage,
		CreatedAt:         x.CreatedAt,
		UpdatedAt:         x.UpdatedAt,
	}
	if x.Owner != nil {
		out.Owner = &Person{ID: x.Owner.ID, Username: x.Owner.Username, DisplayName: x.Owner.DisplayName}
	}
	if x.Team != nil {
		out.Team = &TeamRef{ID: x.Team.ID, Slug: x.Team.Slug, Name: x.Team.Name}
	}
	for i, c := range x.Collaborators {
		out.Collaborators[i] = Person{ID: c.ID, Username: c.Username, DisplayName: c.DisplayName}
	}
	for i, c := range x.CollaboratorTeams {
		out.CollaboratorTeams[i] = TeamRef{ID: c.ID, Slug: c.Slug, Name: c.Name}
	}
	if x.Tags != nil {
		tags := make([]ImageTag, len(x.Tags))
		for i, t := range x.Tags {
			tags[i] = ImageTag{Name: t.Name, Digest: t.Digest, MediaType: t.MediaType, SizeBytes: t.Size, UpdatedAt: t.UpdatedAt}
		}
		out.Tags = &tags
	}
	return out
}
