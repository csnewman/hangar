package frontendapi

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"strings"
	"unicode"

	"github.com/getkin/kin-openapi/openapi3"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/users"
)

// CookieName is the session cookie. It must match the securityScheme in
// openapi.yaml.
const CookieName = "hangar_session"

var (
	errUnauthorized = errors.New("not signed in")
	errForbidden    = errors.New("forbidden")
)

type sessionKey struct{}

// session is what the request's cookie resolved to.
type session struct {
	principal users.Principal
	token     string
	ok        bool
	// secure is whether the browser reached the server over HTTPS, and so
	// whether a cookie set in reply may be marked Secure.
	secure bool
	// bearer is whether the caller signed in with an access token.
	bearer bool
}

func sessionFrom(ctx context.Context) session {
	s, _ := ctx.Value(sessionKey{}).(session)
	return s
}

// session resolves the request's cookie, if any, before anything else runs.
// It never refuses a request itself: whether an operation needs a session is
// the access rules' decision.
func (h *handler) session(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		s := session{secure: r.TLS != nil || strings.EqualFold(r.Header.Get("X-Forwarded-Proto"), "https")}
		via := ""
		if bearer, ok := strings.CutPrefix(r.Header.Get("Authorization"), "Bearer "); ok {
			// A tool outside the browser, with an access token. No cookie
			// comes with it, so there is no request forgery to guard.
			p, name, err := h.users.AuthenticateToken(r.Context(), strings.TrimSpace(bearer))
			switch {
			case err == nil:
				s.principal, s.ok, s.bearer = p, true, true
				via = "access token " + name
			case errors.Is(err, users.ErrNoSession):
				writeError(w, http.StatusUnauthorized, "that access token is not valid")
				return
			default:
				h.log.Error("resolving an access token", "err", err)
				writeError(w, http.StatusInternalServerError, "internal error")
				return
			}
		} else if c, err := r.Cookie(CookieName); err == nil && c.Value != "" {
			p, err := h.users.Authenticate(r.Context(), c.Value)
			switch {
			case err == nil:
				s.principal, s.token, s.ok = p, c.Value, true
			case !errors.Is(err, users.ErrNoSession):
				h.log.Error("resolving session", "err", err)
				writeError(w, http.StatusInternalServerError, "internal error")
				return
			}
		}
		if !s.ok && h.autoSignIn != "" {
			token, p, err := h.users.SignInAs(audit.WithActor(r.Context(), audit.Actor{IP: clientIP(r)}), h.autoSignIn)
			if err != nil {
				h.log.Error("signing in automatically", "user", h.autoSignIn, "err", err)
				writeError(w, http.StatusInternalServerError, "automatic sign-in failed")
				return
			}
			s.principal, s.token, s.ok = p, token, true
			w.Header().Add("Set-Cookie", sessionCookie(token, s.secure, int(users.SessionLifetime.Seconds())))
		}
		// Whatever the request goes on to change is recorded as by whoever
		// it is from.
		actor := audit.Actor{IP: clientIP(r), Via: via}
		if s.ok {
			actor.UserID, actor.Name = s.principal.UserID, s.principal.Username
		}
		ctx := audit.WithActor(context.WithValue(r.Context(), sessionKey{}, s), actor)
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// clientIP is the address a request came from: the first a proxy in front
// names, or the connection's.
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

// sameOrigin refuses a state-changing request that a browser says came from
// another site. The session cookie is SameSite=Lax, which already keeps it
// off cross-site POSTs; this is the second lock on the same door, and the
// one that does not depend on the browser honouring SameSite.
func sameOrigin(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.Method {
		case http.MethodGet, http.MethodHead, http.MethodOptions:
			next.ServeHTTP(w, r)
			return
		}
		if site := r.Header.Get("Sec-Fetch-Site"); site != "" && site != "same-origin" && site != "none" {
			writeError(w, http.StatusForbidden, "cross-site request refused")
			return
		}
		if origin := r.Header.Get("Origin"); origin != "" {
			u, err := url.Parse(origin)
			if err != nil || u.Host != r.Host {
				writeError(w, http.StatusForbidden, "cross-origin request refused")
				return
			}
		}
		next.ServeHTTP(w, r)
	})
}

// access is who may call one operation.
type access int

const (
	signedIn access = iota
	public
	adminOnly
)

type rules map[string]access

