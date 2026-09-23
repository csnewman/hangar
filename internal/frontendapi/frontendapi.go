// Package frontendapi serves the web UI's API under /api/frontend.
//
// openapi.yaml is the source of truth. The Go interface this package
// implements is generated from it, as are the web UI's TypeScript types, and
// every request is checked against it before a handler runs, so the spec
// cannot describe one API while the server accepts another.
//
// The API is unversioned: it ships in the same binary as the UI that calls
// it, so the two cannot be out of step. Nothing else should call it.
package frontendapi

//go:generate go tool oapi-codegen -config oapi-codegen.yaml openapi.yaml

import (
	"context"
	_ "embed"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"strings"

	"github.com/getkin/kin-openapi/openapi3"
	"github.com/getkin/kin-openapi/openapi3filter"
	middleware "github.com/oapi-codegen/nethttp-middleware"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/workers"
)

//go:embed openapi.yaml
var spec []byte

// Spec returns the API's OpenAPI document.
func Spec() []byte { return spec }

type handler struct {
	envs    *environments.Manager
	workers *workers.Manager
	log     *slog.Logger
}

var _ StrictServerInterface = (*handler)(nil)

// New returns the API, to be mounted at the root of a mux: its routes carry
// their full /api/frontend paths.
func New(envs *environments.Manager, wm *workers.Manager, log *slog.Logger) (http.Handler, error) {
	doc, err := openapi3.NewLoader().LoadFromData(spec)
	if err != nil {
		return nil, err
	}
	if err := doc.Validate(context.Background()); err != nil {
		return nil, err
	}

	h := &handler{envs: envs, workers: wm, log: log}
	strict := NewStrictHandlerWithOptions(h, nil, StrictHTTPServerOptions{
		RequestErrorHandlerFunc: func(w http.ResponseWriter, r *http.Request, err error) {
			writeError(w, http.StatusBadRequest, err.Error())
		},
		ResponseErrorHandlerFunc: func(w http.ResponseWriter, r *http.Request, err error) {
			if errors.Is(err, context.Canceled) {
				return
			}
			log.Error("frontend request failed", "method", r.Method, "path", r.URL.Path, "err", err)
			writeError(w, http.StatusInternalServerError, "internal error")
		},
	})
	routes := HandlerWithOptions(strict, StdHTTPServerOptions{
		ErrorHandlerFunc: func(w http.ResponseWriter, r *http.Request, err error) {
			writeError(w, http.StatusBadRequest, err.Error())
		},
	})

	validate := middleware.OapiRequestValidatorWithOptions(doc, &middleware.Options{
		DoNotValidateServers: true,
		ErrorHandlerWithOpts: func(ctx context.Context, err error, w http.ResponseWriter, r *http.Request, opts middleware.ErrorHandlerOpts) {
			if opts.MatchedRoute == nil {
				writeError(w, http.StatusNotFound, "no such endpoint")
				return
			}
			writeError(w, opts.StatusCode, describe(err))
		},
	})
	return validate(routes), nil
}

// describe turns a validation failure into a message fit to show a person:
// the field and what is wrong with it, without the schema and value the
// validator appends.
func describe(err error) string {
	var se *openapi3.SchemaError
	if errors.As(err, &se) {
		if field := strings.Join(se.JSONPointer(), "."); field != "" {
			return field + ": " + se.Reason
		}
		return se.Reason
	}
	var re *openapi3filter.RequestError
	if errors.As(err, &re) {
		if re.Parameter != nil {
			return re.Parameter.Name + ": " + re.Reason
		}
		if re.Reason != "" {
			return re.Reason
		}
	}
	return err.Error()
}

func writeError(w http.ResponseWriter, status int, msg string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(Error{Error: msg})
}

func (h *handler) ListEnvironments(ctx context.Context, _ ListEnvironmentsRequestObject) (ListEnvironmentsResponseObject, error) {
	list, err := h.envs.List(ctx)
	if err != nil {
		return nil, err
	}
	out := make(ListEnvironments200JSONResponse, len(list))
	for i, e := range list {
		out[i] = environment(e)
	}
	return out, nil
}

