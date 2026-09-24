// Package server is hangar-server: the web UI and its API, the worker API,
// and placement.
//
// Nothing here holds state that another replica would need. What a worker
// reports goes to the database, what a worker should hold is read from it,
// and replicas learn of each other's changes through Postgres notifications.
// A worker may land on any replica on every request.
package server

import (
	"bufio"
	"context"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"io/fs"
	"log/slog"
	"net"
	"net/http"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/editor"
	"github.com/csnewman/hangar/internal/environments"
	"github.com/csnewman/hangar/internal/frontendapi"
	"github.com/csnewman/hangar/internal/placement"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/tunnel"
	"github.com/csnewman/hangar/internal/users"
	"github.com/csnewman/hangar/internal/workers"
)

// Config is what a server needs to run.
type Config struct {
	DB *db.DB
	// BootstrapToken is the shared secret a worker presents once, to be
	// issued a credential of its own. Empty disables registration.
	BootstrapToken string
	// Web is the built UI. Nil serves a page saying it was not built.
	Web fs.FS
	// PublicURL is Hangar's origin as browsers reach it, such as
	// https://hangar.example.com. Each environment's editor is served on a
	// subdomain of its host. Empty leaves environments without an editor.
	PublicURL string
	// AutoSignIn, for development only, signs every visitor in as this
	// existing user without a password. Empty requires signing in.
	AutoSignIn string
	// Sealer encrypts the secrets users keep in their profiles. Nil keeps
	// none: credentials and SSH keys are refused.
	Sealer *profile.Sealer
	Log    *slog.Logger
}

type Server struct {
	db        *db.DB
	frontend  http.Handler
	editors   *editor.Gateway
	edits     *editor.Manager
	tunnels   *tunnel.Registry
	workers   *workers.Manager
	users     *users.Manager
	placement *placement.Manager
	bootstrap string
	web       fs.FS
	log       *slog.Logger
	waits     *waiters
	placeKick chan struct{}
	sessions  *profile.Sessions
}

func New(cfg Config) (*Server, error) {
	log := cfg.Log
	if log == nil {
		log = slog.Default()
	}
	wm := workers.NewManager(cfg.DB)
	um := users.NewManager(cfg.DB)
	tunnels := tunnel.NewRegistry(log)
	edits := editor.NewManager(cfg.DB)
	profiles := profile.NewStore(cfg.DB, cfg.Sealer)
	var editors *editor.Gateway
	if cfg.PublicURL != "" {
		var err error
		if editors, err = editor.NewGateway(edits, tunnels, cfg.PublicURL, log); err != nil {
			return nil, err
		}
	}
	cfgAPI := frontendapi.Config{
		Tunnels:      tunnels,
		Environments: environments.NewManager(cfg.DB),
		Templates:    templates.NewManager(cfg.DB),
		Workers:      wm,
		Users:        um,
		Profiles:     profiles,
		AutoSignIn:   cfg.AutoSignIn,
		Log:          log,
	}
	// A nil Gateway would be a non-nil interface.
	if editors != nil {
		cfgAPI.Editors = editors
	}
	frontend, err := frontendapi.New(cfgAPI)
	if err != nil {
		return nil, err
	}
	return &Server{
		db:        cfg.DB,
		frontend:  frontend,
		editors:   editors,
		edits:     edits,
		tunnels:   tunnels,
		workers:   wm,
		users:     um,
		placement: placement.NewManager(cfg.DB),
		bootstrap: cfg.BootstrapToken,
		web:       cfg.Web,
		log:       log,
		waits:     newWaiters(),
		placeKick: make(chan struct{}, 1),
		sessions:  profile.NewSessions(profiles, tunnels, log),
	}, nil
}