// accessRules reads each operation's access from the spec: `security: []`
// makes it public, `x-hangar-admin: true` restricts it to administrators, and
// anything else needs a session. Keys are the Go operation names the
// generated middleware is called with.
func accessRules(doc *openapi3.T) (rules, error) {
	r := rules{}
	for path, item := range doc.Paths.Map() {
		for method, op := range item.Operations() {
			if op.OperationID == "" {
				return nil, fmt.Errorf("%s %s has no operationId", method, path)
			}
			a := signedIn
			if op.Security != nil && len(*op.Security) == 0 {
				a = public
			}
			if v, ok := op.Extensions["x-hangar-admin"]; ok {
				admin, isBool := v.(bool)
				if !isBool {
					return nil, fmt.Errorf("%s: x-hangar-admin must be true or false", op.OperationID)
				}
				if admin {
					if a == public {
						return nil, fmt.Errorf("%s is both public and admin-only", op.OperationID)
					}
					a = adminOnly
				}
			}
			r[goName(op.OperationID)] = a
		}
	}
	return r, nil
}

// goName is the name oapi-codegen gives an operation.
func goName(operationID string) string {
	rs := []rune(operationID)
	rs[0] = unicode.ToUpper(rs[0])
	return string(rs)
}

// enforce runs before every handler. An operation the rules do not know is
// refused rather than let through.
func (r rules) enforce(next StrictHandlerFunc, operation string) StrictHandlerFunc {
	a, known := r[operation]
	return func(ctx context.Context, w http.ResponseWriter, req *http.Request, request any) (any, error) {
		if !known {
			return nil, fmt.Errorf("no access rule for operation %s", operation)
		}
		if a == public {
			return next(ctx, w, req, request)
		}
		s := sessionFrom(ctx)
		if !s.ok {
			return nil, errUnauthorized
		}
		if a == adminOnly && !s.principal.Admin {
			return nil, errForbidden
		}
		return next(ctx, w, req, request)
	}
}

// principal is the caller of an operation that needs a session. The access
// rules have already refused the request if there is none.
func principal(ctx context.Context) users.Principal {
	return sessionFrom(ctx).principal
}

func sessionCookie(value string, secure bool, maxAge int) string {
	c := &http.Cookie{
		Name:     CookieName,
		Value:    value,
		Path:     "/",
		MaxAge:   maxAge,
		HttpOnly: true,
		Secure:   secure,
		SameSite: http.SameSiteLaxMode,
	}
	return c.String()
}

func (h *handler) Login(ctx context.Context, req LoginRequestObject) (LoginResponseObject, error) {
	token, u, err := h.users.Login(ctx, req.Body.Username, req.Body.Password)
	switch {
	case errors.Is(err, users.ErrBadCredentials):
		return Login401JSONResponse{UnauthorizedJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	cookie := sessionCookie(token, sessionFrom(ctx).secure, int(users.SessionLifetime.Seconds()))
	return Login200JSONResponse{Body: me(u), Headers: Login200ResponseHeaders{SetCookie: &cookie}}, nil
}

func (h *handler) Logout(ctx context.Context, _ LogoutRequestObject) (LogoutResponseObject, error) {
	s := sessionFrom(ctx)
	if err := h.users.Logout(ctx, s.token); err != nil {
		return nil, err
	}
	cookie := sessionCookie("", s.secure, -1)
	return Logout204Response{Headers: Logout204ResponseHeaders{SetCookie: &cookie}}, nil
}

func (h *handler) GetMe(ctx context.Context, _ GetMeRequestObject) (GetMeResponseObject, error) {
	u, err := h.users.Get(ctx, principal(ctx).UserID)
	if errors.Is(err, users.ErrNotFound) {
		return nil, errUnauthorized
	}
	if err != nil {
		return nil, err
	}
	m := me(u)
	m.SSH = h.ssh
	return GetMe200JSONResponse(m), nil
}

func (h *handler) ChangePassword(ctx context.Context, req ChangePasswordRequestObject) (ChangePasswordResponseObject, error) {
	s := sessionFrom(ctx)
	err := h.users.ChangePassword(ctx, s.principal.UserID, s.token, req.Body.CurrentPassword, req.Body.NewPassword)
	switch {
	case errors.Is(err, users.ErrInvalid):
		return ChangePassword400JSONResponse{InvalidJSONResponse{Error: err.Error()}}, nil
	case err != nil:
		return nil, err
	}
	return ChangePassword204Response{}, nil
}

func me(u users.User) Me {
	return Me{ID: u.ID, Username: u.Username, DisplayName: u.DisplayName, Admin: u.Admin, HasPassword: u.HasPassword}
}
