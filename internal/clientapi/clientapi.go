// Package clientapi is Hangar's API for clients that ship apart from the
// server -- the desktop app, scripts -- at /api/v1.
//
// It is the stable counterpart of the web UI's API (internal/frontendapi),
// which ships inside the server with the UI that calls it and so may change
// with every release. This one changes only by adding, within a version;
// openapi.yaml states the rules. It is deliberately small: what a client
// outside the browser needs, and no more.
package clientapi

//go:generate go tool oapi-codegen -config oapi-codegen.yaml openapi.yaml

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"net"
	"net/http"
	"net/url"
	"strings"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/packs"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
	"github.com/csnewman/hangar/internal/version"
	"github.com/csnewman/hangar/internal/workers"
)

// Versions are the versions of this API the server serves, oldest first.
var Versions = []string{"v1"}

// CookieName is the web UI's session cookie, which signs a request in as
// well as an access token does.
const CookieName = "hangar_session"

// Config is what the API is served from.
type Config struct {
	Environments *environments.Manager
	Users        *users.Manager
	Profiles     *profile.Store
	// Packs are the shared files' packs, for an environment's choice of
	// copies; nil lists none. Workers says how big an environment may be;
	// nil says nothing.
	Packs   *packs.Store
	Workers *workers.Manager
	// Templates are what environments are made from; nil lists none.
	Templates *templates.Manager
	// Changes are the environments' changes, for the event stream. Nil
	// streams only what is there when it opens.
	Changes *environments.Changes
	// Tunnels reaches workers, for terminals; nil refuses them as
	// unreachable. Audit records who reached into which environment.
	Tunnels Tunnels
	Audit   *audit.Log
	// SSH is where the SSH gateway is reached. Nil when there is none.
	SSH *SSHGateway
	// HostSuffix is what follows env<short> in the hosts environments' own
	// web servers are reached on. Empty when the server has no public URL.
	HostSuffix string
	Log        *slog.Logger
}

type handler struct {
	envs      *environments.Manager
	users     *users.Manager
	profiles  *profile.Store
	packs     *packs.Store
	workers   *workers.Manager
	templates *templates.Manager
	changes   *environments.Changes
	tunnels   Tunnels
	audit     *audit.Log
	ssh       *SSHGateway
	suffix    string
	log       *slog.Logger
}

var _ StrictServerInterface = (*handler)(nil)

var (
	errUnauthorized = errors.New("not signed in")
	errForbidden    = errors.New("cross-site request refused")
)

type principalKey struct{}

// New returns the API, to be mounted at the root of a mux: its routes carry
// their full /api/v1 paths.
func New(cfg Config) http.Handler {
	h := &handler{envs: cfg.Environments, users: cfg.Users, profiles: cfg.Profiles, packs: cfg.Packs,
		workers: cfg.Workers, templates: cfg.Templates, changes: cfg.Changes, ssh: cfg.SSH,
		tunnels: cfg.Tunnels, audit: cfg.Audit, suffix: cfg.HostSuffix,
		log: cfg.Log}
	if h.changes == nil {
		h.changes = environments.NewChanges()
	}
	strict := NewStrictHandlerWithOptions(h, []StrictMiddlewareFunc{signedIn}, StrictHTTPServerOptions{
		RequestErrorHandlerFunc: func(w http.ResponseWriter, r *http.Request, err error) {
			writeError(w, http.StatusBadRequest, err.Error())
		},
		ResponseErrorHandlerFunc: func(w http.ResponseWriter, r *http.Request, err error) {
			switch {
			case errors.Is(err, errUnauthorized):
				writeError(w, http.StatusUnauthorized, err.Error())
			case errors.Is(err, context.Canceled) || r.Context().Err() != nil:
			default:
				cfg.Log.Error("client API request failed", "method", r.Method, "path", r.URL.Path, "err", err)
				writeError(w, http.StatusInternalServerError, "internal error")
			}
		},
	})
	routes := HandlerWithOptions(strict, StdHTTPServerOptions{
		BaseURL: "/api/v1",
		ErrorHandlerFunc: func(w http.ResponseWriter, r *http.Request, err error) {
			writeError(w, http.StatusBadRequest, err.Error())
		},
	})
	mux := http.NewServeMux()
	mux.Handle("/api/v1/", routes)
	mux.HandleFunc("GET /api/v1/environments/{id}/terminal", h.terminalSocket)
	mux.HandleFunc("GET /api/v1/environments/{id}/code", h.codeSocket)
	mux.HandleFunc("GET /api/v1/environments/{id}/desktop", h.desktopSocket)
	return h.authenticate(mux)
}