// Run carries out the server's background work until ctx ends: listening for
// other replicas' changes, and placing environments.
func (s *Server) Run(ctx context.Context) {
	go s.db.Listen(ctx, func(channel, payload string) {
		switch channel {
		case workers.Channel:
			s.waits.wake(payload)
			// A report may have an environment running that has no
			// profile session yet.
			s.sessions.Kick()
		case profile.Channel:
			s.sessions.Changed(payload)
		case placement.Channel, workers.CapacityChannel:
			s.kickPlacement()
		case "":
			// Reconnected: anything sent meanwhile is lost, so assume
			// everything changed.
			s.waits.wakeAll()
			s.kickPlacement()
			s.sessions.Changed("")
			s.sessions.Kick()
		}
	}, workers.Channel, workers.CapacityChannel, placement.Channel, profile.Channel)
	go s.sessions.Run(ctx)
	go s.pruneSessions(ctx)
	s.placementLoop(ctx)
}

// Users returns the server's user manager, for creating the first
// administrator before the server is serving.
func (s *Server) Users() *users.Manager { return s.users }

// pruneSessions deletes expired sessions now and then. They are refused
// whether or not they are deleted; this only keeps the table small.
func (s *Server) pruneSessions(ctx context.Context) {
	t := time.NewTicker(time.Hour)
	defer t.Stop()
	for {
		if n, err := s.users.PruneSessions(ctx); err != nil && ctx.Err() == nil {
			s.log.Warn("pruning sessions", "err", err)
		} else if n > 0 {
			s.log.Info("pruned expired sessions", "count", n)
		}
		if n, err := s.edits.Prune(ctx); err != nil && ctx.Err() == nil {
			s.log.Warn("pruning editor sessions", "err", err)
		} else if n > 0 {
			s.log.Info("pruned expired editor sessions", "count", n)
		}
		select {
		case <-ctx.Done():
			return
		case <-t.C:
		}
	}
}

func (s *Server) kickPlacement() {
	select {
	case s.placeKick <- struct{}{}:
	default:
	}
}

// placementLoop places on every notification and on a timer. The timer
// catches what no notification announces, such as a worker coming back
// online by simply reporting in again.
func (s *Server) placementLoop(ctx context.Context) {
	t := time.NewTicker(10 * time.Second)
	defer t.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-t.C:
		case <-s.placeKick:
		}
		n, err := s.placement.Place(ctx)
		if err != nil {
			if ctx.Err() == nil {
				s.log.Error("placement failed", "err", err)
			}
			continue
		}
		if n > 0 {
			s.log.Info("placed environments", "count", n)
		}
	}
}

// Handler serves the whole server: /api/frontend for the web UI's API,
// /api/worker/v1 for workers, and everything outside /api/ for the UI itself.
//
// The worker API is versioned and the frontend API is not. A worker and the
// server are upgraded separately, so the protocol between them has to say
// which version it speaks; the UI ships inside this binary with the API it
// calls.
func (s *Server) Handler() http.Handler {
	mux := http.NewServeMux()

	mux.HandleFunc("GET /api/healthz", s.healthz)

	mux.Handle("/api/frontend/", s.frontend)

	mux.HandleFunc("POST /api/worker/v1/register", s.registerWorker)
	mux.Handle("GET /api/worker/v1/desired", s.workerAuth(s.workerDesired))
	mux.Handle("PUT /api/worker/v1/status", s.workerAuth(s.workerStatus))
	mux.Handle("GET /api/worker/v1/tunnel", s.workerAuth(func(w http.ResponseWriter, r *http.Request) {
		s.tunnels.Accept(w, r, workerID(r))
	}))

	mux.HandleFunc("/api/", func(w http.ResponseWriter, r *http.Request) {
		writeError(w, http.StatusNotFound, "no such endpoint")
	})
	mux.Handle("/", s.webHandler())

	api := s.logRequests(mux)
	if s.editors == nil {
		return api
	}
	// An environment's editor has a host of its own, and everything on it
	// is the editor's.
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if _, ok := s.editors.Owns(r.Host); ok {
			s.editors.ServeHTTP(w, r)
			return
		}
		api.ServeHTTP(w, r)
	})
}

func (s *Server) healthz(w http.ResponseWriter, r *http.Request) {
	ctx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
	defer cancel()
	err := s.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `SELECT 1`)
		return err
	})
	if err != nil {
		writeError(w, http.StatusServiceUnavailable, "database unreachable")
		return
	}
	writeJSON(w, http.StatusOK, map[string]string{"status": "ok"})
}

