package frontendapi

import (
	"context"
	"errors"

	"github.com/csnewman/hangar/internal/teams"
)

func (h *handler) ListTeams(ctx context.Context, _ ListTeamsRequestObject) (ListTeamsResponseObject, error) {
	list, err := h.teams.List(ctx, principal(ctx))
	if err != nil {
		return nil, err
	}
	out := make(ListTeams200JSONResponse, len(list))
	for i, t := range list {
		out[i] = team(t)
	}
	return out, nil
}

func (h *handler) GetTeam(ctx context.Context, req GetTeamRequestObject) (GetTeamResponseObject, error) {
	t, err := h.teams.Get(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, teams.ErrNotFound):
		return GetTeam404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return GetTeam200JSONResponse(team(t)), nil
}

func (h *handler) CreateTeam(ctx context.Context, req CreateTeamRequestObject) (CreateTeamResponseObject, error) {
	in := teams.Input{Slug: req.Body.Slug, Name: req.Body.Name}
	if req.Body.Description != nil {
		in.Description = *req.Body.Description
	}
	t, err := h.teams.Create(ctx, principal(ctx), in)
	switch {
	case errors.Is(err, teams.ErrInvalid):
		return CreateTeam400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, teams.ErrForbidden):
		return CreateTeam403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, teams.ErrConflict):
		return CreateTeam409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return CreateTeam201JSONResponse(team(t)), nil
}

func (h *handler) UpdateTeam(ctx context.Context, req UpdateTeamRequestObject) (UpdateTeamResponseObject, error) {
	in := teams.Input{Name: req.Body.Name}
	if req.Body.Description != nil {
		in.Description = *req.Body.Description
	}
	t, err := h.teams.Update(ctx, principal(ctx), req.ID, in)
	switch {
	case errors.Is(err, teams.ErrNotFound):
		return UpdateTeam404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, teams.ErrForbidden):
		return UpdateTeam403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, teams.ErrInvalid):
		return UpdateTeam400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return UpdateTeam200JSONResponse(team(t)), nil
}

func (h *handler) DeleteTeam(ctx context.Context, req DeleteTeamRequestObject) (DeleteTeamResponseObject, error) {
	err := h.teams.Delete(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, teams.ErrNotFound):
		return DeleteTeam404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, teams.ErrForbidden):
		return DeleteTeam403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, teams.ErrConflict):
		return DeleteTeam409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteTeam204Response{}, nil
}

func (h *handler) SetTeamMember(ctx context.Context, req SetTeamMemberRequestObject) (SetTeamMemberResponseObject, error) {
	t, err := h.teams.SetMember(ctx, principal(ctx), req.ID, req.User, teams.Role(req.Body.Role))
	switch {
	case errors.Is(err, teams.ErrNotFound):
		return SetTeamMember404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, teams.ErrForbidden):
		return SetTeamMember403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, teams.ErrInvalid):
		return SetTeamMember400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return SetTeamMember200JSONResponse(team(t)), nil
}

func (h *handler) RemoveTeamMember(ctx context.Context, req RemoveTeamMemberRequestObject) (RemoveTeamMemberResponseObject, error) {
	t, err := h.teams.RemoveMember(ctx, principal(ctx), req.ID, req.User)
	switch {
	case errors.Is(err, teams.ErrNotFound):
		return RemoveTeamMember404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, teams.ErrForbidden):
		return RemoveTeamMember403JSONResponse{ForbiddenJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, teams.ErrInvalid):
		return RemoveTeamMember400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return RemoveTeamMember200JSONResponse(team(t)), nil
}

func team(t teams.Team) Team {
	out := Team{
		ID:          t.ID,
		Slug:        t.Slug,
		Name:        t.Name,
		Description: t.Description,
		MemberCount: t.MemberCount,
		Templates:   t.Templates,
		CanManage:   t.CanManage,
		CreatedAt:   t.CreatedAt,
	}
	if t.Role != "" {
		r := TeamRole(t.Role)
		out.Role = &r
	}
	if t.Members != nil {
		members := make([]TeamMember, len(t.Members))
		for i, m := range t.Members {
			members[i] = TeamMember{
				Person: Person{ID: m.ID, Username: m.Username, DisplayName: m.DisplayName},
				Role:   TeamRole(m.Role),
			}
		}
		out.Members = &members
	}
	return out
}