// authenticate resolves who a request is from, by access token or session
// cookie, before anything else runs. It refuses nothing but a bad token and
// a cookie-carrying change from another site: whether an operation needs a
// caller is signedIn's decision.
func (h *handler) authenticate(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := r.Context()
		var p *users.Principal
		via := ""
		if bearer, ok := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer "); ok {
			pr, name, err := h.users.AuthenticateToken(ctx, strings.TrimSpace(bearer))
			switch {
			case err == nil:
				p, via = &pr, "access token "+name
			case errors.Is(err, users.ErrNoSession):
				writeError(w, http.StatusUnauthorized, "that access token is not valid")
				return
			default:
				h.log.Error("resolving an access token", "err", err)
				writeError(w, http.StatusInternalServerError, "internal error")
				return
			}
		} else if c, err := r.Cookie(CookieName); err == nil && c.Value != "" {
			// A browser attaches the cookie to requests other sites cause, so
			// a change it carries must come from this server's own pages.
			if !safe(r) {
				writeError(w, http.StatusForbidden, errForbidden.Error())
				return
			}
			pr, err := h.users.Authenticate(ctx, c.Value)
			switch {
			case err == nil:
				p = &pr
			case !errors.Is(err, users.ErrNoSession):
				h.log.Error("resolving a session", "err", err)
				writeError(w, http.StatusInternalServerError, "internal error")
				return
			}
		}
		actor := audit.Actor{IP: clientIP(r), Via: via}
		if p != nil {
			actor.UserID, actor.Name = p.UserID, p.Username
			ctx = context.WithValue(ctx, principalKey{}, *p)
		}
		next.ServeHTTP(w, r.WithContext(audit.WithActor(ctx, actor)))
	})
}

// safe is whether a request may use the session cookie: it reads, or it
// comes from this server's own origin, or from no browser page at all.
func safe(r *http.Request) bool {
	switch r.Method {
	case http.MethodGet, http.MethodHead, http.MethodOptions:
		return true
	}
	if site := r.Header.Get("Sec-Fetch-Site"); site != "" && site != "same-origin" && site != "none" {
		return false
	}
	if origin := r.Header.Get("Origin"); origin != "" {
		u, err := url.Parse(origin)
		if err != nil || u.Host != r.Host {
			return false
		}
	}
	return true
}

// signedIn refuses every operation but the version to a request from nobody.
func signedIn(next StrictHandlerFunc, operation string) StrictHandlerFunc {
	return func(ctx context.Context, w http.ResponseWriter, r *http.Request, request any) (any, error) {
		// Asking the version, and an app signing in, come before any token.
		if operation != "GetVersion" && operation != "StartSignIn" && operation != "FinishSignIn" {
			if _, ok := ctx.Value(principalKey{}).(users.Principal); !ok {
				return nil, errUnauthorized
			}
		}
		return next(ctx, w, r, request)
	}
}

func principal(ctx context.Context) users.Principal {
	p, _ := ctx.Value(principalKey{}).(users.Principal)
	return p
}