type workerIDKey struct{}

// workerAuth admits a request carrying a worker credential, and passes the
// worker's ID on in the context.
func (s *Server) workerAuth(next http.HandlerFunc) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		cred, ok := bearer(r)
		if !ok {
			writeError(w, http.StatusUnauthorized, "missing credential")
			return
		}
		id, err := s.workers.Authenticate(r.Context(), cred)
		if err != nil {
			if !errors.Is(err, workers.ErrUnauthorized) {
				s.log.Error("authenticating worker", "err", err)
			}
			writeError(w, http.StatusUnauthorized, "invalid credential")
			return
		}
		next(w, r.WithContext(context.WithValue(r.Context(), workerIDKey{}, id)))
	})
}

func workerID(r *http.Request) string { return r.Context().Value(workerIDKey{}).(string) }

func (s *Server) checkBootstrap(r *http.Request) bool {
	tok, ok := bearer(r)
	return ok && s.bootstrap != "" && subtle.ConstantTimeCompare([]byte(tok), []byte(s.bootstrap)) == 1
}

func bearer(r *http.Request) (string, bool) {
	h := r.Header.Get("Authorization")
	tok, ok := strings.CutPrefix(h, "Bearer ")
	return tok, ok && tok != ""
}

func (s *Server) logRequests(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !strings.HasPrefix(r.URL.Path, "/api/") {
			next.ServeHTTP(w, r)
			return
		}
		start := time.Now()
		rec := &statusRecorder{ResponseWriter: w, status: http.StatusOK}
		next.ServeHTTP(rec, r)
		// Long polls and health checks arrive constantly and would drown
		// everything else.
		if rec.status < 400 && (r.URL.Path == "/api/worker/v1/desired" || r.URL.Path == "/api/healthz" ||
			r.URL.Path == "/api/worker/v1/status" || r.Method == http.MethodGet) {
			return
		}
		s.log.Info("request", "method", r.Method, "path", r.URL.Path, "status", rec.status,
			"duration", time.Since(start).Round(time.Millisecond))
	})
}

type statusRecorder struct {
	http.ResponseWriter
	status int
}

func (r *statusRecorder) WriteHeader(code int) {
	r.status = code
	r.ResponseWriter.WriteHeader(code)
}

func (r *statusRecorder) Unwrap() http.ResponseWriter { return r.ResponseWriter }

// Hijack hands over the connection, which a WebSocket needs.
func (r *statusRecorder) Hijack() (net.Conn, *bufio.ReadWriter, error) {
	hj, ok := r.ResponseWriter.(http.Hijacker)
	if !ok {
		return nil, nil, errors.New("the connection cannot be taken over")
	}
	r.status = http.StatusSwitchingProtocols
	return hj.Hijack()
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func writeError(w http.ResponseWriter, status int, msg string) {
	writeJSON(w, status, map[string]string{"error": msg})
}

// readJSON decodes a request body, refusing fields the target does not have
// so a misspelt field is an error rather than a silent default.
func readJSON(w http.ResponseWriter, r *http.Request, v any) bool {
	dec := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20))
	dec.DisallowUnknownFields()
	if err := dec.Decode(v); err != nil {
		writeError(w, http.StatusBadRequest, "invalid request body: "+err.Error())
		return false
	}
	return true
}

// fail answers with the status a manager's error corresponds to.
func (s *Server) fail(w http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, workers.ErrNotFound):
		writeError(w, http.StatusNotFound, err.Error())
	case errors.Is(err, workers.ErrConflict):
		writeError(w, http.StatusConflict, err.Error())
	case errors.Is(err, workers.ErrInvalid):
		writeError(w, http.StatusBadRequest, err.Error())
	case errors.Is(err, context.Canceled):
		// The client went away; there is nobody to answer.
	default:
		s.log.Error("request failed", "err", err)
		writeError(w, http.StatusInternalServerError, "internal error")
	}
}
