package editor

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strings"
	"time"

	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/publichost"
	"github.com/csnewman/hangar/internal/tunnel"
)

// CookieName is the editor origin's session cookie.
const CookieName = "hangar_editor"

// signInPath is where the editor origin redeems a ticket. It is outside any
// path VS Code's server uses.
const signInPath = "/_hangar/sign-in"

// labelPrefix begins the label every editor origin's host is named for,
// which ends in the environment's ID.
const labelPrefix = "code"

// Tunnels opens streams to workers.
type Tunnels interface {
	Open(workerID string, h tunnel.Header) (net.Conn, error)
}

// Gateway serves every environment's editor, each on its own host named
// after Hangar's public one.
type Gateway struct {
	sessions *Manager
	tunnels  Tunnels
	// public is Hangar's own origin, which names the editors' hosts and is
	// the only one that may frame an editor.
	public *publichost.Public
	proxy  *httputil.ReverseProxy
	// portProxy reaches environments' own web servers (ports.go).
	portProxy *httputil.ReverseProxy
	log       *slog.Logger
}

// NewGateway serves editors on hosts named after public, Hangar's own
// origin as the browser sees it. Those names are in Hangar's own site, so
// the browser sends the editor origin its cookie inside Hangar's iframe,
// which it may refuse for a cross-site frame.
func NewGateway(sessions *Manager, tunnels Tunnels, public *publichost.Public, log *slog.Logger) *Gateway {
	g := &Gateway{sessions: sessions, tunnels: tunnels, public: public, log: log}
	g.proxy = &httputil.ReverseProxy{
		Rewrite:        g.rewrite,
		Transport:      g.transport(),
		ModifyResponse: g.modifyResponse,
		ErrorHandler: func(w http.ResponseWriter, r *http.Request, err error) {
			g.log.Warn("reaching an editor", "host", r.Host, "err", err)
			if errors.Is(err, tunnel.ErrNoTunnel) {
				textError(w, http.StatusServiceUnavailable, "The environment's worker is not connected to Hangar just now. Reload in a moment.")
				return
			}
			textError(w, http.StatusBadGateway, "The editor could not be reached. It may still be starting; reload to try again.")
		},
	}
	g.portProxy = g.newPortProxy()
	return g
}

// Owns reports whether a request's host is an editor's, and whose.
func (g *Gateway) Owns(host string) (string, bool) {
	label, ok := g.public.Label(host)
	if !ok {
		return "", false
	}
	id, ok := strings.CutPrefix(label, labelPrefix)
	return id, ok && db.ValidUUID(id)
}

// Origin is one environment's editor origin.
func (g *Gateway) Origin(environmentID string) string {
	return g.public.NameOrigin(labelPrefix + environmentID)
}

// SignIn issues a ticket for the Hangar session whose token is given and
// returns the URL that redeems it and opens the editor on folder, which may
// be empty. The caller has checked that the session's user may reach the
// environment.
func (g *Gateway) SignIn(ctx context.Context, sessionToken, environmentID, folder string) (string, error) {
	ticket, err := g.sessions.Ticket(ctx, sessionToken, environmentID)
	if err != nil {
		return "", err
	}
	to := "/"
	if folder != "" {
		to += "?" + url.Values{"folder": {folder}}.Encode()
	}
	return g.Origin(environmentID) + signInPath + "?" + url.Values{"ticket": {ticket}, "to": {to}}.Encode(), nil
}

// ServeHTTP serves a request for an editor host; Owns has said it is one.
func (g *Gateway) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	env, _ := g.Owns(r.Host)
	if !g.allowed(r, env) {
		textError(w, http.StatusForbidden, "Cross-origin request refused.")
		return
	}
	if r.URL.Path == signInPath {
		g.signIn(w, r, env)
		return
	}

	c, _ := r.Cookie(CookieName)
	var cookie string
	if c != nil {
		cookie = c.Value
	}
	t, err := g.sessions.Authenticate(r.Context(), cookie, env)
	switch {
	case errors.Is(err, ErrNoSession):
		textError(w, http.StatusUnauthorized, "Open this environment's editor from Hangar.")
		return
	case err != nil:
		g.log.Error("authenticating an editor request", "err", err)
		textError(w, http.StatusInternalServerError, "Internal error.")
		return
	case !t.Running:
		textError(w, http.StatusConflict, "The environment is not running.")
		return
	}
	g.proxy.ServeHTTP(w, r.WithContext(context.WithValue(r.Context(), targetKey{}, target{env: env, worker: t.WorkerID})))
}

