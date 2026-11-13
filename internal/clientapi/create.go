package clientapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/environments"
)

// ListTemplates is what the user may make an environment from.
func (h *handler) ListTemplates(ctx context.Context, _ ListTemplatesRequestObject) (ListTemplatesResponseObject, error) {
	if h.templates == nil {
		return ListTemplates200JSONResponse{}, nil
	}
	list, err := h.templates.List(ctx, principal(ctx))
	if err != nil {
		return nil, err
	}
	out := make(ListTemplates200JSONResponse, 0, len(list))
	for _, t := range list {
		s := t.Spec
		repos := make([]Repo, len(s.Repos))
		for i, r := range s.Repos {
			repos[i] = Repo{URL: r.URL, Ref: optional(r.Ref), Path: r.Path, Branch: optional(r.Branch)}
		}
		out = append(out, Template{
			ID: t.ID, Name: t.Name, Description: t.Description, Owner: t.Owner.Username,
			CPUs: s.CPUs, MemoryMiB: s.MemoryMiB, Display: string(s.Display), Gpu: string(s.GPU),
			Image: s.Image, Repos: repos, SwapMiB: s.SwapMiB,
			NamePattern: optional(s.NamePattern), NameHint: optional(s.NameHint),
		})
		o := &out[len(out)-1]
		if t.Team != nil {
			o.Team = optional(t.Team.Name)
		}
		if s.DAX {
			dax := true
			o.Dax = &dax
		}
		if s.MemoryReclaim {
			reclaim := true
			o.MemoryReclaim = &reclaim
		}
		if len(s.Projects) > 0 {
			projects := s.Projects
			o.Projects = &projects
		}
	}
	return out, nil
}

// CreateEnvironment makes an environment from a template, with its machine
// and repositories in place of the template's if given.
func (h *handler) CreateEnvironment(ctx context.Context, req CreateEnvironmentRequestObject) (CreateEnvironmentResponseObject, error) {
	b := req.Body
	in := api.CreateEnvironment{TemplateID: b.TemplateID, Name: b.Name}
	if m := b.Settings; m != nil {
		in.Machine = &api.Machine{CPUs: m.CPUs, MemoryMiB: m.MemoryMiB, Display: api.Display(m.Display),
			GPU: api.GPU(m.Gpu), DAX: m.Dax != nil && *m.Dax, SwapMiB: m.SwapMiB,
			MemoryReclaim: m.MemoryReclaim != nil && *m.MemoryReclaim}
	}
	if rs := b.Repos; rs != nil {
		repos := make([]api.Repo, len(*rs))
		for i, r := range *rs {
			repos[i] = api.Repo{URL: r.URL, Ref: deref(r.Ref), Path: r.Path, Branch: deref(r.Branch)}
		}
		in.Repos = &repos
	}
	e, err := h.envs.Create(ctx, principal(ctx), in)
	switch {
	case errors.Is(err, environments.ErrInvalid):
		return CreateEnvironment400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrConflict):
		return CreateEnvironment409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return CreateEnvironment201JSONResponse(environment(e)), nil
}

func deref(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}
