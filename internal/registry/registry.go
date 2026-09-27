// Package registry is Hangar's own container registry: the OCI distribution
// API at /v2/, on the server's own host, so images for templates can be kept
// in Hangar without being published anywhere else.
//
// A repository is <namespace>/<name>, the namespace being its owner's: a
// user's username, lowercased, or a team's slug. Pushing to a namespace the
// caller may push to makes the repository, private, owned by the
// namespace's user or team. Who may pull, push and manage a repository
// follows templates: see the image_repositories table.
//
// Clients sign in with HTTP Basic authentication: a user with one of their
// access tokens as the password (the username is theirs, and is checked),
// an environment's Docker with the credential its agent was given for its
// owner, and a worker with its credential, which pulls anything. An
// environment's credential acts as its owner, but never as an
// administrator.
//
// Blobs are files in the registry's directory, kept once however many
// repositories have them; what each repository has, its manifests and its
// tags are rows in the database, so several servers can share one directory
// and one database.
package registry

import (
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"

	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/users"
)

// Tokens checks a user's access token.
type Tokens interface {
	AuthenticateToken(ctx context.Context, token string) (users.Principal, string, error)
}

// Workers checks a worker's credential, returning the worker's ID.
type Workers interface {
	Authenticate(ctx context.Context, credential string) (string, error)
}

// Credentials opens the credentials environments are given for the
// registry, returning the user one is for.
type Credentials interface {
	OpenRegistryCredential(credential string) (string, error)
}

// Config is what the registry needs.
type Config struct {
	DB *db.DB
	// Dir holds the blobs and uploads in progress.
	Dir string
	// Host is the registry's own host, port included, which image
	// references to it start with.
	Host    string
	Tokens  Tokens
	Workers Workers
	// Credentials, if set, admits environments' credentials.
	Credentials Credentials
	Log         *slog.Logger
}

// Registry serves the distribution API and manages repositories.
type Registry struct {
	host    string
	db      *db.DB
	dir     string
	tokens  Tokens
	workers Workers
	creds   Credentials
	log     *slog.Logger
}

func New(cfg Config) (*Registry, error) {
	for _, d := range []string{"blobs", "uploads"} {
		if err := os.MkdirAll(filepath.Join(cfg.Dir, d), 0o755); err != nil {
			return nil, err
		}
	}
	log := cfg.Log
	if log == nil {
		log = slog.Default()
	}
	return &Registry{host: cfg.Host, db: cfg.DB, dir: cfg.Dir, tokens: cfg.Tokens, workers: cfg.Workers,
		creds: cfg.Credentials, log: log}, nil
}

// caller is who a request is from: a user, or a worker.
type caller struct {
	users.Principal
	worker string
}

// A repository name's components are lowercase letters and digits,
// separated by one of . _ __ or runs of -, as the distribution
// specification has them.
var (
	nameComponent = `[a-z0-9]+(?:(?:\.|_|__|-+)[a-z0-9]+)*`
	validName     = regexp.MustCompile(`^` + nameComponent + `(?:/` + nameComponent + `)+$`)
	validTag      = regexp.MustCompile(`^[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}$`)
)

const realm = `Basic realm="Hangar", charset="UTF-8"`

// profileCredentialPrefix begins an environment's credential
// (profile.RegistryCredentialPrefix).
const profileCredentialPrefix = "hge_"

// ServeHTTP serves /v2/.
func (reg *Registry) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Docker-Distribution-API-Version", "registry/2.0")
	c, err := reg.authenticate(r)
	if err != nil {
		if !errors.Is(err, errUnauthorized) {
			reg.log.Error("registry: authenticating", "err", err)
		}
		w.Header().Set("WWW-Authenticate", realm)
		writeErr(w, http.StatusUnauthorized, "UNAUTHORIZED", "sign in with your username and an access token")
		return
	}
	rest, ok := strings.CutPrefix(r.URL.Path, "/v2/")
	if !ok {
		writeErr(w, http.StatusNotFound, "NAME_UNKNOWN", "not a registry path")
		return
	}
	if rest == "" {
		writeJSON(w, http.StatusOK, struct{}{})
		return
	}
	// What the request changes is recorded as the caller's doing.
	ctx := actor(r.Context(), c)
	switch {
	case strings.HasSuffix(rest, "/tags/list"):
		reg.tags(ctx, w, r, c, strings.TrimSuffix(rest, "/tags/list"))
	case strings.Contains(rest, "/blobs/uploads"):
		i := strings.LastIndex(rest, "/blobs/uploads")
		id := strings.Trim(rest[i+len("/blobs/uploads"):], "/")
		reg.uploads(ctx, w, r, c, rest[:i], id)
	case strings.Contains(rest, "/manifests/"):
		i := strings.LastIndex(rest, "/manifests/")
		reg.manifests(ctx, w, r, c, rest[:i], rest[i+len("/manifests/"):])
	case strings.Contains(rest, "/blobs/"):
		i := strings.LastIndex(rest, "/blobs/")
		reg.blobs(ctx, w, r, c, rest[:i], rest[i+len("/blobs/"):])
	case strings.Contains(rest, "/referrers/"):
		i := strings.LastIndex(rest, "/referrers/")
		reg.referrers(ctx, w, r, c, rest[:i], rest[i+len("/referrers/"):])
	default:
		writeErr(w, http.StatusNotFound, "NAME_UNKNOWN", "not a registry path")
	}
}

