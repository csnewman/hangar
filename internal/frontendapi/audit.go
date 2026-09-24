package frontendapi

import (
	"context"

	"github.com/csnewman/hangar/internal/audit"
)

func (h *handler) ListAudit(ctx context.Context, req ListAuditRequestObject) (ListAuditResponseObject, error) {
	p := principal(ctx)
	q := audit.Query{}
	if req.Params.Subject != nil {
		q.Subjects = *req.Params.Subject
	}
	if req.Params.Before != nil {
		q.Before = *req.Params.Before
	}
	if req.Params.Limit != nil {
		q.Limit = *req.Params.Limit
	}
	if !p.Admin {
		// What is theirs, about them, or by them.
		q.AnyOf = []string{"owner:" + p.UserID, "user:" + p.UserID, "actor:" + p.UserID}
	}
	entries, err := h.audit.List(ctx, q)
	if err != nil {
		return nil, err
	}
	out := make(ListAudit200JSONResponse, len(entries))
	for i, e := range entries {
		actorID := e.ActorUser
		if e.ActorWorker != "" {
			actorID = e.ActorWorker
		}
		ev := AuditEvent{
			ID:       e.ID,
			At:       e.At,
			Actor:    AuditActor{Kind: AuditActorKind(e.ActorKind), ID: optional(actorID), Name: e.ActorName},
			Action:   e.Action,
			Target:   AuditRef{Type: e.Target.Type, ID: e.Target.ID, Name: e.Target.Name},
			Subjects: e.Subjects,
			Details:  e.Details,
		}
		if p.Admin {
			ev.IP = optional(e.IP)
		}
		out[i] = ev
	}
	return out, nil
}
