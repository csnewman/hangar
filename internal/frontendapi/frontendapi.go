// Package frontendapi serves the web UI's API under /api/frontend.
//
// openapi.yaml is the source of truth. The Go interface this package
// implements is generated from it, as are the web UI's TypeScript types, and
// every request is checked against it before a handler runs, so the spec
// cannot describe one API while the server accepts another. Who may call
// each operation is read from the spec too; see auth.go.
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

	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
	"github.com/csnewman/hangar/internal/workers"
)

//go:embed openapi.yaml
var spec []byte

// Document returns the API's OpenAPI document.
func Document() []byte { return spec }

// Config is what the API is served from.
type Config struct {
	Environments *environments.Manager
	Templates    *templates.Manager
	Workers      *workers.Manager
	Users        *users.Manager
	// Tunnels reaches workers, for terminals. Nil leaves terminals
	// unavailable.
	Tunnels Tunnels
	// Editors signs users in to environments' editors. Nil leaves
	// environments without an editor.
	Editors Editors
	// AutoSignIn, for development only, signs every request that has no
	// session in as this user. Empty requires signing in.
	AutoSignIn string
	Log        *slog.Logger
}

type handler struct {
	envs      *environments.Manager
	templates *templates.Manager
	workers   *workers.Manager
	users     *users.Manager
	tunnels   Tunnels
	editors   Editors
	// autoSignIn is Config.AutoSignIn.
	autoSignIn string
	log        *slog.Logger
}

var _ StrictServerInterface = (*handler)(nil)

// New returns the API, to be mounted at the root of a mux: its routes carry
// their full /api/frontend paths.
func New(cfg Config) (http.Handler, error) {
	doc, err := openapi3.NewLoader().LoadFromData(spec)
	if err != nil {
		return nil, err
	}
	if err := doc.Validate(context.Background()); err != nil {
		return nil, err
	}
	rules, err := accessRules(doc)
	if err != nil {
		return nil, err
	}

	h := &handler{envs: cfg.Environments, templates: cfg.Templates, workers: cfg.Workers, users: cfg.Users,
		tunnels: cfg.Tunnels, editors: cfg.Editors, autoSignIn: cfg.AutoSignIn, log: cfg.Log}
	strict := NewStrictHandlerWithOptions(h, []StrictMiddlewareFunc{rules.enforce}, StrictHTTPServerOptions{
		RequestErrorHandlerFunc: func(w http.ResponseWriter, r *http.Request, err error) {
			writeError(w, http.StatusBadRequest, err.Error())
		},
		ResponseErrorHandlerFunc: func(w http.ResponseWriter, r *http.Request, err error) {
			switch {
			case errors.Is(err, errUnauthorized):
				writeError(w, http.StatusUnauthorized, "not signed in")
			case errors.Is(err, errForbidden):
				writeError(w, http.StatusForbidden, "only an administrator may do this")
			case errors.Is(err, context.Canceled):
			default:
				cfg.Log.Error("frontend request failed", "method", r.Method, "path", r.URL.Path, "err", err)
				writeError(w, http.StatusInternalServerError, "internal error")
			}
		},
	})
	routes := HandlerWithOptions(strict, StdHTTPServerOptions{
		ErrorHandlerFunc: func(w http.ResponseWriter, r *http.Request, err error) {
			writeError(w, http.StatusBadRequest, err.Error())
		},
	})

	validate := middleware.OapiRequestValidatorWithOptions(doc, &middleware.Options{
		DoNotValidateServers: true,
		// The validator checks shapes. Whether the caller is signed in, and
		// as whom, is decided by rules.enforce, which knows about sessions.
		Options: openapi3filter.Options{AuthenticationFunc: openapi3filter.NoopAuthenticationFunc},
		ErrorHandlerWithOpts: func(ctx context.Context, err error, w http.ResponseWriter, r *http.Request, opts middleware.ErrorHandlerOpts) {
			if opts.MatchedRoute == nil {
				writeError(w, http.StatusNotFound, "no such endpoint")
				return
			}
			writeError(w, opts.StatusCode, describe(err))
		},
	})
	// The terminal is a WebSocket, which the spec does not describe; it has
	// a route of its own beside the validated API.
	mux := http.NewServeMux()
	mux.HandleFunc("GET /api/frontend/environments/{id}/terminal", h.terminalSocket)
	mux.HandleFunc("GET /api/frontend/environments/{id}/desktop", h.desktopSocket)
	mux.HandleFunc("GET /api/frontend/environments/{id}/code", h.codeSocket)
	mux.Handle("/", validate(routes))
	return h.session(sameOrigin(mux)), nil
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

func optional(s string) *string {
	if s == "" {
		return nil
	}
	return &s
}