func clientIP(r *http.Request) string {
	if f := r.Header.Get("X-Forwarded-For"); f != "" {
		first, _, _ := strings.Cut(f, ",")
		return strings.TrimSpace(first)
	}
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return host
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

func (h *handler) GetVersion(ctx context.Context, _ GetVersionRequestObject) (GetVersionResponseObject, error) {
	return GetVersion200JSONResponse{Versions: Versions, Server: version.String()}, nil
}

func (h *handler) GetMe(ctx context.Context, _ GetMeRequestObject) (GetMeResponseObject, error) {
	u, err := h.users.Get(ctx, principal(ctx).UserID)
	if errors.Is(err, users.ErrNotFound) {
		return nil, errUnauthorized
	}
	if err != nil {
		return nil, err
	}
	me := GetMe200JSONResponse{ID: u.ID, Username: u.Username, DisplayName: u.DisplayName, Admin: u.Admin, SSH: h.ssh}
	if h.suffix != "" {
		me.EnvironmentHostSuffix = &h.suffix
	}
	return me, nil
}

func (h *handler) ListLoginKeys(ctx context.Context, _ ListLoginKeysRequestObject) (ListLoginKeysResponseObject, error) {
	keys, err := h.profiles.LoginKeys(ctx, principal(ctx).UserID)
	if err != nil {
		return nil, err
	}
	out := make(ListLoginKeys200JSONResponse, len(keys))
	for i, k := range keys {
		out[i] = loginKey(k)
	}
	return out, nil
}

func (h *handler) AddLoginKey(ctx context.Context, req AddLoginKeyRequestObject) (AddLoginKeyResponseObject, error) {
	name := ""
	if req.Body.Name != nil {
		name = *req.Body.Name
	}
	k, err := h.profiles.AddLoginKey(ctx, principal(ctx).UserID, name, req.Body.PublicKey)
	switch {
	case errors.Is(err, profile.ErrInvalid):
		return AddLoginKey400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return AddLoginKey201JSONResponse(loginKey(k)), nil
}

func loginKey(k profile.LoginKey) LoginKey {
	return LoginKey{ID: k.ID, Name: k.Name, Fingerprint: k.Fingerprint, CreatedAt: k.CreatedAt}
}

func (h *handler) ListEnvironments(ctx context.Context, _ ListEnvironmentsRequestObject) (ListEnvironmentsResponseObject, error) {
	list, err := h.envs.List(ctx, principal(ctx))
	if err != nil {
		return nil, err
	}
	out := make(ListEnvironments200JSONResponse, 0, len(list))
	for _, e := range list {
		if e.Desired == api.DesiredDeleted {
			continue
		}
		out = append(out, environment(e))
	}
	return out, nil
}

func (h *handler) GetEnvironment(ctx context.Context, req GetEnvironmentRequestObject) (GetEnvironmentResponseObject, error) {
	e, err := h.envs.Get(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return GetEnvironment404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return GetEnvironment200JSONResponse(environment(e)), nil
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
	return DeleteEnvironment200JSONResponse{EnvironmentJSONResponse(environment(*e))}, nil
}

func (h *handler) ResetEnvironment(ctx context.Context, req ResetEnvironmentRequestObject) (ResetEnvironmentResponseObject, error) {
	e, err := h.envs.ResetToTemplate(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return ResetEnvironment404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrInvalid):
		return ResetEnvironment400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrConflict):
		return ResetEnvironment409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return ResetEnvironment200JSONResponse{EnvironmentJSONResponse(environment(e))}, nil
}

func (h *handler) StartEnvironment(ctx context.Context, req StartEnvironmentRequestObject) (StartEnvironmentResponseObject, error) {
	e, err := h.setDesired(ctx, req.ID, api.DesiredRunning)
	switch {
	case errors.Is(err, environments.ErrNotFound) || (err == nil && e == nil):
		return StartEnvironment404JSONResponse{NotFoundJSONResponse{Error: "no such environment"}}, nil
	case errors.Is(err, environments.ErrConflict):
		return StartEnvironment409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return StartEnvironment200JSONResponse{EnvironmentJSONResponse(environment(*e))}, nil
}

func (h *handler) StopEnvironment(ctx context.Context, req StopEnvironmentRequestObject) (StopEnvironmentResponseObject, error) {
	e, err := h.setDesired(ctx, req.ID, api.DesiredStopped)
	switch {
	case errors.Is(err, environments.ErrNotFound) || (err == nil && e == nil):
		return StopEnvironment404JSONResponse{NotFoundJSONResponse{Error: "no such environment"}}, nil
	case errors.Is(err, environments.ErrConflict):
		return StopEnvironment409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return StopEnvironment200JSONResponse{EnvironmentJSONResponse(environment(*e))}, nil
}

func (h *handler) SuspendEnvironment(ctx context.Context, req SuspendEnvironmentRequestObject) (SuspendEnvironmentResponseObject, error) {
	e, err := h.setDesired(ctx, req.ID, api.DesiredSuspended)
	switch {
	case errors.Is(err, environments.ErrNotFound) || (err == nil && e == nil):
		return SuspendEnvironment404JSONResponse{NotFoundJSONResponse{Error: "no such environment"}}, nil
	case errors.Is(err, environments.ErrConflict):
		return SuspendEnvironment409JSONResponse{ConflictJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return SuspendEnvironment200JSONResponse{EnvironmentJSONResponse(environment(*e))}, nil
}

func (h *handler) setDesired(ctx context.Context, id string, d api.DesiredState) (*api.Environment, error) {
	p := principal(ctx)
	exists, err := h.envs.SetDesired(ctx, p, id, d)
	if err != nil || !exists {
		return nil, err
	}
	e, err := h.envs.Get(ctx, p, id)
	if err != nil {
		return nil, err
	}
	return &e, nil
}

func environment(e api.Environment) Environment {
	out := Environment{
		ID:        e.ID,
		ShortID:   e.ShortID,
		Name:      e.Name,
		OwnerID:   e.OwnerID,
		Owner:     e.Owner,
		Template:  e.Template,
		Phase:     string(e.Phase),
		Desired:   string(e.Desired),
		Reason:    optional(e.Reason),
		Worker:    optional(e.Worker),
		CreatedAt: e.CreatedAt,
		UpdatedAt: e.UpdatedAt,
	}
	if e.WorkerID != "" {
		online := e.WorkerOnline
		out.WorkerOnline = &online
	}
	cpus, mem, display := e.CPUs, e.MemoryMiB, string(e.Spec.Display)
	out.CPUs, out.MemoryMiB, out.Display = &cpus, &mem, optional(display)
	out.Gpu = optional(string(e.Spec.GPU))
	if e.Spec.DAX {
		dax := true
		out.Dax = &dax
	}
	out.SwapMiB = e.Spec.SwapMiB
	out.Access = access(e.Spec.Access)
	if len(e.Spec.Placement) > 0 {
		placement := e.Spec.Placement
		out.Placement = &placement
	}
	out.TemplateID = optional(e.TemplateID)
	if len(e.TemplateChanges) > 0 {
		changes := e.TemplateChanges
		out.TemplateChanges = &changes
	}
	out.Image = optional(e.Image)
	out.ImageDigest = optional(e.ImageDigest)
	out.ImageChange = optional(e.ImageChange)
	if r := e.ImageRollback; r != nil {
		out.ImageRollback = &ImageRollback{Digest: r.Digest, At: r.At, SizeBytes: r.SizeBytes}
	}
	out.EditorPath = optional(e.Spec.EditorPath)
	public := e.PortsPublic
	out.PortsPublic = &public
	if e.Phase == api.PhaseRunning {
		out.Stats = stats(e.Stats)
	}
	if len(e.Spec.WebNames) > 0 {
		names := e.Spec.WebNames
		out.WebNames = &names
	}
	if len(e.Spec.Projects) > 1 {
		out.WorkspaceFile = optional(api.WorkspaceFile)
	}
	if t := e.Task; t != nil {
		task := task(*t)
		out.Task = &task
	}
	if u := e.ImageUpdate; u != nil {
		up := &ImageUpdate{Digest: u.Digest, Checked: u.Checked, Error: optional(u.Error)}
		if u.Packages {
			up.Packages = &u.Packages
		}
		if len(u.Conflicts) > 0 {
			c := u.Conflicts
			up.Conflicts = &c
		}
		if u.MoreConflicts > 0 {
			n := u.MoreConflicts
			up.MoreConflicts = &n
		}
		out.ImageUpdate = up
	}
	if len(e.Agents) > 0 {
		agents := make([]AgentSession, len(e.Agents))
		for i, a := range e.Agents {
			agents[i] = AgentSession{ID: a.ID, Agent: a.Agent, Terminal: optional(a.Terminal), State: string(a.State),
				Step: optional(a.Step), Message: optional(a.Message), Since: a.Since, NeedsYou: a.NeedsYou()}
		}
		out.Agents = &agents
	}
	if p := e.Progress; p != nil {
		sp := &StartProgress{Step: string(p.Step)}
		if p.Total > 0 {
			done, total, unit := p.Done, p.Total, p.Unit
			sp.Done, sp.Total, sp.Unit = &done, &total, &unit
			if p.Rate > 0 {
				rate := float32(p.Rate)
				sp.Rate = &rate
			}
		}
		out.Progress = sp
	}
	return out
}

func task(t api.Task) Task {
	return Task{ID: t.ID, Text: t.Text, SetBy: t.SetBy, SetAt: t.SetAt, DoneAt: t.DoneAt}
}

func (h *handler) SetTask(ctx context.Context, req SetTaskRequestObject) (SetTaskResponseObject, error) {
	t, err := h.envs.SetTask(ctx, principal(ctx), req.ID, req.Body.Text)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return SetTask404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case errors.Is(err, environments.ErrInvalid):
		return SetTask400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return SetTask200JSONResponse(task(t)), nil
}

func (h *handler) EndTask(ctx context.Context, req EndTaskRequestObject) (EndTaskResponseObject, error) {
	err := h.envs.EndTask(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return EndTask404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return EndTask204Response{}, nil
}

func (h *handler) ListTasks(ctx context.Context, req ListTasksRequestObject) (ListTasksResponseObject, error) {
	list, err := h.envs.Tasks(ctx, principal(ctx), req.ID)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return ListTasks404JSONResponse{NotFoundJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	out := make(ListTasks200JSONResponse, len(list))
	for i, t := range list {
		out[i] = task(t)
	}
	return out, nil
}

func (h *handler) SeeAgent(ctx context.Context, req SeeAgentRequestObject) (SeeAgentResponseObject, error) {
	err := h.envs.SeenAgent(ctx, principal(ctx), req.ID, req.Session)
	switch {
	case errors.Is(err, environments.ErrNotFound):
		return SeeAgent404JSONResponse{NotFoundJSONResponse{Error: "no such session"}}, nil
	case err != nil:
		return nil, err
	}
	return SeeAgent204Response{}, nil
}

// access is what an environment is kept from, or nothing when it is given
// everything.
func access(a api.Access) *Access {
	if a == (api.Access{}) {
		return nil
	}
	set := func(b bool) *bool {
		if !b {
			return nil
		}
		return &b
	}
	return &Access{NoSelfAttached: set(a.NoSelfAttached), NoSensitiveFiles: set(a.NoSensitiveFiles),
		NoSSHKeys: set(a.NoSSHKeys), NoRegistry: set(a.NoRegistry), NoEditorTrust: set(a.NoEditorTrust)}
}