// allowed refuses what another site, or another environment's editor, asks
// of this one. Every editor host is one site with Hangar and with each
// other, so the SameSite cookie alone would let one environment's editor
// drive another's; only Hangar's page opening the editor in its iframe, and
// the editor's own requests, get through.
func (g *Gateway) allowed(r *http.Request, env string) bool {
	switch r.Header.Get("Sec-Fetch-Site") {
	case "", "same-origin", "none":
	case "same-site":
		// Hangar's iframe, or a link the user followed.
		if r.Header.Get("Sec-Fetch-Mode") != "navigate" {
			return false
		}
	default:
		return false
	}
	// The browser sends Origin on a WebSocket's opening request, which
	// Sec-Fetch-Site does not cover in every browser.
	if origin := r.Header.Get("Origin"); origin != "" && origin != g.Origin(env) {
		return false
	}
	return true
}

func (g *Gateway) signIn(w http.ResponseWriter, r *http.Request, env string) {
	g.redeem(w, r, env, CookieName, g.public.Scheme() == "https")
}

type targetKey struct{}

type target struct {
	env    string
	worker string
}

// rewrite sends a request on to the editor as the browser made it, less the
// editor origin's own cookie: the guest has no business holding it. The
// Host header is kept, since VS Code's server builds the URLs it hands the
// browser from it.
func (g *Gateway) rewrite(pr *httputil.ProxyRequest) {
	t := pr.In.Context().Value(targetKey{}).(target)
	// The transport keeps connections per host, and dials the environment
	// and worker this names.
	pr.Out.URL.Scheme = "http"
	pr.Out.URL.Host = t.env + "." + t.worker + ".hangar-editor"
	pr.Out.Host = pr.In.Host
	pr.SetXForwarded()
	pr.Out.Header.Set("X-Forwarded-Proto", g.public.Scheme())

	var kept []string
	for _, c := range pr.Out.Cookies() {
		if c.Name != CookieName {
			kept = append(kept, c.String())
		}
	}
	pr.Out.Header.Del("Cookie")
	if len(kept) > 0 {
		pr.Out.Header.Set("Cookie", strings.Join(kept, "; "))
	}
}

// transport opens each connection to an editor as a stream through its
// worker's tunnel.
func (g *Gateway) transport() *http.Transport {
	return &http.Transport{
		DialContext: func(ctx context.Context, _, addr string) (net.Conn, error) {
			host, _, err := net.SplitHostPort(addr)
			if err != nil {
				return nil, err
			}
			parts := strings.Split(host, ".")
			if len(parts) != 3 {
				return nil, fmt.Errorf("not an editor address: %s", addr)
			}
			return g.tunnels.Open(parts[1], tunnel.Header{Kind: tunnel.KindEditor, Environment: parts[0]})
		},
		MaxIdleConnsPerHost:   16,
		IdleConnTimeout:       90 * time.Second,
		ResponseHeaderTimeout: 2 * time.Minute,
		// The browser negotiates compression with VS Code's server itself.
		DisableCompression: true,
	}
}

// modifyResponse lets only Hangar's own page frame the editor, and the
// editor frame itself: VS Code runs its web worker extension host in an
// iframe of its own origin, and every ancestor of a frame must be allowed.
func (g *Gateway) modifyResponse(resp *http.Response) error {
	resp.Header.Add("Content-Security-Policy", "frame-ancestors 'self' "+g.public.Origin())
	return nil
}

func textError(w http.ResponseWriter, code int, msg string) {
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(code)
	fmt.Fprintln(w, msg)
}