var errUnauthorized = errors.New("unauthorized")

// authenticate reads the request's Basic credentials: a user's access
// token, or a worker's credential.
func (reg *Registry) authenticate(r *http.Request) (caller, error) {
	user, password, ok := r.BasicAuth()
	if !ok || password == "" {
		return caller{}, errUnauthorized
	}
	ctx := r.Context()
	if strings.HasPrefix(password, users.TokenPrefix) {
		p, _, err := reg.tokens.AuthenticateToken(ctx, password)
		if errors.Is(err, users.ErrNoSession) {
			return caller{}, errUnauthorized
		}
		if err != nil {
			return caller{}, err
		}
		// The username is checked, so a token pasted under someone else's
		// name fails rather than acting as its owner under theirs.
		if !strings.EqualFold(user, p.Username) {
			return caller{}, errUnauthorized
		}
		return caller{Principal: p}, nil
	}
	if reg.creds != nil && strings.HasPrefix(password, profileCredentialPrefix) {
		id, err := reg.creds.OpenRegistryCredential(password)
		if err != nil {
			return caller{}, errUnauthorized
		}
		var p users.Principal
		err = reg.db.Transact(ctx, func(tx db.Tx) error {
			return tx.QueryRow(ctx, `SELECT id, username FROM users
				WHERE id = $1 AND disabled_at IS NULL AND kind = 'person'`, id).Scan(&p.UserID, &p.Username)
		})
		if errors.Is(err, pgx.ErrNoRows) {
			return caller{}, errUnauthorized
		}
		if err != nil {
			return caller{}, err
		}
		if !strings.EqualFold(user, p.Username) {
			return caller{}, errUnauthorized
		}
		return caller{Principal: p}, nil
	}
	if reg.workers != nil {
		id, err := reg.workers.Authenticate(ctx, password)
		if err == nil {
			return caller{worker: id}, nil
		}
	}
	return caller{}, errUnauthorized
}

// regError is one error in the distribution API's error body.
type regError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
	Detail  any    `json:"detail,omitempty"`
}

// apiError is a failure with its place in the distribution API: the status
// and error code a client is given.
type apiError struct {
	status int
	code   string
	msg    string
}

func (e *apiError) Error() string { return e.msg }

func failure(status int, code, msg string) error { return &apiError{status, code, msg} }

// fail answers a request that failed with err.
func (reg *Registry) fail(w http.ResponseWriter, r *http.Request, err error) {
	var ae *apiError
	switch {
	case errors.As(err, &ae):
		writeErr(w, ae.status, ae.code, ae.msg)
	case errors.Is(err, errNameInvalid):
		writeErr(w, http.StatusBadRequest, "NAME_INVALID", "invalid repository name")
	case errors.Is(err, errNameUnknown):
		writeErr(w, http.StatusNotFound, "NAME_UNKNOWN", "repository not found")
	case errors.Is(err, errDenied):
		writeErr(w, http.StatusForbidden, "DENIED", err.Error())
	case errors.Is(err, context.Canceled):
	default:
		reg.log.Error("registry request failed", "method", r.Method, "path", r.URL.Path, "err", err)
		writeErr(w, http.StatusInternalServerError, "UNKNOWN", "internal error")
	}
}

func writeErr(w http.ResponseWriter, status int, code, msg string) {
	writeJSON(w, status, struct {
		Errors []regError `json:"errors"`
	}{[]regError{{Code: code, Message: msg}}})
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	json.NewEncoder(w).Encode(v)
}

// PruneUploads removes uploads started more than a day ago and not
// finished.
func (reg *Registry) PruneUploads(ctx context.Context) (int, error) {
	var ids []string
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `DELETE FROM registry_uploads WHERE created_at < $1 RETURNING id::text`,
			time.Now().Add(-24*time.Hour))
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var id string
			if err := rows.Scan(&id); err != nil {
				return err
			}
			ids = append(ids, id)
		}
		return rows.Err()
	})
	for _, id := range ids {
		os.Remove(reg.uploadPath(id))
	}
	return len(ids), err
}
