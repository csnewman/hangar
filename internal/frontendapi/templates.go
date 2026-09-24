package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/templates"
)

func (h *handler) ListTemplates(ctx context.Context, _ ListTemplatesRequestObject) (ListTemplatesResponseObject, error) {
	list, err := h.templates.List(ctx, principal(ctx))
	if err != nil {
		return nil, err
	}
	out := make(ListTemplates200JSONResponse, len(list))
	for i, t := range list {
		out[i] = template(t)
	}
	return out, nil
}

func (h *handler) GetTemplate(ctx context.Context, req GetTemplateRequestObject) (GetTemplateResponseObject, error) {
	t, err := h.templates.Get(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, templates.ErrNotFound):
		return GetTemplate404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return GetTemplate200JSONResponse(template(t)), nil
}

func (h *handler) CreateTemplate(ctx context.Context, req CreateTemplateRequestObject) (CreateTemplateResponseObject, error) {
	t, err := h.templates.Create(ctx, principal(ctx), templateInput(*req.Body))
	switch {
	case errors.Is(err, templates.ErrInvalid):
		return CreateTemplate400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, templates.ErrConflict):
		return CreateTemplate409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return CreateTemplate201JSONResponse(template(t)), nil
}

func (h *handler) UpdateTemplate(ctx context.Context, req UpdateTemplateRequestObject) (UpdateTemplateResponseObject, error) {
	t, err := h.templates.Update(ctx, principal(ctx), req.ID, templateInput(*req.Body))
	switch {
	case errors.Is(err, templates.ErrNotFound):
		return UpdateTemplate404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, templates.ErrForbidden):
		return UpdateTemplate403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, templates.ErrInvalid):
		return UpdateTemplate400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, templates.ErrConflict):
		return UpdateTemplate409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return UpdateTemplate200JSONResponse(template(t)), nil
}

func (h *handler) DeleteTemplate(ctx context.Context, req DeleteTemplateRequestObject) (DeleteTemplateResponseObject, error) {
	err := h.templates.Delete(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, templates.ErrNotFound):
		return DeleteTemplate404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, templates.ErrForbidden):
		return DeleteTemplate403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteTemplate204Response{}, nil
}

func (h *handler) SetTemplateCollaborators(ctx context.Context, req SetTemplateCollaboratorsRequestObject) (SetTemplateCollaboratorsResponseObject, error) {
	t, err := h.templates.SetCollaborators(ctx, principal(ctx), req.ID, req.Body.UserIds)
	switch {
	case errors.Is(err, templates.ErrNotFound):
		return SetTemplateCollaborators404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, templates.ErrForbidden):
		return SetTemplateCollaborators403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, templates.ErrInvalid):
		return SetTemplateCollaborators400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return SetTemplateCollaborators200JSONResponse(template(t)), nil
}

func (h *handler) ListPeople(ctx context.Context, _ ListPeopleRequestObject) (ListPeopleResponseObject, error) {
	list, err := h.users.Directory(ctx)
	if err != nil {
		return nil, err
	}
	out := make(ListPeople200JSONResponse, len(list))
	for i, u := range list {
		out[i] = Person{ID: u.ID, Username: u.Username, DisplayName: u.DisplayName}
	}
	return out, nil
}

func templateInput(in TemplateInput) templates.Input {
	out := templates.Input{
		Name:       in.Name,
		Visibility: templates.Visibility(in.Visibility),
		Spec:       api.TemplateSpec{Spec: fromSpec(in.Spec)},
	}
	if in.Description != nil {
		out.Description = *in.Description
	}
	if in.NamePattern != nil {
		out.Spec.NamePattern = *in.NamePattern
	}
	if in.NameHint != nil {
		out.Spec.NameHint = *in.NameHint
	}
	return out
}

func template(t templates.Template) Template {
	collaborators := make([]Person, len(t.Collaborators))
	for i, c := range t.Collaborators {
		collaborators[i] = person(c)
	}
	return Template{
		ID:            t.ID,
		Name:          t.Name,
		Description:   t.Description,
		Visibility:    Visibility(t.Visibility),
		Owner:         person(t.Owner),
		Collaborators: collaborators,
		Spec:          toSpec(t.Spec.Spec),
		NamePattern:   optional(t.Spec.NamePattern),
		NameHint:      optional(t.Spec.NameHint),
		Environments:  t.Environments,
		CanEdit:       t.CanEdit,
		CanManage:     t.CanManage,
		CreatedAt:     t.CreatedAt,
		UpdatedAt:     t.UpdatedAt,
	}
}

func person(p templates.Person) Person {
	return Person{ID: p.ID, Username: p.Username, DisplayName: p.DisplayName}
}

func toSpec(s api.Spec) Spec {
	repos := make([]Repo, len(s.Repos))
	for i, r := range s.Repos {
		repos[i] = Repo{URL: r.URL, Ref: optional(r.Ref), Path: r.Path, Branch: optional(r.Branch)}
	}
	placement := s.Placement
	if placement == nil {
		placement = map[string]string{}
	}
	return Spec{
		Image:      s.Image,
		CPUs:       s.CPUs,
		MemoryMiB:  s.MemoryMiB,
		Display:    Display(s.Display),
		GPU:        GPU(s.GPU),
		Repos:      repos,
		EditorPath: optional(s.EditorPath),
		Untrusted:  optionalBool(s.Untrusted),
		Placement:  placement,
	}
}

func fromSpec(s Spec) api.Spec {
	repos := make([]api.Repo, len(s.Repos))
	for i, r := range s.Repos {
		repos[i] = api.Repo{URL: r.URL, Ref: deref(r.Ref), Path: r.Path, Branch: deref(r.Branch)}
	}
	return api.Spec{
		Image:      s.Image,
		CPUs:       s.CPUs,
		MemoryMiB:  s.MemoryMiB,
		Display:    api.Display(s.Display),
		GPU:        api.GPU(s.GPU),
		Repos:      repos,
		EditorPath: deref(s.EditorPath),
		Untrusted:  s.Untrusted != nil && *s.Untrusted,
		Placement:  s.Placement,
	}
}

func optionalBool(b bool) *bool {
	if !b {
		return nil
	}
	return &b
}

func deref(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}