func (h *handler) CreateEnvironment(ctx context.Context, req CreateEnvironmentRequestObject) (CreateEnvironmentResponseObject, error) {
	e, err := h.envs.Create(ctx, api.CreateEnvironment{
		Name:      req.Body.Name,
		Image:     req.Body.Image,
		CPUs:      req.Body.CPUs,
		MemoryMiB: req.Body.MemoryMiB,
	})
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

func (h *handler) GetEnvironment(ctx context.Context, req GetEnvironmentRequestObject) (GetEnvironmentResponseObject, error) {
	e, err := h.envs.Get(ctx, req.ID)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return GetEnvironment404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return GetEnvironment200JSONResponse(environment(e)), nil
}

func (h *handler) StartEnvironment(ctx context.Context, req StartEnvironmentRequestObject) (StartEnvironmentResponseObject, error) {
	e, err := h.setDesired(ctx, req.ID, api.DesiredRunning)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return StartEnvironment404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrConflict):
		return StartEnvironment409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return StartEnvironment200JSONResponse(environment(*e)), nil
}

func (h *handler) StopEnvironment(ctx context.Context, req StopEnvironmentRequestObject) (StopEnvironmentResponseObject, error) {
	e, err := h.setDesired(ctx, req.ID, api.DesiredStopped)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return StopEnvironment404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrConflict):
		return StopEnvironment409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return StopEnvironment200JSONResponse(environment(*e)), nil
}

func (h *handler) DeleteEnvironment(ctx context.Context, req DeleteEnvironmentRequestObject) (DeleteEnvironmentResponseObject, error) {
	e, err := h.setDesired(ctx, req.ID, api.DesiredDeleted)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return DeleteEnvironment404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	case e == nil:
		return DeleteEnvironment204Response{}, nil
	}
	return DeleteEnvironment200JSONResponse(environment(*e)), nil
}

// setDesired returns the environment as it stands afterwards, or nil if the
// change removed it outright.
func (h *handler) setDesired(ctx context.Context, id string, d api.DesiredState) (*api.Environment, error) {
	exists, err := h.envs.SetDesired(ctx, id, d)
	if err != nil || !exists {
		return nil, err
	}
	e, err := h.envs.Get(ctx, id)
	if err != nil {
		return nil, err
	}
	return &e, nil
}

func (h *handler) ListWorkers(ctx context.Context, _ ListWorkersRequestObject) (ListWorkersResponseObject, error) {
	list, err := h.workers.List(ctx)
	if err != nil {
		return nil, err
	}
	out := make(ListWorkers200JSONResponse, len(list))
	for i, w := range list {
		out[i] = worker(w)
	}
	return out, nil
}

func (h *handler) RevokeWorker(ctx context.Context, req RevokeWorkerRequestObject) (RevokeWorkerResponseObject, error) {
	err := h.workers.Revoke(ctx, req.ID)
	switch {
	case errors.Is(err, workers.ErrNotFound):
		return RevokeWorker404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return RevokeWorker204Response{}, nil
}

func (h *handler) DeleteWorker(ctx context.Context, req DeleteWorkerRequestObject) (DeleteWorkerResponseObject, error) {
	err := h.workers.Delete(ctx, req.ID)
	switch {
	case errors.Is(err, workers.ErrNotFound):
		return DeleteWorker404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, workers.ErrConflict):
		return DeleteWorker409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return DeleteWorker204Response{}, nil
}

func environment(e api.Environment) Environment {
	return Environment{
		ID:        e.ID,
		Name:      e.Name,
		Image:     e.Image,
		CPUs:      e.CPUs,
		MemoryMiB: e.MemoryMiB,
		Desired:   DesiredState(e.Desired),
		Phase:     Phase(e.Phase),
		Reason:    optional(e.Reason),
		WorkerID:  optional(e.WorkerID),
		Worker:    optional(e.Worker),
		CreatedAt: e.CreatedAt,
		UpdatedAt: e.UpdatedAt,
	}
}

func worker(w api.Worker) Worker {
	unknown := w.Unknown
	if unknown == nil {
		unknown = []string{}
	}
	return Worker{
		ID:         w.ID,
		Name:       w.Name,
		Labels:     w.Labels,
		Capacity:   Resources{CPUs: w.Capacity.CPUs, MemoryMiB: w.Capacity.MemoryMiB},
		Allocated:  Resources{CPUs: w.Allocated.CPUs, MemoryMiB: w.Allocated.MemoryMiB},
		Online:     w.Online,
		Revoked:    w.Revoked,
		Unknown:    unknown,
		LastSeenAt: w.LastSeenAt,
		CreatedAt:  w.CreatedAt,
	}
}

func optional(s string) *string {
	if s == "" {
		return nil
	}
	return &s
}
